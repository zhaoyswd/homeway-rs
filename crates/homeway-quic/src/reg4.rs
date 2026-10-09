//! `hr-reg4` 客户端证明协议**帧层**（M2 设计 §1.2）：四帧定长 + 刷新帧（**替换** M1 的
//! `hr-reg3` 单帧，`H3` 起被拒）。
//!
//! 线形态（全定长；控制流按「读 2B 魔数 → 定长收全帧」切分）：
//!
//! ```text
//! Hello      客户端 → 出口                              50B
//!   "H4"(2) ‖ pubkey(32) ‖ devTag(8) ‖ ts(8 BE 秒)
//! Challenge  出口 → 客户端                              18B
//!   "C4"(2) ‖ nonce(16)
//! Proof      客户端 → 出口（回显 Hello 四字段 + nonce）   82B
//!   "P4"(2) ‖ pubkey(32) ‖ devTag(8) ‖ ts(8) ‖ nonce(16) ‖ mac(16)
//!   mac = HMAC-SHA256(secret, "hr-reg4" ‖ pubkey ‖ devTag ‖ ts ‖ nonce ‖ exporter32)[:16]
//! Accept     出口 → 客户端（绑定建立之后立刻写）           2B
//!   "A4"(2)
//! Refresh    客户端 → 出口（60s 节拍；同一控制流）         66B
//!   "R4"(2) ‖ pubkey(32) ‖ devTag(8) ‖ ts(8) ‖ mac(16)
//!   mac = HMAC-SHA256(secret, "hr-reg4-refresh" ‖ pubkey ‖ devTag ‖ ts ‖ exporter32)[:16]
//! ```
//!
//! 三处不变量（各自有单测）：
//!
//! - **版本互斥**：`H2`/`H3` 的字节**不得**被本层解析（[`FrameHead::Legacy`] 只做**可辨
//!   归因**，不进任何解析分支）——设计 §1.1 的「替换不并存」+ 设计门 r14 F4（归因可辨）；
//! - **域分隔**：准入 Proof 与刷新帧的 MAC 标签不同（`hr-reg4` ≠ `hr-reg4-refresh`）
//!   ⇒ 两帧**不可互相冒充**（防「用刷新帧完成准入」与「用 Proof 当刷新」两类跨域重放）；
//! - **连接绑定**：`exporter32`（本连接现算的 TLS exporter）入 MAC ⇒ 同帧**换连接重放**
//!   恒失败（M1 已落，M2 原样保留；这是本帧族存在的理由之一）。
//!
//! 本模块只做**字节层**（帧/魔数/长度/MAC/常量时间比对/域标签）——secret 的持有、时间窗、
//! 吊销、判据行与拒绝计数全部在消费侧（出口面 `exit::conn` + 引擎侧 `server::engine`）。
//! 时间窗仍由 `DeviceTable::register` 承载（重建 v2 报文的手法原样保留）。
//!
//! **nonce 的定位（设计门 r14 F6/F23 的两处措辞收窄，实现期不得夸大）**：它是出口自选的
//! **新鲜值 + 一次性令牌**（同连接内 Proof 复用被关闭 + 给未认证连接一个显式生命周期），
//! 而**不是**抗重放的主防线——换连接重放由 exporter 绑定承担，未持 secret 者本就过不了 MAC。

use std::fmt;

use hmac::{Hmac, Mac};
use sha2::Sha256;

/// Hello 魔数（v3 = `H3`；**换魔数是有意的**：旧版本帧不得被当新帧接受）。
pub const HELLO_MAGIC: [u8; 2] = *b"H4";
/// Challenge 魔数。
pub const CHALLENGE_MAGIC: [u8; 2] = *b"C4";
/// Proof 魔数。
pub const PROOF_MAGIC: [u8; 2] = *b"P4";
/// Accept 魔数。
pub const ACCEPT_MAGIC: [u8; 2] = *b"A4";
/// 刷新帧魔数。
pub const REFRESH_MAGIC: [u8; 2] = *b"R4";

/// 旧版本魔数（**只做可辨归因**：`H2` = v2 的 `hr-reg2`，`H3` = M1 的 `hr-reg3`）。
pub const LEGACY_MAGICS: [[u8; 2]; 2] = [*b"H2", *b"H3"];

/// Hello 帧长：2 + 32 + 8 + 8。
pub const HELLO_LEN: usize = 50;
/// Challenge 帧长：2 + 16。
pub const CHALLENGE_LEN: usize = 18;
/// Proof 帧长：2 + 32 + 8 + 8 + 16 + 16。
pub const PROOF_LEN: usize = 82;
/// Accept 帧长：2。
pub const ACCEPT_LEN: usize = 2;
/// 刷新帧长：2 + 32 + 8 + 8 + 16。
pub const REFRESH_LEN: usize = 66;

/// nonce 长度（16B = 仓内既有惯用法；**不是密钥材料**，是新鲜值 + 一次性令牌）。
pub const NONCE_LEN: usize = 16;
/// MAC 截断长度（HMAC-SHA256 的左 16B，与 v2/v3 同口径）。
pub const MAC_LEN: usize = 16;

/// 准入 Proof 的 MAC 域标签。
pub const MAC_LABEL: &[u8] = b"hr-reg4";
/// 刷新帧的 MAC 域标签（**与准入不同** ⇒ 两帧不可互相冒充）。
pub const REFRESH_MAC_LABEL: &[u8] = b"hr-reg4-refresh";

/// TLS exporter 的标签（两端必须逐字一致，否则导出材料不同、MAC 恒不匹配）。
///
/// 它**沿用 M1 的取值**（M1 `reg3.rs` 的同名常量，逐字节相同）：这是「连接绑定值」的域分隔标签，
/// **不是帧版本号**——版本由 `H3→H4` / `hr-reg3→hr-reg4` 承载（设计 §1.2 明列）。
pub const EXPORTER_LABEL: &[u8] = b"hw-quic-reg";
/// TLS exporter 取值长度（quinn `export_keying_material` 的 `output`）。
pub const EXPORTER_LEN: usize = 32;

/// 控制流首 2B 的**帧头判别**（含旧版本与未知魔数的**可辨**分支）。
///
/// `#[non_exhaustive]`：判别集只许在字节层生长（消费侧必须留通配臂）。
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameHead {
    /// `H4`：准入 Hello（客户端 → 出口）。
    Hello,
    /// `C4`：挑战（出口 → 客户端）。
    Challenge,
    /// `P4`：准入 Proof（客户端 → 出口）。
    Proof,
    /// `A4`：准入回执（出口 → 客户端）。
    Accept,
    /// `R4`：刷新帧（客户端 → 出口；已绑定连接）。
    Refresh,
    /// 旧版本帧（`H2`/`H3`）——**拒不解析**，但归因可辨（设计门 r14 F4：灰度期排障要用）。
    Legacy([u8; 2]),
    /// 未知魔数（垃圾包）。
    Unknown([u8; 2]),
}

impl FrameHead {
    /// 按首 2B 判别。
    pub fn of(head: [u8; 2]) -> Self {
        match &head {
            b"H4" => Self::Hello,
            b"C4" => Self::Challenge,
            b"P4" => Self::Proof,
            b"A4" => Self::Accept,
            b"R4" => Self::Refresh,
            // 旧版本名单只此一份（测试断言 `of()` 与该名单一致，防两处漂移）
            _ if LEGACY_MAGICS.contains(&head) => Self::Legacy(head),
            _ => Self::Unknown(head),
        }
    }

    /// 帧总长（定长切帧的唯一来源）；`None` = 不可解（旧版本/未知魔数——不得进解析）。
    pub fn len(self) -> Option<usize> {
        match self {
            Self::Hello => Some(HELLO_LEN),
            Self::Challenge => Some(CHALLENGE_LEN),
            Self::Proof => Some(PROOF_LEN),
            Self::Accept => Some(ACCEPT_LEN),
            Self::Refresh => Some(REFRESH_LEN),
            Self::Legacy(_) | Self::Unknown(_) => None,
        }
    }

    /// 「非本协议帧」的归因文案（判据行 `why` 的源；**只有这两支不是合法帧**）。
    ///
    /// 旧版本**单列**：`H2`/`H3` 是「旧核或灰度期混装」，与随机垃圾包的排障动作不同。
    pub fn legacy_why(self) -> Option<&'static str> {
        match self {
            Self::Legacy(_) => Some("帧版本不符（H2/H3——旧核或垃圾包）"),
            Self::Unknown(_) => Some("帧格式非法（魔数/长度）"),
            _ => None,
        }
    }

    /// 是否为出口侧**接受**的入站帧（Hello/Proof/Refresh——其余都是「客户端不该发」的）。
    pub fn is_client_inbound(self) -> bool {
        matches!(self, Self::Hello | Self::Proof | Self::Refresh)
    }
}

impl fmt::Debug for FrameHead {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Hello => f.write_str("Hello(H4)"),
            Self::Challenge => f.write_str("Challenge(C4)"),
            Self::Proof => f.write_str("Proof(P4)"),
            Self::Accept => f.write_str("Accept(A4)"),
            Self::Refresh => f.write_str("Refresh(R4)"),
            Self::Legacy(m) => write!(f, "Legacy({})", String::from_utf8_lossy(m)),
            Self::Unknown(m) => write!(f, "Unknown({})", hex4(m)),
        }
    }
}

/// 出口自选的新鲜值（16B CSPRNG；一次性令牌）。
///
/// **Debug 不打印字节**：它是准入期的活跃材料，只许出现在「有/无」语义里（设计 §1.3 的
/// 「nonce 不落盘」纪律延伸到日志面）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Nonce([u8; NONCE_LEN]);

/// nonce 生成失败（系统随机源不可用——**fail-closed**：宁可不发挑战，不降级成弱新鲜值）。
#[derive(Debug, thiserror::Error)]
#[error("系统随机源不可用（{0}）")]
pub struct NonceErr(#[from] getrandom::Error);

impl Nonce {
    /// 取 16B CSPRNG（设计 §1.3：`getrandom`，workspace 既有依赖）。
    pub fn generate() -> Result<Self, NonceErr> {
        let mut b = [0u8; NONCE_LEN];
        getrandom::getrandom(&mut b)?;
        Ok(Self(b))
    }

    /// 由字节重建（解析/测试面）。
    pub fn from_bytes(b: [u8; NONCE_LEN]) -> Self {
        Self(b)
    }

    /// 裸字节（组帧/比对）。
    pub fn as_bytes(&self) -> &[u8; NONCE_LEN] {
        &self.0
    }

    /// 常量时间相等（一次性令牌的比对面；两侧都是 16B 定长）。
    pub fn ct_eq(&self, other: &Nonce) -> bool {
        ct_eq_16(&self.0, &other.0)
    }
}

impl fmt::Debug for Nonce {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Nonce(…)") // 有意不打印字节（见类型文档）
    }
}

/// Hello：客户端开场的三字段帧（不含 MAC——本帧不是凭证，只是「我要证明」）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct HelloFrame {
    /// 设备 WG 公钥（身份 + 派生地址的输入）。
    pub pubkey: [u8; 32],
    /// 设备标签（设备本地生成、跨连接稳定；绑定表的替换键）。
    pub dev_tag: [u8; 8],
    /// 客户端打的新鲜秒级时间戳（服务端在 ±90s 窗口内校验——由设备表路径承载）。
    pub ts: u64,
}

impl HelloFrame {
    /// 解帧（魔数 + 定长校验）。
    pub fn parse(pkt: &[u8]) -> Option<Self> {
        if pkt.len() != HELLO_LEN || pkt[..2] != HELLO_MAGIC {
            return None;
        }
        Some(Self {
            pubkey: pkt[2..34].try_into().expect("长度已判"),
            dev_tag: pkt[34..42].try_into().expect("长度已判"),
            ts: u64::from_be_bytes(pkt[42..50].try_into().expect("长度已判")),
        })
    }

    /// 组帧。
    pub fn encode(pubkey: &[u8; 32], dev_tag: &[u8; 8], ts: u64) -> [u8; HELLO_LEN] {
        let mut out = [0u8; HELLO_LEN];
        out[..2].copy_from_slice(&HELLO_MAGIC);
        out[2..34].copy_from_slice(pubkey);
        out[34..42].copy_from_slice(dev_tag);
        out[42..50].copy_from_slice(&ts.to_be_bytes());
        out
    }
}

impl fmt::Debug for HelloFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HelloFrame(pub={}… dev={}… ts={})", hex4(&self.pubkey), hex4(&self.dev_tag), self.ts)
    }
}

/// Challenge：出口自选 nonce（18B；本帧是**出口 → 客户端**的唯一准入期写）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ChallengeFrame {
    /// 本连接专用的新鲜值（一次性）。
    pub nonce: Nonce,
}

impl ChallengeFrame {
    /// 解帧。
    pub fn parse(pkt: &[u8]) -> Option<Self> {
        if pkt.len() != CHALLENGE_LEN || pkt[..2] != CHALLENGE_MAGIC {
            return None;
        }
        Some(Self {
            nonce: Nonce::from_bytes(pkt[2..18].try_into().expect("长度已判")),
        })
    }

    /// 组帧。
    pub fn encode(nonce: &Nonce) -> [u8; CHALLENGE_LEN] {
        let mut out = [0u8; CHALLENGE_LEN];
        out[..2].copy_from_slice(&CHALLENGE_MAGIC);
        out[2..18].copy_from_slice(nonce.as_bytes());
        out
    }
}

impl fmt::Debug for ChallengeFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ChallengeFrame({:?})", self.nonce)
    }
}

/// 准入 Proof：回显 Hello 四字段 + nonce，带 `hr-reg4` 域 MAC。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ProofFrame {
    /// 设备 WG 公钥（回显 Hello）。
    pub pubkey: [u8; 32],
    /// 设备标签（回显 Hello）。
    pub dev_tag: [u8; 8],
    /// 客户端时间戳（回显 Hello；表内 ±90s 窗的输入）。
    pub ts: u64,
    /// 出口发出的 nonce（回显 Challenge）。
    pub nonce: Nonce,
    /// `mac_of(…)[:16]`（本帧所属连接上才算得出来）。
    pub mac: [u8; MAC_LEN],
}

impl ProofFrame {
    /// 解帧（魔数 + 定长；不做 MAC——MAC 需 secret，属消费侧）。
    pub fn parse(pkt: &[u8]) -> Option<Self> {
        if pkt.len() != PROOF_LEN || pkt[..2] != PROOF_MAGIC {
            return None;
        }
        Some(Self {
            pubkey: pkt[2..34].try_into().expect("长度已判"),
            dev_tag: pkt[34..42].try_into().expect("长度已判"),
            ts: u64::from_be_bytes(pkt[42..50].try_into().expect("长度已判")),
            nonce: Nonce::from_bytes(pkt[50..66].try_into().expect("长度已判")),
            mac: pkt[66..82].try_into().expect("长度已判"),
        })
    }

    /// MAC 校验（**常量时间**左截断比对；`exporter32` = 本帧所属连接现算的 exporter）。
    pub fn mac_matches(&self, secret: &[u8; 32], exporter32: &[u8; EXPORTER_LEN]) -> bool {
        proof_mac(secret, &self.pubkey, &self.dev_tag, self.ts, &self.nonce, exporter32)
            .verify_truncated_left(&self.mac)
            .is_ok()
    }

    /// 组帧（客户端面）。
    pub fn encode(
        secret: &[u8; 32],
        pubkey: &[u8; 32],
        dev_tag: &[u8; 8],
        ts: u64,
        nonce: &Nonce,
        exporter32: &[u8; EXPORTER_LEN],
    ) -> [u8; PROOF_LEN] {
        let sum = proof_mac(secret, pubkey, dev_tag, ts, nonce, exporter32)
            .finalize()
            .into_bytes();
        let mut out = [0u8; PROOF_LEN];
        out[..2].copy_from_slice(&PROOF_MAGIC);
        out[2..34].copy_from_slice(pubkey);
        out[34..42].copy_from_slice(dev_tag);
        out[42..50].copy_from_slice(&ts.to_be_bytes());
        out[50..66].copy_from_slice(nonce.as_bytes());
        out[66..82].copy_from_slice(&sum[..MAC_LEN]);
        out
    }
}

impl fmt::Debug for ProofFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "ProofFrame(pub={}… dev={}… ts={} {:?} mac={}…)",
            hex4(&self.pubkey),
            hex4(&self.dev_tag),
            self.ts,
            self.nonce,
            hex4(&self.mac)
        )
    }
}

/// 刷新帧：已绑定连接上的 60s 节拍（无 nonce、无挑战；域标签独立）。
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RefreshFrame {
    /// 设备 WG 公钥（须与绑定一致）。
    pub pubkey: [u8; 32],
    /// 设备标签（须与绑定一致）。
    pub dev_tag: [u8; 8],
    /// 客户端时间戳（表内 ±90s 窗的输入）。
    pub ts: u64,
    /// `HMAC(secret, "hr-reg4-refresh" ‖ pubkey ‖ devTag ‖ ts ‖ exporter32)[:16]`。
    pub mac: [u8; MAC_LEN],
}

impl RefreshFrame {
    /// 解帧（魔数 + 定长）。
    pub fn parse(pkt: &[u8]) -> Option<Self> {
        if pkt.len() != REFRESH_LEN || pkt[..2] != REFRESH_MAGIC {
            return None;
        }
        Some(Self {
            pubkey: pkt[2..34].try_into().expect("长度已判"),
            dev_tag: pkt[34..42].try_into().expect("长度已判"),
            ts: u64::from_be_bytes(pkt[42..50].try_into().expect("长度已判")),
            mac: pkt[50..66].try_into().expect("长度已判"),
        })
    }

    /// MAC 校验（常量时间左截断；域标签 = `hr-reg4-refresh`）。
    pub fn mac_matches(&self, secret: &[u8; 32], exporter32: &[u8; EXPORTER_LEN]) -> bool {
        refresh_mac(secret, &self.pubkey, &self.dev_tag, self.ts, exporter32)
            .verify_truncated_left(&self.mac)
            .is_ok()
    }

    /// 组帧（客户端面）。
    pub fn encode(
        secret: &[u8; 32],
        pubkey: &[u8; 32],
        dev_tag: &[u8; 8],
        ts: u64,
        exporter32: &[u8; EXPORTER_LEN],
    ) -> [u8; REFRESH_LEN] {
        let sum = refresh_mac(secret, pubkey, dev_tag, ts, exporter32)
            .finalize()
            .into_bytes();
        let mut out = [0u8; REFRESH_LEN];
        out[..2].copy_from_slice(&REFRESH_MAGIC);
        out[2..34].copy_from_slice(pubkey);
        out[34..42].copy_from_slice(dev_tag);
        out[42..50].copy_from_slice(&ts.to_be_bytes());
        out[50..66].copy_from_slice(&sum[..MAC_LEN]);
        out
    }
}

impl fmt::Debug for RefreshFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "RefreshFrame(pub={}… dev={}… ts={} mac={}…)",
            hex4(&self.pubkey),
            hex4(&self.dev_tag),
            self.ts,
            hex4(&self.mac)
        )
    }
}

/// Accept：绑定建立之后的 2B 回执（无载荷；客户端据此**确定性地**结束准入等待）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct AcceptFrame;

impl AcceptFrame {
    /// 解帧（2B 定长 + 魔数）。
    pub fn parse(pkt: &[u8]) -> Option<Self> {
        (pkt == ACCEPT_MAGIC).then_some(Self)
    }

    /// 组帧。
    pub fn encode() -> [u8; ACCEPT_LEN] {
        ACCEPT_MAGIC
    }
}

/// 引擎侧要验 MAC 的两类入站帧（**域标签不同 ⇒ 必须类型化区分**，否则刷新帧可冒充准入）。
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Reg4Frame {
    /// 准入 Proof（`hr-reg4` 域）。
    Proof(ProofFrame),
    /// 刷新帧（`hr-reg4-refresh` 域）。
    Refresh(RefreshFrame),
}

impl Reg4Frame {
    /// 设备公钥。
    pub fn pubkey(&self) -> [u8; 32] {
        match self {
            Self::Proof(f) => f.pubkey,
            Self::Refresh(f) => f.pubkey,
        }
    }

    /// 设备标签。
    pub fn dev_tag(&self) -> [u8; 8] {
        match self {
            Self::Proof(f) => f.dev_tag,
            Self::Refresh(f) => f.dev_tag,
        }
    }

    /// 客户端时间戳（喂 `table.register` 的 ±90s 窗）。
    pub fn ts(&self) -> u64 {
        match self {
            Self::Proof(f) => f.ts,
            Self::Refresh(f) => f.ts,
        }
    }

    /// MAC 校验（逐 secret 试秘面；域标签按变体选）。
    pub fn mac_matches(&self, secret: &[u8; 32], exporter32: &[u8; EXPORTER_LEN]) -> bool {
        match self {
            Self::Proof(f) => f.mac_matches(secret, exporter32),
            Self::Refresh(f) => f.mac_matches(secret, exporter32),
        }
    }
}

impl fmt::Debug for Reg4Frame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Proof(x) => fmt::Debug::fmt(x, f),
            Self::Refresh(x) => fmt::Debug::fmt(x, f),
        }
    }
}

// ---------- MAC 输入（唯一真源：出口面校验与客户端组帧共用，防两侧各写一份标签顺序） ----------

/// 准入 Proof 的 MAC：`"hr-reg4" ‖ pubkey ‖ devTag ‖ ts(BE 8B) ‖ nonce ‖ exporter32`。
fn proof_mac(
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    dev_tag: &[u8; 8],
    ts: u64,
    nonce: &Nonce,
    exporter32: &[u8; EXPORTER_LEN],
) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC 任意长密钥");
    mac.update(MAC_LABEL);
    mac.update(pubkey);
    mac.update(dev_tag);
    mac.update(&ts.to_be_bytes());
    mac.update(nonce.as_bytes());
    mac.update(exporter32);
    mac
}

/// 刷新帧的 MAC：`"hr-reg4-refresh" ‖ pubkey ‖ devTag ‖ ts(BE 8B) ‖ exporter32`。
fn refresh_mac(
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    dev_tag: &[u8; 8],
    ts: u64,
    exporter32: &[u8; EXPORTER_LEN],
) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC 任意长密钥");
    mac.update(REFRESH_MAC_LABEL);
    mac.update(pubkey);
    mac.update(dev_tag);
    mac.update(&ts.to_be_bytes());
    mac.update(exporter32);
    mac
}

/// 16B 常量时间相等（与 `table.rs::const_time_eq_16` 同形；本 crate 是叶子 ⇒ 各自一份）。
fn ct_eq_16(a: &[u8; 16], b: &[u8; 16]) -> bool {
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
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
    const TS: u64 = 1_800_000_000;
    const NONCE: [u8; NONCE_LEN] = [0x5C; NONCE_LEN];

    fn nz() -> Nonce {
        Nonce::from_bytes(NONCE)
    }

    /// **布局锚点（S1-1）**：五帧的**逐字节长度与偏移** + 魔数；MAC 独立重算（不借被测代码）。
    #[test]
    fn layout_is_fixed_and_version_labelled() {
        let pubkey = [3u8; 32];
        let dev = [0xAAu8; 8];

        // Hello：50B
        let hello = HelloFrame::encode(&pubkey, &dev, TS);
        assert_eq!(hello.len(), HELLO_LEN);
        assert_eq!(&hello[..2], b"H4");
        assert_eq!(&hello[2..34], &pubkey);
        assert_eq!(&hello[34..42], &dev);
        assert_eq!(u64::from_be_bytes(hello[42..50].try_into().unwrap()), TS);
        assert_eq!(HelloFrame::parse(&hello), Some(HelloFrame { pubkey, dev_tag: dev, ts: TS }));

        // Challenge：18B
        let ch = ChallengeFrame::encode(&nz());
        assert_eq!(ch.len(), CHALLENGE_LEN);
        assert_eq!(&ch[..2], b"C4");
        assert_eq!(&ch[2..18], &NONCE);
        assert_eq!(ChallengeFrame::parse(&ch), Some(ChallengeFrame { nonce: nz() }));

        // Proof：82B + MAC 覆盖域逐段独立重算
        let proof = ProofFrame::encode(&SECRET, &pubkey, &dev, TS, &nz(), &EXPORTER_A);
        assert_eq!(proof.len(), PROOF_LEN);
        assert_eq!(&proof[..2], b"P4");
        assert_eq!(&proof[2..34], &pubkey);
        assert_eq!(&proof[34..42], &dev);
        assert_eq!(u64::from_be_bytes(proof[42..50].try_into().unwrap()), TS);
        assert_eq!(&proof[50..66], &NONCE);
        let mut mac = Hmac::<Sha256>::new_from_slice(&SECRET).unwrap();
        mac.update(b"hr-reg4");
        mac.update(&pubkey);
        mac.update(&dev);
        mac.update(&TS.to_be_bytes());
        mac.update(&NONCE);
        mac.update(&EXPORTER_A);
        assert_eq!(&mac.finalize().into_bytes()[..16], &proof[66..82]);
        assert_eq!(ProofFrame::parse(&proof).unwrap().nonce, nz());

        // Accept：2B
        assert_eq!(&AcceptFrame::encode()[..], b"A4");
        assert_eq!(AcceptFrame::encode().len(), ACCEPT_LEN);
        assert_eq!(AcceptFrame::parse(b"A4"), Some(AcceptFrame));
        assert_eq!(AcceptFrame::parse(b"XX"), None);

        // Refresh：66B + `hr-reg4-refresh` 域
        let rf = RefreshFrame::encode(&SECRET, &pubkey, &dev, TS, &EXPORTER_A);
        assert_eq!(rf.len(), REFRESH_LEN);
        assert_eq!(&rf[..2], b"R4");
        let mut mac = Hmac::<Sha256>::new_from_slice(&SECRET).unwrap();
        mac.update(b"hr-reg4-refresh");
        mac.update(&pubkey);
        mac.update(&dev);
        mac.update(&TS.to_be_bytes());
        mac.update(&EXPORTER_A);
        assert_eq!(&mac.finalize().into_bytes()[..16], &rf[50..66]);
    }

    /// **定长切帧的唯一来源**：帧头判别与总长逐条对上；短/长一律不解析（防「猜长度」）。
    #[test]
    fn head_discrimination_is_total_and_lengths_are_fixed() {
        assert_eq!(FrameHead::of(*b"H4"), FrameHead::Hello);
        assert_eq!(FrameHead::of(*b"C4"), FrameHead::Challenge);
        assert_eq!(FrameHead::of(*b"P4"), FrameHead::Proof);
        assert_eq!(FrameHead::of(*b"A4"), FrameHead::Accept);
        assert_eq!(FrameHead::of(*b"R4"), FrameHead::Refresh);
        assert_eq!(FrameHead::of(*b"H4").len(), Some(HELLO_LEN));
        assert_eq!(FrameHead::of(*b"P4").len(), Some(PROOF_LEN));
        assert_eq!(FrameHead::of(*b"R4").len(), Some(REFRESH_LEN));
        // 短一字节 / 长一字节都不解析
        let hello = HelloFrame::encode(&[1u8; 32], &[2u8; 8], TS);
        assert!(HelloFrame::parse(&hello[..HELLO_LEN - 1]).is_none());
        let mut long = hello.to_vec();
        long.push(0);
        assert!(HelloFrame::parse(&long).is_none());
        let proof = ProofFrame::encode(&SECRET, &[1u8; 32], &[2u8; 8], TS, &nz(), &EXPORTER_A);
        assert!(ProofFrame::parse(&proof[..PROOF_LEN - 1]).is_none());
        let rf = RefreshFrame::encode(&SECRET, &[1u8; 32], &[2u8; 8], TS, &EXPORTER_A);
        assert!(RefreshFrame::parse(&rf[..REFRESH_LEN - 1]).is_none());
    }

    /// **判据（S1-1 的版本互斥）**：`H2`/`H3` 的字节**不得**被解析成新帧（判别为 `Legacy`
    /// 且**无长度** ⇒ 消费侧只能走拒绝分支）；未知魔数同理。
    #[test]
    fn legacy_versions_are_rejected_not_parsed() {
        let pubkey = [7u8; 32];
        let dev = [0x11u8; 8];
        // M1 的 `H3` 帧（66B）字节：换魔数后**与原帧同长**，也不得被当新帧
        let mut h3 = refresh_bytes(pubkey, dev); // 66B 的 `R4` 形态
        h3[..2].copy_from_slice(b"H3");
        assert_eq!(FrameHead::of(*b"H3"), FrameHead::Legacy(*b"H3"));
        assert_eq!(FrameHead::of(*b"H3").len(), None, "旧版本无（可用的）定长 ⇒ 不得切帧");
        assert_eq!(RefreshFrame::parse(&h3), None, "H3 字节不得被当刷新帧解");
        assert_eq!(FrameHead::of(*b"H2"), FrameHead::Legacy(*b"H2"));
        assert_eq!(FrameHead::of(*b"XY"), FrameHead::Unknown(*b"XY"));
        assert_eq!(
            FrameHead::of(*b"H3").legacy_why(),
            Some("帧版本不符（H2/H3——旧核或垃圾包）")
        );
        assert_eq!(FrameHead::of(*b"XY").legacy_why(), Some("帧格式非法（魔数/长度）"));
        assert!(FrameHead::Hello.legacy_why().is_none(), "合法帧没有归因文案");
        // 旧版本魔数表与判别一致（防两处名单漂移）
        for m in LEGACY_MAGICS {
            assert!(matches!(FrameHead::of(m), FrameHead::Legacy(_)));
        }
    }

    fn refresh_bytes(pubkey: [u8; 32], dev: [u8; 8]) -> [u8; REFRESH_LEN] {
        RefreshFrame::encode(&SECRET, &pubkey, &dev, TS, &EXPORTER_A)
    }

    /// **判据（S1-1 的域分隔）**：准入 Proof 与刷新帧**不可互相冒充**——
    /// 同一组字段、同一 secret：Proof 的 MAC 在刷新域验不过，反之亦然。
    #[test]
    fn proof_and_refresh_are_domain_separated() {
        let pubkey = [9u8; 32];
        let dev = [0x22; 8];
        assert_ne!(MAC_LABEL, REFRESH_MAC_LABEL, "两域标签必须不同");
        let proof = ProofFrame::parse(&ProofFrame::encode(
            &SECRET,
            &pubkey,
            &dev,
            TS,
            &nz(),
            &EXPORTER_A,
        ))
        .unwrap();
        assert!(proof.mac_matches(&SECRET, &EXPORTER_A), "本域内应验得过");
        // 把 Proof 的 mac 塞进刷新帧（同字段）⇒ 必须失败
        let mut rf = refresh_bytes(pubkey, dev);
        rf[50..66].copy_from_slice(&proof.mac);
        assert!(
            !RefreshFrame::parse(&rf).unwrap().mac_matches(&SECRET, &EXPORTER_A),
            "Proof 的 MAC 不得在刷新域成立（否则刷新帧可冒充准入）"
        );
        // 反向：刷新帧的 mac 塞进 Proof（nonce 取任意）⇒ 必须失败
        let rf = RefreshFrame::parse(&refresh_bytes(pubkey, dev)).unwrap();
        let mut pf = ProofFrame::encode(&SECRET, &pubkey, &dev, TS, &nz(), &EXPORTER_A);
        pf[66..82].copy_from_slice(&rf.mac);
        assert!(
            !ProofFrame::parse(&pf).unwrap().mac_matches(&SECRET, &EXPORTER_A),
            "刷新帧的 MAC 不得在准入域成立"
        );
    }

    /// **判据（S1-1 的连接绑定）**：换连接（exporter 变）⇒ 恒失败；错 secret 同理；
    /// 篡改任一被覆盖字段 ⇒ 失败（两帧各测一遍）。
    #[test]
    fn mac_is_bound_to_connection_and_fields() {
        let pubkey = [7u8; 32];
        let dev = [0x11; 8];
        let proof = ProofFrame::parse(&ProofFrame::encode(
            &SECRET,
            &pubkey,
            &dev,
            TS,
            &nz(),
            &EXPORTER_A,
        ))
        .unwrap();
        assert!(proof.mac_matches(&SECRET, &EXPORTER_A));
        assert!(!proof.mac_matches(&SECRET, &EXPORTER_B), "换连接（exporter 变）⇒ 必须失败");
        assert!(!proof.mac_matches(&[0x43; 32], &EXPORTER_A), "错 secret ⇒ 失败");
        for mutate in [
            |f: &mut ProofFrame| f.pubkey[0] ^= 1,
            |f: &mut ProofFrame| f.dev_tag[0] ^= 1,
            |f: &mut ProofFrame| f.ts += 1,
            |f: &mut ProofFrame| f.nonce.0[0] ^= 1,
            |f: &mut ProofFrame| f.mac[0] ^= 1,
        ] {
            let mut t = proof;
            mutate(&mut t);
            assert!(!t.mac_matches(&SECRET, &EXPORTER_A), "篡改后不得验过：{t:?}");
        }

        let rf = RefreshFrame::parse(&refresh_bytes(pubkey, dev)).unwrap();
        assert!(rf.mac_matches(&SECRET, &EXPORTER_A));
        assert!(!rf.mac_matches(&SECRET, &EXPORTER_B), "刷新帧同样绑定连接");
        for mutate in [
            |f: &mut RefreshFrame| f.pubkey[0] ^= 1,
            |f: &mut RefreshFrame| f.dev_tag[0] ^= 1,
            |f: &mut RefreshFrame| f.ts += 1,
            |f: &mut RefreshFrame| f.mac[0] ^= 1,
        ] {
            let mut t = rf;
            mutate(&mut t);
            assert!(!t.mac_matches(&SECRET, &EXPORTER_A), "刷新帧篡改后不得验过：{t:?}");
        }
    }

    /// MAC 是**左 16B 截断**（不是右截断也不是全 32B）。
    #[test]
    fn mac_is_left_truncated_prefix() {
        let pubkey = [9u8; 32];
        let dev = [0x22; 8];
        let mut mac = Hmac::<Sha256>::new_from_slice(&SECRET).unwrap();
        mac.update(MAC_LABEL);
        mac.update(&pubkey);
        mac.update(&dev);
        mac.update(&TS.to_be_bytes());
        mac.update(&NONCE);
        mac.update(&EXPORTER_A);
        let sum = mac.finalize().into_bytes();
        let mut pkt = ProofFrame::encode(&SECRET, &pubkey, &dev, TS, &nz(), &EXPORTER_A);
        pkt[66..82].copy_from_slice(&sum[16..32]); // 右半冒充
        assert!(
            !ProofFrame::parse(&pkt).unwrap().mac_matches(&SECRET, &EXPORTER_A),
            "MAC 必须是左截断"
        );
    }

    /// nonce 生成：两枚互不相同（CSPRNG 的**弱**断言——只判「不是常量」，不判熵）；
    /// Debug 不打印字节（活跃材料不进日志）。
    #[test]
    fn nonce_generates_fresh_bytes_and_hides_them_in_debug() {
        let a = Nonce::generate().expect("系统随机源在测试环境可用");
        let b = Nonce::generate().expect("系统随机源在测试环境可用");
        assert_ne!(a.as_bytes(), b.as_bytes(), "两次生成不得相同（防常量/零填充实现）");
        assert_eq!(format!("{a:?}"), "Nonce(…)");
        assert!(a.ct_eq(&a));
        // 与零填充**必不相等**（代码门 r15 的 G13：原写法 `!ct_eq(zero) || a == zero` 是恒真式）
        assert_ne!(a.as_bytes(), &[0u8; NONCE_LEN], "生成值不得为零填充");
        assert!(!a.ct_eq(&Nonce::from_bytes([0u8; NONCE_LEN])));
    }

    /// `Reg4Frame` 的两支都走各自的域（类型化分派不串域）。
    #[test]
    fn reg4_frame_dispatches_to_its_own_domain() {
        let pubkey = [7u8; 32];
        let dev = [0x11; 8];
        let p = Reg4Frame::Proof(
            ProofFrame::parse(&ProofFrame::encode(
                &SECRET, &pubkey, &dev, TS, &nz(), &EXPORTER_A,
            ))
            .unwrap(),
        );
        let r = Reg4Frame::Refresh(
            RefreshFrame::parse(&refresh_bytes(pubkey, dev)).unwrap(),
        );
        assert_eq!(p.pubkey(), pubkey);
        assert_eq!(r.dev_tag(), dev);
        assert_eq!(p.ts(), TS);
        assert!(p.mac_matches(&SECRET, &EXPORTER_A));
        assert!(r.mac_matches(&SECRET, &EXPORTER_A));
        // 互串：把 Proof 的字节塞进 Refresh 变体 ⇒ 验不过
        let mut raw = refresh_bytes(pubkey, dev);
        raw[50..66].copy_from_slice(&ProofFrame::encode(
            &SECRET, &pubkey, &dev, TS, &nz(), &EXPORTER_A,
        )[66..82]);
        let r2 = Reg4Frame::Refresh(RefreshFrame::parse(&raw).unwrap());
        assert!(!r2.mac_matches(&SECRET, &EXPORTER_A));
        assert!(format!("{p:?}").starts_with("ProofFrame("));
        assert!(format!("{r:?}").starts_with("RefreshFrame("));
    }
}
