//! rl1 中继凭据 token（铸造/解析）与 relay.key 管理。
//!
//! 语义真源 `baseline:pkg/proto/relaytoken.go` + `internal/relay/{token,cli}.go`。
//! 布局与 hmw1 完全相同（复用 `crate::token` 的 body 编解码），仅前缀不同：
//!
//! ```text
//! "rl1" ‖ base64url-raw( relayID(32)=SHA256(secret) ‖ relaySecret(32) ‖ epCount(1) ‖ [type+len+addr]* ‖ crc(4) )
//! ```
//!
//! 为什么有这一层：中继不需要预先知道任何后端身份——后端拿 token 注册即可，
//! 换后端/换身份都不用动中继（rl1 设计动机）。

use std::net::SocketAddr;

use sha2::Digest;

use crate::server::egress;
use crate::token::{self, Endpoint, EndpointKind, Token};

/// 中继密钥的公开标识（日志去重/排障用；不泄露密钥）。
pub fn relay_secret_id(secret: &[u8; 32]) -> [u8; 32] {
    sha2::Sha256::digest(secret).into()
}

/// rl1 前缀。
pub const PREFIX: &str = "rl1";

/// 铸一枚 rl1 token（中继启动时打印；端点 = 中继自己的地址，可多个）。
pub fn encode_relay_token(secret: &[u8; 32], endpoints: &[Endpoint]) -> Result<String, token::TokenError> {
    let eps: Vec<token::EndpointRef<'_>> = endpoints
        .iter()
        .map(|e| token::EndpointRef::new(e.addr.as_str(), e.kind))
        .collect();
    let s = token::encode(&token::TokenSpec {
        peer_id: &token::PeerId::from(relay_secret_id(secret)),
        secret: &token::Secret::from(*secret),
        endpoints: &eps,
    })?;
    // 前缀替换：encode 产 hmw1…，rl1 布局同体
    Ok(format!("{}{}", PREFIX, &s[token::PREFIX.len()..]))
}

/// 解析 rl1 token（后端 `--relay rl1…` 面）。
pub fn decode_relay_token(input: &str) -> Result<Token, token::TokenError> {
    let s = input.trim();
    if !s.starts_with(PREFIX) {
        return Err(token::TokenError::Malformed { reason: "缺少 rl1 前缀" });
    }
    let body = &s[PREFIX.len()..];
    // 复用 hmw1 的 body 解码（剥内嵌 \r\n + base64url + CRC + 布局）
    let raw = token::decode(&format!("{}{}", token::PREFIX, body))
        .map_err(|e| match e {
            token::TokenError::UnsupportedVersion { .. } => {
                token::TokenError::Malformed { reason: "rl1 载荷版本形态不符" }
            }
            other => other,
        })?;
    Ok(raw)
}

/// relay.key：加载或生成（0600；重启不变 ⇒ token 稳定，后端不用跟着改）。
/// 返回 (secret, created)。
pub fn load_or_create_secret(state_dir: &std::path::Path) -> std::io::Result<([u8; 32], bool)> {
    let path = state_dir.join("relay.key");
    if let Ok(b) = std::fs::read(&path) {
        if b.len() == 32 {
            let mut s = [0u8; 32];
            s.copy_from_slice(&b);
            return Ok((s, false));
        }
    }
    std::fs::create_dir_all(state_dir)?;
    let mut secret = [0u8; 32];
    getrandom::getrandom(&mut secret).map_err(std::io::Error::other)?;
    std::fs::write(&path, secret)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    Ok((secret, true))
}

/// 只读 relay.key（不生成——离线推算路径不得有「顺手造新钥」副作用）。
pub fn read_secret(state_dir: &std::path::Path) -> std::io::Result<Option<[u8; 32]>> {
    match std::fs::read(state_dir.join("relay.key")) {
        Ok(b) if b.len() == 32 => {
            let mut s = [0u8; 32];
            s.copy_from_slice(&b);
            Ok(Some(s))
        }
        Ok(b) => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("relay.key 长度 {} 非法", b.len()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// BuildToken 的产物：token 全文 + 端点串列表。
pub struct BuiltToken {
    pub token: String,
    pub endpoints: Vec<String>,
}

/// 把中继地址（advertise 优先，否则本机物理网卡**公网**地址）+ 实际端口 + 密钥编成
/// rl1 token。端口以实际监听口为准：advertise 只给 host 时补上实际端口；给了不同
/// 端口按它写（NAT 外口可不同）+ 一行告警（`ulogf` 面——属于「token 里的端口信息」，
/// 只在配置错误时出现一次）。
#[allow(clippy::too_many_arguments)]
pub fn build_token(
    secret: &[u8; 32],
    advertise: &str,
    port: u16,
    ulogf: &dyn Fn(&str),
    logf: &dyn Fn(&str),
) -> Result<BuiltToken, String> {
    let mut addrs: Vec<String> = Vec::new();
    for part in advertise.split(',') {
        let p = part.trim();
        if p.is_empty() {
            continue;
        }
        let (host, p_str) = p
            .rsplit_once(':')
            .ok_or_else(|| format!("--advertise {p:?} 不是 host:port"))?;
        let pn: u16 = p_str.parse().unwrap_or(0);
        if pn != 0 && pn != port {
            ulogf(&format!(
                "⚠️ --advertise {p:?} 的端口 {pn} 与实际监听口 {port} 不一致：token 里写的是 {pn} —— 除非前面有 NAT 端口映射，否则后端连不上"
            ));
        }
        addrs.push(format!("{host}:{p_str}"));
    }
    let mut skipped: Vec<String> = Vec::new();
    if addrs.is_empty() {
        // 自动探测只取公网地址（永远不把非公网 IP 写进 token）。
        // candidates 是 v4 面（R3 egress 同源）；v6 端点（含 /64 去重）承载 R5 启用。
        for ifi in egress::physical_candidates() {
            for a in ifi.addrs {
                let ap = SocketAddr::new(std::net::IpAddr::V4(a), port);
                if !egress::is_public_addr(std::net::IpAddr::V4(a)) {
                    skipped.push(ap.to_string());
                    continue;
                }
                addrs.push(ap.to_string());
            }
        }
    }
    if addrs.is_empty() {
        if !skipped.is_empty() {
            return Err(format!(
                "本机只有非公网地址（{}）—— token 里不放非公网 IP：公网中继请加 --advertise <公网IP:端口>；局域网内使用也请显式 --advertise <局域网IP:端口>",
                skipped.join("、")
            ));
        }
        return Err("没找到可公布的地址（用 --advertise 指定）".to_owned());
    }
    let endpoints: Vec<Endpoint> = addrs
        .iter()
        .map(|a| Endpoint { addr: a.clone(), kind: EndpointKind::Direct })
        .collect();
    let tok = encode_relay_token(secret, &endpoints).map_err(|e| e.to_string())?;
    let _ = logf;
    Ok(BuiltToken { token: tok, endpoints: addrs })
}

/// 公布的地址全在内网吗（提醒加 --advertise）。
pub fn all_private(addrs: &[String]) -> bool {
    addrs.iter().all(|a| match a.rsplit_once(':') {
        Some((host, _)) => match host.parse::<std::net::IpAddr>() {
            Ok(ip) => match ip {
                std::net::IpAddr::V4(v4) => v4.is_private() || v4.is_loopback(),
                std::net::IpAddr::V6(v6) => {
                    let seg = v6.segments();
                    (seg[0] & 0xfe00) == 0xfc00 || v6.is_loopback()
                }
            },
            Err(_) => false,
        },
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_token_roundtrip_and_id() {
        let secret = [0x44u8; 32];
        let eps = vec![Endpoint { addr: "203.0.113.9:41741".to_owned(), kind: EndpointKind::Direct }];
        let tok = encode_relay_token(&secret, &eps).unwrap();
        assert!(tok.starts_with("rl1"));
        let back = decode_relay_token(&tok).unwrap();
        assert_eq!(back.peer_id.as_bytes(), &relay_secret_id(&secret)[..]);
        assert_eq!(back.secret.as_bytes(), &secret);
        assert_eq!(back.endpoints.len(), 1);
        assert_eq!(back.endpoints[0].addr, "203.0.113.9:41741");
        // 非 rl1 前缀拒
        assert!(decode_relay_token("hmw1xxxx").is_err());
        assert!(decode_relay_token("").is_err());
        // Go 侧 hmw1 解码器吃 rl1 载荷——本函数只认 rl1（防两种凭证互串）
    }

    #[test]
    fn secret_persistence() {
        let dir = std::env::temp_dir().join(format!("rltest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (s1, created) = load_or_create_secret(&dir).unwrap();
        assert!(created);
        let (s2, created2) = load_or_create_secret(&dir).unwrap();
        assert!(!created2);
        assert_eq!(s1, s2, "重启不变 ⇒ token 稳定");
        assert_eq!(read_secret(&dir).unwrap(), Some(s1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn build_token_advertise_forms() {
        let secret = [0x01u8; 32];
        let quiet = |_: &str| {};
        let quiet2 = |_: &str| {};
        // advertise 显式给出：host:port（端口一致无告警）
        let b = build_token(&secret, "127.0.0.1:42741", 42741, &quiet, &quiet2).unwrap();
        assert_eq!(b.endpoints, vec!["127.0.0.1:42741".to_owned()]);
        assert!(b.token.starts_with("rl1"));
        // 非 host:port 形态报错
        assert!(build_token(&secret, "noport", 1, &quiet, &quiet2).is_err());
        // 端口不一致：按 advertise 写 + 告警一次（告警面由 CLI 实测覆盖；此处确保不炸）
        let warn = |_: &str| {};
        let b2 = build_token(&secret, "10.0.0.1:9999", 42741, &warn, &quiet2).unwrap();
        assert_eq!(b2.endpoints, vec!["10.0.0.1:9999".to_owned()]);
    }

    #[test]
    fn private_addr_hint() {
        assert!(all_private(&["192.168.1.2:1".to_owned(), "127.0.0.1:2".to_owned()]));
        assert!(!all_private(&["192.168.1.2:1".to_owned(), "8.8.8.8:2".to_owned()]));
    }
}
