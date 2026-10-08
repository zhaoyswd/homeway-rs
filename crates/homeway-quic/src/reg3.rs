//! `hr-reg3` 注册帧（M1 设计 §1.3）：**连接绑定**版的 reg（v2 → v3）。
//!
//! 线形态（定长 66B，与 v2 同长）：
//!
//! ```text
//! "H3"(2B) ‖ pubkey(32B) ‖ devTag(8B) ‖ ts(8B BE 秒) ‖ mac(16B)
//! mac = HMAC-SHA256(token secret, "hr-reg3" ‖ pubkey ‖ devTag ‖ ts ‖ exporter32)[:16]
//! exporter32 = TLS exporter（本连接上 export_keying_material(b"hw-quic-reg", b"") 的前 32B）
//! ```
//!
//! **为什么必须做连接绑定（设计门 r12 的 B2，高危）**：M1 的准入不再要求「pubkey 先完成
//! WG 握手」⇒ 仅凭 MAC 的 reg 帧在 ±90s 内被重放（被动窃听者）就能换出一条**可用**的
//! QUIC 隧道；把该连接的 TLS exporter 混进 MAC 后，**同一帧换一条连接就验不过**（回放面
//! 关闭）。代价：`mac` 只能在其所属连接上校验（两端各自在该连接上现算 exporter）。
//!
//! 本模块只做**字节层**（帧/魔数/标签/exporter 标签/常量时间比对）——secret 的持有与
//! 策略（命中哪条 secret、时间窗、吊销、拒绝归因）在消费侧：出口 = `homeway-core` 的
//! token 与设备表。时间窗 **不在本层**：命中后仍走 `DeviceTable::register` 的同一条路径
//! （v2 报文重建），窗口/吊销/判据行语义与今日逐字同。
//!
//! 客户端面（M1 S2-6 接线）用 [`Reg3Frame::encode`]——与出口面 [`Reg3Frame::mac_matches`]
//! 共用同一段 HMAC 输入，防两侧各写一份标签顺序而静默错位。

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// 帧魔数（v2 = `H2`；v3 换标签是**有意**的：旧版本帧不得被当新帧接受）。
pub const MAGIC: [u8; 2] = *b"H3";

/// 帧长：2 + 32 + 8 + 8 + 16（定长——控制流上按此长度切帧）。
pub const LEN: usize = 66;

/// MAC 截断长度（HMAC-SHA256 的左 16B，与 v2 同口径）。
pub const MAC_LEN: usize = 16;

/// MAC 域分离标签（与 v2 的 `hr-reg2` **不同**——两个版本的 MAC 不可互相冒充）。
pub const MAC_LABEL: &[u8] = b"hr-reg3";

/// TLS exporter 的标签（两端必须逐字一致，否则导出材料不同、MAC 恒不匹配）。
pub const EXPORTER_LABEL: &[u8] = b"hw-quic-reg";

/// TLS exporter 取值长度（quinn `export_keying_material` 的 `output`）。
pub const EXPORTER_LEN: usize = 32;

/// MAC 输入的一段（仅供测试/排障确认覆盖域；实现里不物化整串）。
/// `MAC_LABEL ‖ pubkey ‖ devTag ‖ ts(BE 8B) ‖ exporter32`
fn mac_of(
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    dev_tag: &[u8; 8],
    ts: u64,
    exporter32: &[u8; EXPORTER_LEN],
) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC 任意长密钥");
    mac.update(MAC_LABEL);
    mac.update(pubkey);
    mac.update(dev_tag);
    mac.update(&ts.to_be_bytes());
    mac.update(exporter32);
    mac
}

/// 一帧 `hr-reg3`（已解出字段，未验 MAC——`mac` 属帧内字段，验证见 [`Self::mac_matches`]）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Reg3Frame {
    /// 设备 WG 公钥（身份 + 派生地址的输入）。
    pub pubkey: [u8; 32],
    /// 设备标签（设备本地生成、跨连接稳定；绑定表的替换键）。
    pub dev_tag: [u8; 8],
    /// 客户端打的新鲜秒级时间戳（服务端在 ±90s 窗口内校验——由设备表路径承载）。
    pub ts: u64,
    /// `mac_of(…)[:16]`（本帧所属连接上才算得出来）。
    pub mac: [u8; MAC_LEN],
}

impl Reg3Frame {
    /// 解帧（魔数 + 定长校验；不做 MAC——MAC 需 secret，属消费侧）。
    pub fn parse(pkt: &[u8]) -> Option<Self> {
        if pkt.len() != LEN || pkt[..2] != MAGIC {
            return None;
        }
        Some(Self {
            pubkey: pkt[2..34].try_into().expect("长度已判"),
            dev_tag: pkt[34..42].try_into().expect("长度已判"),
            ts: u64::from_be_bytes(pkt[42..50].try_into().expect("长度已判")),
            mac: pkt[50..66].try_into().expect("长度已判"),
        })
    }

    /// MAC 校验（**常量时间**左截断比对；`exporter32` = 本帧所属连接现算的 TLS exporter）。
    ///
    /// 换连接重放 ⇒ exporter 不同 ⇒ 恒 false（这正是本帧存在的理由）；换 secret 同理。
    pub fn mac_matches(&self, secret: &[u8; 32], exporter32: &[u8; EXPORTER_LEN]) -> bool {
        mac_of(secret, &self.pubkey, &self.dev_tag, self.ts, exporter32)
            .verify_truncated_left(&self.mac)
            .is_ok()
    }

    /// 组帧（客户端面）：算出 MAC 左 16B 并拼成 66B。
    pub fn encode(
        secret: &[u8; 32],
        pubkey: &[u8; 32],
        dev_tag: &[u8; 8],
        ts: u64,
        exporter32: &[u8; EXPORTER_LEN],
    ) -> [u8; LEN] {
        let sum = mac_of(secret, pubkey, dev_tag, ts, exporter32).finalize().into_bytes();
        let mut out = [0u8; LEN];
        out[..2].copy_from_slice(&MAGIC);
        out[2..34].copy_from_slice(pubkey);
        out[34..42].copy_from_slice(dev_tag);
        out[42..50].copy_from_slice(&ts.to_be_bytes());
        out[50..66].copy_from_slice(&sum[..MAC_LEN]);
        out
    }
}

impl std::fmt::Debug for Reg3Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // 短指纹纪律（同 `RpkPublicKey`；devTag 亦只出头 4B）
        write!(
            f,
            "Reg3Frame(pub={}… dev={}… ts={} mac={}…)",
            hex4(&self.pubkey),
            hex4(&self.dev_tag),
            self.ts,
            hex4(&self.mac)
        )
    }
}

fn hex4(b: &[u8]) -> String {
    b.iter().take(4).map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = [0x42; 32];
    const EXPORTER_A: [u8; EXPORTER_LEN] = [0xA1; EXPORTER_LEN];
    const EXPORTER_B: [u8; EXPORTER_LEN] = [0xB2; EXPORTER_LEN];

    fn frame(pubkey: [u8; 32], dev: [u8; 8], ts: u64, exporter: &[u8; EXPORTER_LEN]) -> [u8; LEN] {
        Reg3Frame::encode(&SECRET, &pubkey, &dev, ts, exporter)
    }

    /// 布局锚点：定长 66B + 各字段偏移 + 魔数（v2 的 `H2` 必须被拒——版本不得互相冒充）。
    #[test]
    fn layout_is_fixed_and_version_labelled() {
        let pubkey = [3u8; 32];
        let dev = [0xAA; 8];
        let ts = 1_800_000_000u64;
        let pkt = frame(pubkey, dev, ts, &EXPORTER_A);
        assert_eq!(pkt.len(), LEN);
        assert_eq!(&pkt[..2], b"H3");
        assert_eq!(&pkt[2..34], &pubkey);
        assert_eq!(&pkt[34..42], &dev);
        assert_eq!(u64::from_be_bytes(pkt[42..50].try_into().unwrap()), ts);
        // MAC = HMAC(secret, "hr-reg3"‖pubkey‖devTag‖ts‖exporter)[:16]（独立重算）
        let mut mac = Hmac::<Sha256>::new_from_slice(&SECRET).unwrap();
        mac.update(b"hr-reg3");
        mac.update(&pubkey);
        mac.update(&dev);
        mac.update(&ts.to_be_bytes());
        mac.update(&EXPORTER_A);
        assert_eq!(&mac.finalize().into_bytes()[..16], &pkt[50..66]);
        // 帧解析往返
        let f = Reg3Frame::parse(&pkt).expect("本仓组帧必可解");
        assert_eq!(f.pubkey, pubkey);
        assert_eq!(f.dev_tag, dev);
        assert_eq!(f.ts, ts);
        // v2 魔数不得被接受（长度同、魔数不同）
        let mut v2 = pkt;
        v2[..2].copy_from_slice(b"H2");
        assert!(Reg3Frame::parse(&v2).is_none(), "v2 帧不得当 v3 解");
        // 定长：短/长一律拒（防「猜长度」）
        assert!(Reg3Frame::parse(&pkt[..LEN - 1]).is_none());
        let mut long = pkt.to_vec();
        long.push(0);
        assert!(Reg3Frame::parse(&long).is_none());
    }

    /// **判据（S1-3 的核心语义）**：同帧**换连接**（exporter 变）必须验不过；同连接域内
    /// 任意字段被改也验不过；错 secret 同理。
    #[test]
    fn mac_is_bound_to_connection_and_fields() {
        let pubkey = [7u8; 32];
        let dev = [0x11; 8];
        let ts = 1_800_000_000u64;
        let f = Reg3Frame::parse(&frame(pubkey, dev, ts, &EXPORTER_A)).unwrap();
        assert!(f.mac_matches(&SECRET, &EXPORTER_A), "同一连接上应验得过");
        assert!(!f.mac_matches(&SECRET, &EXPORTER_B), "换连接（exporter 变）⇒ 必须失败");
        assert!(!f.mac_matches(&[0x43; 32], &EXPORTER_A), "错 secret ⇒ 失败");
        // 字段任一被篡改：MAC 覆盖域含全部四段
        for mutate in [
            |f: &mut Reg3Frame| f.pubkey[0] ^= 1,
            |f: &mut Reg3Frame| f.dev_tag[0] ^= 1,
            |f: &mut Reg3Frame| f.ts += 1,
            |f: &mut Reg3Frame| f.mac[0] ^= 1,
        ] {
            let mut tampered = f;
            mutate(&mut tampered);
            assert!(
                !tampered.mac_matches(&SECRET, &EXPORTER_A),
                "篡改字段后不得验过：{tampered:?}"
            );
        }
    }

    /// MAC 是**左 16B 截断**（不是右截断也不是全 32B）——用「全 32B 的右 16B」冒充必失败。
    #[test]
    fn mac_is_left_truncated_prefix() {
        let pubkey = [9u8; 32];
        let dev = [0x22; 8];
        let pkt = frame(pubkey, dev, 1_800_000_000, &EXPORTER_A);
        let mut mac = Hmac::<Sha256>::new_from_slice(&SECRET).unwrap();
        mac.update(MAC_LABEL);
        mac.update(&pubkey);
        mac.update(&dev);
        mac.update(&1_800_000_000u64.to_be_bytes());
        mac.update(&EXPORTER_A);
        let sum = mac.finalize().into_bytes();
        let mut wrong = pkt;
        wrong[50..66].copy_from_slice(&sum[16..32]); // 右半冒充
        let f = Reg3Frame::parse(&wrong).unwrap();
        assert!(!f.mac_matches(&SECRET, &EXPORTER_A), "MAC 必须是左截断");
    }
}
