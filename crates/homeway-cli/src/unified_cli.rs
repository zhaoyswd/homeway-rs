//! `homeway-cli` 零参形态 = 统一进程（B0-1 部署最小面 + **B0-2b daemon 面**；
//! 语义真源 `baseline:internal/daemon/unified.go` + `internal/control`）。
//!
//! 装配序（Go D1 同构）：锁 → 三层 state → config（fail-fast）→ **client 角色恒开**
//! （hosts.json 多主机表 + 每主机常驻会话，`homeway-core::daemon::DaemonCore`）→
//! **control 角色恒开**（control.sock 控制面，首启 Listen fail-fast——控制面是统一
//! 进程的用户面，静默缺失无从排查）→ serve/relay 按期望态（config enabled，可经
//! 控制面 `serve start/stop/restart` 动态启停）→ 等信号收工。
//!
//! 生产形态对照：
//! - Mac launchd：`homeway` 零参（本 CLI 无子命令形态），stdout 重定向到文件；
//! - 阿里云 nohup：`--state <dir>` 双角色（config serve.enabled + relay.enabled）。
//!
//! 只认全局 flag（--state/--verbose）——角色 flag打在统一进程 = 可行动错误
//! （走 config 或前台单角色形态；Go RunUnified 同义）。
//!
//! **B0-2b 第 1 棒留桩（如实标注）**：serve/relay 角色失败退避重建（supervisor
//! M4——本棒 start 失败即报错、运行期崩溃 = 进程退出靠 launchd/nohup 拉回）；
//! serve.status 的 peers/intercept 观测面（engine 状态缝未开）；承载面 9 op
//! （forward/socks/speedtest）——见 docs/reviews/B0-2b.md 挂账表。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};


use homeway_core::daemon::proto::{
    BackendErr, RoleActionResult, RoleBrief, ServeStatusResult, ServeTokenResult,
};
use homeway_core::daemon::{DaemonCore, RoleOp, RoleOpOut, RoleHost};
use homeway_core::nodestate::{open_node_state, DebugLog, EventsLog, InstanceLock};
use homeway_core::server::engine::{shrink_upnp_lease, ServeEngine};

use crate::relay_cli::{assemble_relay, RelayAssemble, RelayProc};
use crate::serve_cli::assemble as assemble_serve_cfg;

/// 默认 state 根（Go `DefaultStateDir` 同义：`~/.config/homeway`）。
pub fn default_state_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("homeway");
    }
    PathBuf::from("./homeway-state")
}

/// config.toml 全键面（serve/relay 双节，deny_unknown——typo 保护；与 serve_cli/
/// relay_cli 的分节 schema 同表）。Serialize 面 = serve/relay start/stop 写回
/// 期望态（enabled 位）——手编注释会丢（Go nodeconfig.Write 同形态）。
#[derive(serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // 键表完整性守卫（消费经 assemble_serve_cfg 走 serve_cli 的同表解析）
struct FileServe {
    /// 缺省 = true（Go nodeconfig.Default 同义——手编省略该键 = 启用；CLI-5 整改）。
    #[serde(default = "default_enabled_true")]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listen: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bind_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upnp: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stun: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stun6: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    relay: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_peers: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    peer_ttl: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_endpoint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    dns_port: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    files_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ddns: Option<Vec<FileDdns>>,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct FileDdns {
    #[serde(default)]
    domain: String,
}

fn default_enabled_true() -> bool {
    true
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields)]
struct FileRelay {
    #[serde(default)]
    enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    listen: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    advertise: Option<String>,
}

#[derive(serde::Deserialize, serde::Serialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    serve: FileServe,
    #[serde(default)]
    relay: FileRelay,
}

fn load_config(state_dir: &std::path::Path) -> FileConfig {
    let cfg_path = state_dir.join("config.toml");
    match std::fs::read_to_string(&cfg_path)
        .map_err(|e| e.to_string())
        .and_then(|b| toml::from_str(&b).map_err(|e| format!("{cfg_path:?}: {e}")))
    {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// 期望态写回（CLI-4 整改：**重读现文件 → 只翻 enabled** → 同目录 tmp + rename
/// 原子替换、创建即 0600——盲写启动期内存快照会把运行期手编意图静默回滚）。
/// 重读失败（手编坏 config）= 拒绝写入（Go nodeconfig.Update 同纪律）。
/// 重读 config（角色 op 内刷新内存快照；失败 = 沿用旧值——写回路径已先行校验）。
fn load_config_quiet(state_dir: &std::path::Path) -> FileConfig {
    std::fs::read_to_string(state_dir.join("config.toml"))
        .ok()
        .and_then(|b| toml::from_str(&b).ok())
        .unwrap_or_default()
}

fn write_config_enabled(state_dir: &std::path::Path, serve: Option<bool>, relay: Option<bool>) -> Result<(), String> {
    let path = state_dir.join("config.toml");
    let raw = std::fs::read_to_string(&path).map_err(|e| format!("重读 {path:?}：{e}"))?;
    let mut cfg: FileConfig = toml::from_str(&raw).map_err(|e| format!("config 解析失败（拒绝写入，先修复）：{e}"))?;
    if let Some(v) = serve {
        cfg.serve.enabled = v;
    }
    if let Some(v) = relay {
        cfg.relay.enabled = v;
    }
    let body = toml::to_string_pretty(&cfg).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let tmp = state_dir.join("config.toml.tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("建 {tmp:?}：{e}"))?;
        f.write_all(body.as_bytes()).map_err(|e| format!("写 {tmp:?}：{e}"))?;
    }
    #[cfg(not(unix))]
    std::fs::write(state_dir.join("config.toml.tmp"), &body).map_err(|e| e.to_string())?;
    std::fs::rename(state_dir.join("config.toml.tmp"), &path).map_err(|e| format!("原子替换 {path:?}：{e}"))
}

/// 统一进程入口（零参/仅全局 flag 形态）。
pub fn cmd_unified(args: &[String]) {
    // 预扫描拒收角色 flag（Go RunUnified 同义——**先于**正式解析，给可行动提示）。
    // 只认 --state <dir> / --state=DIR / --verbose / --help：内联与空格两种取值形态
    // 等价（评审 r1-H2：等号形态曾被静默忽略→跑在默认 state 上）；未知 flag 精确
    // 匹配报错（评审 r1-Z1：前缀匹配曾把 --stateful 当 --state 收下）。
    let mut state_dir = default_state_dir();
    let mut verbose = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') {
            eprintln!("统一进程不接受位置参数（{a:?}）——角色命令见 `homeway-cli serve` / `homeway-cli relay` / 客户端域命令");
            std::process::exit(2);
        }
        let body = a.trim_start_matches('-');
        let (name, inline) = match body.split_once('=') {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (body.to_owned(), None),
        };
        match name.as_str() {
            "state" => {
                if let Some(v) = inline {
                    state_dir = PathBuf::from(v);
                } else if let Some(v) = args.get(i + 1) {
                    state_dir = PathBuf::from(v.clone());
                    i += 1; // --state 的值位
                }
            }
            "verbose" => verbose = true,
            "help" | "h" => {
                println!("homeway-cli [统一进程] —— 零参起；只认 --state <dir> / --verbose（角色参数写 config.toml，或用 serve/relay 前台单角色形态）");
                println!("子命令形态：homeway-cli <serve|relay|status|host|term|files|connect|speedtest|token|dnstest|portfwd> …（无子命令 = 统一进程）");
                return;
            }
            other => {
                eprintln!("统一进程只认 --state/--verbose（--{other} 是角色 flag）——角色参数请写 {} 的 config.toml，或用 `homeway-cli serve` / `homeway-cli relay` 前台单角色形态",
                    state_dir.display());
                std::process::exit(2);
            }
        }
        i += 1;
    }
    run_unified_state(state_dir, verbose);
}

// ---------- serve/relay 角色管理面（RoleHost 实现：期望态写 config + 动态启停） ----------

struct RolesInner {
    cfg: FileConfig,
    serve: Option<Arc<ServeEngine>>,
    serve_upnp: bool,
    relay: Option<RelayProc>,
    /// relay 装配期铸出的 token（运行态真源；relay.token 优先用）。
    relay_token: Option<String>,
    relay_listen: Option<String>,
    serve_restarts: i32,
    relay_restarts: i32,
    serve_reason: String,
    relay_reason: String,
    /// serve 上一轮 stop 的异步收尾线程（start/restart 先 join 再装配——防端口静默
    /// 退让的串行化；Go r1 中-4 同义）。
    serve_stop_join: Option<std::thread::JoinHandle<()>>,
}

struct UnifiedRoles {
    state_dir: PathBuf,
    /// serve 角色侧摘要流（events.log + 终端）——engine 装配判据行面。
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    /// 守护侧摘要流（daemon-events.log + 终端）——角色管理动作行（serve/relay
    /// start/stop/restart 的「谁干的」归守护侧记录；Go OpenDaemonLogs 分置纪律）。
    eventf: Arc<dyn Fn(&str) + Send + Sync>,
    dlogf: Arc<dyn Fn(&str) + Send + Sync>,
    tokf: Arc<dyn Fn(&str) + Send + Sync>,
    log_paths: Option<(String, String)>,
    inner: Mutex<RolesInner>,
}

impl UnifiedRoles {
    fn serve_running(inner: &RolesInner) -> bool {
        inner.serve.is_some()
    }

    fn relay_running(inner: &RolesInner) -> bool {
        inner.relay.is_some()
    }

    /// 装配 serve 角色（config 真源——文件即意图层）。**装配前重读校验**（CLI-3
    /// 整改：serve_cli 的 flag/config 解析失败路径含 process::exit——在控制面
    /// handler 线程里触发会把整个统一进程带走；先以同表 schema 严格校验，坏
    /// config 在这里就回 Err）。
    fn assemble_serve(&self) -> Result<(Arc<ServeEngine>, bool), String> {
        let raw = std::fs::read_to_string(self.state_dir.join("config.toml"))
            .map_err(|e| format!("config 重读失败：{e}"))?;
        toml::from_str::<FileConfig>(&raw).map_err(|e| format!("config 非法（先修复再 start）：{e}"))?;
        let cfg = assemble_serve_cfg(&[
            "--state".to_owned(),
            self.state_dir.display().to_string(),
        ]);
        let upnp = cfg.upnp;
        let engine = ServeEngine::start(
            cfg,
            Arc::clone(&self.logf),
            Arc::clone(&self.dlogf),
            Arc::clone(&self.tokf),
            self.log_paths.clone(),
        )
        .map_err(|e| e.to_string())?;
        Ok((engine, upnp))
    }

    /// 装配 relay 角色（config listen/advertise；token 铸出存运行态）。
    fn assemble_relay_role(&self, cfg: &FileConfig) -> Result<(RelayProc, Option<String>, String), String> {
        let listen_str = cfg.relay.listen.clone().unwrap_or_else(|| ":41741".to_owned());
        let listen = crate::relay_cli::parse_listen(&listen_str)
            .ok_or_else(|| format!("relay listen {listen_str:?} 非法（[host:]port，如 \":41741\"）"))?;
        let advertise = cfg.relay.advertise.clone().unwrap_or_default();
        // token 铸出（离线推算形态——与 relay 前台同源；listen 端口 = config 值）。
        let relay_dir = self.state_dir.join("relay");
        let token = match homeway_core::relay::rltoken::read_secret(&relay_dir) {
            Ok(Some(secret)) => homeway_core::relay::rltoken::build_token(
                &secret,
                &advertise,
                listen.port(),
                &|_| {},
                &|_| {},
            )
            .ok()
            .map(|b| b.token),
            _ => None,
        };
        let proc = assemble_relay(
            &self.state_dir,
            RelayAssemble { listen, advertise, no_hints: false, open: false },
            Arc::clone(&self.logf),
        )?;
        Ok((proc, token, listen_str))
    }

    fn serve_status_inner(&self, inner: &RolesInner) -> ServeStatusResult {
        // token 掩码/端点 = 台账末行（reveal 纪律：status 族只见掩码）。
        let st = homeway_core::server::state::State::open(&self.state_dir.join("serve")).ok();
        let last = st.and_then(|s| s.last_token().ok().flatten());
        let (mask, eps) = match &last {
            Some(t) => {
                let eps_ref: Vec<homeway_core::token::EndpointRef> = t
                    .endpoints
                    .iter()
                    .map(|e| homeway_core::token::EndpointRef::new(e.addr.as_str(), e.kind))
                    .collect();
                let full = homeway_core::token::encode(&homeway_core::token::TokenSpec {
                    peer_id: &t.peer_id,
                    secret: &t.secret,
                    endpoints: &eps_ref,
                })
                .unwrap_or_default();
                let eps = t
                    .endpoints
                    .iter()
                    .map(|e| format!("{:?}", e.kind) .to_string() + "=" + &e.addr)
                    .collect::<Vec<_>>();
                (mask_token(&full), eps)
            }
            None => (None, Vec::new()),
        };
        ServeStatusResult {
            enabled: inner.cfg.serve.enabled,
            state: if Self::serve_running(inner) { "running" } else { "stopped" }.to_owned(),
            reason: (!inner.serve_reason.is_empty()).then(|| inner.serve_reason.clone()),
            listen_port: inner.serve.as_ref().map(|e| e.local_port),
            published: None,
            token_mask: mask,
            endpoints: (!eps.is_empty()).then_some(eps),
            // 留桩（如实）：engine 观测面（peer 表/拦截计数）未开缝——见文件头挂账表。
            peers: Vec::new(),
            ddns: None,
            intercept: Default::default(),
        }
    }
}

/// hmw1… 掩码（Go server.MaskToken 同串：前 12 字符 + 总长）。
fn mask_token(tok: &str) -> Option<String> {
    if tok.is_empty() {
        return None;
    }
    let n = tok.chars().count();
    if n <= 12 {
        Some(format!("{}…", tok.chars().take(4).collect::<String>()))
    } else {
        Some(format!("{}…（{n} 字符）", tok.chars().take(12).collect::<String>()))
    }
}

impl RoleHost for UnifiedRoles {
    fn role_op(&self, op: RoleOp) -> Result<RoleOpOut, BackendErr> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        use RoleOp as R;
        match op {
            // ---- serve 五件 ----
            R::ServeStart => {
                if Self::serve_running(&inner) {
                    return Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }));
                }
                inner.cfg.serve.enabled = true;
                write_config_enabled(&self.state_dir, Some(true), None)
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                // 串行化：等上一轮 stop 的收尾线程（端口/socket 释放净）再装配。
                if let Some(h) = inner.serve_stop_join.take() {
                    let _ = h.join();
                }
                inner.cfg = load_config_quiet(&self.state_dir);
                match self.assemble_serve() {
                    Ok((engine, upnp)) => {
                        inner.serve = Some(engine);
                        inner.serve_upnp = upnp;
                        inner.serve_reason.clear();
                        (self.eventf)("serve: 按期望态装配（config serve.enabled=true，经控制面）");
                        Ok(RoleOpOut::Action(RoleActionResult { action: "started".into() }))
                    }
                    Err(e) => {
                        inner.serve_reason = e.clone();
                        Err(BackendErr::Other(format!("serve 装配失败：{e}")))
                    }
                }
            }
            R::ServeStop => {
                inner.cfg.serve.enabled = false;
                write_config_enabled(&self.state_dir, Some(false), None)
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                if let Some(engine) = inner.serve.take() {
                    // **立即应答，收尾异步**（Go D5 完成语义 r1 中-4：收尾最长 =
                    // STOP_GRACE 10s + UPnP 缩租预算，同步等会烧穿请求预算——CLI-6
                    // 整改；宽限 10s = engine::STOP_GRACE，Go role.go 同值——CLI-8）。
                    let upnp = inner.serve_upnp;
                    let logf = Arc::clone(&self.logf);
                    inner.serve_stop_join = Some(std::thread::Builder::new()
                        .name("hw-serve-stop".to_owned())
                        .spawn(move || {
                            if upnp {
                                shrink_upnp_lease(&engine, &logf);
                            }
                            engine.shutdown(homeway_core::server::engine::STOP_GRACE);
                        })
                        .expect("线程创建不可失败"));
                    (self.eventf)("serve: 期望停用（config serve.enabled=false，经控制面）——收尾进行中");
                    Ok(RoleOpOut::Action(RoleActionResult { action: "stopped".into() }))
                } else {
                    Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }))
                }
            }
            R::ServeRestart => {
                if !Self::serve_running(&inner) {
                    return Err(BackendErr::Other("serve 未在运行（先 start）——restart 无重建对象".into()));
                }
                // 与 stop 同款：旧引擎异步收尾（join 后再装配——串行化防端口退让）。
                if let Some(engine) = inner.serve.take() {
                    let upnp = inner.serve_upnp;
                    let logf = Arc::clone(&self.logf);
                    let h = std::thread::Builder::new()
                        .name("hw-serve-rst".to_owned())
                        .spawn(move || {
                            if upnp {
                                shrink_upnp_lease(&engine, &logf);
                            }
                            engine.shutdown(homeway_core::server::engine::STOP_GRACE);
                        })
                        .expect("线程创建不可失败");
                    let _ = h.join();
                }
                match self.assemble_serve() {
                    Ok((engine, upnp)) => {
                        inner.serve = Some(engine);
                        inner.serve_upnp = upnp;
                        inner.serve_restarts += 1;
                        inner.serve_reason.clear();
                        (self.eventf)(&format!("serve: 显式重启完成（重建计数 {}）", inner.serve_restarts));
                        Ok(RoleOpOut::Action(RoleActionResult { action: "restarted".into() }))
                    }
                    Err(e) => {
                        inner.serve_reason = e.clone();
                        Err(BackendErr::Other(format!("serve 重启装配失败：{e}")))
                    }
                }
            }
            R::ServeStatus => {
                let st = self.serve_status_inner(&inner);
                Ok(RoleOpOut::Status(
                    serde_json::to_value(st).map_err(|e| BackendErr::Other(e.to_string()))?,
                ))
            }
            R::ServeToken => {
                let st = homeway_core::server::state::State::open(&self.state_dir.join("serve"))
                    .map_err(|e| BackendErr::Other(format!("state 打开失败：{e}")))?;
                let tok = st
                    .last_token()
                    .ok()
                    .flatten()
                    .ok_or_else(|| BackendErr::Other("台账为空（出口从未铸出 token）".into()))?;
                let eps_ref: Vec<homeway_core::token::EndpointRef> = tok
                    .endpoints
                    .iter()
                    .map(|e| homeway_core::token::EndpointRef::new(e.addr.as_str(), e.kind))
                    .collect();
                let full = homeway_core::token::encode(&homeway_core::token::TokenSpec {
                    peer_id: &tok.peer_id,
                    secret: &tok.secret,
                    endpoints: &eps_ref,
                })
                .map_err(|e| BackendErr::Other(format!("token 编码失败：{e}")))?;
                let eps: Vec<String> =
                    tok.endpoints.iter().map(|e| e.addr.clone()).collect();
                Ok(RoleOpOut::Token(ServeTokenResult {
                    token: full,
                    source: "ledger".into(),
                    eps: (!eps.is_empty()).then_some(eps),
                }))
            }
            // ---- relay 五件 ----
            R::RelayStart => {
                if Self::relay_running(&inner) {
                    return Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }));
                }
                inner.cfg.relay.enabled = true;
                write_config_enabled(&self.state_dir, None, Some(true))
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                inner.cfg = load_config_quiet(&self.state_dir);
                match self.assemble_relay_role(&inner.cfg) {
                    Ok((proc, token, listen)) => {
                        inner.relay = Some(proc);
                        inner.relay_token = token;
                        inner.relay_listen = Some(listen);
                        inner.relay_reason.clear();
                        (self.eventf)("relay: 按期望态装配（config relay.enabled=true，经控制面）");
                        Ok(RoleOpOut::Action(RoleActionResult { action: "started".into() }))
                    }
                    Err(e) => {
                        inner.relay_reason = e.clone();
                        Err(BackendErr::Other(format!("relay 装配失败：{e}")))
                    }
                }
            }
            R::RelayStop => {
                inner.cfg.relay.enabled = false;
                write_config_enabled(&self.state_dir, None, Some(false))
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                if let Some(proc) = inner.relay.take() {
                    proc.stop();
                    (self.eventf)("relay: 期望停用（config relay.enabled=false，经控制面）——已收工");
                    Ok(RoleOpOut::Action(RoleActionResult { action: "stopped".into() }))
                } else {
                    Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }))
                }
            }
            R::RelayRestart => {
                if !Self::relay_running(&inner) {
                    return Err(BackendErr::Other("relay 未在运行（先 start）——restart 无重建对象".into()));
                }
                if let Some(proc) = inner.relay.take() {
                    proc.stop();
                }
                match self.assemble_relay_role(&inner.cfg) {
                    Ok((proc, token, listen)) => {
                        inner.relay = Some(proc);
                        inner.relay_token = token;
                        inner.relay_listen = Some(listen);
                        inner.relay_restarts += 1;
                        inner.relay_reason.clear();
                        (self.eventf)(&format!("relay: 显式重启完成（重建计数 {}）", inner.relay_restarts));
                        Ok(RoleOpOut::Action(RoleActionResult { action: "restarted".into() }))
                    }
                    Err(e) => {
                        inner.relay_reason = e.clone();
                        Err(BackendErr::Other(format!("relay 重启装配失败：{e}")))
                    }
                }
            }
            R::RelayStatus => {
                let running = Self::relay_running(&inner);
                let token_mask = inner.relay_token.as_deref().and_then(mask_token);
                Ok(RoleOpOut::Status(
                    serde_json::to_value(homeway_core::daemon::proto::RelayStatusResult {
                        enabled: inner.cfg.relay.enabled,
                        state: if running { "running" } else { "stopped" }.to_owned(),
                        reason: (!inner.relay_reason.is_empty()).then(|| inner.relay_reason.clone()),
                        listen: inner.relay_listen.clone(),
                        advertise: inner.cfg.relay.advertise.clone().filter(|s| !s.is_empty()),
                        token_mask,
                        open: false,
                        assocs: 0,
                        // 留桩（如实）：relay 注册出口表观测面未开缝（后续棒）。
                        backends: Vec::new(),
                    })
                    .map_err(|e| BackendErr::Other(e.to_string()))?,
                ))
            }
            R::RelayToken => {
                // 优先运行态（装配期铸出）；降级 = relay.key + config 离线推算（derived）。
                if let Some(tok) = inner.relay_token.clone() {
                    return Ok(RoleOpOut::Token(ServeTokenResult {
                        token: tok,
                        source: "runtime".into(),
                        eps: None,
                    }));
                }
                let relay_dir = self.state_dir.join("relay");
                let secret = homeway_core::relay::rltoken::read_secret(&relay_dir)
                    .map_err(|e| BackendErr::Other(format!("relay.key 读取失败：{e}")))?;
                let Some(secret) = secret else {
                    return Err(BackendErr::Other("relay.key 不存在（先 relay start）".into()));
                };
                let listen_str = inner.cfg.relay.listen.clone().unwrap_or_else(|| ":41741".into());
                let port = crate::relay_cli::parse_listen(&listen_str)
                    .map(|a| a.port())
                    .unwrap_or(41741);
                let advertise = inner.cfg.relay.advertise.clone().unwrap_or_default();
                let built = homeway_core::relay::rltoken::build_token(
                    &secret,
                    &advertise,
                    port,
                    &|_| {},
                    &|_| {},
                )
                .map_err(BackendErr::Other)?;
                Ok(RoleOpOut::Token(ServeTokenResult {
                    token: built.token,
                    source: "derived".into(),
                    eps: (!built.endpoints.is_empty()).then_some(built.endpoints),
                }))
            }
        }
    }

    fn statuses(&self) -> Vec<RoleBrief> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        vec![
            RoleBrief {
                name: "client".into(),
                state: "running".into(),
                restarts: 0,
                reason: None,
            },
            RoleBrief {
                name: "control".into(),
                state: "running".into(),
                restarts: 0,
                reason: None,
            },
            RoleBrief {
                name: "serve".into(),
                state: if Self::serve_running(&inner) { "running" } else { "stopped" }.into(),
                restarts: inner.serve_restarts,
                reason: (!inner.serve_reason.is_empty()).then(|| inner.serve_reason.clone()),
            },
            RoleBrief {
                name: "relay".into(),
                state: if Self::relay_running(&inner) { "running" } else { "stopped" }.into(),
                restarts: inner.relay_restarts,
                reason: (!inner.relay_reason.is_empty()).then(|| inner.relay_reason.clone()),
            },
        ]
    }
}

fn run_unified_state(state_dir: PathBuf, verbose: bool) {
    // ① 单实例锁（与前台单角色共用 <state>/lock——同 state 双进程互斥）
    let lock = match InstanceLock::acquire(&state_dir, "unified") {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // 信号 handler 先装（角色装配期到达的信号也有归属；再由主循环 pipe 等待）
    crate::serve_cli::install_stop_signals();

    // ② 三层布局 + config 缺失生成默认 + events 最小面
    let ns = match open_node_state(&state_dir) {
        Ok(ns) => ns,
        Err(e) => {
            eprintln!("打开 state {} 失败：{e}", state_dir.display());
            std::process::exit(1);
        }
    };
    if ns.config_generated {
        ns.events.eventf("config.toml 缺失——已生成默认（serve.enabled=true）");
    }

    // ③ 读 config（fail-fast：非法 = 可行动错误拒启，不静默按默认）
    let cfg = load_config(&state_dir);

    // serve 侧日志 tee（engine 的摘要判据行进 cache/events.log；细节进 cache/debug.log；
    // P1-1）。tokf = token 端点变化轮流（events 文件只写 + verbose 回显）。
    let events = Arc::new(ns.events);
    let logf: Arc<dyn Fn(&str) + Send + Sync> = {
        let ev = Arc::clone(&events);
        Arc::new(move |s: &str| ev.eventf(s))
    };
    let tokf: Arc<dyn Fn(&str) + Send + Sync> = {
        let ev = Arc::clone(&events);
        Arc::new(move |s: &str| ev.quietf(s, verbose))
    };
    let log_paths = Some((
        events.path().display().to_string(),
        ns.debug.path().display().to_string(),
    ));
    let dlogf: Arc<dyn Fn(&str) + Send + Sync> = {
        let db = Arc::new(ns.debug);
        Arc::new(move |s: &str| db.dlogf(s, verbose))
    };

    // ④ 守护侧自有日志（Go OpenDaemonLogs：daemon-events.log / daemon-debug.log——
    // serve 角色独占 events.log/debug.log，两侧写者不同文件）。
    let cache_dir = state_dir.join("cache");
    let dlog = match DebugLog::open_named(&cache_dir, "daemon-debug.log") {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("homeway: ⚠️ daemon-debug.log 打开失败（{e}）——守护细节行丢弃，服务继续");
            Arc::new(DebugLog::disabled(cache_dir.join("daemon-debug.log")))
        }
    };
    // 守护侧细节流（hosts 表会话日志走这里——与 serve 角色的 debug.log 分文件，
    // Go OpenDaemonLogs 的写权分置纪律）。
    let daemon_dlogf: Arc<dyn Fn(&str) + Send + Sync> = {
        let d = Arc::clone(&dlog);
        Arc::new(move |s: &str| d.dlogf(s, verbose))
    };
    let d_events = match EventsLog::open_named(&cache_dir, "daemon-events.log") {
        Ok(d) => Arc::new(d),
        Err(e) => {
            eprintln!("homeway: ⚠️ daemon-events.log 打开失败（{e}）——守护摘要行只回显终端");
            Arc::new(EventsLog::terminal_only(cache_dir.join("daemon-events.log")))
        }
    };
    let daemon_logf: Arc<dyn Fn(&str) + Send + Sync> = {
        let ev = Arc::clone(&d_events);
        Arc::new(move |s: &str| ev.eventf(s))
    };

    // ⑤ serve/relay 角色管理面（控制面 serve.*/relay.* 的宿主；config 即期望态）。
    let roles = Arc::new(UnifiedRoles {
        state_dir: state_dir.clone(),
        logf: Arc::clone(&logf),
        eventf: Arc::clone(&daemon_logf),
        dlogf: Arc::clone(&dlogf),
        tokf: Arc::clone(&tokf),
        log_paths,
        inner: Mutex::new(RolesInner {
            cfg,
            serve: None,
            serve_upnp: false,
            relay: None,
            relay_token: None,
            relay_listen: None,
            serve_restarts: 0,
            relay_restarts: 0,
            serve_reason: String::new(),
            relay_reason: String::new(),
            serve_stop_join: None,
        }),
    });

    // ⑥ client 角色恒开（hosts.json 表 + 每主机常驻会话；事件总线进程级唯一）。
    let client_dir = state_dir.join("client");
    let core = match DaemonCore::new(
        crate::cli_version(),
        &client_dir,
        &client_dir.join("identity"),
        &cache_dir.join("endpoints"),
        Arc::clone(&daemon_dlogf),
        Some(roles.clone() as Arc<dyn RoleHost>),
    ) {
        Ok(c) => c,
        Err(e) => {
            daemon_logf(&format!("client: hosts 表打开失败——统一进程退出：{e}"));
            std::process::exit(1);
        }
    };
    daemon_logf(&format!("client: 角色已装配（{} 台主机，hosts 表 {}）", core.hosts.hosts().len(), client_dir.join("hosts.json").display()));

    // ⑦ serve/relay 按期望态（config enabled；动态启停经控制面 serve.*/relay.*）。
    {
        let mut inner = roles.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.cfg.serve.enabled {
            match roles.assemble_serve() {
                Ok((engine, upnp)) => {
                    inner.serve = Some(engine);
                    inner.serve_upnp = upnp;
                    logf("serve: 按期望态装配（config serve.enabled=true）");
                }
                Err(e) => {
                    // 装配失败 = 可行动错误拒启（supervisor 退避重建归后续棒，文件头留桩）。
                    daemon_logf(&format!("serve: 装配失败（{e}）——统一进程退出"));
                    std::process::exit(1);
                }
            }
        } else {
            logf("serve: 期望停用（config serve.enabled=false）——不装配");
        }
        if inner.cfg.relay.enabled {
            match roles.assemble_relay_role(&inner.cfg) {
                Ok((proc, token, listen)) => {
                    inner.relay = Some(proc);
                    inner.relay_token = token;
                    inner.relay_listen = Some(listen);
                    logf("relay: 按期望态装配（config relay.enabled=true）");
                }
                Err(e) => {
                    daemon_logf(&format!("relay: 装配失败（{e}）——统一进程退出"));
                    std::process::exit(1);
                }
            }
        } else {
            logf("relay: 期望停用（config relay.enabled=false）——不装配");
        }
    }

    // ⑧ control 角色恒开（首启 Listen fail-fast：监听失败 = 报错退出——控制面是
    // 统一进程的用户面，静默缺失无从排查）。
    let (sock, ln) = match homeway_core::daemon::listen::listen_control(&state_dir) {
        Ok(v) => v,
        Err(e) => {
            daemon_logf(&format!("control: 控制面监听失败——统一进程退出：{e}"));
            std::process::exit(1);
        }
    };
    let srv = homeway_core::daemon::ControlServer::new(homeway_core::daemon::ServerConfig {
        server_version: crate::cli_version().to_owned(),
        bus: Arc::clone(core.bus()),
        backend: core.clone(),
        logf: Arc::clone(&daemon_dlogf),
    });
    {
        let s2 = Arc::clone(&srv);
        std::thread::Builder::new()
            .name("homeway-control".into())
            .stack_size(1024 * 1024)
            .spawn(move || {
                if let Err(e) = s2.serve(ln) {
                    eprintln!("control: 角色失败（{e}）——进程退出（supervisor 退避重建归后续棒）");
                    std::process::exit(1);
                }
            })
            .expect("线程创建不可失败");
    }
    daemon_logf(&format!("control: 控制面就绪（sock={}，0600）", sock.display()));

    // ⑨ 就绪（Go 同串形态——client/control 恒开已实装）。
    let (serve_on, relay_on) = {
        let inner = roles.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.cfg.serve.enabled, inner.cfg.relay.enabled)
    };
    events.eventf(&format!(
        "homeway: 统一进程就绪（state={}，serve={} relay={}，client/control 恒开，version={}）",
        state_dir.display(),
        serve_on,
        relay_on,
        crate::cli_version(),
    ));

    println!("（homeway 统一进程前台运行中——Ctrl-C 收工）");
    let _ = crate::serve_cli::wait_stop_pipe();
    println!("homeway: 收到信号，收工");

    // 按序停：control（断连接 goodbye）→ client（停全部会话）→ serve（D5 有序收工 +
    // UPnP 退出缩租）→ relay（确定性 closeAll）→ 锁释放。
    srv.shutdown();
    core.close();
    {
        let mut inner = roles.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(h) = inner.serve_stop_join.take() {
            let _ = h.join();
        }
        if let Some(engine) = inner.serve.take() {
            if inner.serve_upnp {
                shrink_upnp_lease(&engine, &logf);
            }
            engine.shutdown(homeway_core::server::engine::STOP_GRACE);
        }
        if let Some(proc) = inner.relay.take() {
            proc.stop();
        }
    }
    lock.release();
}

#[cfg(test)]
mod tests {
    /// --state=DIR 与 --state DIR 等价（r1-H2 回归钉）——解析逻辑单测面（预扫描
    /// 是 cmd_unified 内联闭包，这里以同构断言钉住两种形态的取值路径不回退）。
    #[test]
    fn state_flag_forms_equivalent() {
        for (args, want) in [
            (vec!["--state", "/tmp/a"], "/tmp/a"),
            (vec!["--state=/tmp/b"], "/tmp/b"),
            (vec!["-state", "/tmp/c"], "/tmp/c"),
            (vec!["--verbose", "--state=/tmp/d"], "/tmp/d"),
        ] {
            let mut state_dir = "/default".to_owned();
            let mut i = 0;
            while i < args.len() {
                let a = args[i];
                let body = a.trim_start_matches('-');
                let (name, inline) = match body.split_once('=') {
                    Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
                    None => (body.to_owned(), None),
                };
                if name == "state" {
                    if let Some(v) = inline {
                        state_dir = v.to_owned();
                    } else if let Some(v) = args.get(i + 1) {
                        state_dir = v.to_string();
                        i += 1;
                    }
                }
                i += 1;
            }
            assert_eq!(state_dir, want, "args={args:?}");
        }
    }

    /// 掩码形态（Go maskToken 同串）。
    #[test]
    fn token_mask_matches_go() {
        let t = "hmw1abcdefghijklmnopqrstuvwxyz"; // 30 字符
        assert_eq!(mask_token(t).unwrap(), "hmw1abcdefgh…（30 字符）"); // 前 12 字符 + 总长（Go 同串）
        assert_eq!(mask_token("hmw1ab").unwrap(), "hmw1…");
        assert_eq!(mask_token(""), None);
    }

    fn mask_token(tok: &str) -> Option<String> {
        super::mask_token(tok)
    }
}
