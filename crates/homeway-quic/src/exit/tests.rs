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
use crate::{
    EngineRejectClass, ExitInbound, ExitQuic, ExitQuicConfig, Reg4Verdict, RejectWhy,
    RetryPolicy,
};

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

/// 有界收行（**异步轮询版**：本测试的客户端 quinn 端点驱动跑在同一枚 `current_thread`
/// runtime 上——用阻塞版 `drain_until` 会把驱动一起冻住，「客户端刚写的字节还没上线」的
/// 假红就出来了）。
async fn drain_until_async(rx: &Receiver<String>, needle: &str, wait: Duration) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        while let Ok(line) = rx.try_recv() {
            let hit = line.contains(needle);
            lines.push(line);
            if hit {
                return lines;
            }
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
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

    let lines = drain_until(&rx, "端点就绪", WAIT);
    let line = lines.last().expect("就绪行在").clone();
    assert!(line.contains(&want_port.to_string()), "行含实际端口：{line}");
    assert!(line.contains("migration=true"), "行含 migration：{line}");
    assert!(line.contains("initial_mtu=1400"), "行含 initial_mtu：{line}");
    assert!(
        line.contains("datagram 缓冲 1048576B"),
        "行含 datagram 缓冲（1 MiB）：{line}"
    );
    // E-q6（M5 S4 新增行）：单承载语境的就绪行（面就绪 + 两个关键参数）
    let eq6 = lines
        .iter()
        .find(|l| l.contains("出口 QUIC 面就绪（单承载；"))
        .unwrap_or_else(|| panic!("E-q6 行须在场：{lines:?}"));
    assert!(eq6.contains("migration=true"), "E-q6 含 migration：{eq6}");
    assert!(eq6.contains("initial_mtu=1400"), "E-q6 含 initial_mtu：{eq6}");
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
    // 归因位（M2 §14-1④）：客户端因 pin 不符**主动关**（TLS alert 的 CONNECTION_CLOSE）
    // ⇒ 落 `handshake_peer_closed`（与「对端静默」可区分；计数集不放宽）。
    assert!(
        wait_until(|| quic.snapshot().handshake_peer_closed >= 1, WAIT).await,
        "对端主动关闭应进归因位：{:?}",
        quic.snapshot()
    );
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
    drain_until(&rx, "拒新连接（连接总数", WAIT); // 记行面（节流首条必打）
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
    drain_until(&rx, "拒新连接（并发握手", WAIT);
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
    drain_until(&rx, "握手期限", WAIT);
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
    /// Proof 的裁决改成**引擎裁决拒绝**（S3-3 的 r14 F12 用例：表满/冲突/吊销/窗超这类
    /// 可用性故障**不得**计入证明失败闸）。
    engine_reject: AtomicBool,
    /// 引擎裁决拒绝的**类**（M3 §4：准入关闭码分桶——凭证桶 `0x11` / 资源桶 `0x12`）。
    engine_class: Mutex<EngineRejectClass>,
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
            engine_reject: AtomicBool::new(false),
            engine_class: Mutex::new(EngineRejectClass::Credential),
            packets: Mutex::new(Vec::new()),
        })
    }

    /// 注入「引擎判 = 刷新帧但设备不在册（已淘汰）」（S2-2 用例专用）。
    fn evict_refresh(&self) {
        self.evict_refresh.store(true, Ordering::SeqCst);
    }

    /// 注入「引擎裁决拒绝」（S3-3 的 r14 F12 用例专用；MAC 校验被跳过 ⇒ 只判裁决形态）。
    /// 引擎裁决拒绝的注入（`class` = M3 §4 的桶；缺省凭证桶）。
    fn engine_reject_with(&self, on: bool, class: EngineRejectClass) {
        self.engine_reject.store(on, Ordering::SeqCst);
        *self.engine_class.lock().unwrap() = class;
    }

    fn engine_reject(&self, on: bool) {
        self.engine_reject.store(on, Ordering::SeqCst);
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
                let engine_rejected =
                    !is_refresh && self.engine_reject.load(Ordering::SeqCst);
                if evicted {
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg4Verdict::Rejected {
                        why: RejectWhy::RefreshNotRegistered,
                    });
                } else if engine_rejected {
                    // 引擎裁决拒绝（表满/冲突/吊销/窗超的等价注入）：r14 F12 的负例面
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    let class = *self.engine_class.lock().unwrap();
                    req.reply(Reg4Verdict::Rejected {
                        why: RejectWhy::EngineRejected { class },
                    });
                } else if req.frame.mac_matches(&self.secret, &req.exporter) {
                    self.accepted.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg4Verdict::Accepted { tunnel_ip: TUNNEL_IP, tun_ip: TUN_IP });
                } else {
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg4Verdict::Rejected {
                        why: RejectWhy::MacMismatch,
                    });
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=44444444 tun=100.64.7.2", WAIT).await;
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=66666666", WAIT).await;

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
    drain_until(&rx, "认证超时", WAIT);
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
    drain_until(&rx, "认证超时", WAIT);

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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=d2d2d2d2", WAIT).await;

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

// ---------- 代码门 r15 整改（G2/G17）：准入漏斗的 fail-visible 拒绝串定点用例 ----------
//
// 背景（`docs/reviews/M2.md` §2 的 G2/G17）：准入漏斗的 7 条 `why` 串里有 6 条只有实现、
// 没有断言；判据行「准入挑战已发（…；在途未认证 n/cap；第 n 次）」的**数值**也从未被校验。
// 这些串是设计 §13-4 点名的「fail-visible 拆细」产物 ⇒ 各补一条定点用例（照
// `legacy_h3_frame_is_rejected_with_distinct_reason` 的模板：构帧 → 断言归因串 + 分档计数
// + 关连接）。**不在本组**的两条：`证明失败闸冷却中`（见下方冷却用例）、`TLS exporter 不可得`
// （真连接上恒可得，构造不出 ⇒ 登记为不可测，见 `M2.md`）。

/// **判据（G2）**：首帧不是 Hello（这里是 `P4`）⇒ 拒（`帧格式非法（首帧必须是 Hello）`）
/// + 归到「未发挑战」段 + 不投引擎 + 关连接。
#[tokio::test]
async fn first_frame_must_be_hello() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(71), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = conn.open_bi().await.expect("open_bi");
    // 首帧直送 Proof（语法合法但**位置非法**）：帧头判别先于任何状态
    let frame = ProofFrame::encode(
        &SECRET,
        &[0x11u8; 32],
        &[0x12u8; 8],
        TS,
        &Nonce::from_bytes([0x13u8; 16]),
        &exporter_of(&conn),
    );
    send.write_all(&frame).await.expect("写首帧 Proof");

    pump_until_line(&stub, &quic, &rx, "帧格式非法（首帧必须是 Hello）", WAIT).await;
    assert_eq!(stub.proofs(), 0, "首帧非法不得投引擎");
    assert_eq!(quic.snapshot().proof_rejected, 0, "不归 Proof 段");
    assert_eq!(quic.snapshot().challenges_refused, 1, "归「未发挑战」段");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "首帧非法必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-8 的 `P4` 臂 / G17）**：已绑定连接再发 `H4` **或 `P4`** ⇒ 一律拒
/// （`已绑定连接的再准入`）+ 关连接（防止 `bind()` 把同一连接改指到另一身份）。
#[tokio::test]
async fn bound_connection_rejects_proof_re_admission() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(72), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xC1u8; 32];
    let dev = [0xC2u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    // 用 `four_frames` 保留本连接那枚 nonce（`admit` 会丢掉它）
    let (mut send, _recv, nonce) = four_frames(&stub, &quic, &conn, &SECRET, &pubkey, &dev)
        .await
        .expect("四帧应走通");
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=c2c2c2c2", WAIT).await;

    // 另一台「设备」的 Proof（nonce 是**本连接**的，MAC 也合法）⇒ 仍必须被拒
    let frame = ProofFrame::encode(
        &SECRET,
        &[0xE1u8; 32],
        &[0xE2u8; 8],
        TS,
        &nonce,
        &exporter_of(&conn),
    );
    send.write_all(&frame).await.expect("写已绑定连接的第二条 Proof");
    pump_until_line(&stub, &quic, &rx, "已绑定连接的再准入", WAIT).await;
    assert_eq!(stub.proofs(), 1, "只应有准入那一条 Proof 投到引擎");
    assert_eq!(quic.snapshot().challenges_issued, 1, "不得再发挑战");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "再准入必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（G2）**：已绑定连接上收到**既非刷新帧**的客户端帧（这里 `C4`）⇒ 拒
/// （`帧格式非法（已绑定连接只收刷新帧）`）+ 关连接。
#[tokio::test]
async fn bound_connection_rejects_non_refresh_frame() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(73), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xC3u8; 32];
    let dev = [0xC4u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=c4c4c4c4", WAIT).await;

    // `C4` 是出口→客户端的帧头；客户端发来即「不该出现的帧」
    send.write_all(&ChallengeFrame::encode(&Nonce::from_bytes([0x5Au8; 16])))
        .await
        .expect("写 C4 帧");
    pump_until_line(&stub, &quic, &rx, "帧格式非法（已绑定连接只收刷新帧）", WAIT).await;
    assert_eq!(stub.refreshes(), 0, "非法帧不得投引擎（不是刷新）");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "非法帧必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（G2 · 撤销/轮换的最小对齐面）**：绑定被摘（设备淘汰/轮换/替换 ⇒ `unbind_pub`）
/// ⇒ **连接被立刻拆**（`拆连接（dev=…）` 行 + 对端观察 `close_reason`），后续帧不再投引擎。
///
/// **形态说明（代码门 r15 整改的如实记录）**：`refresh_loop` 里那条「连接未绑定（绑定已摘）」
/// 归因串是**防御位**——产品面上所有摘绑定的路径（`unbind_pub` 摘绑定 **并** 关连接；
/// `bind()` 替换旧连接时同样关旧连接）都先关连接，控制流随即退出 ⇒ 该串构造不出（本用例
/// 一度试图构造，实测只观察到「拆连接」行 ⇒ 改为断言**真实防线**：摘绑定必拆连接）。
#[tokio::test]
async fn binding_removed_midflight_closes_connection() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(74), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xC5u8; 32];
    let dev = [0xC6u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=c6c6c6c6", WAIT).await;

    quic.unbind_pub(&pubkey); // 摘绑定（引擎侧 Remove 的出口面动作）
    pump_until_line(&stub, &quic, &rx, "拆连接（dev=c6c6c6c6", WAIT).await;
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "摘绑定必须拆连接（被淘汰设备不得继续用旧连接）"
    );
    // 摘除后该连接的帧不再投引擎（控制流任务已随连接退出）
    let before = stub.refreshes();
    let frame = RefreshFrame::encode(&SECRET, &pubkey, &dev, TS, &exporter_of(&conn));
    let _ = send.write_all(&frame).await; // 连接已关 ⇒ 写失败是预期，不 panic
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(stub.refreshes(), before, "摘绑定后刷新帧不得投引擎");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 客户端侧观察到的 `CONNECTION_CLOSE` 应用码（`None` = 还没关/非应用关闭）。
fn peer_close_code(conn: &quinn::Connection) -> Option<u64> {
    match conn.close_reason() {
        Some(quinn::ConnectionError::ApplicationClosed(ac)) => Some(ac.error_code.into_inner()),
        _ => None,
    }
}

/// **判据（M3 S5 / §4：准入失败归因回传）**：准入窗内的拒绝把「为什么」带进
/// `CONNECTION_CLOSE` 的**应用码**（此前恒 `0` ⇒ 客户端三种拒绝一律 `登记失败`）：
///
/// | 触发 | 桶 | 码 |
/// |---|---|---|
/// | MAC 不符（`RejectWhy::MacMismatch`） | 凭证 | `0x11` |
/// | 引擎裁决拒绝·凭证面（`no-token`/`revoked`） | 凭证 | `0x11` |
/// | 引擎裁决拒绝·资源面（`table-full`/表压） | 资源 | `0x12` |
/// | 帧格式/版本族（未知魔数） | 数据 | `0x13` |
#[tokio::test]
async fn admission_close_codes_are_bucketed() {
    // ① MAC 不符 ⇒ 0x11（凭证桶）
    {
        let (logf, _rx) = sink();
        let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(76), 32), logf)
            .expect("端点可起");
        let stub = Stub::new(SECRET);
        let (_c, conn, _s) = client_conn(&quic).await;
        let _ = four_frames(&stub, &quic, &conn, &[0x99u8; 32], &[0x77u8; 32], &[0x88u8; 8]).await;
        assert!(
            pump_until(&stub, &quic, || peer_close_code(&conn).is_some(), WAIT).await,
            "被拒连接必须关"
        );
        assert_eq!(peer_close_code(&conn), Some(0x11), "MAC 不符 ⇒ 凭证桶");
        assert!(quic.stop_within(Instant::now() + BUDGET));
    }
    // ②③ 引擎裁决拒绝的两桶（同 MAC 合法，仅裁决结果不同）
    for (class, want) in [
        (EngineRejectClass::Credential, 0x11u64),
        (EngineRejectClass::Resource, 0x12u64),
    ] {
        let (logf, _rx) = sink();
        let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(77), 32), logf)
            .expect("端点可起");
        let stub = Stub::new(SECRET);
        stub.engine_reject_with(true, class);
        let (_c, conn, _s) = client_conn(&quic).await;
        let _ = four_frames(&stub, &quic, &conn, &SECRET, &[0x78u8; 32], &[0x79u8; 8]).await;
        assert!(
            pump_until(&stub, &quic, || peer_close_code(&conn).is_some(), WAIT).await,
            "被拒连接必须关（{class:?}）"
        );
        assert_eq!(peer_close_code(&conn), Some(want), "{class:?} 的桶");
        assert!(quic.stop_within(Instant::now() + BUDGET));
    }
    // ④ 帧格式非法（未知魔数，首帧不是 Hello）⇒ 0x13（数据桶）
    {
        let (logf, _rx) = sink();
        let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(78), 32), logf)
            .expect("端点可起");
        let stub = Stub::new(SECRET);
        let (_c, conn, _s) = client_conn(&quic).await;
        let (mut send, _recv) = conn.open_bi().await.expect("open_bi");
        let _ = send.write_all(b"XXBADFIRSTFRAME").await;
        assert!(
            pump_until(&stub, &quic, || peer_close_code(&conn).is_some(), WAIT).await,
            "非法帧必须关"
        );
        assert_eq!(peer_close_code(&conn), Some(0x13), "帧格式 ⇒ 数据桶");
        assert!(quic.stop_within(Instant::now() + BUDGET));
    }
}

/// **判据（M3 S5 / §4 设计门 F4 的负例）**：**准入后**的会话级关闭（设备被摘除 ⇒
/// `unbind_pub` 的 `device removed`）**不得**带准入码——客户端据此走 `SessionClosed`
/// 分支，不会把「会话中被吊销」误报成「准入被拒」。
#[tokio::test]
async fn post_admission_close_carries_no_admission_code() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(79), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xCBu8; 32];
    let dev = [0xCCu8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    let (_send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=cccccccc", WAIT).await;

    quic.unbind_pub(&pubkey); // 设备摘除（会话级事件）
    assert!(
        pump_until(&stub, &quic, || peer_close_code(&conn).is_some(), WAIT).await,
        "摘绑定必须拆连接"
    );
    let code = peer_close_code(&conn).expect("对端应用关闭");
    assert_eq!(code, 0, "会话级关闭不得带准入码（码域 {:#x}–{:#x} 是准入面）", crate::admit_close::code::CREDENTIAL, crate::admit_close::code::TIMEOUT);
    assert!(
        !crate::admit_close::is_admission_code(code),
        "客户端必须把它判成会话级（非准入面）"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（G2）**：刷新帧的**帧内身份**与绑定不符（同 MAC 合法）⇒ 拒
/// （`刷新帧与绑定身份不符`）+ 关连接（本地双检查的第二道，零设备表查询）。
#[tokio::test]
async fn refresh_identity_mismatch_is_rejected() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(75), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let pubkey = [0xC7u8; 32];
    let dev = [0xC8u8; 8];
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=c8c8c8c8", WAIT).await;

    // 帧内换成另一个 dev/pub（MAC 用同一 secret 照算 ⇒ 域标签/MAC 都合法）
    send_refresh(&conn, &mut send, &SECRET, &[0xC9u8; 32], &[0xCAu8; 8]).await;
    pump_until_line(&stub, &quic, &rx, "刷新帧与绑定身份不符", WAIT).await;
    assert_eq!(stub.refreshes(), 0, "身份不符不得投引擎");
    assert!(
        pump_until(&stub, &quic, || conn.close_reason().is_some(), WAIT).await,
        "身份不符必须关连接"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（G2 · §1.6 的挑战行）**：「准入挑战已发」行的**数值**同源——`在途未认证` 的
/// 分子 = 存活连接 − 已绑定设备（1/64），分母 = `conn_cap`（2×`max_devices`）。
#[tokio::test]
async fn challenge_line_reports_inflight_and_cap() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(76), 32), logf)
        .expect("端点可起");
    let _stub = Stub::new(SECRET); // 本用例只读日志行与快照（不 pump 引擎）
    let (_c, conn, _s) = client_conn(&quic).await;
    let (_send, _recv, _nonce) = hello_and_challenge(&conn, &[0x31u8; 32], &[0x32u8; 8]).await;

    let line = drain_until(&rx, "准入挑战已发", WAIT);
    let line = line
        .iter()
        .find(|l| l.contains("准入挑战已发"))
        .unwrap_or_else(|| panic!("应有挑战行：{line:?}"));
    assert!(
        line.contains("在途未认证 1/64"),
        "分子 = 存活连接 − 已绑定（1）、分母 = conn_cap（64）：{line}"
    );
    assert!(line.contains("第 1 次"), "节流序号从 1 起：{line}");
    assert_eq!(quic.snapshot().challenges_issued, 1, "行与快照同源");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（G2 · §5-11 的代价告警）**：`retry_policy=always` ⇒ 启动期打出
/// 「常态 +1 RTT」告警行；缺省（pressure）⇒ **不打**（告警不得常态噪声）。
#[tokio::test]
async fn retry_policy_always_warns_about_extra_rtt() {
    let (_quic, rx) = exit_face_logged(77, |c| c.retry_policy = RetryPolicy::Always);
    let lines = collect_for(&rx, Duration::from_millis(300));
    assert!(
        lines.iter().any(|l| l.contains("retry_policy=always")),
        "always 档必须打代价告警行：{lines:?}"
    );
    let (_q2, rx2) = exit_face_logged(78, |_c| {});
    let lines2 = collect_for(&rx2, Duration::from_millis(300));
    assert!(
        !lines2.iter().any(|l| l.contains("retry_policy=always")),
        "缺省（pressure）不得打该告警：{lines2:?}"
    );
}

/// **判据（G2 · §3.2-⑥）**：冷却期内拒 Hello 的归因串**可辨**
/// （`证明失败闸冷却中…`）——与「证明失败闸（dev=…）」那条计数行互补（后者已被
/// `proof_fail_gate_cools_bad_dev_and_spares_engine_rejected` 覆盖）。
#[tokio::test]
async fn cooling_reject_why_is_visible_in_log() {
    let (quic, rx) = exit_face_logged(79, |c| {
        c.proof_fail_threshold = 1; // 阈值 1：一次 MAC 不符即进冷却
        c.per_src_window = Duration::from_secs(60);
    });
    let stub = Stub::new(SECRET);
    let pubkey = [0x41u8; 32];
    let dev = [0x42u8; 8];
    // 一次 MAC 不符（错 secret）⇒ 进冷却
    let (_c, conn, _s) = client_conn(&quic).await;
    let got = four_frames(&stub, &quic, &conn, &[0x00u8; 32], &pubkey, &dev).await;
    assert!(got.is_err(), "错 secret 必须被拒");
    drain_until(&rx, "证明失败闸", WAIT);

    // 冷却中：同 devTag 的 Hello ⇒ 拒（可辨归因串）
    let (_c2, conn2, _s2) = client_conn(&quic).await;
    let (mut send, _recv) = conn2.open_bi().await.expect("open_bi");
    send.write_all(&HelloFrame::encode(&pubkey, &dev, TS))
        .await
        .expect("写 Hello");
    pump_until_line(&stub, &quic, &rx, "证明失败闸冷却中", WAIT).await;
    assert_eq!(quic.snapshot().challenges_issued, 1, "冷却期不得再发挑战");
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=f2f2f2f2", WAIT).await;
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
        lines.iter().filter(|l| l.contains("连接采纳")).count(),
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=cccccccc", WAIT).await;

    let (_cb, conn_b, _sb) = client_conn(&quic).await;
    let (_send_b, _recv_b) = admit(&stub, &quic, &conn_b, &SECRET, &pubkey, &dev).await;
    pump_until_line(&stub, &quic, &rx, "替换旧连接（dev=cccccccc", WAIT).await;
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=21212121", WAIT).await;
    quic.bridge.assert_bindings_consistent();
    let (_cb, conn_b, _sb) = client_conn(&quic).await;
    let (_sb_ctl, _rb) = admit(&stub, &quic, &conn_b, &SECRET, &pub_b, &dev_b).await;
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=22222222", WAIT).await;
    quic.bridge.assert_bindings_consistent();

    // ② 同 devTag 换公钥（轮换）：后到者替换 ⇒ 旧连接关、三索引仍互指
    let pub_a2 = [0x13u8; 32];
    let (_cc, conn_c, _sc) = client_conn(&quic).await;
    let (_sc_ctl, _rc) = admit(&stub, &quic, &conn_c, &SECRET, &pub_a2, &dev_a).await;
    pump_until_line(&stub, &quic, &rx, "替换旧连接（dev=21212121", WAIT).await;
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
        "未认证连接不得产生绑定（出站报 Unbound ⇒ 丢 + 计数）"
    );
    drain_until(&rx, "丢弃 超限=0 发送缓冲满=0 未登记=1 源校验拒=0", WAIT); // E-q3 行
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=72727272", WAIT).await;
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

/// **判据（S1-4 + M3 §6 收窄）**：源非法包（src ∉ **{tun_ip}**）⇒ 丢 + `源校验拒` 计数 +
/// E-q3 行；同连接随后的合法包照常投递（拒的是包不是连接）。明细行含**实际 src**（设计
/// §9.1-1 的增强：M1 真机发现①的「无法一眼定性」在这里关闭）。
///
/// **M3 §6 的收窄负例**：`tunnel_ip`（`hw-tun` 派生 = 栈 B 的地址）在 QUIC 档**不再被接受**
/// ——它在 QUIC 档没有任何合法来源（栈 B 数据面不在 QUIC 档），保留它只扩大可伪造面。
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=aaaaaaaa", WAIT).await;

    // 源非法（9.9.9.9 不在 {tun_ip}）
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
    let ls = drain_until(&rx, "丢弃 超限=0 发送缓冲满=0 未登记=0 源校验拒=1", WAIT);
    assert!(
        ls.iter().any(|l| l.contains("src=9.9.9.9")),
        "E-q3 行的明细必须含实际 src（否则真机上仍无法定性）：{ls:?}"
    );
    // **M3 §6 判据（单元素接受集）**：明细打印收窄后的集合，不再出现 `tunnel_ip=`
    assert!(
        ls.iter().any(|l| l.contains("∉ {tun_ip=") && !l.contains("tunnel_ip=")),
        "E-q3 明细须打印收窄后的单元素集：{ls:?}"
    );
    // 收窄负例：`tunnel_ip` 源（M3 §6 起不在接受集内）必须被拒、不得进引擎面
    conn.send_datagram(bytes::Bytes::from(inner_pkt(
        TUNNEL_IP,
        Ipv4Addr::new(8, 8, 8, 8),
    )))
    .expect("发数据报");
    assert!(
        pump_until(&stub, &quic, || quic.snapshot().drop_src_rejected >= 2, WAIT).await,
        "tunnel_ip 源必须被拒（§6 收窄）：{:?}",
        quic.snapshot()
    );
    assert!(stub.packets().is_empty(), "tunnel_ip 源不得进引擎面");

    // 合法（tun_ip 源）照常投递
    let good = inner_pkt(TUN_IP, Ipv4Addr::new(8, 8, 8, 8));
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
    drain_until(&rx, "丢弃 超限=0 发送缓冲满=", WAIT); // E-q3 行（首 3 必打）
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 无绑定（含出口面已死）⇒ `Unbound`（调用方丢 + 计数，不黑洞）。
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
        "面已死也必须报 Unbound（调用方丢 + 计数，不黑洞）"
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
        intakes: Arc::new(crate::ServiceIntakes::default()),
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
    pump_until_line(&stub, &quic, &rx, "连接采纳 dev=d2d2d2d2", WAIT).await;
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
        None, // 明文面钩子（M5 S3a）：本用例只测 QUIC 路径
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

// ---------- S3（M2 §3.1–§3.3）：抗放大闸 + Retry ----------

/// 建出口面（S3 用例共用：可改配置；日志丢弃）。
fn exit_face_with(seed: u8, f: impl FnOnce(&mut ExitQuicConfig)) -> ExitQuic {
    let (logf, _rx) = sink();
    let mut cfg = ExitQuicConfig::new(seed_of(seed), 32);
    f(&mut cfg);
    ExitQuic::start(loopback_socket(), cfg, logf).expect("出口 QUIC 面可起")
}

/// 建出口面 + 收回其日志行（S3 的「行与快照同源」断言用）。
fn exit_face_logged(seed: u8, f: impl FnOnce(&mut ExitQuicConfig)) -> (ExitQuic, Receiver<String>) {
    let (logf, rx) = sink();
    let mut cfg = ExitQuicConfig::new(seed_of(seed), 32);
    f(&mut cfg);
    (ExitQuic::start(loopback_socket(), cfg, logf).expect("出口 QUIC 面可起"), rx)
}

fn seed_of(byte: u8) -> Ed25519Seed {
    Ed25519Seed::from_bytes([byte; 32])
}

/// 一次「同源未完成」的建连尝试：**错 RPK** ⇒ 客户端握手中止（TLS alert）⇒ 出口侧
/// 该次尝试永远不完成（正是每源闸要计的形态）。
async fn wrong_pin_attempt(exit: SocketAddr) -> bool {
    let (client, cfg, _sock) = client_endpoint_and_config(RpkPublicKey::from_bytes([0xEE; 32]));
    let done = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, exit, super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .is_ok_and(|r| r.is_ok());
    client.close(quinn::VarInt::from_u32(0), b"test done");
    done
}

/// **判据（S3-1 / §3.3-1）**：同源短时大量「未完成」尝试 ⇒ 第 `F+1` 次起全拒、
/// `flood_refused = K − F`（确定性注入，不靠真打流量）。
#[tokio::test]
async fn flood_same_source_is_refused_and_counted() {
    let (quic, rx) = exit_face_logged(61, |c| {
        c.per_src_fails = 3;
        c.per_src_window = Duration::from_secs(60);
        c.retry_policy = RetryPolicy::Never; // 本条只测闸（Retry 由下一用例测）
    });
    let addr = quic.local_addr();
    const K: u64 = 8;
    for i in 0..K {
        if i < 3 {
            let ok = wrong_pin_attempt(addr).await;
            assert!(!ok, "错 pin 不得连上");
        } else {
            wrong_pin_attempt(addr).await; // 闸拒（客户端拿到 CONNECTION_REFUSED）
        }
    }
    // 有界等待最后一次尝试的**记账落地**（flake 口径②：只判上界）：K 次尝试是异步收尾的，
    // 立即读快照会丢最后 1 条的归账（ubuntu CI 实测 4 vs 5）。
    assert!(
        wait_until(|| quic.snapshot().flood_refused >= K - 3, WAIT).await,
        "第 F+1..K 次全拒（K−F = 5）须有界到账：{:?}",
        quic.snapshot()
    );
    let snap = quic.snapshot();
    assert_eq!(
        snap.flood_refused,
        K - 3,
        "第 F+1..K 次全拒（K−F = 5）：{snap:?}"
    );
    assert_eq!(snap.retry_sent, 0, "never 档恒不 Retry");
    assert_eq!(snap.admitted, 0, "无一次完成（全是未完成尝试）");
    // 行与快照同源（§3.3-5）：洪泛拒绝行在场；节流口径 = 首 3 + 每 100 ⇒ 本次 K−F=5 条
    // 拒绝里只有前 3 条落行，且首条的「窗内第 k 次」= F+1 = 4（第 4 次尝试起全拒）。
    let lines = drain_until(&rx, "握手洪泛拒绝", WAIT);
    let more = collect_for(&rx, Duration::from_millis(300)); // 收尾：K−F 条拒绝已全在队列里
    let flood_lines: Vec<&String> = lines
        .iter()
        .chain(more.iter())
        .filter(|l| l.contains("握手洪泛拒绝"))
        .collect();
    assert_eq!(flood_lines.len(), 3, "节流口径 = 首 3 + 每 100（5 条拒绝只落 3 条）：{flood_lines:?}");
    assert!(
        flood_lines[0].contains("第 4 次尝试"),
        "首条拒绝 = 窗内第 F+1 次：{}",
        flood_lines[0]
    );
    assert!(flood_lines[0].contains("已拒"), "行文：{}", flood_lines[0]);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S3-2 / §3.1 压力触发）**：同源「未完成/被拒」≥ `RETRY_AFTER_FAILS` ⇒ 下一次
/// 建连收到 Retry（`retry_sent` + 行），且**正常客户端仍能完成握手**（按 token 重发
/// Initial ⇒ 地址被验证，§0.3 R1 形态）。
#[tokio::test]
async fn pressure_arm_sends_retry_and_client_still_completes() {
    let (quic, rx) = exit_face_logged(62, |c| c.per_src_window = Duration::from_secs(60));
    let pin = quic.rpk_public_key();
    let addr = quic.local_addr();
    // 攒「同源未完成」到触发条件②（`RETRY_AFTER_FAILS = 5`）：5 次错 pin 尝试都留下窗内
    // 记录（= 未完成）；读数为 0 说明触发条件②在**下一条**建连前尚未命中。
    for _ in 0..5 {
        assert!(!wrong_pin_attempt(addr).await, "错 pin 尝试不得连上");
    }
    // 下一条（正常 pin）：同源未完成 ≥ 5 ⇒ Retry；客户端按 token 重发 ⇒ 仍能完成
    let (client, cfg, _sock) = client_endpoint_and_config(pin);
    let conn = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, addr, super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("预算内定音")
    .expect("Retry 之后客户端必须仍能完成握手（否则常态重连被打断）");
    assert!(
        wait_until(|| quic.snapshot().retry_sent >= 1, WAIT).await,
        "应有 Retry：{:?}",
        quic.snapshot()
    );
    let lines = drain_until(&rx, "地址校验挑战", WAIT);
    let line = lines.last().expect("地址校验挑战行在");
    assert!(line.contains("在途未认证"), "行文：{line}");
    assert!(
        wait_until(|| quic.snapshot().admitted >= 1, WAIT).await,
        "Retry 后的连接应被采纳：{:?}",
        quic.snapshot()
    );
    drop(conn);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S3-2 / r14 F10 + §3.3-6；**措辞按 M2 §14-1③ 改写**）**：≥3 候选的正常赛跑连续 8 轮
/// ⇒ `retry_sent = 0` 且 `flood_refused = 0`。
///
/// ⚠️ **形态口径（实测登记 + S4 裁决留痕）**：本用例走的是「候选**完成**握手后被收掉」的形态
/// （回环上三候选都在同一 RTT 内完成 ⇒ 每条都销账）。**客户端在候选完成前 abort 输家**的形态
/// （`race::run` 的 `abort_all` 语义）在出口侧落 `HandshakeOutcome::Failed` ⇒ 计入每源闸
/// （实测：真岛 3 候选，第 2 轮起 `retry_sent` 增长、第 4 轮 `flood_refused=5` ⇒ `NoCandidate`）。
/// **M2 §14-1 裁定**：①计数集**不放宽**（abort 同样可被攻击者使用）；②每源阈值缺省
/// `10 → 16`（允许约 5 次 3 候选赛跑/窗）；③判据措辞改为「**正常赛跑在阈值内 ⇒ 不触发闸**」
/// ——该 abort 形态的正面证据 = 下一条用例（`race_abort_form_stays_within_gate_budget`）。
#[tokio::test]
async fn normal_races_do_not_trigger_retry_or_gate() {
    let quic = exit_face_with(63, |c| c.per_src_window = Duration::from_secs(60));
    let pin = quic.rpk_public_key();
    let addr = quic.local_addr();
    // 同一源（同 /32、同一 socket）+ 3 个并行候选到同一出口 —— 设计点名的「同源多候选」
    // 最严形态（§3.1-② 的计数集若不排除「完成的尝试」，这里会攒满 ⇒ Retry）。
    let (client, cfg, _sock) = client_endpoint_and_config(pin);
    const ROUNDS: u64 = 8;
    for round in 0..ROUNDS {
        let a = client.connect_with(cfg.clone(), addr, super::rpk::client_pin::SERVER_NAME).expect("connect 调用面");
        let b = client.connect_with(cfg.clone(), addr, super::rpk::client_pin::SERVER_NAME).expect("connect 调用面");
        let c = client.connect_with(cfg.clone(), addr, super::rpk::client_pin::SERVER_NAME).expect("connect 调用面");
        // 三个候选都走到「握手完成」（设计模型：完成 ⇒ 销账），随后全部收掉（胜者留用语义
        // 在下一轮由新连接承接）。
        let (ra, rb, rc) = tokio::time::timeout(WAIT, async { tokio::join!(a, b, c) })
            .await
            .unwrap_or_else(|_| panic!("第 {round} 轮：三候选都应在预算内定音"));
        for (i, r) in [ra, rb, rc].into_iter().enumerate() {
            assert!(
                r.is_ok(),
                "第 {round} 轮候选 {i} 未连上（{:?}）；快照 = {:?}",
                r.err(),
                quic.snapshot()
            );
        }
        assert!(
            wait_until(|| quic.snapshot().admitted > round, WAIT).await,
            "第 {round} 轮胜者应被采纳：{:?}",
            quic.snapshot()
        );
    }
    // 断言前先**有界等待所有握手落地**（flake 口径②：只判上界）：末轮候选的收尾是异步的，
    // 在途握手会被计入「未完成」并可能触发压力 Retry ⇒ 立即读快照会得到 retry_sent=1
    // 且 handshakes_in_flight=1（macOS CI 实测）。
    assert!(
        wait_until(|| quic.snapshot().handshakes_in_flight == 0, WAIT).await,
        "末轮候选须在预算内全部定音：{:?}",
        quic.snapshot()
    );
    let snap = quic.snapshot();
    assert_eq!(snap.retry_sent, 0, "常态赛跑不得 Retry（r14 F10）：{snap:?}");
    assert_eq!(snap.flood_refused, 0, "常态赛跑不得撞每源闸：{snap:?}");
    // 回环上三候选**都会完成握手**（实测 admitted = 24/24：胜者留用、余者被 explicit close）
    // ⇒ 每次完成都销账（否则 24 次尝试早把 per_src_fails=16 的窗口攒满 ⇒ 上面两条必红）。
    assert!(snap.admitted >= ROUNDS, "每轮至少一个胜者：{snap:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 「3 候选赛跑 × N 轮」的**abort 形态注入**（S4 阈值裁决的判据本体）：每轮 ① 一条对 pin 的
/// 候选**完成**（销账）、② 两条错 pin 候选 = **对端在握手完成前中止**（出口侧落
/// `HandshakeOutcome::Failed`，与真岛 `race::run` 的 abort 输家落同一条计数输入，且**绝不会**
/// 销账 —— 比真岛更悲观）。`retry_policy = never` 只关 Retry 轴（Retry 会给每条「未完成」
/// 再加一条重放尝试；放大面归既有的压力臂与 S5 真机标定），本注入只钉「每源闸预算够不够」。
async fn race_abort_form_rounds(quic: &ExitQuic, rounds: u64) -> SocketAddr {
    let pin = quic.rpk_public_key();
    let addr = quic.local_addr();
    for round in 0..rounds {
        let (client, cfg, _sock) = client_endpoint_and_config(pin);
        let conn = tokio::time::timeout(
            WAIT,
            client
                .connect_with(cfg, addr, super::rpk::client_pin::SERVER_NAME)
                .expect("connect 调用面"),
        )
        .await
        .expect("预算内定音")
        .expect("对 pin 的候选必须完成");
        assert!(
            wait_until(|| quic.snapshot().admitted > round, WAIT).await,
            "第 {round} 轮胜者应被采纳：{:?}",
            quic.snapshot()
        );
        for _ in 0..2 {
            assert!(!wrong_pin_attempt(addr).await, "错 pin 尝试不得连上");
        }
        drop(conn);
    }
    addr
}

/// **判据（S4 / M2 §14-1 裁决②的正面证据）**：「3 候选赛跑 × 5 轮」在**新缺省**
/// （`per_src_fails = 16`）下**仍在阈值内** ⇒ `flood_refused = 0`（每轮消耗 `N−1 = 2` 次
/// 预算 ⇒ 窗内 10 ≤ 16）。措辞按 §14-1③ = 「**正常赛跑在阈值内 ⇒ 不触发闸**」。
///
/// 形态说明见 [`race_abort_form_rounds`]。**读数（如实登记，非断言）**：本形态
/// `retry_sent = 0`（`retry_policy = never` 关掉了 Retry 轴）；真岛（pressure 档）同形态会因
/// 每条「未完成」再叠一条重放尝试而更快接近阈值 ⇒ 缺省 16 **待 S5 真机标定**（§14-1②）。
#[tokio::test]
async fn race_abort_form_stays_within_gate_budget() {
    let quic = exit_face_with(70, |c| {
        c.per_src_window = Duration::from_secs(60);
        c.retry_policy = RetryPolicy::Never;
    });
    let addr = race_abort_form_rounds(&quic, 5).await;
    let snap = quic.snapshot();
    assert_eq!(
        snap.flood_refused, 0,
        "3 候选 × 5 轮应在阈值内（缺省 16）：{snap:?}"
    );
    assert!(snap.admitted >= 5, "每轮至少一个胜者：{snap:?}");
    assert!(
        wait_until(|| quic.snapshot().handshake_failed >= 8, WAIT).await,
        "输家的「未完成」应留下握手失败读数（口径：对端主动中止 = Failed）：{:?}",
        quic.snapshot()
    );
    // 归因位（M2 §14-1④）：对端**主动关闭**（TLS alert ⇒ CONNECTION_CLOSE）落进
    // `handshake_peer_closed` —— 这是「出口**能**区分『主动关闭』与『静默』」的代码证据
    // （归因面；**不放宽**两个闸的输入集，§14-1①）。
    assert!(
        wait_until(|| quic.snapshot().handshake_peer_closed >= 8, WAIT).await,
        "错 pin 中止 = 对端主动关闭，应进归因位：{:?}",
        quic.snapshot()
    );
    // 阈值内的第 16 条尝试（= 第 5 轮后的下一条「未完成」）**仍放行**（窗内 10 < 16）。
    assert!(!wrong_pin_attempt(addr).await, "错 pin 尝试不得连上");
    let snap = quic.snapshot();
    assert_eq!(
        snap.flood_refused, 0,
        "新缺省下第 16 条尝试仍应在阈值内（窗内 10 < 16）：{snap:?}"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **对照臂（S4 / M2 §14-1 裁决②的承重证据）**：**旧缺省**（`per_src_fails = 10`）下的同一
/// 形态（3 候选 × 5 轮）把窗**刚好填满**（10/10）⇒ 紧随其后的下一条「未完成」尝试
/// （= 第 16 次尝试）**被拒**（`flood_refused` +1）。⇒ 缺省 10 → 16 的变更**是承重的**
/// （旧值对 abort 形态零余量；真岛的 retry 放大面在旧值下第 4 轮就撞闸——S3 实测
/// `flood_refused=5`）。
#[tokio::test]
async fn old_gate_budget_trips_on_next_attempt() {
    let quic = exit_face_with(71, |c| {
        c.per_src_fails = 10; // M2 §14-1 裁决前的缺省
        c.per_src_window = Duration::from_secs(60);
        c.retry_policy = RetryPolicy::Never;
    });
    let addr = race_abort_form_rounds(&quic, 5).await;
    let snap = quic.snapshot();
    assert_eq!(
        snap.flood_refused, 0,
        "5 轮刚好填满旧窗（10/10）但尚未超限：{snap:?}"
    );
    assert!(!wrong_pin_attempt(addr).await, "错 pin 尝试不得连上");
    let snap = quic.snapshot();
    assert!(
        snap.flood_refused >= 1,
        "旧缺省下第 16 条尝试应被拒（窗已满）：{snap:?}"
    );
    assert!(snap.admitted >= 5, "撞闸不得影响已完成的胜者：{snap:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// QUIC **Initial** 头里的 token 长度（`None` = 不是 Initial 或解析不出）——「扣住带 token
/// 的 Initial」的中继判据（S3 的 token 有效期用例）。
fn initial_token_len(p: &[u8]) -> Option<u64> {
    if p.len() < 7 || p[0] & 0x80 == 0 || (p[0] >> 4) & 0x3 != 0 {
        return None; // 非长头 / 非 Initial（Retry=3、Handshake=2、0-RTT=1）
    }
    let dcil = *p.get(5)? as usize;
    let mut i = 6 + dcil;
    let scil = *p.get(i)? as usize;
    i += 1 + scil;
    let b = *p.get(i)?;
    match b >> 6 {
        0 => Some(u64::from(b & 0x3f)),
        1 => {
            let b1 = *p.get(i + 1)?;
            Some(u64::from(u16::from_be_bytes([b & 0x3f, b1])))
        }
        2 => {
            let v = u32::from_be_bytes([b & 0x3f, *p.get(i + 1)?, *p.get(i + 2)?, *p.get(i + 3)?]);
            Some(u64::from(v))
        }
        _ => None,
    }
}

/// 「扣住带 token 的 Initial」中继（S3 的 token 有效期判据）：client→exit 方向**放行首发
/// （无 token）Initial**、扣下**带 token** 的 Initial 直到 `hold_until`；exit→client 方向
/// 恒放行（Retry 要能到客户端）。
struct TokenHoldRelay {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl TokenHoldRelay {
    fn start(exit: SocketAddr, hold: Duration) -> Self {
        let front = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("前端 socket 可绑");
        let back = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("后端 socket 可绑");
        front
            .set_read_timeout(Some(Duration::from_millis(20)))
            .expect("前端设超时");
        back.set_read_timeout(Some(Duration::from_millis(20)))
            .expect("后端设超时");
        let addr = front.local_addr().expect("前端地址");
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let hold_until = Instant::now() + hold;
        let thread = std::thread::Builder::new()
            .name("s3-token-hold".into())
            .spawn(move || {
                let mut buf = [0u8; 2048];
                let mut client: Option<SocketAddr> = None;
                while !stop2.load(Ordering::SeqCst) {
                    if let Ok((n, src)) = front.recv_from(&mut buf) {
                        client = Some(src);
                        let has_token = initial_token_len(&buf[..n]).is_some_and(|l| l > 0);
                        if has_token && Instant::now() < hold_until {
                            continue; // 扣住（客户端拿到的 Retry 成果被拖过有效期）
                        }
                        let _ = back.send_to(&buf[..n], exit);
                    }
                    if let Ok((n, _src)) = back.recv_from(&mut buf) {
                        if let Some(c) = client {
                            let _ = front.send_to(&buf[..n], c);
                        }
                    }
                }
            })
            .expect("中继线程可起");
        Self { addr, stop, thread: Some(thread) }
    }
}

impl Drop for TokenHoldRelay {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// **判据（S3-2 / §3.1）**：`retry_token_lifetime` 生效——Retry 的成果被拖过有效期后，
/// token **不再被认**：出口按 RFC 9000 §8.1.3 回 `INVALID_TOKEN`（客户端拿到传输错误，
/// 不是静默超时）。**反证**：缺省 15s 量级下同样扣 1.5s 仍能完成（见下一条用例）。
#[tokio::test]
async fn retry_token_lifetime_is_enforced() {
    let quic = exit_face_with(65, |c| {
        c.retry_policy = RetryPolicy::Always; // 恒 Retry ⇒ 客户端必拿 token（确定性）
        c.retry_token_lifetime = Duration::from_secs(1); // 值域下界：1s
    });
    let tap = TokenHoldRelay::start(quic.local_addr(), Duration::from_millis(1500));
    let (client, cfg, _sock) = client_endpoint_and_config(quic.rpk_public_key());
    let r = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, tap.addr, super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("预算内定音");
    // 出口在包层就回 `INVALID_TOKEN`（CONNECTION_CLOSE，错误码 0xB）——客户端按
    // “TransportError 或 ConnectionClosed”两种包装收到它都算命中。
    match r {
        Err(quinn::ConnectionError::TransportError(e)) => assert_eq!(
            e.code,
            quinn::TransportErrorCode::INVALID_TOKEN,
            "过期 token 应回 INVALID_TOKEN，实得 {e:?}"
        ),
        Err(quinn::ConnectionError::ConnectionClosed(c)) => assert_eq!(
            c.error_code,
            quinn::TransportErrorCode::INVALID_TOKEN,
            "过期 token 应回 INVALID_TOKEN：{c:?}"
        ),
        Err(other) => panic!("应为 INVALID_TOKEN 语义的失败，实得 {other:?}"),
        Ok(_) => panic!("过期 token 不得被接受（1s 有效期未生效？）"),
    }
    assert!(quic.snapshot().retry_sent >= 1, "应至少发过一次 Retry：{:?}", quic.snapshot());
    assert!(quic.stop_within(Instant::now() + BUDGET));
    drop(tap);
}

/// 上一条的**对照臂**：有效期 20s（> 扣包时长）⇒ 同一形态照常完成 —— 证明上一条的红
/// 来自「有效期」而不是「扣包/重传」本身。
#[tokio::test]
async fn retry_token_lifetime_longer_than_hold_still_connects() {
    let quic = exit_face_with(66, |c| {
        c.retry_policy = RetryPolicy::Always;
        c.retry_token_lifetime = Duration::from_secs(20);
    });
    let tap = TokenHoldRelay::start(quic.local_addr(), Duration::from_millis(1500));
    let (client, cfg, _sock) = client_endpoint_and_config(quic.rpk_public_key());
    let conn = tokio::time::timeout(
        WAIT,
        client
            .connect_with(cfg, tap.addr, super::rpk::client_pin::SERVER_NAME)
            .expect("connect 调用面"),
    )
    .await
    .expect("预算内定音")
    .expect("有效期内的 token 必须仍被认（扣包只是拖延）");
    assert!(quic.snapshot().retry_sent >= 1, "Retry 已发（恒档）");
    drop(conn);
    assert!(quic.stop_within(Instant::now() + BUDGET));
    drop(tap);
}

/// **判据（S3-3 / §3.2-⑥ + r14 F12）**：证明失败闸——同 devTag 的 nonce/MAC 类失败
/// 跨阈值 ⇒ 冷却期内**不再发 Challenge**（拒 Hello + 行 + 快照计数同源）；
/// 而**引擎裁决拒绝**（表满/冲突/吊销/窗超的等价注入）**不进计数集** ⇒ 不被冷却。
#[tokio::test]
async fn proof_fail_gate_cools_bad_dev_and_spares_engine_rejected() {
    let (quic, rx) = exit_face_logged(67, |c| {
        c.proof_fail_threshold = 2; // 阈值 2（缺省 10）
        c.per_src_window = Duration::from_secs(60);
    });
    let stub = Stub::new(SECRET);
    let pubkey = [0x77u8; 32];
    let dev = [0x88u8; 8];
    let wrong_secret = [0x00u8; 32];
    // ① 两次 MAC 不符的 Proof ⇒ 跨阈值 ⇒ 进冷却 + 行
    for _ in 0..2 {
        let (_c, conn, _s) = client_conn(&quic).await;
        let got = four_frames(&stub, &quic, &conn, &wrong_secret, &pubkey, &dev).await;
        assert!(got.is_err(), "错 secret 的 Proof 必须被拒");
    }
    drain_until(&rx, "证明失败闸", WAIT);
    assert_eq!(quic.snapshot().proof_cooldowns, 1, "进入冷却一次：{:?}", quic.snapshot());
    // ② 冷却中：同 devTag 再走 Hello ⇒ **不发 Challenge**（连接被拒/关闭）
    let (_c, conn, _s) = client_conn(&quic).await;
    let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
    send.write_all(&HelloFrame::encode(&pubkey, &dev, TS))
        .await
        .expect("写 Hello");
    let mut ch = [0u8; CHALLENGE_LEN];
    let challenge = tokio::time::timeout(WAIT, recv.read_exact(&mut ch))
        .await
        .is_ok_and(|r| r.is_ok())
        && ChallengeFrame::parse(&ch).is_some();
    assert!(!challenge, "冷却期内不得发 Challenge（拒 Hello）");
    assert!(
        quic.snapshot().challenges_refused >= 1,
        "未发挑战的拒绝计数应增长：{:?}",
        quic.snapshot()
    );
    // ③ 引擎裁决拒绝**不**计入：同样两次「引擎拒」不产生第二次冷却
    let stub2_dev = [0x99u8; 8];
    let stub2_pub = [0xAAu8; 32];
    stub.engine_reject(true);
    for _ in 0..2 {
        let (_c, conn, _s) = client_conn(&quic).await;
        let got = four_frames(&stub, &quic, &conn, &SECRET, &stub2_pub, &stub2_dev).await;
        assert!(got.is_err(), "引擎裁决拒绝 ⇒ 准入失败");
    }
    stub.engine_reject(false);
    assert_eq!(
        quic.snapshot().proof_cooldowns,
        1,
        "引擎裁决拒绝不得进证明失败闸（r14 F12）"
    );
    // ④ 该 devTag 未被冷却：新连接照常拿到 Challenge 并完成准入
    let (_c, conn, _s) = client_conn(&quic).await;
    let (_send, _recv) = admit(&stub, &quic, &conn, &SECRET, &stub2_pub, &stub2_dev).await;
    assert!(
        quic.snapshot().regs_accepted >= 1,
        "可用性故障后设备仍能准入（不得被冷却锁死）：{:?}",
        quic.snapshot()
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S3-3 / §3.3-2 与 §3.3-3 的本地可判面）**：洪泛期间——① 并发握手不越
/// `handshake_cap`、连接总数不越 `conn_cap`；② **已采纳连接仍可用**（DATAGRAM 双向
/// 不丢）；（吞吐下降 ≤10% 与 footprint 归 S5 的独占机器实测，本用例只留可用性面。）
#[tokio::test]
async fn flood_keeps_bounds_and_spares_admitted_connection() {
    let (quic, _rx) = exit_face_logged(68, |c| {
        c.per_src_fails = 5;
        c.per_src_window = Duration::from_secs(60);
        c.retry_policy = RetryPolicy::Never; // 只测洪泛面（Retry 另有用例）
    });
    let stub = Stub::new(SECRET);
    let pubkey = [0x33u8; 32];
    let dev = [0x44u8; 8];
    // 先建一条**已采纳**连接（洪泛不得把它打断）
    let (_client, conn, _sock) = client_conn(&quic).await;
    let (_send, _recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    // 洪泛：同一源 12 次「未完成」尝试（上限 5 ⇒ 第 6 次起全拒）
    for _ in 0..12 {
        let _ = wrong_pin_attempt(quic.local_addr()).await;
    }
    let snap = quic.snapshot();
    assert!(snap.flood_refused >= 7, "第 F+1..K 次应全拒：{snap:?}");
    assert!(
        snap.handshakes_in_flight <= 64,
        "并发握手不得越 handshake_cap：{snap:?}"
    );
    assert!(
        snap.connections + snap.handshakes_in_flight <= 64,
        "连接总数不得越 conn_cap（2×max_devices）：{snap:?}"
    );
    // ② 既有连接仍可用：入站数据报仍投引擎、出站 DATAGRAM 仍回得来
    let pkt = inner_pkt(TUN_IP, Ipv4Addr::new(8, 8, 8, 8));
    conn.send_datagram(bytes::Bytes::from(pkt.clone()))
        .expect("洪泛期间既有连接仍应能发");
    assert!(
        pump_until(&stub, &quic, || !stub.packets().is_empty(), WAIT).await,
        "洪泛期间已采纳连接的数据报仍应投引擎"
    );
    assert_eq!(stub.packets()[0], pkt, "字节级一致");
    assert_eq!(
        quic.send_to_pub(&pubkey, &pkt),
        crate::ExitSend::Handled,
        "出站仍走 QUIC 面（绑定未被洪泛打断）"
    );
    let got = tokio::time::timeout(WAIT, conn.read_datagram())
        .await
        .expect("预算内应收到")
        .expect("连接活着");
    assert_eq!(got.as_ref(), &pkt[..], "出站 DATAGRAM 字节级一致");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}



// ---------------------------------------------------------------------------
// M3 S1：服务流受理（tag 分发的最小面 —— probe 回显 / 拒码 / 行）
// ---------------------------------------------------------------------------

/// **判据（M3 §1.1/§1.2/§1.6 + §11-S1 的「复位码/未知 tag」）**：已绑定连接上
/// ① tag ∉ {1..5} ⇒ `reset(0x21)`；② tag ∈ {1..4}（S2 接线前）⇒ `reset(0x22)`；
/// ③ tag=5（probe）⇒ **逐字节回显**，客户端半关（FIN）⇒ 出口 `finish()`（对端读到 EOF）；
/// ④ 三条 E-q5 行（受理/拒/结束）面齐。
#[tokio::test]
async fn service_stream_tag_dispatch_refusals_and_probe_echo() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(0x51), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_client, conn, _sock) = client_conn(&quic).await;
    let (pubkey, dev) = ([0x51u8; 32], [0x52u8; 8]);
    let (_ctl_send, _ctl_recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;

    // ① 未知 tag ⇒ 0x21（读侧拿到 `ReadError::Reset(0x21)`；§1.6 的码表）
    {
        let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
        s.write_all(&[0x99]).await.expect("写未知 tag");
        let e = r
            .read_chunk(32, true)
            .await
            .expect_err("未知 tag 必须被 reset");
        match e {
            quinn::ReadError::Reset(code) => assert_eq!(code.into_inner(), 0x21, "复位码 = TAG_UNKNOWN"),
            other => panic!("期望 Reset(0x21)，实得 {other:?}"),
        }
    }
    // ② files（tag=1）⇒ 0x22（S2 接线前的事实：QUIC 档不提供该服务）
    {
        let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
        s.write_all(&[1]).await.expect("写 tag=1");
        let e = r
            .read_chunk(32, true)
            .await
            .expect_err("tag=1 在 S2 接线前必须被拒");
        match e {
            quinn::ReadError::Reset(code) => {
                assert_eq!(code.into_inner(), 0x22, "复位码 = SERVICE_DISABLED")
            }
            other => panic!("期望 Reset(0x22)，实得 {other:?}"),
        }
    }
    // ③ probe 回显：写 3B ⇒ 收 3B（逐字节）；再写 2B ⇒ 再收 2B（持久流语义）；半关 ⇒ EOF
    {
        let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
        s.write_all(&[5, b'a', b'b', b'c']).await.expect("写 tag+载荷");
        let mut buf = [0u8; 3];
        r.read_exact(&mut buf).await.expect("回显 3B");
        assert_eq!(&buf, b"abc", "逐字节回显（§1.2 的 probe 契约）");
        s.write_all(b"de").await.expect("同流再写");
        let mut buf2 = [0u8; 2];
        r.read_exact(&mut buf2).await.expect("回显 2B");
        assert_eq!(&buf2, b"de", "同流多次往返");
        s.finish().expect("客户端半关");
        // 出口读到 FIN ⇒ `copy` 收 EOF ⇒ `finish()` ⇒ 我方读到 EOF（不是 reset）
        let tail = tokio::time::timeout(WAIT, r.read_chunk(8, true))
            .await
            .expect("半关后应收 EOF（出口 finish）")
            .expect("不得是错误（正常收工走 FIN，不用 reset，§1.3）");
        assert!(tail.is_none(), "EOF ⇒ Ok(None)：{tail:?}");
    }
    // ④ 行面（E-q5 族；首 3 条必出，节流只压后续）
    let lines = drain_until(&rx, "服务流结束", WAIT);
    assert!(
        lines.iter().any(|l| l.contains("服务流拒") && l.contains("未知 tag") && l.contains("0x21")),
        "未知 tag 的归因行：{:?}",
        lines.iter().filter(|l| l.contains("服务流拒")).collect::<Vec<_>>()
    );
    assert!(
        lines.iter().any(|l| l.contains("服务流拒") && l.contains("tag=files") && l.contains("0x22")),
        "tag=1 的归因行：{:?}",
        lines.iter().filter(|l| l.contains("服务流拒")).collect::<Vec<_>>()
    );
    assert!(
        lines.iter().any(|l| l.contains("服务流已受理（tag=probe") && l.contains("dev=")),
        "受理行（含 dev 短指纹）：{:?}",
        lines.iter().filter(|l| l.contains("服务流已受理")).collect::<Vec<_>>()
    );
    assert!(
        lines.iter().any(|l| l.contains("服务流结束（tag=probe") && l.contains("↑5B ↓5B")),
        "结束行（字节数）：{:?}",
        lines.iter().filter(|l| l.contains("服务流结束")).collect::<Vec<_>>()
    );
    // 出口侧计数（快照面；`serve status --json` 的 `quic` 段在 S2 接）
    let snap = quic.snapshot();
    assert_eq!(snap.streams_open, 1, "受理 1 条（probe）");
    assert!(snap.stream_refused >= 2, "两条拒（0x21/0x22）：{}", snap.stream_refused);
    assert_eq!(snap.stream_bytes_in, 5, "回显上行 5B");
    assert_eq!(snap.stream_bytes_out, 5, "回显下行 5B（S2 起按方向拆细）");
    assert_eq!(snap.streams_closed, 1, "收工 1 条");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§1.3 的半关 + 服务流受理的卫生面）**：对端开流后**立刻半关（无 tag）**⇒
/// 出口**静默收**（不计拒绝、不留槽、不打拒行）；随后同一连接上的 probe 流仍能受理并回显
/// （= 受理循环没被前一条畸形流卡住）。
///
/// 为什么不是「读 tag 超时（0x27）到点」用例：QUIC 的流开通 = 对端**首个 STREAM 帧**
/// （`open_bi()` 不通知对端，本用例实测确认）⇒ 1B 的 tag 不存在「部分到达」形态，0x27
/// 臂在正常客户端下不可构造（`exit/serve.rs` 的 `handle_stream` 文档已如实登记）；
/// **客户端侧**的 `0x27 ⇒ StreamErr::BadTag` 分类由 `crate::stream` 的单测钉住。
#[tokio::test]
async fn stream_closed_without_tag_is_absorbed_silently() {
    let (logf, rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig::new(seed(0x52), 32), logf)
        .expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_client, conn, _sock) = client_conn(&quic).await;
    let (pubkey, dev) = ([0x51u8; 32], [0x52u8; 8]);
    let (_ctl_send, _ctl_recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;

    // ① 开流即半关（0B FIN）：出口侧必须静默收（读到 `FinishedEarly(0)`）
    {
        let (mut s, _r) = conn.open_bi().await.expect("open_bi");
        s.finish().expect("立即半关（不发 tag）");
    }
    // ② 同一连接的下一条流必须照常受理（回显 1B）——「前一条畸形流不得卡住受理循环」
    {
        let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
        s.write_all(&[5, 0x5A]).await.expect("写 tag + 1B");
        let mut buf = [0u8; 1];
        tokio::time::timeout(WAIT, r.read_exact(&mut buf))
            .await
            .expect("后续流必须被受理（受理循环未被前一条卡住）")
            .expect("回显 1B");
        assert_eq!(buf, [0x5A], "逐字节回显");
    }
    // ③ 无 tag 的那条**不得**计拒（静默收 ≠ 拒绝）
    let lines = collect_for(&rx, Duration::from_millis(300));
    assert!(
        !lines.iter().any(|l| l.contains("服务流拒")),
        "无 tag 的 FIN 不是拒绝形态：{lines:?}"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.stream_refused, 0, "不得计拒：{snap:?}");
    assert_eq!(snap.streams_open, 1, "只有 probe 那条被受理");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------------------------------------------------------------------------
// M3 S2：服务入口（intake 五件套 + socketpair 泵 + tag 1–4 真入队）
// ---------------------------------------------------------------------------

/// 桩服务（**真线程**——服务的 accept 循环是阻塞面，不能跑在测试的 current_thread
/// runtime 上）：受理一条 ⇒ 发问候 ⇒ 读 4B 请求 ⇒ 回响应 ⇒ **服务侧半关（WRITE）** ⇒
/// 读残尾（验证「客户端 FIN ⇒ 服务侧 read_to_end 收尾」的半关传播）。
///
/// 读数经 `mpsc` 回传（**不**用 `JoinHandle::join` 直接等：测试的 current_thread
/// runtime 一旦被阻塞，客户端的 TAIL/FIN 就发不出去——那就变成「等着自己」的死锁）。
fn stub_service(
    intake: crate::ServiceIntake,
    greeting: &'static [u8],
    response: &'static [u8],
) -> std::sync::mpsc::Receiver<(Vec<u8>, Vec<u8>)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        let mut conn = intake.accept().expect("服务侧受理（QUIC 泵入队）");
        conn.write_all(greeting).expect("问候");
        let mut head = [0u8; 4];
        conn.read_exact(&mut head).expect("读请求头");
        conn.write_all(response).expect("响应");
        conn.shutdown(std::net::Shutdown::Write).expect("服务侧半关（WRITE）");
        let mut rest = Vec::new();
        let _ = conn.read_to_end(&mut rest); // 客户端 FIN 之后的半关读
        let _ = tx.send((head.to_vec(), rest));
    });
    rx
}

/// 等桩服务读数（**异步轮询**：保持 runtime 被驱动，客户端侧的待发字节才有机会上线）。
async fn await_stub(
    rx: &std::sync::mpsc::Receiver<(Vec<u8>, Vec<u8>)>,
    wait: Duration,
) -> (Vec<u8>, Vec<u8>) {
    let deadline = Instant::now() + wait;
    loop {
        match rx.try_recv() {
            Ok(v) => return v,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "桩服务读数未在 {wait:?} 内到达");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("桩服务线程死了"),
        }
    }
}

/// 起一枚「带服务入口」的出口面 + 一条已绑定连接（S2 用例的公共脚手架）。
async fn exit_with_intakes(
    seed_byte: u8,
    intakes: crate::ServiceIntakes,
) -> (
    ExitQuic,
    Receiver<String>,
    quinn::Endpoint,
    quinn::Connection,
    Arc<Stub>,
    [u8; 32],
    [u8; 8],
) {
    let (logf, rx) = sink();
    let cfg = ExitQuicConfig::new(seed(seed_byte), 32).with_intakes(intakes);
    let quic = ExitQuic::start(loopback_socket(), cfg, logf).expect("端点可起");
    let stub = Stub::new(SECRET);
    let (_client, conn, _sock) = client_conn(&quic).await;
    let (pubkey, dev) = ([0x61u8; 32], [0x62u8; 8]);
    let (_ctl_send, _ctl_recv) = admit(&stub, &quic, &conn, &SECRET, &pubkey, &dev).await;
    (quic, rx, _client, conn, stub, pubkey, dev)
}

/// **判据（§2.1 分发 + §2.2 泵：tag 1–3 真入队、字节逐字往返、两向半关、计数/行）**：
/// tag=1 经 socketpair 泵进 intake ⇒ 桩服务受理；问候/请求/响应**逐字节**；服务侧半关 ⇒
/// 客户端读 EOF（FIN 不是 reset）；客户端 FIN ⇒ 服务侧 `read_to_end` 收尾；出口计数按方向
/// 拆细（↑ = 客户端上行、↓ = 下发客户端）。
#[tokio::test]
async fn service_stream_tag1_rides_intake_pump_byte_exact() {
    const GREETING: &[u8] = b"HELLO\n";
    const RESPONSE: &[u8] = b"R-OK\n";
    let (files_intake, files_tx) = crate::ServiceIntake::quic_only(4).expect("intake");
    let svc = stub_service(files_intake, GREETING, RESPONSE);
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) = exit_with_intakes(
        0x61,
        crate::ServiceIntakes {
            files: Some(files_tx),
            ..Default::default()
        },
    )
    .await;

    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[1]).await.expect("tag=files");
    s.write_all(b"GET\n").await.expect("请求体");
    let mut greet = vec![0u8; GREETING.len()];
    r.read_exact(&mut greet).await.expect("问候");
    assert_eq!(greet, GREETING, "服务侧问候逐字节到达客户端");
    let mut resp = vec![0u8; RESPONSE.len()];
    r.read_exact(&mut resp).await.expect("响应");
    assert_eq!(resp, RESPONSE, "服务侧响应逐字节到达客户端");
    // 服务侧半关（shutdown WRITE）⇒ 客户端读到 EOF（FIN；**不是** reset）
    let tail = tokio::time::timeout(WAIT, r.read_chunk(8, true))
        .await
        .expect("服务侧半关后应读得 EOF")
        .expect("不得是错误（收线走 FIN，§1.3）");
    assert!(tail.is_none(), "EOF ⇒ Ok(None)：{tail:?}");
    // 客户端仍可写（半关是**单向**的，§1.3）
    s.write_all(b"TAIL").await.expect("对端 FIN 后仍可写");
    s.finish().expect("客户端 FIN");
    let (req, rest) = await_stub(&svc, WAIT).await;
    assert_eq!(req, b"GET\n", "请求头逐字节");
    assert_eq!(rest, b"TAIL", "客户端 FIN ⇒ 服务侧 read_to_end 收尾（半关传播）");
    assert!(
        wait_until(|| quic.snapshot().streams_closed == 1, WAIT).await,
        "泵应在客户端 FIN 后收工"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.streams_open, 1, "受理 1 条：{snap:?}");
    assert_eq!(snap.stream_refused, 0, "无拒入：{snap:?}");
    assert_eq!(snap.stream_bytes_in, 8, "↑ = GET\\n + TAIL（tag 不算应用字节）");
    assert_eq!(snap.stream_bytes_out, GREETING.len() as u64 + RESPONSE.len() as u64, "↓");
    // E-q5 行族（受理 / 结束）
    let lines = collect_for(&rx, Duration::from_millis(400));
    assert!(
        lines.iter().any(|l| l.contains("服务流已受理（tag=files") && l.contains("dev=")),
        "受理行：{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("服务流结束（tag=files") && l.contains("↑8B ↓11B")),
        "结束行（含逐向字节）：{lines:?}"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§1.7 设计门 2-5：intake 满 ⇒ `0x23` + 行带 n/cap）**：容量 1 的入口先被占满
/// （服务侧不取），第二条 QUIC 服务流必须拿到 `reset(0x23)`（**不是**应用层 busy——那条
/// 路径由服务自身的在册闸承担，见 `homeway-core` 的用例）。
#[tokio::test]
async fn service_stream_intake_full_resets_0x23_with_depth() {
    let (files_intake, files_tx) = crate::ServiceIntake::quic_only(1).expect("intake");
    // 占满：一条原生 socketpair 端入队（服务侧**不**受理——桩不跑 accept）
    let (filler, _peer) = std::os::unix::net::UnixStream::pair().expect("socketpair");
    files_tx.try_enqueue(filler).expect("第 1 条入队");
    assert_eq!(files_tx.depth(), 1);
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) = exit_with_intakes(
        0x62,
        crate::ServiceIntakes {
            files: Some(files_tx),
            ..Default::default()
        },
    )
    .await;

    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[1]).await.expect("tag=files");
    let e = r
        .read_chunk(32, true)
        .await
        .expect_err("入口满 ⇒ 必须被 reset");
    match e {
        quinn::ReadError::Reset(code) => {
            assert_eq!(code.into_inner(), 0x23, "复位码 = INTAKE_FULL")
        }
        other => panic!("期望 Reset(0x23)，实得 {other:?}"),
    }
    let lines = drain_until(&rx, "服务流拒", WAIT);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("入口队列满 1/1") && l.contains("0x23") && l.contains("tag=files")),
        "归因行须带当刻 n/cap：{lines:?}"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.stream_refused, 1, "计拒 1 条：{snap:?}");
    assert_eq!(snap.streams_open, 0, "拒入不记受理：{snap:?}");
    // 服务侧仍能把占位那条取走（拒入不影响已入队的）
    let _ = files_intake.accept().expect("取走占位条");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（M4 §3.2 的第 ④–⑦ 步：真拨号成功 + 回执 + 半关双向 + 计数/行）**：
/// tag=4 + 6B 目标（本机回环上的真 echo）⇒ 出口读得出目标 ⇒ **真拨通** ⇒ 客户端首字节 =
/// `DIAL_OK(0x01)` ⇒ 双向字节逐字往返；客户端 FIN ⇒ 目标见 EOF（半关传播）；目标半关 ⇒
/// 客户端读 EOF（**FIN 不是 reset**）；`stream_bytes_*` **不含 1B 回执**；受理行/结束行逐字。
#[tokio::test]
async fn service_stream_dial_ok_echo_round_trip_and_half_close() {
    // 真 echo 目标（本机回环；`127.0.0.1` = 「出口本机」语义位，§1.3）
    let (target_addr, target_rx) = spawn_echo_target();
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) =
        exit_with_intakes(0x65, crate::ServiceIntakes::default()).await;
    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[4]).await.expect("tag=dial");
    s.write_all(&crate::stream::dial_target(target_addr))
        .await
        .expect("6B 目标");

    // ⑤ 回执先于任何目标字节（首字节 = DIAL_OK）
    let first = tokio::time::timeout(WAIT, r.read_chunk(64, true))
        .await
        .expect("回执必须到达")
        .expect("不得是错误")
        .expect("有数据");
    assert_eq!(first.bytes[0], crate::stream::DIAL_OK, "首字节 = 拨号成功回执");
    let mut got = first.bytes[1..].to_vec();

    // 双向往返（帧后即裸字节）
    s.write_all(b"ping-forward").await.expect("上行");
    let deadline = Instant::now() + WAIT;
    while got.len() < "ping-forward".len() && Instant::now() < deadline {
        let c = tokio::time::timeout(WAIT, r.read_chunk(64, true))
            .await
            .expect("下行")
            .expect("不得是错误")
            .expect("有数据");
        got.extend_from_slice(&c.bytes);
    }
    assert_eq!(got, b"ping-forward", "回显逐字节（回执后的裸字节流）");

    // 客户端 FIN ⇒ 目标侧 read 见 EOF（半关传播；`copy(recv → tcp_w)` 的 `shutdown(WRITE)`）
    s.finish().expect("客户端 FIN");
    let (up, saw_eof) = await_echo_target(&target_rx, WAIT).await;
    assert_eq!(up, b"ping-forward", "目标侧收到上行逐字节");
    assert!(saw_eof, "客户端 FIN ⇒ 目标侧读到 EOF（半关传播）");
    // 目标侧半关（WRITE）⇒ 客户端读 EOF（FIN 语义，**不是 reset**）
    let tail = tokio::time::timeout(WAIT, r.read_chunk(8, true))
        .await
        .expect("目标半关后应读得 EOF")
        .expect("不得是错误（收线走 FIN，§1.3）");
    assert!(tail.is_none(), "EOF ⇒ Ok(None)：{tail:?}");

    assert!(
        wait_until(|| quic.snapshot().streams_closed == 1, WAIT).await,
        "泵应在两侧收线后收工"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.streams_open, 1, "受理 1 条（受理语义 = 拨号成功）：{snap:?}");
    assert_eq!(snap.stream_refused, 0, "无拒入：{snap:?}");
    // 字节账口径（§3.3 写死）：`stream_bytes_*` = **泵搬运的字节**，**不含 1B 回执**
    assert_eq!(snap.stream_bytes_in, 12, "↑ = ping-forward（不含回执）");
    assert_eq!(snap.stream_bytes_out, 12, "↓ = 目标回显（不含回执）");
    let lines = collect_for(&rx, Duration::from_millis(400));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("服务流已受理（tag=dial") && l.contains("dev=")),
        "受理行（语义 = 拨号成功）：{lines:?}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("服务流结束（tag=dial") && l.contains("↑12B ↓12B")),
        "结束行（含逐向字节）：{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("服务流目标侧中断")),
        "正常 FIN 收口不得产「目标侧中断」行：{lines:?}"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§3.2 第 ④ 步失败面：未监听目标 ⇒ `0x25`）**：回环上的死端口 ⇒ 出口
/// `ECONNREFUSED` ⇒ `reset(0x25)` + 拒行（带 errno 原文）；**不写回执**（客户端读到的
/// 是 reset 而不是 `0x01`）。
#[tokio::test]
async fn service_stream_dial_dead_target_resets_0x25_with_errno() {
    let dead = free_loopback_addr();
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) =
        exit_with_intakes(0x66, crate::ServiceIntakes::default()).await;
    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[4]).await.expect("tag=dial");
    s.write_all(&crate::stream::dial_target(dead))
        .await
        .expect("6B 目标");
    let e = r.read_chunk(32, true).await.expect_err("死目标 ⇒ 必须被 reset");
    match e {
        quinn::ReadError::Reset(code) => {
            assert_eq!(code.into_inner(), 0x25, "复位码 = DIAL_REFUSED")
        }
        other => panic!("期望 Reset(0x25)，实得 {other:?}"),
    }
    let lines = drain_until(&rx, "服务流拒", WAIT);
    assert!(
        lines.iter().any(|l| l.contains("tag=dial")
            && l.contains(&format!("目标拨号失败（{dead}："))
            && l.contains("0x25")),
        "拒行须带目标与 errno 原文：{lines:?}"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.stream_refused, 1, "计拒 1 条：{snap:?}");
    assert_eq!(snap.streams_open, 0, "失败拨号不记受理：{snap:?}");
    assert_eq!(snap.stream_bytes_in, 0, "无字节搬运：{snap:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（A8 判定表：拒入类 ⇒ `0x25` + why 逐字）**：五类拒入（未指定 / 本网络 /
/// 受限广播 / 组播 / 端口 0）各自在**拨号之前**被拒——why 串来自 `DialAddrClass::text()`
/// 单源；**透传类（含私网/回环）不得被拒**（那一条由 echo 用例与 `dial.rs` 的单测覆盖）。
#[tokio::test]
async fn service_stream_dial_rejects_a8_classes_before_connecting() {
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) =
        exit_with_intakes(0x67, crate::ServiceIntakes::default()).await;
    let cases = [
        ("0.0.0.0:1", "未指定 0.0.0.0"),
        ("0.1.2.3:1", "本网络 0/8"),
        ("255.255.255.255:1", "受限广播 255.255.255.255"),
        ("224.0.0.1:1", "组播 224/4"),
        ("127.0.0.1:0", "端口 0 不是可拨端口"),
    ];
    for (addr, _why) in cases {
        let dst: std::net::SocketAddrV4 = addr.parse().expect("测试地址可解");
        let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
        s.write_all(&[4]).await.expect("tag=dial");
        s.write_all(&crate::stream::dial_target(dst))
            .await
            .expect("6B 目标");
        let e = r.read_chunk(32, true).await.expect_err("拒入类必须被 reset");
        match e {
            quinn::ReadError::Reset(code) => {
                assert_eq!(code.into_inner(), 0x25, "{addr} 的复位码")
            }
            other => panic!("{addr}：期望 Reset(0x25)，实得 {other:?}"),
        }
    }
    let lines = collect_for(&rx, Duration::from_millis(300));
    // **记行节流**（`log_due` = 首 3 + 每 100）⇒ 只有前三类有行；后两类的 why 由
    // `exit/dial.rs` 的 `a8_address_table_row_by_row` 覆盖（协议面已逐行钉住）。
    for (addr, why) in cases.iter().take(3) {
        assert!(
            lines
                .iter()
                .any(|l| l.contains("目标地址类不可拨") && l.contains(why) && l.contains(addr)),
            "{addr} 的拒行须带 why「{why}」：{lines:?}"
        );
    }
    assert_eq!(
        lines.iter().filter(|l| l.contains("0x25")).count(),
        3,
        "记行节流口径（首 3 有声）：{lines:?}"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.stream_refused, cases.len() as u64, "五类各计一拒：{snap:?}");
    assert_eq!(snap.streams_open, 0, "拒入不记受理：{snap:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§3.2 第 ② 步的失败面：目标帧读不出 ⇒ `0x25` + 拒行）**：只发 tag 就收线 ⇒
/// 「目标帧未读出」拒行（why 与其余两条可区分）；`read_exact` 的 `FinishedEarly` 形态。
#[tokio::test]
async fn service_stream_dial_short_frame_resets_0x25() {
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) =
        exit_with_intakes(0x68, crate::ServiceIntakes::default()).await;
    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[4, 1, 2]).await.expect("tag + 半截目标帧");
    s.finish().expect("对端收线（帧不完整）");
    let e = r.read_chunk(32, true).await.expect_err("短帧 ⇒ 必须被 reset");
    match e {
        quinn::ReadError::Reset(code) => {
            assert_eq!(code.into_inner(), 0x25, "复位码 = DIAL_REFUSED")
        }
        other => panic!("期望 Reset(0x25)，实得 {other:?}"),
    }
    let lines = drain_until(&rx, "服务流拒", WAIT);
    assert!(
        lines
            .iter()
            .any(|l| l.contains("tag=dial") && l.contains("目标帧未读出") && l.contains("0x25")),
        "短帧拒行：{lines:?}"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§3.3 的 additive 可观测面：目标侧异常结束 ⇒ 出口行）**：目标在读走上行后
/// **带 `SO_LINGER(0)` 关闭**（= RST）⇒ 出口下行方向读错误 ⇒ `finish()` 收口 +
/// **「目标侧中断」行**（客户端对白名单外复位码/FIN 都归 EOF ⇒ 这条行是把可观测面拉回
/// 出口的唯一手段）。
#[tokio::test]
async fn service_stream_dial_target_reset_logs_the_additive_line() {
    let (target_addr, _target_rx) = spawn_rst_target();
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) =
        exit_with_intakes(0x69, crate::ServiceIntakes::default()).await;
    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[4]).await.expect("tag=dial");
    s.write_all(&crate::stream::dial_target(target_addr))
        .await
        .expect("6B 目标");
    let first = tokio::time::timeout(WAIT, r.read_chunk(64, true))
        .await
        .expect("回执必须到达")
        .expect("不得是错误")
        .expect("有数据");
    assert_eq!(first.bytes[0], crate::stream::DIAL_OK, "拨号成功回执");
    // 目标在收到上行数据后立刻 RST ⇒ 出口下行读到 ECONNRESET
    s.write_all(b"boom").await.expect("上行触发目标 RST");
    let lines = drain_until_async(&rx, "服务流目标侧中断", WAIT).await;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("tag=dial") && l.contains("down 方向")),
        "目标侧中断行（additive）：{lines:?}"
    );
    // 客户端侧读得 EOF（finish 收口；白名单外 / FIN 都归 EOF）
    let tail = tokio::time::timeout(WAIT, r.read_chunk(8, true))
        .await
        .expect("目标 RST 后客户端应读得 EOF")
        .expect("不得是 typed 错误（出口用 finish 收口）");
    assert!(tail.is_none(), "EOF ⇒ Ok(None)：{tail:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 起一枚本机回环 echo 目标：读多少回多少，**读到 EOF 即半关（WRITE）**并把读数回传。
/// 返回（目标地址，读数口）——读数口给（上行字节，是否读到 EOF）。
fn spawn_echo_target() -> (std::net::SocketAddrV4, Receiver<(Vec<u8>, bool)>) {
    let ln = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("echo 目标可绑");
    let addr = match ln.local_addr().expect("echo 地址") {
        std::net::SocketAddr::V4(v) => v,
        std::net::SocketAddr::V6(_) => unreachable!(),
    };
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        let (mut c, _) = ln.accept().expect("目标被拨通");
        let mut got = Vec::new();
        let mut buf = [0u8; 1024];
        let saw_eof = loop {
            match c.read(&mut buf) {
                Ok(0) => break true,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    if c.write_all(&buf[..n]).is_err() {
                        break false;
                    }
                }
                Err(_) => break false,
            }
        };
        let _ = c.shutdown(std::net::Shutdown::Write); // 目标侧半关 ⇒ 客户端读出 EOF
        let _ = tx.send((got, saw_eof));
    });
    (addr, rx)
}

/// 等 echo 目标读数（异步轮询：保持 runtime 被驱动）。
async fn await_echo_target(rx: &Receiver<(Vec<u8>, bool)>, wait: Duration) -> (Vec<u8>, bool) {
    let deadline = Instant::now() + wait;
    loop {
        match rx.try_recv() {
            Ok(v) => return v,
            Err(std::sync::mpsc::TryRecvError::Empty) => {
                assert!(Instant::now() < deadline, "echo 目标读数未在 {wait:?} 内到达");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => panic!("echo 目标线程死了"),
        }
    }
}

/// 起一枚「收到上行后 `SO_LINGER(0)` 关闭」的目标（= 对端读到 RST）。
fn spawn_rst_target() -> (std::net::SocketAddrV4, Receiver<()>) {
    let ln = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("RST 目标可绑");
    let addr = match ln.local_addr().expect("RST 目标地址") {
        std::net::SocketAddr::V4(v) => v,
        std::net::SocketAddr::V6(_) => unreachable!(),
    };
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        use std::io::Read as _;
        let (mut c, _) = ln.accept().expect("RST 目标被拨通");
        let mut buf = [0u8; 64];
        let _ = c.read(&mut buf); // 收到上行后立刻 RST
        let fd = std::os::fd::AsRawFd::as_raw_fd(&c);
        let l = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        // SAFETY：`setsockopt` 只读这 8 字节结构；失败也不影响「close 即 RST」的尽力语义
        let _ = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                std::ptr::addr_of!(l).cast(),
                std::mem::size_of_val(&l) as libc::socklen_t,
            )
        };
        drop(c);
        let _ = tx.send(());
    });
    (addr, rx)
}

/// 本机回环上的**空闲**端口（bind 后立即关 ⇒ 该端口此刻无监听；`ECONNREFUSED` 面）。
fn free_loopback_addr() -> std::net::SocketAddrV4 {
    let ln = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("探测端口可绑");
    let addr = match ln.local_addr().expect("探测地址") {
        std::net::SocketAddr::V4(v) => v,
        std::net::SocketAddr::V6(_) => unreachable!(),
    };
    drop(ln);
    addr
}

/// **判据（§1.1 受理前置 ⇒ §1.6 的 `0x24`）**：绑定在受理窗内被摘除（设备摘除/轮换的
/// 竞态面：`unbind_pub` 先摘绑定再关连接）⇒ 该连接上的后续服务流一律 `reset(0x24)`。
#[tokio::test]
async fn service_stream_on_unbound_connection_resets_0x24() {
    let (quic, rx, _client, conn, _stub, _pubkey, _dev) =
        exit_with_intakes(0x64, crate::ServiceIntakes::default()).await;
    // 摘绑定（**不**关连接——正是「摘绑定与受理之间的窗」）
    quic.bridge.unbind_conn(1);
    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[5]).await.expect("tag=probe");
    let e = r.read_chunk(32, true).await.expect_err("未绑定 ⇒ 拒");
    match e {
        quinn::ReadError::Reset(code) => {
            assert_eq!(code.into_inner(), 0x24, "复位码 = UNBOUND")
        }
        other => panic!("期望 Reset(0x24)，实得 {other:?}"),
    }
    let lines = drain_until(&rx, "连接未绑定", WAIT);
    assert!(
        lines.iter().any(|l| l.contains("服务流拒") && l.contains("0x24") && l.contains("dev=—")),
        "未绑定归因行（dev=—）：{lines:?}"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.stream_refused, 1, "计拒 1 条：{snap:?}");
    assert_eq!(snap.streams_open, 0, "不得受理：{snap:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§2.1 每 tag 独立入口）**：同一条连接上 tag=2/3 各走各的 intake（term 的桩与
/// speedtest 的桩各自受理自己那条）——分发不串台。
#[tokio::test]
async fn service_streams_route_each_tag_to_its_own_intake() {
    let (term_intake, term_tx) = crate::ServiceIntake::quic_only(4).expect("intake");
    let (speed_intake, speed_tx) = crate::ServiceIntake::quic_only(4).expect("intake");
    let term = stub_service(term_intake, b"T-HI\n", b"T-OK\n");
    let speed = stub_service(speed_intake, b"S-HI\n", b"S-OK\n");
    let (quic, _rx, _client, conn, _stub, _pubkey, _dev) = exit_with_intakes(
        0x65,
        crate::ServiceIntakes {
            term: Some(term_tx),
            speedtest: Some(speed_tx),
            ..Default::default()
        },
    )
    .await;
    for (tag, greeting, response) in [(2u8, b"T-HI\n", b"T-OK\n"), (3u8, b"S-HI\n", b"S-OK\n")] {
        let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
        s.write_all(&[tag]).await.expect("tag");
        s.write_all(b"REQ\n").await.expect("请求");
        let mut g = vec![0u8; greeting.len()];
        r.read_exact(&mut g).await.expect("问候");
        assert_eq!(g, greeting, "tag={tag} 的问候来自自己的入口");
        let mut resp = vec![0u8; response.len()];
        r.read_exact(&mut resp).await.expect("响应");
        assert_eq!(resp, response, "tag={tag} 的响应来自自己的入口");
        s.finish().expect("FIN");
        let _ = r.read_chunk(8, true).await; // 服务侧半关后收 EOF
    }
    let (t_req, _) = await_stub(&term, WAIT).await;
    let (s_req, _) = await_stub(&speed, WAIT).await;
    assert_eq!(t_req, b"REQ\n");
    assert_eq!(s_req, b"REQ\n");
    assert!(wait_until(|| quic.snapshot().streams_closed == 2, WAIT).await);
    assert_eq!(quic.snapshot().streams_open, 2);
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（§2.2-N12：复位 = 关 socketpair（普通 close），错误码不跨 socketpair）**：
/// 客户端 `reset(0)`（不是 FIN）⇒ 泵的**上行**收错即停 ⇒ socketpair 关闭 ⇒ 服务侧
/// `read_to_end` 立即收线（读到 EOF/错误，**不挂死**）；泵两向收工 ⇒ `streams_closed` +1。
#[tokio::test]
async fn service_stream_client_reset_closes_socketpair_like_plain_close() {
    let (files_intake, files_tx) = crate::ServiceIntake::quic_only(4).expect("intake");
    // 桩服务：读问候写完就 `read_to_end`（只在**对端收线**时才返回——复位路径的探针）
    let svc = std::thread::spawn(move || {
        use std::io::{Read as _, Write as _};
        let mut conn = files_intake.accept().expect("受理");
        conn.write_all(b"HI\n").expect("问候");
        let mut rest = Vec::new();
        let _ = conn.read_to_end(&mut rest);
        rest
    });
    let (quic, _rx, _client, conn, _stub, _pubkey, _dev) = exit_with_intakes(
        0x66,
        crate::ServiceIntakes {
            files: Some(files_tx),
            ..Default::default()
        },
    )
    .await;
    let (mut s, mut r) = conn.open_bi().await.expect("open_bi");
    s.write_all(&[1]).await.expect("tag=files");
    let mut greet = [0u8; 3];
    r.read_exact(&mut greet).await.expect("问候");
    assert_eq!(&greet, b"HI\n");
    // 复位（**不是** FIN）：quinn 的对端语义 = 读侧 `ReadError::Reset(0)`
    s.reset(quinn::VarInt::from_u32(0)).expect("客户端 reset");
    let rest = tokio::task::spawn_blocking(move || svc.join().expect("桩线程退出"))
        .await
        .expect("join 不 panic");
    assert!(rest.is_empty(), "复位 ⇒ 服务侧读到 EOF/错误且**无残留字节**：{rest:?}");
    assert!(
        wait_until(|| quic.snapshot().streams_closed == 1, WAIT).await,
        "复位后泵须收工（两向都收）"
    );
    let snap = quic.snapshot();
    assert_eq!(snap.streams_open, 1, "受理计数不受复位影响：{snap:?}");
    assert_eq!(snap.stream_refused, 0, "复位不是拒入形态：{snap:?}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（M6.7 拥塞控制 env 臂的接线）**：`ExitQuic::start` 的启动路径必须**打出行**，
/// 且不设 env 时写的是缺省档 `cubic`——设备/实验室的臂判定（"这一轮跑的到底是哪档"）
/// 全靠这行；行缺失 ⇒ 读数不可归因 ⇒ 本用例红。
///
/// 注：本用例**不**给断言钉死进程 env（`cargo test` 的并行会互相干扰）——只断言行的
/// **形态与字段**（值域检查在 `tuning.rs` 的纯函数用例里）。
#[test]
fn start_logs_congestion_controller_line() {
    let sock = loopback_socket();
    let (logf, rx) = sink();
    let quic = ExitQuic::start(sock, ExitQuicConfig::new(seed(31), 4), Arc::clone(&logf))
        .expect("端点可起");
    let lines = drain_until(&rx, "拥塞控制器（CC）=", WAIT);
    let line = lines.last().expect("CC 行在").clone();
    assert!(line.contains("HOMEWAY_QUIC_CC"), "行须点名 env：{line}");
    assert!(line.contains("cubic"), "行须给出档位（缺省 = cubic）：{line}");
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（CC 档真的进了 `TransportConfig`，不是只打了行）**：出口面的 `ExitQuicConfig.cc`
/// 是**单一来源**——`server_config` 读它、`transport_config_with` 用它；本用例把显式值
/// （绕过 env）灌进配置面，断言「工厂建出的控制器类型/初窗」随该值变。
#[test]
fn exit_config_cc_field_reaches_the_factory() {
    let now = Instant::now();
    let cubic = crate::exit::transport::cc_factory(crate::tuning::CcChoice::Cubic)
        .build(now, crate::exit::transport::INITIAL_MTU);
    let bbr = crate::exit::transport::cc_factory(crate::tuning::CcChoice::Bbr)
        .build(now, crate::exit::transport::INITIAL_MTU);
    assert!(
        cubic.into_any().downcast::<quinn::congestion::Cubic>().is_ok(),
        "cubic 档 = quinn 自带 CUBIC"
    );
    assert!(
        bbr.into_any().downcast::<quinn::congestion::Bbr>().is_ok(),
        "bbr 档 = quinn 自带 BBRv1"
    );
    // 显式配置面（不经 env）也能承载该值
    let cfg = ExitQuicConfig {
        cc: crate::tuning::CcChoice::Bbr,
        ..ExitQuicConfig::new(seed(32), 4)
    };
    assert_eq!(cfg.cc, crate::tuning::CcChoice::Bbr);
}
