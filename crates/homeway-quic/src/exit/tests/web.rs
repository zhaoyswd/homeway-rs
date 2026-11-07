use std::io::{Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    mpsc, Arc, Mutex,
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::{
    Ed25519Seed, ExitInbound, ExitQuic, ExitQuicConfig, ServiceIntake, WebAuth, WebClient,
    WebClientError, WebCredential, WebDial, WebManagementConfig, WebSecret,
};

fn credential(secret: u8) -> WebCredential {
    WebCredential::new([1; 16], WebSecret::from_bytes([secret; 32]))
}

const NETWORK_SECRET: [u8; 32] = [22; 32];

struct Server {
    quic: Arc<ExitQuic>,
    intake: ServiceIntake,
    auth: WebAuth,
    config: WebManagementConfig,
    stop: Arc<AtomicBool>,
    requests: Arc<AtomicUsize>,
    reply_mode: Arc<AtomicUsize>,
    leases: Arc<Mutex<Vec<crate::ManagementLease>>>,
    network_allowed: Arc<AtomicBool>,
    engine: Option<JoinHandle<()>>,
    wake: mpsc::SyncSender<()>,
}
impl Server {
    fn new(configured: bool, capacity: usize) -> Self {
        let auth = WebAuth::empty();
        if configured {
            auth.install(credential(2));
        }
        let (intake, tx) = ServiceIntake::quic_only(capacity).unwrap();
        let config = WebManagementConfig::new(auth.clone(), tx);
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let quic = Arc::new(
            ExitQuic::start(
                socket,
                ExitQuicConfig::new(Ed25519Seed::from_bytes([3; 32]), 64)
                    .with_web_management(config.clone()),
                Arc::new(|_| {}),
            )
            .unwrap(),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let q = quic.clone();
        let requests = Arc::new(AtomicUsize::new(0));
        let count = requests.clone();
        let (wake, wake_rx) = mpsc::sync_channel::<()>(1);
        let reply_mode = Arc::new(AtomicUsize::new(0));
        let mode = reply_mode.clone();
        let leases = Arc::new(Mutex::new(Vec::new()));
        let observed = leases.clone();
        let network_allowed = Arc::new(AtomicBool::new(false));
        let allow_network = network_allowed.clone();
        let engine = thread::spawn(move || {
            while !stopped.load(Ordering::Acquire) {
                q.drain_inbound(|item| match item {
                    ExitInbound::Management(request) => {
                        count.fetch_add(1, Ordering::Relaxed);
                        observed.lock().unwrap().push(request.lease.clone());
                        request.reply(match mode.load(Ordering::Acquire) {
                            1 => crate::ClientConfirmation::LegMissing,
                            2 => crate::ClientConfirmation::GenerationMismatch,
                            3 => crate::ClientConfirmation::CapacityFull,
                            _ => crate::ClientConfirmation::Confirmed,
                        });
                    }
                    ExitInbound::Reg(request) => {
                        if allow_network.load(Ordering::Acquire)
                            && request.is_active()
                            && request
                                .frame
                                .mac_matches(&NETWORK_SECRET, &request.exporter)
                        {
                            request.reply(crate::Reg4Verdict::Accepted {
                                tunnel_ip: "100.64.1.1".parse().unwrap(),
                                tun_ip: "100.64.1.2".parse().unwrap(),
                            });
                        } else {
                            request.reply(crate::Reg4Verdict::Rejected {
                                why: crate::RejectWhy::MacMismatch,
                            });
                        }
                    }
                    ExitInbound::Packet { .. } => {
                        panic!("management must not produce an IP packet")
                    }
                });
                let _ = wake_rx.recv_timeout(Duration::from_millis(1));
            }
        });
        Self {
            quic,
            intake,
            auth,
            config,
            stop,
            requests,
            wake,
            reply_mode,
            leases,
            network_allowed,
            engine: Some(engine),
        }
    }
    fn dial(&self, relay: SocketAddr, secret: u8) -> WebDial {
        WebDial::new(
            relay,
            [7; 8],
            self.quic.rpk_public_key(),
            credential(secret),
        )
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.quic.stop();
        self.stop.store(true, Ordering::Release);
        let _ = self.wake.try_send(());
        if let Some(engine) = self.engine.take() {
            engine.join().unwrap();
        }
    }
}

// 单会话真实UDP封套代理，专门让公面WebClient走其生产RelaySock代码。
// 这里不模拟认证/管理wire，所有TLS与proof都由生产两端完成。
struct Envelope {
    addr: SocketAddr,
    stop: Arc<AtomicBool>,
    driver: Option<JoinHandle<()>>,
}
impl Envelope {
    fn new(target: SocketAddr) -> Self {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        socket
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let addr = socket.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let cancelled = stop.clone();
        let driver = thread::spawn(move || {
            let mut client = None;
            let mut buffer = [0u8; 65536];
            while !cancelled.load(Ordering::Acquire) {
                let Ok((n, source)) = socket.recv_from(&mut buffer) else {
                    continue;
                };
                if source == target {
                    if let Some(client) = client {
                        let mut frame = vec![0xBB, 5];
                        frame.extend_from_slice(&buffer[..n]);
                        let _ = socket.send_to(&frame, client);
                    }
                } else if n >= 11 && buffer[0] == 0xAA && buffer[9..11] == [0xBB, 5] {
                    client = Some(source);
                    let _ = socket.send_to(&buffer[11..n], target);
                }
            }
        });
        Self {
            addr,
            stop,
            driver: Some(driver),
        }
    }
}
impl Drop for Envelope {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(driver) = self.driver.take() {
            driver.join().unwrap();
        }
    }
}

fn until(mut predicate: impl FnMut() -> bool) {
    // 本文件是隔离门认可的异步测试面；测试等待使用当前线程时间驱动，
    // 不在QUIC岛内引入std睡眠或用空通道假装事件。
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .unwrap();
    runtime.block_on(async {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while !predicate() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "condition did not settle"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    });
}

#[test]
fn web_client_real_tls_relay_envelope_round_trip_rotation_and_no_network_binding() {
    let server = Server::new(true, 8);
    let relay = Envelope::new(server.quic.local_addr());
    let client = WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3)).unwrap();
    assert_eq!(client.local_addr().ip(), relay.addr.ip());
    let mut worker = server.intake.accept().unwrap();
    worker
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    client
        .stream()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut stream = client.stream();
    stream.write_all(b"input").unwrap();
    let mut read = [0; 5];
    worker.read_exact(&mut read).unwrap();
    assert_eq!(&read, b"input");
    worker.write_all(b"reply").unwrap();
    stream.read_exact(&mut read).unwrap();
    assert_eq!(&read, b"reply");
    assert!(server.requests.load(Ordering::Acquire) > 0);
    assert_eq!(server.quic.snapshot().regs_accepted, 0);
    server.auth.install(credential(9));
    until(|| client.is_closed());
    until(|| server.config.active_sessions() == 0);
    assert!(worker.read(&mut read).unwrap_or(0) == 0);
}

#[test]
fn web_wrong_secret_empty_handle_hot_install_and_intake_full() {
    let server = Server::new(false, 0);
    let relay = Envelope::new(server.quic.local_addr());
    let result = WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3));
    assert!(matches!(result, Err(WebClientError::Unavailable)));
    server.auth.install(credential(2));
    let result = WebClient::connect(server.dial(relay.addr, 3), Duration::from_secs(3));
    assert!(matches!(result, Err(WebClientError::Authentication)));
    let result = WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3));
    assert!(matches!(result, Err(WebClientError::Capacity)));
    until(|| server.config.active_sessions() == 0);
}

#[test]
fn web_eight_pending_streams_are_bounded_and_drop_releases_all_slots() {
    let server = Server::new(true, 8);
    let mut relays = Vec::new();
    let mut clients = Vec::new();
    for _ in 0..8 {
        let relay = Envelope::new(server.quic.local_addr());
        clients
            .push(WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3)).unwrap());
        relays.push(relay);
    }
    assert_eq!(server.config.active_sessions(), 8);
    let extra = Envelope::new(server.quic.local_addr());
    assert!(matches!(
        WebClient::connect(server.dial(extra.addr, 2), Duration::from_secs(3)),
        Err(WebClientError::Capacity)
    ));
    drop(clients);
    until(|| server.config.active_sessions() == 0);
}

#[test]
fn web_management_saturation_recovers_before_ordinary_reg4_registration() {
    let server = Server::new(true, 8);
    let mut relays = Vec::new();
    let mut clients = Vec::new();
    for _ in 0..8 {
        let relay = Envelope::new(server.quic.local_addr());
        clients
            .push(WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3)).unwrap());
        relays.push(relay);
    }
    assert_eq!(server.config.active_sessions(), 8);
    assert_eq!(
        server.quic.snapshot().regs_accepted,
        0,
        "management creates no network binding"
    );
    let extra = Envelope::new(server.quic.local_addr());
    assert!(matches!(
        WebClient::connect(server.dial(extra.addr, 2), Duration::from_secs(3)),
        Err(WebClientError::Capacity)
    ));
    drop(clients);
    until(|| server.config.active_sessions() == 0);
    assert_eq!(server.quic.snapshot().regs_accepted, 0);
    assert!(server
        .leases
        .lock()
        .unwrap()
        .iter()
        .all(|lease| !lease.is_alive()));

    // 同一QUIC端点/源握手闸，使用真实TLS与原H4/C4/P4/A4，不走Web准入。
    // 这证明该压力序列回收后可恢复，不宣称识别W1前握手资源结构隔离。
    server.network_allowed.store(true, Ordering::Release);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let (endpoint, connection) = direct(&server, server.quic.rpk_public_key()).await;
        let (mut send, mut receive) = connection.open_bi().await.unwrap();
        let public = [23; 32];
        let dev = [24; 8];
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        send.write_all(&crate::reg4::HelloFrame::encode(&public, &dev, timestamp))
            .await
            .unwrap();
        let mut challenge = [0u8; crate::reg4::CHALLENGE_LEN];
        tokio::time::timeout(Duration::from_secs(2), receive.read_exact(&mut challenge))
            .await
            .unwrap()
            .unwrap();
        let challenge = crate::reg4::ChallengeFrame::parse(&challenge).unwrap();
        let mut exporter = [0u8; crate::reg4::EXPORTER_LEN];
        connection
            .export_keying_material(&mut exporter, crate::reg4::EXPORTER_LABEL, b"")
            .unwrap();
        send.write_all(&crate::reg4::ProofFrame::encode(
            &NETWORK_SECRET,
            &public,
            &dev,
            timestamp,
            &challenge.nonce,
            &exporter,
        ))
        .await
        .unwrap();
        let mut accepted = [0u8; crate::reg4::ACCEPT_LEN];
        tokio::time::timeout(Duration::from_secs(2), receive.read_exact(&mut accepted))
            .await
            .unwrap()
            .unwrap();
        assert!(crate::reg4::AcceptFrame::parse(&accepted).is_some());
        assert_eq!(
            server.quic.snapshot().regs_accepted,
            1,
            "only ordinary registration creates binding"
        );
        connection.close(
            quinn::VarInt::from_u32(0),
            b"ordinary registration test completed",
        );
        endpoint.close(quinn::VarInt::from_u32(0), b"test completed");
        let _ = tokio::time::timeout(Duration::from_millis(250), endpoint.wait_idle()).await;
    });
}

async fn direct(server: &Server, pin: crate::RpkPublicKey) -> (quinn::Endpoint, quinn::Connection) {
    let mut endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    let crypto = super::super::rpk::client_pin::client_crypto_config(pin).unwrap();
    endpoint.set_default_client_config(quinn::ClientConfig::new(crypto));
    let connection = tokio::time::timeout(
        Duration::from_secs(2),
        endpoint
            .connect(server.quic.local_addr(), "homeway")
            .unwrap(),
    )
    .await
    .unwrap()
    .unwrap();
    (endpoint, connection)
}

async fn authenticate(connection: &quinn::Connection) -> (quinn::SendStream, quinn::RecvStream) {
    let (mut send, mut receive) = connection.open_bi().await.unwrap();
    send.write_all(&crate::web::hello(&[1; 16])).await.unwrap();
    let mut challenge = [0; 19];
    receive.read_exact(&mut challenge).await.unwrap();
    assert_eq!(&challenge[..3], b"WC\x01");
    let mut exporter = [0; 32];
    connection
        .export_keying_material(&mut exporter, crate::web::EXPORTER_LABEL, b"")
        .unwrap();
    send.write_all(&credential(2).proof(challenge[3..].try_into().unwrap(), &exporter))
        .await
        .unwrap();
    let mut accepted = [0; 3];
    receive.read_exact(&mut accepted).await.unwrap();
    assert_eq!(accepted, crate::web::ACCEPT);
    (send, receive)
}

#[tokio::test]
async fn web_extra_bidi_closes_connection_and_datagram_never_enters_device_table() {
    let server = Server::new(true, 8);
    let (_endpoint, connection) = direct(&server, server.quic.rpk_public_key()).await;
    let (_send, _receive) = authenticate(&connection).await;
    connection
        .send_datagram(bytes::Bytes::from_static(b"not an authorized packet"))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(server.quic.snapshot().regs_accepted, 0);
    let (mut extra, _recv) = connection.open_bi().await.unwrap();
    let _ = extra.write_all(&[2]).await;
    let closed = tokio::time::timeout(Duration::from_secs(2), connection.closed())
        .await
        .unwrap();
    assert!(
        matches!(closed, quinn::ConnectionError::ApplicationClosed(ref app) if u64::from(app.error_code) == 0x71)
    );
}

#[tokio::test]
async fn web_replay_from_different_tls_connection_is_rejected() {
    let server = Server::new(true, 8);
    let (_first_endpoint, first) = direct(&server, server.quic.rpk_public_key()).await;
    let (_second_endpoint, second) = direct(&server, server.quic.rpk_public_key()).await;
    let (mut send, mut recv) = second.open_bi().await.unwrap();
    send.write_all(&crate::web::hello(&[1; 16])).await.unwrap();
    let mut challenge = [0; 19];
    recv.read_exact(&mut challenge).await.unwrap();
    let mut other_exporter = [0; 32];
    first
        .export_keying_material(&mut other_exporter, crate::web::EXPORTER_LABEL, b"")
        .unwrap();
    send.write_all(&credential(2).proof(challenge[3..].try_into().unwrap(), &other_exporter))
        .await
        .unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(2), second.closed())
        .await
        .unwrap();
    assert!(
        matches!(closed, quinn::ConnectionError::ApplicationClosed(ref app) if u64::from(app.error_code) == 0x70)
    );
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn web_late_magic_rejected_without_shortening_existing_reg4_first_stream_window() {
    let server = Server::new(true, 8);
    let (_web_endpoint, web) = direct(&server, server.quic.rpk_public_key()).await;
    let (_network_endpoint, network) = direct(&server, server.quic.rpk_public_key()).await;
    // 在原10s设备窗内、管理5s窗外发送首流。此真实握手验证不能用纯常量断言替代。
    tokio::time::sleep(Duration::from_millis(5200)).await;
    let (mut web_send, _) = web.open_bi().await.unwrap();
    web_send
        .write_all(&crate::web::hello(&[1; 16]))
        .await
        .unwrap();
    let (mut network_send, mut network_recv) = network.open_bi().await.unwrap();
    network_send
        .write_all(&crate::reg4::HelloFrame::encode(&[9; 32], &[8; 8], 1))
        .await
        .unwrap();
    let mut challenge = [0; crate::reg4::CHALLENGE_LEN];
    tokio::time::timeout(
        Duration::from_secs(1),
        network_recv.read_exact(&mut challenge),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(&challenge[..2], b"C4");
    let closed = tokio::time::timeout(Duration::from_secs(1), web.closed())
        .await
        .unwrap();
    assert!(
        matches!(closed, quinn::ConnectionError::ApplicationClosed(ref app) if u64::from(app.error_code) == 0x72)
    );
}

#[test]
fn web_wrong_rpk_is_rejected_in_real_tls_handshake() {
    let server = Server::new(true, 8);
    let relay = Envelope::new(server.quic.local_addr());
    let dial = WebDial::new(
        relay.addr,
        [7; 8],
        crate::RpkPublicKey::from_bytes([99; 32]),
        credential(2),
    );
    assert!(matches!(
        WebClient::connect(dial, Duration::from_secs(2)),
        Err(WebClientError::Transport(_))
    ));
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
    assert_eq!(server.config.active_sessions(), 0);
}

#[test]
fn web_leg_missing_retries_then_recovers_and_capacity_maps_to_relay_close() {
    let server = Server::new(true, 8);
    server.reply_mode.store(1, Ordering::Release);
    let mode = server.reply_mode.clone();
    let count = server.requests.clone();
    let release = thread::spawn(move || {
        until(|| count.load(Ordering::Acquire) >= 3);
        mode.store(0, Ordering::Release);
    });
    let relay = Envelope::new(server.quic.local_addr());
    let client = WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3)).unwrap();
    release.join().unwrap();
    assert!(server.requests.load(Ordering::Acquire) >= 4);
    let ids: Vec<_> = server
        .leases
        .lock()
        .unwrap()
        .iter()
        .map(|lease| lease.conn_id())
        .collect();
    assert!(
        ids.iter().all(|id| *id == ids[0]),
        "retry preserves original owner"
    );
    drop(client);
    until(|| {
        server
            .leases
            .lock()
            .unwrap()
            .iter()
            .all(|lease| !lease.is_alive())
    });
    for mode in [2, 3] {
        server.reply_mode.store(mode, Ordering::Release);
        let result = WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3));
        assert!(matches!(result, Err(WebClientError::Relay)));
        until(|| {
            server
                .leases
                .lock()
                .unwrap()
                .iter()
                .all(|lease| !lease.is_alive())
        });
    }
}

#[tokio::test]
async fn web_leg_missing_client_close_releases_lease_before_auth_deadline() {
    let server = Server::new(true, 8);
    server.reply_mode.store(1, Ordering::Release);
    let (endpoint, connection) = direct(&server, server.quic.rpk_public_key()).await;
    let (mut send, mut receive) = connection.open_bi().await.unwrap();
    send.write_all(&crate::web::hello(&[1; 16])).await.unwrap();
    let mut challenge = [0; 19];
    receive.read_exact(&mut challenge).await.unwrap();
    let mut exporter = [0; 32];
    connection.export_keying_material(&mut exporter, crate::web::EXPORTER_LABEL, b"").unwrap();
    send.write_all(&credential(2).proof(challenge[3..].try_into().unwrap(), &exporter)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.requests.load(Ordering::Acquire) < 3 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }).await.unwrap();
    assert_eq!(server.config.active_sessions(), 1);
    connection.close(quinn::VarInt::from_u32(0), b"cancel while awaiting fixed-generation leg");
    // Existing outer accept_bi completes with connection error and drops Held/authenticate.
    // A one-second bound distinguishes cancellation from the unchanged five-second deadline.
    tokio::time::timeout(Duration::from_secs(1), async {
        while server.config.active_sessions() != 0 {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }).await.unwrap();
    assert!(server.leases.lock().unwrap().iter().all(|lease| !lease.is_alive()));
    assert_eq!(server.quic.snapshot().regs_accepted, 0);
    endpoint.close(quinn::VarInt::from_u32(0), b"test done");
    let _ = tokio::time::timeout(Duration::from_millis(250), endpoint.wait_idle()).await;
}

#[test]
fn web_relay_release_without_client_close_invalidates_stream_and_lease() {
    let server = Server::new(true, 8);
    let relay = Envelope::new(server.quic.local_addr());
    let client = WebClient::connect(server.dial(relay.addr, 2), Duration::from_secs(3)).unwrap();
    let mut worker = server.intake.accept().unwrap();
    worker
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert!(client.alive());
    // 独立于客户端Close：模拟引擎先处理RELEASE撤掉同代腿的活位。
    // client保持存活，不能靠WebClient::Drop或CONNECTION_CLOSE解释该测试通过。
    let lease = server.leases.lock().unwrap()[0].clone();
    lease.cancel();
    let mut byte = [0; 1];
    assert_eq!(worker.read(&mut byte).unwrap(), 0);
    until(|| client.is_closed());
    until(|| server.config.active_sessions() == 0);
    assert!(!lease.is_alive());
}

#[tokio::test]
async fn web_bad_version_magic_key_id_and_short_frame_are_rejected() {
    let server = Server::new(true, 8);
    for case in 0..4 {
        let (_endpoint, connection) = direct(&server, server.quic.rpk_public_key()).await;
        let (mut send, mut recv) = connection.open_bi().await.unwrap();
        let mut hello = crate::web::hello(&[1; 16]);
        if case == 0 {
            hello[2] = 2;
        }
        if case == 3 {
            send.write_all(&hello[..5]).await.unwrap();
            send.finish().unwrap();
        } else {
            send.write_all(&hello).await.unwrap();
            if case != 0 {
                let mut challenge = [0; 19];
                recv.read_exact(&mut challenge).await.unwrap();
                let mut exporter = [0; 32];
                connection
                    .export_keying_material(&mut exporter, crate::web::EXPORTER_LABEL, b"")
                    .unwrap();
                let mut proof = credential(2).proof(challenge[3..].try_into().unwrap(), &exporter);
                if case == 1 {
                    proof[..2].copy_from_slice(b"XX");
                }
                if case == 2 {
                    proof[3] ^= 1;
                }
                send.write_all(&proof).await.unwrap();
            }
        }
        let closed = tokio::time::timeout(Duration::from_secs(2), connection.closed())
            .await
            .unwrap();
        let expected = if case == 2 { 0x70 } else { 0x71 };
        assert!(
            matches!(closed, quinn::ConnectionError::ApplicationClosed(ref app) if u64::from(app.error_code) == expected)
        );
    }
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn web_auth_timeout_releases_unconfirmed_lease_and_slot() {
    let server = Server::new(true, 8);
    let (_endpoint, connection) = direct(&server, server.quic.rpk_public_key()).await;
    let (mut send, mut recv) = connection.open_bi().await.unwrap();
    send.write_all(&crate::web::hello(&[1; 16])).await.unwrap();
    let mut challenge = [0; 19];
    recv.read_exact(&mut challenge).await.unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(6), connection.closed())
        .await
        .unwrap();
    assert!(
        matches!(closed, quinn::ConnectionError::ApplicationClosed(ref app) if u64::from(app.error_code) == 0x72)
    );
    assert_eq!(server.config.active_sessions(), 0);
    assert_eq!(server.requests.load(Ordering::Acquire), 0);
}

#[tokio::test]
async fn cancelled_connection_wins_when_engine_reply_is_already_ready() {
    let server = Server::new(true, 8);
    let (_endpoint, connection) = direct(&server, server.quic.rpk_public_key()).await;
    let (send, receive) = tokio::sync::oneshot::channel();
    send.send(crate::Reg4Verdict::Accepted {
        tunnel_ip: "100.64.1.1".parse().unwrap(),
        tun_ip: "100.64.1.2".parse().unwrap(),
    })
    .unwrap();
    connection.close(quinn::VarInt::from_u32(0), b"cancelled before verdict");
    assert!(super::super::conn::wait_engine_reply(&connection, receive)
        .await
        .is_none());
}

#[tokio::test]
async fn closed_binding_never_mutates_or_closes_current_live_binding() {
    let server = Server::new(true, 8);
    let (_live_endpoint, live) = direct(&server, server.quic.rpk_public_key()).await;
    let (_closed_endpoint, closed) = direct(&server, server.quic.rpk_public_key()).await;
    let bridge = &server.quic.bridge;
    let dev = [7; 8];
    let public = [8; 32];
    let tunnel = "100.64.1.1".parse().unwrap();
    let tun = "100.64.1.2".parse().unwrap();
    assert!(bridge.bind(dev, public, 101, live.clone(), tunnel, tun));
    closed.close(quinn::VarInt::from_u32(0), b"stale verdict");
    assert!(!bridge.bind(dev, public, 100, closed, tunnel, tun));
    assert_eq!(bridge.dev_of_conn(101), Some(dev));
    assert_eq!(bridge.dev_of_conn(100), None);
    assert!(live.close_reason().is_none());
}
