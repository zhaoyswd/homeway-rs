//! 隧道地址派生（客户端与服务端各自独立算出同一地址，无额外往返）。
//!
//! 语义真源 `baseline:pkg/proto/tunneladdr.go`（对照向量 `fixtures/vectors/tunnel_addr.json`）：
//!
//! - `tunnel_ip = 100.64.<HMAC-SHA256(secret, "hw-tun" ‖ pubkey)[:2] 映射 v∈[1,65534]>`
//!   ——隧道侧（栈 B / WG peer 的 allowed_ip）地址，每设备唯一（wireguard 全局前缀表
//!   下同 /32 只能属一个 peer）；
//! - `tun_ip` 同型用 `"hw-app"` 取 sum[2:4]——应用面（TUN）第二派生地址；与
//!   `tunnel_ip` 撞车时按 `hw-app.2..9` 标签再散列（守卫路径，两端必须同规则）。
//! - 地址空间 `100.64.0.1 – 100.64.255.254`（避开网段 .0.0 与广播 .255.255）。

use std::net::Ipv4Addr;

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::token::Secret;

type HmacSha256 = Hmac<Sha256>;

fn hmac_sum(key: &[u8; 32], label: &[u8], pubkey: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC 接受任意长密钥，不可达");
    mac.update(label);
    mac.update(pubkey);
    mac.finalize().into_bytes().into()
}

/// HMAC 摘要两个字节 → v∈[1,65534]（Go：`(sum[a]<<8|sum[b]) % 65534 + 1`）。
#[inline]
fn to_v(sum: &[u8; 32], at: usize) -> u16 {
    let be = (u16::from(sum[at]) << 8) | u16::from(sum[at + 1]);
    be % 65534 + 1
}

#[inline]
fn addr_of(v: u16) -> Ipv4Addr {
    Ipv4Addr::new(100, 64, (v >> 8) as u8, v as u8)
}

/// 隧道侧地址（栈 B 本地地址 = 后端 peer allowed_ip 的第一条）。
pub fn derive_tunnel_ip(secret: &Secret, pubkey: &[u8; 32]) -> Ipv4Addr {
    let sum = hmac_sum(secret.as_bytes(), b"hw-tun", pubkey);
    addr_of(to_v(&sum, 0))
}

/// 应用面（TUN）第二派生地址；与隧道地址同设备相等时按 `hw-app.N`（N=2..=9）再散列。
///
/// 守卫语义与 Go 循环逐拍对齐：`while i<=9 && ip == tunnel_ip { 用 hw-app.{i} 再散列; i+=1 }`
/// ——真撞满 8 次属 2^-256 量级（「身份推导本身坏了」），接受最后结果。
pub fn derive_tun_ip(secret: &Secret, pubkey: &[u8; 32]) -> Ipv4Addr {
    let tunnel = derive_tunnel_ip(secret, pubkey);
    let mut ip = addr_of(to_v(&hmac_sum(secret.as_bytes(), b"hw-app", pubkey), 2));
    let mut i = 2u8;
    while i <= 9 && ip == tunnel {
        let label = format!("hw-app.{i}");
        ip = addr_of(to_v(
            &hmac_sum(secret.as_bytes(), label.as_bytes(), pubkey),
            2,
        ));
        i += 1;
    }
    ip
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h32(s: &str) -> [u8; 32] {
        let mut v = [0u8; 32];
        hex::decode_to_slice(s, &mut v).unwrap();
        v
    }

    /// 吃 Go 真源向量（fixtures/vectors/tunnel_addr.json，含守卫命中样本）。
    #[test]
    fn addr_matches_go_vectors() {
        let cases: &[(&str, &str, &str, &str)] = &[
            // (secret, pubkey, tunnel_ip, tun_ip)
            (
                "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
                "d5cae8cf000000000000000000000000000000000000000000000000000000ff",
                "100.64.132.190",
                "100.64.181.212",
            ),
            (
                "deadbeefcafebabe000000000000000000000000000000000000000000000001",
                "1111111111111111111111111111111111111111111111111111111111111111",
                "100.64.216.183",
                "100.64.32.62",
            ),
            (
                // 守卫路径：hw-app 与 hw-tun 撞 v → hw-app.2.. 再散列
                "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
                "0000807800000000000000000000000000000000000000000000000000000000",
                "100.64.140.211",
                "100.64.187.180",
            ),
        ];
        for (secret, pubkey, tunnel, tun) in cases {
            let sec = Secret::from(h32(secret));
            let pk = h32(pubkey);
            assert_eq!(derive_tunnel_ip(&sec, &pk).to_string(), *tunnel);
            assert_eq!(derive_tun_ip(&sec, &pk).to_string(), *tun);
        }
    }

    /// 守卫自证：对守卫命中样本，hw-app 原始派生与 tunnel 相等、再散列后脱开
    /// （防止向量断言退化成「同函数两端比相等」的恒真）。
    #[test]
    fn guard_case_actually_collides_before_rehash() {
        let sec = Secret::from(h32(
            "0102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f20",
        ));
        let pk = h32("0000807800000000000000000000000000000000000000000000000000000000");
        let raw_app = addr_of(to_v(
            &hmac_sum(sec.as_bytes(), b"hw-app", &pk),
            2,
        ));
        assert_eq!(raw_app, derive_tunnel_ip(&sec, &pk), "样本必须先撞车");
        assert_ne!(
            derive_tun_ip(&sec, &pk),
            derive_tunnel_ip(&sec, &pk),
            "再散列后必须脱开"
        );
    }
}
