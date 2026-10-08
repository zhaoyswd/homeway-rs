//! 数据面的 **TUN fd 侧**（M1 设计 §2.4/§6.4）：专用 std 读线程 + 有界回程队列 + 专用
//! std 写线程。形态镜像 `homeway-core::wgcore` 的 `tun_read_loop` / `write_fd_all`
//! （M0 设计 §3.6-6：**阻塞 syscall 只许留在专用 std 线程**，岛内只做非阻塞）。
//!
//! 与 `wgcore` 的两处**有意差异**（都写在明面上）：
//!
//! 1. **零 `std::thread::sleep`**（隔离门层 3 禁阻塞 sleep）：`wgcore` 在「读返 0 且
//!    poll 立返可读」的空读形态用一次地板睡眠兜底；本模块改用 **`poll` 一份小片**
//!    （[`DEAD_CONFIRM_DELAY`]）——同样是「等一小段再复看」，但不引入 `sleep`。
//! 2. **本模块不做 fd 判死的分类**：判死（`POLLHUP/POLLERR/POLLNVAL`、`read` 返错、
//!    写超预算）一律**记行 + 投 `Cmd::TunFdDead`**（镜像 `wgcore` 的同名命令），由岛
//!    线程按既有取值集打 `fd` 分类（`mark_unhealthy_if_current(gen,"fd")` 的岛侧落点）。
//!
//! fd 所有权：**在扩展**（`Cmd::TunAttach` 的语义），本模块**从不 close**；线程退出靠
//! ①岛收工（命令通道断 ⇒ `send` 返 `Err` ⇒ 读线程自退，M0 设计 §3.6-2④）或 ②fd 失效
//! （读/写返错）。两个线程**分离**（不 join）——与 `wgcore` 同形，收工预算不押在阻塞读上。

use std::io;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cmd::{Cmd, Logf};
use crate::driver::IslandTx;

/// TUN 读线程名（与 `wgcore` 的 `homeway-tun-read` 同形）。
pub(crate) const READ_THREAD: &str = "homeway-tun-read";
/// 回程写线程名（`wgcore` 的写面在引擎线程里做，岛侧独立成线程 ⇒ 新名）。
pub(crate) const WRITE_THREAD: &str = "homeway-tun-write";

/// 回程队列上限（设计 §6.4 矩阵：**2048 条**≈2.6MB @1280B；满 ⇒ 丢新 + 计数）。
pub(crate) const RETURN_QUEUE_MAX: usize = 2048;

/// TUN 读线程 → 岛的**在途上限**（设计 §6.4 矩阵：**有界通道 4096 条**≈5MB @1280B；
/// 满 ⇒ **丢新 + 计数**（归 `未登记`——「投递不进」面；TUN 读线程**不阻塞**）。
///
/// 落法（**形态偏离，语义等价**，见 commit 偏离说明）：岛的命令通道是 M0 的 unbounded
/// `Cmd` 通道（控制面命令也要走它、必须不阻塞），把整条通道改成有界会波及控制面；这里
/// 在**生产侧**用一枚原子计数器把 `Cmd::TunPacket` 的在途条数卡在 4096（单生产者 +
/// 岛侧消费即减 ⇒ 计数精确、无竞态），满则丢新。**等价于一个容量 4096 的有界队列**。
pub(crate) const CMD_INFLIGHT_MAX: usize = 4096;

/// 丢弃上报的**批量**粒度：读线程每攒够这么多条 `丢新` 才投一条 `DatagramDropped`
/// （前提见 [`CMD_INFLIGHT_MAX`] 的等价性说明）；计数仍精确，尾批（≤63 条）在岛收工后
/// 无人可报（丢失，登记在案）。
const DROP_REPORT_BATCH: u64 = 64;

/// poll 片长（镜像 `wgcore::POLL_SLICE`；stop/退出判定的粒度就是它）。
const POLL_SLICE: Duration = Duration::from_millis(500);
/// 空读判死的确认片（镜像 `wgcore::DEAD_CONFIRM_DELAY` 的量级；用 poll 而非 sleep）。
const DEAD_CONFIRM_DELAY: Duration = Duration::from_millis(50);
/// TUN 写总预算（镜像 `wgcore::write_fd_all` 的 5s——超预算按超时收 ⇒ 记行 + 卸面）。
const WRITE_BUDGET: Duration = Duration::from_secs(5);

/// TUN 面的共享计数（**语义镜像** `wgcore::TunCounters`：需求信号 + 字节对表；
/// `write_pkts` 是岛侧新增的回程包计数——快照 `packets_out` 的源）。
#[derive(Debug, Default)]
pub(crate) struct TunCounters {
    /// 读（应用出站方向）累计字节。
    pub(crate) read_bytes: AtomicU64,
    /// 写（应用入站方向）累计字节；同时是回程写线程的活体证据。
    pub(crate) write_bytes: AtomicU64,
    /// 写（应用入站方向）累计**包**数（快照 `packets_out`）。
    pub(crate) write_pkts: AtomicU64,
    /// App 出站包计数（demand-driven-recovery D1：巡检拍「取走清零」消费）。
    pub(crate) out_pkts: AtomicI64,
    /// 最近出站包时刻（unix nano；0 = 本世代从未有过应用出站）。
    pub(crate) last_outbound_ns: AtomicI64,
    /// 同一时刻的单调相对读数（自进程起点的 ns；下推器 D4 的时基——评审核对
    /// `wgcore` 的 r2-H1 口径：单调面与 unix 面分离）。
    pub(crate) last_outbound_mono_ns: AtomicI64,
    /// `Cmd::TunPacket` 在途条数（**有界通道的等价实现**；见 [`CMD_INFLIGHT_MAX`]）。
    inflight: AtomicU64,
}

impl TunCounters {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 写面累计包数（快照 `packets_out` 的读面）。
    pub(crate) fn write_pkts(&self) -> u64 {
        self.write_pkts.load(Ordering::Relaxed)
    }

    /// 取走并清零 App 出站包计数（镜像 `wgcore::Client::swap_out_pkts` 的语义）。
    pub(crate) fn swap_out_pkts(&self) -> i64 {
        self.out_pkts.swap(0, Ordering::Relaxed)
    }

    /// 占一个在途名额（满 ⇒ `false` = **丢新**；调用方计数不投递）。
    pub(crate) fn try_begin_send(&self) -> bool {
        let mut cur = self.inflight.load(Ordering::Relaxed);
        loop {
            if cur as usize >= CMD_INFLIGHT_MAX {
                return false;
            }
            match self
                .inflight
                .compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(now) => cur = now,
            }
        }
    }

    /// 交还在途名额（岛侧消费掉一条 `TunPacket`，或读线程投递失败）。
    pub(crate) fn done_send(&self) {
        let mut cur = self.inflight.load(Ordering::Relaxed);
        while cur > 0 {
            match self.inflight.compare_exchange_weak(
                cur,
                cur - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }

    /// 最近出站包时刻（**单调面**换算；`None` = 本世代从未有过应用出站）——
    /// 镜像 `wgcore::Client::last_outbound_at`（评审 r2-H1：不许用 unix 面换算 `Instant`）。
    pub(crate) fn last_outbound_at(&self) -> Option<Instant> {
        let ns = self.last_outbound_mono_ns.load(Ordering::Relaxed);
        if ns <= 0 {
            return None;
        }
        process_mono_start().checked_add(Duration::from_nanos(ns as u64))
    }
}

/// 进程单调起点（`last_outbound_mono_ns` 的基准；懒初始化——与 `wgcore` 同款）。
fn process_mono_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// 回程通道（岛 → TUN）：有界队列的**生产者面**（消费者 = 写线程）。
///
/// 生产者 = 岛线程内的回程泵（[`crate::client::dataplane::pump_return`]）；`try_push`
/// **非阻塞**（绝不把阻塞传染进岛 runtime）。
pub(crate) struct ReturnPath {
    tx: SyncSender<Box<[u8]>>,
}

/// 投递结果（**两种失败必须可分**：满 = 队列在但来不及排空；Gone = 消费者已死）。
///
/// 为什么分开（代码门 r13 的 A3）：写线程因 fd 失效退出后（`write_loop` 投 `Cmd::TunFdDead`），
/// 队列仍在、`try_push` 恒失败——若与「队列满」同归一类，日志会写成
/// `回程队列满（上限 2048 条）`，与事实（没有消费者）不符，误导排障；且泵会一直空转
/// 拷贝 + 丢弃。`Gone` 让泵**收口并如实记因**。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PushOutcome {
    Pushed,
    /// 队列满（丢新；调用方计 `回程队列满`）。
    Full,
    /// 写线程已退（fd 失效/收工）：泵应收口，不再逐包计 `回程队列满`。
    Gone,
}

impl ReturnPath {
    /// 起写线程并接上队列（fd 的所有权在扩展；本模块不 close）。
    pub(crate) fn new(
        fd: i32,
        tx: IslandTx,
        counters: Arc<TunCounters>,
        logf: Logf,
    ) -> io::Result<Arc<Self>> {
        let (queue_tx, rx) = sync_channel::<Box<[u8]>>(RETURN_QUEUE_MAX);
        std::thread::Builder::new()
            .name(WRITE_THREAD.into())
            .spawn(move || write_loop(fd, rx, tx, counters, logf))?;
        Ok(Arc::new(Self { tx: queue_tx }))
    }

    /// 投一包（满 ⇒ `Full`，消费者已死 ⇒ `Gone`；两者都不阻塞）。
    pub(crate) fn try_push(&self, pkt: Vec<u8>) -> PushOutcome {
        match self.tx.try_send(pkt.into_boxed_slice()) {
            Ok(()) => PushOutcome::Pushed,
            Err(TrySendError::Full(_)) => PushOutcome::Full,
            // 写线程已退（fd 失效/收工）：**单独一类**（丢弃语义同「丢」，但归因不同——见 PushOutcome）
            Err(TrySendError::Disconnected(_)) => PushOutcome::Gone,
        }
    }
}

/// 起 TUN 读线程（**分离**——不等 join；退出面见模块头）。
pub(crate) fn spawn_reader(
    fd: i32,
    tx: IslandTx,
    counters: Arc<TunCounters>,
    logf: Logf,
) -> io::Result<()> {
    std::thread::Builder::new()
        .name(READ_THREAD.into())
        .spawn(move || read_loop(fd, tx, counters, logf))?;
    Ok(())
}

/// 单 fd poll 等待（片长 [`POLL_SLICE`]；EINTR 内部重试；到 `deadline` 返 `TimedOut`；
/// 片到无事件 = `Ready::default()`）。形态镜像 `wgcore::poll_fd`。
fn poll_fd(fd: i32, events: libc::c_short, deadline: Instant) -> io::Result<Ready> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "tun fd 等待超预算"));
        }
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let slice = POLL_SLICE.min(deadline.saturating_duration_since(now));
        let ms = slice.as_millis().min(i32::MAX as u128) as i32;
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        if r == 0 {
            return Ok(Ready::default()); // 片到无事件：超时，不判死
        }
        let rev = pfd.revents;
        let mut ready = Ready {
            readable: rev & libc::POLLIN != 0,
            writable: rev & libc::POLLOUT != 0,
            hup: rev & libc::POLLHUP != 0,
            err: rev & libc::POLLERR != 0,
            nval: rev & libc::POLLNVAL != 0,
        };
        if !ready.readable && !ready.writable && !ready.hup && !ready.err && !ready.nval {
            ready.err = true; // 只认出未知位 ⇒ 保守异常（同 wgcore）
        }
        return Ok(ready);
    }
}

/// poll 结果（镜像 `wgcore::Ready` 的字段面）。
#[derive(Debug, Default)]
struct Ready {
    readable: bool,
    writable: bool,
    hup: bool,
    err: bool,
    nval: bool,
}

/// TUN 全量写（部分写回补 + POLLOUT 等待**有总预算**——镜像 `wgcore::write_fd_all`；
/// 预算检查写死在循环顶，不能只靠 `poll_fd` 的内部期限）。
fn write_fd_all(fd: i32, mut buf: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_BUDGET;
    while !buf.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "tun fd 写等待超预算"));
        }
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    let r = poll_fd(fd, libc::POLLOUT, deadline)?;
                    if r.hup || r.err || r.nval {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "tun fd 已失效（POLLHUP/POLLERR/POLLNVAL）——立即出线",
                        ));
                    }
                    continue; // writable 或片到 ⇒ 回循环顶复检期限
                }
                _ => return Err(e),
            }
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// 读循环（应用出站方向）：裸 read → 计数 → `Cmd::TunPacket` 投岛。
///
/// - OHOS 的 VPN fd 是**非阻塞**的（`wgcore` 真机实证）⇒ EAGAIN 走 poll(POLLIN, 一片)；
/// - `n == 0`（EOF/空读）：`hup|err|nval` ⇒ 判死（记行 + `TunFdDead` + 退出），否则
///   用一小片 poll 兜底（替代 `wgcore` 的地板睡眠）后继续；
/// - 投包失败（岛已收工/通道断）⇒ **自退**（M0 设计 §3.6-2④；不重试、不挂死）；
/// - fd 读错 ⇒ 记行 + `TunFdDead`（岛侧打 `fd` 分类）+ 退出。
fn read_loop(fd: i32, tx: IslandTx, counters: Arc<TunCounters>, logf: Logf) {
    let mut buf = vec![0u8; 65535];
    let mut pending_drops: u64 = 0;
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    // 非阻塞 fd 的常态：等 POLLIN 一片（片界即退出判定点）。
                    // 三分处置（镜像 `wgcore`）：片到（TimedOut）= 继续；poll 出错 = 判死。
                    match poll_fd(fd, libc::POLLIN, Instant::now() + POLL_SLICE) {
                        Ok(_ready) => {} // 可读优先（含 POLLIN|POLLHUP 同置：先读，下一轮定性）
                        Err(pe) if pe.kind() == io::ErrorKind::TimedOut => {}
                        Err(pe) => {
                            let msg = format!("tun fd poll 失败：{pe}（标记隧道不健康）");
                            (logf)(&msg);
                            let _ = tx.send(Cmd::TunFdDead { msg });
                            return;
                        }
                    }
                }
                _ => {
                    let msg = format!("tun fd 读取失败：{e}（标记隧道不健康）");
                    (logf)(&msg);
                    let _ = tx.send(Cmd::TunFdDead { msg });
                    return;
                }
            }
            continue;
        }
        if n == 0 {
            // EOF/空读：判死**只看 hup||err||nval**（darwin 上 EOF 恒带 POLLIN），
            // 且判死前确认一片（poll 本身即等待——不用 sleep）。
            let first = poll_fd(fd, libc::POLLIN, Instant::now() + POLL_SLICE);
            let (dead, readable) = match &first {
                Ok(r) => (r.hup || r.err || r.nval, r.readable),
                Err(pe) if pe.kind() == io::ErrorKind::TimedOut => (false, false),
                Err(_) => (true, false),
            };
            let dead = if dead {
                match poll_fd(fd, libc::POLLIN, Instant::now() + DEAD_CONFIRM_DELAY) {
                    Ok(r) => r.hup || r.err || r.nval,
                    Err(pe) if pe.kind() == io::ErrorKind::TimedOut => false,
                    Err(_) => true, // poll 本身失败（EBADF/EIO…）⇒ 判死
                }
            } else {
                false
            };
            if dead {
                let msg = "tun fd 已失效（POLLHUP/POLLERR/POLLNVAL，确认一片后仍成立）";
                (logf)(&format!("{msg}（标记隧道不健康）"));
                let _ = tx.send(Cmd::TunFdDead { msg: msg.into() });
                return;
            }
            // 「read 返 0 且 poll 立返可读」的空读形态：一小片兜底（防热自旋；
            // 本模块不用 sleep——用 poll 片等价实现，见模块头）。
            if readable {
                let _ = poll_fd(fd, libc::POLLIN, Instant::now() + DEAD_CONFIRM_DELAY);
            }
            continue;
        }
        let mut n = n as usize;
        // packet-info 头自动探测（镜像 `wgcore` 的 r2-L4：4 字节 PI + 合法 IP 版本号才剥）
        let looks_like_pi = n >= 5
            && ((buf[2] == 0x08 && buf[3] == 0x00 && buf[4] >> 4 == 4)
                || (buf[2] == 0x86 && buf[3] == 0xdd && buf[4] >> 4 == 6));
        if n >= 5 && buf[0] == 0 && buf[1] == 0 && looks_like_pi {
            buf.copy_within(4..n, 0);
            n -= 4;
        }
        let pkt: Box<[u8]> = buf[..n].to_vec().into_boxed_slice();
        counters.read_bytes.fetch_add(n as u64, Ordering::Relaxed);
        // 需求信号（demand-driven-recovery D1：只计 App 源；巡检拍取走清零）
        counters.out_pkts.fetch_add(1, Ordering::Relaxed);
        counters.last_outbound_ns.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
            Ordering::Relaxed,
        );
        counters.last_outbound_mono_ns.store(
            process_mono_start().elapsed().as_nanos() as i64,
            Ordering::Relaxed,
        );
        // 在途闸（设计 §6.4 的「有界通道 4096 条」等价实现）：满 ⇒ **丢新 + 计数**，
        // 读线程**不阻塞**（丢的包由端到端 TCP 重传吸收）。
        if !counters.try_begin_send() {
            pending_drops += 1;
            if pending_drops >= DROP_REPORT_BATCH {
                let _ = tx.send(Cmd::DatagramDropped {
                    reason: crate::cmd::DropReason::Unregistered,
                    n: pending_drops,
                });
                pending_drops = 0;
            }
            continue;
        }
        if tx.send(Cmd::TunPacket(pkt)).is_err() {
            // 岛已收工（通道断）——读线程自退（不重试、不挂死）
            counters.done_send();
            (logf)("homeway-tun-read: 投递失败（岛已收工）—— 读线程退出");
            return;
        }
        if pending_drops > 0 {
            let _ = tx.send(Cmd::DatagramDropped {
                reason: crate::cmd::DropReason::Unregistered,
                n: pending_drops,
            });
            pending_drops = 0;
        }
    }
}

/// 写循环（应用入站方向）：有界队列 → `write_fd_all`（含期限）。
///
/// 退出：通道断（岛收工/回程泵随连接结束而 drop）/ fd 写失败（记行 + `TunFdDead`
/// ⇒ 岛侧打 `fd` 分类 + 卸面）。
fn write_loop(
    fd: i32,
    rx: Receiver<Box<[u8]>>,
    tx: IslandTx,
    counters: Arc<TunCounters>,
    logf: Logf,
) {
    loop {
        let pkt = match rx.recv() {
            Ok(p) => p,
            Err(_) => return, // 生产者全 drop（岛收工）⇒ 写线程退出
        };
        match write_fd_all(fd, &pkt) {
            Ok(()) => {
                counters.write_bytes.fetch_add(pkt.len() as u64, Ordering::Relaxed);
                counters.write_pkts.fetch_add(1, Ordering::Relaxed);
            }
            Err(e) => {
                let msg = format!("tun fd 写入失败：{e}（标记隧道不健康）");
                (logf)(&msg);
                let _ = tx.send(Cmd::TunFdDead { msg });
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;

    use super::*;

    /// 测试用投递口（无接收者——写线程的 `TunFdDead` 投递恒失败，与「岛已收工」同面；
    /// `#[cfg(test)]` 缝定义在 `driver.rs`，本文件不命名异步栈类型）。
    fn cmd_tx() -> IslandTx {
        IslandTx::dead_for_test()
    }

    /// 写面：有界队列 → `write_fd_all`（真 fd：socketpair 的另一端），包尺寸/内容逐字节。
    #[test]
    fn write_path_round_trips_through_bounded_queue() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (logf, _rx) = (Arc::new(|_s: &str| {}) as Logf, ());
        let counters = TunCounters::new();
        let ret =
            ReturnPath::new(a.as_raw_fd(), cmd_tx(), Arc::clone(&counters), logf).expect("写线程可起");
        assert_eq!(ret.try_push(vec![1, 2, 3, 4]), PushOutcome::Pushed);
        assert_eq!(ret.try_push(vec![9; 64]), PushOutcome::Pushed);
        let mut got = [0u8; 4];
        (&b).read_exact(&mut got).expect("读到第一包");
        assert_eq!(got, [1, 2, 3, 4]);
        let mut got2 = [0u8; 64];
        (&b).read_exact(&mut got2).expect("读到第二包");
        assert_eq!(got2, [9u8; 64]);
        // 计数（有界轮询上界：只判上界，flake 口径②）
        let deadline = Instant::now() + Duration::from_secs(2);
        while counters.write_pkts() < 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(counters.write_pkts(), 2, "回程包计数（快照 packets_out 的源）");
        assert_eq!(counters.write_bytes.load(Ordering::Relaxed), 68);
    }

    /// **判据（设计 §6.4 矩阵：TUN 读线程 → 岛的「有界通道 4096 条」）**：在途闸满 ⇒
    /// `try_begin_send` 恒 `false`（读线程据此**丢新 + 计数**，不阻塞）；消费/失败交还
    /// 名额后恢复（计数精确 —— 单生产者 + 岛侧消费即还）。
    #[test]
    fn tun_inflight_is_bounded_and_reopens_after_release() {
        let c = TunCounters::new();
        for i in 0..CMD_INFLIGHT_MAX {
            assert!(c.try_begin_send(), "第 {} 个名额应可占", i + 1);
        }
        assert!(
            !c.try_begin_send(),
            "第 {} 个必须被拒（上限 {CMD_INFLIGHT_MAX}）",
            CMD_INFLIGHT_MAX + 1
        );
        c.done_send();
        assert!(c.try_begin_send(), "交还一个名额后应可再占");
        // 交还到 0 后不越界（`done_send` 幂等保护：0 不再减）
        for _ in 0..(CMD_INFLIGHT_MAX + 8) {
            c.done_send();
        }
        assert_eq!(c.inflight.load(Ordering::Relaxed), 0, "在途计数不得下溢");
        assert!(c.try_begin_send(), "归零后仍可用");
    }

    /// 队列满：`try_push` 非阻塞返 `Full`（岛侧据此计 `回程队列满`）。
    ///
    /// 构造：socketpair 的读端**不读**且先灌满内核缓冲 ⇒ 写线程阻塞在第一包上 ⇒
    /// 队列被填满至上限，此后 `try_push` 恒 `Full`（不阻塞、不覆盖最旧）。
    #[test]
    fn return_queue_full_is_non_blocking() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        a.set_nonblocking(true).expect("置非阻塞（灌缓冲用）");
        // 灌满内核发送缓冲（方向与岛的写线程一致：都写进 b 的接收缓冲）
        let filler = vec![0u8; 256 * 1024];
        let mut written = 0usize;
        while written < filler.len() {
            match (&a).write(&filler[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(_) => break, // WouldBlock = 缓冲已满
            }
        }
        assert!(written > 0, "至少写进一些（缓冲非零）");
        let mut got = [0u8; 1];
        assert!(
            b.set_nonblocking(true).is_ok(),
            "读端也置非阻塞（本用例不读，只求不阻塞）"
        );
        let _ = (&b).read(&mut got); // 保持 b 打开（fd 所有权在调用侧）
        let (logf, _rx) = (Arc::new(|_s: &str| {}) as Logf, ());
        let counters = TunCounters::new();
        let ret = ReturnPath::new(a.as_raw_fd(), cmd_tx(), counters, logf).expect("写线程可起");
        let mut pushed = 0usize;
        for _ in 0..(RETURN_QUEUE_MAX + 16) {
            if ret.try_push(vec![0u8; 8]) == PushOutcome::Pushed {
                pushed += 1;
            }
        }
        assert!(
            pushed >= RETURN_QUEUE_MAX,
            "队列应被填满（RETURN_QUEUE_MAX={RETURN_QUEUE_MAX}，实际入队 {pushed}）"
        );
        // 上限面：写线程最多再取走 1 条（取走后卡在 write 上）⇒ 接受数 ≤ 上限 + 1；
        // 其余 `try_push` 必须**非阻塞返 `Full`**（岛侧据此计 `回程队列满`）。
        assert!(
            pushed <= RETURN_QUEUE_MAX + 1,
            "队列不得越过上限（RETURN_QUEUE_MAX={RETURN_QUEUE_MAX}，实际入队 {pushed}）"
        );
    }

    /// **判据（代码门 r13 的 A3）**：消费者（写线程）已退 ⇒ `try_push` 归 `Gone`（**不是**
    /// `Full`）——岛侧据此收口回程泵并如实记因，不把「没有消费者」误报成「队列满」。
    ///
    /// 构造：socketpair 读端先 drop ⇒ 写线程首次 `write_fd_all` 得 EPIPE ⇒ `write_loop`
    /// 记行 + 投 `TunFdDead` + `return`（`rx` 随之 drop）⇒ 队列进入 `Disconnected`。
    #[test]
    fn return_push_reports_gone_when_consumer_exits() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        drop(b); // 读端消失：写线程下一次写必然失败
        let (logf, _rx) = (Arc::new(|_s: &str| {}) as Logf, ());
        let counters = TunCounters::new();
        let ret = ReturnPath::new(a.as_raw_fd(), cmd_tx(), counters, logf).expect("写线程可起");
        // 首包进队（写线程随后取走并失败退出）；之后必须观测到 `Gone`（有界轮询上界）
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut outcome = ret.try_push(vec![0u8; 8]);
        while outcome != PushOutcome::Gone && Instant::now() < deadline {
            std::thread::yield_now();
            outcome = ret.try_push(vec![0u8; 8]);
        }
        assert_eq!(
            outcome,
            PushOutcome::Gone,
            "写线程已退 ⇒ 必须归 `Gone`（不得与「队列满」混同）"
        );
    }
}
