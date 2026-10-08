//! 出口 **RPK 身份**（RFC 7250）的 rustls/quinn 胶水：服务端出示 Ed25519 裸公钥。
//!
//! 为什么必须做（设计门 r12 的 B1，高危）：不给客户端一个**可验证的服务端身份**，
//! 产品面就只能 `dangerous()/SkipVerify`，而 M0 隔离门与 AGENTS 的安全纪律都不允许。
//! M1 只做「出口密钥 + 公钥进 token + 客户端钉定」；**客户端证明半边**
//! （Hello/Challenge/Proof、抗放大）仍留 M2。
//!
//! 形态：私钥 = 种子 → PKCS#8（48B，[`crate::rpk`]）→ `key_provider.load_private_key`（ring）
//! → `AlwaysResolvesServerRawPublicKeys` 向对端出示 SPKI（44B）。客户端侧的**钉定校验器**
//! （比对 token 里的 32B 公钥）与「错 pin ⇒ 握手中止」的判据同批落 [`crate::exit`] 的
//! 下一工作单元。

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
