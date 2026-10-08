//! 出口 **RPK 身份**（RFC 7250）的 rustls/quinn 胶水：服务端出示 Ed25519 裸公钥，
//! 客户端**钉定**比对（M1 设计 §1.3/§12-② 的两半里，M1 做的这半边）。
//!
//! 为什么必须做（设计门 r12 的 B1，高危）：不给客户端一个**可验证的服务端身份**，
//! 产品面就只能 `dangerous()/SkipVerify`，而 M0 隔离门与 AGENTS 的安全纪律都不允许。
//! M1 只做「出口密钥 + 公钥进 token + 客户端钉定校验器」；**客户端证明半边**
//! （Hello/Challenge/Proof、抗放大）仍留 M2。
//!
//! 形态：
//!
//! - **服务端**（本模块顶层）：私钥 = 种子 → PKCS#8（48B，[`crate::rpk`]）→
//!   `key_provider.load_private_key`（ring）→ `AlwaysResolvesServerRawPublicKeys`
//!   向对端出示 SPKI（44B）。
//! - **客户端**（[`client_pin`]）：先把服务端出示的 SPKI 与 token 里的 32B 公钥
//!   （换成的 44B SPKI）**逐字节比对**，再走 `verify_tls13_signature_with_raw_key`
//!   做真正的签名验证——**不是**「连上再拒」：比对失败即 TLS alert，握手中止。
//!
//! **`dangerous()` 的唯一合法位置**（[`client_pin::client_crypto_config`]）：rustls 公开面
//! 里装自定义 verifier 只有 `ClientConfig::builder_*().dangerous()
//! .with_custom_certificate_verifier(...)` 一条路（`with_webpki_verifier` 只收
//! `WebPkiServerVerifier` 具体类型）。隔离门第 ⑤ 条据此开 RPK carve-out，且要求本文件
//! 出现原始公钥验签调用——把「钉定」与「跳过验证」区分开。

use std::sync::Arc;

use quinn::crypto::rustls::QuicServerConfig;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::server::AlwaysResolvesServerRawPublicKeys;
use rustls::sign::CertifiedKey;

use crate::exit::transport;
use crate::rpk::{Ed25519Seed, RpkErr, RpkPublicKey};

/// 加密提供者（ring 后端；与工作区 rustls feature 一一对应，**不用** aws-lc）。
pub(crate) fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    Arc::new(rustls::crypto::ring::default_provider())
}

/// 种子 → `CertifiedKey`（cert_chain = 单条 SPKI，即 RFC 7250 的裸公钥形态）+ 裸公钥。
fn certified_key(seed: &Ed25519Seed) -> Result<(CertifiedKey, RpkPublicKey), RpkErr> {
    let p = provider();
    let key = p
        .key_provider
        .load_private_key(PrivateKeyDer::Pkcs8(seed.to_pkcs8_der().to_vec().into()))
        .map_err(RpkErr::material)?;
    let spki = key.public_key().ok_or(RpkErr::NoPublicKey)?;
    // 自校验：ring 给出的 SPKI 必须是本仓认定的 Ed25519 形态（前缀/长度不符即拒起，
    // 否则客户端钉定会拿一个「我们没预期过」的字节串去比对）
    let pubkey = RpkPublicKey::from_spki_der(spki.as_ref())?;
    let certified = CertifiedKey::new(vec![CertificateDer::from(spki.as_ref().to_vec())], key);
    Ok((certified, pubkey))
}

/// 出口服务端配置 + **公钥**（公钥 = 进 token 的那 32B，见 M1 设计 §12-②）。
///
/// 私钥只活在 `CertifiedKey` 内（quinn/rustls 仅在握手签名时用它）；本函数返回的
/// `RpkPublicKey` 是可公开材料。
pub(crate) fn server_config(seed: &Ed25519Seed) -> Result<(quinn::ServerConfig, RpkPublicKey), RpkErr> {
    let (certified, pubkey) = certified_key(seed)?;
    let rcfg = rustls::ServerConfig::builder_with_provider(provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(RpkErr::material)?
        .with_no_client_auth() // M1 不做客户端证明（M2 的 Hello/Challenge/Proof 面）
        .with_cert_resolver(Arc::new(AlwaysResolvesServerRawPublicKeys::new(Arc::new(
            certified,
        ))));
    let quic_cfg = QuicServerConfig::try_from(rcfg).map_err(RpkErr::material)?;
    let mut scfg = quinn::ServerConfig::with_crypto(Arc::new(quic_cfg));
    scfg.migration(transport::MIGRATION);
    scfg.transport_config(transport::transport_config());
    Ok((scfg, pubkey))
}

/// 客户端钉定面（**消费方 = 岛侧的 `client::Face::open`**：S2a 起接线，故本模块不再需要
/// dead-code 豁免——`#[allow(dead_code)]` 已按 §12.6-3④ 的约定删除）。
///
/// 行为由 `exit::tests` 与 `client::tests` 双侧以真握手断言：错 pin ⇒ 握手中止、
/// 对 pin ⇒ 连通且 `max_datagram_size()`=1362。
pub(crate) mod client_pin {
    use std::sync::Arc;

    use quinn::crypto::rustls::QuicClientConfig;
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};
    use rustls::{DigitallySignedStruct, SignatureScheme};

    use super::provider;
    use crate::rpk::{RpkErr, RpkPublicKey};

    /// 服务端名字（SNI 位）：RPK 体系里名字无语义（身份 = 公钥），服务端 resolver 不看
    /// SNI；取值只需两端一致且稳定（客户端 `connect*` 的 `server_name` 参数用它）。
    pub(crate) const SERVER_NAME: &str = "homeway";

    /// 钉定不符（**类型化**：Display 文案会进 TLS alert 的 reason，排障要看得见
    /// 「是身份对不上，不是网络不通」）。
    #[derive(Debug, thiserror::Error)]
    #[error("RPK 与 token 钉定不符（服务端身份对不上）")]
    struct PinMismatch;

    /// 客户端钉定校验器：**先钉定、再验签**（顺序即语义——钉定不符不进入签名验证）。
    #[derive(Debug)]
    pub(crate) struct RpkPinningVerifier {
        pin: RpkPublicKey,
        provider: Arc<rustls::crypto::CryptoProvider>,
    }

    impl RpkPinningVerifier {
        pub(crate) fn new(pin: RpkPublicKey) -> Self {
            Self {
                pin,
                provider: provider(),
            }
        }
    }

    impl ServerCertVerifier for RpkPinningVerifier {
        fn verify_server_cert(
            &self,
            end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, rustls::Error> {
            // 出示形态 = 44B SPKI（RFC 7250）；与 token 钉定的 32B 公钥换成的 SPKI 比对。
            // 不符 ⇒ 走 rustls 的证书错误分类（客户端发 alert，服务端看到握手中止）。
            if end_entity.as_ref() == self.pin.to_spki_der() {
                Ok(ServerCertVerified::assertion())
            } else {
                Err(rustls::Error::InvalidCertificate(
                    rustls::CertificateError::Other(rustls::OtherError(Arc::new(PinMismatch))),
                ))
            }
        }

        fn verify_tls12_signature(
            &self,
            _message: &[u8],
            _cert: &CertificateDer<'_>,
            _dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            // QUIC 恒 TLS 1.3（本 crate 只开 TLS13 版本）；TLS1.2 面不可达。
            Err(rustls::Error::General(
                "QUIC 恒 TLS1.3——不支持 TLS1.2 签名校验".to_owned(),
            ))
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, rustls::Error> {
            rustls::crypto::verify_tls13_signature_with_raw_key(
                message,
                &SubjectPublicKeyInfoDer::from(cert.as_ref()),
                dss,
                &self.provider.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.provider
                .signature_verification_algorithms
                .supported_schemes()
        }

        /// 声明「本客户端要求裸公钥」——ClientHello 的 `server_certificate_type` 位由
        /// rustls 据此发出（服务端 RPK resolver 才会出示裸公钥而非证书链）。
        fn requires_raw_public_keys(&self) -> bool {
            true
        }
    }

    /// 客户端 crypto 配置（钉定服务端 RPK）——S2 的岛用它建 `quinn::ClientConfig`。
    pub(crate) fn client_crypto_config(pin: RpkPublicKey) -> Result<Arc<QuicClientConfig>, RpkErr> {
        let rcfg = rustls::ClientConfig::builder_with_provider(provider())
            .with_protocol_versions(&[&rustls::version::TLS13])
            .map_err(RpkErr::material)?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(RpkPinningVerifier::new(pin)))
            .with_no_client_auth();
        Ok(Arc::new(
            QuicClientConfig::try_from(rcfg).map_err(RpkErr::material)?,
        ))
    }
}
