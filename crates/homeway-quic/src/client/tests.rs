//! 客户端连接面的单测（**真线程岛 + 真回环 IO + 真出口面**；不钉固定端口、时间断言只判
//! 上界——M0 设计 §9.2 flake 口径①②）。
//!
//! 本文件在异步面白名单内（`client/**`）⇒ 可以用 quinn/tokio 名字组装「测试用出口」与
//! 「引擎桩」（裁决语义真源 = `homeway-core` 的 `admit_reg4`：MAC + **本连接** exporter）。
//!
//! 覆盖的判据（设计 §10 的 S2-1/S2-2/S2-3/S2-6）：
//! - 公面成员的行为面（命令→回执/快照/行文）；
//! - **赛跑**：三候选（一活两死）⇒ 胜者/清单/`via` 正确；全死 ⇒ `NoCandidate`；
//!   错 RPK 钉定 ⇒ 握手中止（失败归 `NoCandidate`）；
//! - **登记 + 刷新**：登记成功后出口侧引擎桩收帧（真出口 `peer: +` 的等价面）；
//!   刷新按节拍（`RefreshTimer` 的 60s 虚拟时钟 + 集成面短节拍）；
//! - **迁移**：`rebind` 后 ①连接未断 ②出口 `remote_address()` 变化（E-q2 行）
//!   ③收发继续（出口发回程 ⇒ 岛侧入站证据 ⇒ N-b 行）；失败判据 = `migration_unconfirmed`；
//! - 丢弃四类计数 + 事件回调 + N-c 行（`DatagramDropped`）。

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::time::Instant as TokioInstant;

use crate::cmd::{
    Candidate, Cmd, DropReason, IslandErr, IslandEvent, IslandReply, IslandSnapshot, Logf,
    OnUnhealthy, Via,
};
use crate::config::{IslandConfig, IslandCredential, TokenSecret};
use crate::driver::seams;
use crate::exit::{EngineRejectClass, ExitInbound, ExitQuic, ExitQuicConfig, Reg4Verdict, RejectWhy};
use crate::reg4::{EXPORTER_LABEL, EXPORTER_LEN, ProofFrame, RefreshFrame};
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::{Island, IslandTx};

use super::register::{self, RefreshTimer};

/// 收工预算（与 `wgcore::CLIENT_CLOSE_BUDGET` 同量级 = 2s）。
const BUDGET: Duration = Duration::from_secs(2);
/// 异步等待上界（flake 口径②：只判上界）。
const WAIT: Duration = Duration::from_secs(8);

/// 测试用 token secret / 设备身份（「引擎桩」与岛共用）。
const SECRET: [u8; 32] = [0x5A; 32];
const PUBKEY: [u8; 32] = [0x11; 32];
const DEV: [u8; 8] = [0x22; 8];
/// 设备派生地址（桩给定；真实值 = `homeway-core` 的 `tunnel_addr` 派生）。
const TUNNEL_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 7, 1);
const TUN_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 7, 2);

fn sink() -> (Logf, Receiver<String>) {
    let (tx, rx) = channel();
    (
        Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }),
        rx,
    )
}

/// 岛侧测试入口（绑定钉成回环 `127.0.0.1:0` —— 迁移用例要从 `127.0.0.1` 起跑到 `127.0.0.2`）。
fn island_with(patrol: Duration, pin: RpkPublicKey) -> Island {
    let (logf, _rx) = sink();
    island_with_log(patrol, pin, logf)
}

/// 不健康回调 + 收集端（断言分类面：`patrol`/`fd` 的取值可达性）。
fn unhealthy_sink() -> (OnUnhealthy, Receiver<String>) {
    let (tx, rx) = channel();
    (
        Arc::new(move |r: &str| {
            let _ = tx.send(r.to_owned());
        }),
        rx,
    )
}

fn island_cfg(patrol: Duration, pin: RpkPublicKey) -> IslandConfig {
    let mut cfg = IslandConfig::new(IslandCredential::new(
        TokenSecret::from_bytes(SECRET),
        PUBKEY,
        DEV,
        pin,
    ));
    cfg.bind = Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    cfg.patrol = patrol;
    cfg
}

/// 岛侧测试入口（带不健康回调；判据：分类值 ∈ 既有取值集）。
fn island_with_unhealthy(patrol: Duration, pin: RpkPublicKey, h: OnUnhealthy) -> Island {
    let (logf, _rx) = sink();
    Island::start(logf, h, island_cfg(patrol, pin)).expect("岛可起（含 QUIC 端点）")
}

/// 岛侧测试入口（挂注入缝；测 detach 面）。
fn island_with_seam(patrol: Duration, pin: RpkPublicKey, logf: Logf, seam: u8) -> Island {
    Island::start_with_seam(logf, Arc::new(|_r: &str| {}), island_cfg(patrol, pin), seam)
        .expect("岛可起（含注入缝）")
}

fn island_with_log(patrol: Duration, pin: RpkPublicKey, logf: Logf) -> Island {
    Island::start(logf, Arc::new(|_r: &str| {}), island_cfg(patrol, pin))
        .expect("岛可起（含 QUIC 端点）")
}

/// TUN 面的测试替身：**数据报**语义的 socketpair（一端当 fd 交给岛、另一端当「应用」）。
/// 用 `UnixDatagram` 而不是 `UnixStream`：写一次 = 一个数据报 ⇒ 读侧不会把两包黏成一包
/// （TUN fd 的对表语义本来就按包）。
fn tun_pair() -> (UnixDatagram, UnixDatagram) {
    UnixDatagram::pair().expect("socketpair(DGRAM)")
}

/// 把内层包写进「应用侧」（= 岛的 TUN 读线程会读到）。
fn write_tun(peer: &UnixDatagram, pkt: &[u8]) {
    peer.send(pkt).expect("写 TUN 一包");
}

/// 从「应用侧」读回程（有界：读超时即 `None`；只判上界，flake 口径②）。
fn read_tun(peer: &UnixDatagram, wait: Duration) -> Option<Vec<u8>> {
    peer.set_read_timeout(Some(wait)).ok()?;
    let mut buf = vec![0u8; 4096];
    match peer.recv(&mut buf) {
        Ok(n) => Some(buf[..n].to_vec()),
        Err(_) => None,
    }
}

/// attach 隧道面（带 reply 的有界等待）。
fn attach(island: &Island, fd: i32, mtu: u32, wait: Duration) -> Result<(), IslandErr> {
    let (tx, rx) = channel();
    island
        .tx()
        .send(Cmd::TunAttach { fd, mtu, reply: tx })
        .map_err(|_| IslandErr::EngineGone)?;
    match rx.recv_timeout(wait) {
        Ok(v) => v,
        Err(_) => panic!("attach 回执超时（岛未应答 ⇒ 挂死）"),
    }
}

/// 有界取一条不健康分类（std 通道 + 异步等待；只判上界）。
async fn wait_reason(rx: &Receiver<String>, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    loop {
        match rx.try_recv() {
            Ok(v) => return Some(v),
            Err(TryRecvError::Disconnected) => return None,
            Err(TryRecvError::Empty) => {}
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 有界等引擎桩收到一份长度 `len` 的包（每轮 pump；返回包体）。
async fn wait_packet(
    stub: &Stub,
    quic: &ExitQuic,
    len: usize,
    wait: Duration,
) -> Option<Vec<u8>> {
    let deadline = Instant::now() + wait;
    loop {
        stub.pump(quic);
        if let Some(p) = stub.take_packet(len) {
            return Some(p);
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 「假中继」= S2-7 的**信封/剥壳仪器**（透明 UDP 代理 + 帧转换；**不含**中继控制面：
/// leg 注册/会话/限速都不实现——S2-7 的判据是信封字节，中继协议本体由 S1c 与全链 E2E 覆盖）。
///
/// 两件事与真中继在 QUIC 面上同形：
/// ① 上行 `[0xAA][label8]‖[0xBB][5]‖pkt` ⇒ 剥全部信封、**裸包**转发给出口（源 = 本代理，
///    故出口看到的对端就是「中继地址」= 候选地址）；
/// ② 出口的裸回包 ⇒ 包成 `[0xBB][5]‖pkt` 发回客户端；另推一帧 **hint**（`[0xBB][1]`，
///    非 kind=5）——真中继会推控制帧（S1c 探针实测 `rx_ignored=4`），客户端必须忽略。
struct FakeRelay {
    addr: SocketAddrV4,
    uplink_frames: Arc<AtomicU64>,
    bad_label: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl FakeRelay {
    fn start(exit: SocketAddrV4, label: [u8; 8]) -> FakeRelay {
        let sock = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("假中继可绑");
        sock.set_read_timeout(Some(Duration::from_millis(20)))
            .expect("读超时可设（片界回看停止位；不用 sleep）");
        let addr = match sock.local_addr().expect("可读地址") {
            SocketAddr::V4(v) => v,
            SocketAddr::V6(_) => unreachable!(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let uplink_frames = Arc::new(AtomicU64::new(0));
        let bad_label = Arc::new(AtomicBool::new(false));
        let (stop2, up2, bad2) = (
            Arc::clone(&stop),
            Arc::clone(&uplink_frames),
            Arc::clone(&bad_label),
        );
        let handle = std::thread::Builder::new()
            .name("fake-relay".into())
            .spawn(move || {
                let exit = SocketAddr::V4(exit);
                let mut client: Option<SocketAddr> = None;
                let mut buf = vec![0u8; 4096];
                let mut hinted = false;
                while !stop2.load(Ordering::SeqCst) {
                    let (n, src) = match sock.recv_from(&mut buf) {
                        Ok(v) => v,
                        Err(_) => continue, // 片到：回看停止位
                    };
                    if src == exit {
                        // 下行：裸 QUIC 包 ⇒ 包帧 `[0xBB][5]` 发回客户端
                        if let Some(c) = client {
                            let mut f = Vec::with_capacity(n + 2);
                            f.push(0xBB);
                            f.push(5);
                            f.extend_from_slice(&buf[..n]);
                            let _ = sock.send_to(&f, c);
                        }
                        continue;
                    }
                    // 上行：信封 `[0xAA][label8]‖[0xBB][5]‖pkt` ⇒ 剥全部 ⇒ 裸包转发出口
                    client = Some(src);
                    up2.fetch_add(1, Ordering::SeqCst);
                    if n >= 11 && buf[0] == 0xAA && buf[9] == 0xBB && buf[10] == 5 {
                        if buf[1..9] != label {
                            bad2.store(true, Ordering::SeqCst);
                        }
                        let _ = sock.send_to(&buf[11..n], exit);
                    } else {
                        bad2.store(true, Ordering::SeqCst); // 非信封形态（上行必须封）
                    }
                    if !hinted {
                        hinted = true;
                        // hint 控制帧（kind=1，非 kind=5）：客户端必须忽略且不喂 quinn
                        let _ = sock.send_to(&[0xBB, 1, b'h', b'i'], src);
                    }
                }
            })
            .expect("假中继线程可起");
        FakeRelay {
            addr,
            uplink_frames,
            bad_label,
            stop,
            handle: Some(handle),
        }
    }

    fn uplink_frames(&self) -> u64 {
        self.uplink_frames.load(Ordering::SeqCst)
    }

    fn clean(&self) -> bool {
        !self.bad_label.load(Ordering::SeqCst)
    }

    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

/// 测试用出口（**真出口面**：`ExitQuic` 自身；引擎侧由 [`Stub`] 扮演）。
/// 绑回环：迁移用例的客户端在 `127.0.0.1` ↔ `127.0.0.2` 间换 IP（回环别名全段本地可达）。
fn exit_face(seed: u8) -> ExitQuic {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("出口 socket 可绑");
    let (logf, _rx) = sink();
    ExitQuic::start(sock, ExitQuicConfig::new(Ed25519Seed::from_bytes([seed; 32]), 32), logf)
        .expect("出口 QUIC 面可起")
}

/// 出口面（**显式流面限制**）：把「对端接收窗」这类自变量在用例里钉死，使用例不随
/// 设计缺省漂移（S9 把每流接收窗 256 KiB→4 MiB 时，背压用例的旧形态就不再必然出现 n=0）。
fn exit_face_streams(seed: u8, streams: crate::tuning::StreamLimits) -> ExitQuic {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("出口 socket 可绑");
    let (logf, _rx) = sink();
    let mut cfg = ExitQuicConfig::new(Ed25519Seed::from_bytes([seed; 32]), 32);
    cfg.streams = streams;
    ExitQuic::start(sock, cfg, logf).expect("出口 QUIC 面可起")
}

/// 出口的可连地址（`local_addr` 是**已绑定**地址；本测试全走回环 ⇒ 取端口 + 127.0.0.1）。
fn connectable(quic: &ExitQuic) -> SocketAddrV4 {
    let port = match quic.local_addr() {
        SocketAddr::V4(v) => v.port(),
        SocketAddr::V6(_) => unreachable!(),
    };
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
}

/// 内层 IPv4 包（出站投递用；形态同 `homeway-core` 的 `server/device.rs` 测试构造）。
fn inner_pkt(src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
    let mut p = vec![0u8; 28];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&28u16.to_be_bytes());
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    p
}

/// 测试侧「引擎桩」：只经**真接口**（`drain_inbound`）拿事件，裁决语义 = 核心侧
/// `admit_reg4` 的 MAC 判定（secret + 本连接 exporter；准入/刷新两域）。
struct Stub {
    accepted: AtomicU64,
    rejected: AtomicU64,
    packets: Mutex<Vec<Vec<u8>>>,
    /// 注入**引擎裁决拒绝**的桶（M3 S5：`Some(..)` ⇒ 不看 MAC 一律按该桶拒——
    /// 复刻真引擎 `table.register` 的 `no-token`/`table-full` 两族）。
    reject_class: Mutex<Option<EngineRejectClass>>,
}

impl Stub {
    fn new() -> Arc<Stub> {
        Arc::new(Stub {
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            packets: Mutex::new(Vec::new()),
            reject_class: Mutex::new(None),
        })
    }

    fn pump(&self, quic: &ExitQuic) {
        quic.drain_inbound(|item| match item {
            ExitInbound::Reg(req) => {
                // M3 S5 注入面（优先于 MAC 判定：真引擎里 MAC 过了才谈裁决）
                if let Some(class) = *self.reject_class.lock().unwrap() {
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg4Verdict::Rejected {
                        why: RejectWhy::EngineRejected { class },
                    });
                } else if req.frame.mac_matches(&SECRET, &req.exporter) {
                    self.accepted.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg4Verdict::Accepted {
                        tunnel_ip: TUNNEL_IP,
                        tun_ip: TUN_IP,
                    });
                } else {
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg4Verdict::Rejected { why: RejectWhy::MacMismatch });
                }
            }
            ExitInbound::Packet { pkt, .. } => self.packets.lock().unwrap().push(pkt),
        });
    }

    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::SeqCst)
    }

    fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::SeqCst)
    }

    /// 取走一份长度 `len` 的内层包（数据面判据用；找不到返回 `None`）。
    fn take_packet(&self, len: usize) -> Option<Vec<u8>> {
        let mut q = self.packets.lock().unwrap();
        let idx = q.iter().position(|p| p.len() == len)?;
        Some(q.remove(idx))
    }

    /// 收到的内层包总数（负判据用：不该到的包）。
    fn packets_len(&self) -> usize {
        self.packets.lock().unwrap().len()
    }
}

/// 有界轮询（每轮先 pump；只判上界）。
async fn wait_for(
    stub: &Stub,
    quic: &ExitQuic,
    mut cond: impl FnMut() -> bool,
    wait: Duration,
) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        stub.pump(quic);
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 投一条带 reply 的命令并**异步有界**取回（不可用阻塞 `recv_timeout`：会卡住测试 runtime，
/// 出口面就没人 pump 了——准入裁决要桩先回执）。
async fn send_wait<T>(
    island: &Island,
    stub: &Stub,
    quic: &ExitQuic,
    make: impl FnOnce(IslandReply<T>) -> Cmd,
    wait: Duration,
) -> Result<T, IslandErr> {
    let (tx, rx) = channel();
    island.tx().send(make(tx)).expect("命令投递（unbounded）");
    let deadline = Instant::now() + wait;
    loop {
        stub.pump(quic);
        match rx.try_recv() {
            Ok(v) => return v,
            Err(TryRecvError::Disconnected) => return Err(IslandErr::EngineGone),
            Err(TryRecvError::Empty) => {}
        }
        assert!(Instant::now() < deadline, "命令回执超时（岛未应答 ⇒ 挂死）");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// [`send_wait`] 的**静默版**：不 pump 引擎桩 ⇒ 出口侧的准入裁决永远拿不到回执
/// （S2-3 的「准入到点」与「引擎不答时客户端自带期限」两半专用）。
async fn send_wait_silent<T>(
    island: &Island,
    make: impl FnOnce(IslandReply<T>) -> Cmd,
    wait: Duration,
) -> Result<T, IslandErr> {
    let (tx, rx) = channel();
    island.tx().send(make(tx)).expect("命令投递（unbounded）");
    let deadline = Instant::now() + wait;
    loop {
        match rx.try_recv() {
            Ok(v) => return v,
            Err(TryRecvError::Disconnected) => return Err(IslandErr::EngineGone),
            Err(TryRecvError::Empty) => {}
        }
        assert!(Instant::now() < deadline, "命令回执超时（岛未应答 ⇒ 挂死）");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 收日志行（非阻塞轮询版 `drain_until`）。
async fn logs_until(rx: &Receiver<String>, needle: &str, wait: Duration) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        while let Ok(l) = rx.try_recv() {
            lines.push(l);
        }
        if lines.iter().any(|l| l.contains(needle)) {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    lines
}

/// 连上真出口的单直连候选（S2-4/S2-5/S2-6 用例的公共前段）。
async fn connect_direct(island: &Island, stub: &Stub, quic: &ExitQuic) -> SocketAddrV4 {
    let addr = connectable(quic);
    let _ = send_wait(
        island,
        stub,
        quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功（真出口 + 引擎桩）");
    addr
}

fn direct(addr: SocketAddrV4) -> Candidate {
    Candidate {
        addr,
        via: Via::Direct,
    }
}

/// 本机非回环 IPv4（`connect` 只选路由、不发包；无默认路由 ⇒ None）。
fn lan_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?; // TEST-NET-1：仅用于选路由
    match s.local_addr().ok()? {
        SocketAddr::V4(v) if !v.ip().is_loopback() => Some(*v.ip()),
        _ => None,
    }
}

/// 迁移目标地址（「换网卡」的本地等价物；设计 §2.3 的本地注入代理）：
/// ① `127.0.0.2`（Linux 的 lo 别名）能绑就用它；② darwin 的 `lo0` 只有 `127.0.0.1`
/// ⇒ 借本机网卡地址（设计 P7 的同款形态）；③ 都不行退回「同 IP 换端口」（仍走 rebind +
/// 路径验证，但不是「换 IP」分支——用例打印实际口径，不静默降级）。
fn alt_bind_addr() -> (SocketAddrV4, &'static str) {
    let alt = Ipv4Addr::new(127, 0, 0, 2);
    if UdpSocket::bind((alt, 0)).is_ok() {
        return (SocketAddrV4::new(alt, 0), "换 IP（127.0.0.2 别名）");
    }
    if let Some(ip) = lan_ipv4() {
        return (SocketAddrV4::new(ip, 0), "换 IP（本机网卡地址）");
    }
    (
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        "同 IP 换端口（本机无第二地址）",
    )
}

/// 「死候选」= 已绑定但**从不回包**的 UDP socket（握手永不完成；不产生 ICMP 干扰）。
fn dead_candidate() -> (UdpSocket, Candidate) {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("死候选可绑");
    let addr = match s.local_addr().expect("可读地址") {
        SocketAddr::V4(v) => v,
        SocketAddr::V6(_) => unreachable!(),
    };
    (s, direct(addr))
}

// ---------- 1. 端点就绪 + 登记（真出口 + 引擎桩） ----------

/// **判据（S2-6 主干 / M2 §1.7）**：岛连上真出口 ⇒ 首条 bidi 控制流**四帧准入** ⇒
/// 出口侧引擎桩收到合法 Proof（= 真出口 `peer: +` 的等价面：绑定成功）⇒ N-a/C4'/C5'/C6'
/// + `准入已发起`/`准入完成` 行齐。
#[tokio::test]
async fn connect_registers_on_control_stream_and_logs_criteria_lines() {
    let quic = exit_face(1);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));

    let outcome = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(connectable(&quic))],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("单候选（活）必须胜出");
    assert_eq!(outcome.completed.len(), 1, "完成清单 = 胜者：{outcome:?}");
    assert!(outcome.unfinished.is_empty(), "无未完成候选：{outcome:?}");
    assert_eq!(outcome.via, Via::Direct, "直连候选 ⇒ via=Direct");
    assert!(outcome.rtt_ms < 1000, "回环 RTT 量级（上界）：{}", outcome.rtt_ms);

    assert!(
        wait_for(&stub, &quic, || stub.accepted() >= 1, WAIT).await,
        "出口侧必须收到并采纳登记（MAC + 本连接 exporter）"
    );
    assert_eq!(stub.accepted(), 1, "恰好一次登记");
    assert_eq!(quic.snapshot().regs_accepted, 1, "出口面登记计数 +1");

    let snap = island.snapshot();
    assert_eq!(snap.via, Some(Via::Direct), "快照 via 入位：{snap:?}");
    assert!(snap.ep.is_some(), "快照 ep 入位");
    assert!(snap.mtu.is_some(), "快照 mtu（max_datagram_size）入位");
    assert!(snap.current_mtu >= 1320, "current_mtu ≥ min_mtu：{}", snap.current_mtu);
    assert_eq!(snap.mirrors, 1, "投出的候选数累计");

    let lines = logs_until(&logs, "准入完成", WAIT).await;
    for needle in [
        "端点就绪（本地 ",
        "赛跑投出 1 个候选（直连 1 / 中继 0；本行每轮限 3 条）",
        "赛跑结算：胜出 直连 ",
        "路径确立：直连 ",
        "（首个完成握手）",
        "准入已发起（dev=22222222，Hello 50B；等挑战/回执）",
        "准入完成（dev=22222222，耗时 ",
    ] {
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "缺判据行「{needle}」：{lines:?}"
        );
    }
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "赛跑登记后必须能预算内收工"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 1b. 准入失败归因回传（M3 S5 / §4） ----------

/// **判据（M3 S5 / §4）**：出口按准入码拒绝 ⇒ 设备侧**看得见原因**（M2 真机发现①洞）：
/// ①`Cmd::Connect` 返回 typed `IslandErr::AdmissionRejected{code}`（不再是通串
/// `RegistrationFailed`）；②岛打**客户端归因行** `quic: 准入回执（code=0x%02x %s）——
//   本世代回落 WG 承载`（**前缀**与出口的 `quic: 准入被拒（` 区分——设计门 P3）；
/// ③岛快照两字段（`admit_reject_code`/`admit_reject_text`）——App 状态面据此回答
/// 「为什么走了 WG」。
#[tokio::test]
async fn admission_rejection_carries_code_line_and_snapshot_fields() {
    for (class, want, text) in [
        (EngineRejectClass::Credential, 0x11u64, "凭证不被接受"),
        (EngineRejectClass::Resource, 0x12u64, "资源暂不可用（稍后重试）"),
    ] {
        let quic = exit_face(9);
        let stub = Stub::new();
        *stub.reject_class.lock().unwrap() = Some(class);
        let (logf, logs) = sink();
        let island =
            island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
        let addr = connectable(&quic);

        let res = send_wait(
            &island,
            &stub,
            &quic,
            |reply| Cmd::Connect {
                cands: vec![direct(addr)],
                budget: WAIT,
                reply,
            },
            WAIT,
        )
        .await;
        match res {
            Err(IslandErr::AdmissionRejected { code }) if code == want => {}
            other => panic!("{class:?} 必须带码 {want:#x} 回传（不再是 RegistrationFailed）：{other:?}"),
        }

        // ② 客户端归因行（前缀与出口行区分 + 短语逐字 + 末句）
        let lines = logs_until(&logs, "准入回执", WAIT).await;
        let l = lines
            .iter()
            .find(|l| l.contains("准入回执"))
            .unwrap_or_else(|| panic!("缺准入回执行：{lines:?}"))
            .clone();
        assert_eq!(
            l,
            format!("准入回执（code=0x{want:02x} {text}）——本世代未建连"),
            "归因行逐字"
        );
        assert!(!l.contains("准入被拒"), "客户端行不得用出口前缀（脚本 grep 会混淆）：{l}");

        // ③ 快照两字段（App 状态面的「为什么没连上」）
        let snap = island.snapshot();
        assert_eq!(snap.admit_reject_code, Some(want), "{class:?} 的码面");
        assert_eq!(snap.admit_reject_text.as_deref(), Some(text), "{class:?} 的短语面");

        assert!(island.stop_within(Instant::now() + BUDGET));
    }
}

/// **判据（M3 S5 / §4 设计门 F4 的负例）**：**准入后**的会话级关闭（对端 `device removed`
/// = `unbind_pub`）⇒ 走 `SessionClosed` 独立分支：**不产生**准入回执行、快照不落准入码、
/// 行文归会话级——否则「会话中被吊销」会被误报成「准入被拒」。
#[tokio::test]
async fn post_admission_session_close_is_not_reported_as_admission() {
    let quic = exit_face(10);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
    let addr = connect_direct(&island, &stub, &quic).await;
    let _ = addr;

    // 准入后摘除设备（出口侧 `unbind_pub` ⇒ `device removed` 的 CONNECTION_CLOSE）
    quic.unbind_pub(&PUBKEY);
    let lines = logs_until(&logs, "会话被对端关闭", WAIT).await;
    assert!(
        lines.iter().any(|l| l.contains("会话被对端关闭")),
        "会话级关闭必须单独归因：{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.starts_with("准入回执（")),
        "会话级关闭**不得**产生准入回执（设计门 F4）：{lines:?}"
    );
    let snap = island.snapshot();
    assert_eq!(snap.admit_reject_code, None, "准入码面必须保持空：{snap:?}");
    assert_eq!(snap.admit_reject_text, None);

    assert!(island.stop_within(Instant::now() + BUDGET));
}

// ---------- 2. 赛跑：三候选（一活两死） ----------

/// **判据（S2-2）**：三候选（一活两死）⇒ 胜者正确、`via` 正确、耗时上界、清单正确。
#[tokio::test]
async fn race_picks_first_completer_among_dead_candidates() {
    let quic = exit_face(2);
    let stub = Stub::new();
    let (_dead_a, cand_a) = dead_candidate();
    let (_dead_b, cand_b) = dead_candidate();
    let alive_addr = connectable(&quic);
    assert_ne!(cand_a.addr, alive_addr);
    assert_ne!(cand_b.addr, alive_addr);

    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let budget = Duration::from_secs(3);
    let t0 = Instant::now();
    let outcome = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            // 死候选夹在活候选两侧：胜者必须是「活」的那条（不是位置效应）
            cands: vec![cand_a, direct(alive_addr), cand_b],
            budget,
            reply,
        },
        WAIT,
    )
    .await
    .expect("一活两死 ⇒ 必须有胜者");
    let elapsed = t0.elapsed();

    assert_eq!(outcome.winner, alive_addr, "胜者 = 活候选：{outcome:?}");
    assert_eq!(outcome.via, Via::Direct);
    assert_eq!(outcome.completed, vec![alive_addr], "完成清单只含胜者");
    assert_eq!(
        outcome.unfinished,
        vec![cand_a.addr, cand_b.addr],
        "未完成清单 = 两个死候选：{outcome:?}"
    );
    assert!(
        elapsed < budget + Duration::from_secs(2),
        "耗时上界（不判精确墙钟，flake 口径②）：实耗 {elapsed:?}"
    );
    assert!(
        wait_for(&stub, &quic, || stub.accepted() >= 1, WAIT).await,
        "胜者必须完成登记"
    );
    assert_eq!(island.snapshot().via, Some(Via::Direct));
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-7）**：直连 + 中继**混合候选赛跑**；中继候选的信封上行（`[0xAA][label8]‖
/// `[0xBB][5]‖pkt`）与下行剥壳（`[0xBB][5]‖pkt`）逐字节正确；**非 kind=5 帧不入 quinn**
/// （计数面 `rx_ignored`）；登记与数据面双向在网络层经「中继」中转。
///
/// 仪器 = [`FakeRelay`]：真中继在 QUIC 面上的两件事（信封上行剥标签 + 裸包下行包帧）
/// 的透明代理；**不实现**中继控制面（leg 注册/会话）——S2-7 的判据是信封/剥壳，
/// 中继协议本体由 S1c（出口侧）与全链 E2E（`tools/quic-island-e2e.sh`）覆盖。
#[tokio::test]
async fn relay_candidate_wins_through_envelope_and_downlink_is_stripped() {
    let quic = exit_face(14);
    let stub = Stub::new();
    let (logf, logs) = sink();
    // 短巡检节拍：让刷新行（C15' 的「中继=」位）也在本用例窗口内出现
    let island = island_with_log(
        Duration::from_millis(300),
        quic.rpk_public_key(),
        Arc::clone(&logf),
    );
    let label = [0xA1u8; 8];
    let mut relay = FakeRelay::start(connectable(&quic), label);
    let (tun, tun_peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));

    // 混合赛跑：死直连 + 活中继 ⇒ 中继胜（且必须**经信封**才能胜——裸包打到假中继上
    // 不会被转发，握手不会完成）
    let (_dead, dead) = dead_candidate();
    let outcome = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![dead, Candidate { addr: relay.addr, via: Via::Relay { label } }],
            budget: Duration::from_secs(5),
            reply,
        },
        WAIT,
    )
    .await
    .expect("中继类候选必须能经信封完成握手");
    assert_eq!(outcome.winner, relay.addr, "胜者 = 中继候选");
    assert_eq!(outcome.via, Via::Relay { label }, "via 如实带回类别");
    assert_eq!(island.snapshot().via, Some(Via::Relay { label }));
    assert!(relay.uplink_frames() > 0, "假中继必须收到过信封帧（上行包封已生效）");
    assert!(relay.clean(), "上行必须恒为合规信封（label 逐字节一致）");
    assert_eq!(stub.accepted(), 1, "登记帧经中继到达出口（引擎桩裁决通过）");

    let lines = logs_until(&logs, "赛跑结算：胜出 中继", WAIT).await;
    let settle = lines
        .iter()
        .find(|l| l.contains("赛跑结算：胜出 中继 "))
        .expect("C5' 行按类别取值（胜出 中继）")
        .clone();
    assert!(
        settle.contains(&relay.addr.to_string()) && settle.contains("未完成="),
        "C5' 行含胜者地址与清单：{settle}"
    );
    // C15'（刷新）行里的「中继=」位也按类别取值
    let refresh = logs_until(&logs, "注册刷新 → ", WAIT).await;
    assert!(
        refresh.iter().any(|l| l.contains("中继=true")),
        "刷新行的中继位如实：{refresh:?}"
    );

    // ---- 数据面双向（经中继）----
    // 上行：TUN fd → 岛 → 信封 → 假中继 → 出口（引擎桩收包，逐字节）
    let inner = inner_pkt(TUN_IP, Ipv4Addr::new(10, 0, 0, 7));
    write_tun(&tun_peer, &inner);
    let got = wait_packet(&stub, &quic, inner.len(), WAIT).await;
    assert_eq!(got.as_deref(), Some(inner.as_slice()), "上行包经中继逐字节到达出口");

    // 下行：出口 → 裸包 → 假中继包帧 → 岛剥壳 → TUN fd（逐字节）
    let back = inner_pkt(TUNNEL_IP, TUN_IP);
    assert_eq!(
        quic.send_to_pub(&PUBKEY, &back),
        crate::ExitSend::Handled,
        "出口侧回程必须已在 QUIC 面绑定"
    );
    let echoed = read_tun(&tun_peer, WAIT).expect("回程必须写到 TUN fd");
    assert_eq!(echoed, back, "下行剥壳后逐字节还原");

    // 非 kind=5 帧（假中继推 hint）⇒ 忽略 + 计数；quinn 不受影响
    assert!(
        wait_for(&stub, &quic, || island.snapshot().rx_ignored >= 1, WAIT).await,
        "非 kind=5 帧必须被忽略并计数（假中继推了 hint）"
    );
    assert!(
        island.snapshot().relay_tx > 0,
        "中继上行包封计数（快照 relay_tx）"
    );

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
    relay.stop();
}

/// **判据（S2-2 的失败面）**：全候选失败 ⇒ `IslandErr::NoCandidate`（且快照无 path）。
#[tokio::test]
async fn race_all_dead_reports_no_candidate() {
    let quic = exit_face(3);
    let stub = Stub::new();
    let (_d1, c1) = dead_candidate();
    let (_d2, c2) = dead_candidate();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());

    let budget = Duration::from_millis(600);
    let t0 = Instant::now();
    let res = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![c1, c2],
            budget,
            reply,
        },
        WAIT,
    )
    .await;
    assert!(
        matches!(res, Err(IslandErr::NoCandidate)),
        "全死必须归 NoCandidate：{res:?}"
    );
    assert!(
        t0.elapsed() < budget + Duration::from_secs(2),
        "失败也必须在上界内定音：{:?}",
        t0.elapsed()
    );
    let snap = island.snapshot();
    assert_eq!(snap.via, None, "无胜者 ⇒ via=None");
    assert_eq!(snap.ep, None);
    // 空候选（且未 SetCandidates）同归 NoCandidate
    let res2 = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: Vec::new(),
            budget,
            reply,
        },
        WAIT,
    )
    .await;
    assert!(matches!(res2, Err(IslandErr::NoCandidate)), "{res2:?}");
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-9 的消费侧）**：错 RPK 钉定 ⇒ 握手在 TLS 内中止 ⇒ 赛跑无胜者
/// （不是「连上再拒」；出口侧握手失败计数增长）。
#[tokio::test]
async fn wrong_pin_aborts_handshake_without_winner() {
    let quic = exit_face(4);
    let stub = Stub::new();
    let wrong = RpkPublicKey::from_bytes([0xEE; 32]);
    assert_ne!(wrong, quic.rpk_public_key());
    let island = island_with(Duration::from_secs(60), wrong);
    let addr = connectable(&quic);
    let res = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: Duration::from_millis(900),
            reply,
        },
        WAIT,
    )
    .await;
    assert!(
        matches!(res, Err(IslandErr::NoCandidate)),
        "错 pin 不得连上：{res:?}"
    );
    assert_eq!(stub.accepted(), 0, "出口侧零采纳（握手期即中止）");
    assert_eq!(quic.snapshot().admitted, 0, "出口面零采纳连接");
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 赛跑在途时再来一条 `Connect` ⇒ `RaceInFlight`（赛跑结论是一次性的）；收工可在途中进行
/// （长任务随 `JoinSet` abort，不拖累收工预算）。
#[tokio::test]
async fn connect_while_racing_is_rejected_and_stop_is_prompt() {
    let quic = exit_face(5);
    let stub = Stub::new();
    let (_d, dead) = dead_candidate();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());

    let (tx1, rx1) = channel();
    island
        .tx()
        .send(Cmd::Connect {
            cands: vec![dead],
            budget: Duration::from_secs(30), // 长预算：确保「在途」窗口足够观察
            reply: tx1,
        })
        .expect("投递第一轮赛跑");
    let second = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![dead],
            budget: Duration::from_millis(100),
            reply,
        },
        Duration::from_secs(2),
    )
    .await;
    assert!(
        matches!(second, Err(IslandErr::RaceInFlight)),
        "在途赛跑必须拒第二轮：{second:?}"
    );
    drop(rx1); // 第一轮的回执在收工 abort 时随之 drop（不读）

    let t0 = Instant::now();
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "赛跑在途也必须预算内收工"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "收工不得越过上界：{:?}",
        t0.elapsed()
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 2b. 准入预算（S2-3） ----------

/// **判据（S2-3 的纯定时语义，设计 §1.7 / r14 F8）**：准入预算 = `max(剩余, ADMIT_MIN)`
/// 的**值语义**（纯函数）+ **虚拟时钟**下「到点恰在预算处」——`start_paused` ⇒ 断言确定，
/// 不等墙钟（M0 flake 口径②）。
#[tokio::test(start_paused = true)]
async fn admit_budget_value_and_virtual_clock_expiry() {
    let total = Duration::from_secs(5);
    assert_eq!(super::race::admit_budget(total, Duration::ZERO), total, "未消耗 ⇒ 全量");
    assert_eq!(
        super::race::admit_budget(total, Duration::from_millis(3000)),
        Duration::from_secs(2),
        "剩 2s ≥ 下界 ⇒ 用剩余量"
    );
    assert_eq!(
        super::race::admit_budget(total, Duration::from_millis(4900)),
        super::race::ADMIT_MIN,
        "剩 100ms < 下界 ⇒ 取下界（晚胜形态：不「必然失败但不说清」）"
    );
    assert_eq!(
        super::race::admit_budget(total, Duration::from_secs(9)),
        super::race::ADMIT_MIN,
        "超支（saturating）⇒ 仍取下界，不是零/负"
    );

    // 虚拟时钟复算同一规则 + 包装到点语义：恰在下界处返回 `None`（不是立刻、也不是无限等）
    let t0 = TokioInstant::now();
    tokio::time::sleep(Duration::from_millis(4900)).await;
    let b = super::race::admit_budget(total, t0.elapsed());
    assert_eq!(b, super::race::ADMIT_MIN);
    let t1 = TokioInstant::now();
    let got = super::race::within(b, std::future::pending::<()>()).await;
    assert!(got.is_none(), "到点必须返回 None（= RegistrationFailed 路径）");
    assert_eq!(TokioInstant::now() - t1, b, "恰在预算到点");
}

/// **判据（S2-3 / 设计 §1.7 / r14 F8）**：`ADMIT_MIN` 形态——赛跑预算压到 1.5s、出口侧
/// **从不回执**（引擎桩不 pump ⇒ 裁决永不到达）⇒ 岛侧准入按 `max(剩余, ADMIT_MIN)` 到点：
/// ①归因 `IslandErr::RegistrationFailed`；②归因行写明**实际预算 = 下界 2s**（证明走了
/// `ADMIT_MIN` 而不是剩余量）；③**连接被显式 close**（出口面连接数回落 0——不是等 30s
/// idle）；④耗时落 [下界, 出口侧 `ADMIT_DEADLINE` 10s) 区间内。
#[tokio::test]
async fn admission_budget_floor_fails_with_explicit_close() {
    let quic = exit_face(8);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
    let addr = connectable(&quic);

    let t0 = Instant::now();
    let res = send_wait_silent(
        &island,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: Duration::from_millis(1500), // 赛跑吃光 ⇒ 剩余 ≪ ADMIT_MIN
            reply,
        },
        WAIT,
    )
    .await;
    let elapsed = t0.elapsed();
    assert!(
        matches!(res, Err(IslandErr::RegistrationFailed)),
        "准入到点必须归 RegistrationFailed：{res:?}"
    );
    assert!(
        elapsed >= super::race::ADMIT_MIN,
        "必须跑满下界（ADMIT_MIN=2s）才收口（剩余量会在毫秒级就失败）：{elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(8),
        "不得拖到出口侧的 ADMIT_DEADLINE（10s）：{elapsed:?}"
    );

    let lines = logs_until(&logs, "准入失败", WAIT).await;
    let line = lines
        .iter()
        .find(|l| l.contains("准入失败"))
        .expect("准入失败行在（失败必须可归因，不静默）")
        .clone();
    assert!(line.contains("预算 2s"), "行文须写明实际预算 = 下界：{line}");
    assert!(
        line.contains("连接已显式关闭"),
        "行文须写明显式收口（不留悬挂）：{line}"
    );
    assert!(
        lines.iter().any(|l| l.contains("准入已发起")),
        "四帧已发起（Hello 已发）后才到点：{lines:?}"
    );

    // ③ 显式 close 的**出口侧证据**：连接数回落 0（若靠 idle 回收，这一格要 30s 才动）
    assert!(
        wait_for(&stub, &quic, || quic.snapshot().connections == 0, WAIT).await,
        "岛侧必须显式关连接（出口面连接数应回落 0）：{:?}",
        quic.snapshot().connections
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 3. 刷新（60s 节拍：虚拟时钟 + 集成面短节拍） ----------

/// **判据（S2-6 的 60s 节拍）**：纯定时语义用虚拟时钟（`start_paused`）——一拍一帧。
#[tokio::test(start_paused = true)]
async fn refresh_timer_ticks_one_frame_per_patrol_in_virtual_time() {
    let patrol = Duration::from_secs(60);
    let t0 = TokioInstant::now();
    let mut t = RefreshTimer::new(patrol);
    assert!(!t.due(t0), "起手不到点");
    assert!(!t.due(t0 + Duration::from_secs(59)), "59s 不到点");
    assert!(t.due(t0 + Duration::from_secs(60)), "60s 到点");
    assert!(!t.due(t0 + Duration::from_secs(119)), "下一拍前不到点");
    assert!(t.due(t0 + Duration::from_secs(120)), "120s 到点");

    // 虚拟时钟推进（自动跳）语义下再走一遍：3 拍 ⇒ 恰 3 次到点
    let mut t = RefreshTimer::new(patrol);
    let mut fired = 0;
    for _ in 0..3 {
        tokio::time::sleep(patrol).await;
        if t.due(TokioInstant::now()) {
            fired += 1;
        }
    }
    assert_eq!(fired, 3, "60s 节拍：3 拍恰 3 次");
}

/// **判据（M2 §1.8 的客户端半边）**：刷新帧（`R4` + 本连接 exporter）字节级到达对端，
/// 行文 = C15'（M1 已登记，**一字不改**）。
/// 岛侧节拍用短值（集成面不判精确墙钟——只判「按拍发生且逐帧合法」）。
#[tokio::test]
async fn refresh_frames_reach_peer_and_log_c15_prime() {
    let quic = exit_face(6);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(
        Duration::from_millis(300),
        quic.rpk_public_key(),
        Arc::clone(&logf),
    );
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    assert_eq!(stub.accepted(), 1, "起手一次登记");

    // 两拍刷新 ⇒ 出口侧再收两帧（每帧都是合法的连接绑定登记帧）
    assert!(
        wait_for(&stub, &quic, || stub.accepted() >= 3, WAIT).await,
        "刷新帧必须按拍到达（accepted={}）",
        stub.accepted()
    );
    assert_eq!(stub.rejected(), 0, "刷新帧不得被拒（exporter 复用正确）");
    let lines = logs_until(&logs, "注册刷新 → ", WAIT).await;
    let line = lines
        .iter()
        .find(|l| l.contains("注册刷新 → "))
        .expect("C15' 行在")
        .clone();
    assert!(line.contains(&addr.to_string()), "行含端点：{line}");
    assert!(
        line.contains("dev=22222222") && line.contains("中继=false"),
        "行含 dev 短指纹与中继位：{line}"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 4. 迁移（rebind） ----------

/// **判据（S2-3）**：`127.0.0.1:0` → `127.0.0.2:0` rebind 后
/// ①连接未断 ②出口 `remote_address()` 变化（E-q2「路径变更」行）③收发继续（出口回程 ⇒
/// 岛侧入站证据 ⇒ N-b 行）④**设备表不新增条目**（引擎桩 accepted 不增）。
///
/// 源 IP 变化（不是换端口）⇒ 走协议的真迁移分支（`paths.rs` 的换 IP 分支）。
#[tokio::test]
async fn rebind_keeps_connection_and_peer_observes_new_path() {
    let quic = exit_face(7);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(
        Duration::from_secs(3), // 迁移保持判据窗（够长：回环 PATH_RESPONSE 是亚秒级）
        quic.rpk_public_key(),
        Arc::clone(&logf),
    );
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    let path_changes0 = quic.snapshot().path_changes;

    // ---- rebind 到另一个本地地址（源地址变化 = 「换网卡」的本地等价物）----
    let (alt, how) = alt_bind_addr();
    println!("迁移目标口径：{how} ⇒ {alt}");
    let to = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Rebind { local: Some(alt), reply },
        WAIT,
    )
    .await
    .expect("换绑必须成功");
    assert_ne!(to.port(), 0, "新本地地址由内核分配：{to}");

    // ② 出口侧：remote_address() 变化 + E-q2 行（路径变更）
    assert!(
        wait_for(
            &stub,
            &quic,
            || quic.snapshot().path_changes > path_changes0,
            WAIT
        )
        .await,
        "出口必须观测到路径变更（path_changes={}）",
        quic.snapshot().path_changes
    );

    // ① 连接未断 + ③ 收发继续：出口发一包 ⇒ 只在**新路径**上才可能到达岛
    let sent = quic.send_to_pub(&PUBKEY, &inner_pkt(TUN_IP, Ipv4Addr::new(10, 0, 0, 7)));
    assert_eq!(
        sent,
        crate::ExitSend::Handled,
        "出口侧出站必须已在 QUIC 面绑定（回程发往新路径）"
    );
    assert!(
        wait_for(&stub, &quic, || island.snapshot().migrations >= 1, WAIT).await,
        "岛必须在保持窗内确认迁移（入站证据 ⇒ N-b）"
    );

    let lines = logs_until(&logs, "迁移完成（", WAIT).await;
    let line = lines
        .iter()
        .find(|l| l.contains("迁移完成（"))
        .expect("N-b 行在")
        .clone();
    assert!(
        line.contains(&to.to_string()) && line.contains(" → "),
        "N-b 行含旧 → 新（新地址 = {to}）：{line}"
    );

    // ④ 出口设备表不新增条目 + 连接仍只有一条 + 岛侧 path 未清
    assert_eq!(stub.accepted(), 1, "迁移不得产生新登记（设备表不新增条目）");
    assert_eq!(quic.snapshot().connections, 1, "出口侧连接数不变");
    let snap = island.snapshot();
    assert_eq!(snap.via, Some(Via::Direct), "连接保持：{snap:?}");
    assert!(!snap.migration_unconfirmed, "迁移已确认");
    assert_eq!(snap.migrations, 1);

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 迁移保持的**失败判据**面：无连接时 `rebind` 只是换 socket（不起保持窗），
/// 且 `migration_unconfirmed` 保持清零——「N 拍无回包 ⇒ 未确认」的到点分支由
/// `client::migration` 的纯逻辑用例覆盖（真路径无回包不可确定性构造）。
#[tokio::test]
async fn rebind_without_connection_only_swaps_socket() {
    let quic = exit_face(8);
    let stub = Stub::new();
    let island = island_with(Duration::from_millis(200), quic.rpk_public_key());
    let to = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Rebind {
            local: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            reply,
        },
        WAIT,
    )
    .await
    .expect("无连接也可换绑（endpoint 粒度）");
    assert_eq!(*to.ip(), Ipv4Addr::LOCALHOST);
    let snap = island.snapshot();
    assert!(!snap.migration_unconfirmed, "无连接 ⇒ 不起保持窗");
    assert_eq!(snap.migrations, 0);
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 5. Probe / 丢弃计数 / SetCandidates ----------

/// **判据（S2-5 的 `Probe` 命令面）**：未建连 ⇒ `NotConnected`；已登记连接 ⇒ 预算内有结论
/// （对端回包证据 ⇒ Ok(rtt)）；连接被关 ⇒ `ConnectionLost`。
#[tokio::test]
async fn probe_reports_alive_then_connection_lost() {
    let quic = exit_face(9);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());

    let not_connected = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Probe {
            budget: Duration::from_millis(200),
            reply,
        },
        Duration::from_secs(2),
    )
    .await;
    assert!(
        matches!(not_connected, Err(IslandErr::NotConnected)),
        "未建连必须归 NotConnected：{not_connected:?}"
    );

    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");

    let rtt = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Probe {
            budget: Duration::from_secs(3),
            reply,
        },
        WAIT,
    )
    .await
    .expect("活连接必须判活（对端回包证据）");
    assert!(rtt < Duration::from_secs(1), "回环 RTT 量级上界：{rtt:?}");

    // ① 连接被**显式关闭**（出口侧摘绑定 ⇒ CONNECTION_CLOSE）⇒ 判活归 ConnectionLost
    quic.unbind_pub(&PUBKEY);
    let mut last = Ok(Duration::ZERO);
    for _ in 0..40 {
        last = send_wait(
            &island,
            &stub,
            &quic,
            |reply| Cmd::Probe {
                budget: Duration::from_millis(300),
                reply,
            },
            Duration::from_secs(2),
        )
        .await;
        if matches!(last, Err(IslandErr::ConnectionLost)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        matches!(last, Err(IslandErr::ConnectionLost)),
        "显式关闭必须归 ConnectionLost：{last:?}"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-5 的失败面，设计 §2.3）**：对端**静默消失**（无专用错误——quinn 侧要等
/// `max_idle_timeout` 才定音）⇒ 判活必须**失败**（`ConnectionLost` 或 `ProbeNoResponse`，
/// 由是否收到 CONNECTION_CLOSE 定——**不得**判活成功）。
#[tokio::test]
async fn probe_fails_when_peer_silently_gone() {
    let quic = exit_face(13);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    // 出口面收工（可能发出 CONNECTION_CLOSE，也可能来不及刷出 ⇒ 客户端侧静默）
    assert!(quic.stop_within(Instant::now() + BUDGET));
    let mut last = Ok(Duration::ZERO);
    for _ in 0..20 {
        last = send_wait(
            &island,
            &stub,
            &quic,
            |reply| Cmd::Probe {
                budget: Duration::from_millis(300),
                reply,
            },
            Duration::from_secs(2),
        )
        .await;
        if last.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        last.is_err(),
        "对端消失后判活必须失败（不误报活）：{last:?}"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
}

/// **判据（§2.4/§6.4 的「丢弃可观测、不静默」）**：四类计数入快照 + N-c 行 + 事件回调；
/// `TunPacket` 在无登记连接时归 `未登记`（登记前丢弃）。
#[tokio::test]
async fn drop_counters_events_and_nc_line() {
    let quic = exit_face(10);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
    let (ev_tx, ev_rx) = channel();
    island
        .tx()
        .send(Cmd::SetOnEvent {
            h: Arc::new(move |e: IslandEvent| {
                let _ = ev_tx.send(e);
            }),
        })
        .expect("装事件回调");

    // 数据面上报（TUN 读线程/回程线程的形态）
    island
        .tx()
        .send(Cmd::DatagramDropped {
            reason: DropReason::TooLarge,
            n: 2,
        })
        .expect("投递丢弃上报");
    let lines = logs_until(&logs, "丢弃 超限=2", WAIT).await;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("丢弃 超限=2 发送缓冲满=0 回程队列满=0 未登记=0")),
        "N-c 行四字段序固定：{lines:?}"
    );
    assert_eq!(
        ev_rx.recv_timeout(WAIT).expect("事件必到"),
        IslandEvent::DatagramDropped {
            reason: DropReason::TooLarge,
            n: 2
        }
    );

    // 无连接时的 TunPacket ⇒ `未登记`（登记前丢弃）
    island
        .tx()
        .send(Cmd::TunPacket(vec![0u8; 64].into_boxed_slice()))
        .expect("投包");
    assert!(
        wait_for(&stub, &quic, || island.snapshot().drops.unregistered >= 1, WAIT).await,
        "无登记连接的 TunPacket 必须计 `未登记`"
    );
    let snap = island.snapshot();
    assert_eq!(snap.packets_in, 1, "投递计数照旧");
    assert_eq!(snap.drops.too_large, 2);

    // 有连接后：`TunPacket` 不再计未登记（发送面 = S2-4）
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    let before = island.snapshot().drops.unregistered;
    island
        .tx()
        .send(Cmd::TunPacket(vec![0u8; 64].into_boxed_slice()))
        .expect("投包");
    assert!(
        wait_for(&stub, &quic, || island.snapshot().packets_in >= 2, WAIT).await,
        "第二包已处置"
    );
    assert_eq!(
        island.snapshot().drops.unregistered,
        before,
        "有登记连接后不再计 `未登记`"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// `SetCandidates` 更新清单；`Connect` 的空 `cands` 回落到它（S2-1 的成员语义）。
#[tokio::test]
async fn set_candidates_is_used_when_connect_has_none() {
    let quic = exit_face(11);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let addr = connectable(&quic);
    island
        .tx()
        .send(Cmd::SetCandidates {
            cands: vec![direct(addr)],
        })
        .expect("更新候选");
    let outcome = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: Vec::new(),
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("回落 SetCandidates 的清单 ⇒ 胜出");
    assert_eq!(outcome.winner, addr);
    assert_eq!(island.snapshot().candidates, 1);
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 岛收工后投递命令 ⇒ `EngineGone`（通道已断；不得挂死）。
#[tokio::test]
async fn send_after_stop_is_engine_gone() {
    let quic = exit_face(12);
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(island.is_finished());
    let (tx, rx) = channel();
    let tx_handle: IslandTx = island.tx();
    // 收工后投递：可能仍进队列（接收端已 drop ⇒ 立刻 Err）或直接 Err——两形态都归 EngineGone
    if tx_handle
        .send(Cmd::Connect {
            cands: Vec::new(),
            budget: Duration::from_millis(1),
            reply: tx,
        })
        .is_err()
    {
        return;
    }
    let got = match rx.recv_timeout(BUDGET) {
        Ok(v) => v,
        Err(_) => return, // 回执口随岛退出被 drop ⇒ 调用侧归 EngineGone
    };
    assert!(matches!(got, Err(IslandErr::EngineGone) | Err(IslandErr::NoCandidate)));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 快照 `Default` 面（S3 的 JSON 段基线）：未建连时四类丢弃零值、`via/ep/mtu` 为空。
#[test]
fn snapshot_default_is_empty_path() {
    let s = IslandSnapshot::default();
    assert_eq!(s.via, None);
    assert_eq!(s.ep, None);
    assert_eq!(s.rtt_ms, 0);
    assert_eq!(s.mtu, None);
    assert_eq!(s.current_mtu, 0);
    assert_eq!(s.mirrors, 0);
    assert_eq!(s.drops.too_large, 0);
    assert_eq!(s.drops.send_buffer_full, 0);
    assert_eq!(s.drops.return_queue_full, 0);
    assert_eq!(s.drops.unregistered, 0);
    assert_eq!(s.migrations, 0);
    assert!(!s.migration_unconfirmed);
    assert_eq!(s.local, None, "未起端点前无本地地址");
    assert_eq!(s.connections, 0);
    assert_eq!(s.packets_out, 0);
    assert_eq!(s.relay_tx, 0);
    assert_eq!(s.rx_ignored, 0);
    assert_eq!(s.lost_packets, 0);
    assert_eq!(s.congestion_events, 0);
}

/// `hr-reg4` 帧的**连接绑定**在客户端侧的字节面复核（S1-1 的对偶）：同 secret 同字段但
/// exporter 不同 ⇒ 帧不同（换连接重放必败的客户端侧根据）；Proof 与刷新帧**域分隔**。
#[test]
fn reg4_frames_bind_to_exporter_and_are_domain_separated() {
    let nonce = crate::reg4::Nonce::from_bytes([0x77; 16]);
    let proof_a = ProofFrame::encode(&SECRET, &PUBKEY, &DEV, 1_800_000_000, &nonce, &[0xA1; EXPORTER_LEN]);
    let proof_b = ProofFrame::encode(&SECRET, &PUBKEY, &DEV, 1_800_000_000, &nonce, &[0xB2; EXPORTER_LEN]);
    assert_ne!(proof_a, proof_b, "exporter 不同 ⇒ Proof 不同");
    assert_eq!(&proof_a[..2], b"P4", "魔数 = P4");
    assert_eq!(proof_a.len(), crate::reg4::PROOF_LEN);
    assert_eq!(&proof_a[2..34], &PUBKEY);
    assert_eq!(&proof_a[34..42], &DEV);
    // 刷新帧：另一域（同 exporter、同字段，但 MAC 不同 ⇒ 不可互冒）
    let refresh = RefreshFrame::encode(&SECRET, &PUBKEY, &DEV, 1_800_000_000, &[0xA1; EXPORTER_LEN]);
    assert_eq!(&refresh[..2], b"R4");
    assert_eq!(refresh.len(), crate::reg4::REFRESH_LEN);
    assert_ne!(&refresh[50..66], &proof_a[66..82], "两域 MAC 必须不同（域分隔）");
    let _ = (register::now_unix(), EXPORTER_LABEL);
}

// ---------- 6. 数据面（S2-4）：TUN ⇄ DATAGRAM + 四类计数 ----------

/// **判据（S2-4 主干）**：数据面双向逐字节 —— TUN fd → DATAGRAM（`send_datagram_checked`）
/// 与 DATAGRAM → TUN fd（有界队列 → 写线程）；`packets_in/out` 真计数；需求信号
/// （`swap_out_pkts`/`last_outbound_*`）与 `wgcore` 同接口。
#[tokio::test]
async fn tun_dataplane_is_bidirectional_and_counts() {
    let quic = exit_face(20);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let (tun, peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));

    // 连接前投的包 ⇒ 归 `未登记`（登记前丢弃；准入窗的窗口面）
    let early = inner_pkt(TUN_IP, Ipv4Addr::new(10, 0, 0, 9));
    write_tun(&peer, &early);
    assert!(
        wait_for(&stub, &quic, || island.snapshot().drops.unregistered >= 1, WAIT).await,
        "无已登记连接时投的包必须计 `未登记`"
    );

    connect_direct(&island, &stub, &quic).await;

    // ① 上行：TUN fd → DATAGRAM（登记已过准入窗 ⇒ 不再丢）
    let before = island.snapshot().drops.unregistered;
    let inner = inner_pkt(TUN_IP, Ipv4Addr::new(10, 0, 0, 7));
    write_tun(&peer, &inner);
    let got = wait_packet(&stub, &quic, inner.len(), WAIT)
        .await
        .expect("出口引擎桩必须收到上行包");
    assert_eq!(got, inner, "TUN → DATAGRAM 逐字节");
    assert_eq!(
        island.snapshot().drops.unregistered,
        before,
        "有登记连接后不再计 `未登记`"
    );

    // ② 下行：DATAGRAM → TUN fd（有界队列 → 写线程；逐字节）
    let back = inner_pkt(TUNNEL_IP, TUN_IP);
    assert_eq!(
        quic.send_to_pub(&PUBKEY, &back),
        crate::ExitSend::Handled,
        "出口侧出站必须已在 QUIC 面绑定"
    );
    let echoed = read_tun(&peer, WAIT).expect("回程必须写到 TUN fd");
    assert_eq!(echoed, back, "DATAGRAM → TUN 逐字节");
    assert!(
        wait_for(&stub, &quic, || island.snapshot().packets_out >= 1, WAIT).await,
        "packets_out 必须是真计数（写线程成功写入 TUN 的包数）"
    );

    // ③ 计数与需求信号（demand-driven 的生产者面：岛 = 生产者，接口与 wgcore 同形）
    let snap = island.snapshot();
    assert_eq!(snap.packets_in, 2, "TUN 读线程投递 2 包（早期 1 + 上行 1）");
    assert_eq!(snap.connections, 1, "在用连接数可观测");
    assert_eq!(island.swap_out_pkts(), 2, "出站包计数取走清零");
    assert_eq!(island.swap_out_pkts(), 0, "取走后归零");
    assert!(island.last_outbound_at().is_some(), "单调面可读");
    assert!(island.last_outbound_unix_ms() > 0, "unix 面可读");

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-4 的「超限丢 + 计数」，设计 §6.4/§12-①）**：内层包 > `max_datagram_size()`
/// ⇒ **丢 + 计 `超限` + 节流记行**（不静默）；该包不得到达出口。
///
/// 注入形态：本机 mds = 1362（MTU 1400 − 38B），投一个 1500B 的「内层包」即越限。
/// （真「窄路径」= mds < 1280 需要改客户端 MTU 的旋钮 `HOMEWAY_QUIC_MTU`——那是 S3-1 的
/// 配置面 + S5-1 的实测面；本用例验的是**判据本体** `len > mds`。）
#[tokio::test]
async fn oversize_tun_packet_is_dropped_and_counted() {
    let quic = exit_face(21);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(
        Duration::from_secs(60),
        quic.rpk_public_key(),
        Arc::clone(&logf),
    );
    let (tun, peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));
    connect_direct(&island, &stub, &quic).await;
    let mds = island.snapshot().mtu.expect("已建连 ⇒ mds 可读");
    assert!(mds < 1500, "本机 mds={mds}（MTU 1400 − 38B）⇒ 1500B 必越限");

    let oversize = vec![0x45u8; 1500];
    write_tun(&peer, &oversize);
    assert!(
        wait_for(&stub, &quic, || island.snapshot().drops.too_large >= 1, WAIT).await,
        "超限包必须计 `超限`（不静默）"
    );
    // 负判据：超限包不得被发出去（也不得写回 TUN）
    assert_eq!(stub.packets_len(), 0, "超限包不得到达出口");
    assert!(read_tun(&peer, Duration::from_millis(200)).is_none(), "无回程");

    // S3-3 的「行 ↔ 计数同源」：N-c 行由**同一份** `IslandSnapshot::drops` 渲染
    // （`note_drop_shared` 一处），故计数 +1 必伴随行里的 `超限=1`
    let lines = logs_until(&logs, "丢弃 ", WAIT).await;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("超限=1") && l.contains("回程队列满=0")),
        "N-c 行须与 JSON 同源（超限=1）：{lines:?}"
    );

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-5 / M1 交下项 N8①，设计 §9.1.1）**：`send_buffer_used` 暴露「已入缓冲、
/// 未确认」的 DATAGRAM 字节数——黑洞期那批包（最多 1 MiB）不再**无计数**。
///
/// 形态：TUN 面一次性投递远超 1 MiB 的包 ⇒ 读数必 > 0（主判据）；「缓冲真被占满」的
/// 交叉证据 = 读数高水位与 `发送缓冲满` 计数的**一致性**（见下）。
///
/// **在册 flake 的订正（代码门 r18 C7② 面；本批）**：旧注文断言「单次投递在**无 `await`**
/// 的循环里完成 ⇒ 岛消费这批命令时其 runtime 不处理 IO ⇒ 缓冲不可能被 ACK 排空」，据此硬断
/// `drops.send_buffer_full > 0`——该前提**不成立**（投递走 unbounded 通道，岛的命令循环在
/// 命令之间仍会跑 IO ⇒ 排空速率随调度变化；M2 S2-5 已登记「全量并行跑红 2/2 含干净树基线、
/// 隔离复跑 13 次 1 红」）。现改为**不依赖排空速率**的两条：
/// ①主判据 `max_used > 0`（= N8① 的可观测面，确定性）；
/// ②**一致性**：若 `send_buffer_used` 触顶（≥ 每连接缓冲）而 `发送缓冲满` 仍为 0 ⇒ 计数面漏报。
/// 负判据（未建连 ⇒ 0）与「先建连再投递」的时序不变。
#[tokio::test]
async fn send_buffer_used_grows_under_load() {
    let quic = exit_face(27);
    let stub = Stub::new();
    let (logf, _logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
    let (tun, _peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));

    // 负判据：未建连 ⇒ 0（瞬时量，不是累计计数）
    assert_eq!(island.snapshot().send_buffer_used, 0, "无连接 ⇒ 读数 0");

    connect_direct(&island, &stub, &quic).await;
    // 内层满包（1280B）投 1500 次 ≈ 1.9 MiB > 每连接 1 MiB 缓冲
    let big: Box<[u8]> = {
        let mut p = inner_pkt(TUN_IP, Ipv4Addr::new(10, 0, 0, 7));
        p.resize(1280, 0);
        p.into_boxed_slice()
    };
    for _ in 0..1500 {
        island
            .tx()
            .send(Cmd::TunPacket(big.clone()))
            .expect("投递（unbounded）");
    }
    // 有界轮询（flake 口径②：只判上界）：记录读数的**最大值**，直到读数触顶或「发送缓冲满」
    // 出现（= 缓冲真被占满的两种等价信号），到点即收（不把「排空速率」写进判据）。
    let deadline = Instant::now() + WAIT;
    let mut max_used = 0u64;
    loop {
        let s = island.snapshot();
        max_used = max_used.max(s.send_buffer_used);
        if (max_used > 0 && s.drops.send_buffer_full > 0) || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert!(
        max_used > 0,
        "缓冲被占后读数必须增长（M1 交下项 N8① 的可观测面）：max_used={max_used}"
    );
    // ②**不变量**：读数不得超过「每连接 DATAGRAM 发送缓冲」上界（超了 = 记账面坏了）。
    //    触顶阈值 = 两端共用组装里的 `datagram_send_buffer_size`（`exit/transport.rs`）。
    //    **不再**断言「`发送缓冲满` > 0」（那条依赖排空速率 ⇒ 在册 flake 的成因）；该计数面
    //    的判据在 `exit/tests.rs::send_buffer_full_is_counted_not_silent`（构造性覆盖）。
    let buf = crate::exit::transport::DATAGRAM_BUFFER as u64;
    let max_used = max_used.max(island.snapshot().send_buffer_used);
    assert!(
        max_used <= buf,
        "读数 {} 超过每连接缓冲上界 {}——记账面越界",
        max_used,
        buf
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（代码门 r13 的 M1 修复）**：岛内**第二次采纳**（换连接）后回程必须仍有泵。
///
/// 时序：采纳连接 A（泵 A 在跑）→ 同岛内再赛跑并采纳连接 B（`adopt` 关闭 A）⇒ 泵 A 随 A
/// 收口。防重位若按**裸 bool**（旧形态），`adopt` 结束时它仍为真 ⇒ B **永不起泵**；随后泵 A
/// 退出把位复位，但已无人再触发 ⇒ **B 期间所有回程无人读**（quinn 接收缓冲静默淘汰 = 回程
/// 黑洞，且无计数）。本用例断言：二次采纳后经 B 的回程仍逐字节到达 TUN fd。
///
/// **为什么 B 走第二枚出口**（本用例必须能**确定性**判红旧形态）：若 B 登记到同一出口，
/// 出口会在 B 入册时**先**关闭 A ⇒ 泵 A 可能在 `adopt(B)` 之前就被轮询到并复位防重位，
/// 于是旧形态也能起泵 B（`select!` 的就绪分支次序决定成败 ⇒ 50/50 假绿）。换成第二枚出口
/// 后，A 直到 `adopt(B)` 内部才被 `close`（`adopt` 是同步函数，其间无 await ⇒ 泵 A 不可能
/// 被轮询）⇒ 旧形态下 `maybe_start_pump` 必然早退、B 必然无泵。
#[tokio::test]
async fn return_pump_restarts_after_connection_replacement() {
    let quic = exit_face(22);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let (tun, peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));

    // 连接 A（出口 #1）：基线（回程可用）
    connect_direct(&island, &stub, &quic).await;
    let back = inner_pkt(TUNNEL_IP, TUN_IP);
    assert_eq!(
        quic.send_to_pub(&PUBKEY, &back),
        crate::ExitSend::Handled,
        "A 期内出口侧出站必须已绑定"
    );
    assert_eq!(
        read_tun(&peer, WAIT).as_deref(),
        Some(back.as_slice()),
        "A 的回程（基线）"
    );

    // 连接 B（出口 #2）：岛内二次赛跑 + 二次采纳（A 由 `adopt` 就地 close）。
    // 出口 #2 用**同一枚 RPK seed**（`exit_face(22)` 再起一枚 socket）——岛的钉定是
    // 「按 token 里的公钥钉死」，换 key 会在握手中止（那正是 S1a 的判据，不是本用例的面）。
    let quic2 = exit_face(22);
    let stub2 = Stub::new();
    let addr2 = connectable(&quic2);
    let _ = send_wait(
        &island,
        &stub2,
        &quic2,
        |reply| Cmd::Connect {
            cands: vec![direct(addr2)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("第二次登记成功（出口 #2）");
    assert!(
        wait_for(&stub2, &quic2, || island.snapshot().connections == 1, WAIT).await,
        "二次采纳后仍只有 1 条在用连接（替换语义）"
    );
    assert_eq!(
        quic2.send_to_pub(&PUBKEY, &back),
        crate::ExitSend::Handled,
        "B 期内出口侧出站必须已绑定"
    );
    assert_eq!(
        read_tun(&peer, WAIT).as_deref(),
        Some(back.as_slice()),
        "二次采纳后回程必须仍有泵（旧泵退出不得让现任连接失去回程面）"
    );

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S3-1 / 设计 §12-① ④⑤，MTU 上限旋钮）**：`mtu_cap` 压到 **1200**（区间
/// `[1320,1400]` 之外——**只有测试缝能给这个值**，区间内 `mds ≥ 1282` 产不出
/// 「mds < 内层 MTU」）⇒ `max_datagram_size()` < 1280 且「**窄路径不可用**」行出现
/// （除计数外显式告知：1280 内层包会被全丢，用户感知是断）。
#[tokio::test]
async fn mtu_cap_below_inner_mtu_marks_narrow_path() {
    let quic = exit_face(25);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let mut cfg = island_cfg(Duration::from_secs(60), quic.rpk_public_key());
    cfg.mtu_cap = 1200; // 测试缝（区间外；生产由世代层夹到 [1320,1400]）
    let island = Island::start(Arc::clone(&logf), Arc::new(|_r: &str| {}), cfg).expect("岛可起");
    let (tun, _peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));
    connect_direct(&island, &stub, &quic).await;

    let mds = island.snapshot().mtu.expect("已建连 ⇒ mds 可读");
    assert!(mds < 1280, "MTU 1200 ⇒ mds 应 < 1280（实测 {mds}）");
    assert!(mds >= 1160, "MTU 1200 ⇒ mds ≈ 1162（实测 {mds}）");
    let lines = logs_until(&logs, "窄路径不可用", WAIT).await;
    assert!(
        lines.iter().any(|l| l.contains("窄路径不可用")),
        "`mds < 内层 MTU` 必须另打「窄路径不可用」行：{lines:?}"
    );

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-4 的「回程队列满丢 + 计数」）**：应用侧不读 TUN ⇒ 写线程阻塞 ⇒ 有界队列
/// （2048 条）填满 ⇒ 后续回程包**丢新 + 计 `回程队列满`**（不静默；TCP 会重传）。
///
/// **fd 形态（代码门 r13 的 A3 连带修正）**：本用例用**阻塞 stream** socketpair 当 TUN fd。
/// 旧形态用 `UnixDatagram`，其「对端不读」在 darwin/Linux 上会以 **ENOBUFS** 立刻返回
/// （不是 `WouldBlock`）⇒ `write_fd_all` 归类为硬失败 ⇒ 写线程**退出**（投 `TunFdDead`）——
/// 于是本用例当时命中的其实是「消费者已死」而不是「队列满」（A3 的误归因正是这条路径）。
/// 阻塞 stream 上「对端不读」表现为内核内阻塞（无 ENOBUFS）⇒ 写线程活着卡住 ⇒ 队列真填满
/// ⇒ 命中 `Full` 语义。消费者已死的路径由下一条用例（`…_reports_write_thread_gone…`）覆盖。
#[tokio::test]
async fn return_queue_full_is_dropped_and_counted() {
    let quic = exit_face(22);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    // **不读** 且保持打开：写线程卡在内核缓冲上（阻塞 stream ⇒ 不返 ENOBUFS）
    let (tun, _peer) = UnixStream::pair().expect("socketpair(STREAM)");
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));
    connect_direct(&island, &stub, &quic).await;

    // 灌一批（喂满内核缓冲 ⇒ 写线程阻塞）→ 补足队列上限
    let pkt = inner_pkt(TUNNEL_IP, TUN_IP);
    for _ in 0..(crate::tun::RETURN_QUEUE_MAX + 512) {
        let _ = quic.send_to_pub(&PUBKEY, &pkt);
        if island.snapshot().drops.return_queue_full > 0 {
            break;
        }
        tokio::task::yield_now().await;
    }
    assert!(
        wait_for(
            &stub,
            &quic,
            || island.snapshot().drops.return_queue_full >= 1,
            WAIT
        )
        .await,
        "回程队列满必须计 `回程队列满`（实测 drops={:?}）",
        island.snapshot().drops
    );
    assert_eq!(
        island.snapshot().drops.unregistered,
        0,
        "写线程仍在（阻塞 stream）⇒ 不得走「消费者已死」面"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（代码门 r13 的 A3）**：TUN 写线程已退（消费者消失）⇒ 回程泵**如实归因并收口**：
/// ① 记一行真因（`回程面已终止（TUN 写线程已退）`）；② 丢弃归 `未登记`（**不**误报
/// `回程队列满`——队列没满，是没有消费者）；③ 泵返回后**不再逐包计数**（一次性）。
#[tokio::test]
async fn return_pump_reports_write_thread_gone_and_stops() {
    let quic = exit_face(23);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(
        Duration::from_secs(60),
        quic.rpk_public_key(),
        Arc::clone(&logf),
    );
    let (tun, peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));
    connect_direct(&island, &stub, &quic).await;
    // 消费者对端消失 ⇒ 写线程下一次写必失败（跟 unix DGRAM 的 EPIPE/ECONNREFUSED，
    // 与平台无关的确定性构造）⇒ `write_loop` 投 `TunFdDead` 后退出
    drop(peer);

    let pkt = inner_pkt(TUNNEL_IP, TUN_IP);
    // ⚠️ 包量必须**压满 TUN socketpair 缓冲**：否则写线程前若干次写全部成功（被缓冲吸收）、
    // 不报错也不退出 ⇒ 本用例的前提（消费者已死）在慢/高负载 runner 上不成立（macOS CI 实测红）。
    // 2,048 × 1,280B ≈ 2.6MB ≫ 缓冲（典型 200KB 级）⇒ 写必失败、写线程必退，平台/负载无关。
    for _ in 0..2048 {
        let _ = quic.send_to_pub(&PUBKEY, &pkt);
    }
    assert!(
        wait_for(
            &stub,
            &quic,
            || island.snapshot().drops.unregistered >= 1,
            WAIT
        )
        .await,
        "写线程已退 ⇒ 回程丢弃须归 `未登记`（实测 drops={:?}）",
        island.snapshot().drops
    );
    assert_eq!(
        island.snapshot().drops.return_queue_full,
        0,
        "不得把「消费者已死」误报成「队列满」（A3）"
    );
    let lines = logs_until(&logs, "回程面已终止", WAIT).await;
    assert!(
        lines.iter().any(|l| l.contains("回程面已终止")),
        "必须记一行真因（写线程已退）：{lines:?}"
    );
    // 泵已收口 ⇒ 再灌一批，计数不增长（一次性，不逐包空转）
    let n_after = island.snapshot().drops.unregistered;
    for _ in 0..64 {
        let _ = quic.send_to_pub(&PUBKEY, &pkt);
    }
    let _ = wait_for(&stub, &quic, || false, Duration::from_millis(300)).await;
    assert_eq!(
        island.snapshot().drops.unregistered,
        n_after,
        "泵返回后不得继续逐包计数"
    );

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-4 的「TUN 读线程投递失败自退」，M0 §3.6-2④）**：岛收工后投递失败 ⇒
/// 读线程**自行退出**（记行留证；不重试、不挂死）。
#[tokio::test]
async fn tun_read_thread_exits_when_island_stops() {
    let quic = exit_face(23);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
    let (tun, peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));
    connect_direct(&island, &stub, &quic).await;
    assert!(island.stop_within(Instant::now() + BUDGET), "预算内收工");
    assert!(quic.stop_within(Instant::now() + BUDGET));

    // 岛已收工（命令通道断）：投一包 ⇒ 读线程读到、投递失败 ⇒ 自退 + 记行
    write_tun(&peer, &inner_pkt(TUN_IP, Ipv4Addr::new(10, 0, 0, 8)));
    let lines = logs_until(&logs, "读线程退出", WAIT).await;
    assert!(
        lines.iter().any(|l| l.contains("homeway-tun-read")),
        "投递失败必须由读线程自行退出并留证：{lines:?}"
    );
}

/// **判据（S2-4 / `fd` 分类，镜像 `wgcore::TunFdDead`）**：TUN fd 不可读 ⇒ 岛打既有
/// 取值 `fd` 的分类（同步面据此走 App 重建；分类值 ∈ `{patrol,fd,panic,stop}`）。
#[tokio::test]
async fn tun_fd_death_classifies_fd() {
    let quic = exit_face(24);
    let stub = Stub::new();
    let (on_unhealthy, reasons) = unhealthy_sink();
    let island = island_with_unhealthy(Duration::from_secs(60), quic.rpk_public_key(), on_unhealthy);
    connect_direct(&island, &stub, &quic).await;
    // fd = -1：读线程必然 EBADF（确定性的「fd 不可读」注入；生产形态由 Ohos fd 回收触发）
    assert!(matches!(attach(&island, -1, 1280, BUDGET), Ok(())));
    let got = wait_reason(&reasons, WAIT).await;
    assert_eq!(got.as_deref(), Some("fd"), "fd 面判死必须归既有取值 `fd`");
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 7. 巡检接线（S2-6）与生命周期（S2-5） ----------

/// **判据（M3 S4 重写，**取代** M1 S2-6 的「连接死 ⇒ 即时归 `patrol`」）**：QUIC 连接被人为
/// 关闭 ⇒ 岛**不**就地判不健康——动作面交**岛内快探阶梯**（§3.1：快探 → 复探 → M/R → B），
/// 只有 **B**（连续重连失败 + 窗 ≥10s）才上报 `patrol`。同时保住反向保护：预算极小的探活
/// 失败（连接仍在）不触发任何分类。
///
/// 语义变化登记（S7）：`patrol` 分类的触发源从「连接死（即时）」变为「阶梯走完 M/R（B）」。
#[tokio::test]
async fn connection_death_goes_to_the_ladder_instead_of_unhealthy() {
    let quic = exit_face(25);
    let stub = Stub::new();
    let (on_unhealthy, reasons) = unhealthy_sink();
    let (logf, logs) = sink();
    let island = Island::start(
        logf,
        on_unhealthy,
        island_cfg(Duration::from_secs(60), quic.rpk_public_key()),
    )
    .expect("岛可起");
    // 阶梯只在 TUN 在位后跑（§3.2 的分档表 = attached 前提）⇒ 本用例先附加数据面
    let (tun, _peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));
    connect_direct(&island, &stub, &quic).await;
    assert_eq!(stub.accepted(), 1, "登记成功（真出口侧绑定）");

    // ① 预算极小的探活：回环下可能成功、也可能 ProbeNoResponse——**两种情况都不该分类**
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Probe {
            budget: Duration::from_millis(1),
            reply,
        },
        Duration::from_secs(2),
    )
    .await;
    tokio::time::sleep(Duration::from_millis(600)).await; // 让拍内务至少跑两拍
    assert!(
        wait_reason(&reasons, Duration::from_millis(50))
            .await
            .is_none(),
        "探活失败/超时不得触发分类（不误伤、不拆世代）"
    );

    // ② 人为关闭：出口摘绑定 ⇒ CONNECTION_CLOSE ⇒ 岛交快探阶梯（**不**就地判不健康）
    quic.unbind_pub(&PUBKEY);
    let deadline = Instant::now() + WAIT;
    while island.snapshot().ladder_action.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let snap = island.snapshot();
    let action = snap.ladder_action.clone();
    println!(
        "[unit] ladder_action={action} connections={} fail_streak={}",
        snap.connections, snap.ladder_fail_streak
    );
    assert!(
        matches!(action.as_str(), "migrate" | "reconnect" | "rebuild"),
        "连接死必须交阶梯动作（实得 {action:?}）"
    );
    // ② 连接死**不得就地**判不健康（M1→M3 的语义变化：只有 B〔连续 2 次 R 失败 + 窗 ≥10s〕
    //    才上报）——短窗（400ms）断言照旧保留（本用例的原有判据之一）。
    assert!(
        wait_reason(&reasons, Duration::from_millis(400))
            .await
            .is_none(),
        "连接死不得再就地判不健康（替代面 = 阶梯 M/R；B 才上报）"
    );
    // ③ **终局断言（代码门 r18 ②-1 / C7② 的收紧）**：只断言「首个动作」会放过「阶梯卡死在
    // 单飞态」的形态（旧实现在该形态下 `due()` 恒 None、housekeeping 无 live 也不再命中 ⇒
    // 岛内再无动作，且不报不健康）。判据 = **有终局**：要么连接恢复（`connections == 1`），
    // 要么已上报不健康（B ⇒ 世代重建）。**窗口 25s**：本用例的出口桩不再接受新连接 ⇒ R 必败，
    // 终局走 B，而 B 的门 = 连续 2 次 R 失败 **且** 窗 ≥10s（R 预算 2.8s/轮 ⇒ 实测 ~11–12s 到点；
    // 25s 留足余量防负载抖动）。因 ② 已先跑过 400ms，此处出现的不健康只可能是 B。
    let deadline = Instant::now() + Duration::from_secs(25);
    let mut terminal = String::new();
    while Instant::now() < deadline {
        let s = island.snapshot();
        if s.connections == 1 {
            terminal = "reconnected".to_owned();
            break;
        }
        if wait_reason(&reasons, Duration::from_millis(10)).await.is_some() {
            terminal = "unhealthy(B)".to_owned();
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    println!(
        "[unit] ladder_terminal={terminal} action={} fail_streak={} connections={}",
        island.snapshot().ladder_action,
        island.snapshot().ladder_fail_streak,
        island.snapshot().connections
    );
    assert!(
        !terminal.is_empty(),
        "阶梯必须有终局（重连成功或 B 上报）——不得静默停在单飞态（实得 action={:?} connections={}）",
        island.snapshot().ladder_action,
        island.snapshot().connections
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
    drop(logs); // 日志面在本用例只作现场留痕（判据 = 阶梯动作 + 不误分类 + 有终局）
}

/// **M5 §2.4-A-2 ①/④（无 TUN 岛的阶梯启动；§15-3 待验证项 ③）**：**不** `TunAttach`
/// 的岛（= 宿主会话 `facade/host_session.rs` 的承载形态）在连接死后**同样**必须交快探
/// 阶梯——旧判据 `live && tun`（`:844`/`:859`/`:893`/`:903`）把无 TUN 会话的阶梯与快探
/// 节拍**结构性关闭** ⇒ 宿主会话的「自愈」是空转的假能力。本用例与
/// `connection_death_goes_to_the_ladder_instead_of_unhealthy` 的唯一差别 = 不附加数据面。
#[tokio::test]
async fn ladder_runs_without_tun_attach() {
    let quic = exit_face(27);
    let stub = Stub::new();
    let (on_unhealthy, reasons) = unhealthy_sink();
    let (logf, logs) = sink();
    let island = Island::start(
        logf,
        on_unhealthy,
        island_cfg(Duration::from_secs(60), quic.rpk_public_key()),
    )
    .expect("岛可起");
    // **不** attach（宿主会话无 TUN）：准入照常（登记在握 = QUIC 档唯一的「可用」事实）
    connect_direct(&island, &stub, &quic).await;
    assert_eq!(stub.accepted(), 1, "登记成功（真出口侧绑定）");
    assert!(!island.snapshot().attached, "前置：本用例确实没有 TUN 面");
    // 人为关闭：出口摘绑定 ⇒ CONNECTION_CLOSE ⇒ 无 TUN 的岛也必须交快探阶梯
    quic.unbind_pub(&PUBKEY);
    let deadline = Instant::now() + WAIT;
    while island.snapshot().ladder_action.is_empty() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let action = island.snapshot().ladder_action.clone();
    println!(
        "[unit] no-tun ladder_action={action} connections={} fail_streak={}",
        island.snapshot().connections,
        island.snapshot().ladder_fail_streak
    );
    assert!(
        matches!(action.as_str(), "migrate" | "reconnect" | "rebuild"),
        "无 TUN（宿主会话）的连接死必须交阶梯动作（实得 {action:?}）"
    );
    // 连接死**不得就地**判不健康（与 TUN 档同语义：只有 B 才上报）。
    assert!(
        wait_reason(&reasons, Duration::from_millis(400)).await.is_none(),
        "连接死不得就地判不健康（替代面 = 阶梯 M/R；B 才上报）"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
    drop(logs);
}

/// **判据（S2-5 / M0 §8.1 残余项）**：`stop_within` **到点 detach** 后，老世代仍持
/// UDP 源端口与连接数——两件都**可观测**（快照 + detach 计数行；岛线程卡死、命令面已不可用，
/// 故可观测面必须是轮询面）。
#[tokio::test]
async fn detach_keeps_old_generation_observable() {
    let quic = exit_face(26);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_seam(
        Duration::from_secs(60),
        quic.rpk_public_key(),
        Arc::clone(&logf),
        seams::HANG_WITH_LIVE,
    );
    let (tun, _peer) = tun_pair();
    assert!(matches!(attach(&island, tun.as_raw_fd(), 1280, BUDGET), Ok(())));
    connect_direct(&island, &stub, &quic).await;
    // 确定性同步点：连接在位后的拍内务里卡死（先见到注入行，再起 detach 预算）
    let lines = logs_until(&logs, seams::MARK_HANG_LIVE, WAIT).await;
    assert!(
        lines.iter().any(|l| l.contains(seams::MARK_HANG_LIVE)),
        "注入缝必须先在拍内务里留证：{lines:?}"
    );

    assert!(
        !island.stop_within(Instant::now() + Duration::from_millis(200)),
        "卡死岛到点必须返回 false（detach）"
    );
    // detach 计数行（UDP 源端口 + 连接数）
    let lines = logs_until(&logs, "到点 detach", Duration::from_secs(2)).await;
    let line = lines
        .iter()
        .find(|l| l.contains("老世代仍持 UDP 源端口"))
        .expect("detach 计数行在")
        .clone();
    // 快照（老世代句柄仍可读：可观测面本体）
    let snap = island.snapshot();
    let local = snap.local.expect("UDP 源端口可观测（detach 后亦然）");
    assert_ne!(local.port(), 0, "源端口由内核分配：{local}");
    assert_eq!(snap.connections, 1, "连接数可观测（老世代仍持 1 条）");
    assert!(
        line.contains(&local.to_string()) && line.contains("连接 1 条"),
        "计数行与快照同源：{line}"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------------------------------------------------------------------------
// M3 S1：服务流族（岛侧 API 判据：半关 / 背压 / 配额 / 取消 / 错误面）
// ---------------------------------------------------------------------------

/// 服务流命令的投递 + 有界回执（形态同 [`send_wait`]，但回执口是 `StreamReply`）。
async fn stream_wait<T>(
    island: &Island,
    stub: &Stub,
    quic: &ExitQuic,
    make: impl FnOnce(crate::cmd::StreamReply<T>) -> Cmd,
    wait: Duration,
) -> Result<T, crate::stream::StreamErr> {
    let (tx, rx) = channel();
    island.tx().send(make(tx)).expect("命令投递（unbounded）");
    let deadline = Instant::now() + wait;
    loop {
        stub.pump(quic);
        match rx.try_recv() {
            Ok(v) => return v,
            Err(TryRecvError::Disconnected) => panic!("岛已收工（回执口断开）"),
            Err(TryRecvError::Empty) => {}
        }
        assert!(Instant::now() < deadline, "服务流命令回执超时（岛未应答 ⇒ 挂死）");
        // 1ms 轮询：流命令是**同步入队即答**（背压在 `n` 上，不在回执时延上）⇒ 细粒度
        // 轮询只影响测试自身的观测开销（10ms 轮询会把 bulk 用例的读数打成 harness 噪声）
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

/// **判据（§1.2/§1.3/§1.4 的主干）**：岛侧流面 API 全链——`StreamOpen`（tag 由岛写）⇒
/// `StreamWrite`（有界回执 + 非阻塞接纳）⇒ `StreamRead`（**按需拉取**，回显逐字节相同）
/// ⇒ 同流多次往返 ⇒ `StreamShutdown`（半关 = FIN；对端收工 ⇒ 读回 EOF）⇒ `StreamClose`。
/// 快照计数（open/active/bytes）与事件回调同批断言。
#[tokio::test]
async fn stream_open_write_read_echo_half_close_and_close() {
    let quic = exit_face(0x31);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
    let (ev_tx, ev_rx) = channel();
    island
        .tx()
        .send(Cmd::SetOnEvent {
            h: Arc::new(move |e: IslandEvent| {
                let _ = ev_tx.send(e);
            }),
        })
        .expect("装事件回调");
    let _ = connect_direct(&island, &stub, &quic).await;

    // ① 开流（tag=probe：唯一被出口受理的类别）
    let id = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamOpen {
            tag: crate::StreamTag::Probe,
            reply,
        },
        WAIT,
    )
    .await
    .expect("已登记连接必须能开流");
    assert_eq!(
        ev_rx.recv_timeout(WAIT).expect("开流事件"),
        IslandEvent::StreamOpened {
            tag: crate::StreamTag::Probe
        }
    );

    // ② 写 ⇒ 回显逐字节相同（非阻塞接纳：5B 全额入队）
    let out = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamWrite {
            id,
            data: b"hello".to_vec(),
            reply,
        },
        WAIT,
    )
    .await
    .expect("写回执");
    assert_eq!(out.n, 5, "全额接纳");
    assert!(out.back.is_none(), "全额接纳不回带");
    let got = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamRead { id, reply }, WAIT)
        .await
        .expect("读回显");
    assert_eq!(got, b"hello", "出口逐字节回显（§1.2 的 probe 契约）");

    // ③ 同流再往返（持久流语义：不每拍开新流）
    let _ = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamWrite {
            id,
            data: b"again".to_vec(),
            reply,
        },
        WAIT,
    )
    .await
    .expect("二次写");
    let got2 = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamRead { id, reply }, WAIT)
        .await
        .expect("二次读");
    assert_eq!(got2, b"again");

    // ④ 半关（FIN）：出口读到 EOF ⇒ `finish()` ⇒ 我方读回 EOF（`Closed` = 今天的 EOF 语义）
    stream_wait(&island, &stub, &quic, |reply| Cmd::StreamShutdown { id, reply }, WAIT)
        .await
        .expect("半关入队即答");
    let eof = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamRead { id, reply }, WAIT)
        .await;
    assert_eq!(eof, Err(crate::stream::StreamErr::Closed), "对端 FIN ⇒ 读回 EOF");

    // ⑤ 关流（复位 + 摘表）：重复关 ⇒ `Closed`（未知/已关 id）
    stream_wait(&island, &stub, &quic, |reply| Cmd::StreamClose { id, reply }, WAIT)
        .await
        .expect("关流即答");
    let again = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamClose { id, reply }, WAIT)
        .await;
    assert_eq!(again, Err(crate::stream::StreamErr::Closed), "重入关流必须快速失败");
    let w = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamWrite {
            id,
            data: vec![1],
            reply,
        },
        WAIT,
    )
    .await;
    assert_eq!(w, Err(crate::stream::StreamErr::Closed), "关流后写必须失败");

    // ⑥ 快照计数（在册归零、字节数累计）——轮询到点上界（housekeeping 拍）
    let deadline = Instant::now() + WAIT;
    while island.snapshot().streams_active != 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let snap = island.snapshot();
    assert_eq!(snap.streams_open, 1, "{snap:?}");
    assert_eq!(snap.streams_active, 0, "关流后在册必须归零：{snap:?}");
    assert!(snap.stream_bytes_out >= 10, "↑ 累计 ≥ 10B：{snap:?}");
    assert!(snap.stream_bytes_in >= 10, "↓ 累计 ≥ 10B：{snap:?}");
    assert_eq!(
        ev_rx.recv_timeout(WAIT).expect("关流事件"),
        IslandEvent::StreamClosed {
            tag: crate::StreamTag::Probe
        }
    );
    // ⑦ 行面（C19 族的开/关行；首 3 条必出）
    let lines = logs_until(&logs, "服务流已关", WAIT).await;
    assert!(
        lines.iter().any(|l| l.contains("服务流已开（tag=probe")),
        "开流行：{lines:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("服务流已关（id=1 tag=probe，↑10B ↓10B")),
        "关流行（含字节数）：{lines:?}"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§1.5 的取消面）**：`StreamRead` **无限期挂起**（无预算——本用例 300ms 内必须
/// 无回执）；**取消只经 `Cmd::StreamClose`**：关闭后挂起的读立即回 `Closed`，
/// 且关流后的再次读/写快速失败。
#[tokio::test]
async fn stream_read_hangs_until_close_cancels_it() {
    let quic = exit_face(0x32);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let _ = connect_direct(&island, &stub, &quic).await;
    let id = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamOpen {
            tag: crate::StreamTag::Probe,
            reply,
        },
        WAIT,
    )
    .await
    .expect("开流");

    // ① 不发数据 ⇒ 读挂起（**不得**有任何回执，也不得被预算打断）
    let (tx, rx) = channel();
    island
        .tx()
        .send(Cmd::StreamRead { id, reply: tx })
        .expect("投读命令");
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        matches!(rx.try_recv(), Err(TryRecvError::Empty)),
        "读必须无限期挂起（无预算，§1.5）"
    );

    // ② 关流 ⇒ 在途读被取消（回 `Closed`）
    stream_wait(&island, &stub, &quic, |reply| Cmd::StreamClose { id, reply }, WAIT)
        .await
        .expect("关流即答");
    let cancelled = rx.recv_timeout(WAIT).expect("取消必须唤醒在途读");
    assert_eq!(
        cancelled,
        Err(crate::stream::StreamErr::Closed),
        "取消 ⇒ Closed（EOF 同面）"
    );

    // ③ 关闭后的读：未知 id ⇒ 立即 `Closed`（不等、不挂）
    let after = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamRead { id, reply }, WAIT)
        .await;
    assert_eq!(after, Err(crate::stream::StreamErr::Closed));
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§1.6/§1.7 的配额快速失败）**：自记账容量 = `max_bidi − 2`（N14：控制流 +
/// probe 各占 1）；额度耗尽 ⇒ **快速失败** `Busy`（不等 `open_bi` 阻塞/5s 超时），
/// 且失败行附「本机在册 n/cap」。
#[tokio::test]
async fn stream_quota_exhausts_and_fails_fast_with_busy() {
    let quic = exit_face(0x33);
    let stub = Stub::new();
    let (logf, logs) = sink();
    // 测试缝：把上限压到 4 ⇒ 有效服务容量 2（小配置跑出同一判定）
    let mut cfg = island_cfg(Duration::from_secs(60), quic.rpk_public_key());
    cfg.streams.max_bidi = 4;
    let island = crate::Island::start(logf, Arc::new(|_r: &str| {}), cfg).expect("岛可起");
    let _ = connect_direct(&island, &stub, &quic).await;

    let open = |reply| Cmd::StreamOpen {
        tag: crate::StreamTag::Probe,
        reply,
    };
    let _a = stream_wait(&island, &stub, &quic, open, WAIT).await.expect("第 1 条");
    let _b = stream_wait(&island, &stub, &quic, open, WAIT).await.expect("第 2 条");
    let t0 = Instant::now();
    let third = stream_wait(&island, &stub, &quic, open, WAIT).await;
    assert_eq!(
        third,
        Err(crate::stream::StreamErr::Busy),
        "额度耗尽必须快速失败（不是 5s 超时）"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "快速失败的时延上界（实测 {:?}）",
        t0.elapsed()
    );
    let lines = logs_until(&logs, "服务流失败", WAIT).await;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("服务流失败（tag=probe；入口队列满；本机在册 2/2")),
        "失败行必须附在册读数：{lines:?}"
    );
    // 关一条 ⇒ 立即腾出额度（重新可开）
    let id = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamOpen {
            tag: crate::StreamTag::Probe,
            reply,
        },
        WAIT,
    )
    .await;
    assert_eq!(id, Err(crate::stream::StreamErr::Busy), "在册未减时仍拒");
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§1.4 背压：`n=0` + 原 Vec 带回）**：对端读取停滞（回显被我们的接收窗卡住）
/// ⇒ 待发队列填满 ⇒ `StreamWrite` 回 `n=0` 且 `back` 携带原载荷（调用方走 Ok(0) 退避环）；
/// 快照 `stream_backpressure_events` 与「服务流背压」行同批可见。
#[tokio::test]
async fn stream_write_reports_backpressure_with_original_buffer() {
    // 对端（出口）**显式**每流接收窗 64 KiB：本用例的机制是「对端停读 ⇒ 我方在途窗耗尽」，
    // 该窗必须由用例给定（S9 把设计缺省抬到 4 MiB 后，写 1.25 MiB 不再必然撞窗）。
    let quic = exit_face_streams(
        0x34,
        crate::tuning::StreamLimits { recv_window: 64 * 1024, ..crate::tuning::StreamLimits::design() },
    );
    let stub = Stub::new();
    let (logf, logs) = sink();
    // 测试缝：待发队列 4 KiB（下界）＋**我方**接收窗 32 KiB（让出口回显尽早阻塞）
    let mut cfg = island_cfg(Duration::from_secs(60), quic.rpk_public_key());
    cfg.streams.pending_bytes = 4096;
    cfg.streams.recv_window = 32 * 1024;
    let island = crate::Island::start(logf, Arc::new(|_r: &str| {}), cfg).expect("岛可起");
    let _ = connect_direct(&island, &stub, &quic).await;
    let id = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamOpen {
            tag: crate::StreamTag::Probe,
            reply,
        },
        WAIT,
    )
    .await
    .expect("开流");

    // 只写不读：出口回显填满我方接收窗（32 KiB）⇒ 出口的 echo 阻塞 ⇒ 我方流控窗
    // （对端广告 64 KiB，本用例显式给定）与待发队列（4 KiB）依次填满 ⇒ 必然出现 `n=0`。
    let chunk = vec![0xEEu8; 8 * 1024];
    let mut saw_back = false;
    for _ in 0..160 {
        let out = stream_wait(
            &island,
            &stub,
            &quic,
            |reply| Cmd::StreamWrite {
                id,
                data: chunk.clone(),
                reply,
            },
            WAIT,
        )
        .await
        .expect("写回执");
        if out.n == 0 {
            assert_eq!(
                out.back.as_deref(),
                Some(chunk.as_slice()),
                "零接纳必须**原样带回**（不重拷、不丢）"
            );
            saw_back = true;
            break;
        }
    }
    assert!(saw_back, "对端停读 + 小队列 ⇒ 必须有背压回执（n=0）");
    let lines = logs_until(&logs, "服务流背压", WAIT).await;
    assert!(
        lines.iter().any(|l| l.contains("服务流背压（id=1 tag=probe")),
        "背压行（含 id/tag/队列容量）：{lines:?}"
    );
    let deadline = Instant::now() + WAIT;
    while island.snapshot().stream_backpressure_events == 0 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        island.snapshot().stream_backpressure_events >= 1,
        "快照的背压计数：{:?}",
        island.snapshot().stream_backpressure_events
    );
    // 关流收口（写者任务随之退；不判定它何时退——只要求命令面立即应答）
    stream_wait(&island, &stub, &quic, |reply| Cmd::StreamClose { id, reply }, WAIT)
        .await
        .expect("关流即答");
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§1.4-N14 的自记账容量；标定证据）**：**设计缺省**（`max_bidi=64`）下
/// 恰好能开 **62** 条服务流（= 上限 − 控制流 1 − probe 1），第 63 条被**自记账**拒
/// （`Busy`）——即「自记账域 = {控制流, probe, 服务流}」与 quinn 的并发信用同域；
/// 同时验证 62 条在册流都真被出口受理（不是本地假成功）。
#[tokio::test]
async fn stream_capacity_is_max_bidi_minus_two_at_design_defaults() {
    let quic = exit_face(0x35);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let _ = connect_direct(&island, &stub, &quic).await;
    let cap = island.stream_limits().service_capacity();
    assert_eq!(cap, 62, "设计缺省：64 − 2（N14）");
    let mut ids = Vec::with_capacity(cap);
    for i in 0..cap {
        let id = stream_wait(
            &island,
            &stub,
            &quic,
            |reply| Cmd::StreamOpen {
                tag: crate::StreamTag::Probe,
                reply,
            },
            WAIT,
        )
        .await
        .unwrap_or_else(|e| panic!("第 {} 条必须可开（容量 {cap}）：{e:?}", i + 1));
        ids.push(id);
    }
    // 第 63 条：**自记账**拒（不是等 quinn 信用耗尽/5s 超时）
    let t0 = Instant::now();
    let sixty_third = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamOpen {
            tag: crate::StreamTag::Probe,
            reply,
        },
        WAIT,
    )
    .await;
    assert_eq!(sixty_third, Err(crate::stream::StreamErr::Busy));
    assert!(t0.elapsed() < Duration::from_secs(1), "必须快速失败");
    // 在册计数与容量一致（housekeeping 拍内同步）
    let deadline = Instant::now() + WAIT;
    while island.snapshot().streams_active != cap as u64 && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(island.snapshot().streams_active, cap as u64);
    // 出口侧真受理：每条开流都发了 tag 首字节（探活流在出口的 echo 等待读）——
    // 用「关掉一条 ⇒ 立刻能再开一条」证明额度是真回收（而非记假账）
    stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamClose {
            id: ids[0],
            reply,
        },
        WAIT,
    )
    .await
    .expect("关流");
    let again = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamOpen {
            tag: crate::StreamTag::Probe,
            reply,
        },
        WAIT,
    )
    .await;
    assert!(again.is_ok(), "关一条腾一格：{again:?}");
    for id in ids.iter().skip(1) {
        let _ = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamClose { id: *id, reply }, WAIT).await;
    }
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **标定读数（§7/§15-3）**：1 MiB 走 `STREAM[probe]` 的**逐块往返**（16 KiB 块 × 64 轮，
/// 每轮一写一读）——判据面 = ①字节逐段相同 ②「1 MiB 持续搬运不卡」③**只判上界**（flake 口径②；
/// 本用例的读数是**回程往返 + RPC 轮询**的复合量，**不是**产品 bulk 吞吐口径——§7-N15 的
/// 服务流吞吐相对门槛（≥0.95× WG/UDS 档）要到 S8 用 S2 的 socketpair 泵 + 同刻同承载
/// 对照来测；此处只证明「初值下能持续搬运、窗口/队列不卡」。
///
/// **口径订正（代码门 r18 ③-7）**：旧注文写「1 MiB 总量 > 双方各自的接收窗（256 KiB）⇒ 证明
/// 窗口更新在重负载下不死锁」——S9 起缺省每流窗 = **4 MiB**，1 MiB **不再**越过窗 ⇒ 该论证前提
/// 已不成立（用例仍绿，但不再证明它声称的性质）。**窗口压力面**（越窗搬运 / 窗更新往返不卡）
/// 改由 S9 的 bulk 臂承担（`tools/m3-s9-bulk.sh` + `homeway-core/tests/quic_stream_perf.rs`，
/// 64 MiB 级、含 256 KiB 负向对照臂）；本用例只留「小块逐块往返的字节正确性 + 上界」。
#[tokio::test]
async fn stream_bulk_echo_throughput_is_calibration_evidence() {
    let quic = exit_face(0x36);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let _ = connect_direct(&island, &stub, &quic).await;
    let id = stream_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::StreamOpen {
            tag: crate::StreamTag::Probe,
            reply,
        },
        WAIT,
    )
    .await
    .expect("开流");
    const CHUNK: usize = 16 * 1024;
    const ROUNDS: usize = 64; // 1 MiB
    let t0 = Instant::now();
    let mut done = 0usize;
    for round in 0..ROUNDS {
        let payload: Vec<u8> = (0..CHUNK).map(|i| (i as u8) ^ (round as u8)).collect();
        // 写满额（队列 64 KiB ⇒ 16 KiB 块必全额接纳；若遇背压按契约重发余量）
        let mut off = 0usize;
        while off < payload.len() {
            let rest = payload[off..].to_vec();
            let out = stream_wait(
                &island,
                &stub,
                &quic,
                |reply| Cmd::StreamWrite {
                    id,
                    data: rest.clone(),
                    reply,
                },
                WAIT,
            )
            .await
            .expect("写回执");
            if out.n == 0 {
                // 背压：等一拍重试（与 `SessionWriteHalf` 的 Ok(0) 退避环同款）
                tokio::time::sleep(Duration::from_millis(2)).await;
                continue;
            }
            off += out.n;
            done += out.n;
        }
        // 逐段读回（回显必须逐字节相同）
        let mut got = vec![0u8; payload.len()];
        let mut filled = 0usize;
        while filled < got.len() {
            let part = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamRead { id, reply }, WAIT)
                .await
                .expect("读回显");
            assert!(!part.is_empty(), "回显不得空块（EOF 会回 Closed）");
            got[filled..filled + part.len()].copy_from_slice(&part);
            filled += part.len();
        }
        assert_eq!(got, payload, "第 {round} 轮逐字节相同");
    }
    let elapsed = t0.elapsed();
    let mib_s = (done as f64 / (1024.0 * 1024.0)) / elapsed.as_secs_f64();
    println!(
        "[标定] STREAM[probe] 逐块往返 1 MiB（16 KiB × {ROUNDS}，含 RPC 轮询开销）：耗时 {:?}，复合吞吐 {mib_s:.2} MiB/s",
        elapsed
    );
    assert_eq!(done, ROUNDS * CHUNK, "总字节数");
    // 上界给足（flake 口径：只判上界；本仓全量测试并行跑时 CPU 争用会让该读数抖动）
    assert!(
        elapsed < Duration::from_secs(30),
        "上界：1 MiB 逐块往返必须在 30s 内跑完（flake 口径：只判上界）"
    );
    let _ = stream_wait(&island, &stub, &quic, |reply| Cmd::StreamClose { id, reply }, WAIT).await;
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}
