//! QUIC 臂：quinn + rustls(ring) + QUIC DATAGRAM 承载内层包。
//!
//! 形态 = 「把 TUN 数据包简单包装丢给 QUIC 通道」：
//! - 客户端 `send_datagram(内层 IPv4 包)` → 服务端 `read_datagram` → 原样回发
//! - 单连接、单方向满载（bulk 形态），与 WG 臂可比
//!
//! 度量口径（与 WG 臂一致）：
//! - 主指标 = 客户端进程 user+sys CPU / 成功往返 = 每包 CPU（含 runtime 轮询）
//! - 用 `ConnectionStats.udp_rx.bytes` 取**线上真实字节**（含 ACK）⇒ 包开销的地道口径
//!
//! 转入自 `/tmp/quic-lab/quic/src/bin/quic.rs`，三处改动（M0 设计 §4.3）：
//!   ① `MTU` 默认 **1400**（lab 缺省 1200 ⇒ 载荷被压到 max_datagram_size=1162；转正须与
//!      WG 臂同载荷 1280）；
//!   ② 客户端 JSON 由 harness 落盘（`cpu-<arm>-r<N>.json`）；
//!   ③ 证书路径走仓内 `tools/quic-ab/certs/`（编译期相对路径）。
//! 另：CPU/JSON 走 `quic-ab-common`（arms 布局的公共件）。

use quic_ab_common::{cpu_now_us, env_num, ipv4, JsonLine};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use quinn::crypto::rustls::{QuicClientConfig, QuicServerConfig};
use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};

const PKT: usize = 1280;
const ALPN: &[u8] = b"hw-ip/1"; // 自定义 ALPN（非 h3——避免被当作 HTTP/3 处理）

// ---------- 证书：实验用自签（certs/gen_certs.sh 现场生成；DER 不入库） ----------

/// 跳过证书验证的客户端 verifier。
// SECURITY: harness-only —— 跳过验证**只允许**存在于 tools/quic-ab/（产品面由
// `tools/check-quic-isolation.sh` 断言零命中；M2 设计门把「SkipVerify → RPK 钉定」列为门项）。
#[derive(Debug)]
struct SkipVerify;

impl rustls::client::danger::ServerCertVerifier for SkipVerify {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider()
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// 服务端自签证书/私钥（**测试用固定材料**——只为把 QUIC 跑道，不做身份语义）。
fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
    // 仓内路径（改动③）：tools/quic-ab/certs/{cert,key}.der —— 由 certs/gen_certs.sh 现场生成
    let cert = include_bytes!("../../../certs/cert.der").to_vec();
    let key = include_bytes!("../../../certs/key.der").to_vec();
    (
        CertificateDer::from(cert),
        PrivateKeyDer::try_from(key).expect("私钥 DER 形态"),
    )
}

fn transport() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    // 与 WG 臂可比的单流形态：不压缩 ACK 时钟、不做 MTU 探测（固定内层 1280）
    // 改动①：默认 MTU = 1400（lab 缺省 1200 会把载荷压到 1162，与 WG 臂不同载荷）
    let mtu: u16 = env_num("MTU", 1400u16);
    t.initial_mtu(mtu);
    t.min_mtu(mtu);
    t.datagram_receive_buffer_size(Some(64 * 1024 * 1024));
    t.datagram_send_buffer_size(64 * 1024 * 1024);
    // 空口形态：把空闲/保活关掉，只测数据面
    t.max_idle_timeout(Some(Duration::from_secs(30).try_into().unwrap()));
    t.keep_alive_interval(None);
    Arc::new(t)
}

fn server(expect: u64) -> io::Result<()> {
    let (cert, key) = self_signed();
    let mut tls = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("TLS1.3 配置")
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .expect("装载自签证书");
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qsc = QuicServerConfig::try_from(Arc::new(tls)).expect("QUIC server config");

    let mut sc = ServerConfig::with_crypto(Arc::new(qsc));
    sc.transport_config(transport());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _guard = rt.enter(); // quinn::Endpoint 构造需要 runtime 上下文
    let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
    let endpoint = Endpoint::server(sc, addr)?;
    println!("PORT {}", endpoint.local_addr()?.port());
    use std::io::Write as _;
    io::stdout().flush()?;

    rt.block_on(async move {
        while let Some(incoming) = endpoint.accept().await {
            tokio::spawn(async move {
                let conn = match incoming.await {
                    Ok(c) => c,
                    Err(_) => return,
                };
                let mut got: u64 = 0;
                while let Ok(d) = conn.read_datagram().await {
                    got += 1;
                    if expect == 0 {
                        // 回显循环：收 datagram → 原样发回（= 后端拆封后同样处理）
                        if conn.send_datagram(d).is_err() {
                            break;
                        }
                    } else if got >= expect {
                        // oneway 精确口径：本进程 udp_rx = 纯客户端数据包
                        let st = conn.stats();
                        println!(
                            "{{\"arm\":\"quic-oneway-srv\",\"data_pkts\":{got},\"wire_rx_bytes\":{},\"wire_rx_dgrams\":{},\"wire_data_pkt_size\":{:.3},\"ios\":{}}}",
                            st.udp_rx.bytes,
                            st.udp_rx.datagrams,
                            st.udp_rx.bytes as f64 / got as f64,
                            st.udp_rx.ios,
                        );
                        use std::io::Write as _;
                        io::stdout().flush().ok();
                        break;
                    }
                }
                // 给客户端留出收 ACK 的时间窗
                tokio::time::sleep(Duration::from_millis(1500)).await;
            });
        }
    });
    Ok(())
}

fn client(port: u16, n: usize, oneway: bool) -> io::Result<()> {
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let mut tls = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_protocol_versions(&[&rustls::version::TLS13])
    .expect("TLS1.3 配置")
    // SECURITY: harness-only（见 SkipVerify 定义处）
    .dangerous()
    .with_custom_certificate_verifier(Arc::new(SkipVerify))
    .with_no_client_auth();
    tls.alpn_protocols = vec![ALPN.to_vec()];
    let qcc = QuicClientConfig::try_from(Arc::new(tls)).expect("QUIC client config");

    let mut cc = ClientConfig::new(Arc::new(qcc));
    cc.transport_config(transport());

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _guard = rt.enter(); // Endpoint::client 同样需要 runtime 上下文

    rt.block_on(async move {
        let mut endpoint = Endpoint::client("127.0.0.1:0".parse().unwrap())?;
        endpoint.set_default_client_config(cc);
        let conn = endpoint.connect(addr, "localhost").unwrap().await.unwrap();
        eprintln!("QUIC 已连接（alpn={:?}）", conn.handshake_data().is_some());

        // QUIC DATAGRAM 上限：= 路径 MTU - IP/UDP 头 - QUIC 短头 - DATAGRAM 帧头 - AEAD tag
        let mds = conn.max_datagram_size();
        let payload_len = match mds {
            Some(m) => PKT.min(m),
            None => PKT,
        };
        eprintln!("max_datagram_size = {mds:?}；实际载荷 = {payload_len}B（请求 {PKT}B）");

        let pkt = ipv4([10, 0, 0, 2], [10, 0, 0, 1], payload_len);
        let dg = bytes::Bytes::from(pkt);

        // 暖机（含 0-RTT/握手余波）。**oneway 模式下服务端不回显**——不能等 read
        // （会空等到 max_idle_timeout 把连接等死）。
        if oneway {
            for _ in 0..2_000 {
                if conn.send_datagram(dg.clone()).is_err() {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
        } else {
            for _ in 0..2_000 {
                if conn.send_datagram(dg.clone()).is_err() {
                    break;
                }
                let _ = conn.read_datagram().await;
            }
        }

        // IDLE 模式：握手/暖机后纯 hold（无流量）——用于 footprint 稳态采样
        let idle_secs: u64 = env_num("IDLE_SECS", 0u64);
        if idle_secs > 0 {
            eprintln!("IDLE 模式：hold {idle_secs}s（无流量）");
            tokio::time::sleep(Duration::from_secs(idle_secs)).await;
            println!("{{\"arm\":\"quic-idle\",\"idle_secs\":{idle_secs}}}");
            conn.close(0u32.into(), b"done");
            endpoint.wait_idle().await;
            return Ok(());
        }

        let (u0, s0) = cpu_now_us();
        let t0 = Instant::now();
        let mut done = 0usize;
        let (mut big_count, mut big_bytes, mut small_count, mut small_bytes) =
            (0u64, 0u64, 0u64, 0u64);
        for i in 0..n {
            let send_res = if oneway {
                conn.send_datagram_wait(dg.clone()).await
            } else {
                conn.send_datagram(dg.clone())
            };
            match send_res {
                Ok(()) => {
                    if oneway {
                        done += 1;
                    } else if let Ok(d) = conn.read_datagram().await {
                        done += 1;
                        let l = d.len();
                        if l >= 1000 {
                            big_count += 1;
                            big_bytes += l as u64;
                        } else {
                            small_count += 1;
                            small_bytes += l as u64;
                        }
                    }
                }
                Err(e) => {
                    if i % 1000 == 0 {
                        eprintln!("send_datagram err: {e:?}");
                    }
                }
            }
            if i % 5000 == 4999 {
                eprintln!(
                    "  进度 {}/{n}：成功 {done}，已用 {:.1}s",
                    i + 1,
                    t0.elapsed().as_secs_f64()
                );
            }
        }
        let wall = t0.elapsed();
        if oneway {
            tokio::time::sleep(Duration::from_millis(1500)).await;
        }
        let (u1, s1) = cpu_now_us();
        let st = conn.stats();

        let cpu_us = ((u1 - u0) + (s1 - s0)) as f64;
        let rx_bytes = st.udp_rx.bytes;
        let rx_dgrams = st.udp_rx.datagrams;
        println!(
            "{}",
            JsonLine::new("quic-dgram")
                .num("pkts", done as u64)
                .num("payload", payload_len as u64)
                .num("max_datagram_size", mds.unwrap_or(0) as u64)
                .f("wall_ms", wall.as_secs_f64() * 1000.0, 1)
                .f("cpu_ms", cpu_us / 1000.0, 1)
                .f("cpu_us_per_pkt", cpu_us / done.max(1) as f64, 3)
                .f(
                    "mbps",
                    (done * payload_len * 2) as f64 * 8.0 / wall.as_secs_f64() / 1e6,
                    1
                )
                .num("wire_rx_bytes", rx_bytes)
                .num("wire_rx_dgrams", rx_dgrams)
                .f("wire_bytes_per_pkt", rx_bytes as f64 / done.max(1) as f64, 2)
                .f(
                    "wire_dgrams_per_pkt",
                    rx_dgrams as f64 / done.max(1) as f64,
                    4
                )
                .num("data_dgrams", big_count)
                .num("data_dgram_bytes", big_bytes)
                .num("ack_dgrams", small_count)
                .num("ack_dgram_bytes", small_bytes)
                .f("data_dgram_size", big_bytes as f64 / big_count.max(1) as f64, 2)
                .f(
                    "ack_dgram_size",
                    small_bytes as f64 / small_count.max(1) as f64,
                    2
                )
                .render()
        );
        // 干净收尾
        conn.close(0u32.into(), b"done");
        endpoint.wait_idle().await;
        Ok::<(), io::Error>(())
    })?;
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("server") {
        let expect: u64 = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(0);
        server(expect).unwrap();
        return;
    }
    let port: u16 = args
        .get(1)
        .expect("用法: quic <port> <n> [--oneway]")
        .parse()
        .unwrap();
    let n: usize = args
        .get(2)
        .expect("用法: quic <port> <n> [--oneway]")
        .parse()
        .unwrap();
    let oneway = args.iter().any(|a| a == "--oneway");
    client(port, n, oneway).unwrap();
}
