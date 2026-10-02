//! relaywire —— 中继控制子协议（帧 type=3）的编解码与 MAC 四族。
//!
//! 中立模块：relay 角色（服务端）与 exit 角色（客户端）共用，不挂任何角色模块
//! （R4-design §1.1，评审 ①-6）。语义真源 `baseline:pkg/proto/{relay,relayctl}.go`：
//!
//! ```text
//! 后端(NAT 后) ── 出站注册腿 ──►  中继   注册证明 = X25519 DH 挑战响应
//! 客户端       ── 标签帧 ──────►  中继 ──per-client socket──► 后端注册腿源地址
//! ```
//!
//! 全部消息装在腿上帧里（`[0xBB][3][子类型 ‖ …]`；TCP 控制面剥掉帧头走
//! `[2B BE 长度][消息]` 流分帧——子类型字节在消息内首字节）。接收方未知子类型一律
//! 忽略（前向兼容）。**v2-only（FIX-89）**：Proof 恰 50B（49B 载荷 + 1B 版本 =2），
//! 33B/49B 历史形态拒绝；TCP OK 恒 17B（1B 子类型 + 16B MAC）。

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::wtransport::frame::{decode_frame, encode_frame};

/// 帧类型：中继控制（注册挑战/证明/心跳）。
pub const FRAME_TYPE_RELAY_REG: u8 = 3;

/// 中继控制子类型（`pkg/proto/relay.go`；TCP 面另加 0x10/0x11）。
pub mod sub {
    pub const HELLO: u8 = 0x01; // 后端→中继：pubkey(32)
    pub const CHALLENGE: u8 = 0x02; // 中继→后端：ephPub(32) ‖ nonce(16)
    pub const PROOF: u8 = 0x03; // 后端→中继：nonce(16) ‖ macDH(16) ‖ macPSK(16) ‖ ver(1)
    pub const OK: u8 = 0x04; // 中继→后端：注册成功（UDP 1B；TCP 17B 带 MAC）
    pub const KEEPALIVE: u8 = 0x05; // 后端→中继：保活（无 payload）
    pub const AGAIN: u8 = 0x06; // 中继→后端：腿不在了，请重新注册
    pub const SESSION: u8 = 0x10; // 中继→后端：sid(8BE) ‖ dataPort(2BE) ‖ cookie(16)
    pub const RELEASE: u8 = 0x11; // 中继→后端：sid(8BE)
}

/// 控制协议版本（FIX-89 起 v2-only；Proof 末字节自报，不符即拒）。
pub const RELAY_CTL_VER: u8 = 2;

/// 单条 TCP 控制消息上限（所有子类型都 ≤64B；防恶意长度行撑爆读侧）。
pub const CTL_MAX: usize = 256;

// ---------- MAC 四族（域分隔逐字节对齐 Go） ----------

fn hmac_sha256_trunc16(key: &[u8], parts: &[&[u8]]) -> [u8; 16] {
    let mut mac = <Hmac<Sha256>>::new_from_slice(key).expect("HMAC 接受任意长度密钥");
    for p in parts {
        mac.update(p);
    }
    let out = mac.finalize().into_bytes();
    out[..16].try_into().expect("HMAC-SHA256 恒 32B")
}

/// 注册证明 MAC（DH 为密钥；开放模式的准入证明；TCP 面**恒校**）。
/// `HMAC-SHA256(dh, "hmac-relay" ‖ nonce ‖ pubkey)[:16]`
pub fn proof_mac(dh: &[u8], nonce: &[u8; 16], pubkey: &[u8; 32]) -> [u8; 16] {
    hmac_sha256_trunc16(dh, &[b"hmac-relay", nonce, pubkey])
}

/// token 模式的鉴权 MAC（密钥 = 中继鉴权密钥；UDP 面 token 模式的准入证明）。
/// `HMAC-SHA256(secret, "relay-psk" ‖ nonce ‖ pubkey)[:16]`
pub fn auth_mac(secret: &[u8; 32], nonce: &[u8; 16], pubkey: &[u8; 32]) -> [u8; 16] {
    hmac_sha256_trunc16(secret, &[b"relay-psk", nonce, pubkey])
}

/// TCP OK 消息里的中继身份 MAC（后端认证中继，#29）。
/// `HMAC-SHA256(secret, nonce ‖ "ok")[:16]`
pub fn ok_auth_mac(secret: &[u8; 32], nonce: &[u8; 16]) -> [u8; 16] {
    hmac_sha256_trunc16(secret, &[nonce, b"ok"])
}

/// 拨腿首包的腿认证 MAC（#3；key = token 模式中继密钥 / 开放模式 cookie 本身）。
/// `HMAC-SHA256(key, "legup-v2" ‖ sid(8BE) ‖ cookie)[:16]`
pub fn legup_mac(sid: u64, cookie: &[u8; 16], key: &[u8; 32]) -> [u8; 16] {
    hmac_sha256_trunc16(key, &[b"legup-v2", &sid.to_be_bytes(), cookie])
}

/// 常量时间 16 字节比较（`subtle.ConstantTimeCompare` 等价）。
pub fn ct_eq_16(a: &[u8], b: &[u8]) -> bool {
    a.len() == 16 && b.len() == 16 && crate::server::table::const_time_eq_16(a, b)
}

// ---------- 子协议编解码（载荷形态 = 子类型字节 + 体） ----------

/// Hello：`[01] pub(32)`（33B）。
pub fn encode_hello(pubkey: &[u8; 32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(33);
    v.push(sub::HELLO);
    v.extend_from_slice(pubkey);
    v
}

/// 解析 Hello 载荷；长度/子类型不符 = None。
pub fn decode_hello(p: &[u8]) -> Option<[u8; 32]> {
    if p.len() != 33 || p[0] != sub::HELLO {
        return None;
    }
    Some(p[1..33].try_into().expect("长度已判 33"))
}

/// Challenge：`[02] ephPub(32) ‖ nonce(16)`（49B）。
pub fn encode_challenge(eph_pub: &[u8; 32], nonce: &[u8; 16]) -> Vec<u8> {
    let mut v = Vec::with_capacity(49);
    v.push(sub::CHALLENGE);
    v.extend_from_slice(eph_pub);
    v.extend_from_slice(nonce);
    v
}

/// 解析 Challenge；长度/子类型不符 = None。
pub fn decode_challenge(p: &[u8]) -> Option<([u8; 32], [u8; 16])> {
    if p.len() != 49 || p[0] != sub::CHALLENGE {
        return None;
    }
    let eph: [u8; 32] = p[1..33].try_into().expect("长度已判");
    let nonce: [u8; 16] = p[33..49].try_into().expect("长度已判");
    Some((eph, nonce))
}

/// Proof：`[03] nonce(16) ‖ macDH(16) ‖ macPSK(16) ‖ ver(1)`（50B；**只接受 v2 形状**）。
/// `macPSK = None` = 开放模式（填全零——对端开放模式本就不校验）。
pub fn encode_proof(nonce: &[u8; 16], dh: &[u8], pubkey: &[u8; 32], psk: Option<&[u8; 16]>) -> Vec<u8> {
    let mut v = Vec::with_capacity(50);
    v.push(sub::PROOF);
    v.extend_from_slice(nonce);
    v.extend_from_slice(&proof_mac(dh, nonce, pubkey));
    v.extend_from_slice(psk.unwrap_or(&[0u8; 16]));
    v.push(RELAY_CTL_VER);
    v
}

/// Proof 解析产物（校验留给调用方：它才知道 DH / 鉴权密钥）。
pub struct ProofParts<'a> {
    pub nonce: [u8; 16],
    pub mac_dh: &'a [u8],
    pub mac_psk: &'a [u8],
    pub ver: u8,
}

/// 解析 Proof（只认 50B v2 形状）。
pub fn decode_proof(p: &[u8]) -> Option<ProofParts<'_>> {
    if p.len() != 50 || p[0] != sub::PROOF {
        return None;
    }
    Some(ProofParts {
        nonce: p[1..17].try_into().expect("长度已判"),
        mac_dh: &p[17..33],
        mac_psk: &p[33..49],
        ver: p[49],
    })
}

/// UDP OK（1B）。
pub fn ok_bytes() -> Vec<u8> {
    vec![sub::OK]
}

/// Again（1B）。
pub fn again_bytes() -> Vec<u8> {
    vec![sub::AGAIN]
}

/// Keepalive（1B）。
pub fn keepalive_bytes() -> Vec<u8> {
    vec![sub::KEEPALIVE]
}

/// TCP OK：`[04] MAC(16)`（恒 17B；MAC 不足补零占位——开放模式无密钥可算）。
pub fn encode_ok_auth(mac: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(17);
    v.push(sub::OK);
    v.extend_from_slice(mac);
    v.resize(17, 0);
    v
}

/// 解析 TCP OK（只认 17B v2 形状；`None` = 对端不是 v2 中继）。
pub fn decode_ok_auth(p: &[u8]) -> Option<&[u8]> {
    if p.len() == 17 && p[0] == sub::OK {
        Some(&p[1..])
    } else {
        None
    }
}

/// SESSION 通告（v2 恒带 cookie，27B）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CtlSession {
    /// 中继侧会话号（RELEASE 关联用）。
    pub id: u64,
    /// 该客户端专属数据口（后端拨腿的目标端口）。
    pub data_port: u16,
    /// 每会话随机（只经控制通道发给该后端；拨腿首包回带认证）。
    pub cookie: [u8; 16],
}

/// 组 SESSION 消息（27B）。
pub fn encode_session(s: &CtlSession) -> Vec<u8> {
    let mut v = Vec::with_capacity(27);
    v.push(sub::SESSION);
    v.extend_from_slice(&s.id.to_be_bytes());
    v.extend_from_slice(&s.data_port.to_be_bytes());
    v.extend_from_slice(&s.cookie);
    v
}

/// 解 SESSION（只认 27B v2 形态）。
pub fn decode_session(p: &[u8]) -> Option<CtlSession> {
    if p.len() != 27 || p[0] != sub::SESSION {
        return None;
    }
    Some(CtlSession {
        id: u64::from_be_bytes(p[1..9].try_into().expect("长度已判")),
        data_port: u16::from_be_bytes(p[9..11].try_into().expect("长度已判")),
        cookie: p[11..27].try_into().expect("长度已判"),
    })
}

/// 组 RELEASE 消息（9B）。
pub fn encode_release(id: u64) -> Vec<u8> {
    let mut v = Vec::with_capacity(9);
    v.push(sub::RELEASE);
    v.extend_from_slice(&id.to_be_bytes());
    v
}

/// 解 RELEASE。
pub fn decode_release(p: &[u8]) -> Option<u64> {
    if p.len() != 9 || p[0] != sub::RELEASE {
        return None;
    }
    Some(u64::from_be_bytes(p[1..9].try_into().expect("长度已判")))
}

// ---------- LEGUP（拨腿首包；UDP 数据口上的独立形态，不套腿帧） ----------

const LEGUP_MAGIC: &[u8; 5] = b"LEGUP";
/// v2 拨腿首包全长：`"LEGUP"(5) ‖ cookie(16) ‖ MAC(16)`。
pub const LEGUP_LEN: usize = 37;

/// 组 v2 拨腿首包载荷。
pub fn legup_payload(sid: u64, cookie: &[u8; 16], key: &[u8; 32]) -> Vec<u8> {
    let mut v = Vec::with_capacity(LEGUP_LEN);
    v.extend_from_slice(LEGUP_MAGIC);
    v.extend_from_slice(cookie);
    v.extend_from_slice(&legup_mac(sid, cookie, key));
    v
}

/// 合法形状的 v2 拨腿首包里取 cookie（不校验 MAC；调用方再走 [`verify_legup`]）。
pub fn legup_cookie(pkt: &[u8]) -> Option<[u8; 16]> {
    if pkt.len() != LEGUP_LEN || &pkt[..5] != LEGUP_MAGIC {
        return None;
    }
    Some(pkt[5..21].try_into().expect("长度已判"))
}

/// 完整校验一个 v2 拨腿首包。
pub fn verify_legup(pkt: &[u8], sid: u64, cookie: &[u8; 16], key: &[u8; 32]) -> bool {
    match legup_cookie(pkt) {
        Some(c) if c == *cookie => {}
        _ => return false,
    }
    ct_eq_16(&legup_mac(sid, cookie, key), &pkt[21..37])
}

// ---------- TCP 流分帧 + 腿帧封装 ----------

/// 把一条控制消息（子类型 + 体）封成腿上帧（UDP 注册腿路径用）。
pub fn relay_reg_frame(payload: &[u8]) -> Vec<u8> {
    frame_bytes(payload)
}

fn frame_bytes(payload: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(2 + payload.len());
    encode_frame(FRAME_TYPE_RELAY_REG, payload, &mut v);
    v
}

/// 从腿上帧取出（子类型, 体）——type=3 时子类型 = 载荷首字节。
/// 返回 None = 非 type=3 帧或载荷空。
pub fn decode_relay_reg_frame(frame: &[u8]) -> Option<(u8, &[u8])> {
    let (kind, payload) = decode_frame(frame)?;
    if kind != FRAME_TYPE_RELAY_REG || payload.is_empty() {
        return None;
    }
    Some((payload[0], &payload[1..]))
}

/// TCP 写帧：`[2B BE len][msg]`（len ∈ (0, 256]）。`out` 追加形态（调用方拼批）。
pub fn ctl_frame_into(msg: &[u8], out: &mut Vec<u8>) {
    debug_assert!((1..=CTL_MAX).contains(&msg.len()), "控制消息长度门在调用方保证");
    let len = msg.len() as u16;
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(msg);
}

/// 长度行非法（0 或 >256）——调用方断连（typed error，AGENTS 工程原则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("控制面长度行非法")]
pub struct CtlFrameError;

/// TCP 增量读侧的半帧状态机（驱动线程对每个已建立控制连接各持一份；非阻塞 fd
/// 上反复 feed，产出完整消息——R4-design §4.1「四件套」之二）。
#[derive(Default)]
pub struct CtlDecoder {
    buf: Vec<u8>,
}

impl CtlDecoder {
    pub fn new() -> Self {
        Self { buf: Vec::with_capacity(CTL_MAX + 2) }
    }

    /// feed 一段新读到的字节，弹出所有完整消息（子类型, 体）。
    /// `Err(CtlFrameError)` = 长度行非法（0 或 >256）——调用方断连。
    pub fn feed(&mut self, chunk: &[u8], out: &mut Vec<(u8, Vec<u8>)>) -> Result<(), CtlFrameError> {
        self.buf.extend_from_slice(chunk);
        loop {
            if self.buf.len() < 2 {
                return Ok(());
            }
            let n = usize::from(u16::from_be_bytes([self.buf[0], self.buf[1]]));
            if n == 0 || n > CTL_MAX {
                return Err(CtlFrameError);
            }
            if self.buf.len() < 2 + n {
                return Ok(());
            }
            let msg: Vec<u8> = self.buf[2..2 + n].to_vec();
            self.buf.drain(..2 + n);
            out.push((msg[0], msg[1..].to_vec()));
        }
    }
}

/// 一条消息的可读形态（判据日志用；`CtlMsgString` 同义）。
pub fn ctl_msg_string(subtype: u8, payload: &[u8]) -> String {
    // 拼回「子类型 + 体」整条再解（decode_* 以整条为对象）。
    let mut whole = Vec::with_capacity(1 + payload.len());
    whole.push(subtype);
    whole.extend_from_slice(payload);
    match subtype {
        s if s == sub::HELLO => "HELLO".to_owned(),
        s if s == sub::CHALLENGE => "CHALLENGE".to_owned(),
        s if s == sub::PROOF => "PROOF".to_owned(),
        s if s == sub::OK => "OK".to_owned(),
        s if s == sub::KEEPALIVE => "KEEPALIVE".to_owned(),
        s if s == sub::SESSION => match decode_session(&whole) {
            Some(s) => format!("SESSION id={} port={}", s.id, s.data_port),
            None => format!("type=0x{s:02x} len={}", payload.len()),
        },
        s if s == sub::RELEASE => match decode_release(&whole) {
            Some(id) => format!("RELEASE id={id}"),
            None => format!("type=0x{s:02x} len={}", payload.len()),
        },
        _ => format!("type=0x{subtype:02x} len={}", payload.len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PUB: [u8; 32] = [0x11u8; 32];
    const EPH: [u8; 32] = [0x22u8; 32];
    const NONCE: [u8; 16] = [0x33u8; 16];
    const SECRET: [u8; 32] = [0x44u8; 32];

    #[test]
    fn hello_challenge_proof_shapes() {
        assert_eq!(encode_hello(&PUB), {
            let mut v = vec![0x01];
            v.extend_from_slice(&PUB);
            v
        });
        assert_eq!(decode_hello(&encode_hello(&PUB)), Some(PUB));
        // 长度门：33B 少一字节 / 子类型错
        assert!(decode_hello(&encode_hello(&PUB)[..32]).is_none());
        assert!(decode_hello(&encode_challenge(&EPH, &NONCE)).is_none());

        let ch = encode_challenge(&EPH, &NONCE);
        assert_eq!(ch.len(), 49);
        assert_eq!(decode_challenge(&ch), Some((EPH, NONCE)));
        assert!(decode_challenge(&ch[..48]).is_none());

        let dh = [0x55u8; 32];
        let psk = auth_mac(&SECRET, &NONCE, &PUB);
        let pr = encode_proof(&NONCE, &dh, &PUB, Some(&psk));
        assert_eq!(pr.len(), 50);
        assert_eq!(pr[0], 0x03);
        assert_eq!(pr[49], RELAY_CTL_VER);
        let parts = decode_proof(&pr).unwrap();
        assert_eq!(parts.nonce, NONCE);
        assert_eq!(parts.mac_dh, &proof_mac(&dh, &NONCE, &PUB)[..]);
        assert_eq!(parts.mac_psk, &psk[..]);
        // 49B 历史形态（无版本字节）拒绝
        assert!(decode_proof(&pr[..49]).is_none());
        // 开放模式：macPSK 全零
        let pr_open = encode_proof(&NONCE, &dh, &PUB, None);
        assert_eq!(&pr_open[33..49], &[0u8; 16][..]);
    }

    #[test]
    fn mac_families_domain_separated() {
        // 同输入不同域分隔 → 不同 MAC（防两族互替）
        let a = proof_mac(&SECRET, &NONCE, &PUB);
        let b = auth_mac(&SECRET, &NONCE, &PUB);
        assert_ne!(a, b);
        // ok_auth_mac / legup_mac 形状
        assert_eq!(ok_auth_mac(&SECRET, &NONCE).len(), 16);
        let c = [0x66u8; 16];
        assert_eq!(legup_mac(7, &c, &SECRET).len(), 16);
        assert_ne!(legup_mac(7, &c, &SECRET), legup_mac(8, &c, &SECRET));
        // 常量时间比较
        assert!(ct_eq_16(&a, &a));
        assert!(!ct_eq_16(&a, &b));
        assert!(!ct_eq_16(&a, &[0u8; 15]));
    }

    #[test]
    fn ok_session_release_shapes() {
        assert_eq!(ok_bytes(), vec![0x04]);
        assert_eq!(again_bytes(), vec![0x06]);
        assert_eq!(keepalive_bytes(), vec![0x05]);

        let ok = encode_ok_auth(&ok_auth_mac(&SECRET, &NONCE));
        assert_eq!(ok.len(), 17);
        assert_eq!(decode_ok_auth(&ok), Some(&ok_auth_mac(&SECRET, &NONCE)[..]));
        // 裸 1B 历史形态拒绝
        assert!(decode_ok_auth(&ok_bytes()).is_none());
        // MAC 不足补零占位
        assert_eq!(encode_ok_auth(&[1, 2, 3]).len(), 17);

        let s = CtlSession { id: 42, data_port: 51000, cookie: [7u8; 16] };
        let m = encode_session(&s);
        assert_eq!(m.len(), 27);
        assert_eq!(decode_session(&m), Some(s));
        assert!(decode_session(&m[..26]).is_none()); // 11B 无 cookie 历史形态拒绝

        let r = encode_release(99);
        assert_eq!(r.len(), 9);
        assert_eq!(decode_release(&r), Some(99));
        assert!(decode_release(&r[..8]).is_none());
    }

    #[test]
    fn legup_roundtrip_and_rejects() {
        let cookie = [0x77u8; 16];
        let pkt = legup_payload(5, &cookie, &SECRET);
        assert_eq!(pkt.len(), LEGUP_LEN);
        assert_eq!(&pkt[..5], b"LEGUP");
        assert_eq!(legup_cookie(&pkt), Some(cookie));
        assert!(verify_legup(&pkt, 5, &cookie, &SECRET));
        // sid/cookie/key 任一错 → 拒
        assert!(!verify_legup(&pkt, 6, &cookie, &SECRET));
        assert!(!verify_legup(&pkt, 5, &[0u8; 16], &SECRET));
        assert!(!verify_legup(&pkt, 5, &cookie, &[0u8; 32]));
        // 5B v1 纯标记形态：不认（v2-only）
        assert!(legup_cookie(b"LEGUP").is_none());
        // 畸形
        assert!(legup_cookie(b"LEGUX00").is_none());
    }

    #[test]
    fn relay_reg_frame_roundtrip() {
        let f = relay_reg_frame(&encode_hello(&PUB));
        assert_eq!(&f[..2], &[0xBB, 3][..]);
        let (sub, body) = decode_relay_reg_frame(&f).unwrap();
        assert_eq!(sub, sub::HELLO);
        assert_eq!(body, &PUB[..]);
        // 非 type=3 / 空载荷
        assert!(decode_relay_reg_frame(&crate::wtransport::frame::frame_bytes(crate::wtransport::frame::FrameKind::Data, b"x")).is_none());
        assert!(decode_relay_reg_frame(&frame_bytes(&[])).is_none());
    }

    #[test]
    fn ctl_stream_decoder_half_frames() {
        let mut d = CtlDecoder::new();
        let mut wire = Vec::new();
        ctl_frame_into(&keepalive_bytes(), &mut wire);
        ctl_frame_into(&encode_session(&CtlSession { id: 1, data_port: 2, cookie: [0u8; 16] }), &mut wire);
        ctl_frame_into(&encode_release(3), &mut wire);
        // 逐字节喂（最碎形态）——半帧状态机必须重组出全部三条
        let mut got = Vec::new();
        for b in &wire {
            d.feed(&[*b], &mut got).unwrap();
        }
        assert_eq!(got.len(), 3);
        assert_eq!(got[0], (sub::KEEPALIVE, vec![]));
        assert_eq!(decode_session(&{
            let mut w = vec![got[1].0];
            w.extend_from_slice(&got[1].1);
            w
        })
        .map(|s| (s.id, s.data_port)), Some((1, 2)));
        assert_eq!(decode_release(&{
            let mut w = vec![got[2].0];
            w.extend_from_slice(&got[2].1);
            w
        }), Some(3));
        // 长度行非法（0 / >256）→ Err
        let mut bad = CtlDecoder::new();
        let mut out = Vec::new();
        assert_eq!(bad.feed(&[0, 0], &mut out), Err(CtlFrameError));
        let mut bad2 = CtlDecoder::new();
        assert_eq!(bad2.feed(&[1, 1], &mut out), Err(CtlFrameError)); // 257 > 256
    }

    #[test]
    fn ctl_msg_string_forms() {
        assert_eq!(ctl_msg_string(sub::HELLO, &[]), "HELLO");
        let s = encode_session(&CtlSession { id: 9, data_port: 40000, cookie: [0u8; 16] });
        assert_eq!(ctl_msg_string(s[0], &s[1..]), "SESSION id=9 port=40000");
        let r = encode_release(4);
        assert_eq!(ctl_msg_string(r[0], &r[1..]), "RELEASE id=4");
        assert_eq!(ctl_msg_string(0x7f, &[1, 2]), "type=0x7f len=2");
    }
}
