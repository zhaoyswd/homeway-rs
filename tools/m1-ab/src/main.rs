//! `tools/m1-ab` —— M1 S5-2 / S5-3③④ 的**产品路径**端到端吞吐与迁移台（harness-only）。
//!
//! 为什么需要它（真源 = `docs/reviews/M1-design.md` §8 门槛表 + §10 的 S5-2/S5-3）：
//! - **S5-2**（历史叙事；M5 C4 后只剩 quic 单臂）：承载开关两档的端到端 A/B 必须走**产品路径**
//!   （本地出口 + 世代装配的客户端核 + 合成 IP 流）——`tools/quic-probe` 是 quinn 直连
//!   的旁路（不是产品路径），`tools/quic-ab.sh cpu` 测的是内建 socket（丢 GSO 的
//!   Q-K 风险面）。本工具 = 那条产品路径的最小驱动面。
//! - **S5-3③**：经中继下行吞吐 QUIC/WG 同刻 A/B —— 同一驱动面 + `--force-relay`
//!   （把 token 里的 QUIC 直连端点改指死端口；WG 档用既有
//!   `homeway-cli token … --dead-direct`，两者都不改产品代码）。
//! - **S5-3④**：经中继**迁移**用例（连接保持 + 腿表/assoc 峰值 + pend 窗丢包量）——
//!   走 `homeway_quic::Island` 的 `Cmd::Rebind`（与 S2a/S2b 用例同源）。
//!
//! **它是 harness**：只驱动 `homeway-core::facade` / `homeway-quic` 的**公开面**，
//! 不含任何产品代码改动；产物（读数）落 /tmp。
//!
//! 用法：
//! ```text
//! m1-ab run     --token <hmw2…> --transport wg|quic [--secs 8] [--rate 0] [--window 32]
//!               [--req-size 60] [--reply-size 1252] [--reply-count 1]
//!               [--force-relay] [--bind 127.0.0.2:0] [--tag N] [--workdir DIR]
//! m1-ab migrate --token <hmw2…> [--secs 24] [--rate 100] [--window 8]
//!               [--migrate-after 8] [--alt-bind 127.0.0.2:0] [--relay ip:port]
//! ```
//! 读数形态 = `m1ab: key=value` 行（机器可解析）+ 逐秒表（`sec=N sent=… acked=…`）。

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixDatagram;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::facade::demand::DemandSignals;
use homeway_core::facade::tun_exec::TunnelExec;
use homeway_core::facade::ClientCore;
use homeway_core::identity::Identity;
use homeway_core::token;
use homeway_quic::{
    Candidate, Cmd, Island, IslandConfig, IslandCredential, IslandReply, RpkPublicKey, TokenSecret,
    Via,
};

// ---------------------------------------------------------------------------
// 参数
// ---------------------------------------------------------------------------

struct Args {
    cmd: String,
    token: String,
    transport: String,
    secs: u64,
    rate: u32,
    window: u64,
    req_size: usize,
    reply_size: usize,
    reply_count: usize,
    /// token 重写：中继端点留、QUIC 直连端点改死（= 只剩中继候选）。`None` = 不改。
    force_relay: bool,
    /// token 重写：中继端点改死端口（`--force-direct`），让岛的赛跑只剩直连候选。
    dead_relay: bool,
    bind: Option<SocketAddrV4>,
    alt_bind: Option<SocketAddrV4>,
    /// 岛 MTU 上限（M2 S5-5 的**窄路径注入缝**：`IslandConfig::mtu_cap`，
    /// 区间外取值合法——生产层会把 env/config 夹到 `[1320,1400]`，本缝刻意不夹）。
    mtu_cap: u16,
    /// QUIC 端点改写（M2 S5-3 的 300ms RTT 缝）：token 里 QUIC 类端点统统改指本地址
    /// （本地延迟代理），岛仍走产品路径。
    quic_ep: Option<String>,
    /// 外置回显目标（`ip:port`）：给了就不起内置 echo（诊断/对照用）。
    dst: Option<SocketAddrV4>,
    /// 内置 echo 的绑定地址（缺省 `127.0.0.1:0`）；诊断端口域假设用。
    echo_bind: Option<SocketAddrV4>,
    migrate_after: u64,
    relay: Option<SocketAddrV4>,
    tag: String,
    workdir: PathBuf,
}

fn usage() -> ! {
    eprintln!(
        "用法：\n  m1-ab run --token <hmw2…> --transport wg|quic [--secs 8] [--rate 0] [--window 32]\n\
         \x20            [--req-size 60] [--reply-size 1252] [--reply-count 1]\n\
         \x20            [--force-relay（中继端点留、QUIC 直连改死）| --force-direct（中继改死）]\n\
         \x20            [--bind 127.0.0.2:0] [--tag N] [--workdir DIR]\n\
         \x20            [--mtu-cap 1200（**只 migrate 档**：岛缝窄路径注入；run 档走 facade ⇒ 旋钮 = HOMEWAY_QUIC_MTU）]\n\
         \x20            [--quic-ep 127.0.0.1:P（token 的 QUIC 端点统统改指此处——延迟代理注入）]\n\
         \x20 m1-ab migrate --token <hmw2…> [--secs 24] [--rate 100] [--migrate-after 8]\n\
         \x20            [--alt-bind 127.0.0.2:0] [--relay ip:port] [--workdir DIR]"
    );
    std::process::exit(2);
}

fn parse_args() -> Args {
    let argv: Vec<String> = std::env::args().collect();
    if argv.len() < 2 || argv[1].starts_with('-') {
        usage();
    }
    let mut a = Args {
        cmd: argv[1].clone(),
        token: String::new(),
        transport: "quic".into(),
        secs: 8,
        rate: 0,
        window: 32,
        req_size: 60,
        reply_size: 1252,
        reply_count: 1,
        force_relay: false,
        dead_relay: false,
        bind: None,
        alt_bind: None,
        mtu_cap: homeway_quic::QUIC_MTU_CAP_DEFAULT,
        quic_ep: None,
        dst: None,
        echo_bind: None,
        migrate_after: 8,
        relay: None,
        tag: "run".into(),
        workdir: std::env::temp_dir().join(format!("m1-ab-{}", std::process::id())),
    };
    let mut i = 2;
    while i < argv.len() {
        let v = argv[i].as_str();
        let next = |i: &mut usize| -> String {
            *i += 1;
            argv.get(*i).cloned().unwrap_or_else(|| usage())
        };
        match v {
            "--token" => a.token = next(&mut i),
            "--transport" => a.transport = next(&mut i),
            "--secs" => a.secs = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--rate" => a.rate = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--window" => a.window = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--req-size" => a.req_size = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--reply-size" => a.reply_size = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--reply-count" => a.reply_count = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--migrate-after" => a.migrate_after = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--relay" => {
                let s = next(&mut i);
                a.relay = Some(s.parse().unwrap_or_else(|_| usage()));
            }
            "--bind" => {
                let s = next(&mut i);
                a.bind = Some(s.parse().unwrap_or_else(|_| usage()));
            }
            "--alt-bind" => {
                let s = next(&mut i);
                a.alt_bind = Some(s.parse().unwrap_or_else(|_| usage()));
            }
            "--tag" => a.tag = next(&mut i),
            "--workdir" => a.workdir = PathBuf::from(next(&mut i)),
            "--mtu-cap" => a.mtu_cap = next(&mut i).parse().unwrap_or_else(|_| usage()),
            "--quic-ep" => a.quic_ep = Some(next(&mut i)),
            "--force-relay" => a.force_relay = true,
            "--force-direct" => a.dead_relay = true,
            "--dst" => {
                let s = next(&mut i);
                a.dst = Some(s.parse().unwrap_or_else(|_| usage()));
            }
            "--echo-bind" => {
                let s = next(&mut i);
                a.echo_bind = Some(s.parse().unwrap_or_else(|_| usage()));
            }
            _ => usage(),
        }
        i += 1;
    }
    if a.token.is_empty() {
        usage();
    }
    if a.req_size < 28 + 8 || a.req_size > 1400 {
        eprintln!("!! --req-size 必须在 [36,1400]");
        std::process::exit(2);
    }
    if a.reply_size > 1252 {
        eprintln!("!! --reply-size（内层 UDP 载荷）≤1252（1280 内层 MTU 的面）");
        std::process::exit(2);
    }
    a
}

// ---------------------------------------------------------------------------
// 内层包（真源 = crates/homeway-core/tests/quic_wg_e2e.rs 的同名函数：校验和必须真算，
// 出口 smoltcp 对 IPv4 头校验和是强制校验）
// ---------------------------------------------------------------------------

fn inet_checksum(buf: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < buf.len() {
        sum += u16::from_be_bytes([buf[i], buf[i + 1]]) as u32;
        i += 2;
    }
    if i < buf.len() {
        sum += (buf[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn inner_udp(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 20 + 8 + payload.len()];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
    p[8] = 64;
    p[9] = 17;
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    let ip_sum = inet_checksum(&p[..20]);
    p[10..12].copy_from_slice(&ip_sum.to_be_bytes());
    p[20..22].copy_from_slice(&sport.to_be_bytes());
    p[22..24].copy_from_slice(&dport.to_be_bytes());
    p[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    p[28..].copy_from_slice(payload);
    let mut pseudo = Vec::with_capacity(12 + 8 + payload.len());
    pseudo.extend_from_slice(&src.octets());
    pseudo.extend_from_slice(&dst.octets());
    pseudo.push(0);
    pseudo.push(17);
    pseudo.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    pseudo.extend_from_slice(&p[20..]);
    let mut udp_sum = inet_checksum(&pseudo);
    if udp_sum == 0 {
        udp_sum = 0xffff;
    }
    p[26..28].copy_from_slice(&udp_sum.to_be_bytes());
    p
}

/// 请求载荷 = `seq(8B BE)` ‖ 填充。回显面**保留前 8B**（否则无法做丢包归因）。
fn req_payload(seq: u64, len: usize) -> Vec<u8> {
    let mut v = vec![0x5Au8; len.max(8)];
    v[..8].copy_from_slice(&seq.to_be_bytes());
    v
}

fn payload_seq(datagram: &[u8]) -> Option<u64> {
    if datagram.len() < 28 + 8 {
        return None;
    }
    let mut b = [0u8; 8];
    b.copy_from_slice(&datagram[28..36]);
    Some(u64::from_be_bytes(b))
}

// ---------------------------------------------------------------------------
// 回显面（出口 intercept 的 transit 目标；同机回环 ⇒ 不依赖外网）
// ---------------------------------------------------------------------------

/// 回显：把收到的 UDP 载荷前 8B（seq）保留、补齐/截到 `payload_len`，回 `count` 份。
fn spawn_echo(
    payload_len: usize,
    count: usize,
    bind: Option<SocketAddrV4>,
) -> (SocketAddrV4, Arc<AtomicU64>, Arc<AtomicBool>) {
    let want = bind.unwrap_or_else(|| SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    let sock = UdpSocket::bind(want).unwrap_or_else(|e| panic!("echo 可绑（{want}）：{e}"));
    let addr = match sock.local_addr().expect("echo 地址") {
        SocketAddr::V4(v) => v,
        SocketAddr::V6(_) => unreachable!(),
    };
    sock.set_read_timeout(Some(Duration::from_millis(50))).ok();
    let got = Arc::new(AtomicU64::new(0));
    let reply_bytes = Arc::new(AtomicU64::new(0));
    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        let got = Arc::clone(&got);
        let reply_bytes = Arc::clone(&reply_bytes);
        std::thread::Builder::new()
            .name("m1ab-echo".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                let mut reply = vec![0u8; payload_len.max(8)];
                while !stop.load(Ordering::SeqCst) {
                    match sock.recv_from(&mut buf) {
                        Ok((n, from)) => {
                            // ⚠️ 出口 intercept 的 UDP transit 转发的是**内层 UDP 载荷**
                            // （UDP 重拨语义），**不是内层 IP 包**——首跑把载荷当 IP 包
                            // 解析（找偏移 28）⇒ 全部静默 continue、回显面看似"死"。
                            // 载荷首 8B = seq（本工具自己的约定）。
                            if n < 8 {
                                continue; // 载荷 < 8B 带不回 seq
                            }
                            reply[..8].copy_from_slice(&buf[..8]);
                            for _ in 0..count {
                                let _ = sock.send_to(&reply, from);
                            }
                            got.fetch_add(1, Ordering::Relaxed);
                            reply_bytes.fetch_add((reply.len() * count) as u64, Ordering::Relaxed);
                        }
                        Err(_) => continue, // 超时/中断：继续（收工靠 stop）
                    }
                }
            })
            .expect("echo 线程");
    }
    (addr, got, stop)
}

/// 放大 socketpair 的收发缓冲（**测量面鲁棒性**，不是产品面）。
///
/// 为什么必需：AF_UNIX 数据报 socket 的缺省缓冲只有 ~8KB（6 个 1280B 包），而真实
/// TUN 设备的队列是内核驱动的量级（500 包 + 应用侧随时排空）。不放大时，一次 32 包
/// 突发就顶满：①回程侧（岛写 TUN）→ `回程队列满` 计数；②WG 档 → `write` 返 ENOBUFS
/// → `tun fd 写入失败` + `unhealthyReason=fd`（世代被拆）——两者都是**伪影**，会把
/// 吞吐 A/B 打成「承载停机」。首跑实测：direct-quic-r1 只有 40 包就停。
fn bump_bufs(sock: &UnixDatagram, bytes: i32) {
    use std::os::fd::AsRawFd as _;
    let fd = sock.as_raw_fd();
    let v = bytes;
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &v as *const i32 as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &v as *const i32 as *const libc::c_void,
            std::mem::size_of::<i32>() as libc::socklen_t,
        );
    }
}

// ---------------------------------------------------------------------------
// 流量计数（逐秒桶 + 逐 seq 归因：迁移窗/pend 窗的丢包量靠它算）
// ---------------------------------------------------------------------------

struct Flow {
    cap: u64,
    seq_bucket: Vec<AtomicU64>, // seq → 发送秒桶（值 = 桶+1；0 = 从未发）
    acked_seqs: Vec<AtomicBool>,
    bucket_sent: Vec<AtomicU64>,
    bucket_acked: Vec<AtomicU64>,
    sent: AtomicU64,
    sent_bytes: AtomicU64,
    acked: AtomicU64,
    acked_bytes: AtomicU64,
    down_pkts: AtomicU64,
    down_bytes: AtomicU64,
    /// TUN 写侧内核缓冲满的丢包数（socketpair 满 —— 应用侧读得慢，非承载丢包）。
    buf_full: AtomicU64,
    /// 写线程实际跑的时长（ms；0 = 尚未收束 ⇒ 用 `t0.elapsed()`）。
    elapsed_ms: AtomicU64,
    t0: Instant,
}

impl Flow {
    fn new(cap: u64, secs: u64) -> Arc<Flow> {
        let n = (secs + 8) as usize;
        Arc::new(Flow {
            cap,
            seq_bucket: (0..cap).map(|_| AtomicU64::new(0)).collect(),
            acked_seqs: (0..cap).map(|_| AtomicBool::new(false)).collect(),
            bucket_sent: (0..n).map(|_| AtomicU64::new(0)).collect(),
            bucket_acked: (0..n).map(|_| AtomicU64::new(0)).collect(),
            sent: AtomicU64::new(0),
            sent_bytes: AtomicU64::new(0),
            acked: AtomicU64::new(0),
            acked_bytes: AtomicU64::new(0),
            down_pkts: AtomicU64::new(0),
            down_bytes: AtomicU64::new(0),
            buf_full: AtomicU64::new(0),
            elapsed_ms: AtomicU64::new(0),
            t0: Instant::now(),
        })
    }

    /// 写线程收束：记实际跑时（读数分母用**写线程**的时长，不含收尾排空窗）。
    fn finish(&self, el: Duration) {
        self.elapsed_ms.store(el.as_millis() as u64, Ordering::Relaxed);
    }

    fn note_buf_full(&self) {
        self.buf_full.fetch_add(1, Ordering::Relaxed);
    }

    fn bucket(&self) -> u64 {
        self.t0.elapsed().as_secs().min((self.bucket_sent.len() - 1) as u64)
    }

    /// 记一笔「已写入隧道面的请求」（seq 递增；返回 seq）。
    fn note_sent(&self, seq: u64, bytes: usize) {
        let b = self.bucket();
        if (seq as usize) < self.seq_bucket.len() {
            self.seq_bucket[seq as usize].store(b + 1, Ordering::Relaxed);
        }
        if (b as usize) < self.bucket_sent.len() {
            self.bucket_sent[b as usize].fetch_add(1, Ordering::Relaxed);
        }
        self.sent.fetch_add(1, Ordering::Relaxed);
        self.sent_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }

    /// 记一笔回程包（seq 首次到时记 acked；放大档的重复包只计 down_*）。
    fn note_down(&self, datagram: &[u8]) {
        self.down_pkts.fetch_add(1, Ordering::Relaxed);
        self.down_bytes.fetch_add(datagram.len() as u64, Ordering::Relaxed);
        if let Some(seq) = payload_seq(datagram) {
            let s = seq as usize;
            if s >= self.seq_bucket.len() {
                return;
            }
            let b = self.seq_bucket[s].load(Ordering::Relaxed);
            if !self.acked_seqs[s].swap(true, Ordering::Relaxed) {
                self.acked.fetch_add(1, Ordering::Relaxed);
                self.acked_bytes.fetch_add(datagram.len() as u64, Ordering::Relaxed);
            }
            if b > 0 && (b - 1) < self.bucket_acked.len() as u64 {
                self.bucket_acked[(b - 1) as usize].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn render(&self, tag: &str) {
        let em = self.elapsed_ms.load(Ordering::Relaxed);
        let el = if em > 0 { em as f64 / 1000.0 } else { self.t0.elapsed().as_secs_f64() };
        let el = el.max(1e-3);
        let sent = self.sent.load(Ordering::Relaxed);
        let acked = self.acked.load(Ordering::Relaxed);
        let up_b = self.sent_bytes.load(Ordering::Relaxed);
        let down_b = self.down_bytes.load(Ordering::Relaxed);
        let loss = if sent == 0 {
            0.0
        } else {
            100.0 * (sent.saturating_sub(acked)) as f64 / sent as f64
        };
        println!("m1ab[{}]: ---------- 读数（计时窗 {:.2}s）----------", tag, el);
        println!(
            "m1ab[{}]: up.pkt={} up.bytes={} up.pps={:.1} up.mbps={:.2} tun_buf_full={}",
            tag,
            sent,
            up_b,
            sent as f64 / el,
            up_b as f64 * 8.0 / el / 1e6,
            self.buf_full.load(Ordering::Relaxed)
        );
        println!(
            "m1ab[{}]: down.pkt={} down.pkt_uniq={} down.bytes={} down.pps={:.1} down.mbps={:.2} loss.pct={:.2}",
            tag,
            self.down_pkts.load(Ordering::Relaxed),
            acked,
            down_b,
            self.down_pkts.load(Ordering::Relaxed) as f64 / el,
            down_b as f64 * 8.0 / el / 1e6,
            loss
        );
        println!("m1ab[{}]: 逐秒 sent/acked（丢包归因）", tag);
        for i in 0..self.bucket_sent.len() {
            let s = self.bucket_sent[i].load(Ordering::Relaxed);
            let a = self.bucket_acked[i].load(Ordering::Relaxed);
            if s == 0 && a == 0 {
                continue;
            }
            println!(
                "m1ab[{}]: sec={} sent={} acked={} loss={}",
                tag,
                i,
                s,
                a,
                s.saturating_sub(a)
            );
        }
    }
}

/// 写线程：`window` = 在飞上限（`sent - acked < window`）；`rate>0` 时再按 pps 节拍。
/// 写侧用**阻塞 send + 写超时**（TUN socketpair 满 ⇒ 超时返回，下一轮再试）。
fn spawn_writer(
    peer: UnixDatagram,
    flow: Arc<Flow>,
    src: Ipv4Addr,
    dst: SocketAddrV4,
    sport: u16,
    req_payload_len: usize,
    rate: u32,
    window: u64,
    dur: Duration,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("m1ab-writer".into())
        .spawn(move || {
            // **非阻塞** + 手写重试（不用 SO_SNDTIMEO：阻塞 send 在 fd 无人读时会把
            // 整个工具挂死——首跑实测踩过；纪律 = 热路径不做无期限的阻塞 syscall）。
            peer.set_nonblocking(true).ok();
            let t0 = Instant::now();
            let mut seq: u64 = 0;
            while !stop.load(Ordering::SeqCst) && t0.elapsed() < dur && seq < flow.cap {
                // 在飞窗（UDP 无流控 ⇒ 用窗口模拟「有界在飞」，把丢包限制在真实瓶颈上）；
                // **窗口等待也受 dur 约束**（否则回程不来时这里会死等——首跑实测踩过）。
                while !stop.load(Ordering::SeqCst)
                    && t0.elapsed() < dur
                    && flow.sent.load(Ordering::Relaxed).saturating_sub(flow.acked.load(Ordering::Relaxed))
                        >= window
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                if stop.load(Ordering::SeqCst) || t0.elapsed() >= dur {
                    break;
                }
                if rate > 0 {
                    let due = t0 + Duration::from_micros(1_000_000 * (seq + 1) / u64::from(rate));
                    let now = Instant::now();
                    if due > now {
                        std::thread::sleep(due - now);
                    }
                }
                let pkt = inner_udp(
                    src,
                    *dst.ip(),
                    sport,
                    dst.port(),
                    &req_payload(seq, req_payload_len),
                );
                match peer.send(&pkt) {
                    Ok(n) => {
                        flow.note_sent(seq, n);
                        seq += 1;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        // 内核缓冲满（应用读得慢）：丢 + 计数，不当成已发
                        flow.note_buf_full();
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    Err(_) => {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                }
            }
            flow.finish(t0.elapsed());
        })
        .expect("writer 线程")
}

/// 读线程（回程）：把 TUN fd 收空（不读会让内核缓冲顶住出口的回程）。
fn spawn_reader(
    peer: UnixDatagram,
    flow: Arc<Flow>,
    stop: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("m1ab-reader".into())
        .spawn(move || {
            peer.set_nonblocking(true).ok();
            let mut buf = vec![0u8; 4096];
            let mut idle = false;
            while !stop.load(Ordering::SeqCst) {
                match peer.recv(&mut buf) {
                    Ok(n) => {
                        flow.note_down(&buf[..n]);
                        idle = false; // 有数据就**不睡**（紧排空：睡 500µs 会把排空率
                                      // 压到 2000pps，一次 30Mbps 下行（3000pps）当场顶满）
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if idle {
                            std::thread::sleep(Duration::from_micros(50));
                        }
                        idle = true;
                    }
                    Err(_) => continue,
                }
            }
        })
        .expect("reader 线程")
}

// ---------------------------------------------------------------------------
// token 面小件
// ---------------------------------------------------------------------------

fn relay_label_of(tok: &token::Token) -> [u8; 8] {
    homeway_core::legframe::relay_id(tok.peer_id.as_bytes())
}

/// `--force-relay`：把 token 里的 **QUIC 直连端点（kind=2）改指 127.0.0.1:1**
/// （死端口，握手必然超时）⇒ 岛只剩中继候选。
///
/// 为什么本工具自己做而不用 `homeway-cli token --dead-direct`：那条只改写 **Direct**
/// （WG，kind=0）端点——QUIC 类的死端口注入在产品 CLI 里没有对应开关，而 S5-3③ 的
/// A/B 需要两档都经中继。**本函数只改 harness 侧送给岛的 token，不碰产品代码**。
fn token_rewrite(
    tok: &token::Token,
    dead_quic: bool,
    dead_relay: bool,
    quic_override: Option<&str>,
) -> String {
    let eps: Vec<token::EndpointRef<'_>> = tok
        .endpoints
        .iter()
        .map(|e| {
            let dead = (dead_quic && e.kind == token::EndpointKind::Quic)
                || (dead_relay && e.kind == token::EndpointKind::Relay);
            // `--quic-ep`（M2 S5-3 的 300ms RTT 注入缝）：把 QUIC 类端点改指本地延迟代理
            // ⇒ 岛仍走**产品路径**（四帧准入 + 比赛跑），只是路径 RTT 变了。
            if dead {
                token::EndpointRef::new("127.0.0.1:1", e.kind)
            } else if e.kind == token::EndpointKind::Quic {
                if let Some(ov) = quic_override {
                    return token::EndpointRef::new(ov, e.kind);
                }
                token::EndpointRef::new(&e.addr, e.kind)
            } else {
                token::EndpointRef::new(&e.addr, e.kind)
            }
        })
        .collect();
    let spec = token::TokenSpec {
        peer_id: &tok.peer_id,
        secret: &tok.secret,
        endpoints: &eps,
        rpk: tok.rpk.as_ref(),
    };
    token::encode(&spec).expect("token 重编码")
}

// ---------------------------------------------------------------------------
// mode: run（产品路径 A/B）
// ---------------------------------------------------------------------------

fn wait_until(mut f: impl FnMut() -> bool, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn run_product_ab(a: &Args) -> Result<(), String> {
    let tok = token::decode(&a.token).map_err(|e| format!("token 解析失败：{e}"))?;
    // 承载面收窄（A/B 的**同路径**保证）：
    //   --force-relay  ⇒ QUIC 直连端点改死（只剩中继候选）
    //   --force-direct ⇒ 中继端点改死（只剩直连候选；否则本地回环下 QUIC 赛跑可能被
    //                    中继抢先——首跑实测：直连臂跑成了 relay）
    let token_str = if a.force_relay || a.dead_relay || a.quic_ep.is_some() {
        token_rewrite(&tok, a.force_relay, a.dead_relay, a.quic_ep.as_deref())
    } else {
        a.token.clone()
    };
    // M5 C4：承载开关 `HOMEWAY_TRANSPORT` 已删（设计 §4 三键全删）；WG 臂随 boringtun/
    // WG 面退役 ⇒ `--transport` 只剩 `quic` 一个合法值（`wg` fail-fast，不静默出空读数）。
    if a.transport != "quic" {
        eprintln!(
            "m1-ab: --transport {} 已退役（M5 C4：WG 承载与开关删除）——只用 quic",
            a.transport
        );
        std::process::exit(2);
    }

    // 回显目标（出口 intercept 的 transit 目标；同机 127.0.0.1）
    // `--dst` 给了就用外置回显（内部不绑 socket；诊断/对照面）。
    let (echo_addr, echo_got, echo_stop) = match a.dst {
        Some(d) => {
            println!("m1ab[{}]: 用外置回显目标 {d}（内部不起 echo）", a.tag);
            (d, Arc::new(AtomicU64::new(0)), Arc::new(AtomicBool::new(true)))
        }
        None => {
            let (addr, got, stop) = spawn_echo(a.reply_size, a.reply_count, a.echo_bind);
            println!(
                "m1ab[{}]: echo 就绪 {}（reply_payload={} × {}）",
                a.tag, addr, a.reply_size, a.reply_count
            );
            (addr, got, stop)
        }
    };

    std::fs::create_dir_all(&a.workdir).map_err(|e| format!("workdir: {e}"))?;
    let out = a.workdir.join(format!("gen-{}-{}.log", a.transport, a.tag));
    // 身份**按 transport 复用**（不是按 tag）：同档多轮跑的是同一台设备（同 pubkey ⇒
    // 同派生地址 ⇒ 同一次登记被刷新），避免每轮往出口设备表塞一条新设备。
    let ident_dir = a.workdir.join(format!("identity-{}", a.transport));
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{token_str}","out":"{}","identityDir":"{}","transport":"{}"}}"#,
        out.display(),
        ident_dir.display(),
        a.transport
    );
    let demand = Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(Arc::clone(&demand));
    let core = Arc::new(ClientCore::with_shared(exec, demand));
    if core.tun_prepare(&cfg, true) != 0 {
        return Err("tun_prepare 被拒（配置非法？）".into());
    }
    let t_ready = Instant::now();
    if !wait_until(
        || core.tun_status().contains("\"state\":\"ready\""),
        Duration::from_secs(30),
    ) {
        return Err(format!("世代未 ready：{}", core.tun_status()));
    }
    let ready_ms = t_ready.elapsed().as_millis();
    let st: serde_json::Value =
        serde_json::from_str(&core.tun_status()).map_err(|e| format!("tun_status 非 JSON：{e}"))?;
    let tun_ip: Ipv4Addr = st["tunIp"]
        .as_str()
        .ok_or("状态面缺 tunIp")?
        .parse()
        .map_err(|e| format!("tunIp: {e}"))?;

    // TUN 面（socketpair；一端交核，另一端当「应用」）
    let (tun, peer) = UnixDatagram::pair().map_err(|e| format!("socketpair: {e}"))?;
    // 缓冲放大（测量面鲁棒性；见 bump_bufs 注释）——两端都放大
    bump_bufs(&tun, 4 << 20);
    bump_bufs(&peer, 4 << 20);
    if core.tun_attach(tun.as_raw_fd(), 1280) != 0 {
        return Err("tun_attach 被拒".into());
    }
    if !wait_until(
        || core.tun_status().contains("\"state\":\"attached\""),
        Duration::from_secs(20),
    ) {
        return Err(format!("世代未 attached：{}", core.tun_status()));
    }
    // 数据的通过性自检（1 发 1 收，等 1.5s）：不算读数，只为归因「承载是否真通」
    let cap = 1_000_000u64;
    let flow = Flow::new(cap, a.secs + 8);
    let stop = Arc::new(AtomicBool::new(false));
    {
        let probe = inner_udp(
            tun_ip,
            *echo_addr.ip(),
            40010,
            echo_addr.port(),
            &req_payload(0, a.req_size - 28),
        );
        let _ = peer.send(&probe);
        let mut buf = vec![0u8; 4096];
        peer.set_read_timeout(Some(Duration::from_millis(1500))).ok();
        let ok = matches!(peer.recv(&mut buf), Ok(n) if n >= 28 + 8);
        println!("m1ab[{}]: 通过性自检 first_reply={}", a.tag, ok);
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&core.tun_status()) {
            println!(
                "m1ab[{}]: 自检后快照 quic.via={} packets_in={} packets_out={} drops={}",
                a.tag, v["quic"]["via"], v["quic"]["packets_in"], v["quic"]["packets_out"], v["quic"]["drops"]
            );
        }
    }

    let writer = spawn_writer(
        peer.try_clone().map_err(|e| format!("peer 克隆: {e}"))?,
        Arc::clone(&flow),
        tun_ip,
        echo_addr,
        40011,
        a.req_size - 28,
        a.rate,
        a.window,
        Duration::from_secs(a.secs),
        Arc::clone(&stop),
    );
    let reader = spawn_reader(
        peer.try_clone().map_err(|e| format!("peer 克隆: {e}"))?,
        Arc::clone(&flow),
        Arc::clone(&stop),
    );
    writer.join().ok();
    // 收尾窗：等在飞回程排空（否则 loss 会高估）
    std::thread::sleep(Duration::from_millis(1500));
    stop.store(true, Ordering::SeqCst);
    reader.join().ok();

    // 状态面读数（岛快照 + link）
    let st2: serde_json::Value =
        serde_json::from_str(&core.tun_status()).map_err(|e| format!("tun_status 非 JSON：{e}"))?;
    println!(
        "m1ab[{}]: transport={} force_relay={} force_direct={} ready_ms={}",
        a.tag, a.transport, a.force_relay, a.dead_relay, ready_ms
    );
    println!("m1ab[{}]: tun_ip={} echo={} req_size={} reply_size={} reply_count={} rate={} window={} secs={}",
        a.tag, tun_ip, echo_addr, a.req_size, a.reply_size, a.reply_count, a.rate, a.window, a.secs);
    println!("m1ab[{}]: link={}", a.tag, st2["link"]);
    println!("m1ab[{}]: quic={}", a.tag, st2["quic"]);
    flow.render(&a.tag);
    println!(
        "m1ab[{}]: echo.rx={} echo.reply_bytes={} gen_log={}",
        a.tag,
        echo_got.load(Ordering::Relaxed),
        (a.reply_size.max(8) * a.reply_count) as u64 * echo_got.load(Ordering::Relaxed),
        out.display()
    );
    let _ = core.tun_stop();
    echo_stop.store(true, Ordering::SeqCst);
    Ok(())
}

// ---------------------------------------------------------------------------
// mode: migrate（岛级：中继候选 + Rebind 迁移；S5-3④）
// ---------------------------------------------------------------------------

fn cmd<T>(
    island: &Island,
    make: impl FnOnce(IslandReply<T>) -> Cmd,
    wait: Duration,
) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    island.tx().send(make(tx)).map_err(|e| format!("投递失败：{e}"))?;
    match rx.recv_timeout(wait) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("岛侧错误：{e}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err("回执超时（岛挂死？）".into()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("回执口断开（岛已退出）".into()),
    }
}

fn run_migrate(a: &Args) -> Result<(), String> {
    let tok = token::decode(&a.token).map_err(|e| format!("token 解析失败：{e}"))?;
    let rpk = tok.rpk.ok_or("token 未带 rpk（服务端身份，M1 必需）")?;
    let label = relay_label_of(&tok);
    let relay_addr: SocketAddrV4 = match a.relay {
        Some(v) => v,
        None => tok
            .endpoints
            .iter()
            .find(|e| e.kind == token::EndpointKind::Relay)
            .ok_or("token 无中继端点（出口未挂中继？）")?
            .addr
            .parse()
            .map_err(|e| format!("中继端点地址: {e}"))?,
    };
    let (echo_addr, echo_got, echo_stop) = spawn_echo(a.reply_size, 1, None);

    let id = Identity::ephemeral().map_err(|e| format!("身份：{e}"))?;
    let pubkey = id.public_key();
    let secret = tok.secret.clone();
    let mut cfg = IslandConfig::new(IslandCredential::new(
        TokenSecret::from_bytes(*secret.as_bytes()),
        pubkey,
        *id.dev_tag().as_bytes(),
        RpkPublicKey::from_bytes(*rpk.as_bytes()),
    ));
    cfg.bind = Some(
        a.bind
            .unwrap_or_else(|| SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
    );
    cfg.patrol = Duration::from_secs(2);
    // M2 S5-5 窄路径注入缝（`tools/m1-ab --mtu-cap`）：`IslandConfig::mtu_cap` 刻意不夹
    // 区间（生产层在 facade 侧夹 [1320,1400]）⇒ 给 1200 即得 `mds=1162 < 内层 1280`。
    cfg.mtu_cap = a.mtu_cap;
    let logf: homeway_quic::Logf =
        Arc::new(|s: &str| println!("[island] {s}"));
    let unhealthy: homeway_quic::OnUnhealthy = Arc::new(|r: &str| println!("[island][unhealthy] {r}"));
    let island = Island::start(logf, unhealthy, cfg).map_err(|e| format!("岛起不来：{e}"))?;

    let (tun, peer) = UnixDatagram::pair().map_err(|e| format!("socketpair: {e}"))?;
    bump_bufs(&tun, 4 << 20);
    bump_bufs(&peer, 4 << 20);
    cmd(
        &island,
        |reply| Cmd::TunAttach {
            fd: tun.as_raw_fd(),
            mtu: 1280,
            reply,
        },
        Duration::from_secs(5),
    )
    .map_err(|e| format!("TunAttach：{e}"))?;

    // 只给中继候选（`via=relay` 的信封路径）
    let outcome = cmd(
        &island,
        |reply| Cmd::Connect {
            cands: vec![Candidate {
                addr: relay_addr,
                via: Via::Relay { label },
            }],
            budget: Duration::from_secs(8),
            reply,
        },
        Duration::from_secs(20),
    )
    .map_err(|e| format!("Connect（中继候选）：{e}"))?;
    println!(
        "m1ab[migrate]: connect.winner={} via={:?} rtt_ms={}",
        outcome.winner, outcome.via, outcome.rtt_ms
    );

    // tun_ip = 本设备派生地址（内层包 src 必须 ∈ {tunnel_ip, tun_ip}——出口 src_allowed）
    let tun_ip = homeway_core::tunnel_addr::derive_tun_ip(&secret, &pubkey);

    let cap = 1_000_000u64;
    let flow = Flow::new(cap, a.secs + 8);
    let stop = Arc::new(AtomicBool::new(false));
    let writer = spawn_writer(
        peer.try_clone().map_err(|e| format!("peer 克隆: {e}"))?,
        Arc::clone(&flow),
        tun_ip,
        echo_addr,
        40021,
        a.req_size - 28,
        a.rate,
        a.window,
        Duration::from_secs(a.secs),
        Arc::clone(&stop),
    );
    let reader = spawn_reader(
        peer.try_clone().map_err(|e| format!("peer 克隆: {e}"))?,
        Arc::clone(&flow),
        Arc::clone(&stop),
    );

    // 到点迁移（Rebind 到另一个本地地址 ⇒ 换源 ⇒ 中继新建 assoc/腿）
    std::thread::sleep(Duration::from_secs(a.migrate_after));
    let snap_before = island.snapshot();
    let t_mig = Instant::now();
    let alt = a
        .alt_bind
        .unwrap_or_else(|| SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 0));
    let rebind = cmd(
        &island,
        |reply| Cmd::Rebind {
            local: Some(alt),
            reply,
        },
        Duration::from_secs(10),
    );
    let mig_ms = t_mig.elapsed().as_millis();
    match &rebind {
        Ok(v) => println!("m1ab[migrate]: rebind.ok local={} 耗时={}ms", v, mig_ms),
        Err(e) => println!("m1ab[migrate]: rebind.err={} 耗时={}ms", e, mig_ms),
    }

    writer.join().ok();
    std::thread::sleep(Duration::from_millis(1500));
    stop.store(true, Ordering::SeqCst);
    reader.join().ok();
    let snap_after = island.snapshot();
    println!(
        "m1ab[migrate]: before via={:?} ep={:?} migrations={} unconfirmed={} local={:?} conns={} lost={} cong={} relay_tx={} drops={:?}",
        snap_before.via, snap_before.ep, snap_before.migrations, snap_before.migration_unconfirmed,
        snap_before.local, snap_before.connections, snap_before.lost_packets,
        snap_before.congestion_events, snap_before.relay_tx, snap_before.drops
    );
    println!(
        "m1ab[migrate]: after  via={:?} ep={:?} migrations={} unconfirmed={} local={:?} conns={} packets_in={} packets_out={} lost={} cong={} relay_tx={} relays_rx_ignored={} drops={:?} mtu={:?} current_mtu={}",
        snap_after.via, snap_after.ep, snap_after.migrations, snap_after.migration_unconfirmed,
        snap_after.local, snap_after.connections, snap_after.packets_in, snap_after.packets_out,
        snap_after.lost_packets, snap_after.congestion_events, snap_after.relay_tx,
        snap_after.rx_ignored, snap_after.drops, snap_after.mtu, snap_after.current_mtu
    );
    println!(
        "m1ab[migrate]: echo.rx={} migrate_at_s={}",
        echo_got.load(Ordering::Relaxed),
        a.migrate_after
    );
    flow.render("migrate");
    let stopped = island.stop_within(Instant::now() + Duration::from_secs(3));
    println!("m1ab[migrate]: stopped_in_budget={}", stopped);
    echo_stop.store(true, Ordering::SeqCst);
    Ok(())
}

fn main() {
    let a = parse_args();
    let r = match a.cmd.as_str() {
        "run" => run_product_ab(&a),
        "migrate" => run_migrate(&a),
        _ => usage(),
    };
    if let Err(e) = r {
        eprintln!("m1ab: 失败：{e}");
        std::process::exit(1);
    }
}
