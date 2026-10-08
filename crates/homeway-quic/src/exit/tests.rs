//! 出口 QUIC 面单测（**真线程 + 真回环 IO**；不钉固定端口、时间断言只判上界——
//! M0 设计 §9.2 flake 口径①②）。
//!
//! 断言面：E-q1 就绪行（实际端口/MTU/缓冲）、收工预算与幂等、端口释放、S1-9 的 RPK
//! 钉定与 Q-O 资源上限；**S1b** 加：准入（`hr-reg3` + exporter 连接绑定 + 换连接重放
//! 必败）、入站（源校验 + 唤醒队列）、出站（`send_datagram_checked` 不静默）。

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cmd::Logf;
use crate::exit::bridge::ExitBridge;
use crate::exit::conn::send_datagram_checked;
use crate::exit::{FaceCtx, ExitStats};
use crate::reg3::{self, Reg3Frame};
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::{ExitInbound, ExitQuic, ExitQuicConfig, Reg3Verdict};

/// 收工预算（与 `wgcore::CLIENT_CLOSE_BUDGET` 同量级 = 2s）。
const BUDGET: Duration = Duration::from_secs(2);
/// 日志等待上界（flake 口径②：只判上界）。
const WAIT: Duration = Duration::from_secs(8);

/// 日志落点 + 收集端。
fn sink() -> (Logf, Receiver<String>) {
    let (tx, rx) = channel();
    (
        Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }),
        rx,
    )
}

/// 有界收集日志行直到出现含 `needle` 的行（`recv_timeout` 当等待——不用阻塞 sleep）。
fn drain_until(rx: &Receiver<String>, needle: &str, wait: Duration) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(line) => {
                let hit = line.contains(needle);
                lines.push(line);
                if hit {
                    return lines;
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("日志未在 {wait:?} 内出现「{needle}」（已收 {} 行：{lines:?}）", lines.len());
}

/// 回环 socket（端口由内核分配后读回——flake 口径①）。
fn loopback_socket() -> UdpSocket {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("回环 :0 必可绑")
}

fn seed(byte: u8) -> Ed25519Seed {
    Ed25519Seed::from_bytes([byte; 32])
}

/// E-q1：端点就绪行含**实际端口 / migration / initial_mtu / datagram 缓冲**。
#[test]
fn endpoint_logs_e_q1_ready_line_with_actual_port() {
    let sock = loopback_socket();
    let want_port = sock.local_addr().unwrap().port();
    let (logf, rx) = sink();
    let quic = ExitQuic::start(
        sock,
        ExitQuicConfig::new(seed(1), 32),
        Arc::clone(&logf),
    )
    .expect("端点可起");

    let lines = drain_until(&rx, "quic: 端点就绪", WAIT);
    let line = lines.last().expect("就绪行在").clone();
    assert!(line.contains(&want_port.to_string()), "行含实际端口：{line}");
    assert!(line.contains("migration=true"), "行含 migration：{line}");
    assert!(line.contains("initial_mtu=1400"), "行含 initial_mtu：{line}");
    assert!(
        line.contains("datagram 缓冲 1048576B"),
        "行含 datagram 缓冲（1 MiB）：{line}"
    );
    assert_eq!(quic.local_addr().port(), want_port, "local_addr = 真实绑定端口");
    assert_eq!(
        quic.rpk_public_key().as_bytes().len(),
        32,
        "出口公钥 = 32B 裸 Ed25519"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET), "预算内收工");
    assert!(quic.is_finished(), "线程已退出");
}

/// 收工后端口释放（socket 随端点 drop 归还内核——退让语义依赖这条）。
#[test]
fn stop_releases_the_udp_port() {
    let sock = loopback_socket();
    let addr: SocketAddr = sock.local_addr().unwrap();
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(sock, ExitQuicConfig::new(seed(2), 32), logf).expect("端点可起");
    assert!(quic.stop_within(Instant::now() + BUDGET));
    drop(quic);
    // 同端口可再绑 = socket 真已释放（UDP 无 TIME_WAIT）
    UdpSocket::bind(addr).expect("收工后同端口应可再绑");
}

/// 收工幂等（重复 `stop_within` 不挂死、不重复 join）。
#[test]
fn stop_within_is_idempotent() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(3), 32), logf)
        .expect("端点可起");
    assert!(quic.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET), "重入即 true");
    assert!(quic.is_finished());
}

// ---------- S1-9：客户端钉定（真握手；错 RPK 必须**握手中止**，不是「连上再拒」） ----------

/// 测试用客户端端点 + 配置（**与岛同形态**：同一份 `transport_config` + RPK 钉定）。
/// S2 的岛接线照此组装（`quinn::ClientConfig::new(client_crypto_config(pin))`）。
fn client_endpoint_and_config(
    pin: RpkPublicKey,
) -> (quinn::Endpoint, quinn::ClientConfig, UdpSocket) {
    let sock = loopback_socket();
    let ep = quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        None,
        sock.try_clone().expect("回环 socket 可克隆"),
        Arc::new(quinn::TokioRuntime),
    )
    .expect("客户端端点可起");
    let mut cfg = quinn::ClientConfig::new(
        super::rpk::client_pin::client_crypto_config(pin).expect("客户端 crypto 配置可建"),
    );
    cfg.transport_config(super::transport::transport_config());
    (ep, cfg, sock)
}

/// 在**上界**内轮询等待（flake 口径②：只判上界、不判区间；不用阻塞 sleep）。
async fn wait_until(mut cond: impl FnMut() -> bool, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    cond()
}

/// **判据（S1-9）**：错 RPK ⇒ 客户端**握手中止**；服务端零采纳 + 有握手失败读数
/// （证明失败发生在握手内，不是"连上再拒"）；对 pin 的对照在下一用例。
#[tokio::test]
async fn rpk_pin_mismatch_aborts_handshake() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(11), 32), logf)
        .expect("端点可起");
    let good = quic.rpk_public_key();
    let wrong = RpkPublicKey::from_bytes([0xEE; 32]);
    assert_ne!(good, wrong, "错 pin 用例必须真的对不上");

    let (client, cfg, _sock) = client_endpoint_and_config(wrong);
    let res = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, quic.local_addr(), super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("握手必须在预算内定音（挂住 = 红）");

    // ① 客户端侧：**没有 Connection**，拿到的是握手期传输错误（TLS alert）
    let err = res.expect_err("错 RPK 不得连上");
    assert!(
        matches!(err, quinn::ConnectionError::TransportError(_)),
        "错 RPK 应为握手期传输错误（TLS alert），实得：{err:?}"
    );
    // ② 服务端侧：零采纳 + 有握手失败（**不是**"连上再拒"）
    assert!(
        wait_until(|| quic.snapshot().handshake_failed >= 1, WAIT).await,
        "服务端应有握手失败读数：{:?}",
        quic.snapshot()
    );
    let snap = quic.snapshot();
    assert_eq!(snap.admitted, 0, "错 RPK 不得采纳任何连接：{snap:?}");
    assert_eq!(snap.connections, 0, "错 RPK 不得留下连接：{snap:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 对 pin（token 里的公钥）= 连上；顺带把 §1.2 的两个可观测面钉住：
/// `max_datagram_size()`（1400 − 38 = 1362，即 §0.3 P2）与发送缓冲 1 MiB 量级
/// （**不是** 4MB——§6.3 裁决）。
#[tokio::test]
async fn rpk_pin_match_connects_with_1_2_transport_readings() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(12), 32), logf)
        .expect("端点可起");
    let pin = quic.rpk_public_key();

    let (client, cfg, _sock) = client_endpoint_and_config(pin);
    let conn = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, quic.local_addr(), super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("握手预算内")
    .expect("对 pin 应连上");

    assert_eq!(
        conn.max_datagram_size(),
        Some(1362),
        "initial_mtu 1400 − 1RTT 开销 38B（§0.3 P2 实测锚点）"
    );
    let space = conn.datagram_send_buffer_space();
    assert!(
        space > (1usize << 20) - 4096 && space <= (1usize << 20),
        "发送缓冲应为 1 MiB 量级（quinn 预留 256B），实得 {space}"
    );

    assert!(
        wait_until(|| quic.snapshot().admitted >= 1, WAIT).await,
        "服务端应采纳连接：{:?}",
        quic.snapshot()
    );
    assert_eq!(quic.snapshot().handshake_failed, 0, "对 pin 不应有握手失败");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **迁移（`migration(true)` 的可观测证明）**：客户端 `rebind` 换本地 socket 后
/// 连接保持、服务端观测到远端地址变化（E-q2 行的前身）。
///
/// 口径说明：本机（darwin）回环只有 `127.0.0.1` 可用（`127.0.0.2` 需 root 加别名），
/// 故此处是**同 IP 换端口**（NAT rebinding 语义）；换 IP 的真迁移由设计探针 P7
/// （`127.0.0.1:*` → LAN `192.168.3.12:*`）与真机用例承接。
#[tokio::test]
async fn rebind_keeps_connection_and_surfaces_path_change() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(13), 32), logf)
        .expect("端点可起");
    let pin = quic.rpk_public_key();
    let (client, cfg, _sock) = client_endpoint_and_config(pin);
    let conn = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, quic.local_addr(), super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("握手预算内")
    .expect("对 pin 应连上");
    assert!(wait_until(|| quic.snapshot().admitted >= 1, WAIT).await, "先采纳");

    // 换本地 socket（同 IP 新端口；内核分配端口——flake 口径①）
    let new_sock = loopback_socket();
    client.rebind(new_sock).expect("rebind 应成功");
    // 迁移后仍能发（连接未断）
    conn.send_datagram(bytes::Bytes::from_static(b"after-rebind"))
        .expect("迁移后发送应成功（连接保持）");

    assert!(
        wait_until(|| quic.snapshot().path_changes >= 1, WAIT).await,
        "服务端应观测到路径变更：{:?}",
        quic.snapshot()
    );
    assert_eq!(quic.snapshot().connections, 1, "迁移不新增连接（§1.7/E-q2 的判读面）");
    assert!(conn.close_reason().is_none(), "迁移后连接不得被关");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- Q-O：保守资源上限（并发握手 ≤64 / 连接总数 ≤2×max_devices / 握手期限 10s） ----------

/// **单向中继**：把客户端包转给出口、**不回**出口的应答 ⇒ 出口侧留下一个永不完成的
/// 握手（确定性造「在途握手」；不 sleep、不钉端口——用读超时轮看 stop 位）。
struct OneWayRelay {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl OneWayRelay {
    /// `exit_addr` = 出口地址（作「哪些包是回程」的判据：来自出口地址的一律丢）。
    fn start(exit_addr: SocketAddr) -> OneWayRelay {
        let sock = loopback_socket();
        let addr = sock.local_addr().unwrap();
        sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while !stop_t.load(Ordering::SeqCst) {
                match sock.recv_from(&mut buf) {
                    Ok((n, from)) if from != exit_addr => {
                        let _ = sock.send_to(&buf[..n], exit_addr);
                    }
                    // 出口回程：丢（这就是「不回」）；超时/错误：下一轮再看 stop 位
                    Ok(_) | Err(_) => {}
                }
            }
        });
        OneWayRelay { addr, stop, thread: Some(thread) }
    }
}

impl Drop for OneWayRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Q-O 闸①：连接总数 = `2 × max_devices`——`max_devices=1` ⇒ 第 3 条被拒（计数 + 记行）。
#[tokio::test]
async fn connection_cap_refuses_beyond_two_max_devices() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(
        loopback_socket(),
        ExitQuicConfig::new(seed(21), 1), // conn_cap = 2
        logf,
    )
    .expect("端点可起");
    let pin = quic.rpk_public_key();

    let mut clients = Vec::new();
    for _ in 0..2 {
        let (client, cfg, sock) = client_endpoint_and_config(pin);
        let conn = tokio::time::timeout(
            WAIT,
            client
                .connect_with(cfg, quic.local_addr(), super::rpk::client_pin::SERVER_NAME)
                .expect("connect 调用面"),
        )
        .await
        .expect("握手预算内")
        .expect("上限内应连上");
        clients.push((client, conn, sock));
    }
    assert!(wait_until(|| quic.snapshot().connections >= 2, WAIT).await, "两条都采纳");

    // 第 3 条：上限命中 ⇒ 拒（客户端拿到握手期错误）
    let (client3, cfg3, _s3) = client_endpoint_and_config(pin);
    let r3 = tokio::time::timeout(
        WAIT,
        client3
            .connect_with(cfg3, quic.local_addr(), super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("拒绝必须在预算内定音");
    assert!(r3.is_err(), "超限连接不得连上");
    assert!(
        wait_until(|| quic.snapshot().conn_refused >= 1, WAIT).await,
        "应有连接总数拒绝计数：{:?}",
        quic.snapshot()
    );
    assert_eq!(quic.snapshot().connections, 2, "存活连接数不因拒绝而变");
    drain_until(&rx, "quic: 拒新连接（连接总数", WAIT); // 记行面（节流首条必打）
    assert!(quic.stop_within(Instant::now() + BUDGET));
    drop(clients);
}

/// Q-O 闸②：并发握手上限——上限调成 1：一条卡住（单向中继）后，第二条被拒。
#[tokio::test]
async fn handshake_cap_refuses_extra_in_flight() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(
        loopback_socket(),
        ExitQuicConfig {
            handshake_cap: 1,
            ..ExitQuicConfig::new(seed(22), 32)
        },
        logf,
    )
    .expect("端点可起");
    let pin = quic.rpk_public_key();
    let relay = OneWayRelay::start(quic.local_addr());

    // ① 经单向中继：Initial 到得了出口、握手完不成 ⇒ 占住唯一的在途槽位
    let (stalled_client, stalled_cfg, _s1) = client_endpoint_and_config(pin);
    let stalled = stalled_client
        .connect_with(stalled_cfg, relay.addr, super::rpk::client_pin::SERVER_NAME)
        .expect("connect 调用面");
    assert!(
        wait_until(|| quic.snapshot().handshakes_in_flight >= 1, WAIT).await,
        "应有在途握手：{:?}",
        quic.snapshot()
    );

    // ② 直连（不经中继）：在途槽位已满 ⇒ 拒
    let (client2, cfg2, _s2) = client_endpoint_and_config(pin);
    let r2 = tokio::time::timeout(
        WAIT,
        client2
            .connect_with(cfg2, quic.local_addr(), super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("拒绝必须在预算内定音");
    assert!(r2.is_err(), "并发握手超限时不得连上");
    assert!(
        wait_until(|| quic.snapshot().handshake_refused >= 1, WAIT).await,
        "应有并发握手拒绝计数：{:?}",
        quic.snapshot()
    );
    drain_until(&rx, "quic: 拒新连接（并发握手", WAIT);
    drop(stalled); // 卡住的客户端放掉（服务端那条在途握手随期限/收工清）
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// Q-O 闸③：握手期限——期限调成 300ms：卡住的握手到点被弃（计数 + 记行）。
#[tokio::test]
async fn handshake_deadline_drops_stalled_handshake() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(
        loopback_socket(),
        ExitQuicConfig {
            handshake_deadline: Duration::from_millis(300),
            ..ExitQuicConfig::new(seed(23), 32)
        },
        logf,
    )
    .expect("端点可起");
    let pin = quic.rpk_public_key();
    let relay = OneWayRelay::start(quic.local_addr());

    let (client, cfg, _s) = client_endpoint_and_config(pin);
    let stalled = client
        .connect_with(cfg, relay.addr, super::rpk::client_pin::SERVER_NAME)
        .expect("connect 调用面");

    assert!(
        wait_until(|| quic.snapshot().handshake_timeouts >= 1, WAIT).await,
        "期限到点应有计数：{:?}",
        quic.snapshot()
    );
    let snap = quic.snapshot();
    assert_eq!(snap.admitted, 0, "卡住的握手不得被采纳：{snap:?}");
    assert_eq!(snap.connections, 0, "卡住的握手不得留下连接：{snap:?}");
    drain_until(&rx, "quic: 握手期限", WAIT);
    drop(stalled);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 缺省上限 = 设计定值（64 并发 / 10s 期限；Q-O）——防实现期被悄悄改小/改大。
#[test]
fn qo_defaults_match_design() {
    let cfg = ExitQuicConfig::new(seed(24), 32);
    assert_eq!(cfg.handshake_cap, 64, "并发握手 ≤64（§9.3 Q-O）");
    assert_eq!(
        cfg.handshake_deadline,
        Duration::from_secs(10),
        "握手期限 10s（§9.3 Q-O）"
    );
    assert_eq!(cfg.conn_cap(), 64, "连接总数 = 2 × max_devices");
}

// ---------- S1b：准入（hr-reg3 + exporter 连接绑定）/ 入站（源校验）/ 出站（checked 发送） ----------

/// 测试用 token secret（「引擎桩」与客户端帧共用；真实 secret 属 `homeway-core`）。
const SECRET: [u8; 32] = [0x5A; 32];
/// 设备派生地址（测试桩给定；真实值 = `homeway-core` 的 `tunnel_addr` 派生）。
const TUNNEL_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 7, 1);
const TUN_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 7, 2);
const TS: u64 = 1_800_000_000;

/// 内层 IPv4 包（源校验用；与 `server/device.rs` 的测试构造同形）。
fn inner_pkt(src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
    let mut p = vec![0u8; 28];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&28u16.to_be_bytes());
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    p
}

/// 测试侧「引擎桩」：只经**真接口**（`drain_inbound`）拿事件，裁决语义 = 核心侧
/// `admit_reg3` 的 MAC 判定（secret + **本连接**的 exporter）。
struct Stub {
    secret: [u8; 32],
    accepted: AtomicU64,
    rejected: AtomicU64,
    packets: Mutex<Vec<Vec<u8>>>,
}

impl Stub {
    fn new(secret: [u8; 32]) -> Arc<Stub> {
        Arc::new(Stub {
            secret,
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            packets: Mutex::new(Vec::new()),
        })
    }

    fn pump(&self, quic: &ExitQuic) {
        quic.drain_inbound(|item| match item {
            ExitInbound::Reg(req) => {
                if req.frame.mac_matches(&self.secret, &req.exporter) {
                    self.accepted.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg3Verdict::Accepted { tunnel_ip: TUNNEL_IP, tun_ip: TUN_IP });
                } else {
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg3Verdict::Rejected);
                }
            }
            ExitInbound::Packet { pkt, .. } => self.packets.lock().unwrap().push(pkt),
        });
    }

    fn packets(&self) -> Vec<Vec<u8>> {
        self.packets.lock().unwrap().clone()
    }

    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::SeqCst)
    }

    fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::SeqCst)
    }
}

/// 有界轮询（每轮先 pump 一次；只判上界）。
async fn pump_until(
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
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 轮询（每轮先 pump 一次）+ 收行：直到出现含 `needle` 的行（超时 panic）。
///
/// 为什么不能直接用 `drain_until`：准入裁决**要引擎桩先回执**（face 把 Reg 请求放进队列后
/// 等 reply）——只等日志而不 pump = 死等。
async fn pump_until_line(
    stub: &Stub,
    quic: &ExitQuic,
    rx: &Receiver<String>,
    needle: &str,
    wait: Duration,
) {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    loop {
        stub.pump(quic);
        while let Ok(line) = rx.try_recv() {
            let hit = line.contains(needle);
            lines.push(line);
            if hit {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "日志未在 {wait:?} 内出现「{needle}」（已收 {} 行：{lines:?}）",
            lines.len()
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// 客户端连接（RPK 钉定匹配；与 S1a 的用例同构）。
async fn client_conn(quic: &ExitQuic) -> (quinn::Endpoint, quinn::Connection, UdpSocket) {
    let (client, cfg, sock) = client_endpoint_and_config(quic.rpk_public_key());
    let conn = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, quic.local_addr(), super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("握手预算内")
    .expect("对 pin 应连上");
    (client, conn, sock)
}

/// 首条 bidi 控制流上写一帧 `hr-reg3`（exporter 取**本连接**的）；返回（发送半边, 帧字节）。
///
/// 发送半边交还调用方保持打开（S2 的 60s 刷新帧只用发送方向；提前 drop 会发 RESET_STREAM）。
async fn send_reg3(
    conn: &quinn::Connection,
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    dev: &[u8; 8],
) -> (quinn::SendStream, [u8; reg3::LEN]) {
    let mut exporter = [0u8; reg3::EXPORTER_LEN];
    conn.export_keying_material(&mut exporter, reg3::EXPORTER_LABEL, b"")
        .expect("客户端必可得 exporter（握手已完成）");
    let frame = Reg3Frame::encode(secret, pubkey, dev, TS, &exporter);
    let (mut send, _recv) = conn.open_bi().await.expect("open_bi");
    // quinn 的流无 `flush`：写入即随驱动发送（66B 远小于流控窗）
    send.write_all(&frame).await.expect("写 reg3 帧");
    (send, frame)
}

/// **判据（S1-3 + S1-4 + S1-5 的主干）**：合法 reg3 ⇒ 绑定（E-q2 采纳行）⇒ 入站数据报
/// 走「源校验 → 引擎桩」字节级到达 ⇒ 出站 `send_to_pub` 经 DATAGRAM 回到客户端；
/// 全程四类丢弃计数为 0。
#[tokio::test]
async fn reg3_admission_binds_and_datagram_round_trip() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(31), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_client, conn, _sock) = client_conn(&quic).await;
    let pubkey = [0x33u8; 32];
    let dev = [0x44u8; 8];
    let (_send, _frame) = send_reg3(&conn, &SECRET, &pubkey, &dev).await;

    // ① 绑定：采纳行在（**行在 ⇒ 绑定表已插**——bind 先插后记行）
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=44444444 tun=100.64.7.2", WAIT).await;
    assert!(quic.snapshot().regs_accepted >= 1, "服务端应有采纳计数：{:?}", quic.snapshot());

    // ② 入站：合法源（tun_ip）的数据报 → 引擎桩字节级收到
    let pkt = inner_pkt(TUN_IP, Ipv4Addr::new(8, 8, 8, 8));
    conn.send_datagram(bytes::Bytes::from(pkt.clone())).expect("客户端发数据报");
    assert!(
        pump_until(&stub, &quic, || !stub.packets().is_empty(), WAIT).await,
        "入站应到引擎桩（源校验面）"
    );
    assert_eq!(stub.packets()[0], pkt, "DATAGRAM → 引擎明文面必须字节级一致");

    // ③ 出站：引擎 → 该设备 = QUIC DATAGRAM（客户端读回同字节）
    assert_eq!(
        quic.send_to_pub(&pubkey, &pkt),
        crate::ExitSend::Handled,
        "已绑定设备 ⇒ 出站走 QUIC 面"
    );
    let got = tokio::time::timeout(WAIT, conn.read_datagram())
        .await
        .expect("预算内应收到")
        .expect("连接活着");
    assert_eq!(got.as_ref(), &pkt[..], "出站 DATAGRAM 字节级一致");

    let s = quic.snapshot();
    assert_eq!(
        (s.drop_too_large, s.drop_send_buffer_full, s.drop_unregistered, s.drop_src_rejected),
        (0, 0, 0, 0),
        "主干路径不得有丢弃：{s:?}"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-3 的核心）**：**同一条 reg3 帧换一条连接（重放）必须失败**——帧里的 MAC 绑定
/// 在原连接的 exporter 上，新连接的 exporter 不同 ⇒ MAC 不符 ⇒ 拒绝 + 关连接；
/// 原连接的绑定不受影响（回程仍发往原连接）。
#[tokio::test]
async fn reg3_replay_on_another_connection_is_rejected() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(32), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0x55u8; 32];
    let dev = [0x66u8; 8];

    // A：正常注册
    let (_ca, conn_a, _sa) = client_conn(&quic).await;
    let (_send_a, frame) = send_reg3(&conn_a, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=66666666", WAIT).await;

    // B：把 A 的 66B 帧原样搬过来（被动窃听者形态）
    let (_cb, conn_b, _sb) = client_conn(&quic).await;
    let (mut send_b, _recv_b) = conn_b.open_bi().await.expect("open_bi");
    send_b.write_all(&frame).await.expect("写重放帧");

    assert!(
        pump_until(&stub, &quic, || stub.rejected() >= 1, WAIT).await,
        "重放帧的 MAC 必须验不过（引擎桩按本连接 exporter 判定）"
    );
    assert!(
        pump_until(&stub, &quic, || quic.snapshot().regs_rejected >= 1, WAIT).await,
        "出口面应有准入拒绝计数：{:?}",
        quic.snapshot()
    );
    assert!(
        pump_until(&stub, &quic, || conn_b.close_reason().is_some(), WAIT).await,
        "重放连接必须被服务端关闭"
    );
    assert_eq!(quic.snapshot().regs_accepted, 1, "只有 A 被采纳（重放不得再采纳一次）");
    assert_eq!(stub.accepted(), 1, "引擎桩只见一次采纳");

    // 原连接不受影响：出站仍发到 A（B 拿不到）
    let pkt = inner_pkt(TUN_IP, Ipv4Addr::new(1, 1, 1, 1));
    assert_eq!(quic.send_to_pub(&pubkey, &pkt), crate::ExitSend::Handled);
    let got = tokio::time::timeout(WAIT, conn_a.read_datagram())
        .await
        .expect("原连接应收到")
        .expect("原连接活着");
    assert_eq!(got.as_ref(), &pkt[..]);
    drop(send_b);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 坏 MAC（换 secret 组帧）⇒ 拒绝 + 关连接 + **不得**出现采纳行。
#[tokio::test]
async fn reg3_bad_mac_is_rejected() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(33), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let (_send, _frame) = send_reg3(&conn, &[0x99u8; 32], &[0x77u8; 32], &[0x88u8; 8]).await;

    assert!(
        pump_until(&stub, &quic, || quic.snapshot().regs_rejected >= 1, WAIT).await,
        "错 MAC 必须被拒：{:?}",
        quic.snapshot()
    );
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "被拒连接必须被关闭"
    );
    assert_eq!(quic.snapshot().regs_accepted, 0, "错 MAC 不得被采纳");
    assert_eq!(stub.packets().len(), 0, "不得有明文包投进来");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-3）**：未登记连接的数据报**直接丢 + 计数 +1**（`未登记`）+ E-q3 行。
#[tokio::test]
async fn datagram_from_unregistered_connection_is_dropped_and_counted() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(34), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await; // **不注册**
    conn.send_datagram(bytes::Bytes::from(inner_pkt(TUN_IP, Ipv4Addr::new(8, 8, 8, 8))))
        .expect("客户端发数据报");

    assert!(
        pump_until(&stub, &quic, || quic.snapshot().drop_unregistered >= 1, WAIT).await,
        "未登记数据报应被丢且计数：{:?}",
        quic.snapshot()
    );
    assert_eq!(stub.packets().len(), 0, "未登记连接的包不得进引擎面");
    assert_eq!(
        quic.snapshot().drop_unregistered, 1,
        "只应计一次（连接的其余流量不存在）"
    );
    drain_until(&rx, "quic: 丢弃 超限=0 发送缓冲满=0 未登记=1 源校验拒=0", WAIT); // E-q3 行
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-4）**：源非法包（src ∉ {tunnel_ip, tun_ip}）⇒ 丢 + `源校验拒` 计数 + E-q3 行；
/// 同连接随后的合法包照常投递（拒的是包不是连接）。
#[tokio::test]
async fn datagram_with_illegal_source_is_dropped_and_counted() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(35), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let pubkey = [0x99u8; 32];
    let dev = [0xAAu8; 8];
    let (_send, _frame) = send_reg3(&conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=aaaaaaaa", WAIT).await;

    // 源非法（9.9.9.9 不在 {tunnel_ip, tun_ip}）
    conn.send_datagram(bytes::Bytes::from(inner_pkt(
        Ipv4Addr::new(9, 9, 9, 9),
        Ipv4Addr::new(8, 8, 8, 8),
    )))
    .expect("发数据报");
    assert!(
        pump_until(&stub, &quic, || quic.snapshot().drop_src_rejected >= 1, WAIT).await,
        "源非法包应计 `源校验拒`：{:?}",
        quic.snapshot()
    );
    assert_eq!(stub.packets().len(), 0, "源非法包不得进引擎面");
    drain_until(&rx, "quic: 丢弃 超限=0 发送缓冲满=0 未登记=0 源校验拒=1", WAIT);

    // 合法（tunnel_ip 源）照常投递
    let good = inner_pkt(TUNNEL_IP, Ipv4Addr::new(8, 8, 8, 8));
    conn.send_datagram(bytes::Bytes::from(good.clone())).expect("发数据报");
    assert!(
        pump_until(&stub, &quic, || !stub.packets().is_empty(), WAIT).await,
        "合法包仍应投递"
    );
    assert_eq!(stub.packets()[0], good);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-3 的现任裁决，§2.2 专2-2）**：同 devTag 后到者替换——旧连接被
/// `CONNECTION_CLOSE`、回程只发往新连接。
#[tokio::test]
async fn same_dev_tag_newer_registration_replaces_old_connection() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(36), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xBBu8; 32];
    let dev = [0xCCu8; 8];

    let (_ca, conn_a, _sa) = client_conn(&quic).await;
    let (_send_a, _fa) = send_reg3(&conn_a, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=cccccccc", WAIT).await;

    let (_cb, conn_b, _sb) = client_conn(&quic).await;
    let (_send_b, _fb) = send_reg3(&conn_b, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 替换旧连接（dev=cccccccc", WAIT).await;
    assert!(quic.snapshot().regs_accepted >= 2, "第二条（同 devTag）应被采纳：{:?}", quic.snapshot());
    assert!(
        pump_until(&stub, &quic, || conn_a.close_reason().is_some(), WAIT).await,
        "旧连接必须被服务端关闭（回程不得发往被丢弃的连接）"
    );

    // 回程只到新连接
    let pkt = inner_pkt(TUN_IP, Ipv4Addr::new(2, 2, 2, 2));
    assert_eq!(quic.send_to_pub(&pubkey, &pkt), crate::ExitSend::Handled);
    let got = tokio::time::timeout(WAIT, conn_b.read_datagram())
        .await
        .expect("新连接应收到")
        .expect("新连接活着");
    assert_eq!(got.as_ref(), &pkt[..]);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-5）**：缓冲满 ⇒ **计数 +1 且不静默**。
///
/// 注入形态（确定性）：客户端**同步**连发（循环内无 `await` ⇒ quinn 的驱动任务拿不到运行
/// 机会 ⇒ 出站缓冲被填满）——正是 §0.3 P4 观察「裸 `send_datagram` 静默淘汰最旧」的形态；
/// 本用例断言换成 [`send_datagram_checked`] 后同一形态变成**可观测丢弃**。
#[tokio::test]
async fn send_buffer_full_is_counted_not_silent() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(37), 32), Arc::clone(&logf))
        .expect("端点可起");
    let (_c, conn, _s) = client_conn(&quic).await;
    let ctx = test_face_ctx(Arc::clone(&logf));

    let pkt = vec![0u8; 1280];
    let (mut sent, mut dropped) = (0u64, 0u64);
    for _ in 0..4000 {
        match send_datagram_checked(&conn, pkt.clone(), &ctx) {
            crate::exit::conn::SendOutcome::Sent => sent += 1,
            crate::exit::conn::SendOutcome::Dropped => dropped += 1,
        }
    }
    assert!(sent >= 1, "注入面应至少发出一个包（实得 sent={sent}）");
    assert!(dropped >= 1, "缓冲满必须被预检拦下（sent={sent}）");
    assert_eq!(
        ctx.stats.snapshot().drop_send_buffer_full,
        dropped,
        "`发送缓冲满` 计数必须与丢弃数一一对应（不静默：既不淘汰旧包也不返 Ok）"
    );
    drain_until(&rx, "quic: 丢弃 超限=0 发送缓冲满=", WAIT); // E-q3 行（首 3 必打）
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 无绑定（含出口面已死）⇒ `Unbound`（调用方回落 WG 原样，不黑洞）。
#[test]
fn send_to_pub_is_unbound_without_binding() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(38), 32), logf)
        .expect("端点可起");
    assert_eq!(
        quic.send_to_pub(&[0x11u8; 32], b"x"),
        crate::ExitSend::Unbound,
        "无绑定设备必须报 Unbound"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
    assert_eq!(
        quic.send_to_pub(&[0x11u8; 32], b"x"),
        crate::ExitSend::Unbound,
        "面已死也必须报 Unbound（出站回落 WG，不黑洞）"
    );
}

/// 测试用出口面上下文（`send_datagram_checked` 的计数/记行面）。
fn test_face_ctx(logf: Logf) -> Arc<FaceCtx> {
    let stats = Arc::new(ExitStats::default());
    let (wake_tx, wake_rx) = std::os::unix::net::UnixStream::pair().expect("self-pipe 可建");
    wake_tx.set_nonblocking(true).unwrap();
    wake_rx.set_nonblocking(true).unwrap();
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(1);
    let bridge = Arc::new(ExitBridge::new(Arc::clone(&stats), logf, wake_tx, wake_rx, out_tx));
    Arc::new(FaceCtx { stats, bridge, logf: Arc::new(|_| {}) })
}
