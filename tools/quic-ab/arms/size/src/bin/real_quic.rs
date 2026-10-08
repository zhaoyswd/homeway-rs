//! 真实引用版（bin）：真正构造 QUIC server/client 配置并跑一次握手（不靠 size_of 探针），
//! 强制把 quinn/rustls/ring/tokio 的可达代码全部拉进二进制。这正是「把 QUIC 接进核」时的
//! 真实代码面。转入自 `/tmp/quic-lab/size-probe2/src/bin/real_quic.rs`（仅证书路径改仓内
//! + 两处 `SECURITY: harness-only` 标注）。
//!
//! 注：cdylib 档（`--features quic` 的 lib 测的是 `ProbeRunQuic` 全路径）与本 bin 是同一份
//! 代码的两处副本——cdylib 不能被同包 bin 当 rlib 链接（lab 原样保留；口径可比优先）。

use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig};
use std::sync::Arc;

const CERT_DER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../certs/cert.der");
const KEY_DER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../certs/key.der");

fn tls_server() -> rustls::ServerConfig {
    let cert = std::fs::read(CERT_DER).unwrap();
    let key = std::fs::read(KEY_DER).unwrap();
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(
            vec![rustls::pki_types::CertificateDer::from(cert)],
            rustls::pki_types::PrivateKeyDer::try_from(key).unwrap(),
        )
        .unwrap()
}

fn tls_client() -> rustls::ClientConfig {
    // SECURITY: harness-only（同 lib 侧说明；唯一合法位置 = tools/quic-ab/）
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
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Skip))
        .with_no_client_auth()
}

/// 真跑一次 QUIC 自连（server+client 同进程），返回握手后连接是否建立。
#[no_mangle]
pub extern "C" fn ProbeRunQuic() -> i32 {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(_) => return -1,
    };
    let _g = rt.enter();

    let mut sc = ServerConfig::with_crypto(Arc::new(
        quinn::crypto::rustls::QuicServerConfig::try_from(Arc::new(tls_server())).unwrap(),
    ));
    let mut t = TransportConfig::default();
    t.datagram_send_buffer_size(1 << 20);
    t.datagram_receive_buffer_size(Some(1 << 20));
    sc.transport_config(Arc::new(t));

    let addr: std::net::SocketAddr = "127.0.0.1:0".parse().unwrap();
    let ep = match Endpoint::server(sc, addr) {
        Ok(e) => e,
        Err(_) => return -2,
    };
    let sa = match ep.local_addr() {
        Ok(a) => a,
        Err(_) => return -3,
    };

    let mut cc = ClientConfig::new(Arc::new(
        quinn::crypto::rustls::QuicClientConfig::try_from(Arc::new(tls_client())).unwrap(),
    ));
    let mut t2 = TransportConfig::default();
    t2.datagram_send_buffer_size(1 << 20);
    t2.datagram_receive_buffer_size(Some(1 << 20));
    cc.transport_config(Arc::new(t2));
    let mut cep = match Endpoint::client("127.0.0.1:0".parse().unwrap()) {
        Ok(e) => e,
        Err(_) => return -4,
    };
    cep.set_default_client_config(cc);

    rt.block_on(async move {
        let srv = tokio::spawn(async move {
            if let Some(inc) = ep.accept().await {
                if let Ok(c) = inc.await {
                    let _ = c.read_datagram().await;
                }
            }
        });
        let conn = match cep.connect(sa, "localhost").unwrap().await {
            Ok(c) => c,
            Err(_) => return -5,
        };
        let ok = conn
            .send_datagram(bytes::Bytes::from_static(b"hello"))
            .is_ok();
        let _ = srv.await;
        0i32 + if ok { 1 } else { 0 }
    })
}

fn main() {
    // 直接调用导出面（同一份代码路径），供体积测量时真实可达
    let rc = ProbeRunQuic();
    println!("{{\"arm\":\"size-real-quic\",\"probe_rc\":{rc}}}");
}
