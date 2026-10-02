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

#[cfg(test)]
mod tests {
    use super::*;

    fn h(s: &str) -> [u8; 32] {
        let mut v = [0u8; 32];
        hex::decode_to_slice(s, &mut v).unwrap();
        v
    }

    /// 吃 Go 真源向量（fixtures/vectors/psk.json，经 tools/gen-vectors.sh 产出）。
    #[test]
    fn psk_matches_go_vectors() {
        for (secret, want) in [
            (
                "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
                "9e28dcb06a224b2b9573cd11e98f147afe456831d88d88be42447573bcc11d14",
            ),
            (
                "deadbeefcafebabe000000000000000000000000000000000000000000000001",
                "71fb220f3981f7b1e9df3ae57d831c5428f61a3e53f87c9f9e61295dd0249d79",
            ),
        ] {
            let psk = Psk::from(Secret::from(h(secret)));
            assert_eq!(hex::encode(psk.as_bytes()), want, "secret={secret}");
        }
    }
}
