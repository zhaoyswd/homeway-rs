//! `homeway-cli serve` 命令面（R3-3f；语义真源 `cmd/homeway` 的 serve 子命令 +
//! `internal/daemon/servegroup_cli.go` 的 token 族——裁剪面：无 daemon 期望态，
//! 只有前台 `serve` 与纯读/纯文件操作的 `serve token [list|revoke]`）。
//!
//! 配置覆盖序 = flag 显式设值 > config.toml（`<state>/config.toml`，[serve] 节同
//! schema）> 内置默认（nodeconfig 口径）。`--state` 是引导 flag，不进 config。
//!
//! **Q-H F1（config 单表）**：本文件的 `FileConfig` 是**唯一 schema**（统一进程与
//! relay 侧同表复用）；`load_config_strict` 是**唯一读点**（缺失 = 默认；存在即
//! 「TOML 严格解析（deny_unknown）+ Go `validateFile` 全量值域」）；`serve_config_of`
//! 是纯映射（无打印/无 exit）。`process::exit` 只允许出现在**最前台**（flag parser
//! 的 exit(2) 与前台壳的 exit(1)）——控制面路径与角色装配路径零 exit。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use homeway_core::server::engine::{BindMode, ServeConfig, ServeEngine};

use crate::cli_flags;

// ---------- config.toml 单表（Q-H F1；唯一 schema） ----------

fn default_enabled_true() -> bool {
    true
}

/// config.toml 的 `[serve]` 节（fileServe 子集；deny_unknown = Go 的 typo 保护同口径）。
///
/// **缺省语义（Go `nodeconfig.Default()` 同义）**：`serve.enabled` 缺省 = **true**
/// （手编省略该键 = 启用）；`relay.enabled` 缺省 = false。手写 `impl Default` 而非
/// derive——serde 的 `default = fn` 只在反序列化时生效，`FileConfig::default()`（缺失
/// 文件路径）也必须同义。
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileServe {
    /// 缺省 = true（Go `nodeconfig.Default()` 同义——手编省略该键 = 启用）。
    #[serde(default = "default_enabled_true")]
    pub(crate) enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) listen: Option<u16>,
    /// QUIC 独立 UDP 端口（M1 §1.1）：缺省 = `serve.listen + 1`；被占用按 WG 同款退让。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quic_listen: Option<u16>,
    /// QUIC 面总开关（M1 S3-4）：缺省 = **true**。`false` ⇒ 不监听 QUIC 端口、不打
    /// E-q1/E-q4 行、token **不带** QUIC 端点与 `rpk` 尾字段（⇒ token 串与 M1 前逐字节
    /// 相同——Go 客户端 × Rust 出口的全量矩阵行 L4/L5 的可保命形态）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quic: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) bind_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) upnp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stun: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stun6: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) relay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) max_peers: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) peer_ttl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) public_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dns_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) files_root: Option<String>,
    /// Go 键表含 `[[serve.ddns]]`（serve ddns add 写出）——P0-4 起消费：token
    /// 叠加域名条目 + 自检。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ddns: Option<Vec<FileDdns>>,
    /// Q-J F2：DNS 显式上游覆盖（`ip` 或 `ip:port` 列表；缺省/空列表 = 跟随
    /// `/etc/resolv.conf`——spec MUST 的默认面不动；**opt-in 偏离登记**见
    /// `docs/INTEROP-CRITERIA.md`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dns_upstream: Option<Vec<String>>,
    /// Q-J F2：兜底上游（`ip` 或 `ip:port`；缺省 `223.5.5.5`；**空串 = 拒启**——
    /// 「空 = 关兜底」不是本键语义）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dns_fallback: Option<String>,
    /// Q-J F2：DDNS 自检直查解析器（IPv4 `ip[:port]` 列表；空 = 默认常量表）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) ddns_resolver: Option<Vec<String>>,
    /// Q-J F2：挑卡/健康探针/udpcap `DNS:53` 能力位目标（IPv4 `ip[:port]` 列表；
    /// 空 = 默认常量表；**一键喂三路是显式登记的耦合**）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) dns_probe_target: Option<Vec<String>>,
    /// Q-J F2：通用 UDP（非 53）能力位目标（IPv4 `ip:port` 列表——**须显式带端口**；
    /// 空 = 默认常量表）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) stun_probe_target: Option<Vec<String>>,
    /// 发送整形/pacing（D-3 反过拟合约束 3：参数 config 化——键表 =
    /// `homeway_core::server::intercept::TxShapeCfg`；覆盖序 env > config > 默认）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) tx_shape: Option<homeway_core::server::intercept::TxShapeCfg>,
    /// 抗放大闸（M2 §3.2 的 `[serve.quic_admit]`；缺省 = 设计定值 ⇒ 缺省不改行为）。
    /// **值域非法 ⇒ 拒启**（`serve` 节严格表纪律；与 env 面的「记行 + 缺省」不同面）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) quic_admit: Option<FileQuicAdmit>,
}

/// `[serve.quic_admit]` 节（M2 §3.2 表：**六行七键**——`per_src_fails`/`per_src_window`
/// 同行两键）。时长照 `serve.peer_ttl` 的 Go 时长串口径（如 `"5s"`/`"1h"`）。
#[derive(serde::Deserialize, serde::Serialize, Clone, Debug, Default)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileQuicAdmit {
    /// Retry token 有效期（`1s..=60s`；缺省 `5s`——收自 quinn 缺省 15s，M2 §3.1 登记）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retry_token_lifetime: Option<String>,
    /// 每源滑动窗上限（`1..=1000`；缺省 **16**——M2 §14-1 裁决 `10 → 16`，真源 = `admit.rs`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) per_src_fails: Option<u32>,
    /// 每源滑动窗窗长（`1s..=1h`；缺省 `10s`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) per_src_window: Option<String>,
    /// nonce 有效期（`1s..=30s`；缺省 `5s`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) nonce_ttl: Option<String>,
    /// 准入总期限（`1s..=60s`；缺省 `10s`）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) admit_deadline: Option<String>,
    /// 证明失败闸阈值（`0..=1000`；缺省 `10`；`0` = 关闭该闸）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) proof_fail_threshold: Option<u32>,
    /// Retry 策略（枚举 `pressure`（缺省）| `always` | `never`；非法值 ⇒ 拒启）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) retry_policy: Option<String>,
}

impl FileQuicAdmit {
    /// 解析 + 值域校验 ⇒ `AdmitLimits`（**唯一收口点**：`validate_file` 与 `serve_config_of`
    /// 都走它——语法错与越界都在这里变成拒启文案；值域真源 = `AdmitLimits::validate`）。
    pub(crate) fn resolve(
        &self,
    ) -> Result<homeway_core::server::quic_admit::AdmitLimits, String> {
        use homeway_core::server::quic_admit::{AdmitLimits, RetryPolicy};
        let dur = |name: &str, raw: &str| -> Result<Duration, String> {
            parse_go_duration(raw).ok_or_else(|| {
                format!("serve.quic_admit.{name}：{raw:?} 非法（时长串，如 \"5s\"/\"10m\"/\"1h\"）")
            })
        };
        let mut l = AdmitLimits::default();
        if let Some(v) = &self.retry_token_lifetime {
            l.retry_token_lifetime = dur("retry_token_lifetime", v)?;
        }
        if let Some(v) = self.per_src_fails {
            l.per_src_fails = v;
        }
        if let Some(v) = &self.per_src_window {
            l.per_src_window = dur("per_src_window", v)?;
        }
        if let Some(v) = &self.nonce_ttl {
            l.nonce_ttl = dur("nonce_ttl", v)?;
        }
        if let Some(v) = &self.admit_deadline {
            l.admit_deadline = dur("admit_deadline", v)?;
        }
        if let Some(v) = self.proof_fail_threshold {
            l.proof_fail_threshold = v;
        }
        if let Some(v) = &self.retry_policy {
            l.retry_policy = RetryPolicy::parse(v).ok_or_else(|| {
                format!(
                    "serve.quic_admit.retry_policy：{v:?} 非法（合法取值 {}）",
                    RetryPolicy::VALUES.join("|")
                )
            })?;
        }
        l.validate()?;
        Ok(l)
    }
}

impl Default for FileServe {
    fn default() -> Self {
        FileServe { enabled: true, ..FileServe::empty() }
    }
}

impl FileServe {
    /// 全字段零值（仅内部装配用；对外缺省见 `Default`）。
    fn empty() -> FileServe {
        FileServe {
            enabled: false,
            listen: None,
            quic_listen: None,
            quic: None,
            bind_interface: None,
            upnp: None,
            stun: None,
            stun6: None,
            relay: None,
            max_peers: None,
            peer_ttl: None,
            public_endpoint: None,
            dns_port: None,
            files_root: None,
            ddns: None,
            dns_upstream: None,
            dns_fallback: None,
            ddns_resolver: None,
            dns_probe_target: None,
            stun_probe_target: None,
            tx_shape: None,
            quic_admit: None,
        }
    }
}

#[derive(serde::Deserialize, serde::Serialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileDdns {
    #[serde(default)]
    pub(crate) domain: String,
}

#[derive(serde::Deserialize, serde::Serialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileRelay {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) advertise: Option<String>,
}

/// config.toml 全键面（serve/relay 双节；deny_unknown——typo 保护）。
#[derive(serde::Deserialize, serde::Serialize, Default, Clone, Debug)]
#[serde(deny_unknown_fields)]
pub(crate) struct FileConfig {
    #[serde(default)]
    pub(crate) serve: FileServe,
    #[serde(default)]
    pub(crate) relay: FileRelay,
}

/// 严格读 config（**唯一读点**；Q-H F1）：缺失 = 默认；存在即「TOML 严格解析 +
/// Go `validateFile` 全量值域」——错误文案 = `config.toml 绝对路径: 字段：值域`
/// （Go `Error.String` 同形）。写回面与启动期共用本函数（坏 config 拒写/拒启）。
pub(crate) fn load_config_strict(state_dir: &Path) -> Result<FileConfig, String> {
    let path = state_dir.join("config.toml");
    let body = match std::fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(FileConfig::default()),
        Err(e) => return Err(format!("{}: 读取失败：{e}", path.display())),
    };
    let fc: FileConfig = toml::from_str(&body).map_err(|e| format!("{}: {e}", path.display()))?;
    validate_file(&path, &fc)?;
    Ok(fc)
}

/// `ip` 或 `ip:port` → `SocketAddr`（缺端口补 `default_port`；非法 None）。F2 配置面
/// 的边界解析单源（**不用 `filter_map(parse().ok())` 静默丢项**——非法即拒启）。
fn parse_addr_with_default_port(s: &str, default_port: u16) -> Option<std::net::SocketAddr> {
    let t = s.trim();
    if let Ok(ip) = t.parse::<std::net::IpAddr>() {
        return Some(std::net::SocketAddr::new(ip, default_port));
    }
    t.parse::<std::net::SocketAddr>().ok()
}

/// `ip[:port]` → `SocketAddrV4`（F2 三个 IPv4-only 键：v6/非法一律 None）。
fn parse_v4_addr(s: &str, default_port: u16) -> Option<std::net::SocketAddrV4> {
    match parse_addr_with_default_port(s, default_port)? {
        std::net::SocketAddr::V4(v4) => Some(v4),
        std::net::SocketAddr::V6(_) => None,
    }
}

/// Go `validateFile`（`baseline:internal/nodeconfig/config.go:254-292`）全量值域，
/// 逐条对齐（本批补齐 5 项：serve.listen / bind_interface / public_endpoint /
/// serve.relay / relay.listen）。
fn validate_file(path: &Path, f: &FileConfig) -> Result<(), String> {
    let p = path.display();
    let bad = |field: &str, detail: String| Err(format!("{p}: {field}：{detail}"));
    if let Some(v) = f.serve.listen {
        if v == 0 {
            return bad("serve.listen", format!("{v} 非法（合法值域 1–65535）"));
        }
    }
    if let Some(v) = f.serve.quic_listen {
        if v == 0 {
            return bad("serve.quic_listen", format!("{v} 非法（合法值域 1–65535）"));
        }
    }
    // dns_port：Option<u16> 天然等价 Go 的 0–65535（登记为「已等价」）。
    if let Some(v) = &f.serve.bind_interface {
        validate_bind_interface(v).map_err(|e| {
            format!("{p}: serve.bind_interface：{e}")
        })?;
    }
    if let Some(v) = &f.serve.public_endpoint {
        if !v.is_empty() {
            for line in v.split(',') {
                let t = line.trim();
                if let Err(e) = t.parse::<std::net::SocketAddr>() {
                    return bad(
                        "serve.public_endpoint",
                        format!("{line:?} 非法（{e}；须为逗号分隔的 ip:port）"),
                    );
                }
            }
        }
    }
    if let Some(v) = &f.serve.peer_ttl {
        if parse_go_duration(v).is_none() {
            return bad(
                "serve.peer_ttl",
                format!("{v:?} 非法（时长串，如 \"168h\"；须 ≥ 0，0 = 关闭 TTL 回收）"),
            );
        }
    }
    // [serve.quic_admit]（M2 §3.2）：语法 + 值域都在 `FileQuicAdmit::resolve` 内收口；
    // 越界/非法 ⇒ **拒启**（`serve` 节严格表纪律——与 env 面的「记行 + 缺省」不同面）。
    if let Some(q) = &f.serve.quic_admit {
        q.resolve().map_err(|e| format!("{p}: {e}"))?;
    }
    if let Some(list) = &f.serve.ddns {
        for d in list {
            // 代码门 L7：Go `validateFile` 查**未 trim 的原串**（`" x"` 因含空格被拒）——
            // 此处不 trim 再查（存储面仍按 trim 后落 cfg，接受集与 Go 等价）。
            let dom = d.domain.as_str();
            if dom.is_empty() {
                return bad("serve.ddns.domain", "空域名非法".to_owned());
            }
            if dom.contains(':') || dom.contains('/') || dom.contains(' ') {
                return bad(
                    "serve.ddns.domain",
                    format!("{dom:?} 非法（只要裸域名，不带端口/路径）"),
                );
            }
        }
    }
    if let Some(v) = &f.serve.relay {
        validate_relay_arg(v).map_err(|e| format!("{p}: serve.relay：{e}"))?;
    }
    // ---- Q-J F2 五键值域（非法即拒启 + 可行动文案）----
    if let Some(list) = &f.serve.dns_upstream {
        for (i, item) in list.iter().enumerate() {
            if !matches!(parse_addr_with_default_port(item, 53), Some(a) if a.port() != 0) {
                return bad(
                    "serve.dns_upstream",
                    format!("第 {} 项 {item:?} 非法（ip 或 ip:port 且端口非 0；空列表 = 跟随 /etc/resolv.conf）", i + 1),
                );
            }
        }
    }
    if let Some(v) = &f.serve.dns_fallback {
        if v.trim().is_empty() {
            return bad(
                "serve.dns_fallback",
                "空串非法（删掉该键 = 默认 223.5.5.5；空串不表示「关兜底」）".to_owned(),
            );
        }
        if !matches!(parse_addr_with_default_port(v, 53), Some(a) if a.port() != 0) {
            return bad("serve.dns_fallback", format!("{v:?} 非法（ip 或 ip:port 且端口非 0）"));
        }
    }
    if let Some(list) = &f.serve.ddns_resolver {
        for (i, item) in list.iter().enumerate() {
            if !matches!(parse_v4_addr(item, 53), Some(a) if a.port() != 0) {
                return bad(
                    "serve.ddns_resolver",
                    format!("第 {} 项 {item:?} 非法（IPv4 ip 或 ip:port 且端口非 0；空列表 = 默认常量表）", i + 1),
                );
            }
        }
    }
    if let Some(list) = &f.serve.dns_probe_target {
        for (i, item) in list.iter().enumerate() {
            if !matches!(parse_v4_addr(item, 53), Some(a) if a.port() != 0) {
                return bad(
                    "serve.dns_probe_target",
                    format!("第 {} 项 {item:?} 非法（IPv4 ip 或 ip:port 且端口非 0；空列表 = 默认常量表）", i + 1),
                );
            }
        }
    }
    if let Some(list) = &f.serve.stun_probe_target {
        for (i, item) in list.iter().enumerate() {
            if !matches!(item.trim().parse::<std::net::SocketAddrV4>(), Ok(a) if a.port() != 0) {
                return bad(
                    "serve.stun_probe_target",
                    format!("第 {} 项 {item:?} 非法（IPv4 **ip:port** 且端口非 0——须显式带端口，如 162.159.207.1:3478）", i + 1),
                );
            }
        }
    }
    let listen = f.relay.listen.as_deref().unwrap_or(":41741");
    validate_relay_listen(listen).map_err(|e| format!("{p}: relay.listen：{e}"))?;
    Ok(())
}

/// serve.relay 取值（CLI 与 config 同口径；Go `validateRelayToken` 同义）：
/// 空 = 不用中继；`rl1` 前缀 = 必须能解码；其余 = 裸 IP:port 开放模式。
pub(crate) fn validate_relay_arg(tok: &str) -> Result<(), String> {
    let t = tok.trim();
    if t.is_empty() {
        return Ok(());
    }
    if t.starts_with("rl1") {
        return homeway_core::relay::rltoken::decode_relay_token(t)
            .map(|_| ())
            .map_err(|e| format!("rl1 token 解码失败：{e}"));
    }
    if let Ok(ap) = t.parse::<std::net::SocketAddr>() {
        if ap.port() != 0 {
            return Ok(());
        }
    }
    Err(format!("{tok:?} 非法（rl1… token 或裸 IP:port；域名不支持）"))
}

/// relay.listen 形态（`[host:]port`，端口 1–65535；Go `validateUDPAddr` 同义——
/// 复用 relay 装配期的 `parse_listen`，验收集与装配面一致）。
pub(crate) fn validate_relay_listen(v: &str) -> Result<(), String> {
    if v.is_empty() {
        return Err("空地址非法（如 \":41741\"）".to_owned());
    }
    match crate::relay_cli::parse_listen(v) {
        Some(a) if a.port() >= 1 => Ok(()),
        Some(a) => Err(format!("端口 {} 非法（合法值域 1–65535）", a.port())),
        None => Err(format!("{v:?} 非法（[host:]port 形态，如 \":41741\"）")),
    }
}

/// Go `validateBindInterface`：空/auto/none/off/no（大小写不敏感）、IP 字面量、
/// 其余按网卡名放行（含 `:/ \t` 拒）。
fn validate_bind_interface(v: &str) -> Result<(), String> {
    let t = v.trim();
    match t.to_ascii_lowercase().as_str() {
        "" | "auto" | "none" | "off" | "no" => return Ok(()),
        _ => {}
    }
    if t.parse::<std::net::IpAddr>().is_ok() {
        return Ok(());
    }
    if t.contains(':') || t.contains('/') || t.contains(' ') || t.contains('\t') {
        return Err(format!("{v:?} 非法（auto / none / 网卡名 / IP 字面量）"));
    }
    Ok(())
}

/// Go 时长串（"15s"/"168h"/"0s"；支持 s/m/h 组合）。
pub(crate) fn parse_go_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s == "0" || s == "0s" {
        return Some(Duration::ZERO);
    }
    let mut total = Duration::ZERO;
    let mut rest = s;
    while !rest.is_empty() {
        let num_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if num_end == 0 {
            return None;
        }
        let n: u64 = rest[..num_end].parse().ok()?;
        let unit = rest[num_end..].chars().next()?;
        let mult = match unit {
            's' => 1,
            'm' => 60,
            'h' => 3600,
            _ => return None,
        };
        total += Duration::from_secs(n * mult);
        rest = &rest[num_end + 1..];
    }
    Some(total)
}

// ---------- flag 解析（前台面；exit(2) 允许——最前台） ----------

/// CLI 层错误分类：Usage ⇒ 前台 exit 2；Config ⇒ 前台 exit 1（控制面路径不调用本层）。
#[derive(Debug)]
pub(crate) enum CliErr {
    Usage(String),
    Config(String),
}

#[derive(Default)]
struct ServeFlags {
    state: Option<PathBuf>,
    listen: Option<u16>,
    bind_interface: Option<String>,
    upnp: Option<bool>,
    /// QUIC 面总开关（`--quic[=bool]`；M1 S3-4——本地/CI 起 WG-only 出口用）。
    quic: Option<bool>,
    stun: Option<String>,
    stun6: Option<String>,
    peer_ttl: Option<Duration>,
    max_peers: Option<usize>,
    public_endpoint: Option<String>,
    dns_port: Option<u16>,
    files_root: Option<String>,
    verbose: bool,
    relay: Option<String>,
    /// DDNS 域名（`--ddns` 单值 flag：显式给 = 覆盖 config 的**全部**条目——
    /// 一次性覆盖语义，Go cli.go:127-134 同口径）。
    ddns: Option<String>,
    /// 位置参数（serve 不接受）。
    extra: Vec<String>,
}

fn serve_usage() {
    eprintln!("用法：homeway-cli serve [--state DIR] [--listen P] [--bind-interface M] [--upnp[=bool]] [--quic[=bool]] [--stun H:P] [--stun6 H:P]");
    eprintln!("       [--relay rl1…|ip:port] [--peer-ttl 168h] [--max-peers N] [--public-endpoint ip:port,ip:port]");
    eprintln!("       [--dns-port P] [--files-root DIR] [--ddns 裸域名] [--verbose]");
    eprintln!("  --stun= / --stun6= / --relay= 空值 = 关（Go flag 空串同义，F0 carve-out）；");
    eprintln!("  --bind-interface= 空值 = auto（Go ResolveBind(\"\") 同义，Q-L carve-out）；");
    eprintln!("  = 前台单出口（Ctrl-C 收工）；启停/查询用 `homeway-cli serve start|stop|restart|status|token`（控制面）。");
}

fn parse_serve_flags(args: &[String]) -> Result<ServeFlags, CliErr> {
    let mut f = ServeFlags::default();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(a) else {
            f.extra.push(a.to_owned());
            i += 1;
            continue;
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        // 取值 flag：取下一个 token（内联形态不消费）——缺值/空值由 cli_flags fail-fast。
        let take_next = |adv: &mut usize| {
            if inline.is_none() {
                *adv = 2;
            }
        };
        match name {
            "state" => {
                f.state = Some(cli_flags::take_state_or_exit("state", inline, next));
                take_next(&mut adv);
            }
            // 数值 flag 非法即报错退出（Go flag 包同语义）——静默回退默认值会让
            // 「--listen 127.0.0.1:42671」这类形态占到非预期端口
            "listen" => {
                let v = cli_flags::take_num_or_exit::<u16>(
                    "listen",
                    inline,
                    next,
                    "仅收端口数字 1-65535，如 41641；不收 ip:port；0 不收——Go 同口径",
                );
                if v == 0 {
                    return Err(CliErr::Usage(
                        "--listen 非法（0——仅收端口数字 1-65535，如 41641；不收 ip:port；0 不收——Go 同口径）"
                            .to_owned(),
                    ));
                }
                f.listen = Some(v);
                take_next(&mut adv);
            }
            "bind-interface" => {
                // Q-L L3 carve-out（第五个）：`--bind-interface=`/`--bind-interface ""` 空值
                // Go = auto（`ResolveBind("")` 先 TrimSpace 再判空 ⇒ `BindAuto`，
                // `baseline/homeway/internal/server/cli.go:190-198`）——修前 Rust exit 2。
                f.bind_interface =
                    Some(cli_flags::take_value_empty_ok_or_exit("bind-interface", inline, next));
                take_next(&mut adv);
            }
            "upnp" => {
                f.upnp = Some(cli_flags::take_bool_or_exit("upnp", inline, true));
            }
            "quic" => {
                f.quic = Some(cli_flags::take_bool_or_exit("quic", inline, true));
            }
            "stun" => {
                // F0 carve-out：空值 = 关公网 STUN 观测（Go flag 空串同义；实测
                // `bin/homeway-go serve --stun=` 正常起服）。缺值仍 fail-fast。
                f.stun = Some(cli_flags::take_value_empty_ok_or_exit("stun", inline, next));
                take_next(&mut adv);
            }
            "stun6" => {
                f.stun6 = Some(cli_flags::take_value_empty_ok_or_exit("stun6", inline, next));
                take_next(&mut adv);
            }
            "peer-ttl" => {
                let v = cli_flags::take_value_or_exit("peer-ttl", inline, next, false);
                take_next(&mut adv);
                match parse_go_duration(&v) {
                    Some(d) => f.peer_ttl = Some(d),
                    None => {
                        return Err(CliErr::Usage(format!(
                            "--peer-ttl 非法（{v:?}——时长串，如 15s / 168h；0 = 关闭）"
                        )));
                    }
                }
            }
            "max-peers" => {
                f.max_peers = Some(cli_flags::take_num_or_exit::<usize>(
                    "max-peers",
                    inline,
                    next,
                    "非负整数，如 32",
                ));
                take_next(&mut adv);
            }
            "public-endpoint" => {
                f.public_endpoint =
                    Some(cli_flags::take_value_or_exit("public-endpoint", inline, next, false));
                take_next(&mut adv);
            }
            "dns-port" => {
                f.dns_port = Some(cli_flags::take_num_or_exit::<u16>(
                    "dns-port",
                    inline,
                    next,
                    "端口数字，0 = 关闭代答",
                ));
                take_next(&mut adv);
            }
            "files-root" => {
                f.files_root = Some(cli_flags::take_value_or_exit("files-root", inline, next, false));
                take_next(&mut adv);
            }
            "relay" => {
                // F0 carve-out：空值 = 显式关掉注册腿（config 开了想用 CLI 关的形态；
                // Go flag 空串同义，实测 `bin/homeway-go serve --relay=` 正常起服）。
                f.relay = Some(cli_flags::take_value_empty_ok_or_exit("relay", inline, next));
                take_next(&mut adv);
            }
            "ddns" => {
                // F0 carve-out（代码门 M1 扩面）：空值 = 清空 config 的全部条目
                // （Go `cli.go:127-135` 同义；`bin/homeway-go serve --ddns=` 实测 STARTED）
                // ——此前该分支（下方 `if v.is_empty()`）不可达，现与注释一致。
                let v = cli_flags::take_value_empty_ok_or_exit("ddns", inline, next);
                take_next(&mut adv);
                if v.contains(':') || v.contains('/') || v.contains(' ') {
                    // Go cli.go:55 同校验同串
                    return Err(CliErr::Usage(format!(
                        "--ddns 只要裸域名（不带端口/路径）：{v:?}"
                    )));
                }
                f.ddns = Some(v);
            }
            "verbose" => f.verbose = cli_flags::take_bool_or_exit("verbose", inline, true),
            "help" | "h" => {
                serve_usage();
                std::process::exit(0);
            }
            other => {
                return Err(CliErr::Usage(format!("未知参数：--{other}")));
            }
        }
        i += adv;
    }
    Ok(f)
}

/// 前台默认 state（Q-H F15/N1·L7）：与统一进程同一默认（`~/.config/homeway`，
/// Go 前台单角色 `internal/server/cli.go:30` 同义）。
fn default_state_or(f: &ServeFlags) -> PathBuf {
    f.state.clone().unwrap_or_else(crate::unified_cli::default_state_dir)
}

/// 纯映射：`FileConfig` → `ServeConfig`（无值域校验——已在校验层；只做字段搬运与
/// `parse_bind_iface` 这类纯变换）。控制面装配路径直接用它（零 exit）。
pub(crate) fn serve_config_of(fc: &FileConfig, state_dir: &Path) -> Result<ServeConfig, String> {
    let mut cfg = ServeConfig {
        state_dir: state_dir.to_owned(),
        ..Default::default()
    };
    if let Some(v) = fc.serve.listen {
        cfg.listen_port = v;
    }
    if let Some(v) = fc.serve.quic_listen {
        cfg.quic_listen_port = Some(v);
    }
    if let Some(v) = fc.serve.quic {
        cfg.quic = v;
    }
    if let Some(v) = &fc.serve.bind_interface {
        cfg.bind_iface = parse_bind_iface(v);
    }
    if let Some(v) = fc.serve.upnp {
        cfg.upnp = v;
    }
    if let Some(v) = &fc.serve.stun {
        cfg.stun = v.clone();
    }
    if let Some(v) = &fc.serve.stun6 {
        cfg.stun6 = v.clone(); // 空串 = 显式关 v6 校验（config 显式写 "" 才覆盖默认）
    }
    if let Some(v) = fc.serve.max_peers {
        cfg.max_devices = v;
    }
    if let Some(v) = &fc.serve.peer_ttl {
        cfg.peer_ttl = parse_go_duration(v)
            .ok_or_else(|| format!("serve.peer_ttl {v:?} 非法（时长串，如 \"168h\"）"))?;
    }
    if let Some(v) = &fc.serve.public_endpoint {
        cfg.public_endpoint = v.clone();
    }
    if let Some(v) = fc.serve.dns_port {
        cfg.dns_port = v;
    }
    if let Some(v) = &fc.serve.files_root {
        cfg.files_root = Some(PathBuf::from(v));
    }
    // [serve.tx_shape]（D-3）：解析与 env 覆盖在 tx_shape_resolve（engine 装配点）。
    cfg.tx_shape_cfg = fc.serve.tx_shape;
    // [serve.quic_admit]（M2 §3.2）：值域已在校验层收口；这里只做「时长串 → Duration」搬运
    // （env `HOMEWAY_QUIC_ADMIT_RETRY` 的叠加在 engine 装配点）。
    if let Some(q) = &fc.serve.quic_admit {
        cfg.quic_admit = Some(q.resolve()?);
    }
    // serve.relay：注册腿端点（rl1 token / 裸 host:port——R4-4c 接线）
    if let Some(v) = &fc.serve.relay {
        if !v.is_empty() {
            cfg.relay = Some(v.clone());
        }
    }
    if let Some(list) = &fc.serve.ddns {
        cfg.ddns = list.iter().map(|d| d.domain.trim().to_owned()).collect();
    }
    // ---- Q-J F2 五键（边界已解析成型：这里只做类型搬运，非法值在校验层已拒）----
    // 空列表 = 取默认（等价不配置）——cfg 已由 ServeConfig::default() 填好默认值。
    if let Some(list) = &fc.serve.dns_upstream {
        if !list.is_empty() {
            let mut v = Vec::with_capacity(list.len());
            for item in list {
                v.push(
                    parse_addr_with_default_port(item, 53)
                        .ok_or_else(|| format!("serve.dns_upstream {item:?} 非法（ip 或 ip:port）"))?,
                );
            }
            cfg.dns_upstream = v;
        }
    }
    if let Some(v) = &fc.serve.dns_fallback {
        if parse_addr_with_default_port(v, 53).is_none() {
            return Err(format!("serve.dns_fallback {v:?} 非法（ip 或 ip:port）"));
        }
        cfg.dns_fallback = v.trim().to_owned();
    }
    if let Some(list) = &fc.serve.ddns_resolver {
        if !list.is_empty() {
            let mut v = Vec::with_capacity(list.len());
            for item in list {
                v.push(parse_v4_addr(item, 53).ok_or_else(|| format!("serve.ddns_resolver {item:?} 非法（IPv4 ip 或 ip:port）"))?);
            }
            cfg.ddns_resolver = v;
        }
    }
    if let Some(list) = &fc.serve.dns_probe_target {
        if !list.is_empty() {
            let mut v = Vec::with_capacity(list.len());
            for item in list {
                v.push(
                    parse_v4_addr(item, 53)
                        .ok_or_else(|| format!("serve.dns_probe_target {item:?} 非法（IPv4 ip 或 ip:port）"))?,
                );
            }
            cfg.dns_probe_target = v;
        }
    }
    if let Some(list) = &fc.serve.stun_probe_target {
        if !list.is_empty() {
            let mut v = Vec::with_capacity(list.len());
            for item in list {
                let Ok(a) = item.trim().parse::<std::net::SocketAddrV4>() else {
                    return Err(format!("serve.stun_probe_target {item:?} 非法（IPv4 ip:port，须显式带端口）"));
                };
                v.push(a);
            }
            cfg.stun_probe_target = v;
        }
    }
    Ok(cfg)
}

/// 组装 ServeConfig（flag > config.toml > 默认）的**非 exit** 形态（Q-H F1）：
/// flag 解析失败 = `CliErr::Usage`；config 层失败 = `CliErr::Config`。
pub(crate) fn assemble_result(args: &[String]) -> Result<ServeConfig, CliErr> {
    let f = parse_serve_flags(args)?;
    if !f.extra.is_empty() {
        return Err(CliErr::Usage(format!(
            "serve 不接受位置参数（得 {:?}）",
            f.extra
        )));
    }
    let state_dir = default_state_or(&f);
    let fc = load_config_strict(&state_dir).map_err(CliErr::Config)?;
    let mut cfg = serve_config_of(&fc, &state_dir).map_err(CliErr::Config)?;
    cfg.verbose = f.verbose;
    // flag 覆盖
    if let Some(v) = f.listen {
        cfg.listen_port = v;
    }
    if let Some(v) = &f.bind_interface {
        cfg.bind_iface = parse_bind_iface(v);
    }
    if let Some(v) = f.upnp {
        cfg.upnp = v;
    }
    if let Some(v) = f.quic {
        cfg.quic = v;
    }
    if let Some(v) = f.stun {
        cfg.stun = v;
    }
    if let Some(v) = f.stun6 {
        cfg.stun6 = v; // --stun6 '' = 关 v6 校验（Go flag 空串同义）
    }
    if let Some(v) = f.peer_ttl {
        cfg.peer_ttl = v;
    }
    if let Some(v) = f.max_peers {
        cfg.max_devices = v;
    }
    if let Some(v) = &f.public_endpoint {
        // Q5/S7b：**flag 显式给出 ⇒ 当场校验**（fail fast；Go `cli.go:98-105`
        // `netip.ParseAddrPort` 同义——用户明确写了端点，静默清空/忽略比报错更糟）。
        // config 面校验在 `load_config_strict`（serve_cli.rs:295-305，保持）。
        validate_public_endpoint_flag(v).map_err(CliErr::Usage)?;
        cfg.public_endpoint = v.clone();
    }
    if let Some(v) = f.dns_port {
        cfg.dns_port = v;
    }
    if let Some(v) = &f.files_root {
        cfg.files_root = Some(PathBuf::from(v));
    }
    if let Some(v) = &f.relay {
        if v.is_empty() {
            // 显式空值 = 显式关掉注册腿（--relay=）
            cfg.relay = None;
        } else {
            cfg.relay = Some(v.clone());
        }
    }
    // --ddns 单值 flag 显式给出 = 覆盖 config 的**全部**条目（一次性覆盖语义；
    // --ddns= 空值 = 清空）。
    if let Some(v) = &f.ddns {
        cfg.ddns = if v.is_empty() {
            Vec::new()
        } else {
            vec![v.clone()]
        };
    }
    Ok(cfg)
}

/// 组装 ServeConfig（**前台薄壳**：exit(2)/exit(1) 只在这里）。
pub fn assemble(args: &[String]) -> ServeConfig {
    match assemble_result(args) {
        Ok(cfg) => cfg,
        Err(CliErr::Usage(m)) => {
            eprintln!("{m}");
            std::process::exit(2);
        }
        Err(CliErr::Config(m)) => {
            eprintln!("{m}");
            std::process::exit(1);
        }
    }
}

/// `--bind-interface` 值 → 绑定模式（Go `ResolveBind` 同口径，`cli.go:190-207`）：
/// **先 `TrimSpace` 再判**——空值/auto ⇒ `Auto`；none/off/no（大小写不敏感）⇒ `Off`；
/// IP 字面量 ⇒ 单栈绑地址；其余按**网卡名**（保原大小写，只去首尾空白——Go
/// `net.InterfaceByName(v)` 吃的是 trim 后的原串）⇒ `Explicit`（存在性在引擎运行期查，
/// 找不到告警退回 auto，`engine.rs:309-338`）。
fn parse_bind_iface(v: &str) -> BindMode {
    let t = v.trim();
    match t.to_ascii_lowercase().as_str() {
        // Go `ResolveBind("")` ⇒ BindAuto（Q-L L3：空值 carve-out 的落点）
        "" | "auto" => BindMode::Auto,
        "none" | "off" | "no" => BindMode::Off,
        _ => match t.parse::<std::net::IpAddr>() {
            Ok(ip) => BindMode::Addr(ip),
            Err(_) => BindMode::Explicit(t.to_owned()),
        },
    }
}

/// `--public-endpoint` 显式 flag 的值域校验（Q5/S7b；对齐 Go `cli.go:98-105` 的
/// 「显式给的值当场校验（fail fast）」）。每段 = `ip:port` 字面（`SocketAddr` 解析，
/// v4/v6 皆可）+ 端口 1–65535；非法 ⇒ 可行动文案（调用方 `CliErr::Usage` ⇒ exit 2）。
///
/// **空值 carve-out**：`--public-endpoint=''` = 显式关掉公网端点公布（Go flag 空串同义，
/// config 面同样只对非空值校验）——空串不是「非法」。
/// **比 Go 严一点**（登记）：Go `netip.ParseAddrPort` 收端口 0，本函数拒（端口 0 不是
/// 可公布的端点）；其余口径一致。
fn validate_public_endpoint_flag(v: &str) -> Result<(), String> {
    if v.trim().is_empty() {
        return Ok(());
    }
    for line in v.split(',') {
        match line.trim().parse::<std::net::SocketAddr>() {
            Ok(a) if a.port() != 0 => {}
            Ok(_) => {
                return Err(format!(
                    "--public-endpoint {line:?} 非法（端口 0 不是可公布的端点；须为逗号分隔的 ip:port，端口 1–65535）"
                ))
            }
            Err(e) => {
                return Err(format!(
                    "--public-endpoint {line:?} 非法（{e}；须为逗号分隔的 ip:port）"
                ))
            }
        }
    }
    Ok(())
}

/// `homeway-cli serve [...]`：前台出口（Ctrl-C / SIGTERM 有序收工）。
pub fn cmd_serve(args: &[String]) {
    let cfg = assemble(args);
    // 单实例锁（Go 全形态共用 <state>/lock；form=serve——防 launchd 双起/统一进程
    // 与前台单角色同 state 互抢 UDP）
    let _lock = crate::relay_cli::acquire_lock_or_exit(&cfg.state_dir, "serve");
    // 文件日志先立起来（Go cli.go initLogs(cacheDir) 同义）：events.log（摘要，2MB×3）
    // + debug.log（细节，8MB×2）落 `<state>/cache/`；打开失败只告警降级，不挡启动。
    // 前台形态摘要行保持 stdout 可见（终端 + 文件双写——与统一进程同口径）。
    let verbose = cfg.verbose;
    let cache_dir = cfg.state_dir.join("cache");
    let events = match homeway_core::nodestate::EventsLog::open(&cache_dir) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            eprintln!("homeway: ⚠️ events.log 打开失败（{e}）——本轮公告只回显终端");
            Arc::new(homeway_core::nodestate::EventsLog::terminal_only(cache_dir.join("events.log")))
        }
    };
    let debug = match homeway_core::nodestate::DebugLog::open(&cache_dir) {
        Ok(w) => Arc::new(w),
        Err(e) => {
            eprintln!("homeway: ⚠️ debug.log 打开失败（{e}）——本轮细节日志缺失，服务继续");
            Arc::new(homeway_core::nodestate::DebugLog::disabled(cache_dir.join("debug.log")))
        }
    };
    let logf: Arc<dyn Fn(&str) + Send + Sync> = {
        let ev = Arc::clone(&events);
        Arc::new(move |s: &str| ev.eventf(s))
    };
    // token 端点变化轮流（Go tokenToFile：events 文件只写 + verbose 回显）
    let tokf: Arc<dyn Fn(&str) + Send + Sync> = {
        let ev = Arc::clone(&events);
        Arc::new(move |s: &str| ev.quietf(s, verbose))
    };
    let log_paths = Some((
        events.path().display().to_string(),
        debug.path().display().to_string(),
    ));
    let dlogf: Arc<dyn Fn(&str) + Send + Sync> = {
        let db = Arc::new(debug);
        Arc::new(move |s: &str| db.dlogf(s, verbose))
    };
    let upnp_used = cfg.upnp;
    let engine = match ServeEngine::start(
        cfg,
        Arc::clone(&logf),
        Arc::clone(&dlogf),
        Arc::clone(&tokf),
        log_paths,
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("serve 启动失败：{e}");
            std::process::exit(1);
        }
    };
    // SIGTERM/SIGINT → 有序收工（D5 + UPnP 退出缩租）
    install_stop_signals();
    println!("（serve 前台运行中——Ctrl-C 收工）");
    match wait_stop_pipe() {
        StopWait::Signaled => {}
        StopWait::PipeErr(e) => eprintln!("homeway: {e}——按收到停止处理（收工）"),
    }
    if upnp_used {
        homeway_core::server::engine::shrink_upnp_lease(&engine, &logf);
    }
    engine.shutdown(Duration::from_secs(2));
}

/// 信号 → 自写管道一字节（async-signal-safe 面）。
static STOP_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_stop_signal(_sig: i32) {
    let fd = STOP_FD.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        unsafe {
            let b = b"x";
            libc::write(fd, b.as_ptr().cast(), 1);
        }
    }
}

static STOP_PIPE: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();

pub fn install_stop_signals() {
    // Q-G F1：管道经 `sysfd` 建（两端 CLOEXEC）→ 立即 `into_raw_fd()` 交既有 i32
    // 字段（生命周期仍由 `wait_stop_pipe`/进程存活期收口——本批不改结构，单次
    // 生命周期、无 restart 面）。建立失败 = 可行动错误退出（旧形态忽略 rc 会让
    // handler 写 fd 0）。
    let (r, w) = *STOP_PIPE.get_or_init(|| match homeway_core::sysfd::pipe_cloexec() {
        Ok((r, w)) => (
            std::os::fd::IntoRawFd::into_raw_fd(r),
            std::os::fd::IntoRawFd::into_raw_fd(w),
        ),
        Err(e) => {
            eprintln!("homeway: serve 停止管道建立失败（{e}）——退出");
            std::process::exit(1);
        }
    });
    STOP_FD.store(w, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        let h = on_stop_signal as extern "C" fn(i32) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
    let _ = r;
}

/// 注入缝形（Q-H 代码门 H1：不碰 errno 的可测面；`__error` 在 linux 上不存在）。
type ReadFn<'a> = dyn FnMut(i32, &mut [u8]) -> Result<usize, std::io::Error> + 'a;

/// 停止等待结果（Q-H F16：返回值自此有语义——不再是无意义 `bool`）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StopWait {
    /// 收到停止字节 / 管道 EOF / 信号路径。
    Signaled,
    /// 管道不可用（未安装/IO 失败）——**不读 fd 0**；调用方按「收到停止」收工。
    PipeErr(String),
}

/// 纯逻辑（可单测）：fd < 0/未安装 ⇒ Err；read 循环：Ok(n>0) ⇒ Signaled；Ok(0)(EOF)
/// ⇒ Signaled；`Interrupted`（EINTR）⇒ 重试；其它 Err ⇒ Err（可读文案）。
///
/// 注入缝形 = `Result<usize, std::io::Error>`（**不碰 errno**——`libc::__error` 只在
/// apple/bsd 存在，linux 面是 `__errno_location`；用 errno 注入会让 ubuntu CI 的
/// `--all-targets` 编译失败，代码门 H1）。
fn read_stop_signal_with(fd: i32, read_fn: &mut ReadFn<'_>) -> Result<StopWait, String> {
    if fd < 0 {
        return Err("停止管道未安装（fd 无效）——本函数不会去读 fd 0（stdin）".to_owned());
    }
    let mut b = [0u8; 1];
    loop {
        match read_fn(fd, &mut b) {
            Ok(0) => return Ok(StopWait::Signaled), // EOF（写端已关）= 收工
            Ok(_) => return Ok(StopWait::Signaled),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue, // EINTR：重试
            Err(e) => return Err(format!("读停止管道失败：{e}")),
        }
    }
}

pub(crate) fn read_stop_signal(fd: i32) -> Result<StopWait, String> {
    read_stop_signal_with(fd, &mut |fd, buf| {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n >= 0 {
            Ok(n as usize)
        } else {
            Err(std::io::Error::last_os_error())
        }
    })
}

/// 等待停止（壳；**不 exit**——可进程内断言）：未安装 ⇒ 不读 fd 0，返回 `PipeErr`。
pub fn wait_stop_pipe() -> StopWait {
    let fd = match STOP_PIPE.get() {
        Some((r, _)) => *r,
        None => -1,
    };
    match read_stop_signal(fd) {
        Ok(v) => v,
        Err(e) => StopWait::PipeErr(e),
    }
}

// ---------- serve token [list|revoke]（纯读/纯文件操作） ----------

fn token_usage() {
    eprintln!("用法：homeway-cli serve token [list | revoke <id>] [--state DIR] [--reason S]");
    eprintln!("  无动词 = reveal（完整凭证只经本命令族；来源 = 台账末行）");
}

/// token 族的状态目录（`--state` 全形态；缺省 = 统一进程默认 state——Q-H F15）。
fn token_state_dir(args: &[String]) -> PathBuf {
    let mut i = 0;
    while i < args.len() {
        if let Some((name, inline)) = cli_flags::split_flag(&args[i]) {
            if name == "state" {
                return cli_flags::take_state_or_exit(
                    "state",
                    inline,
                    args.get(i + 1).map(String::as_str),
                );
            }
        }
        i += 1;
    }
    crate::unified_cli::default_state_dir()
}

pub fn cmd_serve_token(args: &[String]) {
    // Q-H F8/CA13：`--help` 短路（此前会落进 rest 后照跑）。
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        token_usage();
        std::process::exit(0);
    }
    // flag 之后的第一个非 flag 位置参数 = 动词（list / revoke <id>；无动词 = reveal）
    let mut verb: Option<String> = None;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--state" || a == "--reason" {
            rest.push(a.clone());
            if let Some(v) = args.get(i + 1) {
                rest.push(v.clone());
            }
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            rest.push(a.clone());
            i += 1;
            continue;
        }
        if verb.is_none() {
            verb = Some(a.clone());
        } else {
            rest.push(a.clone());
        }
        i += 1;
    }
    match verb.as_deref() {
        None => token_reveal(&rest),
        Some("list") => token_list(&rest),
        Some("revoke") => token_revoke(&rest),
        Some(other) => {
            eprintln!("serve token 不认识的动词 {other:?}（可用：list / revoke <id>）");
            std::process::exit(2);
        }
    }
}

/// reveal：完整凭证只经本命令族（来源注记 = 台账末行——写入纪律下末行 = 最近在用）。
fn token_reveal(args: &[String]) {
    let state = token_state_dir(args);
    let st = match homeway_core::server::state::State::open(&state.join("serve")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("state 打开失败：{e}");
            std::process::exit(1);
        }
    };
    let Some(tok) = st.last_token().ok().flatten() else {
        eprintln!("台账为空（出口从未铸出 token）：先 `homeway-cli serve`，等首轮端点探测后重试");
        std::process::exit(1);
    };
    let eps: Vec<homeway_core::token::EndpointRef> = tok
        .endpoints
        .iter()
        .map(|e| homeway_core::token::EndpointRef::new(e.addr.as_str(), e.kind))
        .collect();
    let s = match homeway_core::token::encode(&homeway_core::token::TokenSpec {
        peer_id: &tok.peer_id,
        secret: &tok.secret,
        endpoints: &eps,
        rpk: tok.rpk.as_ref(),
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("token 编码失败：{e}");
            std::process::exit(1);
        }
    };
    println!("serve token：{s}");
    println!("来源：台账末行（进程未跑/角色未装配——写入纪律下末行 = 最近在用 token）");
    if !tok.endpoints.is_empty() {
        let addrs: Vec<&str> = tok.endpoints.iter().map(|e| e.addr.as_str()).collect();
        println!("端点：{}", addrs.join("、"));
    }
}

/// 台账只读列表（id / 签发 / 状态 / 端点 / 掩码）。
fn token_list(args: &[String]) {
    let state = token_state_dir(args);
    let st = match homeway_core::server::state::State::open(&state.join("serve")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("state 打开失败：{e}");
            std::process::exit(1);
        }
    };
    let ledger = match st.ledger() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("台账读取失败：{e}");
            std::process::exit(1);
        }
    };
    if ledger.is_empty() {
        println!("台账为空（出口从未铸出 token）：先 `homeway-cli serve`，等首轮端点探测后重试");
        return;
    }
    println!("{:<8}  {:<20}  {:<14}  端点/凭证（掩码）", "id", "签发", "状态");
    let mut creds = std::collections::HashSet::new();
    for (i, e) in ledger.iter().enumerate() {
        creds.insert(e.id.clone());
        let mut st_str = if i == ledger.len() - 1 { "有效·末行(在用)" } else { "有效" }.to_owned();
        if e.revoked {
            st_str = if e.reason.is_empty() { "已吊销".to_owned() } else { format!("已吊销({})", e.reason) };
        }
        let eps = if e.endpoints.is_empty() {
            "(无端点)".to_owned()
        } else {
            e.endpoints.iter().map(|x| x.addr.clone()).collect::<Vec<_>>().join(",")
        };
        println!("{:<8}  {:<20}  {:<10}  {}  {}", e.id, e.issued, st_str, mask_secret(&e.secret), eps);
    }
    if let Ok(revs) = st.revocations() {
        if !revs.is_empty() {
            println!("\n吊销表（revoked.jsonl，{} 条）：", revs.len());
            for (id, secret, at, reason) in revs {
                println!("  {id:<8}  {at:<20}  {}  {reason}", mask_secret(&secret));
            }
        }
    }
    println!("\n共 {} 行 / {} 枚凭证（id 相同 = 同一凭证的多轮铸出）", ledger.len(), creds.len());
    println!("吊销：homeway-cli serve token revoke <id>（即时对新注册生效；已登记设备随出口重启清空）");
}

fn mask_secret(s: &str) -> String {
    if s.len() <= 8 {
        return "***".to_owned();
    }
    format!("{}…{}", &s[..6], &s[s.len() - 4..])
}

/// 吊销一枚凭证 id（写吊销表；对在跑出口经跟随读秒级对新注册生效）。
fn token_revoke(args: &[String]) {
    let mut state: Option<PathBuf> = None;
    let mut reason = "manual".to_owned();
    let mut id = String::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(a) else {
            id = a.to_owned();
            i += 1;
            continue;
        };
        match name {
            "state" => {
                state = Some(cli_flags::take_state_or_exit(
                    "state",
                    inline,
                    args.get(i + 1).map(String::as_str),
                ));
                if inline.is_none() {
                    i += 1;
                }
            }
            "reason" => {
                reason = cli_flags::take_value_or_exit(
                    "reason",
                    inline,
                    args.get(i + 1).map(String::as_str),
                    false,
                );
                if inline.is_none() {
                    i += 1;
                }
            }
            _ => {}
        }
        i += 1;
    }
    let state = state.unwrap_or_else(crate::unified_cli::default_state_dir);
    if id.is_empty() {
        eprintln!("serve token revoke 需要 <id>（先 `homeway-cli serve token list` 查）");
        std::process::exit(2);
    }
    let st = match homeway_core::server::state::State::open(&state.join("serve")) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("state 打开失败：{e}");
            std::process::exit(1);
        }
    };
    let ledger = match st.ledger() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("台账读取失败：{e}");
            std::process::exit(1);
        }
    };
    let Some(entry) = ledger.iter().find(|e| e.id == id) else {
        eprintln!("台账里没有 id={id}（先 `homeway-cli serve token list` 查）");
        std::process::exit(1);
    };
    let secret_b64 = entry.secret.clone();
    use base64::Engine as _;
    let raw = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(secret_b64.as_bytes())
        .ok()
        .and_then(|v| <[u8; 32]>::try_from(v).ok());
    let Some(secret) = raw else {
        eprintln!("台账行 secret 非法（{id}）");
        std::process::exit(1);
    };
    match st.revoke(&secret, &reason) {
        Ok(already) => {
            if already {
                println!("id={id} 已在吊销表（幂等——不重复追加）");
            } else {
                println!("已吊销 id={id}（写 revoked.jsonl；在跑出口 ≤1s 跟随生效——对新注册即时拒）");
            }
            println!("彻底清场（拆已在线设备）：重启出口（设备表随角色重建清空）；在用凭证被吊销时重启会自动铸新");
        }
        Err(e) => {
            eprintln!("吊销失败：{e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_cfg(dir: &Path, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("config.toml"), body).unwrap();
    }

    fn tmp_state(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "hw-servecli-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// F1：缺失 config = 默认（serve.enabled 缺省 true）。
    #[test]
    fn strict_load_missing_is_default() {
        let d = tmp_state("missing");
        let fc = load_config_strict(&d).unwrap();
        assert!(fc.serve.enabled);
        assert!(fc.serve.listen.is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F1 值域表逐行坏值（每行 1 例）+ 好值往返。
    #[test]
    fn strict_load_value_domain_per_row() {
        let d = tmp_state("domain");
        let cases: &[(&str, &str)] = &[
            ("[serve]\nlisten = 0\n", "serve.listen"),
            ("[serve]\nquic_listen = 0\n", "serve.quic_listen"),
            ("[serve]\nbind_interface = \"en0:1\"\n", "serve.bind_interface"),
            ("[serve]\npublic_endpoint = \"1.2.3.4\"\n", "serve.public_endpoint"),
            ("[serve]\npeer_ttl = \"abc\"\n", "serve.peer_ttl"),
            ("[serve]\nddns = [{ domain = \"\" }]\n", "serve.ddns.domain"),
            ("[serve]\nddns = [{ domain = \"a/b\" }]\n", "serve.ddns.domain"),
            ("[serve]\nrelay = \"垃圾串\"\n", "serve.relay"),
            ("[serve]\ndns_fallback = \"\"\n", "serve.dns_fallback"),
            ("[serve]\ndns_fallback = \"不是地址\"\n", "serve.dns_fallback"),
            ("[serve]\ndns_upstream = [\"垃圾\"]\n", "serve.dns_upstream"),
            ("[serve]\nddns_resolver = [\"::1\"]\n", "serve.ddns_resolver"),
            ("[serve]\ndns_probe_target = [\"1.2.3.4:99999\"]\n", "serve.dns_probe_target"),
            ("[serve]\nstun_probe_target = [\"1.2.3.4\"]\n", "serve.stun_probe_target"),
            ("[serve]\ndns_fallback = \"1.2.3.4:0\"\n", "serve.dns_fallback"),
            ("[serve]\ndns_upstream = [\"1.2.3.4:0\"]\n", "serve.dns_upstream"),
            ("[serve]\nddns_resolver = [\"1.2.3.4:0\"]\n", "serve.ddns_resolver"),
            ("[serve]\ndns_probe_target = [\"223.5.5.5:0\"]\n", "serve.dns_probe_target"),
            ("[serve]\nstun_probe_target = [\"1.2.3.4:0\"]\n", "serve.stun_probe_target"),
            ("[relay]\nlisten = \"41741\"\n", "relay.listen"),
            ("[relay]\nlisten = \":0\"\n", "relay.listen"),
            ("[serve]\nlisten = 99999\n", "config.toml"),
            ("[serve]\n不认识的键 = 1\n", "config.toml"),
            ("[serve]\ntx_shape = { rate_mbps = \"200\" }\n", "config.toml"),
            // M2 S3-4：`[serve.quic_admit]` 七键——语法/值域非法一律**拒启**（§3.2 表）
            (
                "[serve.quic_admit]\nretry_token_lifetime = \"5\"\n",
                "serve.quic_admit.retry_token_lifetime",
            ),
            (
                "[serve.quic_admit]\nretry_token_lifetime = \"0s\"\n",
                "serve.quic_admit.retry_token_lifetime",
            ),
            (
                "[serve.quic_admit]\nretry_token_lifetime = \"61s\"\n",
                "serve.quic_admit.retry_token_lifetime",
            ),
            ("[serve.quic_admit]\nper_src_fails = 0\n", "serve.quic_admit.per_src_fails"),
            (
                "[serve.quic_admit]\nper_src_fails = 1001\n",
                "serve.quic_admit.per_src_fails",
            ),
            (
                "[serve.quic_admit]\nper_src_window = \"2h\"\n",
                "serve.quic_admit.per_src_window",
            ),
            ("[serve.quic_admit]\nnonce_ttl = \"31s\"\n", "serve.quic_admit.nonce_ttl"),
            (
                "[serve.quic_admit]\nadmit_deadline = \"0s\"\n",
                "serve.quic_admit.admit_deadline",
            ),
            (
                "[serve.quic_admit]\nadmit_deadline = \"61s\"\n",
                "serve.quic_admit.admit_deadline",
            ),
            (
                "[serve.quic_admit]\nproof_fail_threshold = 1001\n",
                "serve.quic_admit.proof_fail_threshold",
            ),
            (
                "[serve.quic_admit]\nretry_policy = \"恒开\"\n",
                "serve.quic_admit.retry_policy",
            ),
            ("[serve.quic_admit]\n不认识的键 = 1\n", "config.toml"),
        ];
        for (body, want_field) in cases {
            write_cfg(&d, body);
            let e = load_config_strict(&d).unwrap_err();
            assert!(
                e.contains(want_field),
                "body={body:?} 应报 {want_field}，实得：{e}"
            );
            assert!(e.contains("config.toml"), "错误必须带路径，实得：{e}");
        }
        // 好值往返：serde 序列化后再严格读仍 Ok（写回面共用本函数）。
        let good = "[serve]\nenabled = true\nlisten = 41641\nquic_listen = 41642\nquic = false\nbind_interface = \"auto\"\n\
                    peer_ttl = \"168h\"\npublic_endpoint = \"1.2.3.4:41641\"\nrelay = \"\"\n\
                    dns_port = 5300\n[relay]\nenabled = false\nlisten = \":41741\"\nadvertise = \"\"\n";
        write_cfg(&d, good);
        let fc = load_config_strict(&d).unwrap();
        let body = toml::to_string_pretty(&fc).unwrap();
        write_cfg(&d, &body);
        let fc2 = load_config_strict(&d).unwrap();
        assert_eq!(fc2.serve.listen, Some(41641));
        assert_eq!(fc2.serve.quic_listen, Some(41642), "M1：QUIC 端口键往返");
        assert_eq!(fc2.serve.quic, Some(false), "M1 S3-4：QUIC 面开关键往返");
        assert_eq!(fc2.relay.listen.as_deref(), Some(":41741"));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F2 缺省回归：不配置任何新键 ⇒ 五值与修前常量**逐值相等**（缺省行为兼容既有部署）。
    #[test]
    fn f2_keys_defaults_unchanged() {
        use homeway_core::server::{ddnscheck, egress};
        let d = tmp_state("f2default");
        let state = d.display().to_string();
        let cfg = assemble_result(&["--state".to_owned(), state]).unwrap();
        assert!(cfg.dns_upstream.is_empty(), "缺省 = 跟随 /etc/resolv.conf");
        assert_eq!(cfg.dns_fallback, "223.5.5.5");
        assert_eq!(cfg.dns_probe_target, egress::default_probe_targets());
        assert_eq!(cfg.stun_probe_target, egress::default_stun_targets());
        assert_eq!(cfg.ddns_resolver, ddnscheck::default_resolvers_v4());
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F2：五键配置生效（边界即解析成型——`ip` 补默认端口，`ip:port` 原样）。
    #[test]
    fn f2_keys_take_effect() {
        let d = tmp_state("f2eff");
        write_cfg(
            &d,
            "[serve]\ndns_upstream = [\"1.1.1.1\", \"9.9.9.9:5353\"]\ndns_fallback = \"8.8.4.4\"\n\
             ddns_resolver = [\"223.5.5.5\", \"119.29.29.29:5353\"]\n\
             dns_probe_target = [\"1.1.1.1\", \"9.9.9.9:53\"]\n\
             stun_probe_target = [\"162.159.207.1:3478\"]\n",
        );
        let state = d.display().to_string();
        let cfg = assemble_result(&["--state".to_owned(), state]).unwrap();
        assert_eq!(cfg.dns_upstream.len(), 2);
        assert_eq!(cfg.dns_upstream[0].to_string(), "1.1.1.1:53", "裸 ip 补 :53");
        assert_eq!(cfg.dns_upstream[1].to_string(), "9.9.9.9:5353");
        assert_eq!(cfg.dns_fallback, "8.8.4.4");
        assert_eq!(cfg.ddns_resolver[1].to_string(), "119.29.29.29:5353");
        assert_eq!(cfg.dns_probe_target[0].to_string(), "1.1.1.1:53");
        assert_eq!(cfg.stun_probe_target, vec!["162.159.207.1:3478".parse().unwrap()]);
        // 空列表 = 取默认（等价不配置）
        use homeway_core::server::egress;
        write_cfg(&d, "[serve]\ndns_probe_target = []\nstun_probe_target = []\nddns_resolver = []\ndns_upstream = []\n");
        let cfg = assemble_result(&["--state".to_owned(), d.display().to_string()]).unwrap();
        assert_eq!(cfg.dns_probe_target, egress::default_probe_targets());
        assert_eq!(cfg.stun_probe_target, egress::default_stun_targets());
        assert!(cfg.dns_upstream.is_empty());
        let _ = std::fs::remove_dir_all(&d);
    }


    /// F1：assemble_result 不 exit（Err 分类：Usage vs Config）。
    #[test]
    fn assemble_result_classifies_without_exit() {
        let d = tmp_state("classify");
        write_cfg(&d, "[serve]\npeer_ttl = \"abc\"\n");
        let state = d.display().to_string();
        match assemble_result(&["--state".to_owned(), state.clone()]) {
            Err(CliErr::Config(_)) => {}
            other => panic!("坏 config 应 CliErr::Config，实得 {:?}", other.err().map(|e| format!("{e:?}"))),
        }
        match assemble_result(&["--state".to_owned(), state, "--nope".to_owned()]) {
            Err(CliErr::Usage(_)) => {}
            other => panic!("未知 flag 应 CliErr::Usage，实得 {:?}", other.err().map(|e| format!("{e:?}"))),
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F2：`--state` 全形态（等号形正例 + 缺值/空值 fail-fast 的**非 exit** 面——
    /// fail-fast 路径由 exit 承担，这里只钉取值器分类）。
    #[test]
    fn state_flag_forms() {
        let f = parse_serve_flags(&["--state=/tmp/a".to_owned()]).unwrap();
        assert_eq!(f.state, Some(PathBuf::from("/tmp/a")));
        let f = parse_serve_flags(&["--state".to_owned(), "/tmp/b".to_owned()]).unwrap();
        assert_eq!(f.state, Some(PathBuf::from("/tmp/b")));
    }

    /// F7b：布尔显式值（`--upnp=false` / `--verbose=false` 真生效）。
    #[test]
    fn bool_flags_explicit_values() {
        let f = parse_serve_flags(&["--upnp=false".to_owned()]).unwrap();
        assert_eq!(f.upnp, Some(false));
        let f = parse_serve_flags(&["--verbose=false".to_owned()]).unwrap();
        assert!(!f.verbose);
        let f = parse_serve_flags(&["--upnp".to_owned()]).unwrap();
        assert_eq!(f.upnp, Some(true));
    }

    /// **M1 S3-4：`serve.quic` 双面**（config 键 + `--quic[=bool]` flag）——缺省 true；
    /// `false` 经两条面各自落到 `ServeConfig.quic=false`（flag 优先于 config）。
    #[test]
    fn quic_switch_config_key_and_flag() {
        // 缺省（不写键）= true
        let d = tmp_state("quicdef");
        let state = d.display().to_string();
        let cfg = assemble_result(&["--state".to_owned(), state.clone()]).unwrap();
        assert!(cfg.quic, "缺省 = true（QUIC 面常开）");
        // config 键
        write_cfg(&d, "[serve]\nquic = false\n");
        let cfg = assemble_result(&["--state".to_owned(), state.clone()]).unwrap();
        assert!(!cfg.quic, "config serve.quic=false 生效");
        // flag 覆盖 config（body 里是 true，flag 给 false）
        write_cfg(&d, "[serve]\nquic = true\n");
        let cfg = assemble_result(&["--state".to_owned(), state.clone(), "--quic=false".to_owned()])
            .unwrap();
        assert!(!cfg.quic, "flag 优先于 config");
        let cfg = assemble_result(&["--state".to_owned(), state, "--quic".to_owned()]).unwrap();
        assert!(cfg.quic, "`--quic` 无值形 = true");
        // 键往返（写回面共用 load_config_strict）
        let f = parse_serve_flags(&["--quic=false".to_owned()]).unwrap();
        assert_eq!(f.quic, Some(false));
        let _ = std::fs::remove_dir_all(&d);
    }

    /// F0（Q-I 尾段）：空值 carve-out 四 flag（stun/stun6/relay/ddns）——等号形/空格形
    /// 空值都收，语义 = 关/清空；且经 `assemble_result` 落到 `ServeConfig`
    /// （stun/stun6 空串、relay 显式 None、ddns 空表）。
    /// 边界：`--state=`/`--public-endpoint=` 等**不在** carve-out（`take_value` 层仍判 Empty，
    /// parser 站点仍 exit 2）；生产调用点各自有 `--flag=` 形态的 E2E（本文件下方与 qh E2E）。
    #[test]
    fn empty_value_carveout_flags_accept_empty() {
        let f = parse_serve_flags(&[
            "--stun=".to_owned(),
            "--stun6".to_owned(),
            "".to_owned(),
            "--relay=".to_owned(),
            "--ddns=".to_owned(),
            "--public-endpoint=127.0.0.1:42659".to_owned(),
        ])
        .unwrap();
        assert_eq!(f.stun.as_deref(), Some(""));
        assert_eq!(f.stun6.as_deref(), Some(""));
        assert_eq!(f.relay.as_deref(), Some(""));
        assert_eq!(f.ddns.as_deref(), Some(""));
        // 空值形态不再 exit 2（回归面：Q-H 后本地 harness 全起不来）
        let d = tmp_state("emptyok");
        // 裸 IP:port = 合法 relay 值域（开放模式；rl1 token 需要真 token，测试不用）
        write_cfg(&d, "[serve]\nrelay = \"127.0.0.1:41741\"\nstun = \"stun.example:3478\"\n");
        let state = d.display().to_string();
        write_cfg(&d, "[serve]\nrelay = \"127.0.0.1:41741\"\nstun = \"stun.example:3478\"\nddns = [{ domain = \"a.example\" }]\n");
        let cfg = assemble_result(&[
            "--state".to_owned(),
            state,
            "--relay=".to_owned(),
            "--stun=".to_owned(),
            "--ddns=".to_owned(),
        ])
        .expect("空值 carve-out 形态不得报错");
        assert_eq!(cfg.relay, None, "显式空 relay = 关注册腿（覆盖 config）");
        assert_eq!(cfg.stun, "", "显式空 stun = 关");
        assert!(cfg.ddns.is_empty(), "显式空 ddns = 清空 config 全部条目（Go 同义）");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Q-L L3：`--bind-interface=` 空值 carve-out（第五个）——Go `ResolveBind("")` 先
    /// TrimSpace 再判空 ⇒ auto；同时钉 **trim/lowercase 枚举形态**（`" AUTO "`/`NONE`
    /// 修前漂到 `Explicit` 打伪告警）与**网卡名保原大小写**（只去首尾空白——Go
    /// `InterfaceByName(v)` 吃 trim 后的原串）。
    #[test]
    fn bind_interface_empty_and_enum_forms() {
        // flag 层两形态空值都收（修前 exit 2）
        let f = parse_serve_flags(&["--bind-interface=".to_owned()]).unwrap();
        assert_eq!(f.bind_interface.as_deref(), Some(""));
        let f = parse_serve_flags(&["--bind-interface".to_owned(), "".to_owned()]).unwrap();
        assert_eq!(f.bind_interface.as_deref(), Some(""));
        // 经 assemble_result 落 BindMode::Auto（flag 形态 + config 形态同 parse 层）
        let d = tmp_state("bindempty");
        let state = d.display().to_string();
        let cfg = assemble_result(&[
            "--state".to_owned(),
            state.clone(),
            "--bind-interface=".to_owned(),
        ])
        .expect("--bind-interface= 不得报错");
        assert_eq!(cfg.bind_iface, BindMode::Auto, "空值 ⇒ auto（Go ResolveBind(\"\")）");
        write_cfg(&d, "[serve]\nbind_interface = \"\"\n");
        let cfg = assemble_result(&["--state".to_owned(), state]).unwrap();
        assert_eq!(cfg.bind_iface, BindMode::Auto, "config 空串 ⇒ auto（不再打伪告警）");
        let _ = std::fs::remove_dir_all(&d);
        // 枚举形态：trim + 大小写不敏感（关键词判定用 lower）
        assert_eq!(parse_bind_iface(""), BindMode::Auto);
        assert_eq!(parse_bind_iface("  "), BindMode::Auto);
        assert_eq!(parse_bind_iface(" AUTO "), BindMode::Auto);
        assert_eq!(parse_bind_iface("NONE"), BindMode::Off);
        assert_eq!(parse_bind_iface("Off"), BindMode::Off);
        assert_eq!(parse_bind_iface("no"), BindMode::Off);
        // 网卡名保原大小写（只去首尾空白）
        assert_eq!(parse_bind_iface("en0"), BindMode::Explicit("en0".to_owned()));
        assert_eq!(parse_bind_iface("en0 "), BindMode::Explicit("en0".to_owned()));
        assert_eq!(parse_bind_iface(" EN0"), BindMode::Explicit("EN0".to_owned()));
        // IP 字面量按 trim 后解析
        assert_eq!(
            parse_bind_iface(" 192.0.2.7 "),
            BindMode::Addr("192.0.2.7".parse().unwrap())
        );
    }

    /// F15：前台默认 state 与统一进程一致。
    #[test]
    fn default_state_matches_unified() {
        let f = ServeFlags::default();
        assert_eq!(default_state_or(&f), crate::unified_cli::default_state_dir());
        assert_eq!(
            token_state_dir(&[]),
            crate::unified_cli::default_state_dir()
        );
    }

    /// F16：read_stop_signal 四档（字节/EOF/未安装/EINTR 注入重试）。
    #[test]
    fn read_stop_signal_cases() {
        // 未安装（fd < 0）⇒ Err（绝不读 fd 0）。
        assert!(read_stop_signal(-1).is_err());
        // 真管道：写 1 字节 ⇒ Signaled；关写端（EOF）⇒ Signaled。
        let (r, w) = homeway_core::sysfd::pipe_cloexec().unwrap();
        let rfd = std::os::fd::IntoRawFd::into_raw_fd(r);
        let wfd = std::os::fd::IntoRawFd::into_raw_fd(w);
        unsafe { libc::write(wfd, b"x".as_ptr().cast(), 1) };
        assert_eq!(read_stop_signal(rfd).unwrap(), StopWait::Signaled);
        // 关写端 ⇒ 读到 0 字节 EOF ⇒ 同样 Signaled（不关写端会阻塞——先关再读）。
        unsafe { libc::close(wfd) };
        assert_eq!(read_stop_signal(rfd).unwrap(), StopWait::Signaled); // EOF
        unsafe { libc::close(rfd) };
        // EINTR：注入 read 先返 Interrupted 再返 1（用注入缝——进程级 raise(SIGUSR1)
        // 在 cargo test 并发下有误伤面，且 macOS 的 raise 是线程定向、不保证命中
        // 在途 read ⇒ 形态脆弱；注入缝钉的是同一段重试逻辑，且**不碰 errno**
        // 〔libc::__error 只存在于 apple/bsd——用了会让 linux CI 编译失败〕）。
        let mut calls = 0;
        let mut fake = |_fd: i32, buf: &mut [u8]| {
            calls += 1;
            if calls == 1 {
                return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "注入 EINTR"));
            }
            buf[0] = b'x';
            Ok(1)
        };
        assert_eq!(read_stop_signal_with(3, &mut fake).unwrap(), StopWait::Signaled);
        assert_eq!(calls, 2, "EINTR 后必须重试一次");
        // 其它错误 ⇒ Err（可读文案），不重试。
        let mut calls2 = 0;
        let mut bad = |_fd: i32, _b: &mut [u8]| {
            calls2 += 1;
            Err(std::io::Error::other("注入 IO 错"))
        };
        let e = read_stop_signal_with(3, &mut bad).unwrap_err();
        assert!(e.contains("读停止管道失败"), "{e}");
        assert_eq!(calls2, 1, "非 EINTR 不重试");
    }

    /// 壳：未安装时 wait_stop_pipe 返回 PipeErr（不 exit、不读 fd 0）。
    #[test]
    fn wait_stop_pipe_shell_reports_pipe_err() {
        match wait_stop_pipe() {
            StopWait::PipeErr(e) => assert!(e.contains("未安装"), "{e}"),
            other => panic!("未安装应 PipeErr，实得 {other:?}"),
        }
    }

    /// F13：默认 config 模板同时过「serde 严格表 + load_config_strict 值域层」，
    /// 且注释键表与 schema 字段名一致（防未来漂移）。
    #[test]
    fn default_config_template_passes_strict() {
        let tpl = homeway_core::nodestate::DEFAULT_CONFIG_TOML;
        // ① serde 严格表可解析。
        let fc: FileConfig = toml::from_str(tpl).expect("模板必须能被唯一 schema 解析");
        // ② 落盘后过 load_config_strict（含值域）。
        let d = tmp_state("tpl");
        write_cfg(&d, tpl);
        let fc2 = load_config_strict(&d).expect("模板必须过严格读（含值域）");
        assert_eq!(fc2.serve.listen, fc.serve.listen);
        let _ = std::fs::remove_dir_all(&d);
        // ③ 注释键名：burst_kb（不是 burst_kib）+ public_endpoint 在表。
        assert!(tpl.contains("burst_kb"), "注释键必须写 burst_kb");
        assert!(!tpl.contains("burst_kib"), "burst_kib 是错键（Q-A 遗留）");
        assert!(tpl.contains("public_endpoint"), "注释键表须列 public_endpoint");
        // ④ 注释键表逐键 = 本断言清单（schema 字段漂移时先破这里）。
        const SERVE_KEYS: &[&str] = &[
            "enabled", "listen", "bind_interface", "upnp", "stun", "stun6", "relay", "max_peers",
            "peer_ttl", "dns_port", "files_root", "public_endpoint",
            // M1：QUIC 面（总开关 + 独立端口）
            "quic_listen", "quic",
            // Q-J F2 五键
            "dns_upstream", "dns_fallback", "ddns_resolver", "dns_probe_target", "stun_probe_target",
        ];
        const RELAY_KEYS: &[&str] = &["enabled", "listen", "advertise"];
        // M2 S3-4：`[serve.quic_admit]` 七键（六行——per_src_fails/per_src_window 同行）
        const QUIC_ADMIT_KEYS: &[&str] = &[
            "retry_token_lifetime", "per_src_fails", "per_src_window", "nonce_ttl",
            "admit_deadline", "proof_fail_threshold", "retry_policy",
        ];
        for k in SERVE_KEYS
            .iter()
            .chain(RELAY_KEYS.iter())
            .chain(QUIC_ADMIT_KEYS.iter())
        {
            assert!(
                tpl.contains(k),
                "模板注释键表缺 {k}"
            );
        }
        assert!(tpl.contains("[serve.quic_admit]"), "注释键表须列 quic_admit 节名");
        // 逐键「能被 schema 接受」复核：把每个键以合法值形态喂进去必须 Ok。
        // （含 M2 的 `[serve.quic_admit]` 全七键——显式配置当场过语法 + 值域。）
        let served = "[serve]\nenabled = true\nlisten = 41641\nquic = true\nbind_interface = \"auto\"\nupnp = false\n\
             stun = \"\"\nstun6 = \"\"\nrelay = \"\"\nmax_peers = 32\npeer_ttl = \"168h\"\n\
             dns_port = 5300\nfiles_root = \"\"\npublic_endpoint = \"\"\n\
             dns_upstream = [\"1.1.1.1\", \"9.9.9.9:5353\"]\ndns_fallback = \"223.5.5.5\"\n\
             ddns_resolver = [\"223.5.5.5\"]\ndns_probe_target = [\"223.5.5.5:53\"]\n\
             stun_probe_target = [\"162.159.207.1:3478\"]\n\
             [serve.quic_admit]\nretry_token_lifetime = \"5s\"\nper_src_fails = 16\n\
             per_src_window = \"10s\"\nnonce_ttl = \"5s\"\nadmit_deadline = \"10s\"\n\
             proof_fail_threshold = 10\nretry_policy = \"pressure\"\n\
             [relay]\nenabled = false\nlisten = \":41741\"\nadvertise = \"\"\n";
        let fc = toml::from_str::<FileConfig>(served).expect("全键（含 quic_admit 七键）可解析");
        let limits = fc
            .serve
            .quic_admit
            .as_ref()
            .expect("quic_admit 节在")
            .resolve()
            .expect("显式值合法");
        assert_eq!(
            limits,
            homeway_core::server::quic_admit::AdmitLimits::default(),
            "显式写全的合法值 == 设计缺省（缺省不改行为的对照）"
        );
        // 缺省（不写 `[serve.quic_admit]`）⇒ 与设计缺省逐值同（不改行为）。
        let bare: FileConfig = toml::from_str("[serve]\nenabled = true\n").unwrap();
        assert!(bare.serve.quic_admit.is_none(), "缺省即无节");
        assert_eq!(
            bare.serve.quic_admit.unwrap_or_default().resolve().unwrap(),
            homeway_core::server::quic_admit::AdmitLimits::default()
        );
    }

    /// S7b/Q5：`--public-endpoint` 显式值**当场校验**（对齐 Go `cli.go:98-105` 的
    /// fail-fast）——非法 ⇒ `CliErr::Usage`（前台 exit 2）+ 可行动文案；合法与显式空值放行。
    #[test]
    fn public_endpoint_flag_is_validated_fail_fast() {
        let d = tmp_state("peflag");
        write_cfg(&d, "[serve]\n");
        let state = d.display().to_string();
        let ok = |v: &str| {
            assemble_result(&[
                "--state".to_owned(),
                state.clone(),
                "--public-endpoint".to_owned(),
                v.to_owned(),
            ])
        };
        // 合法：v4/v6 字面 + 多段 + 空白容错（值原样搬运，不做归一）
        let cfg = ok(" 1.2.3.4:41641 , [::1]:41641 ").expect("合法值放行");
        assert_eq!(cfg.public_endpoint, " 1.2.3.4:41641 , [::1]:41641 ");
        // 显式空值：**flag 层就拒**（`take_value_or_exit(..,false)` 的既有行为，exit 2；
        // Go `--public-endpoint=` 同样报错——`ParseAddrPort("")` 失败）⇒ 不会走到本校验函数；
        // 校验函数内的空值早退只是防御面（直接构造 `ServeConfig` 的调用方）。
        assert!(validate_public_endpoint_flag("").is_ok(), "空值在校验函数内 = no-op 防御");
        // 非法：缺端口 / 非地址 / 端口 0 / 端口越界 ⇒ 一律 Usage（exit 2）+ 文案带值
        for bad in ["1.2.3.4", "nonsense", "1.2.3.4:0", "1.2.3.4:70000", "1.2.3.4:1,bad"] {
            match ok(bad) {
                Err(CliErr::Usage(m)) => {
                    assert!(
                        m.starts_with("--public-endpoint ") && m.contains("逗号分隔的 ip:port"),
                        "文案须可行动（值 + 期望形态）：{m}"
                    );
                }
                other => panic!("非法值须 Usage（exit 2）：{bad}（is_ok={}）", other.is_ok()),
            }
        }
        let _ = std::fs::remove_dir_all(&d);
    }
}
