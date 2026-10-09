//! 出口服务入口（M3 §2.2 方案 B′ 的**服务侧半边**）：两源合一的 [`ServiceIntake`]。
//!
//! 为什么是这个形态（设计门 1-1 判「方案 A 泛型化」不可实现后的定稿）：三服务今天都吃
//! `UnixListener`/`UnixStream`（帧层 / per-syscall 期限 / `poll(2)` 语义 / `try_clone` /
//! `shutdown_both` / busy 路径全挂在这上面）⇒ **不泛型化、不改服务本体**，改为给服务喂
//! 「一条到 QUIC 流的 socketpair」（[`crate::exit::pump`] 是异步侧另一半）。于是服务侧的
//! 形参从 `UnixListener` 收宽成「本类型 + `from_listener` 构造面」——`serve_stoppable`
//! 之外的服务代码**零改动**（§1.2 的「应用层帧逐字节不变」由 socketpair 字节搬运保证）。
//!
//! ## 五件套（设计门 N1–N4，逐条在这里落纸）
//!
//! ① [`ServiceIntake::from_listener`]：WG-only/调试形态（单 UDS 源，零 QUIC 面）——
//!    仓内两处监听面测试**语义零改**地继续用它（`term/service.rs` 的 P7 用例、
//!    `speedtest_server.rs` 的 stop 用例）；
//! ② [`ServiceIntake::accept`]：两源（UDS 优先 + QUIC 队列）；无连接可受理时按
//!    [`ACCEPT_POLL_BUDGET`] 阻塞等待唤醒面，超时返回 `WouldBlock`（调用方既有的
//!    200ms 重试节拍不变）；
//! ③ **唤醒面**：内建 pipe（`UnixStream::pair`，与出口 `bridge` 的自唤醒管道同形态）；
//!    QUIC 泵入队时写 1B，服务侧 poll 该 fd ⇒ **无 200ms 空转延迟**（不唤醒则
//!    `files_server.rs` 把 `WouldBlock` 归 `Retry` ⇒ 每条 QUIC 服务流最多 +200ms）；
//!    **电平触发**：队列非空时管道恒可读（只在队列排空时收干）⇒ 服务侧 poll 不会漏。
//! ④ **两源顺序/公平**：**UDS 源优先**（每次回环都先查——比设计下限「QUIC 连取上限
//!    N=8 后回让」更紧：回让发生在**每一条** QUIC 流之间，防 QUIC 洪泛饿死 WG 服务腿
//!    〔该腿的消费者 = CLI 远程面 DC14/DC15/CA1〕）；连取计数到 [`QUIC_STREAK_MAX`] 时
//!    强制回环一次（先重查 UDS 再取 QUIC）。
//! ⑤ [`ServiceIntake::ready_fds`]：**就绪 fd 登记**——term 的监听循环重写后 poll 的目标
//!    集 =〔UDS 监听 fd（若有），唤醒读端〕。
//!
//! ## 容量（§1.7 设计门 2-4 / 2-5）
//!
//! 每 tag 一个**进程级** intake；容量 = 该服务**在册上限 + K**（K=4，见
//! [`crate::tuning::service_defaults::INTAKE_K`]）——保证「服务在册闸先于 intake 满」触发，
//! 于是 files 的 `server_busy` / speedtest 的 `error:"busy"` 两条**应用层**拒入路径在
//! STREAM 面上仍可达（intake 满只在极端洪泛〔> 在册上限 + K 并发〕时出现）。
//! 计数口径 = **在队列条数**（服务取走后即释放名额；服务自身的在册闸独立计数）。
//!
//! **本文件纯 std**（隔离门 ② 条扫描面：不得出现 `tokio::|quinn|rustls|async fn|.await`）
//! ——它同时被同步面（三服务的 accept 循环：真线程 + `libc::poll`）与异步面（出口分发
//! 侧只调 [`ServiceIntakeTx::try_enqueue`]，同步、非阻塞）使用。

use std::collections::VecDeque;
use std::io;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::sync_util::lock_unpoison;

/// `accept()` 的 poll 预算。
///
/// 取 200ms = `files_server::serve_stoppable_accepts` 既有的 `Retry` 节拍（`WouldBlock`
/// ⇒ `thread::sleep(200ms)`）：两处同拍 ⇒ 服务侧「停止位到点退出」的粒度与改前同量级
/// （≤ 一个 poll 预算 + 一个重试节拍），且**连接到达即返回**（有唤醒面，不再空转 200ms）。
pub const ACCEPT_POLL_BUDGET: Duration = Duration::from_millis(200);

/// QUIC 源**连取上限**（§2.2-④：连取 N=8 后回让）。
///
/// 实现上每取一条 QUIC 流之前都重查 UDS 源（比上限更紧），本计数只承担「连取到上限后
/// 强制走一次等待回环」的形态用途——两条一起构成「QUIC 洪泛不饿死 UDS 源」的构造性保证。
const QUIC_STREAK_MAX: usize = 8;

/// 入队被拒（QUIC 源队列满）——携带当刻深度与容量（E-q5 归因行的 `入口队列满 %d/%d`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, thiserror::Error)]
#[error("入口队列满（{queued}/{capacity}）")]
pub struct IntakeFull {
    /// 拒绝当刻的队列深度。
    pub queued: usize,
    /// 该 tag 的 intake 容量（在册上限 + K）。
    pub capacity: usize,
}

/// 两源共享件（出口侧入队句柄与服务侧 intake 各持一份 `Arc`）。
struct Shared {
    /// QUIC 源队列（在队列条数 = 容量占用口径）。
    queue: Mutex<VecDeque<UnixStream>>,
    /// 容量（0 = 无 QUIC 源：`from_listener` 形态）。
    capacity: usize,
    /// 唤醒管道写端（**非阻塞**：队列非空时写 1B，`EAGAIN` 忽略——电平触发不需要计数）。
    wake_tx: UnixStream,
    /// 唤醒管道读端（非阻塞；服务侧 poll 目标之一）。
    wake_rx: UnixStream,
    /// 入队成功累计（诊断/用例读数）。
    enqueued: AtomicU64,
    /// 入队被拒累计（诊断/用例读数；出口侧另有 E-q5 计数与行）。
    refused: AtomicU64,
    /// 服务侧取走累计（诊断/用例读数）。
    taken: AtomicU64,
}

impl Shared {
    /// 唤醒（非阻塞写 1B；`EAGAIN`/`EPIPE` 一律忽略——管道是电平信号不是计数）。
    fn wake(&self) {
        let _ = (&self.wake_tx).write(&[1u8]);
    }

    /// 收干唤醒字节（**只在队列排空时调**——电平触发：队列非空 ⇒ 管道恒可读）。
    fn drain_wake(&self) {
        let mut buf = [0u8; 64];
        while let Ok(n) = (&self.wake_rx).read(&mut buf) {
            if n == 0 {
                break;
            }
        }
    }
}

/// 每 tag 的出口侧入队句柄集（[`crate::ExitQuicConfig`] 的装配面）。
///
/// `None` = 该服务本期**不可用**（UDS 监听失败 / `HOMEWAY_TERM=off` / 未装配）⇒ 出口对该
/// tag 一律 `reset(0x22)`（§1.6 设计门 N16 的保守选择）。`dial`/`probe` 不在表内：
/// probe 不进队列（回显任务直连）、dial 本期只拒（M4 换轨）。
#[derive(Clone, Default)]
pub struct ServiceIntakes {
    /// tag=1 files 的入口（在册上限 = `files_server::MAX_CONNS`）。
    pub files: Option<ServiceIntakeTx>,
    /// tag=2 term 的入口（在册上限 = 会话上限 `HOMEWAY_TERM_MAX_SESSIONS`）。
    pub term: Option<ServiceIntakeTx>,
    /// tag=3 speedtest 的入口（在册上限 = `speedtest_server::MAX_CONNS`）。
    pub speedtest: Option<ServiceIntakeTx>,
}

impl ServiceIntakes {
    /// 按 tag 取入队句柄（`probe`/`dial` 与未装配的服务恒 `None`）。
    pub fn slot(&self, tag: crate::stream::StreamTag) -> Option<&ServiceIntakeTx> {
        match tag {
            crate::stream::StreamTag::Files => self.files.as_ref(),
            crate::stream::StreamTag::Term => self.term.as_ref(),
            crate::stream::StreamTag::Speedtest => self.speedtest.as_ref(),
            crate::stream::StreamTag::Dial | crate::stream::StreamTag::Probe => None,
        }
    }

    /// 是否有任一服务入口（装配面判据；`false` = QUIC 档只提供 probe）。
    pub fn any(&self) -> bool {
        self.files.is_some() || self.term.is_some() || self.speedtest.is_some()
    }
}

/// 出口侧入队句柄（**克隆便宜**：只持 `Arc`；`try_enqueue` 同步非阻塞——不得在命令面等锁）。
#[derive(Clone)]
pub struct ServiceIntakeTx {
    shared: Arc<Shared>,
}

impl ServiceIntakeTx {
    /// 入队一条**服务侧**socketpair 端。满 ⇒ [`IntakeFull`]（调用方按 `reset(0x23)` 处置）。
    pub fn try_enqueue(&self, conn: UnixStream) -> Result<(), IntakeFull> {
        let mut q = lock_unpoison(&self.shared.queue);
        if q.len() >= self.shared.capacity {
            let full = IntakeFull {
                queued: q.len(),
                capacity: self.shared.capacity,
            };
            drop(q);
            self.shared.refused.fetch_add(1, Ordering::Relaxed);
            return Err(full);
        }
        q.push_back(conn);
        drop(q);
        self.shared.enqueued.fetch_add(1, Ordering::Relaxed);
        self.shared.wake();
        Ok(())
    }

    /// 容量（在册上限 + K）。
    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// 当刻队列深度。
    pub fn depth(&self) -> usize {
        lock_unpoison(&self.shared.queue).len()
    }

    /// 累计入队 / 被拒（诊断面读数）。
    pub fn totals(&self) -> (u64, u64) {
        (
            self.shared.enqueued.load(Ordering::Relaxed),
            self.shared.refused.load(Ordering::Relaxed),
        )
    }
}

/// 服务侧受理面（**单消费者**：每个服务一枚 accept 循环线程；`Send` 但不必 `Sync`）。
pub struct ServiceIntake {
    /// UDS 源（`None` = QUIC-only 形态）。
    listener: Option<UnixListener>,
    /// 两源共享件。
    shared: Arc<Shared>,
    /// QUIC 源连取计数（单消费者用；原子只为让本类型 `Sync`——测试/装配面可 `Arc` 共享）。
    streak: AtomicUsize,
}

impl ServiceIntake {
    /// ① **WG-only/调试形态**：单 UDS 源（无 QUIC 面、无容量）——仓内既有监听面测试与
    /// 「不起 QUIC 面的本地形态」用它。
    ///
    /// 监听器置非阻塞（`accept()` 的 poll 前提）；失败即 `Err`（调用方按起不来处置）。
    pub fn from_listener(listener: UnixListener) -> io::Result<Self> {
        listener.set_nonblocking(true)?;
        let shared = new_shared(0)?;
        Ok(Self {
            listener: Some(listener),
            shared,
            streak: AtomicUsize::new(0),
        })
    }

    /// ② **两源形态**：UDS 监听器 + QUIC 源（容量 = 在册上限 + K）。返回值 =
    /// （服务侧 intake，出口侧入队句柄）。
    pub fn with_quic(
        listener: UnixListener,
        capacity: usize,
    ) -> io::Result<(Self, ServiceIntakeTx)> {
        listener.set_nonblocking(true)?;
        Self::pair(Some(listener), capacity)
    }

    /// ③ **QUIC-only 形态**（无 UDS 监听器；测试与纯 QUIC 部署面）。
    pub fn quic_only(capacity: usize) -> io::Result<(Self, ServiceIntakeTx)> {
        Self::pair(None, capacity)
    }

    fn pair(listener: Option<UnixListener>, capacity: usize) -> io::Result<(Self, ServiceIntakeTx)> {
        let shared = new_shared(capacity)?;
        let tx = ServiceIntakeTx {
            shared: Arc::clone(&shared),
        };
        Ok((
            Self {
                listener,
                shared,
                streak: AtomicUsize::new(0),
            },
            tx,
        ))
    }

    /// ⑤ **就绪 fd 登记**：服务侧 poll 的目标集 =〔UDS 监听 fd（若有），唤醒读端〕。
    ///
    /// term 的监听循环重写后 poll 它；files/speedtest 直接调 [`Self::accept`]（内部 poll
    /// 同一集合）——两条路径共用同一份登记，不各写一份 fd 列表。
    pub fn ready_fds(&self) -> Vec<RawFd> {
        let mut fds = Vec::with_capacity(2);
        if let Some(ln) = &self.listener {
            fds.push(ln.as_raw_fd());
        }
        fds.push(self.shared.wake_rx.as_raw_fd());
        fds
    }

    /// ② **两源受理**（UDS 优先 + QUIC 队列）。
    ///
    /// 返回的连接**置阻塞**（服务本体的读写语义 = 今天 UDS 逐字同款：per-syscall 期限 /
    /// `BufReader` 预读 / `try_clone` 都建立在阻塞 fd 上）。
    ///
    /// 无连接可受理时 poll〔就绪 fd〕至多 [`ACCEPT_POLL_BUDGET`]，到点返回
    /// `WouldBlock`（调用方既有 `Retry` 节拍：sleep 200ms 后再来，顺带查停止位）。
    /// UDS 源的**真错**（监听器坏了）与 QUIC 源不同：前者原样上报（调用方 `classify_accept_err`
    /// 分类：瞬态退避 / 致命退工），后者不产生错误（队列空只是空）。
    pub fn accept(&self) -> io::Result<UnixStream> {
        loop {
            // ① UDS 源优先（**每次回环都先查**——见模块头 ④）
            if let Some(ln) = &self.listener {
                match ln.accept() {
                    Ok((conn, _)) => {
                        self.streak.store(0, Ordering::Relaxed);
                        conn.set_nonblocking(false)?;
                        return Ok(conn);
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                    Err(e) => return Err(e),
                }
            }
            // ② QUIC 源（未达连取上限；到上限即清零回让——下一条必经①重查）
            if self.streak.load(Ordering::Relaxed) < QUIC_STREAK_MAX {
                if let Some(conn) = self.take_quic() {
                    self.streak.fetch_add(1, Ordering::Relaxed);
                    conn.set_nonblocking(false)?;
                    return Ok(conn);
                }
            } else {
                self.streak.store(0, Ordering::Relaxed);
            }
            // ③ 两源皆空（或回让一拍）⇒ 等唤醒面（电平触发：队列非空即刻返回）
            if !self.wait_ready() {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "服务入口两源皆空（poll 预算到点）",
                ));
            }
        }
    }

    /// 队列深度（诊断/用例）。
    pub fn depth(&self) -> usize {
        lock_unpoison(&self.shared.queue).len()
    }

    /// **QUIC 源**已取走累计（诊断/用例；UDS 源不经本计数）。
    pub fn taken_total(&self) -> u64 {
        self.shared.taken.load(Ordering::Relaxed)
    }

    /// 容量（0 = WG-only 形态）。
    pub fn capacity(&self) -> usize {
        self.shared.capacity
    }

    /// 从 QUIC 队列取一条；**队列排空时收干唤醒字节**（电平触发的收口——否则遗留的 1B
    /// 会让后续 poll 恒可读而空转）。
    fn take_quic(&self) -> Option<UnixStream> {
        let got = {
            let mut q = lock_unpoison(&self.shared.queue);
            q.pop_front()
        };
        let empty = lock_unpoison(&self.shared.queue).is_empty();
        if empty {
            self.shared.drain_wake();
        }
        if got.is_some() {
            self.shared.taken.fetch_add(1, Ordering::Relaxed);
        }
        got
    }

    /// poll〔就绪 fd〕至多 [`ACCEPT_POLL_BUDGET`]；`true` = 有事可做（回环重试），
    /// `false` = 预算到点或 `EINTR`（调用方按 `WouldBlock` 处置）。
    fn wait_ready(&self) -> bool {
        let fds = self.ready_fds();
        let mut pfds: Vec<libc::pollfd> = fds
            .iter()
            .map(|fd| libc::pollfd {
                fd: *fd,
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let r = unsafe {
            libc::poll(
                pfds.as_mut_ptr(),
                pfds.len() as libc::nfds_t,
                ACCEPT_POLL_BUDGET.as_millis() as libc::c_int,
            )
        };
        r > 0
    }
}

/// 两源共享件构造（唤醒管道 = `UnixStream::pair`，两端非阻塞）。
fn new_shared(capacity: usize) -> io::Result<Arc<Shared>> {
    let (wake_tx, wake_rx) = UnixStream::pair()?;
    wake_tx.set_nonblocking(true)?;
    wake_rx.set_nonblocking(true)?;
    Ok(Arc::new(Shared {
        queue: Mutex::new(VecDeque::new()),
        capacity,
        wake_tx,
        wake_rx,
        enqueued: AtomicU64::new(0),
        refused: AtomicU64::new(0),
        taken: AtomicU64::new(0),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream as StdUnixStream;
    use std::path::PathBuf;
    use std::sync::atomic::AtomicU32;

    /// 进程内唯一 UDS 路径（flake 口径①：不钉固定端口/路径）。
    fn temp_sock(tag: &str) -> (UnixListener, PathBuf) {
        static SEQ: AtomicU32 = AtomicU32::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "hw-intake-{tag}-{}-{n}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        let ln = UnixListener::bind(&path).expect("绑 UDS");
        (ln, path)
    }

    /// 客户端连上监听器（UDS 源的一条待受理连接）。
    fn uds_connect(path: &PathBuf) -> StdUnixStream {
        StdUnixStream::connect(path).expect("连 UDS")
    }

    /// 一条「服务侧」socketpair 端（模拟出口泵交给 intake 的那一端）+ 对端（模拟泵侧）。
    fn quic_pair() -> (StdUnixStream, StdUnixStream) {
        StdUnixStream::pair().expect("socketpair")
    }

    /// 队列里一条「已断」的 QUIC 端（对端即弃——用于只判容量/计数的用例）。
    fn quic_end() -> StdUnixStream {
        quic_pair().0
    }

    /// 从受理到的连接读 1B 标记（服务侧 fd 是阻塞的；标记由对端在受理前写入）。
    fn read_marker(s: &mut StdUnixStream) -> u8 {
        let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
        let mut b = [0u8; 1];
        s.read_exact(&mut b).expect("读标记");
        b[0]
    }

    /// **判据（① from_listener 单源形态）**：WG-only 构造面吃真 UDS，`accept` 逐字节可用，
    /// 且容量 0/无 QUIC 源（`try_enqueue` 面不存在——编译器保证）。
    #[test]
    fn from_listener_serves_uds_only() {
        let (ln, path) = temp_sock("uds");
        let intake = ServiceIntake::from_listener(ln).expect("intake");
        assert_eq!(intake.capacity(), 0, "WG-only 形态无 QUIC 容量");
        // 就绪集恒 =〔监听 fd，唤醒读端〕（唤醒端在 WG-only 形态下不被写——登记形态统一）
        assert_eq!(intake.ready_fds().len(), 2, "就绪集 = 监听 fd + 唤醒读端");
        let mut c = uds_connect(&path);
        let mut s = intake.accept().expect("受理 UDS 源");
        // 单向字节可通（服务侧看到的 fd 是阻塞的）
        c.write_all(b"ping").expect("客户端写");
        let mut buf = [0u8; 4];
        s.read_exact(&mut buf).expect("服务侧读");
        assert_eq!(&buf, b"ping");
        let _ = std::fs::remove_file(&path);
    }

    /// **判据（② accept 两源 + ④ UDS 优先）**：QUIC 队列里先压 3 条，UDS 侧再连 1 条 ⇒
    /// 下一条 accept **必须是 UDS 那条**（源优先），随后才轮到 QUIC 队列（FIFO）。
    #[test]
    fn accept_prefers_uds_source_and_keeps_quic_fifo() {
        let (ln, path) = temp_sock("prio");
        let (intake, tx) = ServiceIntake::with_quic(ln, 4).expect("intake");
        // 每条源在受理前写好 1B 标记（读到的标记即身份）
        let mut _peers = Vec::new();
        for i in 0..3u8 {
            let (a, b) = quic_pair();
            (&b).write_all(&[0xA0 + i]).expect("写 QUIC 侧标记");
            _peers.push(b);
            tx.try_enqueue(a).expect("入队");
        }
        let mut c = uds_connect(&path);
        c.write_all(&[0x55]).expect("写 UDS 侧标记");
        let mut got = Vec::new();
        for _ in 0..4 {
            got.push(intake.accept().expect("受理"));
        }
        assert_eq!(read_marker(&mut got[0]), 0x55, "第一条必须来自 UDS 源（UDS 优先）");
        let tail: Vec<u8> = got[1..].iter_mut().map(read_marker).collect();
        assert_eq!(tail, vec![0xA0, 0xA1, 0xA2], "其余按 FIFO 来自 QUIC 源");
        assert_eq!(intake.taken_total(), 3, "QUIC 源取走 3 条（UDS 源不经该计数）");
        assert_eq!(intake.depth(), 0, "队列已排空");
        let _ = std::fs::remove_file(&path);
    }

    /// **判据（④ 公平性：QUIC 洪泛下 UDS 仍能受理）**：队列压满 8 条 QUIC，连 UDS 一条 ⇒
    /// **下一条** accept 即取到 UDS（不迟于 1 条 QUIC；设计下限是 ≤8）。
    #[test]
    fn uds_source_is_never_starved_by_quic_backlog() {
        let (ln, path) = temp_sock("fair");
        let (intake, tx) = ServiceIntake::with_quic(ln, 8).expect("intake");
        let mut _peers = Vec::new();
        for i in 0..8u8 {
            let (a, b) = quic_pair();
            (&b).write_all(&[0xB0 + i]).expect("写标记");
            _peers.push(b);
            tx.try_enqueue(a).expect("入队");
        }
        let mut c = uds_connect(&path);
        c.write_all(&[0x66]).expect("写 UDS 标记");
        let mut s = intake.accept().expect("受理");
        assert_eq!(
            read_marker(&mut s),
            0x66,
            "QUIC 洪泛（队列满 8）下 UDS 源必须在下一轮即被受理"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// **判据（③ 唤醒面：QUIC 入队唤醒阻塞中的 accept）**：accept 已在 poll 中等待 ⇒
    /// 入队必须**立即**唤醒它（远小于 poll 预算），且返回的正是那条 QUIC 连接。
    #[test]
    fn enqueue_wakes_a_blocked_accept_promptly() {
        let (intake, tx) = ServiceIntake::quic_only(4).expect("intake");
        let intake = Arc::new(intake);
        let i2 = Arc::clone(&intake);
        let t0 = std::time::Instant::now();
        let h = std::thread::spawn(move || {
            let s = i2.accept().expect("被唤醒后受理");
            (s, t0.elapsed())
        });
        // 让 accept 先进入 poll（起线程后小睡；200ms 预算内必已进 poll）
        std::thread::sleep(Duration::from_millis(50));
        let (a, _b) = StdUnixStream::pair().expect("socketpair");
        let t_enq = std::time::Instant::now();
        tx.try_enqueue(a).expect("入队");
        let (_s, waited) = h.join().expect("线程退出");
        // 上界口径（flake 口径②：只判上界）：唤醒后 accept 的总等待 ≈ 入队前的 50ms + ε，
        // 远小于「无唤醒面」下的 200ms 空转
        assert!(
            waited < ACCEPT_POLL_BUDGET * 2,
            "唤醒面未生效（等待 {waited:?} ≥ 2× 预算）"
        );
        assert!(
            t_enq.elapsed() < ACCEPT_POLL_BUDGET,
            "入队本身必须非阻塞（实得 {:?}）",
            t_enq.elapsed()
        );
    }

    /// **判据（② accept 超时 ⇒ WouldBlock；容量口径）**：两源皆空 ⇒ 一个 poll 预算后
    /// `WouldBlock`（调用方按 Retry 节拍续查停止位）；队列满 ⇒ `IntakeFull` 带
    /// 当刻深度/容量（E-q5 行的 `入口队列满 n/cap`）。
    #[test]
    fn accept_times_out_and_full_is_typed_with_depth() {
        let (intake, tx) = ServiceIntake::quic_only(2).expect("intake");
        let t0 = std::time::Instant::now();
        let e = intake.accept().expect_err("两源皆空应超时");
        assert_eq!(e.kind(), io::ErrorKind::WouldBlock);
        assert!(
            t0.elapsed() >= ACCEPT_POLL_BUDGET && t0.elapsed() < ACCEPT_POLL_BUDGET * 4,
            "超时预算区间（实得 {:?}）",
            t0.elapsed()
        );
        tx.try_enqueue(quic_end()).expect("第 1 条");
        tx.try_enqueue(quic_end()).expect("第 2 条");
        let full = tx.try_enqueue(quic_end()).expect_err("第 3 条必被拒");
        assert_eq!(full.queued, 2, "拒绝当刻深度");
        assert_eq!(full.capacity, 2, "容量 = 在册上限 + K（由调用方给定）");
        assert_eq!(tx.depth(), 2);
        assert_eq!(tx.totals(), (2, 1), "（入队成功, 被拒）");
        // 服务侧取走一条后名额即释放（容量按在队列条数计）
        let _ = intake.accept().expect("取一条");
        tx.try_enqueue(quic_end()).expect("取走后名额释放");
    }

    /// **判据（⑤ 就绪 fd 登记 = 服务侧 poll 的真实目标）**：登记的 fd 可被 poll(2)
    /// 直接使用——QUIC 入队后登记集即报可读（term 重写后的监听循环就靠它）。
    #[test]
    fn ready_fds_are_pollable_and_signal_quic_arrival() {
        let (intake, tx) = ServiceIntake::quic_only(4).expect("intake");
        let fds = intake.ready_fds();
        assert_eq!(fds.len(), 1, "QUIC-only 形态：登记集 = 唤醒读端");
        let mut pfd = libc::pollfd {
            fd: fds[0],
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 50) };
        assert_eq!(r, 0, "未入队 ⇒ 登记 fd 不可读（不是恒可读的假信号）");
        tx.try_enqueue(quic_end()).expect("入队");
        let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 50) };
        assert_eq!(r, 1, "入队 ⇒ 登记 fd 立即可读（唤醒面）");
        assert_ne!(pfd.revents & libc::POLLIN, 0);
    }

    /// **判据（电平触发的收口）**：队列取空后唤醒字节被收干 ⇒ 后续 poll **不再**恒可读
    /// （否则服务侧会空转成忙等）。
    #[test]
    fn wake_pipe_is_drained_when_queue_becomes_empty() {
        let (intake, tx) = ServiceIntake::quic_only(4).expect("intake");
        tx.try_enqueue(quic_end()).expect("入队");
        let _ = intake.accept().expect("取走唯一一条");
        let mut pfd = libc::pollfd {
            fd: intake.ready_fds()[0],
            events: libc::POLLIN,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 50) };
        assert_eq!(r, 0, "队列已空 ⇒ 唤醒字节必须已收干");
        // 再来一条仍能唤醒（收干不影响后续入队的信号）
        tx.try_enqueue(quic_end()).expect("入队 2");
        let r = unsafe { libc::poll(&mut pfd as *mut libc::pollfd, 1, 50) };
        assert_eq!(r, 1, "后续入队照常唤醒");
    }
}
