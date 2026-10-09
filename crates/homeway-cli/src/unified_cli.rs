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
//! **已完成面（原 B0-2b 留桩，逐项兑现）**：supervisor 退避重建（B0-2b 第 2 棒：
//! 退避表 [500ms,1s,5s,30s] 表尾封顶 + serve/relay 双角色看护 + 状态面三态）；
//! serve.status 的 peers/intercept 观测面（EngineCmd::StatusQuery + 原子直读）；
//! 承载面 9 op（forward/socks/speedtest 守护托管——D-1 批）。记录 = docs/reviews/B0-2b.md
//! §七–§十七。

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;


use homeway_core::daemon::proto::{
    BackendErr, RoleActionResult, RoleBrief, ServeStatusResult, ServeTokenResult,
};
use homeway_core::daemon::{DaemonCore, RoleOp, RoleOpOut, RoleHost};
use homeway_core::nodestate::{open_node_state, DebugLog, EventsLog, InstanceLock};
use homeway_core::server::engine::{shrink_upnp_lease, ServeEngine};

use crate::cli_flags;
use crate::relay_cli::{assemble_relay, RelayAssemble, RelayProc};
use crate::serve_cli::{load_config_strict, serve_config_of, FileConfig, FileDdns};

/// 默认 state 根（Go `DefaultStateDir` 同义：`~/.config/homeway`）。
pub fn default_state_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("homeway");
    }
    PathBuf::from("./homeway-state")
}

// config 单表（Q-H F1）：schema 收敛到 `serve_cli` 的唯一表（FileConfig/FileServe/
// FileRelay/FileDdns + `load_config_strict`/`serve_config_of`）——本模块不再有弱表
// （曾用 `Option<toml::Value>` 承载 tx_shape，弱表放行的类型/值域非法会在控制面
// handler 线程里经严格表 exit(1) ⇒ 打崩统一进程；审计 P1 实测两形态）。

/// 期望态写回（CLI-4 整改：**重读现文件 → 只翻 enabled** → 同目录 tmp + rename
/// 原子替换、创建即 0600——盲写启动期内存快照会把运行期手编意图静默回滚）。
/// 重读走严格表（Q-H F1；Go `nodeconfig.Update` 同纪律：坏 config = 拒绝写入）。
pub(crate) fn write_config_enabled(
    state_dir: &std::path::Path,
    serve: Option<bool>,
    relay: Option<bool>,
) -> Result<(), String> {
    let mut cfg = load_config_strict(state_dir)
        .map_err(|e| format!("config 读取失败（拒绝写入，先修复）：{e}"))?;
    if let Some(v) = serve {
        cfg.serve.enabled = v;
    }
    if let Some(v) = relay {
        cfg.relay.enabled = v;
    }
    write_config_file(state_dir, &cfg)
}

/// 读 config 的角色 enabled 位（纯读降级面用；Q-H F1：坏 config = 如实报错——
/// Go `degradedServe` 的「config 读取失败：<err>」同形，不再静默按默认 true）。
pub(crate) fn config_role_enabled(state_dir: &std::path::Path, role: &str) -> Result<bool, String> {
    let cfg = load_config_strict(state_dir)?;
    Ok(if role == "serve" { cfg.serve.enabled } else { cfg.relay.enabled })
}

/// 纯配置写命令的通用改写壳（`serve ddns add/delete`、`serve relay set/clear`
/// 用——Go nodeconfig.Update 同纪律：**重读现文件 → 只改目标键 → 同目录 tmp +
/// rename 原子替换、创建即 0600**；重读失败（手编坏 config）= 拒绝写入）。
/// FileConfig 字段私有——操作面收敛为枚举（调用方不直接碰 config 结构）。
pub(crate) enum ConfigEdit {
    DdnsAdd(String),
    DdnsRemove(String),
    RelaySet(String),
    RelayClear,
}

/// ddns 写结果（CLI 的幂等输出语义）。
pub(crate) enum DdnsWrite {
    Written,
    AlreadyThere,
    Absent,
}

pub(crate) fn update_config(state_dir: &std::path::Path, edit: ConfigEdit) -> Result<DdnsWrite, String> {
    let mut cfg = load_config_strict(state_dir)
        .map_err(|e| format!("config 读取失败（拒绝写入，先修复）：{e}"))?;
    let out = match edit {
        ConfigEdit::DdnsAdd(domain) => {
            let list = cfg.serve.ddns.get_or_insert_with(Vec::new);
            if list.iter().any(|d| d.domain == domain) {
                DdnsWrite::AlreadyThere
            } else {
                list.push(FileDdns { domain });
                DdnsWrite::Written
            }
        }
        ConfigEdit::DdnsRemove(domain) => {
            let mut removed = false;
            if let Some(list) = cfg.serve.ddns.as_mut() {
                let before = list.len();
                list.retain(|d| d.domain != domain);
                removed = list.len() != before;
            }
            if removed { DdnsWrite::Written } else { DdnsWrite::Absent }
        }
        ConfigEdit::RelaySet(tok) => {
            cfg.serve.relay = Some(tok);
            DdnsWrite::Written
        }
        ConfigEdit::RelayClear => {
            cfg.serve.relay = None;
            DdnsWrite::Written
        }
    };
    if matches!(out, DdnsWrite::AlreadyThere | DdnsWrite::Absent) {
        return Ok(out); // 无动作不写盘
    }
    write_config_file(state_dir, &cfg)?;
    Ok(out)
}

/// 读 [[serve.ddns]] 域名表（`serve ddns list` 纯读；坏/缺 config = 如实报错——
/// Q-H F1 起走严格读）。
pub(crate) fn config_serve_ddns(state_dir: &std::path::Path) -> Result<Vec<String>, String> {
    let cfg = load_config_strict(state_dir)?;
    Ok(cfg
        .serve
        .ddns
        .unwrap_or_default()
        .into_iter()
        .map(|d| d.domain)
        .collect())
}

fn write_config_file(state_dir: &std::path::Path, cfg: &FileConfig) -> Result<(), String> {
    let path = state_dir.join("config.toml");
    let body = toml::to_string_pretty(cfg).map_err(|e| e.to_string())?;
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

/// 统一进程参数（零参/仅全局 flag）。
pub(crate) enum UnifiedInvocation {
    /// 运行（默认 state 或显式 `--state`）。
    Run { state_dir: PathBuf, verbose: bool },
    /// `--help`/`-h`：用法已打印，调用方直接返回。
    Help,
}

/// 统一进程参数解析（Q-H F2：`--state` 全形态 fail-fast——缺值/空值/吞 flag 都是
/// exit 2 + 可行动文案；取值器 = `cli_flags` 唯一实现）。
pub(crate) fn parse_unified_args(args: &[String]) -> UnifiedInvocation {
    let mut state_dir = default_state_dir();
    let mut verbose = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        let Some((name, inline)) = cli_flags::split_flag(a) else {
            eprintln!("统一进程不接受位置参数（{a:?}）——角色命令见 `homeway-cli serve` / `homeway-cli relay` / 客户端域命令");
            std::process::exit(2);
        };
        match name {
            "state" => {
                state_dir = cli_flags::take_state_or_exit(
                    "state",
                    inline,
                    args.get(i + 1).map(String::as_str),
                );
                if inline.is_none() {
                    i += 1; // --state 的值位
                }
            }
            "verbose" => verbose = cli_flags::take_bool_or_exit("verbose", inline, true),
            "help" | "h" => {
                println!("homeway-cli [统一进程] —— 零参起；只认 --state <dir> / --verbose（角色参数写 config.toml，或用 serve/relay 前台单角色形态）");
                println!("子命令形态：homeway-cli <serve|relay|status|host|term|files|connect|speedtest|token|dnstest|portfwd> …（无子命令 = 统一进程）");
                return UnifiedInvocation::Help;
            }
            other => {
                eprintln!("统一进程只认 --state/--verbose（--{other} 是角色 flag）——角色参数请写 {} 的 config.toml，或用 `homeway-cli serve` / `homeway-cli relay` 前台单角色形态",
                    state_dir.display());
                std::process::exit(2);
            }
        }
        i += 1;
    }
    UnifiedInvocation::Run { state_dir, verbose }
}

/// 统一进程入口（零参/仅全局 flag 形态）。
pub fn cmd_unified(args: &[String]) {
    match parse_unified_args(args) {
        UnifiedInvocation::Help => {}
        UnifiedInvocation::Run { state_dir, verbose } => run_unified_state(state_dir, verbose),
    }
}

// ---------- serve/relay 角色管理面（RoleHost 实现：期望态写 config + 动态启停） ----------

struct RolesInner {
    cfg: FileConfig,
    serve: Option<Arc<ServeEngine>>,
    serve_upnp: bool,
    relay: Option<RelayProc>,
    /// 角色「代际」：serve/relay 的一切宿主侧变更（start/stop/restart/重建）各自
    /// 前进——supervisor 线程据此区分「我看的引擎死了但被动态操作接管」（静默退）
    /// 与「仍在属主位上死了」（失败 → 退避重建）。
    serve_epoch: u64,
    relay_epoch: u64,
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
    /// 自引用（构造后注入 Weak；supervisor 线程 spawn 需要 Arc 而 RoleHost::role_op
    /// 只拿得到 &self——经此升级）。
    me: Mutex<Option<std::sync::Weak<UnifiedRoles>>>,
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
    /// 构造后注入自引用（一次性）。
    fn set_self_ref(&self, me: std::sync::Weak<UnifiedRoles>) {
        *self.me.lock().unwrap_or_else(|e| e.into_inner()) = Some(me);
    }

    fn self_arc(&self) -> Option<Arc<UnifiedRoles>> {
        self.me.lock().unwrap_or_else(|e| e.into_inner()).as_ref().and_then(std::sync::Weak::upgrade)
    }

    fn serve_running(inner: &RolesInner) -> bool {
        inner.serve.is_some()
    }

    fn relay_running(inner: &RolesInner) -> bool {
        inner.relay.is_some()
    }

    /// 装配 serve 角色（config 真源——文件即意图层）。**装配前严格读**（Q-H F1：
    /// 值域/类型非法在这里回 Err；控制面 handler 线程内**零 exit**——旧形态经
    /// `serve_cli::assemble` 的 exit(1) 会打崩整个统一进程，审计 P1 实测两形态）。
    fn assemble_serve(&self) -> Result<(Arc<ServeEngine>, bool), String> {
        let fc = load_config_strict(&self.state_dir)?;
        let cfg = serve_config_of(&fc, &self.state_dir)?;
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
        // N3（B0-2a 登记→D-1 收口）：relay 的 ulogf 走**终端 + relay.log**，不经
        // events tee（Go 同形——中继 token/端点变化行不进 events.log；serve 侧的
        // 摘要面才进）。
        let relay_term: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
        let proc = assemble_relay(
            &self.state_dir,
            RelayAssemble { listen, advertise, no_hints: false, open: false },
            relay_term,
        )?;
        Ok((proc, token, listen_str))
    }

    /// serve.status 载荷（评审 r2-13：引擎查询（≤2s 的 StatusQuery 往返）必须在
    /// inner 锁**外**做——观测面不挡角色操作）。
    fn serve_status_out_of_lock(&self) -> ServeStatusResult {
        let engine = {
            let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            inner.serve.clone()
        };
        let bits = engine.as_ref().map(|e| e.status_bits());
        // M3 S2（M2 交下的 L4 项）：出口 QUIC 面计数**原子直读**（不占驱动线程）。
        // 映射在核心侧的单源构造面 `ServeQuicBits::from_snapshot` 里（键表不在此另写）。
        let quic = engine
            .as_ref()
            .and_then(|e| e.quic_snapshot())
            .map(|s| homeway_core::daemon::proto::ServeQuicBits::from_snapshot(&s));
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.serve_status_inner(&mut inner, bits, quic)
    }

    fn serve_status_inner(&self, inner: &mut RolesInner, bits: Option<(Vec<homeway_core::server::engine::EnginePeerBrief>, homeway_core::server::engine::EngineInterceptBits, Vec<homeway_core::server::ddnscheck::DdnsBrief>)>, quic: Option<homeway_core::daemon::proto::ServeQuicBits>) -> ServeStatusResult {
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
                    rpk: t.rpk.as_ref(),
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
        // engine 观测缝（B0-2b：peers 经驱动线程快照 + 拦截计数原子直读；引擎不在
        // 位 = 空表零计数——观测面不复活角色）。bits 在锁外取（评审 r2-13）。
        let (peers, intercept, ddns) = bits
            .map(|(p, i, d)| {
                (
                    p.into_iter()
                        .map(|b| homeway_core::daemon::proto::ServePeerBrief {
                            dev: b.dev,
                            tunnel_ip: b.tunnel_ip.to_string(),
                            last_reg: b.last_reg_ms,
                            idle_ms: b.idle_ms,
                        })
                        .collect::<Vec<_>>(),
                    homeway_core::daemon::proto::ServeInterceptBits {
                        dial_ok: i.dial_ok,
                        dial_fail: i.dial_fail,
                        reject: i.reject,
                        flows: i.flows,
                        udp_drop: i.udp_drop,
                        shape_drop: i.shape_drop,
                        frag_drop: i.frag_drop,
                    },
                    d,
                )
            })
            .unwrap_or_default();
        ServeStatusResult {
            enabled: inner.cfg.serve.enabled,
            state: role_state(Self::serve_running(inner), &inner.serve_reason).to_owned(),
            reason: (!inner.serve_reason.is_empty()).then(|| inner.serve_reason.clone()),
            listen_port: inner.serve.as_ref().map(|e| e.local_port),
            published: None,
            token_mask: mask,
            endpoints: (!eps.is_empty()).then_some(eps),
            peers,
            ddns: (!ddns.is_empty()).then(|| {
                ddns
                    .into_iter()
                    .map(|b| serde_json::to_value(&b).unwrap_or_default())
                    .collect::<Vec<_>>()
            }),
            intercept,
            // M3 S2：出口 QUIC 面计数段（additive；面未起/未装配 = None ⇒ 整段缺席）
            quic,
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

// ---------- supervisor（r1-M4：角色运行期失败 = 进程内退避重建，绝不 exit） ----------

/// 重建退避表（Go defaultRoleBackoff 同值：索引递增取值、越界取表尾——失败节拍
/// 收敛到表尾间隔，不无限加密、也绝不退出进程）。
const ROLE_BACKOFF: [Duration; 4] = [
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(5),
    Duration::from_secs(30),
];

/// 角色三态（Go roleState 同名：running / stopped / failed——非运行且带原因 = failed）。
fn role_state(running: bool, reason: &str) -> &'static str {
    if running {
        "running"
    } else if reason.is_empty() {
        "stopped"
    } else {
        "failed"
    }
}

fn role_backoff(fails: usize) -> Duration {
    ROLE_BACKOFF[fails.min(ROLE_BACKOFF.len() - 1)]
}

/// 退避时长文案（Go time.Duration.String 同形：500ms / 1s / 5s / 30s）。
fn backoff_text(d: Duration) -> String {
    if d.subsec_millis() > 0 {
        format!("{}ms", d.as_millis())
    } else {
        format!("{}s", d.as_secs())
    }
}

impl UnifiedRoles {
    /// serve 角色看护：轮询引擎在世位；死时仍属主（代际未变 + 表内即本台）= 运行期
    /// 失败 → 状态面 failed → Go 同串日志 → 退避 → 重装配（与显式 restart 同一重建面）；
    /// 非属主（动态 stop/restart 已接管）= 静默退。装配失败同路退避重试（Go makeRole
    /// 失败 = Run 快速失败的等价面）。
    fn supervise_serve(self: &Arc<Self>, mut engine: Arc<ServeEngine>, mut epoch: u64) {
        let mut fails = 0usize;
        let mut last_err = String::new();
        loop {
            while engine.alive() {
                std::thread::sleep(Duration::from_millis(500));
            }
            // 属主判定（评审 r2-A/r2-2：只看代际——「表内已无本台」在自身失败路径
            // 也是真〔失败时已置 None〕，混进判据会把首次重建失败当「被接管」而退出。
            // 动态 stop/restart/接管必前进代际 ⇒ 唯一可靠的接管信号）。
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.serve_epoch != epoch {
                    return;
                }
            }
            // 失败路径（Go runRoleLoop 同序：failed → 日志 → 退避 → 重建 → restarts++ → running）
            let reason = if last_err.is_empty() {
                "serve 引擎线程退出（异常终结）".to_owned()
            } else {
                std::mem::take(&mut last_err)
            };
            {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.serve = None;
                inner.serve_reason = reason.clone();
            }
            let wait = role_backoff(fails);
            fails += 1;
            (self.eventf)(&format!(
                "role serve: 失败（{reason}）——退避 {} 后进程内重建",
                backoff_text(wait)
            ));
            std::thread::sleep(wait);
            // 重建前复查属主（评审 r2-2：退避窗内动态 start 可能把新引擎入表——
            // 再装配 = 双引擎双写台账；此时本线程静默退）。
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.serve_epoch != epoch || inner.serve.is_some() {
                    return;
                }
            }
            // 重建（与 ServeStart/Restart 的串行化一致：先等上一轮 stop 收尾再装配）
            let rebuilt = {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(h) = inner.serve_stop_join.take() {
                    drop(inner);
                    let _ = h.join();
                } else {
                    drop(inner);
                }
                self.assemble_serve()
            };
            match rebuilt {
                Ok((e, upnp)) => {
                    let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    if inner.serve_epoch != epoch || inner.serve.is_some() || !inner.cfg.serve.enabled {
                        // 窗口内被动态操作/停用接管：撤掉刚建的引擎（评审 r2-2——
                        // 不撤 = 双引擎并存），静默退。
                        drop(inner);
                        e.shutdown(homeway_core::server::engine::STOP_GRACE);
                        return;
                    }
                    inner.serve_epoch += 1;
                    epoch = inner.serve_epoch;
                    inner.serve = Some(Arc::clone(&e));
                    inner.serve_upnp = upnp;
                    inner.serve_restarts += 1;
                    inner.serve_reason.clear();
                    let n = inner.serve_restarts;
                    drop(inner);
                    (self.dlogf)(&format!("role serve: 第 {n} 次进程内重建"));
                    engine = e;
                }
                Err(e) => {
                    // 装配失败：留在循环里继续退避重试（评审 r2-A——此前下一轮属主
                    // 闸门会误判退出，退避重试只试一次）。
                    last_err = e;
                }
            }
        }
    }

    /// serve 启动装配失败的看护（原 exit(1) 形态的进程内替代）：退避重试装配直至
    /// 成功转 supervise_serve，或期望态被动态翻停。Go：装配错误经 runRoleLoop 同表退避。
    fn bootstrap_serve(self: &Arc<Self>) {
        let mut fails = 0usize;
        loop {
            // 期望态/在跑检查在装配**之前**（评审 r2-3：先装配后查 = 并发 serve start
            // 成功后 bootstrap 仍每 30s 装一次〔必失败〕刷事件 + 覆写 reason）。
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if !inner.cfg.serve.enabled || inner.serve.is_some() {
                    return; // 动态翻停 / 已有人在跑（start 或另一轮 bootstrap）
                }
            }
            let r = self.assemble_serve();
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if !inner.cfg.serve.enabled || inner.serve.is_some() {
                    if let Ok((e, _)) = &r {
                        // 窗口内被接管/翻停：撤掉刚建的引擎。
                        e.shutdown(homeway_core::server::engine::STOP_GRACE);
                    }
                    return;
                }
            }
            match r {
                Ok((engine, upnp)) => {
                    let epoch = {
                        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.serve_epoch += 1;
                        inner.serve = Some(Arc::clone(&engine));
                        inner.serve_upnp = upnp;
                        inner.serve_reason.clear();
                        inner.serve_epoch
                    };
                    (self.eventf)("serve: 退避重建装配成功");
                    self.spawn_serve_supervisor(engine, epoch);
                    return;
                }
                Err(e) => {
                    {
                        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.serve_reason = e.clone();
                    }
                    let wait = role_backoff(fails);
                    fails += 1;
                    (self.eventf)(&format!(
                        "role serve: 失败（装配失败：{e}）——退避 {} 后进程内重建",
                        backoff_text(wait)
                    ));
                    std::thread::sleep(wait);
                }
            }
        }
    }

    fn spawn_serve_supervisor(&self, engine: Arc<ServeEngine>, epoch: u64) {
        let Some(roles) = self.self_arc() else { return };
        std::thread::Builder::new()
            .name("hw-super-serve".to_owned())
            .stack_size(512 * 1024)
            .spawn(move || roles.supervise_serve(engine, epoch))
            .expect("线程创建不可失败");
    }

    /// relay 角色看护：轮询 run 线程在世位；死时仍属主（代际未变 + 表内仍 relay）=
    /// 失败 → 退避 → 重装配。RelayStop 会 take + 前进代际 ⇒ 看护静默退。
    fn supervise_relay(self: &Arc<Self>, mut exited: Arc<std::sync::atomic::AtomicBool>, mut epoch: u64) {
        use std::sync::atomic::Ordering;
        let mut fails = 0usize;
        let mut last_err = String::new();
        loop {
            while !exited.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(500));
            }
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.relay_epoch != epoch {
                    return; // 动态 stop/restart 接管（stop 会 join run 线程 ⇒ exited 也置位；失败路径已置 None——只认代际〔评审 r2-A〕）
                }
            }
            let reason = if last_err.is_empty() {
                "relay run 线程退出（异常终结）".to_owned()
            } else {
                std::mem::take(&mut last_err)
            };
            {
                let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.relay = None;
                inner.relay_reason = reason.clone();
            }
            let wait = role_backoff(fails);
            fails += 1;
            (self.eventf)(&format!(
                "role relay: 失败（{reason}）——退避 {} 后进程内重建",
                backoff_text(wait)
            ));
            std::thread::sleep(wait);
            // 重建前复查属主（评审 r2-2：防与动态 start 双装配）。
            {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if inner.relay_epoch != epoch || inner.relay.is_some() || !inner.cfg.relay.enabled {
                    return;
                }
            }
            let cfg = {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                inner.cfg.clone()
            };
            match self.assemble_relay_role(&cfg) {
                Ok((proc, token, listen)) => {
                    let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                    if inner.relay_epoch != epoch || inner.relay.is_some() || !inner.cfg.relay.enabled {
                        // 窗口内被接管：停掉刚建的 run 线程（`RelayProc` 有幂等 Drop
                        // 兜底——评审 r2-E 的 fd 泄漏面；此处显式 stop 走同一收口），静默退。
                        drop(inner);
                        proc.stop();
                        return;
                    }
                    let exited2 = proc.exited_flag();
                    inner.relay_epoch += 1;
                    epoch = inner.relay_epoch;
                    inner.relay = Some(proc);
                    inner.relay_token = token;
                    inner.relay_listen = Some(listen);
                    inner.relay_restarts += 1;
                    inner.relay_reason.clear();
                    let n = inner.relay_restarts;
                    drop(inner);
                    (self.dlogf)(&format!("role relay: 第 {n} 次进程内重建"));
                    exited = exited2;
                }
                Err(e) => {
                    last_err = e; // 留在循环里继续退避重试（评审 r2-A）
                }
            }
        }
    }

    /// relay 启动装配失败的看护（原 exit(1) 形态的进程内替代）。
    fn bootstrap_relay(self: &Arc<Self>) {
        loop {
            let cfg = {
                let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                if !inner.cfg.relay.enabled || inner.relay.is_some() {
                    return; // 动态翻停 / 已在跑（评审 r2-3）
                }
                inner.cfg.clone()
            };
            match self.assemble_relay_role(&cfg) {
                Ok((proc, token, listen)) => {
                    {
                        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        if inner.relay.is_some() || !inner.cfg.relay.enabled {
                            proc.stop(); // 窗口内被接管：撤掉刚建的（评审 r2-2）
                            return;
                        }
                    }
                    let exited = proc.exited_flag();
                    let epoch = {
                        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.relay_epoch += 1;
                        inner.relay = Some(proc);
                        inner.relay_token = token;
                        inner.relay_listen = Some(listen);
                        inner.relay_reason.clear();
                        inner.relay_epoch
                    };
                    (self.eventf)("relay: 退避重建装配成功");
                    self.spawn_relay_supervisor(exited, epoch);
                    return;
                }
                Err(e) => {
                    {
                        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
                        inner.relay_reason = e.clone();
                    }
                    let wait = role_backoff(0);
                    (self.eventf)(&format!(
                        "role relay: 失败（装配失败：{e}）——退避 {} 后进程内重建",
                        backoff_text(wait)
                    ));
                    std::thread::sleep(wait);
                }
            }
        }
    }

    fn spawn_relay_supervisor(
        &self,
        exited: Arc<std::sync::atomic::AtomicBool>,
        epoch: u64,
    ) {
        let Some(roles) = self.self_arc() else { return };
        std::thread::Builder::new()
            .name("hw-super-relay".to_owned())
            .stack_size(512 * 1024)
            .spawn(move || roles.supervise_relay(exited, epoch))
            .expect("线程创建不可失败");
    }
}

impl RoleHost for UnifiedRoles {
    fn role_op(&self, op: RoleOp) -> Result<RoleOpOut, BackendErr> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        use RoleOp as R;
        match op {
            // ---- serve 五件 ----
            R::ServeStart => {
                // Q-H F1：op 顺序固定「严格读 → 写回 → 改内存 → 装配」，任一步 Err ⇒
                // 拒绝 + **零副作用**（内存/文件均不变；Go lifecycleStart→Update 同形——
                // `roleops.go:161-176`：Update 失败直接返回、角色不动、期望态不写）。
                // **代码门 M1**：严格读/写回**先于** `already` 短路（Go 的 Update 也在
                // before 判定之前）——「角色在跑 + config 被手编坏」⇒ 拒绝（不再 rc=0）。
                let mut cfg = load_config_strict(&self.state_dir)
                    .map_err(|e| BackendErr::Other(format!("config 非法（拒绝 start，先修复）：{e}")))?;
                write_config_enabled(&self.state_dir, Some(true), None)
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                cfg.serve.enabled = true;
                inner.cfg = cfg;
                if Self::serve_running(&inner) {
                    return Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }));
                }
                // 串行化：等上一轮 stop 的收尾线程（端口/socket 释放净）再装配。
                if let Some(h) = inner.serve_stop_join.take() {
                    let _ = h.join();
                }
                let started = match self.assemble_serve() {
                    Ok((engine, upnp)) => {
                        inner.serve_epoch += 1;
                        let epoch = inner.serve_epoch;
                        inner.serve = Some(Arc::clone(&engine));
                        inner.serve_upnp = upnp;
                        inner.serve_reason.clear();
                        (self.eventf)("serve: 按期望态装配（config serve.enabled=true，经控制面）");
                        Some((engine, epoch))
                    }
                    Err(e) => {
                        inner.serve_reason = e.clone();
                        None
                    }
                };
                match started {
                    Some((engine, epoch)) => {
                        self.spawn_serve_supervisor(engine, epoch);
                        Ok(RoleOpOut::Action(RoleActionResult { action: "started".into() }))
                    }
                    None => {
                        // r2-11 剩余半边（D-1）：失败也 spawn 退避重建（Go StartRole 同款；
                        // bootstrap 自带期望态复核——并发 start 成功后自动让位）。
                        if let Some(roles) = self.self_arc() {
                            std::thread::Builder::new()
                                .name("hw-boot-serve2".to_owned())
                                .stack_size(512 * 1024)
                                .spawn(move || roles.bootstrap_serve())
                                .expect("线程创建不可失败");
                        }
                        Err(BackendErr::Other(
                            "serve 装配失败（状态面 failed；退避重建已启动——稍后 serve status 复查）".into(),
                        ))
                    }
                }
            }
            R::ServeStop => {
                // Q-H F1：同上顺序（Stop 现状同病——先翻内存再写回）；坏 config ⇒
                // 拒绝 + 零副作用（不翻期望态、不停引擎）。
                let mut cfg = load_config_strict(&self.state_dir)
                    .map_err(|e| BackendErr::Other(format!("config 非法（拒绝 stop，先修复）：{e}")))?;
                write_config_enabled(&self.state_dir, Some(false), None)
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                cfg.serve.enabled = false;
                inner.cfg = cfg;
                if let Some(engine) = inner.serve.take() {
                    inner.serve_epoch += 1;
                    inner.serve_reason.clear(); // 停用 = stopped 收面（评审 r2-11：failed 残留 reason 会把停用态误呈 failed）
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
                    inner.serve_reason.clear();
                    Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }))
                }
            }
            R::ServeRestart => {
                if !Self::serve_running(&inner) && inner.serve_reason.is_empty() {
                    return Err(BackendErr::Other("serve 未在运行（先 start）——restart 无重建对象".into()));
                }
                // Q-H F1：restart **不变期望态**（Go `lifecycleRestart` 不调 Update——
                // `roleops.go:194-203`）⇒ 只做严格读（坏 config = 拒绝 + 零副作用）；
                // 装配期失败（合法 config 但绑定失败等）仍走 failed + 退避重建。
                let cfg = load_config_strict(&self.state_dir)
                    .map_err(|e| BackendErr::Other(format!("config 非法（拒绝 restart，先修复）：{e}")))?;
                inner.cfg = cfg;
                // failed 态可 restart（Go RestartRole 同义：跳过剩余退避立即重建——
                // 评审 r2-11：failed 变常态后「先 start」文案不可行动）。
                // 与 stop 同款：旧引擎异步收尾（join 后再装配——串行化防端口退让）。
                if let Some(engine) = inner.serve.take() {
                    inner.serve_epoch += 1;
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
                let restarted = match self.assemble_serve() {
                    Ok((engine, upnp)) => {
                        inner.serve_epoch += 1;
                        let epoch = inner.serve_epoch;
                        inner.serve = Some(Arc::clone(&engine));
                        inner.serve_upnp = upnp;
                        inner.serve_restarts += 1;
                        inner.serve_reason.clear();
                        (self.eventf)(&format!("serve: 显式重启完成（重建计数 {}）", inner.serve_restarts));
                        Some((engine, epoch))
                    }
                    Err(e) => {
                        inner.serve_reason = e.clone();
                        None
                    }
                };
                match restarted {
                    Some((engine, epoch)) => {
                        self.spawn_serve_supervisor(engine, epoch);
                        Ok(RoleOpOut::Action(RoleActionResult { action: "restarted".into() }))
                    }
                    None => {
                        // r2-11 剩余半边（D-1）：与 start 同款——失败 spawn 退避重建。
                        if let Some(roles) = self.self_arc() {
                            std::thread::Builder::new()
                                .name("hw-boot-serve3".to_owned())
                                .stack_size(512 * 1024)
                                .spawn(move || roles.bootstrap_serve())
                                .expect("线程创建不可失败");
                        }
                        Err(BackendErr::Other(
                            "serve 重启装配失败（状态面 failed；退避重建已启动——稍后 serve status 复查）".into(),
                        ))
                    }
                }
            }
            R::ServeStatus => {
                // 出锁查询（评审 r2-13）：role_op 顶部的 inner 守卫先放——引擎查询
                // ≤2s，持锁做会挡住全部角色操作与 daemon.status。
                drop(inner);
                let st = self.serve_status_out_of_lock();
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
                    rpk: tok.rpk.as_ref(),
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
                // Q-H F1 + 代码门 M1：与 serve 同序，且严格读/写回**先于** already 短路。
                let mut cfg = load_config_strict(&self.state_dir)
                    .map_err(|e| BackendErr::Other(format!("config 非法（拒绝 start，先修复）：{e}")))?;
                write_config_enabled(&self.state_dir, None, Some(true))
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                cfg.relay.enabled = true;
                inner.cfg = cfg;
                if Self::relay_running(&inner) {
                    return Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }));
                }
                let started = match self.assemble_relay_role(&inner.cfg) {
                    Ok((proc, token, listen)) => {
                        inner.relay_epoch += 1;
                        let epoch = inner.relay_epoch;
                        let exited = proc.exited_flag();
                        inner.relay = Some(proc);
                        inner.relay_token = token;
                        inner.relay_listen = Some(listen);
                        inner.relay_reason.clear();
                        (self.eventf)("relay: 按期望态装配（config relay.enabled=true，经控制面）");
                        Some((exited, epoch))
                    }
                    Err(e) => {
                        inner.relay_reason = e.clone();
                        None
                    }
                };
                match started {
                    Some((exited, epoch)) => {
                        self.spawn_relay_supervisor(exited, epoch);
                        Ok(RoleOpOut::Action(RoleActionResult { action: "started".into() }))
                    }
                    None => {
                        // r2-11 剩余半边（D-1）：失败 spawn 退避重建（bootstrap_relay
                        // 自带期望态复核——并发 start 成功后自动让位）。
                        if let Some(roles) = self.self_arc() {
                            std::thread::Builder::new()
                                .name("hw-boot-relay2".to_owned())
                                .stack_size(512 * 1024)
                                .spawn(move || roles.bootstrap_relay())
                                .expect("线程创建不可失败");
                        }
                        Err(BackendErr::Other(
                            "relay 装配失败（状态面 failed；退避重建已启动——稍后 relay status 复查）".into(),
                        ))
                    }
                }
            }
            R::RelayStop => {
                // Q-H F1：严格读先于任何副作用（坏 config = 拒绝 + 零副作用）。
                let mut cfg = load_config_strict(&self.state_dir)
                    .map_err(|e| BackendErr::Other(format!("config 非法（拒绝 stop，先修复）：{e}")))?;
                write_config_enabled(&self.state_dir, None, Some(false))
                    .map_err(|e| BackendErr::Other(format!("写 config：{e}")))?;
                cfg.relay.enabled = false;
                inner.cfg = cfg;
                if let Some(proc) = inner.relay.take() {
                    inner.relay_epoch += 1;
                    inner.relay_reason.clear(); // 停用 = stopped 收面（评审 r2-11）
                    proc.stop();
                    (self.eventf)("relay: 期望停用（config relay.enabled=false，经控制面）——已收工");
                    Ok(RoleOpOut::Action(RoleActionResult { action: "stopped".into() }))
                } else {
                    Ok(RoleOpOut::Action(RoleActionResult { action: "already".into() }))
                }
            }
            R::RelayRestart => {
                if !Self::relay_running(&inner) && inner.relay_reason.is_empty() {
                    return Err(BackendErr::Other("relay 未在运行（先 start）——restart 无重建对象".into()));
                }
                // Q-H F1：与 serve 同款——restart 不变期望态，只严格读（拒坏 config）。
                let cfg = load_config_strict(&self.state_dir)
                    .map_err(|e| BackendErr::Other(format!("config 非法（拒绝 restart，先修复）：{e}")))?;
                inner.cfg = cfg;
                // failed 态可 restart（同 serve——评审 r2-11）。
                if let Some(proc) = inner.relay.take() {
                    inner.relay_epoch += 1;
                    proc.stop();
                }
                let restarted = match self.assemble_relay_role(&inner.cfg) {
                    Ok((proc, token, listen)) => {
                        inner.relay_epoch += 1;
                        let epoch = inner.relay_epoch;
                        let exited = proc.exited_flag();
                        inner.relay = Some(proc);
                        inner.relay_token = token;
                        inner.relay_listen = Some(listen);
                        inner.relay_restarts += 1;
                        inner.relay_reason.clear();
                        (self.eventf)(&format!("relay: 显式重启完成（重建计数 {}）", inner.relay_restarts));
                        Some((exited, epoch))
                    }
                    Err(e) => {
                        inner.relay_reason = e.clone();
                        None
                    }
                };
                match restarted {
                    Some((exited, epoch)) => {
                        self.spawn_relay_supervisor(exited, epoch);
                        Ok(RoleOpOut::Action(RoleActionResult { action: "restarted".into() }))
                    }
                    None => {
                        // r2-11 剩余半边（D-1）：与 start 同款——失败 spawn 退避重建。
                        if let Some(roles) = self.self_arc() {
                            std::thread::Builder::new()
                                .name("hw-boot-relay3".to_owned())
                                .stack_size(512 * 1024)
                                .spawn(move || roles.bootstrap_relay())
                                .expect("线程创建不可失败");
                        }
                        Err(BackendErr::Other(
                            "relay 重启装配失败（状态面 failed；退避重建已启动——稍后 relay status 复查）".into(),
                        ))
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
                state: role_state(Self::serve_running(&inner), &inner.serve_reason).into(),
                restarts: inner.serve_restarts,
                reason: (!inner.serve_reason.is_empty()).then(|| inner.serve_reason.clone()),
            },
            RoleBrief {
                name: "relay".into(),
                state: role_state(Self::relay_running(&inner), &inner.relay_reason).into(),
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

    // ③ 读 config（fail-fast：非法 = 可行动错误**拒启**——Go `nodeconfig.Load` 同形，
    //    发生在任何角色/control 装配之前；Q-H F1 起 = 唯一严格表 + 全量值域，
    //    含 [relay] 段整文件校验）
    let cfg = match load_config_strict(&state_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("homeway: config 非法——拒绝启动（先修复再启动）：{e}");
            std::process::exit(1);
        }
    };

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
        me: Mutex::new(None),
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
            serve_epoch: 0,
            relay_epoch: 0,
            relay_token: None,
            relay_listen: None,
            serve_restarts: 0,
            relay_restarts: 0,
            serve_reason: String::new(),
            relay_reason: String::new(),
            serve_stop_join: None,
        }),
    });

    roles.set_self_ref(Arc::downgrade(&roles));

    // ⑥ client 角色恒开（hosts.json 表 + 每主机常驻会话；事件总线进程级唯一；
    // 承载面随表同生命周期——forwards.json/socks.json 重建监听 + speedtest 运行面）。
    let client_dir = state_dir.join("client");
    let core = match DaemonCore::new(
        crate::cli_version(),
        &client_dir,
        &client_dir.join("identity"),
        &cache_dir.join("endpoints"),
        Arc::clone(&daemon_dlogf),
        Arc::clone(&daemon_logf),
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
    // r1-M4：装配失败 ≠ 进程退出——状态面 failed + supervisor 退避重试（Go 装配错误
    // 经 runRoleLoop 同表退避；统一进程 client/control 照常）。
    let mut serve_watch: Option<(Arc<ServeEngine>, u64)> = None;
    let mut serve_bootstrap = false;
    let mut relay_watch: Option<(Arc<std::sync::atomic::AtomicBool>, u64)> = None;
    let mut relay_bootstrap = false;
    {
        let mut inner = roles.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.cfg.serve.enabled {
            match roles.assemble_serve() {
                Ok((engine, upnp)) => {
                    inner.serve_epoch += 1;
                    inner.serve = Some(Arc::clone(&engine));
                    inner.serve_upnp = upnp;
                    serve_watch = Some((engine, inner.serve_epoch));
                    logf("serve: 按期望态装配（config serve.enabled=true）");
                }
                Err(e) => {
                    inner.serve_reason = e.clone();
                    serve_bootstrap = true;
                    daemon_logf(&format!(
                        "serve: 装配失败（{e}）——状态面 failed，退避后进程内重建（supervisor）"
                    ));
                }
            }
        } else {
            logf("serve: 期望停用（config serve.enabled=false）——不装配");
        }
        if inner.cfg.relay.enabled {
            match roles.assemble_relay_role(&inner.cfg) {
                Ok((proc, token, listen)) => {
                    inner.relay_epoch += 1;
                    let exited = proc.exited_flag();
                    inner.relay = Some(proc);
                    inner.relay_token = token;
                    inner.relay_listen = Some(listen);
                    relay_watch = Some((exited, inner.relay_epoch));
                    logf("relay: 按期望态装配（config relay.enabled=true）");
                }
                Err(e) => {
                    inner.relay_reason = e.clone();
                    relay_bootstrap = true;
                    daemon_logf(&format!(
                        "relay: 装配失败（{e}）——状态面 failed，退避后进程内重建（supervisor）"
                    ));
                }
            }
        } else {
            logf("relay: 期望停用（config relay.enabled=false）——不装配");
        }
    }
    if let Some((engine, epoch)) = serve_watch {
        roles.spawn_serve_supervisor(engine, epoch);
    }
    if serve_bootstrap {
        let roles2 = roles.self_arc().expect("自引用已注入");
        std::thread::Builder::new()
            .name("hw-boot-serve".to_owned())
            .stack_size(512 * 1024)
            .spawn(move || roles2.bootstrap_serve())
            .expect("线程创建不可失败");
    }
    if let Some((exited, epoch)) = relay_watch {
        roles.spawn_relay_supervisor(exited, epoch);
    }
    if relay_bootstrap {
        let roles2 = roles.self_arc().expect("自引用已注入");
        std::thread::Builder::new()
            .name("hw-boot-relay".to_owned())
            .stack_size(512 * 1024)
            .spawn(move || roles2.bootstrap_relay())
            .expect("线程创建不可失败");
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
        // Q-H F5：控制面资源收口（连接上限 64 + 握手期限 10s；Rust 形态加固，
        // Go 无对应约束——登记见 INTEROP-CRITERIA 判据变更记录）。
        max_conns: homeway_core::daemon::server::DEFAULT_MAX_CONTROL_CONNS,
        handshake_deadline: homeway_core::daemon::server::DEFAULT_HANDSHAKE_DEADLINE,
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
    match crate::serve_cli::wait_stop_pipe() {
        crate::serve_cli::StopWait::Signaled => {}
        crate::serve_cli::StopWait::PipeErr(e) => {
            eprintln!("homeway: {e}——按收到停止处理（收工）")
        }
    }
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
    use super::*;

    /// --state=DIR 与 --state DIR 等价（r1-H2 回归钉）——Q-H F2 起走 `cli_flags`
    /// 唯一取值器（解析逻辑抽为 `parse_unified_args` 可测）。
    #[test]
    fn state_flag_forms_equivalent() {
        for (args, want) in [
            (vec!["--state", "/tmp/a"], "/tmp/a"),
            (vec!["--state=/tmp/b"], "/tmp/b"),
            (vec!["-state", "/tmp/c"], "/tmp/c"),
            (vec!["--verbose", "--state=/tmp/d"], "/tmp/d"),
        ] {
            let args: Vec<String> = args.iter().map(|s| s.to_string()).collect();
            match parse_unified_args(&args) {
                UnifiedInvocation::Run { state_dir, .. } => {
                    assert_eq!(state_dir, PathBuf::from(want), "args={args:?}")
                }
                UnifiedInvocation::Help => panic!("不应是 help：{args:?}"),
            }
        }
    }

    /// F7b：`--verbose=false` 真生效（此前显式值被忽略、恒 true）。
    #[test]
    fn verbose_false_takes_effect() {
        match parse_unified_args(&["--verbose=false".to_owned()]) {
            UnifiedInvocation::Run { verbose, .. } => assert!(!verbose),
            UnifiedInvocation::Help => panic!("不应是 help"),
        }
    }

    /// F1：op 顺序 =「严格读 → 写回 → 改内存 → 装配」——坏 config 时 **零副作用**
    /// （文件逐字节不变、内存 enabled 不变、状态面 stopped）。用临时 state 直接
    /// 构造 UnifiedRoles（控制面 handler 的宿主），不装配任何角色。
    #[test]
    fn role_op_rejects_bad_config_with_zero_side_effects() {
        let dir = std::env::temp_dir().join(format!(
            "hw-unified-op-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let good = "[serve]\nenabled = false\nlisten = 41641\n[relay]\nenabled = false\n";
        std::fs::write(dir.join("config.toml"), good).unwrap();
        let cfg = load_config_strict(&dir).unwrap();
        let roles = Arc::new(UnifiedRoles {
            state_dir: dir.clone(),
            me: Mutex::new(None),
            logf: Arc::new(|_| {}),
            eventf: Arc::new(|_| {}),
            dlogf: Arc::new(|_| {}),
            tokf: Arc::new(|_| {}),
            log_paths: None,
            inner: Mutex::new(RolesInner {
                cfg,
                serve: None,
                serve_upnp: false,
                relay: None,
                serve_epoch: 0,
                relay_epoch: 0,
                relay_token: None,
                relay_listen: None,
                serve_restarts: 0,
                relay_restarts: 0,
                serve_reason: String::new(),
                relay_reason: String::new(),
                serve_stop_join: None,
            }),
        });
        // 注入坏 config（内存 = 上一份好值）。
        let bad = "[serve]\nenabled = false\npeer_ttl = \"abc\"\n";
        std::fs::write(dir.join("config.toml"), bad).unwrap();
        let e = RoleHost::role_op(&*roles, RoleOp::ServeStart).unwrap_err();
        assert!(format!("{e}").contains("peer_ttl"), "{e}");
        assert!(format!("{e}").contains("config.toml"), "{e}");
        // 文件未变（零副作用）。
        assert_eq!(std::fs::read_to_string(dir.join("config.toml")).unwrap(), bad);
        // 内存未变 + 状态面 stopped（不谎报 enabled）。
        let inner = roles.inner.lock().unwrap();
        assert!(!inner.cfg.serve.enabled);
        assert!(inner.serve.is_none());
        drop(inner);
        let st = roles.statuses();
        let srv = st.iter().find(|r| r.name == "serve").unwrap();
        assert_eq!(srv.state, "stopped");
        let _ = std::fs::remove_dir_all(&dir);
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
