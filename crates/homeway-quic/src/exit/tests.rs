//! 出口 QUIC 面单测（**真线程 + 真回环 IO**；不钉固定端口、时间断言只判上界——
//! M0 设计 §9.2 flake 口径①②）。
//!
//! 断言面：E-q1 就绪行（实际端口/MTU/缓冲）、收工预算与幂等、端口释放、S1-9 的 RPK
//! 钉定与 Q-O 资源上限；**M2 S1** 加：`hr-reg4` 四帧准入（nonce 一次性 / 窗 / 双期限 /
//! 已绑定再准入门禁 / 版本与域）/ 刷新不重绑 / exporter 连接绑定 / 未认证门禁。

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::cmd::Logf;
use crate::exit::bridge::ExitBridge;
use crate::exit::conn::{bounded_by, send_datagram_checked};
use crate::exit::{FaceCtx, ExitStats, DEFAULT_ADMIT_DEADLINE};
use crate::reg4::{
    self, ACCEPT_MAGIC, CHALLENGE_LEN, ChallengeFrame, EXPORTER_LABEL, EXPORTER_LEN, HelloFrame,
    Nonce, ProofFrame, RefreshFrame,
};
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::{ExitInbound, ExitQuic, ExitQuicConfig, Reg4Verdict, RejectWhy};

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

/// 有界收行（**不要求命中**——「此窗口内不得出现 X」类断言用；到点即返已收行）。
fn collect_for(rx: &Receiver<String>, wait: Duration) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(line) => lines.push(line),
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    lines
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

/// **判据（S2-6 的纯定时语义，设计 §1.3 / 设计门 r14 F3）**：`ADMIT_DEADLINE` 是**真期限**
/// ——静止的准入 future（「握手完成但不发 Hello」的等价面）恰在 10s **虚拟时刻**被弃：
/// 不是无限等，也不是等到 `max_idle_timeout=30s`；且期限与 idle 回收的**序关系**写死在此
/// （期限必须更紧，否则那条连接永不自然过期 ⇒ 单攻击者可用 `conn_cap` 条占满槽位）。
#[tokio::test(start_paused = true)]
async fn admit_deadline_is_a_real_deadline_in_virtual_time() {
    let t0 = tokio::time::Instant::now();
    let got = bounded_by(DEFAULT_ADMIT_DEADLINE, std::future::pending::<()>()).await;
    assert!(got.is_none(), "静止 future 必须在期限内被弃（未认证状态有界）");
    assert_eq!(
        tokio::time::Instant::now() - t0,
        DEFAULT_ADMIT_DEADLINE,
        "恰在 ADMIT_DEADLINE 到点（虚拟时钟，无需等墙钟）"
    );
    assert!(
        DEFAULT_ADMIT_DEADLINE < crate::exit::transport::MAX_IDLE_TIMEOUT,
        "期限必须比 idle 回收（30s）更紧——否则「握手完成但不发 Hello」永不自然过期"
    );
}

// ---------- S1b/M2 S1：准入（hr-reg4 四帧 + exporter 连接绑定）/ 入站（源校验）/ 出站（checked 发送） ----------

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
/// `admit_reg4` 的 MAC 判定（secret + **本连接**的 exporter；两类帧各自域标签）。
struct Stub {
    secret: [u8; 32],
    accepted: AtomicU64,
    rejected: AtomicU64,
    /// 收到的**帧类型**计数（proof / refresh——「nonce 类拒绝不投引擎」的负判据面）。
    proofs: AtomicU64,
    refreshes: AtomicU64,
    /// 刷新帧的裁决改成「设备不在册」（S2-2 第三道前置的出口面接线用例：引擎侧的那道
    /// 判定由真引擎用例 `admit_reg4_refresh_does_not_resurrect_evicted_device` 覆盖，
    /// 这里只注入它的**裁决结果**）。
    evict_refresh: AtomicBool,
    packets: Mutex<Vec<Vec<u8>>>,
}

impl Stub {
    fn new(secret: [u8; 32]) -> Arc<Stub> {
        Arc::new(Stub {
            secret,
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            proofs: AtomicU64::new(0),
            refreshes: AtomicU64::new(0),
            evict_refresh: AtomicBool::new(false),
            packets: Mutex::new(Vec::new()),
        })
    }

    /// 注入「引擎判 = 刷新帧但设备不在册（已淘汰）」（S2-2 用例专用）。
    fn evict_refresh(&self) {
        self.evict_refresh.store(true, Ordering::SeqCst);
    }

    fn pump(&self, quic: &ExitQuic) {
        quic.drain_inbound(|item| match item {
            ExitInbound::Reg(req) => {
                let is_refresh = match req.frame {
                    crate::Reg4Frame::Proof(_) => false,
                    crate::Reg4Frame::Refresh(_) => true,
                };
                if is_refresh {
                    self.refreshes.fetch_add(1, Ordering::SeqCst);
                } else {
                    self.proofs.fetch_add(1, Ordering::SeqCst);
                }
                // 第三道前置的注入：刷新帧一律按「设备不在册」拒（**不看 MAC**——该判定在
                // 真引擎里先于 MAC 试秘，本桩复刻同一序）
                let evicted = is_refresh && self.evict_refresh.load(Ordering::SeqCst);
                if !evicted && req.frame.mac_matches(&self.secret, &req.exporter) {
                    self.accepted.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg4Verdict::Accepted { tunnel_ip: TUNNEL_IP, tun_ip: TUN_IP });
                } else {
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    let why = if evicted {
                        RejectWhy::RefreshNotRegistered
                    } else {
                        RejectWhy::MacMismatch
                    };
                    req.reply(Reg4Verdict::Rejected { why });
                }
            }
            ExitInbound::Packet { pkt, .. } => self.packets.lock().unwrap().push(pkt),
        });
    }

    fn proofs(&self) -> u64 {
        self.proofs.load(Ordering::SeqCst)
    }

    fn refreshes(&self) -> u64 {
        self.refreshes.load(Ordering::SeqCst)
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

/// 本连接的 TLS exporter（客户端侧现算——连接绑定的输入）。
fn exporter_of(conn: &quinn::Connection) -> [u8; EXPORTER_LEN] {
    let mut exporter = [0u8; EXPORTER_LEN];
    conn.export_keying_material(&mut exporter, EXPORTER_LABEL, b"")
        .expect("客户端必可得 exporter（握手已完成）");
    exporter
}

/// 走完四帧（**可失败**）：Hello → 等 `C4` → Proof → 等 `A4`。
///
/// ⚠️ **Accept 之前必须 pump**：出口面把 Proof 投给引擎（本测试的桩）后要等回执，
/// 而回执只有本测试线程能 pump ⇒ 这里「边 pump 边等」（真产品面是独立的引擎线程）。
///
/// 返回 `Err(())` = 任一腿失败（出口拒/连接关）——失败面用例据此断言。
async fn four_frames(
    stub: &Stub,
    quic: &ExitQuic,
    conn: &quinn::Connection,
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    dev: &[u8; 8],
) -> Result<(quinn::SendStream, quinn::RecvStream, Nonce), ()> {
    let exporter = exporter_of(conn);
    let (mut send, mut recv) = conn.open_bi().await.map_err(|_| ())?;
    send.write_all(&HelloFrame::encode(pubkey, dev, TS)).await.map_err(|_| ())?;
    let mut ch = [0u8; CHALLENGE_LEN];
    if recv.read_exact(&mut ch).await.is_err() {
        return Err(());
    }
    let nonce = ChallengeFrame::parse(&ch).ok_or(())?.nonce;
    send.write_all(&ProofFrame::encode(secret, pubkey, dev, TS, &nonce, &exporter))
        .await
        .map_err(|_| ())?;
    // Accept：需要引擎桩回执 ⇒ 边 pump 边等（有界）
    let mut acc = [0u8; reg4::ACCEPT_LEN];
    {
        let fut = recv.read_exact(&mut acc);
        tokio::pin!(fut);
        let deadline = Instant::now() + WAIT;
        loop {
            tokio::select! {
                r = &mut fut => {
                    if r.is_err() {
                        return Err(());
                    }
                    break;
                }
                () = tokio::time::sleep(Duration::from_millis(10)) => {
                    stub.pump(quic);
                    assert!(Instant::now() < deadline, "Accept 超时（引擎桩未回执？）");
                }
            }
        }
    }
    if acc != ACCEPT_MAGIC {
        return Err(());
    }
    Ok((send, recv, nonce))
}

/// 四帧走通用的瘦封装（失败即 panic）。
async fn admit(
    stub: &Stub,
    quic: &ExitQuic,
    conn: &quinn::Connection,
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    dev: &[u8; 8],
) -> (quinn::SendStream, quinn::RecvStream) {
    let (send, recv, _nonce) = four_frames(stub, quic, conn, secret, pubkey, dev)
        .await
        .expect("四帧准入应走通");
    (send, recv)
}

/// 只写 Hello 并取回 Challenge（**不写 Proof**——pending/期限用例的形态）。
async fn hello_and_challenge(
    conn: &quinn::Connection,
    pubkey: &[u8; 32],
    dev: &[u8; 8],
) -> (quinn::SendStream, quinn::RecvStream, Nonce) {
    let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
    send.write_all(&HelloFrame::encode(pubkey, dev, TS)).await.expect("写 Hello");
    let mut ch = [0u8; CHALLENGE_LEN];
    recv.read_exact(&mut ch).await.expect("等 Challenge");
    let nonce = ChallengeFrame::parse(&ch).expect("Challenge 可解").nonce;
    (send, recv, nonce)
}

/// 在**已准入**的连接上写一帧刷新（`R4`）。
async fn send_refresh(
    conn: &quinn::Connection,
    send: &mut quinn::SendStream,
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    dev: &[u8; 8],
) {
    let frame = RefreshFrame::encode(secret, pubkey, dev, TS, &exporter_of(conn));
    assert_eq!(frame.len(), crate::reg4::REFRESH_LEN);
    send.write_all(&frame).await.expect("写刷新帧");
}

/// **判据（S1-1…S1-5 的主干）**：四帧走完 ⇒ 绑定（E-q2 采纳行）⇒ 入站数据报走
/// 「源校验 → 引擎桩」字节级到达 ⇒ 出站 `send_to_pub` 经 DATAGRAM 回到客户端；
/// 全程四类丢弃计数为 0；挑战/接受计数各 +1。
#[tokio::test]
async fn reg4_admission_binds_and_datagram_round_trip() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(31), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_client, conn, _sock) = client_conn(&quic).await;
    let pubkey = [0x33u8; 32];
    let dev = [0x44u8; 8];
    let (_send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;

    // ① 绑定：采纳行在（**行在 ⇒ 绑定表已插**——bind 先插后记行）+ 挑战行在
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=44444444 tun=100.64.7.2", WAIT).await;
    let s = quic.snapshot();
    assert!(s.regs_accepted >= 1, "服务端应有采纳计数：{s:?}");
    assert_eq!(s.challenges_issued, 1, "挑战计数 +1：{s:?}");
    assert_eq!(stub.proofs(), 1, "引擎桩只应看到一帧 Proof");
    assert_eq!(s.regs_rejected, 0, "主干不得有拒绝：{s:?}");

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

/// **判据（S1-2 的 nonce 一次性/跨连接面）**：把 A 连接上的 `P4` 原文搬到 B 连接上
/// ⇒ B 的 nonce 不同 ⇒ 出口面在**投引擎之前**就拒（`nonce 缺失/过期/已消费`）；
/// 原连接的绑定不受影响（回程仍发往 A）。
#[tokio::test]
async fn reg4_proof_replay_on_another_connection_is_rejected() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(32), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0x55u8; 32];
    let dev = [0x66u8; 8];

    // A：正常走完四帧（记下 A 的 nonce 与 exporter ⇒ 可复现 A 的 Proof 原文）
    let (_ca, conn_a, _sa) = client_conn(&quic).await;
    let exporter_a = exporter_of(&conn_a);
    let (_send_a, _recv_a, nonce_a) = four_frames(&stub, &quic, &conn_a, &SECRET, &pubkey, &dev)
        .await
        .expect("A 应走通四帧");
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=66666666", WAIT).await;

    // B：发 Hello 拿自己的 nonce，然后**原样**送 A 的 Proof 字节（被动窃听者形态）
    let (_cb, conn_b, _sb) = client_conn(&quic).await;
    let (mut send_b, mut recv_b) = conn_b.open_bi().await.expect("open_bi");
    send_b.write_all(&HelloFrame::encode(&pubkey, &dev, TS)).await.expect("写 Hello");
    let mut ch = [0u8; CHALLENGE_LEN];
    recv_b.read_exact(&mut ch).await.expect("等 Challenge");
    let replayed = ProofFrame::encode(&SECRET, &pubkey, &dev, TS, &nonce_a, &exporter_a);
    send_b.write_all(&replayed).await.expect("写重放帧");

    // 出口面拒绝（**不投引擎**——nonce 门在 MAC 试秘之前）
    pump_until_line(&stub, &quic, &rx, "nonce 缺失/过期/已消费", WAIT).await;
    assert_eq!(stub.proofs(), 1, "重放帧不得投到引擎（A 那一帧是唯一一次）");
    assert_eq!(stub.rejected(), 0, "引擎桩不得见到重放帧");
    assert!(
        pump_until(&stub, &quic, || conn_b.close_reason().is_some(), WAIT).await,
        "重放连接必须被服务端关闭"
    );
    assert_eq!(quic.snapshot().regs_accepted, 1, "只有 A 被采纳（重放不得再采纳一次）");
    assert_eq!(quic.snapshot().regs_rejected, 1, "重放计一次拒绝");

    // 原连接不受影响：出站仍发到 A（B 拿不到）
    let pkt = inner_pkt(TUN_IP, Ipv4Addr::new(1, 1, 1, 1));
    assert_eq!(quic.send_to_pub(&pubkey, &pkt), crate::ExitSend::Handled);
    let got = tokio::time::timeout(WAIT, conn_a.read_datagram())
        .await
        .expect("原连接应收到")
        .expect("原连接活着");
    assert_eq!(got.as_ref(), &pkt[..]);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-2 的 nonce 门 · 错 nonce）**：secret/exporter/字段全对，但 nonce 不是本连接
/// 出口发的那一枚 ⇒ 出口面在**投引擎之前**拒（`nonce 缺失/过期/已消费`）——引擎桩零帧、
/// 零设备表副作用、连接被关。（真机上无法构造，设计 §7 明列：错 nonce 只本地注入。）
#[tokio::test]
async fn wrong_nonce_is_rejected_before_engine() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(52), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0x81u8; 32];
    let dev = [0x82u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv, nonce) = hello_and_challenge(&conn, &pubkey, &dev).await;

    // 造一枚**不同**的 nonce（同长度、非零、!= 出口那枚）
    let mut wrong = *nonce.as_bytes();
    wrong[0] ^= 0xFF;
    let wrong = crate::reg4::Nonce::from_bytes(wrong);
    assert!(!wrong.ct_eq(&nonce), "用例自身有效：两枚 nonce 必须不同");
    let frame = ProofFrame::encode(&SECRET, &pubkey, &dev, TS, &wrong, &exporter_of(&conn));
    send.write_all(&frame).await.expect("写错 nonce 的 Proof");

    pump_until_line(&stub, &quic, &rx, "nonce 缺失/过期/已消费", WAIT).await;
    assert_eq!(stub.proofs(), 0, "错 nonce 不得投到引擎（门在试秘之前）");
    assert_eq!(quic.snapshot().proof_rejected, 1, "拒绝归到 Proof 段");
    assert_eq!(quic.snapshot().regs_accepted, 0, "不得采纳");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "错 nonce 必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-1 的连接绑定）**：持 secret 但**用别的连接的值算 MAC**（exporter 不符）
/// ⇒ 引擎试秘全不命中 ⇒ `MacMismatch` ⇒ 出口面打 `hr-reg4 MAC 不符——含换连接重放`。
#[tokio::test]
async fn reg4_proof_mac_is_bound_to_exporter() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(39), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0x77u8; 32];
    let dev = [0x88u8; 8];

    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
    send.write_all(&HelloFrame::encode(&pubkey, &dev, TS)).await.expect("写 Hello");
    let mut ch = [0u8; CHALLENGE_LEN];
    recv.read_exact(&mut ch).await.expect("等 Challenge");
    let nonce = ChallengeFrame::parse(&ch).unwrap().nonce;
    // 同 secret、同字段，但 MAC 用的是**另一条连接的** exporter（换连接重放的等价形态）
    let forged = ProofFrame::encode(&SECRET, &pubkey, &dev, TS, &nonce, &[0xEE; EXPORTER_LEN]);
    send.write_all(&forged).await.expect("写错绑定帧");

    pump_until_line(&stub, &quic, &rx, "hr-reg4 MAC 不符——含换连接重放", WAIT).await;
    assert_eq!(stub.proofs(), 1, "该帧投到了引擎（MAC 由引擎判定）");
    assert_eq!(stub.rejected(), 1, "引擎桩按本连接 exporter 判定 ⇒ 不命中");
    assert_eq!(stub.accepted(), 0, "错绑定的帧不得被采纳");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "被拒连接必须被关闭"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 坏 MAC（换 secret 组帧）⇒ 拒绝 + 关连接 + **不得**出现采纳行/绑定。
#[tokio::test]
async fn reg4_bad_mac_is_rejected() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(33), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let _ = four_frames(&stub, &quic, &conn, &[0x99u8; 32], &[0x77u8; 32], &[0x88u8; 8]).await;
    assert!(conn.close_reason().is_some(), "错 MAC 的连接必须已被出口关掉");

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
    assert_eq!(quic.snapshot().proof_rejected, 1, "拒绝归到 Proof 段");
    assert_eq!(stub.packets().len(), 0, "不得有明文包投进来");
    assert_eq!(quic.send_to_pub(&[0x77u8; 32], b"x"), crate::ExitSend::Unbound, "不得有绑定");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-6 的 pending 面）**：Hello 之后**不发 Proof** ⇒ `NONCE_TTL` 到点弃连接
/// （认证超时行 + `pending_expired` 计数；不等 `max_idle_timeout=30s`）。
#[tokio::test]
async fn pending_expires_without_proof() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(
        loopback_socket(),
        ExitQuicConfig {
            nonce_ttl: Duration::from_millis(300),
            ..ExitQuicConfig::new(seed(43), 32)
        },
        logf,
    )
    .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let (_send, _recv, _nonce) = hello_and_challenge(&conn, &[0xAAu8; 32], &[0xBBu8; 8]).await;

    assert!(
        pump_until(&stub, &quic, || quic.snapshot().pending_expired >= 1, WAIT).await,
        "pending 过期应计数：{:?}",
        quic.snapshot()
    );
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "过期必须主动关连接（不等 idle）"
    );
    let s = quic.snapshot();
    assert_eq!((s.challenges_issued, s.pending_expired, s.admit_timeouts), (1, 1, 0), "{s:?}");
    assert_eq!(stub.proofs(), 0, "没有 Proof 到引擎");
    drain_until(&rx, "quic: 认证超时", WAIT);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-6 的连接级期限，设计门 r14 F3）**：**握手完成但从不发 Hello** 的连接
/// 在 `ADMIT_DEADLINE` 内全部被关（`admit_timeouts` = 注入数）；槽位回收后合法连接
/// 仍可被接纳（=「未认证连接不占额度」的资源面证据）。
#[tokio::test]
async fn admit_deadline_reclaims_silent_connections() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(
        loopback_socket(),
        ExitQuicConfig {
            admit_deadline: Duration::from_millis(300),
            ..ExitQuicConfig::new(seed(44), 1) // conn_cap = 2（钉死「占满全部槽位」的形态）
        },
        logf,
    )
    .expect("端点可起");
    let pin = quic.rpk_public_key();
    // 注入 conn_cap 条「只握手、不开控制流」的连接（每条都占一个连接槽）
    let mut silent = Vec::new();
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
        .expect("对 pin 应连上");
        silent.push((client, conn, sock));
    }
    assert!(
        wait_until(|| quic.snapshot().connections == 2, WAIT).await,
        "两条静默连接应占住槽位：{:?}",
        quic.snapshot()
    );

    // 到点：全部被关（对端可见 CONNECTION_CLOSE）+ 计数 + 槽位回收
    assert!(
        wait_until(|| quic.snapshot().admit_timeouts >= 2, WAIT).await,
        "应是全部（2 条）到点回收：{:?}",
        quic.snapshot()
    );
    assert!(
        wait_until(|| quic.snapshot().connections == 0, WAIT).await,
        "槽位应回收：{:?}",
        quic.snapshot()
    );
    assert!(
        wait_until(|| silent.iter().all(|(_, c, _)| c.close_reason().is_some()), WAIT).await,
        "静默连接必须被主动关闭（不等 max_idle_timeout=30s）"
    );
    drain_until(&rx, "quic: 认证超时", WAIT);

    // 槽位回收后：合法客户端照常走完四帧（连接总数上限已腾出）
    let stub = Stub::new(SECRET);
    let (_c3, conn3, _s3) = client_conn(&quic).await;
    let (_send3, _recv3) = admit(&stub, &quic, &conn3, &SECRET, &[0xC1u8; 32], &[0xC2u8; 8]).await;
    assert!(
        pump_until(&stub, &quic, || quic.snapshot().regs_accepted >= 1, WAIT).await,
        "回收后合法连接应能被接纳：{:?}",
        quic.snapshot()
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-8）**：**已绑定连接**再发 `H4`（或 `P4`）⇒ 拒（`已绑定连接的再准入`）+ 关连接；
/// 该连接原来的绑定不受影响（回程仍发往它）——「一条连接只对应一个 dev」。
#[tokio::test]
async fn bound_connection_re_admission_is_rejected() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(45), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xD1u8; 32];
    let dev = [0xD2u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=d2d2d2d2", WAIT).await;

    // 再发一个 Hello（另一台「设备」的身份）⇒ 必须被拒
    send.write_all(&HelloFrame::encode(&[0xE1u8; 32], &[0xE2u8; 8], TS))
        .await
        .expect("写第二个 Hello");
    pump_until_line(&stub, &quic, &rx, "已绑定连接的再准入", WAIT).await;
    assert_eq!(stub.proofs(), 1, "第二个 Hello 不得产生 Proof 投递");
    assert_eq!(quic.snapshot().challenges_issued, 1, "不得再发第二次挑战");
    assert_eq!(quic.snapshot().challenges_refused, 1, "拒绝归到「未发挑战」段");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "再准入必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-8 的正向半边 + 刷新面 · r14 F11）**：已绑定连接的刷新帧 ⇒ 引擎 `Accepted`
/// ⇒ **不重绑、不打第二条 E-q2**（行频从「每次 Accepted」收成「仅首次准入」）。
#[tokio::test]
async fn refresh_on_bound_connection_does_not_rebind() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(46), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xF1u8; 32];
    let dev = [0xF2u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=f2f2f2f2", WAIT).await;
    let accepted0 = quic.snapshot().regs_accepted;

    send_refresh(&conn, &mut send, &SECRET, &pubkey, &dev).await;
    assert!(
        pump_until(&stub, &quic, || stub.refreshes() >= 1, WAIT).await,
        "刷新帧应投到引擎"
    );
    assert!(
        pump_until(&stub, &quic, || quic.snapshot().regs_accepted > accepted0, WAIT).await,
        "刷新应被采纳（计数增长）"
    );
    // 记账面：刷新不计 E-q2（在收行窗口内找第二条采纳行）
    let lines = collect_for(&rx, Duration::from_millis(200));
    assert_eq!(
        lines.iter().filter(|l| l.contains("quic: 连接采纳")).count(),
        0,
        "刷新成功不得再打采纳行：{lines:?}"
    );
    assert!(conn.close_reason().is_none(), "刷新不得关连接");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-8 的拒绝面）**：**未绑定**连接上的刷新帧 ⇒ 拒（`刷新帧但连接未绑定`）。
#[tokio::test]
async fn refresh_on_unbound_connection_is_rejected() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(47), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = conn.open_bi().await.expect("open_bi");
    let frame = RefreshFrame::encode(&SECRET, &[0x91u8; 32], &[0x92u8; 8], TS, &exporter_of(&conn));
    send.write_all(&frame).await.expect("写刷新帧");

    pump_until_line(&stub, &quic, &rx, "刷新帧但连接未绑定", WAIT).await;
    assert_eq!(stub.refreshes(), 0, "未绑定连接的刷新帧不得投引擎");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "未绑定刷新必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-2 的第三道前置，出口面半边）**：引擎判「刷新帧但设备不在册（已淘汰）」⇒
/// 出口面 ①按类型化 `why` 打归因行（可辨、fail-visible）②**关连接**（不 resurrect：连接
/// 不会活着等下一拍刷新）；拒绝不计入 `regs_accepted`。
#[tokio::test]
async fn refresh_of_evicted_device_is_rejected_with_reason_and_closed() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(49), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    stub.evict_refresh();
    let pubkey = [0xF3u8; 32];
    let dev = [0xF4u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    // 准入（Proof 帧不受第三道前置约束）⇒ 绑定
    let (mut send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    assert_eq!(quic.snapshot().regs_accepted, 1, "准入那一次采纳");

    // 刷新帧（帧内身份 == 绑定、MAC 合法）⇒ 仍被第三道前置拒
    send_refresh(&conn, &mut send, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "刷新帧但设备不在册", WAIT).await;
    assert_eq!(
        quic.snapshot().regs_accepted,
        1,
        "被拒刷新不得计入采纳（regs_accepted 不动）"
    );
    assert_eq!(stub.accepted(), 1, "只有准入那一次");
    assert_eq!(stub.rejected(), 1, "刷新被拒一次");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "刷新被拒必须关连接（不 resurrect）"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-1 的版本互斥 + r14 F4 的可辨归因）**：`H3` 字节（M1 的 `hr-reg3` 帧）⇒
/// 拒 + 归因串**可辨**（`帧版本不符（H2/H3——旧核或垃圾包）`），不投引擎。
#[tokio::test]
async fn legacy_h3_frame_is_rejected_with_distinct_reason() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(48), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = conn.open_bi().await.expect("open_bi");
    // M1 的 `H3` 帧（66B）：`"H3"‖pubkey32‖devTag8‖ts8‖mac16`
    let mut h3 = [0u8; 66];
    h3[..2].copy_from_slice(b"H3");
    h3[2..34].copy_from_slice(&[0x81u8; 32]);
    send.write_all(&h3).await.expect("写 H3 帧");

    pump_until_line(&stub, &quic, &rx, "帧版本不符（H2/H3——旧核或垃圾包）", WAIT).await;
    assert_eq!(stub.proofs(), 0, "旧版本帧不得投引擎");
    assert_eq!(quic.snapshot().challenges_refused, 1, "归到「未发挑战」段");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "旧版本帧必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-2 的重复 Hello · r14 F22）**：一个连接只接受一个 Hello——第二个 Hello
/// （pending 在/不在都一样）⇒ 拒 + 关连接。
#[tokio::test]
async fn second_hello_is_rejected() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(49), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv, _nonce) = hello_and_challenge(&conn, &[0xA1u8; 32], &[0xA2u8; 8]).await;
    send.write_all(&HelloFrame::encode(&[0xA1u8; 32], &[0xA2u8; 8], TS))
        .await
        .expect("写第二个 Hello");

    pump_until_line(&stub, &quic, &rx, "重复 Hello（一个连接只接受一个 Hello）", WAIT).await;
    assert_eq!(quic.snapshot().challenges_issued, 1, "第二次 Hello 不得再发挑战");
    assert_eq!(stub.proofs(), 0, "不得投引擎");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "重复 Hello 必须关连接"
    );
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
    let (_send_a, _recv_a) = admit(&stub, &quic, &conn_a, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=cccccccc", WAIT).await;

    let (_cb, conn_b, _sb) = client_conn(&quic).await;
    let (_send_b, _recv_b) = admit(&stub, &quic, &conn_b, &SECRET, &pubkey, &dev).await;
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

/// **判据（S1-8 的三索引一致性 · r14 F15）**：`by_dev`/`by_pub`/`by_conn` 在
/// **绑定 / 替换 / 摘除**后逐次互指一致，且「一条连接只对应一个 dev」——同 devTag 后到者
/// 替换后旧连接被关、其索引清空（不留悬挂）。
#[tokio::test]
async fn binding_indexes_stay_consistent_across_bind_replace_unbind() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(50), 4), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    quic.bridge.assert_bindings_consistent();

    // ① 两条独立设备各绑一条连接
    let (pub_a, dev_a) = ([0x11u8; 32], [0x21u8; 8]);
    let (pub_b, dev_b) = ([0x12u8; 32], [0x22u8; 8]);
    let (_ca, conn_a, _sa) = client_conn(&quic).await;
    let (_sa_ctl, _ra) = admit(&stub, &quic, &conn_a, &SECRET, &pub_a, &dev_a).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=21212121", WAIT).await;
    quic.bridge.assert_bindings_consistent();
    let (_cb, conn_b, _sb) = client_conn(&quic).await;
    let (_sb_ctl, _rb) = admit(&stub, &quic, &conn_b, &SECRET, &pub_b, &dev_b).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=22222222", WAIT).await;
    quic.bridge.assert_bindings_consistent();

    // ② 同 devTag 换公钥（轮换）：后到者替换 ⇒ 旧连接关、三索引仍互指
    let pub_a2 = [0x13u8; 32];
    let (_cc, conn_c, _sc) = client_conn(&quic).await;
    let (_sc_ctl, _rc) = admit(&stub, &quic, &conn_c, &SECRET, &pub_a2, &dev_a).await;
    pump_until_line(&stub, &quic, &rx, "quic: 替换旧连接（dev=21212121", WAIT).await;
    quic.bridge.assert_bindings_consistent();
    assert!(
        pump_until(&stub, &quic, || conn_a.close_reason().is_some(), WAIT).await,
        "旧连接必须被关（不留悬挂索引）"
    );
    // 旧公钥的出站回落（无绑定）⇒ 不黑洞也不误发
    assert_eq!(quic.send_to_pub(&pub_a, b"x"), crate::ExitSend::Unbound, "旧公钥绑定已摘");
    let pkt = inner_pkt(TUN_IP, Ipv4Addr::new(3, 3, 3, 3));
    assert_eq!(quic.send_to_pub(&pub_a2, &pkt), crate::ExitSend::Handled);
    let got = tokio::time::timeout(WAIT, conn_c.read_datagram())
        .await
        .expect("新连接应收到")
        .expect("新连接活着");
    assert_eq!(got.as_ref(), &pkt[..], "回程只到新连接");

    // ③ 摘绑定（设备表摘除/轮换路径）：三索引清干净
    quic.unbind_pub(&pub_a2);
    quic.bridge.assert_bindings_consistent();
    assert!(
        pump_until(&stub, &quic, || conn_c.close_reason().is_some(), WAIT).await,
        "摘绑定必须关连接"
    );
    quic.unbind_pub(&pub_b);
    quic.bridge.assert_bindings_consistent();
    assert_eq!(quic.send_to_pub(&pub_b, b"x"), crate::ExitSend::Unbound, "全部摘净");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-3 / §2.2 门禁）**：未认证（四帧未完成）连接的数据报**直接丢 + 计数 +1**
/// （`未登记`）+ E-q3 行——**不触碰引擎、不产生绑定**（「未认证连接不占额度」的数据面）。
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
    assert_eq!(quic.snapshot().challenges_issued, 0, "未认证连接请求不到挑战之外的任何东西");
    assert_eq!(
        quic.send_to_pub(&[0xABu8; 32], b"x"),
        crate::ExitSend::Unbound,
        "未认证连接不得产生绑定（出站回落 WG）"
    );
    drain_until(&rx, "quic: 丢弃 超限=0 发送缓冲满=0 未登记=1 源校验拒=0", WAIT); // E-q3 行
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1 的门禁面 · §2.2-1）**：**准入未完成**（Hello/Challenge 在途、Proof 未到）的连接
/// 发来的数据报同样被门禁拦下（丢 + `未登记`）；四帧走完后同一连接的报文照常到达
/// （门禁随裁决面开合）。
#[tokio::test]
async fn datagram_is_gated_until_admission_completes() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(51), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0x71u8; 32];
    let dev = [0x72u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;

    // ① 未认证（连 Hello 都还没发）：数据报丢弃 + 计数
    let early = inner_pkt(TUN_IP, Ipv4Addr::new(8, 8, 8, 8));
    conn.send_datagram(bytes::Bytes::from(early)).expect("发数据报");
    assert!(
        pump_until(&stub, &quic, || quic.snapshot().drop_unregistered >= 1, WAIT).await,
        "未认证连接的数据报必须丢 + 计数：{:?}",
        quic.snapshot()
    );
    assert_eq!(stub.packets().len(), 0, "未认证连接的包不得进引擎面");

    // ② 四帧走完（门禁开）⇒ 同形数据报照常到达
    let (_send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=72727272", WAIT).await;
    let good = inner_pkt(TUN_IP, Ipv4Addr::new(8, 8, 8, 8));
    conn.send_datagram(bytes::Bytes::from(good.clone())).expect("发数据报");
    assert!(
        pump_until(&stub, &quic, || !stub.packets().is_empty(), WAIT).await,
        "准入完成后数据报应到达引擎面"
    );
    assert_eq!(stub.packets()[0], good);
    assert_eq!(
        quic.snapshot().drop_unregistered,
        1,
        "只应计未认证那一次：{:?}",
        quic.snapshot()
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-4）**：源非法包（src ∉ {tunnel_ip, tun_ip}）⇒ 丢 + `源校验拒` 计数 + E-q3 行；
/// 同连接随后的合法包照常投递（拒的是包不是连接）。明细行含**实际 src**（设计 §9.1-1 的
/// 增强：M1 真机发现①的「无法一眼定性」在这里关闭）。
#[tokio::test]
async fn datagram_with_illegal_source_is_dropped_and_counted() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(35), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let pubkey = [0x99u8; 32];
    let dev = [0xAAu8; 8];
    let (_send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
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
    let ls = drain_until(&rx, "quic: 丢弃 超限=0 发送缓冲满=0 未登记=0 源校验拒=1", WAIT);
    assert!(
        ls.iter().any(|l| l.contains("src=9.9.9.9")),
        "E-q3 行的明细必须含实际 src（否则真机上仍无法定性）：{ls:?}"
    );

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
    Arc::new(FaceCtx { proof_gate: std::sync::Arc::new(std::sync::Mutex::new(
        crate::exit::admit::ProofGate::new(crate::exit::admit::PROOF_FAIL_THRESHOLD_DEFAULT),
    )),
        stats,
        bridge,
        logf: Arc::new(|_| {}),
        admit_deadline: crate::exit::DEFAULT_ADMIT_DEADLINE,
        nonce_ttl: crate::exit::DEFAULT_NONCE_TTL,
        conn_cap: 64,
    })
}

// ---------- S1-6：中继承载（kind=5 + 自定义 AsyncUdpSocket） ----------

/// 测试用**最小中继 + 「引擎」双子**（`relay/mod.rs` 拨腿模式的等价物）：
///
/// ```text
/// 客户端 ──裸 QUIC──▶ [R]（中继数据口）
///   [R] 收客户端 ⇒ 包腿帧 [0xBB][5] ⇒ 送腿 socket（出口侧）
///   [腿 socket 读侧 = 「引擎」] ⇒ ExitQuic::inject_leg(R, 载荷) ⇒ quinn
///   quinn 回程 ⇒ Transmit.destination = R ⇒ 腿表命中 ⇒ 包 [0xBB][5] ⇒ 腿 socket
///   [R] 收腿帧（src = 腿本地地址）⇒ 剥壳 ⇒ 裸 QUIC 回客户端
/// ```
///
/// 三条职责刻意分开（与产品路径一一对应）：`R` = 中继（kind 原样透传）、腿 socket 的
/// **读侧** = 引擎（唯一读者 → 注入）、**写侧**（QUIC 面的 `try_clone` 句柄）= 腿表。
/// 客户端的包封/剥壳（S2-7）在此由 `R` 代劳（测试客户端说裸 QUIC）。
struct MiniRelay {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    injected: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl MiniRelay {
    fn start(quic: Arc<ExitQuic>) -> MiniRelay {
        let r = loopback_socket(); // 中继数据口（客户端直连目标 = QUIC 眼里的对端）
        let r_addr = r.local_addr().expect("已绑定");
        let leg = loopback_socket(); // 出口腿（connect 到 R）
        leg.connect(r_addr).expect("腿 socket connect");
        leg.set_nonblocking(true).expect("腿 socket 非阻塞");
        let leg_addr = leg.local_addr().expect("已绑定");
        // 出口 QUIC 面的腿表：远端 = R ⇒ 发送走这条腿并包 [0xBB][5]
        quic.leg_open(r_addr, leg.try_clone().expect("腿 socket 可克隆"));
        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = Arc::clone(&stop);
        let injected = Arc::new(AtomicU64::new(0));
        let injected_t = Arc::clone(&injected);
        let thread = std::thread::spawn(move || {
            let r_clone = r.try_clone().expect("R 可克隆");
            // 阻塞读 + 读超时（**不用 sleep**：隔离门 ③ 对 `std::thread::sleep` 零豁免，
            // 测试线程也一样；5ms 超时让两个方向轮流被服务，延迟影响 ≪ WAIT 预算）。
            let tick = Some(Duration::from_millis(5));
            leg.set_read_timeout(tick).expect("设读超时");
            r_clone.set_read_timeout(tick).expect("设读超时");
            let mut client: Option<SocketAddr> = None;
            let mut buf = [0u8; 4096];
            while !stop_t.load(Ordering::SeqCst) {
                // 「引擎」侧：读腿 socket（唯一读者）→ 剥壳后注入 QUIC 面（src = 腿远端 R）
                if let Ok((n, _src)) = leg.recv_from(&mut buf) {
                    if n >= 2 && buf[0] == 0xBB && buf[1] == 5 {
                        injected_t.fetch_add(1, Ordering::SeqCst);
                        quic.inject_leg(r_addr, buf[2..n].to_vec());
                    }
                }
                // 中继侧
                if let Ok((n, src)) = r_clone.recv_from(&mut buf) {
                    if src == leg_addr {
                        // 出口腿 → 客户端：剥壳（腿帧 [0xBB][5]）
                        if n >= 2 && buf[0] == 0xBB && buf[1] == 5 {
                            if let Some(c) = client {
                                let _ = r_clone.send_to(&buf[2..n], c);
                            }
                        }
                    } else {
                        // 客户端 → 出口腿：包壳（兼 S2-7 的客户端组帧）
                        client = Some(src);
                        let mut out = Vec::with_capacity(n + 2);
                        out.push(0xBB);
                        out.push(5);
                        out.extend_from_slice(&buf[..n]);
                        let _ = r_clone.send_to(&out, leg_addr);
                    }
                }
            }
        });
        MiniRelay { addr: r_addr, stop, injected, thread: Some(thread) }
    }

    fn injected(&self) -> u64 {
        self.injected.load(Ordering::SeqCst)
    }
}

impl Drop for MiniRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// **判据（S1-6 最小原型，§1.6/Q-G）**：一条 quinn 连接**完全经中继腿**跑通——
/// ①握手（入口 = 腿帧 `[0xBB][5]` 注入）；②`hr-reg4` 四帧准入（绑定在腿连接上）；
/// ③入站数据报（注入 → 源校验 → 引擎桩）；④出站数据报（腿表命中 → 包 `[0xBB][5]`）；
/// ⑤同端点**直连路径并存**（另一条连接直连 QUIC 端口照常）。
///
/// 顺带钉住：`max_datagram_size() == 1362` 在腿路径上**与直连同值**（信封加在外层，
/// 不减内层容量——设计 §5.1 的不变量）。
#[tokio::test]
async fn relay_leg_carries_quic_handshake_and_datagrams() {
    let (logf, rx) = sink();
    let quic = Arc::new(
        ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(41), 32), logf)
            .expect("端点可起"),
    );
    let stub = Stub::new(SECRET);
    let relay = MiniRelay::start(Arc::clone(&quic));

    // ① 经腿握手：客户端连到中继数据口 R（只有 [0xBB][5] 一条路能到出口）
    let (client, cfg, _sock) = client_endpoint_and_config(quic.rpk_public_key());
    let conn = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, relay.addr, super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("腿路径握手必须在预算内定音")
    .expect("经腿应连上");
    assert!(
        wait_until(|| quic.snapshot().admitted >= 1, WAIT).await,
        "服务端应采纳经腿的连接：{:?}",
        quic.snapshot()
    );
    assert!(relay.injected() >= 1, "腿帧必须真被注入（实得 {}）", relay.injected());
    assert_eq!(conn.max_datagram_size(), Some(1362), "腿路径 mds 与直连同值（信封在外层）");

    // ② 准入：四帧走腿路径（绑定 peer = 腿远端 R）
    let pubkey = [0xD1u8; 32];
    let dev = [0xD2u8; 8];
    let (_send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "quic: 连接采纳 dev=d2d2d2d2", WAIT).await;
    assert!(quic.snapshot().regs_accepted >= 1, "腿路径准入应通过：{:?}", quic.snapshot());

    // ③ 入站：客户端数据报 → 中继包壳 → 注入 → 源校验 → 引擎桩（字节级一致）
    let pkt = inner_pkt(TUN_IP, Ipv4Addr::new(8, 8, 8, 8));
    conn.send_datagram(bytes::Bytes::from(pkt.clone())).expect("客户端发数据报");
    assert!(
        pump_until(&stub, &quic, || !stub.packets().is_empty(), WAIT).await,
        "腿路径入站应到引擎桩"
    );
    assert_eq!(stub.packets()[0], pkt, "腿路径 DATAGRAM 字节级一致");

    // ④ 出站：引擎 → 该设备 = 腿帧 [0xBB][5] → 中继剥壳 → 客户端读回同字节
    assert_eq!(quic.send_to_pub(&pubkey, &pkt), crate::ExitSend::Handled);
    let got = tokio::time::timeout(WAIT, conn.read_datagram())
        .await
        .expect("预算内应收到")
        .expect("连接活着");
    assert_eq!(got.as_ref(), &pkt[..], "腿路径出站 DATAGRAM 字节级一致");

    // ⑤ 直连路径并存：另一条连接直连 QUIC 端口照常（同一端点、同一抽象 socket）
    let (_c2, conn2, _s2) = client_conn(&quic).await;
    assert!(conn2.max_datagram_size().is_some(), "直连连接照常可用");

    let s = quic.snapshot();
    assert_eq!(
        (s.drop_too_large, s.drop_send_buffer_full, s.drop_unregistered, s.drop_src_rejected),
        (0, 0, 0, 0),
        "腿路径主干不得有丢弃：{s:?}"
    );
    drop(relay);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-6，§1.6 发送侧路由的兜底）**：腿摘除后对该远端的发送**丢 + 计数**、
/// **不回落直连端口**（照 `server/bind.rs` 的 #17：打到中继数据口只会污染别的会话）。
///
/// 直接驱动 `ExitSock`（不建 quinn 连接——该行为与连接状态无关，只由腿表决定）；
/// 「摘腿后仍要发包」的时序由 `Transmit` 直驱模拟。
#[tokio::test]
async fn removed_leg_does_not_fall_back_to_direct_socket() {
    use crate::exit::socket::{ExitSock, LegTable};
    use quinn::udp::Transmit;
    use quinn::AsyncUdpSocket as _;

    let stats = Arc::new(ExitStats::default());
    let (wake_tx, wake_rx) = std::os::unix::net::UnixStream::pair().expect("self-pipe 可建");
    wake_tx.set_nonblocking(true).unwrap();
    wake_rx.set_nonblocking(true).unwrap();
    let (out_tx, _out_rx) = tokio::sync::mpsc::channel(1);
    let bridge = Arc::new(ExitBridge::new(
        Arc::clone(&stats),
        Arc::new(|_: &str| {}),
        wake_tx,
        wake_rx,
        out_tx,
    ));
    // 直连面：本 socket 的「直连端口」+ 观察者（收它发出的裸包）
    let observer = loopback_socket();
    observer.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let observer_addr = observer.local_addr().unwrap();
    let direct = loopback_socket();
    let direct_addr = direct.local_addr().unwrap();
    direct.set_nonblocking(true).expect("from_std 前置");
    // 腿面：R = 中继数据口；腿 socket connect 到 R
    let relay = loopback_socket();
    relay.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    let relay_addr = relay.local_addr().unwrap();
    let leg = loopback_socket();
    leg.connect(relay_addr).expect("腿 socket connect");

    let legs = Arc::new(LegTable::default());
    legs.open(relay_addr, leg.try_clone().expect("腿 socket 可克隆"));
    let (_inject_tx, inject_rx) = tokio::sync::mpsc::channel(4);
    let sock = ExitSock::new(
        tokio::net::UdpSocket::from_std(direct).expect("进 runtime 面"),
        direct_addr,
        Arc::clone(&legs),
        inject_rx,
        Arc::clone(&bridge),
    );
    let tx = |dest: SocketAddr, body: &'static [u8]| Transmit {
        destination: dest,
        ecn: None,
        contents: body,
        segment_size: None,
        src_ip: None,
    };
    // 直连面首发可能撞 tokio 的「就绪缓存未起」WouldBlock（产品路径由 quinn 的
    // io_poller 等待重试）——测试按同一形态重试。
    async fn send_ready(sock: &ExitSock, t: &Transmit<'_>) {
        for _ in 0..200 {
            match sock.try_send(t) {
                Ok(()) => return,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                Err(e) => panic!("发送失败：{e}"),
            }
        }
        panic!("重试预算内仍 WouldBlock");
    }
    let mut buf = [0u8; 64];

    // ① 腿命中 ⇒ 包 [0xBB][5] 走腿 socket（R 收到），观察者零收
    send_ready(&sock, &tx(relay_addr, b"quic-pkt")).await;
    let (n, _src) = relay.recv_from(&mut buf).expect("腿 socket 应收到");
    assert_eq!(&buf[..n], b"\xBB\x05quic-pkt", "腿帧 = [0xBB][5]‖QUIC 报文");
    assert!(observer.recv_from(&mut buf).is_err(), "腿命中不得回落直连");

    // ② 非腿目的地 ⇒ 直连端口原样发（裸 QUIC 包，无壳）
    send_ready(&sock, &tx(observer_addr, b"direct-pkt")).await;
    let (n, _src) = observer.recv_from(&mut buf).expect("直连应收到");
    assert_eq!(&buf[..n], b"direct-pkt", "直连路径不加壳");

    // ③ 摘腿 ⇒ 丢 + 计数，**不**回落直连（#17）
    legs.close(relay_addr);
    assert!(!legs.has(&relay_addr), "腿已摘");
    let before = stats.snapshot().drop_unregistered;
    send_ready(&sock, &tx(relay_addr, b"stale-leg")).await;
    assert_eq!(
        stats.snapshot().drop_unregistered,
        before + 1,
        "摘腿后的发送必须计 `未登记`（丢 + 计数，不静默）"
    );
    assert!(observer.recv_from(&mut buf).is_err(), "摘腿后不得回落直连");
    assert!(relay.recv_from(&mut buf).is_err(), "摘腿后不得到达腿 socket");

    // ④ 同远端重登记（中继 assoc 重建）：撤「最近摘除」标记，腿路径恢复
    legs.open(relay_addr, leg.try_clone().expect("腿 socket 可克隆"));
    send_ready(&sock, &tx(relay_addr, b"again")).await;
    let (n, _src) = relay.recv_from(&mut buf).expect("重登记后腿路径应恢复");
    assert_eq!(&buf[..n], b"\xBB\x05again");
}
