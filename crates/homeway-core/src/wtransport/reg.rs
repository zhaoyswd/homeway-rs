//! 注册报文 v2（客户端 WG 公钥的入场券，与 WG 首个握手同数据报发出，1 RTT 建连）。
//!
//! 语义真源 `baseline:pkg/proto/reg.go`：
//!
//! ```text
//! "H2"(2B) ‖ pubkey(32B) ‖ devTag(8B) ‖ ts(8B BE 秒) ‖ mac(16B)
//! mac = HMAC-SHA256(token secret, "hr-reg2" ‖ pubkey ‖ devTag ‖ ts)[:16]
//! ```
//!
//! devTag 在 MAC 覆盖内（防篡改）但**不是凭证**——准入只由 HMAC 决定。重放无害：
//! 旧公钥无对应私钥，握手无法完成；同设备重复注册只刷新既有记录。
//! 时间窗 ±90s（服务端校验；客户端只管打新鲜 ts）。v1（"HR"）不再接受。
//! R1 客户端只需 encode（verify 属出口侧，R3）。

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::identity::Identity;
use crate::token::Secret;

const REG_MAGIC: &[u8; 2] = b"H2";
/// 定长 2+32+8+8+16。
pub const REG_LEN: usize = 66;
const MAC_LABEL: &[u8] = b"hr-reg2";

/// 生成带新鲜时间戳的注册报文（`Identity::Reg` 同义；`now_unix` 由调用方注入便于测试）。
pub fn encode_reg(secret: &Secret, identity: &Identity, now_unix: u64, out: &mut Vec<u8>) {
    encode_reg_parts(secret, &identity.public_key(), identity.dev_tag().as_bytes(), now_unix, out);
}

/// 按材料编码（Bind 的 reg 搭车持有 pubkey/dev_tag 原件，不持 Identity）。
pub fn encode_reg_parts(
    secret: &Secret,
    pubkey: &[u8; 32],
    dev_tag: &[u8; 8],
    now_unix: u64,
    out: &mut Vec<u8>,
) {
    out.clear();
    out.reserve(REG_LEN);
    out.extend_from_slice(REG_MAGIC);
    out.extend_from_slice(pubkey);
    out.extend_from_slice(dev_tag);
    out.extend_from_slice(&now_unix.to_be_bytes());
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC 任意长密钥");
    mac.update(MAC_LABEL);
    mac.update(pubkey);
    mac.update(dev_tag);
    mac.update(&now_unix.to_be_bytes());
    let sum = mac.finalize().into_bytes();
    out.extend_from_slice(&sum[..16]);
}

/// 便捷形态（整包所有权）。
pub fn reg_bytes(secret: &Secret, identity: &Identity, now_unix: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(REG_LEN);
    encode_reg(secret, identity, now_unix, &mut v);
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DevTag;
    use crate::token::PeerId;
    use std::fs;

    /// 形状与 MAC 覆盖域自检（字节布局钉死；与 Go 的互证在 1e 真出口 `peer: +` 行）。
    #[test]
    fn reg_shape_and_mac_coverage() {
        // 用向量族里的固定身份（master=000102..1f peer=11..11 → 私钥/公钥已知）
        let master_hex = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";
        let peer_hex = "1111111111111111111111111111111111111111111111111111111111111111";
        let mut master = [0u8; 32];
        for (i, pair) in master_hex.as_bytes().chunks(2).enumerate() {
            master[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
        }
        let mut peer = [0u8; 32];
        for (i, pair) in peer_hex.as_bytes().chunks(2).enumerate() {
            peer[i] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
        }

        // 走 store 路径拿 Identity（devTag 文件预置固定值便于断言）
        let dir = std::env::temp_dir().join(format!(
            "homeway-rs-reg-test-{}-{:?}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("master.key"), master).unwrap();
        fs::write(dir.join("devtag"), [0xAA; 8]).unwrap();
        let (id, _, _) = crate::identity::load_or_create(Some(&dir), &PeerId::from(peer)).unwrap();

        let secret = Secret::from([0x42; 32]);
        let reg = reg_bytes(&secret, &id, 1_800_000_000);
        assert_eq!(reg.len(), REG_LEN);
        assert_eq!(&reg[..2], b"H2");
        assert_eq!(&reg[2..34], &id.public_key()[..]);
        assert_eq!(&reg[34..42], &[0xAA; 8]);
        assert_eq!(
            u64::from_be_bytes(reg[42..50].try_into().unwrap()),
            1_800_000_000
        );
        // MAC 覆盖域：标签+pubkey+devTag+ts（对 reg[50:66] 重算应一致）
        let mut mac = Hmac::<Sha256>::new_from_slice(&[0x42; 32]).unwrap();
        mac.update(b"hr-reg2");
        mac.update(&id.public_key());
        mac.update(&DevTag([0xAA; 8]).as_bytes()[..]);
        mac.update(&1_800_000_000u64.to_be_bytes());
        assert_eq!(&mac.finalize().into_bytes()[..16], &reg[50..66]);
        let _ = fs::remove_dir_all(&dir);
    }
}
