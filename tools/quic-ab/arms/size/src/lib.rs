//! 体积探针：在 boringtun + smoltcp 已在场的前提下，量 QUIC 栈的**边际增量**。
//!
//! 注意（口径登记）：真迁移中 ring-shim 会随 WG 删除退役，故本探针（含垫片依赖）的
//! 增量是**上界**；转入自 `/tmp/quic-lab/size-probe2/src/lib.rs`。
//!
//! 三档导出面（供 harness 的四档矩阵）：
//!   · 无 feature      = 空壳（只 ClientCore2Version）
//!   · `quic-shallow`  = 浅引用（QUIC 类型/常量级 —— 量死码消除后的在场成本）
//!   · `quic`          = 真引用（ProbeRunQuic 同进程自连一次）

use std::ffi::CString;

#[no_mangle]
pub extern "C" fn ClientCore2Version() -> *mut std::os::raw::c_char {
    CString::new("size-probe2").unwrap().into_raw()
}

/// boringtun + smoltcp 保活（模拟核里的数据面引用）
#[no_mangle]
pub extern "C" fn ProbeWgTouch() -> usize {
    use boringtun::x25519::{PublicKey, StaticSecret};
    let sk = StaticSecret::from([7u8; 32]);
    let pk = PublicKey::from(&sk);
    let t = boringtun::noise::Tunn::new(sk, pk, None, None, 1, None).unwrap();
    let _ = t;
    let mut cfg = smoltcp::iface::Config::new(smoltcp::wire::HardwareAddress::Ip);
    cfg.random_seed = 42;
    let _medi = &mut smoltcp::phy::Loopback::new(smoltcp::phy::Medium::Ip);
    std::mem::size_of_val(&cfg) + cfg.random_seed as usize
}

/// 浅引用档：触碰 QUIC 栈的公共面但不跑全路径（死码消除后仍被链接的部分即此档增量）。
#[cfg(feature = "quic-shallow")]
#[no_mangle]
pub extern "C" fn SizeProbeQuicTouch() -> usize {
    use quinn::{TransportConfig, VarInt};
    let mut t = TransportConfig::default();
    t.datagram_send_buffer_size(1 << 20);
    t.max_concurrent_bidi_streams(VarInt::from_u32(0));
    let _ = rustls::crypto::ring::default_provider();
    std::mem::size_of_val(&t)
        + std::mem::size_of::<tokio::runtime::Runtime>()
        + rustls::crypto::ring::default_provider().cipher_suites.len()
        + bytes::Bytes::from_static(b"size-probe").len()
}

#[cfg(feature = "quic")]
mod quicreal {
    use quinn::{ClientConfig, Endpoint, ServerConfig, TransportConfig};
    use std::sync::Arc;

    /// 证书路径（仓内；certs/gen_certs.sh 现场生成——DER 不入库）。
    const CERT_DER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../certs/cert.der");
    const KEY_DER: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../certs/key.der");

    fn tls_server() -> rustls::ServerConfig {
        let cert = std::fs::read(CERT_DER).unwrap();
        let key = std::fs::read(KEY_DER).unwrap();
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
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
        // SECURITY: harness-only —— 跳过验证只允许存在于 tools/quic-ab/（见 SkipVerify 说明）
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
        rustls::ClientConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_protocol_versions(&[&rustls::version::TLS13])
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Skip))
        .with_no_client_auth()
    }

    /// 导出给 cdylib 宿主：真跑一次 QUIC 自连（server+client 同进程），
    /// 返回握手后连接是否建立（迫使全部路径可达，不被 LTO 消掉）。
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
            let ok = conn.send_datagram(bytes::Bytes::from_static(b"hello")).is_ok();
            let _ = srv.await;
            0i32 + if ok { 1 } else { 0 }
        })
    }
}
#[cfg(feature = "quic")]
pub use quicreal::ProbeRunQuic;
