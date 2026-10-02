//! hmw1 凭证（token）：可见前缀 + base64url(裸二进制)。
//!
//! 布局（语义真源 `baseline:pkg/proto/token.go`，基线 621fe0e）：
//!
//! ```text
//! "hmw1" ‖ base64url-raw( peerId(32B) ‖ secret(32B) ‖ epCount(1B) ‖ [type(1B)+len(1B)+addr]* ‖ crc(4B) )
//! ```
//!
//! - `peerId` = 后端静态 WG 公钥；`secret` = 凭证种子；`type` 0=direct 1=relay；
//!   `crc` = SHA-256(前文)[:4]；base64 为**无填充** base64url（尾缀 `=` 不容忍，FIX-89）。
//! - 解析先 `trim`（Go `strings.TrimSpace` 同义：Unicode White_Space）。
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EndpointKind {
    Direct,
    Relay,
}

impl EndpointKind {
    /// 线上字节：Direct=0、Relay=1。
    pub fn to_wire(self) -> u8 {
        match self {
            EndpointKind::Direct => 0,
            EndpointKind::Relay => 1,
        }
    }
    /// 线上字节→类别。Go 同义宽松语义：**非 1 一律按 Direct 收**（不报错）。
    pub fn from_wire(b: u8) -> Self {
        if b == 1 {
            EndpointKind::Relay
        } else {
            EndpointKind::Direct
        }
    }
}

macro_rules! byte_array_newtype {
    ($(#[$doc:meta])* $name:ident, $redacted:expr) => {
        $(#[$doc])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
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

byte_array_newtype!(
    /// 后端静态 WG 公钥（token 载荷首 32B；设备身份按它派生）。
    PeerId,
    false
);
byte_array_newtype!(
    /// 凭证种子（注册 HMAC / WG PSK / 隧道地址派生输入）。
    ///
    /// 敏感材料：Debug 全脱敏；R2 接入 zeroize（Drop 擦除）——R0 阶段保持 Copy 便于向量对账。
    Secret,
    true
);

/// 借用形态端点：地址直接来自载荷字节，零拷贝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct EndpointRef<'a> {
    /// `host:port`——host 可为 IP（含 `[v6]` 括号形）或域名。
    pub addr: &'a str,
    pub kind: EndpointKind,
}

/// 借用形态 token：对**已 base64 解码的载荷**借用解析（端点地址零拷贝）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct TokenRef<'a> {
    peer_id: [u8; 32],
    secret: [u8; 32],
    endpoints: Vec<EndpointRef<'a>>,
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
    if !rest.is_empty() {
        return Err(TokenError::Malformed {
            reason: "载荷尾部有多余字节",
        });
    }
    Ok(TokenRef {
        peer_id,
        secret,
        endpoints,
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
        let raw = B64.decode(tok[PREFIX.len()..].as_bytes()).unwrap();
        let parsed = parse_body(&raw).unwrap();
        let spec2 = TokenSpec {
            peer_id: &parsed.peer_id(),
            secret: &parsed.secret(),
            endpoints: parsed.endpoints(),
        };
        assert_eq!(encode(&spec2).unwrap(), tok);
        assert_eq!(parsed.direct_endpoints().count(), 1);
        assert_eq!(parsed.relay_endpoints().count(), 1);
    }

    /// L3：线字节宽松语义——非 1 的 type 字节一律按 Direct（Go 同义）。
    #[test]
    fn endpoint_kind_from_wire_is_lenient_like_go() {
        assert_eq!(EndpointKind::from_wire(0), EndpointKind::Direct);
        assert_eq!(EndpointKind::from_wire(1), EndpointKind::Relay);
        for odd in [2, 7, 0xff] {
            assert_eq!(EndpointKind::from_wire(odd), EndpointKind::Direct);
        }
    }
}
