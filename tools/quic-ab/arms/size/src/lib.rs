//! 体积探针：在 **smoltcp**（M5 后核里最大的保留依赖块）已在场的前提下，量 QUIC 栈的
//! **边际增量**。
//!
//! 口径登记（M5 C4）：base 曾含 `boringtun`（WG 数据面），随 WG 面退役删除 ⇒ ③ 格口径由
//! 「WG+QUIC 双栈 base」变为「单承载 base」——**与 M0–M4 的该格读数不可直接互引**（登记 =
//! 设计 §9.2 默认 (c) 的臂退役条）。
//!
//! 导出面：`ClientCore2Version` + `ProbeBaseTouch`（base 引用保活）+ `ProbeRealQuic`
//! （lab 同款占位导出）+ `--features quic` 下的 `ProbeRunQuic`（真自连）。

use std::ffi::CString;

#[no_mangle]
pub extern "C" fn ClientCore2Version() -> *mut std::os::raw::c_char {
    CString::new("size-probe2").unwrap().into_raw()
}

/// smoltcp 保活（模拟核里的拦截栈引用；M5 前还含 boringtun 的 WG 数据面引用）
#[no_mangle]
pub extern "C" fn ProbeBaseTouch() -> usize {
    let mut cfg = smoltcp::iface::Config::new(smoltcp::wire::HardwareAddress::Ip);
    cfg.random_seed = 42;
    let _medi = &mut smoltcp::phy::Loopback::new(smoltcp::phy::Medium::Ip);
    std::mem::size_of_val(&cfg) + cfg.random_seed as usize
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

/// 空壳导出（lab 同款占位：cdylib 至少需要一个导出；真实面在 bin 里）
#[no_mangle]
pub extern "C" fn ProbeRealQuic() -> i32 {
    0
}
