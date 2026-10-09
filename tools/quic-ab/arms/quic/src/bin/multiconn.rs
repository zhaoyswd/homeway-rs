//! 多连接内存标定：服务端 hold N 条 QUIC 连接，测每连接边际 footprint（空转档）
//! 与负载态增量（`--load` 档）。
//! 产品相关性：出口最多承载 32 设备（DEFAULT_MAX_DEVICES）。
//!
//! 转入自 `/tmp/quic-lab/quic/src/bin/multiconn.rs`，改动：证书路径走仓内
//! `tools/quic-ab/certs/`（编译期相对路径）+ 两处跳过验证标注 `SECURITY: harness-only`。
//!
//! **M1 S5-1c 增补：`--load` 负载态档**（`docs/reviews/M1-design.md` §9.1-4 的判据
//! 「出口 32 连接 + 持续流量 ⇒ footprint 增量 ≤ 64 MiB + 自有队列上限」）：
//! - `server <n> --load`：逐连接起回显 task（`read_datagram` → 预检 → `send_datagram`）；
//! - `<port> <n> --load [--rate R] [--size S] [--dur D]`：逐连接起发送 + 接收 task，
//!   发送侧恒走**预检**（缓冲不够即丢 + 计数，不静默——真源 = M1 §1.5 的纪律）。
//!
//! 口径说明：**空转档**（不带 `--load`）测的是「缓冲按需增长 ⇒ 稳态不占」的惰性下限；
//! **负载态档**才是把每连接 1 MiB × 2 收发缓冲用起来的那一档。两档一起覆盖 §6.3 的主张。
//! `--rate` 缺省 0 = 尽快发（冲满缓冲；负载态判据要的就是这个最坏面）。

use quinn::{Endpoint, ServerConfig, TransportConfig};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

const ALPN: &[u8] = b"hw-ip/1";

/// 证书路径（仓内；由 certs/gen_certs.sh 现场生成——DER 不入库）。
const CERT_DER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../certs/cert.der");
const KEY_DER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../certs/key.der");

fn tls_server() -> rustls::ServerConfig {
    let cert = std::fs::read(CERT_DER).unwrap();
    let key = std::fs::read(KEY_DER).unwrap();
    let mut sc = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![rustls::pki_types::CertificateDer::from(cert)],
        rustls::pki_types::PrivateKeyDer::try_from(key).unwrap(),
    )
    .unwrap();
    sc.alpn_protocols = vec![ALPN.to_vec()];
    sc
}

/// SECURITY: harness-only —— 跳过验证**只允许**存在于 tools/quic-ab/（产品面由
/// `tools/check-quic-isolation.sh` 断言零命中；M2 设计门列「SkipVerify → RPK 钉定」为门项）。
#[derive(Debug)]
struct Skip;
impl rustls::client::danger::ServerCertVerifier for Skip {
    fn verify_server_cert(
        &self,
        _e: &rustls::pki_types::CertificateDer<'_>,
        _i: &[rustls::pki_types::CertificateDer<'_>],
        _s: &rustls::pki_types::ServerName<'_>,
        _o: &[u8],
        _n: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _m: &[u8],
        _c: &rustls::pki_types::CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

fn tls_client() -> rustls::ClientConfig {
    let mut cc = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .unwrap()
    // SECURITY: harness-only（见 Skip 定义处）
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(Skip))
    .with_no_client_auth();
    cc.alpn_protocols = vec![ALPN.to_vec()];
    cc
}

/// 负载态计数（两侧都打——服务端侧与客户端侧各自的发送/接收/丢弃面）。
#[derive(Default)]
struct LoadStats {
    tx: AtomicU64,
    tx_drop: AtomicU64,
    tx_bytes: AtomicU64,
    rx: AtomicU64,
    rx_bytes: AtomicU64,
}

impl LoadStats {
    fn render(&self, who: &str) -> String {
        format!(
            "{who}: tx={} tx_bytes={} tx_drop={} rx={} rx_bytes={}",
            self.tx.load(Ordering::Relaxed),
            self.tx_bytes.load(Ordering::Relaxed),
            self.tx_drop.load(Ordering::Relaxed),
            self.rx.load(Ordering::Relaxed),
            self.rx_bytes.load(Ordering::Relaxed),
        )
    }
}

/// 回显面（服务端负载档）：读数据报 → **预检**（M1 §1.5 纪律）→ 发回。
fn spawn_echo(conn: quinn::Connection, st: Arc<LoadStats>) {
    tokio::spawn(async move {
        while let Ok(dg) = conn.read_datagram().await {
            st.rx.fetch_add(1, Ordering::Relaxed);
            st.rx_bytes.fetch_add(dg.len() as u64, Ordering::Relaxed);
            if conn.datagram_send_buffer_space() < dg.len() {
                st.tx_drop.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            match conn.send_datagram(dg) {
                Ok(()) => {
                    st.tx.fetch_add(1, Ordering::Relaxed);
                }
                Err(_) => {
                    st.tx_drop.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
}

/// 发送面（客户端负载档）：`--rate 0` = 尽快发（冲满缓冲）；否则按 pps 节拍。
fn spawn_sender(conn: quinn::Connection, size: usize, rate: u32, dur: Duration, st: Arc<LoadStats>) {
    tokio::spawn(async move {
        let pkt = bytes::Bytes::from(vec![0x5Au8; size]);
        let t0 = std::time::Instant::now();
        let mut i: u64 = 0;
        while t0.elapsed() < dur {
            if rate > 0 {
                let due = t0 + Duration::from_micros(1_000_000 * (i + 1) / u64::from(rate));
                let now = std::time::Instant::now();
                if due > now {
                    tokio::time::sleep(due - now).await;
                }
            }
            i += 1;
            if conn.datagram_send_buffer_space() < size {
                st.tx_drop.fetch_add(1, Ordering::Relaxed);
                if rate == 0 {
                    tokio::task::yield_now().await;
                }
                continue;
            }
            match conn.send_datagram(pkt.clone()) {
                Ok(()) => {
                    st.tx.fetch_add(1, Ordering::Relaxed);
                    st.tx_bytes.fetch_add(size as u64, Ordering::Relaxed);
                }
                Err(_) => {
                    st.tx_drop.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
    });
}

/// 接收面（客户端负载档）：把回程读空（否则服务端发送缓冲会顶住）。
/// `--no-read` 时不启本 task —— 用来把**服务端发送缓冲顶满**（最坏缓冲面，
/// §6.3 的「每连接 1 MiB send」上界要真被用起来才叫测过）。
fn spawn_reader(conn: quinn::Connection, st: Arc<LoadStats>) {
    tokio::spawn(async move {
        while let Ok(dg) = conn.read_datagram().await {
            st.rx.fetch_add(1, Ordering::Relaxed);
            st.rx_bytes.fetch_add(dg.len() as u64, Ordering::Relaxed);
        }
    });
}

/// 参数：`--load` / `--rate R` / `--size S` / `--dur D` / `--start-after S`（位置参数外的旗标）。
struct Flags {
    load: bool,
    rate: u32,
    size: usize,
    dur: Duration,
    /// 负载**开跑延时**（客户端侧）：先建链 → 空转 `start_after` 秒 → 再开流量。
    /// 用途 = 同一轮里拿到「空转 footprint」与「负载态 footprint」两读数（增量可直接算）。
    start_after: Duration,
    /// 客户端**不读回程**（顶满服务端发送缓冲的最坏面；见 `spawn_reader` 注释）。
    no_read: bool,
}

fn parse_flags(args: &[String]) -> Flags {
    let mut f = Flags {
        load: false,
        rate: 0,
        size: 1280,
        dur: Duration::from_secs(600),
        start_after: Duration::ZERO,
        no_read: false,
    };
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--load" => f.load = true,
            "--no-read" => f.no_read = true,
            "--rate" => {
                if let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) {
                    f.rate = v;
                    i += 1;
                }
            }
            "--size" => {
                if let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) {
                    f.size = v;
                    i += 1;
                }
            }
            "--dur" => {
                if let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) {
                    f.dur = Duration::from_secs(v);
                    i += 1;
                }
            }
            "--start-after" => {
                if let Some(v) = args.get(i + 1).and_then(|v| v.parse().ok()) {
                    f.start_after = Duration::from_secs(v);
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    f
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1);
    let flags = parse_flags(&args[3..]);
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let _g = rt.enter();

    if args.get(1).map(|s| s.as_str()) == Some("server") {
        let mut sc = ServerConfig::with_crypto(Arc::new(
            quinn::crypto::rustls::QuicServerConfig::try_from(Arc::new(tls_server())).unwrap(),
        ));
        let mut t = TransportConfig::default();
        // 产品口径：QUIC 的最小流面（IP 承载不需要 stream）
        t.max_concurrent_bidi_streams(0u8.into());
        t.max_concurrent_uni_streams(0u8.into());
        // 缓冲按需（惰性）——不给 64MB，档位对齐「保守产品配置」
        t.datagram_receive_buffer_size(Some(1 << 20));
        t.datagram_send_buffer_size(1 << 20);
        sc.transport_config(Arc::new(t));
        let ep = Endpoint::server(sc, "127.0.0.1:0".parse().unwrap()).unwrap();
        println!("PORT {}", ep.local_addr().unwrap().port());
        use std::io::Write as _;
        std::io::stdout().flush().unwrap();
        let st = Arc::new(LoadStats::default());
        rt.block_on(async move {
            let mut conns = Vec::new();
            let mut announced = false;
            while let Some(inc) = ep.accept().await {
                if let Ok(c) = inc.await {
                    if flags.load {
                        spawn_echo(c.clone(), Arc::clone(&st));
                    }
                    conns.push(c);
                    if conns.len() >= n && !announced {
                        announced = true;
                        eprintln!("已 hold {} 连接（load={}）", conns.len(), flags.load);
                        let st2 = Arc::clone(&st);
                        let who = if flags.load { "srv-load" } else { "srv-idle" };
                        std::thread::spawn(move || loop {
                            std::thread::sleep(Duration::from_secs(5));
                            eprintln!("{}", st2.render(who));
                        });
                    }
                }
            }
        });
        return;
    }

    // client 模式：开 n 条连接并 hold（--load 时每连接起发送 + 接收 task）
    let port: u16 = args[1].parse().unwrap();
    let addr: std::net::SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut cc = quinn::ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(tls_client())).unwrap(),
    ));
    let mut t = TransportConfig::default();
    t.datagram_receive_buffer_size(Some(1 << 20));
    t.datagram_send_buffer_size(1 << 20);
    cc.transport_config(Arc::new(t));
    let mut ep = Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    ep.set_default_client_config(cc);
    let st = Arc::new(LoadStats::default());
    rt.block_on(async move {
        let mut conns = Vec::new();
        for _ in 0..n {
            if let Ok(c) = ep.connect(addr, "localhost").unwrap().await {
                conns.push(c);
            }
        }
        let who = if flags.load { "cli-load" } else { "cli-idle" };
        eprintln!(
            "客户端已建 {} 连接，hold（load={} no_read={} size={} rate={} dur={}s）",
            conns.len(),
            flags.load,
            flags.no_read,
            flags.size,
            flags.rate,
            flags.dur.as_secs()
        );
        if flags.load {
            // 先空转 `start_after` 秒（外部在这段里采「空转 footprint」），再开流量
            if !flags.start_after.is_zero() {
                eprintln!("客户端空转 {}s（外部可采空转基线）…", flags.start_after.as_secs());
                tokio::time::sleep(flags.start_after).await;
                eprintln!("客户端开跑负载（size={} rate={} dur={}s）", flags.size, flags.rate, flags.dur.as_secs());
            }
            for c in &conns {
                if !flags.no_read {
                    spawn_reader(c.clone(), Arc::clone(&st));
                }
                spawn_sender(
                    c.clone(),
                    flags.size,
                    flags.rate,
                    flags.dur,
                    Arc::clone(&st),
                );
            }
            let st2 = Arc::clone(&st);
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(5));
                eprintln!("{}", st2.render(who));
            });
        }
        tokio::time::sleep(Duration::from_secs(3600)).await;
    });
}
