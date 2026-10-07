//! `homeway-cli relay` 命令面（R4-4a4；语义真源 `cmd/homeway` 的 relay 子命令 +
//! `internal/relay/{role,cli}.go`——裁剪面：无 daemon 期望态/instance lock/status 面）。
//!
//! 前台单角色：不改期望态，flag 为一次性覆盖（覆盖序 flag > config.toml [relay] 节 >
//! 内置默认）。`--state` 即统一 state 根：relay.key = `<state>/relay/`、
//! relay.log = `<state>/cache/`。

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::Arc;
use homeway_core::relay::logfile::RelayLog;
use homeway_core::relay::rltoken;
use homeway_core::relay::{Config, Relay};

/// config.toml 的 [relay] 节（与 serve_cli 同 schema；deny_unknown 同口径）。
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileRelay {
    #[serde(default)]
    #[allow(dead_code)]
    enabled: bool,
    #[serde(default)]
    listen: Option<String>,
    #[serde(default)]
    advertise: Option<String>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileConfig {
    #[serde(default)]
    _serve: FileServeIgnore,
    #[serde(default)]
    relay: FileRelay,
}

/// serve 节整节忽略（本命令只读 [relay]；serve 的键表由 serve 命令自己校验）。
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileServeIgnore {
    #[serde(default)]
    #[serde(rename = "*")]
    _rest: (),
}

struct RelayFlags {
    state: Option<PathBuf>,
    listen: Option<String>,
    advertise: Option<String>,
    open: bool,
    no_hints: bool,
    extra: Vec<String>,
}

fn parse_flags(args: &[String]) -> RelayFlags {
    let mut f = RelayFlags { state: None, listen: None, advertise: None, open: false, no_hints: false, extra: Vec::new() };
    let mut i = 0;
    while i < args.len() {
        let stripped = args[i].strip_prefix("--").or_else(|| args[i].strip_prefix('-'));
        let Some(body) = stripped else {
            f.extra.push(args[i].clone());
            i += 1;
            continue;
        };
        let (name, inline) = match body.split_once('=') {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (body.to_owned(), None),
        };
        let take_val = |i: &mut usize| -> Option<String> {
            if let Some(v) = inline.clone() {
                return Some(v);
            }
            *i += 1;
            args.get(*i).cloned()
        };
        let mut j = i;
        match name.as_str() {
            "state" => f.state = take_val(&mut j).map(PathBuf::from),
            "listen" => f.listen = take_val(&mut j),
            "advertise" => f.advertise = take_val(&mut j),
            // 测试形态开关（不进生产命令面文档——FIX-89 同款「显式开放」哲学）
            "open" => f.open = true,
            // 测试形态：不递送 hint（升级条纹的中继驻留前提；见 Config::no_hints 注释）
            "no-hints" => f.no_hints = true,
            other => {
                eprintln!("未知参数：--{other}");
                std::process::exit(2);
            }
        }
        i = j + 1;
    }
    f
}

/// 解析监听地址（":41741"/"127.0.0.1:41741"——缺 host = 全卡 v4）。
pub fn parse_listen(v: &str) -> Option<SocketAddr> {
    let v = v.trim();
    if let Some(port) = v.strip_prefix(':') {
        let p: u16 = port.parse().ok()?;
        return Some(SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), p));
    }
    v.parse().ok()
}

/// relay 角色装配产物（统一进程与前台单角色共用）：停止位 fd + 运行线程句柄。
pub struct RelayProc {
    stop_w: i32,
    join: Option<std::thread::JoinHandle<()>>,
    /// run 线程在世位（线程出口清零——panic 也清；统一进程 supervisor 的运行期
    /// 失败判据）。
    exited: Arc<std::sync::atomic::AtomicBool>,
}

impl RelayProc {
    /// 停止并收线程（确定性 closeAll——Ctrl-C/SIGTERM 的统一收口）。
    pub fn stop(mut self) {
        unsafe { libc::write(self.stop_w, b"x".as_ptr().cast(), 1) };
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
        unsafe { libc::close(self.stop_w) };
    }

    /// 在世位的共享句柄（supervisor 看护用——proc 本体留在宿主表内被 stop 消费；
    /// false = 在跑，true = run 线程已退〔正常 stop 或失败〕）。
    pub fn exited_flag(&self) -> Arc<std::sync::atomic::AtomicBool> {
        Arc::clone(&self.exited)
    }

    /// run 线程是否已退出（false = 在跑；前台等待循环与 supervisor 同判据）。
    pub fn exited(&self) -> bool {
        self.exited.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for RelayProc {
    /// 兜底收口（评审 r2-E：无 Drop 时宿主覆盖/丢弃表项 = stop 管道 fd 泄漏；
    /// 显式 stop() 先走、Drop 幂等——write 对已关 fd 只 EBADF）。
    fn drop(&mut self) {
        unsafe {
            libc::write(self.stop_w, b"x".as_ptr().cast(), 1);
            if let Some(h) = self.join.take() {
                let _ = h.join();
            }
            libc::close(self.stop_w);
        }
    }
}

/// relay 角色装配参数（flag 覆盖后的终值；统一进程 = 纯 config 值）。
pub struct RelayAssemble {
    pub listen: SocketAddr,
    pub advertise: String,
    pub no_hints: bool,
    pub open: bool,
}

/// 装配 relay 角色（key 加载/token 铸出/run 线程起跑——on_ready 打中继 token）。
/// `logf` = 摘要行出口（前台 = println；统一进程 = events tee）。
pub fn assemble_relay(
    state_dir: &std::path::Path,
    a: RelayAssemble,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<RelayProc, String> {
    let relay_dir = state_dir.join("relay");
    let cache_dir = state_dir.join("cache");
    std::fs::create_dir_all(&relay_dir)
        .and_then(|_| std::fs::create_dir_all(&cache_dir))
        .map_err(|e| format!("state 目录建不起来（{}）：{e}", state_dir.display()))?;

    // 两级日志（终端只出 token 与端点变化；全量进 cache/relay.log）
    let log2 = Arc::new(RelayLog::open(&cache_dir, Arc::clone(&logf)));
    let log2_run = Arc::clone(&log2); // run 线程的失败行出口（log2 本体随后移入 on_ready）
    let (secret, created) =
        rltoken::load_or_create_secret(&relay_dir).map_err(|e| format!("relay.key 管理失败（{}）：{e}", relay_dir.display()))?;
    if created {
        log2.logf(&format!(
            "已生成中继鉴权密钥（{}/relay.key，0600）—— 重启不变，token 因此稳定",
            relay_dir.display()
        ));
    }
    if let Some(p) = log2.log_path() {
        log2.ulogf(&format!("日志：{p} —— 终端只出 token 与端点变化"));
    }
    let auth = if a.open { None } else { Some(secret) };
    let mut cfg = Config::new(a.listen, auth, log2.logf_fn());
    cfg.no_hints = a.no_hints;
    // F9：探针应答上报真实构建（此前 `Config.build` 全仓无赋值 ⇒ 恒 "relay-dev"）
    cfg.build = homeway_core::BUILD_STR.to_owned();
    let relay = Relay::new(cfg);
    let adv = a.advertise;
    let on_ready = move |port: u16| match rltoken::build_token(
        &secret,
        &adv,
        port,
        &|s: &str| log2.ulogf(s),
        &|_s: &str| {},
    ) {
        Ok(b) => {
            log2.ulogf(&format!("中继 token：{}", b.token));
            log2.ulogf(&format!("端点：{}", b.endpoints.join("、")));
            if rltoken::all_private(&b.endpoints) {
                log2.ulogf("⚠️ 公布的地址都在内网：公网中继请加 --advertise <公网IP:端口>");
            }
            log2.logf(&format!("后端这样用：homeway serve --relay '{}'", b.token));
        }
        Err(e) => {
            log2.logf(&format!("⚠️ token 生成失败（{e}）—— 后端可用裸地址走开放模式"));
        }
    };
    // 只建 pipe、不装信号 handler（统一进程形态：信号 handler 由宿主统一安装——
    // 装配期覆盖宿主 handler 会造成「relay 收工、主线程挂等」的半死窗口，评审 r1-Z5）
    let (stop_r, stop_w) = new_stop_pipe();
    let exited = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let exited_t = Arc::clone(&exited);
    let join = std::thread::Builder::new()
        .name("homeway-relay".into())
        .stack_size(1024 * 1024)
        .spawn(move || {
            // 失败不再 exit（r1-M4：角色失败 = supervisor 退避重建，绝不带走统一进程）；
            // 在世位出口清零（panic 也清）——supervisor 据此判终结。
            struct ExitedGuard(Arc<std::sync::atomic::AtomicBool>);
            impl Drop for ExitedGuard {
                fn drop(&mut self) {
                    self.0.store(true, std::sync::atomic::Ordering::SeqCst);
                }
            }
            let _guard = ExitedGuard(exited_t);
            if let Err(e) = relay.run(stop_r, on_ready) {
                log2_run.logf(&format!("⚠️ relay 运行失败（{e}）—— 线程收工，宿主 supervisor 决定重建"));
            }
        })
        .expect("spawn relay");
    Ok(RelayProc { stop_w, join: Some(join), exited })
}

/// `homeway-cli relay [...]`：前台中继（Ctrl-C / SIGTERM 收工——确定性 closeAll）。
pub fn cmd_relay(args: &[String]) {
    let f = parse_flags(args);
    if !f.extra.is_empty() {
        eprintln!("relay 不接受位置参数（得 {:?}）——启停/查询命令组不在本 CLI 裁剪面", f.extra);
        std::process::exit(2);
    }
    let state_dir = f.state.clone().unwrap_or_else(|| PathBuf::from("."));
    // 单实例锁（Go 全形态共用 <state>/lock；form=relay）
    let _lock = acquire_lock_or_exit(&state_dir, "relay");

    // config.toml（存在即解析 [relay] 节；缺失 = 内置默认）
    let mut listen_str = ":41741".to_owned();
    let mut advertise = String::new();
    let cfg_path = state_dir.join("config.toml");
    if cfg_path.exists() {
        let body = match std::fs::read_to_string(&cfg_path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: 读取失败：{e}", cfg_path.display());
                std::process::exit(1);
            }
        };
        // 只取 [relay] 节（serve 节键表不归本命令校验——手工切片避免整表 deny_unknown 拒启）
        let fc: FileConfig = match parse_relay_section(&body) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("{}: {e}", cfg_path.display());
                std::process::exit(1);
            }
        };
        if let Some(v) = fc.relay.listen {
            listen_str = v;
        }
        if let Some(v) = fc.relay.advertise {
            advertise = v;
        }
    }
    if let Some(v) = &f.listen {
        listen_str = v.clone();
    }
    if let Some(v) = &f.advertise {
        advertise = v.clone();
    }
    let listen = match parse_listen(&listen_str) {
        Some(a) => a,
        None => {
            eprintln!("--listen {listen_str:?} 不是合法监听地址（:port / ip:port）");
            std::process::exit(2);
        }
    };
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let proc = match assemble_relay(
        &state_dir,
        RelayAssemble { listen, advertise, no_hints: f.no_hints, open: f.open },
        Arc::clone(&logf),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("（relay 前台运行中——Ctrl-C 收工）");
    stop_pipe(); // 装 handler（前台形态；pipe 已在装配时建好）
    // 评审 r2-4：run 失败不再 exit(1) 后，前台等待必须双看——只等信号会把
    // 「run 早失败」退化成无输出挂死（RelayLog::logf 只进 relay.log 不进终端）。
    // 形态：poll stop 管道（非阻塞）+ exited 位；任一先到即收。
    {
        let (r, _) = *STOP_PIPE.get().expect("stop_pipe 已装");
        unsafe {
            let fl = libc::fcntl(r, libc::F_GETFL);
            libc::fcntl(r, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
        loop {
            let mut b = [0u8; 8];
            let n = unsafe { libc::read(r, b.as_mut_ptr().cast(), b.len()) };
            if n > 0 {
                break; // 信号
            }
            if proc.exited() {
                eprintln!("relay: 运行失败已收线（细节见 cache/relay.log）——前台退出");
                proc.stop();
                std::process::exit(1);
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    proc.stop();
}

/// 单实例锁失败 = 可行动错误退出（Held 文案带 pid/形态/state）。
pub fn acquire_lock_or_exit(state_dir: &std::path::Path, form: &str) -> homeway_core::nodestate::InstanceLock {
    match homeway_core::nodestate::InstanceLock::acquire(state_dir, form) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// 手工抽 [relay] 节（避免整文件反序列化时被 serve 节的键表拒启——serve 键归 serve 校验）。
fn parse_relay_section(body: &str) -> Result<FileConfig, String> {
    let mut in_relay = false;
    let mut section = String::from("[relay]\n");
    for line in body.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_relay = t == "[relay]";
            continue;
        }
        if in_relay && !t.is_empty() && !t.starts_with('#') {
            section.push_str(line);
            section.push('\n');
        }
    }
    toml::from_str(&section).map_err(|e| format!("[relay] 节解析失败：{e}"))
}

// ---------- stop 管道（与 serve_cli 同款形态） ----------

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

/// 建 stop pipe（不装 handler——装配与信号安装分离，见 assemble_relay 注释）。
fn new_stop_pipe() -> (i32, i32) {
    *STOP_PIPE.get_or_init(|| unsafe {
        let mut fds = [0i32; 2];
        libc::pipe(fds.as_mut_ptr());
        (fds[0], fds[1])
    })
}

/// 前台单角色形态：装信号 handler（Ctrl-C/SIGTERM → relay stop pipe）。
fn stop_pipe() -> (i32, i32) {
    let (r, w) = new_stop_pipe();
    STOP_FD.store(w, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        let h = on_stop_signal as extern "C" fn(i32) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
    (r, w)
}
