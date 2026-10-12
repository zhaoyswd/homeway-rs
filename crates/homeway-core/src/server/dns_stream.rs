//! DNS 代答的**流服务面**（`STREAM[tag=6]` dns-resolve；DNS-STREAM 批，2026-10-12）。
//!
//! **消费方** = 宿主侧 socks 承载面的「远程解析腿」（`daemon/carriers/dnsq.rs` 的
//! [`crate::daemon::carriers::dnsq::SOCKS_DNS_PORT`]）：客户端把 DNS-over-TCP **裸分帧**
//! （2B BE 长度 + 报文）经流送出，本服务逐帧交 [`DnsProxy::answer_stream`] 代答——与出口
//! 栈内 `:5300` TCP 面（`intercept::dnsface`）同一条 dnsproxy 管线、同一个 `qtcp` 计数面。
//!
//! **与既有服务（files/term/speedtest）的两个有意差异**（设计门 r33 逐条处置）：
//!
//! 1. **在册闸是真的**（2-1，高）：`ServiceIntake` 的容量**只约束队列条数**，取走即释放
//!    （`exit/intake.rs`）⇒ 不能当在册上限用。本服务自带 [`MAX_CONNS`] 原子闸（RAII 守卫）：
//!    超限**直接关流**（drop socketpair ⇒ 泵 `finish()` ⇒ 客户端立刻读 EOF，比 5s 超时更可
//!    归因）。也因此「容量 = 在册上限 + K ⇒ 应用层 busy 先于 intake 满」这条 intake 不变量
//!    **对本服务不适用**（本服务没有应用层 busy 面；0x23 是唯一拒入）。
//! 2. **读期限是每帧绝对**（2-2，中）：`SO_RCVTIMEO` 是 per-syscall，只设一次会被「每 <预算
//!    送 1 字节」的滴流无限续命 ⇒ 逐 syscall 收敛到帧首算起的绝对预算（同 `dnsproxy` 的
//!    `DeadlineIo` 口径）。
//!
//! **内存账（2-1 订正 + 代码门 r34 L3 算清）**：每连接 = 线程栈 256 KiB（照
//! `facade/portfwd.rs` 的 `PF_THREAD_STACK` 先例；files/term 用默认 2 MiB 栈）+ socketpair
//! 4 面装填值（`tuning.rs` 的 `SOCKPAIR_BYTES` 64 KiB/面 ⇒ 256 KiB；口径照 `exit/pump.rs`
//! 「按设置值记」、不乘内核系数）+ 泵缓冲 8 KiB ⇒ **≈520 KiB/连接**；闸上限 64 ⇒
//! **最坏 ≈33 MiB**（原设计稿「≈16 MiB」漏算 socketpair 与泵缓冲，本批两轮订正后以此式为准）。
//!
//! **帧异常面**（与栈内 TCP 面逐条同形）：`len=0` ⇒ 收线（Go「空 TCP 消息」判异常；**不计
//! `qtcp`**——计数发生在读帧之后）；畸形（len<12）⇒ 交 dnsproxy（`malformed` +1）且**不回帧**；
//! 丢弃/无应答（`answer_stream` 返 `None`）⇒ 不回帧（客户端按自身 5s 预算超时归因）；
//! 应答长度域 > `u16::MAX` ⇒ **收线**（镜像 `dnsface::deliver_tcp`；静默截断会把帧语义写坏）。
//! 应答下界 ≥12B 由 `dnsproxy` 保证（能过 `qtype` 的查询必得 ≥12B 应答或 SERVFAIL）——
//! 本层不另设长度下界（代码门 r34 L7）。

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_quic::ServiceIntake;

use crate::Logf;
use crate::files_server::serve_stoppable_accepts;
use crate::server::dnsproxy::DnsProxy;

/// 在册连接上限（**真闸**；与栈内 TCP 面 `dnsface::MAX_TCP_CONNS` 同阶 = 64）。
pub const MAX_CONNS: usize = 64;
/// 单帧（含 2B 长度前缀）的**绝对**读取预算：从该帧首字节等待算起、逐 syscall 收敛。
/// 客户端形态 = 一帧一问一答即关 ⇒ 10s 只服务「慢客户端」的余量，不作常态等待。
pub const FRAME_BUDGET: Duration = Duration::from_secs(10);
/// 应答写预算（per-syscall 余量；帧小的正常写入远快于此）。
pub const WRITE_BUDGET: Duration = Duration::from_secs(5);
/// 连接线程栈（照 `PF_THREAD_STACK` 先例；内存账依据见模块头）。
const THREAD_STACK: usize = 256 * 1024;

/// 起服务：accept 循环（可停）+ 每连接一线程。
///
/// `stop`/返回语义与 files/speedtest 同（`serve_stoppable_accepts` 单源）：accept 循环下一拍
/// 退（≤200ms）；**在途 handler 线程不 join**（随泵/FIN 收尾，进程退出兜底）。
pub fn serve(
    intake: ServiceIntake,
    dns: Arc<DnsProxy>,
    stop: Arc<AtomicBool>,
    logf: Logf,
) -> std::io::Result<()> {
    serve_with_cap(intake, dns, stop, logf, MAX_CONNS)
}

/// [`serve`] 的在册上限可注入面（**测试用**：真闸语义的用例不必起 65 条连接）。
pub(crate) fn serve_with_cap(
    intake: ServiceIntake,
    dns: Arc<DnsProxy>,
    stop: Arc<AtomicBool>,
    logf: Logf,
    max_conns: usize,
) -> std::io::Result<()> {
    let live = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicU64::new(0));
    let spawn_fails = Arc::new(AtomicU64::new(0));
    let logf_accept = Arc::clone(&logf);
    serve_stoppable_accepts(
        || intake.accept(),
        move |conn| {
            let logf = &logf_accept;
            let n = live.fetch_add(1, Ordering::AcqRel) + 1;
            if n > max_conns {
                live.fetch_sub(1, Ordering::AcqRel);
                let k = refused.fetch_add(1, Ordering::Relaxed) + 1;
                if k <= 3 || k.is_multiple_of(100) {
                    (logf)(&format!(
                        "dns: 流腿在册已满（{n} > {max_conns}）——连接直接收线（累计拒 {k}）"
                    ));
                }
                drop(conn);
                return;
            }
            let guard = LiveGuard(Arc::clone(&live));
            let dns2 = Arc::clone(&dns);
            let logf2 = Arc::clone(logf);
            // 守卫随闭包 move：spawn 失败时闭包被 drop ⇒ 名额自动回收；线程退出时同样兜底。
            if let Err(e) = std::thread::Builder::new()
                .name("hw-dns-stream".into())
                .stack_size(THREAD_STACK)
                .spawn(move || {
                    let _g = guard;
                    handle_conn(conn, dns2, logf2);
                })
            {
                // 节流（代码门 r34 L6）：fd/内存耗尽期正是该行的放大期——首 3 + 每 100
                let k = spawn_fails.fetch_add(1, Ordering::Relaxed) + 1;
                if k <= 3 || k.is_multiple_of(100) {
                    (logf)(&format!(
                        "⚠️ dns: 流腿会话线程起不来（{e}）——连接直接收线（累计 {k}；在册名额已回收）"
                    ));
                }
            }
        },
        stop,
        &logf,
        "dns",
    )
}

struct LiveGuard(Arc<AtomicUsize>);

impl Drop for LiveGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// 一条连接：逐帧「读 2B 长度 + 读正文 → 代答 → 写 2B 长度 + 应答」，直到 EOF/异常/超限。
fn handle_conn(mut conn: UnixStream, dns: Arc<DnsProxy>, logf: Logf) {
    let _ = conn.set_write_timeout(Some(WRITE_BUDGET));
    loop {
        let deadline = Instant::now() + FRAME_BUDGET;
        let mut hdr = [0u8; 2];
        if read_exact_deadline(&mut conn, &mut hdr, deadline) != ReadOutcome::Done {
            return;
        }
        let mlen = u16::from_be_bytes(hdr) as usize;
        if mlen == 0 {
            // Go「空 TCP 消息」判异常收线；**不计 qtcp**（计数在读帧之后 ⇒ 与栈内面一致）
            return;
        }
        let mut msg = vec![0u8; mlen];
        if read_exact_deadline(&mut conn, &mut msg, deadline) != ReadOutcome::Done {
            return;
        }
        // 代答：畸形（len<12）也进来（malformed+1 且不回帧）；丢弃/无应答 ⇒ 同样不回帧。
        let Some(resp) = dns.answer_stream(&msg) else {
            continue;
        };
        let Ok(len) = u16::try_from(resp.len()) else {
            // 长度域溢出 ⇒ 收线（镜像 dnsface::deliver_tcp 的 u16 收线；静默截断 = 帧语义坏）
            (logf)(&format!(
                "dns: 流腿应答超长度域（{}B > 65535）——收线（查询 {}B）",
                resp.len(),
                msg.len()
            ));
            return;
        };
        let mut framed = Vec::with_capacity(2 + resp.len());
        framed.extend_from_slice(&len.to_be_bytes());
        framed.extend_from_slice(&resp);
        if conn.write_all(&framed).is_err() {
            return;
        }
    }
}

#[derive(PartialEq, Eq, Debug)]
enum ReadOutcome {
    Done,
    Eof,
    TimedOut,
    Err,
}

/// 读满 `buf`，**绝对期限**由 `deadline` 收敛（逐 syscall 重设 `SO_RCVTIMEO`——只设一次会被
/// 滴流（每 <预算 送 1 字节）无限续命，同 `dnsproxy` 的 `DeadlineIo` 处置）。
fn read_exact_deadline(conn: &mut UnixStream, buf: &mut [u8], deadline: Instant) -> ReadOutcome {
    let mut off = 0;
    while off < buf.len() {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return ReadOutcome::TimedOut;
        }
        let _ = conn.set_read_timeout(Some(remain));
        match conn.read(&mut buf[off..]) {
            Ok(0) => return ReadOutcome::Eof,
            Ok(n) => off += n,
            // 带超时的 socket 上，到点即 EAGAIN/WouldBlock（= TimedOut）
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return ReadOutcome::TimedOut;
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return ReadOutcome::Err,
        }
    }
    ReadOutcome::Done
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::dnsproxy::{DnsConfig, DnsProxy};

    fn noop() -> Logf {
        Arc::new(|_| {})
    }

    fn a_query(name: &str, id: u16) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&id.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        q
    }

    /// 带 N 条 A 的应答（question 段压缩指针回指；构造与 `dnsproxy` 测试同形）。
    fn a_response(name: &str, id: u16, ttl: u32, addrs: &[u32]) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&id.to_be_bytes());
        r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]);
        r.extend_from_slice(&(addrs.len() as u16).to_be_bytes());
        r.extend_from_slice(&0u16.to_be_bytes());
        r.extend_from_slice(&0u16.to_be_bytes());
        let q = a_query(name, id);
        r.extend_from_slice(&q[12..]);
        for a in addrs {
            r.extend_from_slice(&[0xC0, 0x0C, 0x00, 0x01, 0x00, 0x01]);
            r.extend_from_slice(&ttl.to_be_bytes());
            r.extend_from_slice(&4u16.to_be_bytes());
            r.extend_from_slice(&a.to_be_bytes());
        }
        r
    }

    /// 假上游（UDP，回显事务 ID + 固定应答）+ 指向它的 DnsProxy。
    fn proxy_with_fake_upstream(tag: &str, addrs: &[u32]) -> Arc<DnsProxy> {
        let up = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        let addrs: Vec<u32> = addrs.to_vec();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((_n, from)) = up.recv_from(&mut buf) else { return };
                let id = u16::from_be_bytes([buf[0], buf[1]]);
                let r = a_response("s.test", id, 30, &addrs);
                let _ = up.send_to(&r, from);
            }
        });
        let dir =
            std::env::temp_dir().join(format!("homeway-rs-dnsstream-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();
        let (p, _rx) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                ..Default::default()
            },
            noop(),
            noop(),
        );
        p
    }

    fn spawn_server(
        proxy: Arc<DnsProxy>,
        cap: usize,
    ) -> (homeway_quic::ServiceIntakeTx, Arc<AtomicBool>) {
        let (intake, tx) = ServiceIntake::quic_only(8).expect("intake");
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        std::thread::spawn(move || {
            let _ = serve_with_cap(intake, proxy, stop2, noop(), cap);
        });
        (tx, stop)
    }

    /// **判据（CA5 机制面 + 帧异常面）**：① 一帧一问一答逐字节返回（qtcp 计数 = 收到即计）；
    /// ② `len=0` ⇒ 收线且**不计** qtcp；③ 畸形（len<12）⇒ 计 qtcp 但不回帧（客户端读超时）；
    /// ④ 应答 >1232B **不截断**（`is_tcp=true`，`trunc` 不增）。
    #[test]
    fn stream_serves_frames_closes_on_len0_and_keeps_big_answers() {
        let big: Vec<u32> = (0..100).map(|i| 0x0a00_0000 | i).collect();
        let proxy = proxy_with_fake_upstream("rt", &big);
        let (tx, stop) = spawn_server(Arc::clone(&proxy), 4);

        // ① 往返（大应答：100 条 A ≈1632B > 1232 ⇒ 不截断）
        let (a, mut b) = std::os::unix::net::UnixStream::pair().unwrap();
        tx.try_enqueue(a).unwrap();
        let q = a_query("s.test", 0x2222);
        let mut framed = (q.len() as u16).to_be_bytes().to_vec();
        framed.extend_from_slice(&q);
        b.write_all(&framed).unwrap();
        b.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut hdr = [0u8; 2];
        b.read_exact(&mut hdr).unwrap();
        let n = u16::from_be_bytes(hdr) as usize;
        let mut resp = vec![0u8; n];
        b.read_exact(&mut resp).unwrap();
        assert_eq!(&resp[..2], &0x2222u16.to_be_bytes());
        assert!(resp.len() > 1232, "大应答不得按 1232 截断（len={}）", resp.len());
        let line = proxy.stats_line();
        assert!(line.contains("qtcp=1"), "stats: {line}");
        assert!(line.contains("trunc=0"), "不得截断：{line}");
        assert!(line.contains("q=0"), "流腿不计 UDP q：{line}");

        // ② len=0 ⇒ 收线（读回 EOF）且不计 qtcp
        let (a2, mut b2) = std::os::unix::net::UnixStream::pair().unwrap();
        tx.try_enqueue(a2).unwrap();
        b2.write_all(&[0, 0]).unwrap();
        b2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut one = [0u8; 1];
        assert_eq!(b2.read(&mut one).unwrap(), 0, "len=0 应收线");
        assert!(proxy.stats_line().contains("qtcp=1"), "len=0 不计 qtcp：{}", proxy.stats_line());

        // ③ 畸形（len=5 <12）⇒ 计 qtcp、不回帧（客户端读超时）
        let (a3, mut b3) = std::os::unix::net::UnixStream::pair().unwrap();
        tx.try_enqueue(a3).unwrap();
        b3.write_all(&[0, 5, 1, 2, 3, 4, 5]).unwrap();
        b3.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
        let e = b3.read(&mut one).unwrap_err();
        assert!(
            matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut),
            "畸形查询不回帧（客户端读超时）：{e:?}"
        );
        let line = proxy.stats_line();
        assert!(line.contains("qtcp=2"), "畸形计入 qtcp（收到即计）：{line}");
        assert!(line.contains("malformed=1"), "畸形计数：{line}");
        stop.store(true, Ordering::Relaxed);
    }

    /// **判据（在册闸 2-1）**：第 `cap+1` 条连接**直接关流**（读回 EOF），前面的不受影响。
    #[test]
    fn stream_live_gate_closes_extra_connections() {
        // 上游无关紧要（本测只发两条不发帧的连接 + 一条被拒连接）
        let proxy = proxy_with_fake_upstream("gate", &[0x0a00_0001]);
        let (tx, stop) = spawn_server(Arc::clone(&proxy), 2);
        let (a1, _b1) = std::os::unix::net::UnixStream::pair().unwrap();
        let (a2, _b2) = std::os::unix::net::UnixStream::pair().unwrap();
        tx.try_enqueue(a1).unwrap();
        tx.try_enqueue(a2).unwrap();
        std::thread::sleep(Duration::from_millis(300)); // 等两条 handler 起来（占住闸）
        let (a3, mut b3) = std::os::unix::net::UnixStream::pair().unwrap();
        tx.try_enqueue(a3).unwrap();
        b3.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut one = [0u8; 1];
        assert_eq!(b3.read(&mut one).unwrap(), 0, "超在册上限的连接应被直接关流（EOF）");
        stop.store(true, Ordering::Relaxed);
    }
}
