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
//! 1. Go `DecodeToken("hmw")` 会 panic（`s[:4]` 越界，已登记 Go 侧问题清单）；本实现
//!    返回 [`TokenError::UnsupportedVersion`]。
//! 2. 端点地址字节须为 UTF-8（Go 接受任意字节串）；非 UTF-8 → [`TokenError::Malformed`]。
//!
//! 解析形态：`parse_body` 对**已解码的载荷字节**借用解析（端点地址零拷贝 `&str`）；
//! `decode` 为便捷入口（解码 base64 + 解析 + 转移交所有权）。

use core::fmt;
use std::sync::LazyLock;

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
/// base64 crate 默认查，须显式放开才能逐字节同判）。
static B64: LazyLock<GeneralPurpose> = LazyLock::new(|| {
    GeneralPurpose::new(
        &URL_SAFE,
        base64::engine::GeneralPurposeConfig::new()
            .with_decode_padding_mode(base64::engine::DecodePaddingMode::RequireNone)
            .with_decode_allow_trailing_bits(true)
            .with_encode_padding(false),
    )
});

/// 三类可区分失败，与 Go 哨兵错误（token.go:33-37）一一对应；
/// `Malformed` 的 `reason` 供诊断，不参与与 Go 的对齐面。
#[derive(Debug, Error, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TokenError {
    /// `homeway/token: 不支持的 token 版本`
    #[error("homeway/token: 不支持的 token 版本")]
    UnsupportedVersion,
    /// `homeway/token: 校验失败（串被截断或损坏）`
    #[error("homeway/token: 校验失败（串被截断或损坏）")]
    Corrupted,
    /// `homeway/token: 格式非法`
    #[error("homeway/token: 格式非法：{reason}")]
    Malformed { reason: &'static str },
}

/// 端点类型字节（载荷里的 `type`）。
pub mod endpoint_type {
    pub const DIRECT: u8 = 0;
    pub const RELAY: u8 = 1;
}

macro_rules! byte_array_newtype {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        ///
        /// Debug 面只出 4B 短指纹 hex（同 Go 日志纪律，敏感/大材料不进 `%{:#?}`）。
        #[derive(Clone, Copy, PartialEq, Eq, Hash)]
        pub struct $name([u8; 32]);

        impl $name {
            pub(crate) fn from_array(v: [u8; 32]) -> Self {
                Self(v)
            }
            /// 借视图（不拷贝）。
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                let short: String = self.0.iter().take(4).map(|b| format!("{b:02x}")).collect();
                write!(f, "{}({short}…)", stringify!($name))
            }
        }
    };
}

byte_array_newtype!(
    /// 后端静态 WG 公钥（token 载荷首 32B；设备身份按它派生）。
    PeerId
);
byte_array_newtype!(
    /// 凭证种子（注册 HMAC / WG PSK / 隧道地址派生输入）。
    ///
    /// 敏感材料：R2 接入 zeroize（Drop 擦除）——R0 阶段保持 Copy 便于向量对账。
    Secret
);

/// 借用形态端点：地址直接来自载荷字节，零拷贝。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndpointRef<'a> {
    /// `host:port`——host 可为 IP 或域名。
    pub addr: &'a str,
    pub relay: bool,
}

/// 借用形态 token：对**已 base64 解码的载荷**借用解析（端点地址零拷贝）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TokenRef<'a> {
    peer_id: [u8; 32],
    secret: [u8; 32],
    endpoints: Vec<EndpointRef<'a>>,
}

impl<'a> TokenRef<'a> {
    pub fn peer_id(&self) -> PeerId {
        PeerId::from_array(self.peer_id)
    }
    pub fn secret(&self) -> Secret {
        Secret::from_array(self.secret)
    }
    pub fn endpoints(&self) -> &[EndpointRef<'a>] {
        &self.endpoints
    }
    /// 直连端点（Go `DirectEndpoints`）。
    pub fn direct_endpoints(&self) -> impl Iterator<Item = EndpointRef<'a>> + '_ {
        self.endpoints.iter().copied().filter(|e| !e.relay)
    }
    /// 中继端点（Go `RelayEndpoints`）。
    pub fn relay_endpoints(&self) -> impl Iterator<Item = EndpointRef<'a>> + '_ {
        self.endpoints.iter().copied().filter(|e| e.relay)
    }
}

/// 所有权形态端点（会话/配置持有面）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub addr: String,
    pub relay: bool,
}

/// 所有权形态 token（对应 Go `proto.Token`）。
#[derive(Debug, Clone, PartialEq, Eq)]
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
                .iter()
                .map(|e| Endpoint {
                    addr: e.addr.to_owned(),
                    relay: e.relay,
                })
                .collect(),
        }
    }
}

/// 解析整串 token：trim → 版本前缀 → base64url 解码 → [`parse_body`]。
pub fn decode(input: &str) -> Result<Token, TokenError> {
    let s = input.trim();
    if s.starts_with("hmw") && !s.starts_with(PREFIX) {
        // Go 侧此处 s[:4] 对恰 "hmw" 长度会 panic（已登记 Go 侧问题清单）；本实现安全返回
        return Err(TokenError::UnsupportedVersion);
    }
    if !s.starts_with(PREFIX) {
        return Err(TokenError::Malformed {
            reason: "缺少 hmw1 前缀",
        });
    }
    let raw = B64
        .decode(s[PREFIX.len()..].as_bytes())
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
    let (&ep_count, mut rest) = rest
        .split_first()
        .ok_or(TokenError::Corrupted)?; // 不可达（MIN_BODY_LEN 已含 1B）；防御性收口

    let mut endpoints = Vec::with_capacity(usize::from(ep_count));
    for _ in 0..ep_count {
        if rest.len() < 2 {
            return Err(TokenError::Malformed {
                reason: "端点头不足（type+len）",
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
            // Go 同义：typ==RELAY 才是中继，其余值一律按 direct 收（不报错）
            relay: typ == endpoint_type::RELAY,
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
        buf.push(if e.relay {
            endpoint_type::RELAY
        } else {
            endpoint_type::DIRECT
        });
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
        assert_eq!(decode("hmw"), Err(TokenError::UnsupportedVersion)); // Go 侧此串 panic（已登记差异）
        assert_eq!(
            decode("rl1AAAA"),
            Err(TokenError::Malformed {
                reason: "缺少 hmw1 前缀"
            })
        );
    }

    #[test]
    fn encode_decode_roundtrip_is_byte_stable() {
        let mut peer = [0u8; 32];
        peer[0] = 0x11;
        let mut secret = [0u8; 32];
        secret[31] = 1;
        let eps = [
            EndpointRef {
                addr: "127.0.0.1:42641",
                relay: false,
            },
            EndpointRef {
                addr: "198.51.100.212:41741",
                relay: true,
            },
        ];
        let spec = TokenSpec {
            peer_id: &PeerId::from_array(peer),
            secret: &Secret::from_array(secret),
            endpoints: &eps,
        };
        let tok = encode(&spec).unwrap();
        let decoded = decode(&tok).unwrap();
        assert_eq!(decoded.peer_id.as_bytes(), &peer);
        assert_eq!(decoded.secret.as_bytes(), &secret);
        assert_eq!(decoded.endpoints.len(), 2);
        assert_eq!(decoded.endpoints[0].addr, "127.0.0.1:42641");
        assert!(!decoded.endpoints[0].relay);
        assert_eq!(decoded.endpoints[1].addr, "198.51.100.212:41741");
        assert!(decoded.endpoints[1].relay);
        // 借用面再编码 → 与原串逐字节一致
        let raw = B64.decode(tok[PREFIX.len()..].as_bytes()).unwrap();
        let parsed = parse_body(&raw).unwrap();
        let spec2 = TokenSpec {
            peer_id: &parsed.peer_id(),
            secret: &parsed.secret(),
            endpoints: parsed.endpoints(),
        };
        assert_eq!(encode(&spec2).unwrap(), tok);
        // direct/relay 过滤器
        assert_eq!(parsed.direct_endpoints().count(), 1);
        assert_eq!(parsed.relay_endpoints().count(), 1);
    }
}
