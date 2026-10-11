//! WG PSK 派生（token secret → WireGuard PSK）。
//!
//! 语义真源 `baseline:pkg/proto/psk.go`：保证「偷走后端静态私钥但无 token 的攻击者
//! 也无法中间人」（PSK 混入握手密钥，noise_psk2）。两端（客户端 peer 配置 / 出口动态
//! peer 登记）共用本派生。
//!
//! `psk = HKDF-SHA256(ikm = token.secret, salt = nil, info = "homeway/wg-psk", 32B)`
//! （salt=nil 按 RFC 5869 语义 = HashLen 个零字节；对照向量 `fixtures/vectors/psk.json`）。

use hkdf::Hkdf;
use sha2::Sha256;

use crate::token::Secret;

/// PSK 域分离标签（与 Go 逐字节一致，勿改）。
const PSK_LABEL: &[u8] = b"homeway/wg-psk";

/// 敏感 32B 材料的脱敏 Debug（Go 直译检查：不落任何指纹片段）。
macro_rules! fmt_debug_redacted {
    ($name:ident) => {
        impl core::fmt::Debug for $name {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                write!(f, concat!(stringify!($name), "(<redacted>)"))
            }
        }
    };
}


/// WG 预共享密钥（newtype：防与私钥/secret 裸字节数组混用）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Psk([u8; 32]);

fmt_debug_redacted!(Psk);

impl Psk {
    /// 借视图（喂给 boringtun `Tunn::new` 的 `preshared_key`）。
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl From<Secret> for Psk {
    fn from(secret: Secret) -> Self {
        Psk(hkdf_expand_32(secret.as_bytes(), PSK_LABEL))
    }
}

/// HKDF-SHA256(ikm, salt=nil, info) → 32B（长度恒 32 ⇒ expand 不可能失败，unreachable 标注）。
pub(crate) fn hkdf_expand_32(ikm: &[u8], info: &[u8]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let mut out = [0u8; 32];
    hk.expand(info, &mut out)
        .expect("hkdf expand 32B < 255*32 上限，不可达");
    out
}

// 向量数值面（secret → psk 逐案对账）由集成测试 `tests/identity_vectors.rs::psk_matches_go`
// 吃 `fixtures/vectors/psk.json` 承担（单一真源）；本模块不再重复内联数值副本。
