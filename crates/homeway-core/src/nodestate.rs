//! 三层 state 布局与单实例锁（B0-1 部署最小面；语义真源 `baseline:internal/nodestate/`）。
//!
//! 布局树（NS「三层状态分离」）：
//! ```text
//! <state>/
//! ├── config.toml   L1 意图（唯一人写文件；缺失时生成默认——生成后绝不重写）
//! ├── serve/        L2 serve 不变量（key/tokens/revoked——engine 侧自管）
//! ├── relay/        L2 relay 不变量（relay.key）
//! ├── client/       L2 client 不变量（identity/hosts.json——B0-2 接，目录先建）
//! ├── cache/        L3 可弃（events.log / listen_port.txt / public_endpoint.txt）
//! └── lock          单实例锁（flock——进程死亡内核自动释放，无 stale 文件问题）
//! ```
//!
//! 单实例锁（Go `AcquireInstanceLock` 同义）：`<state>/lock` 的 flock(LOCK_EX|LOCK_NB)；
//! 取得后写 `pid=<n>\nrole=homeway\nform=<形态>`；拿不到读出持有者报错退出（统一
//! 进程与 serve/relay 前台单角色共用——同 state 双进程在任何形态组合下互斥，防
//! launchd 双起互抢 UDP）。
//!
//! events 面与 debug 面（Go `nodestate.Eventf` + `server/logging.go` 的文件侧）：
//! `cache/events.log`（摘要，2MB×3 轮转）+ `cache/debug.log`（细节，8MB×2 轮转）——
//! 统一进程的 serve 角色与前台 `serve` 共用这对文件（Go 同一 `cache/` 落点，两写者
//! 不同时在世）。时间前缀与 Go `logTimePrefix` 同形（本地时区）。

use std::fs::{File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt as _, FileExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

use crate::logfile::{RotatingWriter, DEBUG_BACKUPS, DEBUG_MAX_BYTES, EVENTS_BACKUPS, EVENTS_MAX_BYTES};

/// 单实例锁错误（文案即契约——Go 同串）。
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    /// 同 state 已有进程持锁（pid/形态从锁文件读出；读不到给 "?" 形态兜底）。
    #[error("homeway 已在运行（pid {pid}，形态 {form}，state={state}）——拒绝二次启动")]
    Held { pid: i32, form: String, state: String },
    /// 建/开/锁底层失败。
    #[error("单实例锁：{0}")]
    Io(#[from] std::io::Error),
}

/// 一个 state 目录的单实例锁句柄（Drop 释放）。
#[derive(Debug)]
pub struct InstanceLock {
    f: Option<File>,
    #[allow(dead_code)]
    path: PathBuf,
}

impl InstanceLock {
    /// 取 `<state>/lock` 排他非阻塞锁；成功写 pid + 归一角色（homeway）+ 形态
    /// （unified/serve/relay），失败读持有者报 `Held`。
    pub fn acquire(state_dir: &Path, form: &str) -> Result<Self, LockError> {
        // state 根收紧 0700（Go MkdirAll(dir, 0o700)；chmod 失败只告警不阻断——
        // r1-W2：create_dir_all 的默认权限不该留一个 0755 的 state 根）。
        // Q-G F4/A4：`DirBuilder::mode` 创建即收紧 + 保留告警（umask 掩码仍可能
        // 压掉 mode ⇒ 后置 chmod 仍需执行）。
        if let Err(e) = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state_dir)
            .and_then(|_| std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700)))
        {
            eprintln!("homeway: ⚠️ state 目录 {} 建立/收紧 0700 失败（{e}）——建议手工 chmod", state_dir.display());
        }
        let path = state_dir.join("lock");
        #[allow(clippy::suspicious_open_options)] // 读写打开持锁文件：不清内容（截断在锁内做）
        let mut f = OpenOptions::new().create(true).read(true).write(true).mode(0o600).open(&path)?;
        let fd = std::os::unix::io::AsRawFd::as_raw_fd(&f);
        let r = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
        if r != 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::WouldBlock {
                let (pid, held_form) = read_lock_holder(&mut f);
                let form = if held_form.is_empty() || held_form == "?" {
                    "未知（旧格式锁或读不到）".to_owned()
                } else {
                    held_form
                };
                return Err(LockError::Held {
                    pid,
                    form,
                    state: state_dir.display().to_string(),
                });
            }
            return Err(LockError::Io(e));
        }
        // 写持有者信息（截断重写：崩溃残留的旧内容不该存活）
        let _ = f.set_len(0);
        let _ = f.write_all_at(
            format!("pid={}\nrole=homeway\nform={form}\n", std::process::id()).as_bytes(),
            0,
        );
        Ok(Self { f: Some(f), path })
    }

    /// 主动释放（幂等；Drop 同义）。
    pub fn release(mut self) {
        self.f = None;
    }
}

impl Drop for InstanceLock {
    fn drop(&mut self) {
        if let Some(f) = self.f.take() {
            let fd = std::os::unix::io::AsRawFd::as_raw_fd(&f);
            unsafe { libc::flock(fd, libc::LOCK_UN) };
        }
    }
}

/// 只读锁试探（**不重写持有者信息**、不持有锁）：探测 `<state>/lock` 是否被持有，
/// 被持有时读出 (pid, 形态)。CLI 的「守护在跑判定/锁僵死归因」共用面
///（D-1 补-3 收敛：此前 daemon_cli 侧有一份逐字段重复实现）。
pub fn probe_lock_holder(state_dir: &Path) -> (bool, i32, String) {
    let Ok(f) = File::open(state_dir.join("lock")) else { return (false, 0, String::new()) };
    let fd = std::os::unix::io::AsRawFd::as_raw_fd(&f);
    let r = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    if r == 0 {
        unsafe { libc::flock(fd, libc::LOCK_UN) };
        return (false, 0, String::new());
    }
    let mut f = f;
    let (pid, form) = read_lock_holder(&mut f);
    (true, pid, form)
}

/// 持有者 pid/形态（读不到 = 0 / "?"）。
fn read_lock_holder(f: &mut File) -> (i32, String) {
    use std::os::unix::fs::FileExt as _;
    let mut buf = [0u8; 128];
    let n = f.read_at(&mut buf, 0).unwrap_or(0);
    let mut pid = 0;
    let mut form = "?".to_owned();
    for line in String::from_utf8_lossy(&buf[..n]).lines() {
        if let Some(v) = line.strip_prefix("pid=") {
            pid = v.trim().parse().unwrap_or(0);
        } else if let Some(v) = line.strip_prefix("form=") {
            form = v.trim().to_owned();
        } else if let Some(v) = line.strip_prefix("role=") {
            // 旧格式（role 归一为 homeway）——形态未知（Go readLockHolder 同义）
            if form == "?" {
                form = format!("legacy:{}", v.trim());
            }
        }
    }
    (pid, form)
}

/// 摘要级日志：`cache/events.log`（2MB×3 轮转）+ 终端回显。时间前缀 = Go
/// `logTimePrefix` 同形（`2006-01-02 15:04:05.000 [homeway] `）。
/// ⚠️ 前缀单标签说明：Go serve 侧行带 `[homewayd]`（旧守护二进制名的历史遗留）、
/// nodestate/daemon 面带 `[homeway]`——Rust 有意收拢为单一 `[homeway]`（判据行
/// 检索按内容 grep，双前缀无诊断价值、徒增形态分裂；GAP-AUDIT 措辞差异类）。
pub struct EventsLog {
    w: Option<RotatingWriter>,
    path: PathBuf,
}

impl EventsLog {
    pub fn open(cache_dir: &Path) -> std::io::Result<Self> {
        Self::open_named(cache_dir, "events.log")
    }

    /// 自定义文件名（daemon 侧 daemon-events.log 防撞落名——B0-2b：serve 角色独占
    /// events.log/debug.log，守护侧自有日志走同目录不同名，两侧写者不同文件）。
    pub fn open_named(cache_dir: &Path, name: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(cache_dir)?;
        let path = cache_dir.join(name);
        let w = RotatingWriter::open(cache_dir, name, EVENTS_MAX_BYTES, EVENTS_BACKUPS)?;
        Ok(Self { w: Some(w), path })
    }

    /// 无文件句柄形态（打开失败时的告警降级——只回显终端）。
    pub fn terminal_only(path: PathBuf) -> Self {
        Self { w: None, path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 摘要行（events.log + 终端）。engine 侧 logf tee 也走它（判据行进摘要文件的
    /// Go 形态）。多线程安全（engine 的观测线程/驱动线程都持有 tee 闭包）。
    pub fn eventf(&self, line: &str) {
        let ts = crate::go_fmt::now_log_prefix();
        let full = format!("{ts}[homeway] {line}");
        println!("{full}");
        if let Some(w) = &self.w {
            w.write_line(&full);
        }
    }

    /// 摘要行只进文件、不回显终端（`echo` = `--verbose` 时回显）——Go `server.logf`
    /// 形态；token 端点变化轮专用（终端只出首轮 token，2026-09-21 用户口径）。
    pub fn quietf(&self, line: &str, echo: bool) {
        let ts = crate::go_fmt::now_log_prefix();
        let full = format!("{ts}[homeway] {line}");
        if echo {
            println!("{full}");
        }
        if let Some(w) = &self.w {
            w.write_line(&full);
        }
    }
}

/// 细节级日志：`cache/debug.log`（8MB×2 轮转；Go `internal/server/logging.go` 的
/// dlogf 文件侧）。`--verbose` 时回显终端（显式调试模式），否则只落盘——生产形态
/// 判据行（`peer: +/-`、`dns: q=`、`intercept: …（dialok）`）由这里持久化。
pub struct DebugLog {
    w: Option<RotatingWriter>,
    path: PathBuf,
}

impl DebugLog {
    pub fn open(cache_dir: &Path) -> std::io::Result<Self> {
        Self::open_named(cache_dir, "debug.log")
    }

    /// 自定义文件名（daemon 侧 daemon-debug.log；同 EventsLog::open_named 的防撞面）。
    pub fn open_named(cache_dir: &Path, name: &str) -> std::io::Result<Self> {
        std::fs::create_dir_all(cache_dir)?;
        let path = cache_dir.join(name);
        let w = RotatingWriter::open(cache_dir, name, DEBUG_MAX_BYTES, DEBUG_BACKUPS)?;
        Ok(Self { w: Some(w), path })
    }

    /// 无文件句柄形态（打开失败时的告警降级——细节行丢弃，服务继续）。
    pub fn disabled(path: PathBuf) -> Self {
        Self { w: None, path }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 细节行（debug.log 恒写；`echo` = `--verbose` 时同时回显终端）。
    pub fn dlogf(&self, line: &str, echo: bool) {
        let ts = crate::go_fmt::now_log_prefix();
        let full = format!("{ts}[homeway] {line}");
        if echo {
            println!("{full}");
        }
        if let Some(w) = &self.w {
            w.write_line(&full);
        }
    }
}

/// config.toml 缺失时的默认模板（Go `nodeconfig.Default()` + 头注释同口径；生成后
/// 绝不重写——手编意图不被覆盖）。键表与 serve_cli/relay_cli 的 serde schema 一致。
pub const DEFAULT_CONFIG_TOML: &str = r#"# homeway 配置（L1 意图层，唯一人写文件；0600）
# 覆盖序 = flag > 本文件 > 内置默认（flag 一次性覆盖、不写回）；省略键 = 默认。
# 身份密钥不进本文件（key 与 token 台账同居 <state>/serve|relay/，L2）。
# 改动经重启生效（不热更）。
# 键表（省略即默认）：
#   [serve] enabled / listen(1-65535) / quic(true|false；M1 的 QUIC 面总开关：关 =
#           不监听 QUIC 端口 + token 不公布 QUIC 端点/RPK ⇒ 该 token 的客户端回落 WG) /
#           quic_listen(1-65535；缺省 = listen+1) / bind_interface(auto|none|网卡|IP) /
#           upnp / stun / stun6 / relay(rl1… 或 IP:port) / max_peers /
#           peer_ttl(时长串，"0s"=关) / dns_port(0=关；非 0 = 客户端解析腿端口，缺省 5300) /
#           files_root(空=$HOME) / public_endpoint(逗号分隔 ip:port；空=推断) /
#           dns_upstream(ip[:port] 列表；空=跟随 /etc/resolv.conf；显式覆盖 = opt-in 偏离 spec MUST) /
#           dns_fallback(ip[:port]；缺省 223.5.5.5；空串非法) /
#           ddns_resolver / dns_probe_target / stun_probe_target(IPv4 列表；空=默认常量表；
#           dns_probe_target 一键喂挑卡/健康探针/udpcap 三路；stun_probe_target 须带端口)
#   [[serve.ddns]] domain = "裸域名"（可多条；出口只读解析，记录由外部 DDNS 维护）
#   [serve.tx_shape] 发送整形（rate_mbps/burst_kb；缺省 = 产品默认 200MiB/s+256KiB，HOMEWAY_TX_SHAPING=off 整套关）
#   [relay] enabled / listen(":41741") / advertise(逗号分隔，空=自动探测)
# 客户端角色无配置节（随进程常开）；host 表在 <state>/client/hosts.json（不进 config）。

[serve]
enabled = true
listen = 41641
quic = true
bind_interface = "auto"
upnp = true
stun = "stun.cloudflare.com:3478"
stun6 = "stun.cloudflare.com:3478"
relay = ""
max_peers = 32
peer_ttl = "168h0m0s"
public_endpoint = ""
dns_port = 5300
files_root = ""

[relay]
enabled = false
listen = ":41741"
advertise = ""
"#;

/// 打开（或初始化）三层 state 布局（Go `OpenNodeState` 裁剪面：无旧布局迁移——
/// Rust 版无历史 state）。0700 收紧 + 四子目录 + config 缺失生成默认 + events.log
/// + debug.log（两个都带轮转——P1-1；打开失败各自告警降级，不挡启动）。
pub struct NodeState {
    pub events: EventsLog,
    pub debug: DebugLog,
    pub config_generated: bool,
}

pub fn open_node_state(dir: &Path) -> std::io::Result<NodeState> {
    // Q-G F4/A5：`DirBuilder::mode(0o700)` 创建即收紧（create_dir_all 的默认权限
    // 会留一个 0755 的 state 根）——**仍随后无条件 chmod 归一**（mkdir 同样受
    // umask 掩码）+ 失败告警（保留既有告警形态）。
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
        eprintln!("homeway: ⚠️ state 目录 {} 收紧 0700 失败（{e}）——建议手工 chmod", dir.display());
    }
    for sub in ["serve", "relay", "client", "cache"] {
        let p = dir.join(sub);
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&p)?;
        if let Err(e) = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o700)) {
            eprintln!("homeway: ⚠️ state 子目录 {} 收紧 0700 失败（{e}）——建议手工 chmod", p.display());
        }
    }
    let cfg_path = dir.join("config.toml");
    let mut generated = false;
    if !cfg_path.exists() {
        // 原子写（tmp+rename——中断不留半文件）。Q-G F4/A6：**创建即 0600** +
        // handle 后 fchmod 归一（旧形态默认权限建 → 静默 chmod，chmod 失败无告警）。
        use std::io::Write as _;
        let tmp = dir.join(".config.toml.tmp");
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        // 失败**告警不阻断**（代码门②：配置模板不含密钥，不得为权限归一牺牲进程可
        // 启动性；与 A1–A5 的告警形态一致）
        if let Err(e) = f.set_permissions(std::fs::Permissions::from_mode(0o600)) {
            eprintln!("homeway: ⚠️ config 模板 {} 收紧 0600 失败（{e}）——建议手工 chmod", tmp.display());
        }
        f.write_all(DEFAULT_CONFIG_TOML.as_bytes())?;
        drop(f);
        std::fs::rename(&tmp, &cfg_path)?;
        generated = true;
    }
    // 摘要/细节双文件日志（打开失败只告警——日志不该挡启动；Go initLogs 同义）
    let events = match EventsLog::open(&dir.join("cache")) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("homeway: ⚠️ events.log 打开失败（{e}）——本轮公告只回显终端");
            EventsLog::terminal_only(dir.join("cache").join("events.log"))
        }
    };
    let debug = match DebugLog::open(&dir.join("cache")) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("homeway: ⚠️ debug.log 打开失败（{e}）——本轮细节日志缺失，服务继续");
            DebugLog::disabled(dir.join("cache").join("debug.log"))
        }
    };
    Ok(NodeState { events, debug, config_generated: generated })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("homeway-nodestate-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// 三层布局 + config 生成一次（存在即绝不重写）。
    #[test]
    fn layout_and_config_generation() {
        let d = tmpdir("layout");
        let ns = open_node_state(&d).unwrap();
        assert!(ns.config_generated);
        for sub in ["serve", "relay", "client", "cache"] {
            assert!(d.join(sub).is_dir(), "{sub} 应建");
        }
        assert!(d.join("config.toml").exists());
        // 手改后重开：不再生成
        std::fs::write(d.join("config.toml"), "[serve]\nenabled = false\n").unwrap();
        let ns2 = open_node_state(&d).unwrap();
        assert!(!ns2.config_generated);
        let body = std::fs::read_to_string(d.join("config.toml")).unwrap();
        assert_eq!(body, "[serve]\nenabled = false\n", "手编意图不被覆盖");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 单实例锁：第二实例拒起（读出持有 pid/form）；释放后可再取。
    #[test]
    fn instance_lock_mutual_exclusion() {
        let d = tmpdir("lock");
        std::fs::create_dir_all(&d).unwrap();
        let l1 = InstanceLock::acquire(&d, "unified").unwrap();
        let e = InstanceLock::acquire(&d, "serve").unwrap_err();
        match e {
            LockError::Held { pid, form, .. } => {
                assert_eq!(pid, std::process::id() as i32);
                assert_eq!(form, "unified");
            }
            other => panic!("应为 Held：{other}"),
        }
        let msg = InstanceLock::acquire(&d, "serve").unwrap_err().to_string();
        assert!(msg.starts_with("homeway 已在运行（pid "), "{msg}");
        assert!(msg.contains("形态 unified"), "{msg}");
        l1.release();
        let _l2 = InstanceLock::acquire(&d, "serve").unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 双文件日志面（P1-1）：open_node_state 建 events.log + debug.log；quietf 只进
    /// 文件（token 端点变化轮流——Go tokenToFile 形态）；dlogf 恒进 debug.log。
    /// （stdout 回显面单测不可捕，由真机/统一进程验收覆盖。）
    #[test]
    fn dual_log_files_and_quietf() {
        let d = tmpdir("logs");
        let ns = open_node_state(&d).unwrap();
        assert!(d.join("cache").join("events.log").exists(), "events.log 应建");
        assert!(d.join("cache").join("debug.log").exists(), "debug.log 应建");
        ns.events.quietf("客户端 token（端点已变化…）：hmw1TEST", false);
        ns.debug.dlogf("peer: + dev=… n=1/32", false);
        let ev = std::fs::read_to_string(d.join("cache").join("events.log")).unwrap();
        assert!(ev.contains("端点已变化"), "quietf 落 events.log");
        assert!(ev.contains("[homeway] "), "时间前缀形态");
        let dbg = std::fs::read_to_string(d.join("cache").join("debug.log")).unwrap();
        assert!(dbg.contains("peer: + dev=…"), "dlogf 落 debug.log");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// 默认 config 模板可被 serve_cli/relay_cli 的 serde schema 解析（键表一致性）。
    #[test]
    fn default_config_parses() {        #[derive(serde::Deserialize, Default)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct FileServe {
            #[serde(default)] enabled: bool,
            #[serde(default)] listen: Option<u16>,
            // M1：QUIC 面（总开关 + 独立端口）——模板与 schema 漂移由本镜像捕获
            #[serde(default)] quic: Option<bool>,
            #[serde(default)] quic_listen: Option<u16>,
            #[serde(default)] bind_interface: Option<String>,
            #[serde(default)] upnp: Option<bool>,
            #[serde(default)] stun: Option<String>,
            #[serde(default)] stun6: Option<String>,
            #[serde(default)] relay: Option<String>,
            #[serde(default)] max_peers: Option<usize>,
            #[serde(default)] peer_ttl: Option<String>,
            #[serde(default)] public_endpoint: Option<String>,
            #[serde(default)] dns_port: Option<u16>,
            #[serde(default)] files_root: Option<String>,
            #[serde(default)] ddns: Option<Vec<FileDdns>>,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct FileDdns { #[serde(default)] domain: String }
        #[derive(serde::Deserialize, Default)]
        #[serde(deny_unknown_fields)]
        #[allow(dead_code)]
        struct FileRelay {
            #[serde(default)] enabled: bool,
            #[serde(default)] listen: Option<String>,
            #[serde(default)] advertise: Option<String>,
        }
        #[derive(serde::Deserialize, Default)]
        #[serde(deny_unknown_fields)]
        struct FileConfig {
            #[serde(default)] serve: FileServe,
            #[serde(default)] relay: FileRelay,
        }
        let fc: FileConfig = toml::from_str(DEFAULT_CONFIG_TOML).expect("默认模板必须可解析");
        assert!(fc.serve.enabled);
        assert_eq!(fc.serve.listen, Some(41641));
        assert_eq!(fc.serve.stun6.as_deref(), Some("stun.cloudflare.com:3478"));
        assert!(!fc.relay.enabled);
        assert_eq!(fc.relay.listen.as_deref(), Some(":41741"));
    }
}
