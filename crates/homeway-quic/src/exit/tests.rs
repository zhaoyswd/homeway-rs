//! 出口 QUIC 面单测（**真线程 + 真回环 IO**；不钉固定端口、时间断言只判上界——
//! M0 设计 §9.2 flake 口径①②）。
//!
//! 断言面：E-q1 就绪行（实际端口/MTU/缓冲）、收工预算与幂等、端口释放；
//! 「对 pin/错 pin 握手」与资源上限用例随 S1a 的后续工作单元进本文件。

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cmd::Logf;
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::{ExitQuic, ExitQuicConfig};

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
