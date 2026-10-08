//! RFC 7250 裸公钥（RPK）的**纯 std 小件**：Ed25519 种子 ↔ PKCS#8 ↔ SPKI 的字节层。
//!
//! 为什么单开一个文件、且只有 std 类型：本文件是**同步面也看得懂**的编码层
//! （长度/前缀/字段位置），而 rustls/quinn 胶水在 [`crate::exit::rpk`]（异步面）。
//! 隔离门层 3 对 `src/` 下非 `driver.rs`/`exit/` 文件**零异步栈名字**的断言因此仍成立。
//!
//! 线形态（实测锚点：`openssl genpkey -algorithm ed25519` 的 DER 产物，与
//! rustls/ring 的 `key_provider.load_private_key` 产物逐字节一致）：
//!
//! ```text
//! PKCS#8 私钥（48B） = 302e020100300506032b657004220420 ‖ seed(32B)
//! SPKI 公钥（44B）   = 302a300506032b6570032100       ‖ pubkey(32B)
//! ```
//!
//! 出口侧身份（M1 设计 §1.3/§12-②）= **Ed25519 RPK**：token 里带的是 32B 裸公钥
//! （[`RpkPublicKey`]），客户端钉定比对的是**服务端在 TLS 里出示的 44B SPKI**——
//! 两者的换算就在本文件（[`RpkPublicKey::to_spki_der`]）。M1 不做证书链（无 CA）。

use std::fmt;

/// PKCS#8 私钥前缀（`PrivateKeyInfo{version=0, alg=Ed25519(1.3.101.112)}` + 内层
/// `CurvePrivateKey` OCTET STRING 头 4B）。共 16B，其后即 32B 种子。
pub(crate) const PKCS8_PREFIX: [u8; 16] = [
    0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22, 0x04, 0x20,
];

/// SPKI 公钥前缀（`SubjectPublicKeyInfo{alg=Ed25519}` + BIT STRING 头 2B）。共 12B，
/// 其后即 32B 裸公钥。
pub(crate) const SPKI_PREFIX: [u8; 12] = [
    0x30, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00,
];

/// RPK 面失败（**std-only 载荷**：本类型属公面，不得夹带异步栈错误类型；底层
/// rustls/ring 错误经 [`RpkErr::Material`] 装箱后由 `source()` 透出）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RpkErr {
    /// 输入不是 Ed25519 的 SPKI DER（长度 ≠ 44 或前缀不符）。
    #[error("RPK 公钥不是 Ed25519 SPKI DER（须 44B 且前缀为 302a300506032b6570032100）")]
    BadSpki,
    /// 私钥不提供公钥（Ed25519 的 `SigningKey::public_key` 应恒有——形态异常即拒起）。
    #[error("Ed25519 私钥不提供公钥（形态异常）")]
    NoPublicKey,
    /// 身份材料构建失败（PKCS#8 加载 / 建 rustls 配置 / 转 quinn crypto 配置）。
    #[error("RPK 身份材料构建失败（{0}）")]
    Material(#[source] Box<dyn std::error::Error + Send + Sync>),
}

impl RpkErr {
    /// 装箱底层错误（`exit/rpk.rs` 的 rustls/quinn 失败点统一走这里）。
    pub(crate) fn material(e: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::Material(Box::new(e))
    }
}

/// Ed25519 私钥种子（32B）。
///
/// **敏感材料**：Debug 全脱敏、**不 `Copy`**（每处 `Copy` 都会留下追踪不到的栈副本），
/// Drop 时擦除——形态与 `homeway-core::token::Secret` 同族，但本 crate 是叶子（不得依赖
/// core），故自持一份最小实现。
pub struct Ed25519Seed([u8; 32]);

impl Ed25519Seed {
    /// 从 32B 种子构造（出口侧 = `HKDF(后端静态私钥, "homeway/quic-rpk")`，见
    /// `homeway-core::server` 的装配点）。
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// 借视图（喂给 PKCS#8 组装；不拷贝出所有权）。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// PKCS#8 `PrivateKeyInfo` DER（48B）——rustls 的 `key_provider` 认这一形态。
    pub fn to_pkcs8_der(&self) -> [u8; 48] {
        let mut out = [0u8; 48];
        out[..16].copy_from_slice(&PKCS8_PREFIX);
        out[16..].copy_from_slice(&self.0);
        out
    }
}

impl Clone for Ed25519Seed {
    fn clone(&self) -> Self {
        Self(self.0)
    }
}

impl fmt::Debug for Ed25519Seed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 敏感材料：不落任何指纹（连前缀都不出——跨实例可关联）
        write!(f, "Ed25519Seed(<redacted>)")
    }
}

impl Drop for Ed25519Seed {
    fn drop(&mut self) {
        for x in self.0.iter_mut() {
            // SAFETY: 引用来源合法；volatile 写只保证不被优化消除，不引入 UB
            unsafe { std::ptr::write_volatile(x, 0) };
        }
        std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
    }
}

/// Ed25519 裸公钥（32B）——**公开材料**，按 token 携带的正是这 32B。
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct RpkPublicKey([u8; 32]);

impl RpkPublicKey {
    /// 从 32B 裸公钥构造（token 的 RPK 字段面）。
    pub fn from_bytes(b: [u8; 32]) -> Self {
        Self(b)
    }

    /// 借视图（32B）。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 转成 TLS 出示形态：SPKI DER（44B）——客户端钉定比对用的就是这个。
    pub fn to_spki_der(&self) -> [u8; 44] {
        let mut out = [0u8; 44];
        out[..12].copy_from_slice(&SPKI_PREFIX);
        out[12..].copy_from_slice(&self.0);
        out
    }

    /// 从 SPKI DER（44B）拆出裸公钥；非 Ed25519 SPKI 形态即 [`RpkErr::BadSpki`]。
    pub fn from_spki_der(der: &[u8]) -> Result<Self, RpkErr> {
        if der.len() != 44 || !der.starts_with(&SPKI_PREFIX) {
            return Err(RpkErr::BadSpki);
        }
        let mut b = [0u8; 32];
        b.copy_from_slice(&der[12..]);
        Ok(Self(b))
    }
}

impl fmt::Debug for RpkPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // 公钥：日志短指纹纪律（4B hex，同 core 侧 `PeerId`）
        let short: String = self.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
        write!(f, "RpkPublicKey({short}…)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DER 形态锚点：与 `openssl genpkey -algorithm ed25519` 的产物逐字节同构
    /// （前缀 + 32B 种子；SPKI = 前缀 + 32B 公钥）。
    #[test]
    fn der_layout_matches_openssl_anchor() {
        let seed = Ed25519Seed::from_bytes([0xAB; 32]);
        let pkcs8 = seed.to_pkcs8_der();
        assert_eq!(pkcs8.len(), 48);
        assert_eq!(
            &pkcs8[..16],
            &[
                0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
                0x04, 0x20
            ]
        );
        assert_eq!(&pkcs8[16..], &[0xABu8; 32]);
    }

    #[test]
    fn spki_roundtrip_and_rejects_non_ed25519() {
        let pk = RpkPublicKey::from_bytes([7u8; 32]);
        let spki = pk.to_spki_der();
        assert_eq!(spki.len(), 44);
        assert_eq!(&spki[..12], &SPKI_PREFIX);
        assert_eq!(RpkPublicKey::from_spki_der(&spki).unwrap(), pk);
        // 长度/前缀不符一律拒（不得「猜」别的曲线）
        assert!(matches!(
            RpkPublicKey::from_spki_der(&spki[..43]),
            Err(RpkErr::BadSpki)
        ));
        let mut other = spki;
        other[8] = 0x71; // 改 OID 字节（rsaEncryption 之类）
        assert!(matches!(
            RpkPublicKey::from_spki_der(&other),
            Err(RpkErr::BadSpki)
        ));
    }

    /// 敏感材料：Debug 不落指纹；公钥落 4B 短指纹。
    #[test]
    fn seed_is_redacted_public_key_shows_fingerprint() {
        let s = Ed25519Seed::from_bytes([0xCD; 32]);
        assert_eq!(format!("{s:?}"), "Ed25519Seed(<redacted>)");
        let p = RpkPublicKey::from_bytes([0xCD; 32]);
        assert_eq!(format!("{p:?}"), "RpkPublicKey(cdcdcdcd…)");
    }
}
