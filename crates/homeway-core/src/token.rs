//! hmw1 凭证（token）：可见前缀 + base64url(裸二进制)。
//!
//! 布局（语义真源 `baseline:pkg/proto/token.go`，基线 621fe0e；M1 追加一段**可选**
//! 尾字段——见下）：
//!
//! ```text
//! "hmw1" ‖ base64url-raw( peerId(32B) ‖ secret(32B) ‖ epCount(1B) ‖ [type(1B)+len(1B)+addr]* ‖ [rpk(32B)]? ‖ crc(4B) )
//! ```
//!
//! - `peerId` = 后端静态 WG 公钥；`secret` = 凭证种子；`type` 0=direct 1=relay
//!   **2=QUIC（M1 新增类别）**；`crc` = SHA-256(前文)[:4]；base64 为**无填充** base64url
//!   （尾缀 `=` 不容忍，FIX-89）。
//! - 解析先 `trim`（Go `strings.TrimSpace` 同义：Unicode White_Space）。
//!
//! **M1 追加字段（`rpk`，可选 32B，additive）**：出口 Ed25519 **RPK 裸公钥**——客户端
//! 钉定服务端身份用（M1 设计 §1.3/§12-②；登记条草案见 §3.6，落库 = S4 判据行批）。
//! 形态取**尾部追加**：无该字段的串（全部既有 Go 向量与既有部署 token）逐字节不变；
//! 带该字段的串 = 端点数之后恰 32B（其余尾长仍判 Malformed——不留「猜长度」的口子）。
//! 旧解析器会拒带 rpk 的串（无兼容包袱：用户拍板①；M2 重写 token 时统一）。
//!
//! 与 Go 的**已登记差异**（均为对抗性输入面，正常铸造的 token 不受影响）：
//! 1. Go `DecodeToken("hmw")` 会 panic（`s[:4]` 越界，已登记 Go 侧问题清单 G1）；本实现
//!    返回 [`TokenError::UnsupportedVersion`]。
//! 2. 端点地址字节须为 UTF-8（Go 接受任意字节串）；非 UTF-8 → [`TokenError::Malformed`]。
//! 3. `Malformed` 的 reason 文案与 Go 各打点不逐字对齐（App 按错误**类别**归因，reason
//!    仅诊断用；R7 若需逐字对齐再按打点补）。
//!
//! 解析形态：`parse_body` 对**已解码的载荷字节**借用解析（端点地址零拷贝 `&str`）；
//! `decode` 为便捷入口（剥离换行 → 解码 base64 → 解析 → 转移交所有权）。

use core::fmt;

use base64::alphabet::URL_SAFE;
use base64::engine::GeneralPurpose;
use base64::Engine;
use sha2::{Digest, Sha256};
use thiserror::Error;

/// 可见版本前缀（"hmw2" = 下一版；未知 hmw* 前缀报 [`TokenError::UnsupportedVersion`]）。
pub const PREFIX: &str = "hmw1";

/// 载荷下界：peerId(32) + secret(32) + epCount(1) + crc(4)。
const MIN_BODY_LEN: usize = 32 + 32 + 1 + 4;

/// base64url 引擎：与 Go `base64.RawURLEncoding` 同严格度——URL 字母表、**无填充**
/// （编码不带 `=`、解码遇 `=` 即拒，FIX-89），且**容忍非规范尾位**（Go 默认不查尾位；
/// base64 crate 默认查，须显式放开才能逐字节同判）。Go 解码器还会跳过输入中任意位置的
/// `\r`/`\n`（终端折行常态），crate 不做——由 [`decode`] 先行剥离对齐。
/// （构造全为 const fn ⇒ 无需 LazyLock。）
static B64: GeneralPurpose = GeneralPurpose::new(
    &URL_SAFE,
    base64::engine::GeneralPurposeConfig::new()
        .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone)
        .with_decode_allow_trailing_bits(true)
        .with_encode_padding(false),
);

/// 三类可区分失败，与 Go 哨兵错误（token.go:33-37）一一对应；Display 前缀段与 Go 哨兵
/// 逐字一致（该文案经 NAPI 直达 App，R7 前必须钉死）。`Malformed` 的 `reason` 仅供诊断。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenError {
    /// `homeway/token: 不支持的 token 版本: <所见前缀>`
    #[error("homeway/token: 不支持的 token 版本: {seen}")]
    UnsupportedVersion { seen: String },
    /// `homeway/token: 校验失败（串被截断或损坏）`
    #[error("homeway/token: 校验失败（串被截断或损坏）")]
    Corrupted,
    /// `homeway/token: 格式非法: <原因>`（ASCII 冒号空格，同 Go `fmt.Errorf("%w: …")`）
    #[error("homeway/token: 格式非法: {reason}")]
    Malformed { reason: &'static str },
}

/// 端点类别（载荷 `type` 字节的语义化形态）。
///
/// **M1 新增 `Quic`（wire 2，additive）**：QUIC 类端点（`serve.quic_listen` 的独立端口，
/// M1 设计 §1.1）——**端口与 WG 端口不同**，故必须靠类别字节区分（地址本身看不出）。
/// 消费面（候选过滤）见 `wtransport::domain_eps`：WG 档**不吃** QUIC 端点（设计 §2.1
/// 末段「两族候选不得互相投喂」），S2 的岛只吃 QUIC 类 + 中继端点。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointKind {
    Direct,
    Relay,
    Quic,
}

impl EndpointKind {
    /// 线上字节：Direct=0、Relay=1、Quic=2。
    pub fn to_wire(self) -> u8 {
        match self {
            EndpointKind::Direct => 0,
            EndpointKind::Relay => 1,
            EndpointKind::Quic => 2,
        }
    }
    /// 线上字节→类别。Go 同义宽松语义：**未知值一律按 Direct 收**（不报错）。
    pub fn from_wire(b: u8) -> Self {
        match b {
            1 => EndpointKind::Relay,
            2 => EndpointKind::Quic,
            _ => EndpointKind::Direct,
        }
    }

    /// 该类别是否属于 **WG 面**（M1 §2.1 末段「候选按 transport 过滤」的判据）：
    /// QUIC 类**不是**——WG 档不吃它（端口都不同，§1.1），吃到只会产出握手超时噪声与
    /// 赛跑结算失真。S2 的岛按 `!is_wg()` 取 QUIC 档候选。
    pub fn is_wg(self) -> bool {
        !matches!(self, EndpointKind::Quic)
    }
}

/// WG 面候选的端点过滤（M1 §2.1 末段）：滤掉 QUIC 类端点，其余类别与顺序原样。
///
/// 为什么放在这里而不是 `wtransport::domain_eps`：①规则住在 `EndpointKind` 的家乡
/// （一处定义、两侧引用）；②`wtransport/**` 是 M1 S1c 的红线面（候选展开函数保持零改动，
/// 由调用方喂已过滤的入参——见 `domain_eps::split_and_resolve` 的文档）。
pub fn wg_endpoint_refs(eps: &[Endpoint]) -> Vec<EndpointRef<'_>> {
    eps.iter()
        .filter(|e| e.kind.is_wg())
        .map(|e| EndpointRef::new(e.addr.as_str(), e.kind))
        .collect()
}

/// 32B 定长 newtype 的展开骨架（`PeerId` 与 `Secret` 共用；差异只在 `$copy` 与
/// Drop——见下面两个入口宏）。
macro_rules! byte_array_newtype_common {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(PartialEq, Eq, Hash)]
        pub struct $name([u8; 32]);

        impl From<[u8; 32]> for $name {
            fn from(v: [u8; 32]) -> Self {
                Self(v)
            }
        }

        impl $name {
            /// 借视图（不拷贝）。
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }
    };
}

/// 公开材料（可 `Copy`：多份副本无擦除义务）。
macro_rules! byte_array_newtype {
    ($(#[$doc:meta])* $name:ident, $redacted:expr) => {
        byte_array_newtype_common!($(#[$doc])* $name);

        impl Clone for $name {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl Copy for $name {}

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                if $redacted {
                    // 敏感材料：不落任何指纹（连前缀都不出——跨实例可关联）
                    write!(f, concat!(stringify!($name), "(<redacted>)"))
                } else {
                    // 公钥：日志短指纹纪律（4B hex，同 Go 侧）
                    let short: String =
                        self.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
                    write!(f, "{}({short}…)", stringify!($name))
                }
            }
        }
    };
}

/// 敏感材料 newtype（**不 `Copy`** + `Drop` 擦除——F8d）。
///
/// 为什么去 `Copy`：`Copy` 使每处传参/赋值都留下**无法追踪的栈副本**，Drop 只能擦到
/// 自己这一份（审计 P2「`Secret` 未 zeroize」的根因形态）。去 `Copy` 后持有者唯一，
/// 引用面走 `&Secret`/`as_bytes()`（wgcore/bind/reg 全是借用面，不受影响）。
macro_rules! secret_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        byte_array_newtype_common!($(#[$doc])* $name);

        impl Clone for $name {
            fn clone(&self) -> Self {
                Self(self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                // 敏感材料：不落任何指纹（连前缀都不出——跨实例可关联）
                write!(f, concat!(stringify!($name), "(<redacted>)"))
            }
        }

        impl Drop for $name {
            fn drop(&mut self) {
                wipe_bytes(&mut self.0);
            }
        }
    };
}

/// 敏感字节擦除（volatile 写 + 编译器栅栏；**不引新依赖**）。
///
/// 形状取 zeroize 的最小等价面：逐字节 `write_volatile` 防「写后即死」被优化掉，
/// 末尾 `compiler_fence` 防重排。栈上中间量（例：`Secret::from([..])` 的临时数组）
/// 不在此面——见 `docs/INTEROP-CRITERIA.md` 的 F8d 残余登记。
pub(crate) fn wipe_bytes(b: &mut [u8; 32]) {
    for x in b.iter_mut() {
        // SAFETY: 引用来源合法；volatile 写只保证不被消除，不引入未定义行为
        unsafe { std::ptr::write_volatile(x, 0) };
    }
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

byte_array_newtype!(
    /// 后端静态 WG 公钥（token 载荷首 32B；设备身份按它派生）。
    PeerId,
    false
);
byte_array_newtype!(
    /// 出口 Ed25519 **RPK 裸公钥**（token 载荷可选尾字段，32B；M1 设计 §1.3/§12-②）。
    ///
    /// 谁消费：客户端钉定校验器（错公钥 ⇒ TLS 握手中止，不是「连上再拒」）。
    /// 挂哪来：`homeway_quic::ExitQuic::rpk_public_key()`（出口身份 = `HKDF(后端
    /// 静态私钥, "homeway/quic-rpk")` 的 Ed25519 公钥）。
    RpkPubKey,
    false
);
secret_newtype!(
    /// 凭证种子（注册 HMAC / WG PSK / 隧道地址派生输入）。
    ///
    /// 敏感材料：Debug 全脱敏；**不 `Copy`** + Drop 擦除（F8d）。`PeerId` 与它
    /// 同宏族但保持 `Copy`（公开材料无擦除义务——设计门 7.2 的拆宏要求）。
    Secret
);

/// 借用形态端点：地址直接来自载荷字节，零拷贝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EndpointRef<'a> {
    /// `host:port`——host 可为 IP（含 `[v6]` 括号形）或域名。
    pub addr: &'a str,
    pub kind: EndpointKind,
}

impl<'a> EndpointRef<'a> {
    /// 跨 crate 构造面（`#[non_exhaustive]` 不挡构造——编码侧 CLI/引擎用）。
    pub fn new(addr: &'a str, kind: EndpointKind) -> Self {
        Self { addr, kind }
    }
}

/// 借用形态 token：对**已 base64 解码的载荷**借用解析（端点地址零拷贝）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenRef<'a> {
    peer_id: [u8; 32],
    secret: [u8; 32],
    endpoints: Vec<EndpointRef<'a>>,
    /// 服务端 RPK 公钥（M1 追加的可选尾字段；旧串 = `None`）。
    rpk: Option<RpkPubKey>,
}

impl<'a> TokenRef<'a> {
    pub fn peer_id(&self) -> PeerId {
        PeerId::from(self.peer_id)
    }
    pub fn secret(&self) -> Secret {
        Secret::from(self.secret)
    }
    pub fn endpoints(&self) -> &[EndpointRef<'a>] {
        &self.endpoints
    }
    /// 服务端 RPK 公钥（`None` = 该 token 未携带——M1 之前的串与 Go 向量都是此形态）。
    pub fn rpk(&self) -> Option<RpkPubKey> {
        self.rpk
    }
    /// 直连端点（Go `DirectEndpoints`）。
    pub fn direct_endpoints(&self) -> impl Iterator<Item = EndpointRef<'a>> + '_ {
        self.endpoints
            .iter()
            .copied()
            .filter(|e| e.kind == EndpointKind::Direct)
    }
    /// 中继端点（Go `RelayEndpoints`）。
    pub fn relay_endpoints(&self) -> impl Iterator<Item = EndpointRef<'a>> + '_ {
        self.endpoints
            .iter()
            .copied()
            .filter(|e| e.kind == EndpointKind::Relay)
    }
    /// QUIC 类端点（M1 新增类别；S2 的岛按 transport 过滤候选时的输入集之一）。
    pub fn quic_endpoints(&self) -> impl Iterator<Item = EndpointRef<'a>> + '_ {
        self.endpoints
            .iter()
            .copied()
            .filter(|e| e.kind == EndpointKind::Quic)
    }
}

/// 所有权形态端点（会话/配置持有面）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Endpoint {
    pub addr: String,
    pub kind: EndpointKind,
}

/// 所有权形态 token（对应 Go `proto.Token`）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Token {
    pub peer_id: PeerId,
    pub secret: Secret,
    pub endpoints: Vec<Endpoint>,
    /// 服务端 RPK 公钥（可选；见 [`RpkPubKey`]）。
    pub rpk: Option<RpkPubKey>,
}

impl<'a> From<TokenRef<'a>> for Token {
    fn from(t: TokenRef<'a>) -> Self {
        Token {
            peer_id: t.peer_id(),
            secret: t.secret(),
            endpoints: t
                .endpoints
                .into_iter()
                .map(|e| Endpoint {
                    addr: e.addr.to_owned(),
                    kind: e.kind,
                })
                .collect(),
            rpk: t.rpk,
        }
    }
}

/// 解析整串 token：trim → 版本前缀 → 剥离内嵌 `\r`/`\n`（Go base64 解码器行为）→
/// base64url 解码 → [`parse_body`]。
pub fn decode(input: &str) -> Result<Token, TokenError> {
    let s = input.trim();
    if s.starts_with("hmw") && !s.starts_with(PREFIX) {
        // Go 侧此处 s[:4] 对恰 "hmw" 长度会 panic（已登记 Go 侧问题 G1）；本实现安全返回。
        // seen 取前 4 字符（不足则全取），对齐 Go 错误文案后缀。
        let seen: String = s.chars().take(4).collect();
        return Err(TokenError::UnsupportedVersion { seen });
    }
    if !s.starts_with(PREFIX) {
        return Err(TokenError::Malformed {
            reason: "缺少 hmw1 前缀",
        });
    }
    // Go base64 解码器跳过任意位置的 \r\n（终端/聊天工具折行）；crate 引擎不做，先剥离对齐
    let body_b64 = s[PREFIX.len()..].replace(['\r', '\n'], "");
    let raw = B64
        .decode(body_b64.as_bytes())
        .map_err(|_| TokenError::Malformed {
            reason: "base64url 解码失败",
        })?;
    Ok(parse_body(&raw)?.into())
}

/// 对已解码载荷借用解析（零拷贝核心；CRC 校验先行，与 Go `decodeTokenBytes` 同序）。
pub fn parse_body(raw: &[u8]) -> Result<TokenRef<'_>, TokenError> {
    if raw.len() < MIN_BODY_LEN {
        return Err(TokenError::Corrupted); // 截断（含 base64 解码后过短）
    }
    let (body, crc) = raw.split_at(raw.len() - 4);
    let sum = Sha256::digest(body);
    if sum[..4] != *crc {
        return Err(TokenError::Corrupted);
    }

    let (peer_id, rest) = body.split_at(32);
    let peer_id: [u8; 32] = peer_id.try_into().expect("split_at(32) 保证");
    let (secret, rest) = rest.split_at(32);
    let secret: [u8; 32] = secret.try_into().expect("split_at(32) 保证");
    let (&ep_count, mut rest) = rest.split_first().ok_or(TokenError::Corrupted)?; // 不可达（MIN_BODY_LEN 已含 1B）

    let mut endpoints = Vec::with_capacity(usize::from(ep_count));
    for _ in 0..ep_count {
        if rest.len() < 2 {
            return Err(TokenError::Malformed {
                reason: "端点数量声明与载荷不符（端点头不足）",
            });
        }
        let (&typ, &addr_len) = (&rest[0], &rest[1]);
        rest = &rest[2..];
        if addr_len == 0 || rest.len() < usize::from(addr_len) {
            return Err(TokenError::Malformed {
                reason: "端点长度越界或为零",
            });
        }
        let (addr_bytes, rest2) = rest.split_at(usize::from(addr_len));
        rest = rest2;
        let addr = core::str::from_utf8(addr_bytes).map_err(|_| TokenError::Malformed {
            reason: "端点地址非 UTF-8",
        })?;
        validate_host_port(addr)?;
        endpoints.push(EndpointRef {
            addr,
            kind: EndpointKind::from_wire(typ),
        });
    }
    // 可选尾字段：恰 32B = 出口 RPK 裸公钥（M1 追加）；其余尾长一律拒（不猜）。
    let rpk = match rest.len() {
        0 => None,
        32 => {
            let mut b = [0u8; 32];
            b.copy_from_slice(rest);
            Some(RpkPubKey::from(b))
        }
        _ => {
            return Err(TokenError::Malformed {
                reason: "载荷尾部有多余字节",
            })
        }
    };
    Ok(TokenRef {
        peer_id,
        secret,
        endpoints,
        rpk,
    })
}

/// 端点地址合法性：镜像 Go `net.SplitHostPort` 的**结构规则**（纯切分器——端口非数字、
/// host 为空、port 为空都不拒绝；只查结构，net/ipsock.go）：
/// 无 `:` 拒；`[` 开头：首个 `]` 须恰在末位 `:` 前（`]` 结尾 = 缺端口、更早/更晚 = 多冒号）；
/// 非括号形：host 段不得含 `:`；其后按 Go 的 j/k 段查错位 `[`/`]`。
fn validate_host_port(s: &str) -> Result<(), TokenError> {
    let b = s.as_bytes();
    let last_colon = b
        .iter()
        .rposition(|&c| c == b':')
        .ok_or(TokenError::Malformed {
            reason: "端点缺端口",
        })?;
    let (j, k) = if b.first() == Some(&b'[') {
        let end = b
            .iter()
            .position(|&c| c == b']')
            .ok_or(TokenError::Malformed {
                reason: "端点缺 ']'",
            })?;
        if end + 1 == b.len() {
            return Err(TokenError::Malformed {
                reason: "']' 后缺端口",
            });
        }
        if end + 1 != last_colon {
            return Err(TokenError::Malformed {
                reason: "']' 后端口段不合法（多冒号）",
            });
        }
        (1, end + 1)
    } else {
        if b[..last_colon].contains(&b':') {
            return Err(TokenError::Malformed {
                reason: "host 内含多余 ':'",
            });
        }
        (0, 0)
    };
    if b[j..].contains(&b'[') {
        return Err(TokenError::Malformed {
            reason: "地址含错位 '['",
        });
    }
    if b[k..].contains(&b']') {
        return Err(TokenError::Malformed {
            reason: "地址含错位 ']'",
        });
    }
    Ok(())
}

/// 编码输入（借用面）。
pub struct TokenSpec<'a> {
    pub peer_id: &'a PeerId,
    pub secret: &'a Secret,
    pub endpoints: &'a [EndpointRef<'a>],
    /// 服务端 RPK 公钥（`None` = 不打该字段——WG 档/无 QUIC 档的形态）。
    pub rpk: Option<&'a RpkPubKey>,
}

/// 编码并 base64url（无填充）。校验与 Go `EncodeToken` 同面：端点地址须过 host:port
/// 结构校验、长度 ≤255；端点数 ≤255。
pub fn encode(spec: &TokenSpec<'_>) -> Result<String, TokenError> {
    for e in spec.endpoints {
        validate_host_port(e.addr)?;
        if e.addr.len() > 255 {
            return Err(TokenError::Malformed {
                reason: "端点地址过长",
            });
        }
    }
    if spec.endpoints.len() > 255 {
        return Err(TokenError::Malformed {
            reason: "端点数超上限",
        });
    }

    let mut buf = Vec::with_capacity(MIN_BODY_LEN + spec.endpoints.len() * 3);
    buf.extend_from_slice(spec.peer_id.as_bytes());
    buf.extend_from_slice(spec.secret.as_bytes());
    buf.push(spec.endpoints.len() as u8);
    for e in spec.endpoints {
        buf.push(e.kind.to_wire());
        buf.push(e.addr.len() as u8);
        buf.extend_from_slice(e.addr.as_bytes());
    }
    if let Some(k) = spec.rpk {
        buf.extend_from_slice(k.as_bytes());
    }
    let sum = Sha256::digest(&buf);
    buf.extend_from_slice(&sum[..4]);
    let mut out = String::with_capacity(PREFIX.len() + buf.len().div_ceil(3) * 4);
    out.push_str(PREFIX);
    out.push_str(&B64.encode(&buf));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// M1：`rpk` 尾字段（32B，可选）——带它可往返、逐字节稳定；不带它（旧串）逐字节不变。
    #[test]
    fn rpk_field_is_optional_trailing_and_byte_stable() {
        let peer = PeerId::from([0x11; 32]);
        let secret = Secret::from([0x22; 32]);
        let eps = [EndpointRef {
            addr: "127.0.0.1:42641",
            kind: EndpointKind::Direct,
        }];
        let rpk = RpkPubKey::from([0x33; 32]);
        // 端点按解码结果重建（借用面不参与本用例的字节稳定性判据）
        let eps_of = |t: &Token| -> Vec<EndpointRef<'static>> {
            t.endpoints
                .iter()
                .map(|e| EndpointRef::new(Box::leak(e.addr.clone().into_boxed_str()), e.kind))
                .collect()
        };

        // 不带 rpk：与 M1 之前同形（无尾字段）
        let plain = encode(&TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
            rpk: None,
        })
        .unwrap();
        let t = decode(&plain).unwrap();
        assert_eq!(t.rpk, None, "无尾字段 ⇒ rpk = None");
        assert_eq!(t.endpoints.len(), 1);
        let eps_plain = eps_of(&t);
        let back = encode(&TokenSpec {
            peer_id: &t.peer_id,
            secret: &t.secret,
            endpoints: &eps_plain,
            rpk: t.rpk.as_ref(),
        })
        .unwrap();
        assert_eq!(back, plain, "无 rpk 路径逐字节稳定");

        // 带 rpk：往返带上、再编码逐字节一致
        let with = encode(&TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
            rpk: Some(&rpk),
        })
        .unwrap();
        assert_ne!(with, plain, "带 rpk 的串必不同于不带（尾 32B + CRC 变化）");
        let t2 = decode(&with).unwrap();
        assert_eq!(t2.rpk, Some(rpk), "rpk 逐字节还原");
        let eps2 = eps_of(&t2);
        let back2 = encode(&TokenSpec {
            peer_id: &t2.peer_id,
            secret: &t2.secret,
            endpoints: &eps2,
            rpk: t2.rpk.as_ref(),
        })
        .unwrap();
        assert_eq!(back2, with, "带 rpk 路径逐字节稳定");
        // 借用面（parse_body）同判
        let raw = B64.decode(&with.as_bytes()[PREFIX.len()..]).unwrap();
        assert_eq!(parse_body(&raw).unwrap().rpk(), Some(rpk));
    }

    /// 尾字段**只认恰 32B**：1/31/33/64 字节一律 Malformed（不留「猜长度」的口子）。
    #[test]
    fn rpk_field_rejects_other_trailing_lengths() {
        for extra in [1usize, 31, 33, 64] {
            let mut body = Vec::new();
            body.extend_from_slice(&[0x01u8; 32]); // peer
            body.extend_from_slice(&[0x02u8; 32]); // secret
            body.push(0); // epCount = 0
            body.extend(std::iter::repeat_n(0xAAu8, extra));
            let sum = Sha256::digest(&body);
            body.extend_from_slice(&sum[..4]);
            match parse_body(&body) {
                Err(TokenError::Malformed { reason }) => {
                    assert_eq!(reason, "载荷尾部有多余字节", "extra={extra}")
                }
                other => panic!("extra={extra} 应判 Malformed，实得 {other:?}"),
            }
        }
    }

    #[test]
    fn validate_host_port_mirrors_go_structure_rules() {
        // Go net.SplitHostPort 是纯结构切分：这些全**接受**（实测于 baseline 克隆）
        for ok in [
            "a:b",
            ":80",
            "host:",
            "127.0.0.1:42641",
            "[::1]:53",
            "home.example.com:443",
        ] {
            validate_host_port(ok).unwrap_or_else(|e| panic!("{ok} 应合法：{e:?}"));
        }
        // 这些**拒绝**（结构错误；规则面同 Go ipsock.go）
        for bad in ["noport", "a:b:c", "[::1", "[::1]53", "[::1]:53:x", "a]b:80", "a[b]:80"] {
            assert!(validate_host_port(bad).is_err(), "{bad} 应非法");
        }
    }

    #[test]
    fn decode_empty_and_short() {
        // base64 解码后 < 69B → Corrupted（Go 同）
        for s in ["hmw1", "hmw1AAAA"] {
            assert_eq!(decode(s), Err(TokenError::Corrupted), "{s}");
        }
        // Go 侧此串 panic（已登记差异 G1）；seen 取不足 4 字符的全量
        assert_eq!(
            decode("hmw"),
            Err(TokenError::UnsupportedVersion {
                seen: "hmw".into()
            })
        );
        assert_eq!(
            decode("rl1AAAA"),
            Err(TokenError::Malformed {
                reason: "缺少 hmw1 前缀"
            })
        );
    }

    /// M3：Go base64 解码器跳过任意位置 \r/\n（实测 baseline 克隆）——Rust 剥离后同判。
    #[test]
    fn decode_skips_embedded_newlines_like_go() {
        let spec = TokenSpec {
            peer_id: &PeerId::from([7u8; 32]),
            secret: &Secret::from([9u8; 32]),
            endpoints: &[EndpointRef {
                addr: "127.0.0.1:42641",
                kind: EndpointKind::Direct,
            }],
            rpk: None,
        };
        let tok = encode(&spec).unwrap();
        for sep in ["\n", "\r\n", "\n\r"] {
            let mid = tok.len() / 2;
            let folded = format!("{}{}{}", &tok[..mid], sep, &tok[mid..]);
            let t = decode(&folded)
                .unwrap_or_else(|e| panic!("折行串（{sep:?}）应可解析：{e:?}"));
            assert_eq!(t.endpoints.len(), 1);
            assert_eq!(t.endpoints[0].addr, "127.0.0.1:42641");
        }
        // 尾缀 '=' 仍拒（FIX-89，与换行剥离无关）
        assert!(decode(&format!("{tok}=")).is_err());
    }

    /// M4：哨兵 Display 前缀段与 Go 逐字一致（文案经 NAPI 直达 App）。
    #[test]
    fn error_display_prefixes_match_go_sentinels() {
        assert_eq!(
            TokenError::Corrupted.to_string(),
            "homeway/token: 校验失败（串被截断或损坏）"
        );
        assert_eq!(
            TokenError::UnsupportedVersion {
                seen: "hmw2".into()
            }
            .to_string(),
            "homeway/token: 不支持的 token 版本: hmw2"
        );
        let m = TokenError::Malformed {
            reason: "缺少 hmw1 前缀",
        }
        .to_string();
        assert!(m.starts_with("homeway/token: 格式非法: "), "{m}"); // ASCII 冒号空格
    }

    /// L2：Secret 的 Debug 全脱敏（不含任何字节片段）。
    #[test]
    fn secret_debug_is_redacted() {
        let s = Secret::from([0xab; 32]);
        let d = format!("{s:?}");
        assert_eq!(d, "Secret(<redacted>)");
        assert!(!d.contains("abab"));
    }

    /// F8d：`wipe_bytes` 擦除（填 0xAB → 逐字节 0）。
    #[test]
    fn wipe_bytes_zeroes_all() {
        let mut b = [0xABu8; 32];
        wipe_bytes(&mut b);
        assert_eq!(b, [0u8; 32]);
    }

    #[test]
    fn encode_decode_roundtrip_is_byte_stable() {
        let peer = {
            let mut p = [0u8; 32];
            p[0] = 0x11;
            p
        };
        let secret = {
            let mut s = [0u8; 32];
            s[31] = 1;
            s
        };
        let eps = [
            EndpointRef {
                addr: "127.0.0.1:42641",
                kind: EndpointKind::Direct,
            },
            EndpointRef {
                addr: "198.51.100.212:41741",
                kind: EndpointKind::Relay,
            },
        ];
        let spec = TokenSpec {
            peer_id: &PeerId::from(peer),
            secret: &Secret::from(secret),
            endpoints: &eps,
            rpk: None,
        };
        let tok = encode(&spec).unwrap();
        let decoded = decode(&tok).unwrap();
        assert_eq!(decoded.peer_id.as_bytes(), &peer);
        assert_eq!(decoded.secret.as_bytes(), &secret);
        assert_eq!(decoded.endpoints.len(), 2);
        assert_eq!(decoded.endpoints[0].addr, "127.0.0.1:42641");
        assert_eq!(decoded.endpoints[0].kind, EndpointKind::Direct);
        assert_eq!(decoded.endpoints[1].addr, "198.51.100.212:41741");
        assert_eq!(decoded.endpoints[1].kind, EndpointKind::Relay);
        // 借用面再编码 → 与原串逐字节一致
        let raw = B64.decode(&tok.as_bytes()[PREFIX.len()..]).unwrap();
        let parsed = parse_body(&raw).unwrap();
        let spec2 = TokenSpec {
            peer_id: &parsed.peer_id(),
            secret: &parsed.secret(),
            endpoints: parsed.endpoints(),
            rpk: None,
        };
        assert_eq!(encode(&spec2).unwrap(), tok);
        assert_eq!(parsed.direct_endpoints().count(), 1);
        assert_eq!(parsed.relay_endpoints().count(), 1);
    }

    /// L3：线字节宽松语义——**未知** type 字节一律按 Direct（Go 同义）；M1 起 2 = QUIC 是
    /// **已知名**（不再是未知值）⇒ 2 归 Quic，只有 >2 才是宽松面。
    #[test]
    fn endpoint_kind_from_wire_is_lenient_like_go() {
        assert_eq!(EndpointKind::from_wire(0), EndpointKind::Direct);
        assert_eq!(EndpointKind::from_wire(1), EndpointKind::Relay);
        assert_eq!(EndpointKind::from_wire(2), EndpointKind::Quic);
        for odd in [3, 7, 0xff] {
            assert_eq!(EndpointKind::from_wire(odd), EndpointKind::Direct);
        }
        // wire 字节（M1 定值；Go 侧未知值按 Direct 收 = 无兼容包袱，用户拍板①）
        assert_eq!(EndpointKind::Direct.to_wire(), 0);
        assert_eq!(EndpointKind::Relay.to_wire(), 1);
        assert_eq!(EndpointKind::Quic.to_wire(), 2);
    }

    /// **判据（M1 S1c 的「WG 档不吃 QUIC 端点」，§2.1 末段）**：`wg_endpoint_refs` 滤掉
    /// QUIC 类（地址/端口都不是 WG 面），其余类别与顺序原样；反向对照：把同址标成
    /// Direct ⇒ 会留下（证明过滤真在起作用）。
    #[test]
    fn wg_endpoint_refs_filters_quic_class() {
        let eps = vec![
            Endpoint { addr: "1.2.3.4:41641".into(), kind: EndpointKind::Direct },
            Endpoint { addr: "1.2.3.4:41652".into(), kind: EndpointKind::Quic },
            Endpoint { addr: "5.6.7.8:41741".into(), kind: EndpointKind::Relay },
            Endpoint { addr: "203.0.113.7:41652".into(), kind: EndpointKind::Quic },
        ];
        let wg = wg_endpoint_refs(&eps);
        assert_eq!(wg.len(), 2, "QUIC 类全滤掉：{wg:?}");
        assert!(wg.iter().all(|e| e.kind.is_wg()));
        assert_eq!(wg[0].addr, "1.2.3.4:41641");
        assert_eq!(wg[1].addr, "5.6.7.8:41741");
        assert_eq!(wg[1].kind, EndpointKind::Relay, "中继位原样");
        // 反向对照：同址标成 Direct ⇒ 留下
        let mislabeled = vec![Endpoint { addr: "1.2.3.4:41652".into(), kind: EndpointKind::Direct }];
        assert_eq!(wg_endpoint_refs(&mislabeled).len(), 1);
        assert!(!EndpointKind::Quic.is_wg() && EndpointKind::Direct.is_wg());
    }

    /// **判据（S1-8）**：QUIC 类端点 encode/decode 往返 + **既有 WG 端点逐字节不变**。
    ///
    /// 「逐字节不变」的验法：同一端点表去掉 QUIC 条目后，载荷里的 **WG 段字节**必须与
    /// 含 QUIC 条目的那串**逐字节相同**（QUIC 只追加自己的段，不动前文）；同时
    /// `direct_endpoints`/`relay_endpoints` 的过滤结果不受 QUIC 条目影响。
    #[test]
    fn quic_endpoint_kind_round_trips_and_keeps_wg_bytes() {
        let peer_id = PeerId::from([0x51; 32]);
        let secret = Secret::from([0x52; 32]);
        let wg = [
            EndpointRef::new("192.168.3.12:41641", EndpointKind::Direct),
            EndpointRef::new("198.51.100.212:41741", EndpointKind::Relay),
        ];
        let with_quic = [
            wg[0], wg[1],
            EndpointRef::new("192.168.3.12:42652", EndpointKind::Quic),
            EndpointRef::new("203.0.113.7:42652", EndpointKind::Quic),
        ];
        let tok_wg = encode(&TokenSpec { peer_id: &peer_id, secret: &secret, endpoints: &wg, rpk: None })
            .expect("编码");
        let tok_quic = encode(&TokenSpec {
            peer_id: &peer_id,
            secret: &secret,
            endpoints: &with_quic,
            rpk: None,
        })
        .expect("编码");

        // ① WG 段字节不变：解出段序列的前两段（type+len+addr）逐字节相同
        let body = |s: &str| -> Vec<u8> {
            B64.decode(s.strip_prefix(PREFIX).unwrap()).expect("base64")
        };
        let segs = |b: &[u8], n: usize| -> Vec<u8> {
            let mut off = 65; // peer(32)+secret(32)+count(1)
            for _ in 0..n {
                let ln = b[off + 1] as usize;
                off += 2 + ln;
            }
            b[65..off].to_vec()
        };
        assert_eq!(segs(&body(&tok_wg), 2), segs(&body(&tok_quic), 2), "WG 段逐字节不变");
        assert_eq!(body(&tok_quic)[64], 4, "端点计数 = 4（含 2 条 QUIC）");

        // ② 往返：类别与地址逐条还原；过滤函数各归各族
        let t = decode(&tok_quic).expect("解析");
        assert_eq!(t.endpoints[0].kind, EndpointKind::Direct);
        assert_eq!(t.endpoints[1].kind, EndpointKind::Relay);
        assert_eq!(t.endpoints[2].kind, EndpointKind::Quic);
        assert_eq!(t.endpoints[3].addr, "203.0.113.7:42652");
        let raw_quic = body(&tok_quic);
        let t2 = parse_body(&raw_quic).expect("借用解析");
        assert_eq!(t2.direct_endpoints().count(), 1);
        assert_eq!(t2.relay_endpoints().count(), 1);
        assert_eq!(t2.quic_endpoints().count(), 2);
        assert_eq!(
            t2.quic_endpoints().map(|e| e.addr).collect::<Vec<_>>(),
            vec!["192.168.3.12:42652", "203.0.113.7:42652"]
        );
        // ③ 再编码逐字节稳定
        let refs: Vec<EndpointRef<'_>> = t
            .endpoints
            .iter()
            .map(|e| EndpointRef::new(e.addr.as_str(), e.kind))
            .collect();
        assert_eq!(
            encode(&TokenSpec { peer_id: &t.peer_id, secret: &t.secret, endpoints: &refs, rpk: None })
                .unwrap(),
            tok_quic
        );
    }
}
