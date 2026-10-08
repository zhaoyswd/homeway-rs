//! 多连接内存标定：服务端 hold N 条 QUIC 连接（空转），测每连接边际 footprint。
//! 产品相关性：出口最多承载 32 设备（DEFAULT_MAX_DEVICES）。
//!
//! 转入自 `/tmp/quic-lab/quic/src/bin/multiconn.rs`，改动：证书路径走仓内
//! `tools/quic-ab/certs/`（编译期相对路径）+ 两处跳过验证标注 `SECURITY: harness-only`。

use quinn::{Endpoint, ServerConfig, TransportConfig};
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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let n: usize = args.get(2).and_then(|v| v.parse().ok()).unwrap_or(1);
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
        rt.block_on(async move {
            let mut conns = Vec::new();
            while let Some(inc) = ep.accept().await {
                if let Ok(c) = inc.await {
                    conns.push(c);
                    if conns.len() >= n {
                        eprintln!("已 hold {} 连接", conns.len());
                        // 报告服务端自身 footprint 由外部采样；这里保持存活
                        tokio::time::sleep(Duration::from_secs(3600)).await;
                    }
                }
            }
        });
        return;
    }

    // client 模式：开 n 条连接并 hold
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
    rt.block_on(async move {
        let mut conns = Vec::new();
        for _ in 0..n {
            if let Ok(c) = ep.connect(addr, "localhost").unwrap().await {
                conns.push(c);
            }
        }
        eprintln!("客户端已建 {} 连接，hold", conns.len());
        tokio::time::sleep(Duration::from_secs(3600)).await;
    });
}
