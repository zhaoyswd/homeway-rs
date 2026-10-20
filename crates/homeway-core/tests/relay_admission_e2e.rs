//! 真中继 + 真出口引擎 + 真客户端：客户端准入确认必须穿过生产桥，不能由 LEGUP 冒充。
use std::net::{Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use homeway_core::legframe;
use homeway_core::relay::{self, Config as RelayConfig, Relay, StopPipe};
use homeway_core::server::engine::BindMode;
use homeway_core::server::engine::{ServeConfig, ServeEngine};
use homeway_core::server::state::State;
use homeway_quic::{Candidate, Cmd, Island, IslandConfig, IslandCredential, IslandErr, IslandReply, StreamId, StreamTag, TokenSecret, Via};

struct Fixture {
    island: Island,
    engine: Arc<ServeEngine>,
    stop: StopPipe,
    relay_thread: Option<std::thread::JoinHandle<std::io::Result<()>>>,
    dir: PathBuf,
    relay: SocketAddr,
    label: [u8; 8],
    lines: Arc<Mutex<Vec<String>>>,
    secret: [u8; 32],
    pin: homeway_quic::RpkPublicKey,
    _tcp_holder: Option<TcpListener>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.island.stop_within(Instant::now() + Duration::from_secs(3));
        self.engine.shutdown(Duration::ZERO);
        self.stop.signal();
        if let Some(thread) = self.relay_thread.take() { let _ = thread.join(); }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn command<T>(island: &Island, make: impl FnOnce(IslandReply<T>) -> Cmd) -> Result<T, IslandErr> {
    let (tx, rx) = mpsc::channel();
    island.tx().send(make(tx)).unwrap();
    rx.recv_timeout(Duration::from_secs(10)).expect("岛必须有界回执")
}

fn open_echo(island: &Island) -> StreamId {
    let (tx, rx) = mpsc::channel();
    island.tx().send(Cmd::StreamOpen { tag: StreamTag::Probe, reply: tx }).unwrap();
    rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap()
}

fn echo(island: &Island, id: StreamId, bytes: &[u8]) {
    let (tx, rx) = mpsc::channel();
    island.tx().send(Cmd::StreamWrite { id, data: bytes.to_vec(), reply: tx }).unwrap();
    assert_eq!(rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap().n, bytes.len());
    let mut received = Vec::new();
    while received.len() < bytes.len() {
        let (tx, rx) = mpsc::channel();
        island.tx().send(Cmd::StreamRead { id, reply: tx }).unwrap();
        received.extend(rx.recv_timeout(Duration::from_secs(3)).unwrap().unwrap());
    }
    assert_eq!(received, bytes);
}

fn fixture(udp_only: bool) -> Fixture {
    let dir = std::env::temp_dir().join(format!("homeway-relay-admission-e2e-{}-{}", std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
    let lines = Arc::new(Mutex::new(Vec::new()));
    let log_lines = Arc::clone(&lines);
    let logf: homeway_core::Logf = Arc::new(move |line| log_lines.lock().unwrap().push(line.to_owned()));
    let tcp_holder = udp_only.then(|| TcpListener::bind("127.0.0.1:0").unwrap());
    // 正常形态需要TCP可监听，不能把UDP :0随机选中的TCP占用口当作正常fixture。
    // 降级形态仍故意保留tcp_holder，确保实际走UDP-only。
    let reservation = (!udp_only).then(|| {
        for _ in 0..64 {
            let tcp = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = tcp.local_addr().unwrap();
            if let Ok(udp) = UdpSocket::bind(addr) { return (tcp, udp); }
        }
        panic!("未找到同号TCP/UDP私有测试口");
    });
    let listen = tcp_holder.as_ref().map(|s| s.local_addr().unwrap())
        .or_else(|| reservation.as_ref().map(|(tcp, _)| tcp.local_addr().unwrap())).unwrap();
    let mut cfg = RelayConfig::new(listen, Some([77; 32]), Arc::clone(&logf));
    cfg.no_hints = true; // 本用例验证中继驻留与配额，不让 hint 驱动切走数据路径。
    let stop = StopPipe::new().unwrap();
    let fd = stop.read_fd();
    let (ready_tx, ready_rx) = mpsc::channel();
    let relay_thread = std::thread::spawn(move || {
        drop(reservation);
        Relay::new(cfg).run(fd, |port| { ready_tx.send(port).unwrap(); })
    });
    let relay = SocketAddr::from((Ipv4Addr::LOCALHOST, ready_rx.recv_timeout(Duration::from_secs(3)).unwrap()));
    let relay_token = relay::rltoken::build_token(&[77; 32], &relay.to_string(), relay.port(), &|_| {}, &|_| {}).unwrap().token;
    let cfg = ServeConfig {
        state_dir: dir.clone(),
        listen_port: 0,
        quic_listen_port: Some(0),
        bind_iface: BindMode::Addr(Ipv4Addr::LOCALHOST.into()),
        upnp: false,
        public_endpoint: "8.8.8.8:42650".into(),
        stun: String::new(),
        stun6: String::new(),
        doh: String::new(),
        files_port: 0,
        term_port: 0,
        speedtest_port: 0,
        dns_port: 0,
        relay: Some(relay_token),
        dns_probe_target: Vec::new(),
        stun_probe_target: Vec::new(),
        ..ServeConfig::default()
    };
    let engine = ServeEngine::start(cfg, Arc::clone(&logf), Arc::clone(&logf), Arc::clone(&logf), None).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if lines.lock().unwrap().iter().any(|line| line.starts_with(if udp_only { "中继：注册成功（腿" } else { "中继控制面已连" })) { break; }
        assert!(Instant::now() < deadline, "真实出口必须先向私有中继注册");
        std::thread::sleep(Duration::from_millis(10));
    }
    let token = State::open(&dir.join("serve")).unwrap().last_token().unwrap().unwrap();
    let secret = *token.secret.as_bytes();
    let label = legframe::relay_id(token.peer_id.as_bytes());
    let pin = engine.quic_rpk_public_key.unwrap();
    let mut cfg = IslandConfig::new(IslandCredential::new(TokenSecret::from_bytes(secret), [11; 32], [22; 8], pin));
    cfg.bind = Some("127.0.0.1:0".parse().unwrap());
    let island = Island::start(Arc::clone(&logf), Arc::new(|_| {}), cfg).unwrap();
    let fixture = Fixture { island, engine, stop, relay_thread: Some(relay_thread), dir, relay, label, lines, secret, pin, _tcp_holder: tcp_holder };
    command(&fixture.island, |reply| Cmd::Connect { cands: vec![Candidate { addr: relay, via: Via::Relay { label } }], budget: Duration::from_secs(5), demote_adopted: false, reply }).unwrap();
    fixture
}

#[test]
fn real_engine_confirms_relay_admission_and_keeps_stream_through_bootstrap_expiry() {
    let fixture = fixture(false);
    let id = open_echo(&fixture.island);
    echo(&fixture.island, id, b"before flood");
    let mut packet = Vec::new();
    legframe::encode_tagged_frame(&fixture.label, &legframe::frame_bytes(5u8, b"invalid QUIC"), &mut packet);
    let sources: Vec<_> = (0..40).map(|_| UdpSocket::bind("127.0.0.1:0").unwrap()).collect();
    for source in &sources { source.send_to(&packet, fixture.relay).unwrap(); }
    // 超过预准入的10s绝对期限；没有真准入确认的实现会误回收这条健康连接。
    for sequence in 0u8..12 {
        echo(&fixture.island, id, &[sequence; 1024]);
        std::thread::sleep(Duration::from_secs(1));
    }
    echo(&fixture.island, id, b"same stream after bootstrap expiry");
    assert!(matches!(fixture.island.snapshot().via, Some(Via::Relay { .. })));
    assert!(fixture.lines.lock().unwrap().iter().any(|line| line.contains("客户端准入已确认")), "须实际经过引擎准入确认，而非LEGUP或假中继桩");
    // 洪水后的新身份仍可进入正式会话，原身份服务流也保留。
    let mut cfg = IslandConfig::new(IslandCredential::new(TokenSecret::from_bytes(fixture.secret), [33; 32], [44; 8], fixture.pin));
    cfg.bind = Some("127.0.0.1:0".parse().unwrap());
    let other = Island::start(Arc::new(|_| {}), Arc::new(|_| {}), cfg).unwrap();
    let connected = command(&other, |reply| Cmd::Connect { cands: vec![Candidate { addr: fixture.relay, via: Via::Relay { label: fixture.label } }], budget: Duration::from_secs(5), demote_adopted: false, reply });
    assert!(connected.is_ok(), "匿名来源不得消耗正式名额：{connected:?}");
    echo(&other, open_echo(&other), b"new legitimate client");
    echo(&fixture.island, id, b"first client still alive");
    assert!(other.stop_within(Instant::now() + Duration::from_secs(3)));
}

#[test]
fn udp_only_relay_session_is_authenticated_by_real_engine() {
    let fixture = fixture(true);
    let id = open_echo(&fixture.island);
    echo(&fixture.island, id, b"authenticated UDP SESSION path");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !fixture.lines.lock().unwrap().iter().any(|line| line.contains("客户端准入已确认")) {
        assert!(Instant::now() < deadline, "UDP SESSION也必须由真实引擎准入提升");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(matches!(fixture.island.snapshot().via, Some(Via::Relay { .. })));
}

#[test]
fn invalid_client_credential_does_not_promote_provisional_relay_session() {
    let fixture = fixture(false);
    let initial = fixture.lines.lock().unwrap().iter().filter(|line| line.contains("客户端准入已确认")).count();
    let mut cfg = IslandConfig::new(IslandCredential::new(TokenSecret::from_bytes([99;32]), [55;32], [66;8], fixture.pin));
    cfg.bind = Some("127.0.0.1:0".parse().unwrap());
    let bad = Island::start(Arc::new(|_| {}), Arc::new(|_| {}), cfg).unwrap();
    let failed = command(&bad, |reply| Cmd::Connect { cands: vec![Candidate { addr: fixture.relay, via: Via::Relay { label: fixture.label } }], budget: Duration::from_secs(3), demote_adopted: false, reply });
    assert!(matches!(failed, Err(IslandErr::AdmissionRejected { .. })), "假客户端凭证必须被真实引擎拒绝：{failed:?}");
    assert!(bad.stop_within(Instant::now() + Duration::from_secs(3)));
    assert_eq!(fixture.lines.lock().unwrap().iter().filter(|line| line.contains("客户端准入已确认")).count(), initial);
    echo(&fixture.island, open_echo(&fixture.island), b"legitimate client retained");
}

#[test]
fn new_client_admits_during_continuous_initial_flood_with_network_delay() {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicBool, Ordering};
    let fixture = fixture(false);
    // 单socket转发，故意延后下行100ms，使新连接留在真实TLS/RREG网络窗口内。
    let proxy = UdpSocket::bind("127.0.0.1:0").unwrap();
    proxy.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
    let proxy_addr = proxy.local_addr().unwrap(); let relay = fixture.relay;
    let stopped = Arc::new(AtomicBool::new(false)); let seen_reply = Arc::new(AtomicBool::new(false));
    let stop_proxy = Arc::clone(&stopped); let replied = Arc::clone(&seen_reply);
    let proxy_thread = std::thread::spawn(move || {
        let mut client = None; let mut queued = VecDeque::new(); let mut buf = [0;65536];
        while !stop_proxy.load(Ordering::Acquire) {
            if let Ok((n, from)) = proxy.recv_from(&mut buf) {
                if from == relay {
                    replied.store(true, Ordering::Release);
                    queued.push_back((Instant::now() + Duration::from_millis(100), buf[..n].to_vec()));
                } else { client = Some(from); let _ = proxy.send_to(&buf[..n], relay); }
            }
            while queued.front().is_some_and(|(at, _)| *at <= Instant::now()) {
                let (_, bytes) = queued.pop_front().unwrap();
                if let Some(client) = client { let _ = proxy.send_to(&bytes, client); }
            }
        }
    });
    struct Stopper(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
    impl Drop for Stopper { fn drop(&mut self) { self.0.store(true,Ordering::Release); if let Some(h)=self.1.take(){ let _=h.join(); } } }
    let _proxy_guard = Stopper(Arc::clone(&stopped), Some(proxy_thread));
    let mut cfg=IslandConfig::new(IslandCredential::new(TokenSecret::from_bytes(fixture.secret),[88;32],[89;8],fixture.pin));
    cfg.bind=Some("127.0.0.1:0".parse().unwrap());
    let client=Island::start(Arc::new(|_|{}),Arc::new(|_|{}),cfg).unwrap();
    let (tx,rx)=mpsc::channel();
    client.tx().send(Cmd::Connect{cands:vec![Candidate{addr:proxy_addr,via:Via::Relay{label:fixture.label}}],budget:Duration::from_secs(5),demote_adopted:false,reply:tx}).unwrap();
    let deadline=Instant::now()+Duration::from_secs(3);
    while !seen_reply.load(Ordering::Acquire) { assert!(Instant::now()<deadline,"须先进入已LEGUP的TLS窗口"); std::thread::sleep(Duration::from_millis(2)); }
    let stop_flood=Arc::new(AtomicBool::new(false)); let stop=Arc::clone(&stop_flood); let label=fixture.label;
    let flood=std::thread::spawn(move || {
        let sockets:Vec<_>=(0..8).map(|_|UdpSocket::bind("127.0.0.1:0").unwrap()).collect();
        let mut sequence=0u64;
        while !stop.load(Ordering::Acquire) {
            let mut initial=vec![0;1200]; initial[0]=0xc0;initial[4]=1;initial[5]=8; initial[6..14].copy_from_slice(&sequence.to_be_bytes());
            let mut tagged=Vec::new(); legframe::encode_tagged_frame(&label,&legframe::frame_bytes(5u8,&initial),&mut tagged);
            sockets[sequence as usize%sockets.len()].send_to(&tagged,relay).unwrap();
            sequence+=1; std::thread::sleep(Duration::from_millis(10));
        }
    });
    let _flood_guard=Stopper(stop_flood,Some(flood));
    let connected=rx.recv_timeout(Duration::from_secs(7)).unwrap();
    assert!(connected.is_ok(),"持续100pps合法Initial形状洪水不得挤掉正在认证的新客户端：{connected:?}");
    echo(&client,open_echo(&client),b"new client during continuous initial flood");
    echo(&fixture.island,open_echo(&fixture.island),b"existing admitted client retained");
    assert!(client.stop_within(Instant::now()+Duration::from_secs(3)));
}
