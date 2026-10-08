//! 腿帧（leg frame）线格式：所有 WG/控制流量的统一封装。
//!
//! 语义真源 `baseline:pkg/proto/frame.go`（FIX-91 统一线格式）：
//!
//! ```text
//! 客户端→中继 listener: [0xAA][peerId(8B)]‖腿帧
//! 其余腿（直连双向等）: 腿帧 = [0xBB][type][payload]
//! 容器帧（type=4）: [0xBB][4] + 消息序列 [type(1)][len(2 BE)][payload]*
//! ```
//!
//! type：0=数据（不透明 WG 包）1=控制（hint）2=reg 3=中继控制 4=容器 5=QUIC 载荷（M1）。
//! 接收方 MUST 忽略未知 type 且不中断会话（前向兼容挂在格式上）——`decode_frame`
//! 对未知 type 原样返回 kind 字节，由调用方决定忽略。
//!
//! **kind=5（M1 S1c，设计 §1.6）**：QUIC 报文作为**不透明载荷**走腿（中继 `forward_up`
//! 保 kind 原样透传、`decode_tagged` 只校验魔数 ⇒ **中继零改动**）；出口侧由驱动线程按
//! `FrameKind::Quic` 分流到 QUIC 面（`server/bind.rs` 的 kind 分支 + `Inbound.quic`），
//! 客户端侧包封/剥壳 = S2-7。**唯一硬约束 = 线字节 5**：本条与 `homeway-quic` 的
//! `FRAME_KIND_QUIC`（叶子 crate 按字节复刻，不依赖本 crate）由本文件测试断言一致。
//!
//! 魔数 0xAA/0xBB 与 WG 报文类型（1–4）不冲突。例外（明文非腿）：STUN 与参照点探测。

use sha2::{Digest, Sha256};

pub const FRAME_MAGIC: u8 = 0xBB;
pub const RELAY_TAG_MAGIC: u8 = 0xAA;

/// 帧类型（wire 字节的语义化形态；`from_wire`/`to_wire` 只覆盖已知名，未知值原样透传）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameKind {
    /// 数据（不透明 WG 包）。
    Data,
    /// 控制（hint：对端观察地址线索）。
    Control,
    /// 注册报文（客户端身份入场券）。
    Reg,
    /// 容器（一个数据报携带多条消息——首个握手包 [reg][data] 保 1 RTT）。
    Batch,
    /// QUIC 载荷（M1 S1c；不透明 QUIC 报文——中继原样透传，出口按此分流到 QUIC 面）。
    Quic,
}

impl FrameKind {
    pub fn to_wire(self) -> u8 {
        match self {
            FrameKind::Data => 0,
            FrameKind::Control => 1,
            FrameKind::Reg => 2,
            FrameKind::Batch => 4,
            FrameKind::Quic => 5,
        }
    }
}

impl From<FrameKind> for u8 {
    fn from(k: FrameKind) -> u8 {
        k.to_wire()
    }
}

/// 腿帧编码（`[0xBB][kind][payload]`）。
pub fn encode_frame(kind: impl Into<u8>, payload: &[u8], out: &mut Vec<u8>) {
    out.reserve(2 + payload.len());
    out.push(FRAME_MAGIC);
    out.push(kind.into());
    out.extend_from_slice(payload);
}

/// 腿帧编码的便捷形态（单帧整包）。
pub fn frame_bytes(kind: impl Into<u8>, payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + payload.len());
    encode_frame(kind, payload, &mut v);
    v
}

/// 腿帧借用解析（零拷贝）：`Some((kind 字节, payload 借用))`；魔数不符/过短 = None。
/// 未知 kind 原样返回（Go 同义——前向兼容由调用方忽略）。
pub fn decode_frame(buf: &[u8]) -> Option<(u8, &[u8])> {
    if buf.len() < 2 || buf[0] != FRAME_MAGIC {
        return None;
    }
    Some((buf[1], &buf[2..]))
}

/// 只看帧头 kind（不产生 payload 借用——F2 采纳判定用，避免与 `&mut self` 借用冲突）。
pub fn frame_kind(buf: &[u8]) -> Option<u8> {
    if buf.len() < 2 || buf[0] != FRAME_MAGIC {
        return None;
    }
    Some(buf[1])
}

/// 中继路由键：后端静态公钥的 sha256 前 8 字节（`[0xAA]` 路由头用；R1 不发中继腿，
/// 解码侧保留）。
pub fn relay_id(peer_pubkey: &[u8; 32]) -> [u8; 8] {
    let sum = Sha256::digest(peer_pubkey);
    sum[..8].try_into().expect("sha256 恒 32B")
}

/// `[0xAA][peerId(8B)]‖已编码腿帧`（R1 保留：中继腿属 R2）。
pub fn encode_tagged_frame(relay_id: &[u8; 8], frame: &[u8], out: &mut Vec<u8>) {
    out.reserve(9 + frame.len());
    out.push(RELAY_TAG_MAGIC);
    out.extend_from_slice(relay_id);
    out.extend_from_slice(frame);
}

/// 解析 listener 形态的标签帧（`[0xAA][id8][0xBB][kind][payload]`；≥11B）。
pub fn decode_tagged(buf: &[u8]) -> Option<(&[u8; 8], u8, &[u8])> {
    if buf.len() < 11 || buf[0] != RELAY_TAG_MAGIC || buf[9] != FRAME_MAGIC {
        return None;
    }
    let id: &[u8; 8] = buf[1..9].try_into().expect("切片长度已判 8");
    Some((id, buf[10], &buf[11..]))
}

/// 容器帧编码（首个握手包搭车用）：`[0xBB][4] + [type][len BE u16][payload]*`。
/// 调用方保证每条 payload ≤ 65535（数据面 MTU 远小于此）。
pub fn encode_batch(msgs: &[(u8, &[u8])], out: &mut Vec<u8>) {
    let size = 2 + msgs.iter().map(|(_, p)| 3 + p.len()).sum::<usize>();
    out.reserve(size);
    out.push(FRAME_MAGIC);
    out.push(FrameKind::Batch.to_wire());
    for (kind, payload) in msgs {
        // 长度域是 u16：> 65535 会**静默回绕**（现状不可达——数据面 MTU 远小于此；
        // 热路径不宜返 Result，故用 debug_assert 防御性收口）。
        debug_assert!(
            payload.len() <= u16::MAX as usize,
            "容器帧 payload 超 u16 长度域（会静默回绕）"
        );
        out.push(*kind);
        let len = payload.len() as u16;
        out.extend_from_slice(&len.to_be_bytes());
        out.extend_from_slice(payload);
    }
}

/// 容器帧便捷形态。
pub fn batch_bytes(msgs: &[(u8, &[u8])]) -> Vec<u8> {
    let mut v = Vec::new();
    encode_batch(msgs, &mut v);
    v
}

/// 容器 payload 段借用解析（`decode_frame` 返回的 payload 再拆消息序列）。
/// 空容器/结构越界 = None（Go `ErrFrameMalformed` 同义）。
pub fn decode_batch(payload: &[u8]) -> Option<Vec<(u8, &[u8])>> {
    let mut out = Vec::new();
    let mut rest = payload;
    while !rest.is_empty() {
        if rest.len() < 3 {
            return None;
        }
        let n = usize::from(rest[1]) << 8 | usize::from(rest[2]);
        if 3 + n > rest.len() {
            return None;
        }
        out.push((rest[0], &rest[3..3 + n]));
        rest = &rest[3 + n..];
    }
    if out.is_empty() {
        return None;
    }
    Some(out)
}

// ---- 控制子协议：hint（对端观察地址，不可信线索；payload = [len(2B BE)][addr]）----

/// hint 控制帧整包（`[0xBB][1][len BE][addr]`）。
pub fn hint_bytes(addr: &str) -> Vec<u8> {
    // 长度域是 u16：> 65535 静默回绕（现状不可达——hint 地址串远小于此）。
    debug_assert!(addr.len() <= u16::MAX as usize, "hint 地址超 u16 长度域（会静默回绕）");
    let mut p = Vec::with_capacity(4 + addr.len());
    p.extend_from_slice(&(addr.len() as u16).to_be_bytes());
    p.extend_from_slice(addr.as_bytes());
    frame_bytes(FrameKind::Control, &p)
}

/// hint payload 解析（不含 0xBB/type 头）；长度不符 = None。
pub fn decode_hint_payload(payload: &[u8]) -> Option<&str> {
    if payload.len() < 2 {
        return None;
    }
    let n = usize::from(u16::from_be_bytes([payload[0], payload[1]]));
    if n == 0 || 2 + n != payload.len() {
        return None;
    }
    core::str::from_utf8(&payload[2..2 + n]).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 对拍 baseline pkg/proto/frame_test.go 的形状样例（字节级）。
    #[test]
    fn frame_roundtrip_and_known_bytes() {
        // 腿帧：已知字节形状
        assert_eq!(frame_bytes(FrameKind::Data, b"wg"), vec![0xBB, 0, b'w', b'g']);
        assert_eq!(frame_bytes(FrameKind::Reg, &[0x41; 3]), vec![0xBB, 2, 0x41, 0x41, 0x41]);
        let (kind, payload) = decode_frame(&[0xBB, 1, 1, 2, 3]).unwrap();
        assert_eq!((kind, payload), (1, &[1u8, 2, 3][..]));
        // 非帧/过短
        assert!(decode_frame(&[0xAA, 0, 1]).is_none());
        assert!(decode_frame(&[0xBB]).is_none());
        // 未知 type 原样透传（前向兼容）
        assert_eq!(decode_frame(&[0xBB, 9, 7]).unwrap().0, 9);
    }

    #[test]
    fn batch_roundtrip_and_malformed() {
        let msgs: Vec<(u8, &[u8])> = vec![(2, b"reg"), (0, b"wg-packet")];
        let raw = batch_bytes(&msgs);
        assert_eq!(raw[..2], [0xBB, 4]);
        let (kind, payload) = decode_frame(&raw).unwrap();
        assert_eq!(kind, 4);
        let got = decode_batch(payload).unwrap();
        assert_eq!(got[0], (2, &b"reg"[..]));
        assert_eq!(got[1], (0, &b"wg-packet"[..]));
        // 越界/空容器拒绝
        assert!(decode_batch(&[0, 0xFF, 0]).is_none());
        assert!(decode_batch(&[]).is_none());
        assert!(decode_batch(&[0, 0, 1, b'x', 0]).is_none()); // 第二条头不足
    }

    /// **kind=5 的线字节是跨 crate 契约**（M1 §1.6）：本条 = 真源，`homeway-quic` 按字节
    /// 复刻（叶子 crate 不得依赖本 crate）——两处一旦漂移，经中继的 QUIC 帧就会被当作
    /// 未知 kind 静默丢弃（症状 = 「直连通、经中继不通」），故用一条断言钉住。
    #[test]
    fn quic_kind_wire_byte_matches_island_constant() {
        assert_eq!(FrameKind::Quic.to_wire(), 5, "kind=5（设计 §1.6）");
        assert_eq!(
            FrameKind::Quic.to_wire(),
            homeway_quic::FRAME_KIND_QUIC,
            "与 homeway-quic 的复刻常量必须一致"
        );
        // 与既有 type 不撞（0/1/2/4 各有主）
        for k in [FrameKind::Data, FrameKind::Control, FrameKind::Reg, FrameKind::Batch] {
            assert_ne!(k.to_wire(), FrameKind::Quic.to_wire());
        }
        // 腿帧往返：中继按 kind 原样透传（decode_frame 只解壳）
        let raw = frame_bytes(FrameKind::Quic, b"quic-initial");
        assert_eq!(raw, vec![0xBB, 5, b'q', b'u', b'i', b'c', b'-', b'i', b'n', b'i', b't', b'i', b'a', b'l']);
        assert_eq!(decode_frame(&raw).unwrap().0, FrameKind::Quic.to_wire());
    }

    #[test]
    fn relay_id_is_sha256_prefix() {
        // Go：RelayID = sha256(pubkey)[:8]
        let id = relay_id(&[0u8; 32]);
        let sum = Sha256::digest([0u8; 32]);
        assert_eq!(id, &sum[..8]);
        // 标签帧往返
        let frame = frame_bytes(FrameKind::Data, b"wg");
        let mut tagged = Vec::new();
        encode_tagged_frame(&id, &frame, &mut tagged);
        assert_eq!(tagged[0], 0xAA);
        let (id2, kind, payload) = decode_tagged(&tagged).unwrap();
        assert_eq!(id2, &id[..]);
        assert_eq!((kind, payload), (0, &b"wg"[..]));
        assert!(decode_tagged(&frame).is_none());
    }

    #[test]
    fn hint_roundtrip() {
        let addr = "192.168.3.12:41641";
        let raw = hint_bytes(addr);
        let (kind, payload) = decode_frame(&raw).unwrap();
        assert_eq!(kind, FrameKind::Control.to_wire());
        assert_eq!(decode_hint_payload(payload), Some(addr));
        assert!(decode_hint_payload(&[0, 5, b'x']).is_none()); // 长度不符
        assert!(decode_hint_payload(&[0, 0]).is_none()); // n=0 拒
    }
}
