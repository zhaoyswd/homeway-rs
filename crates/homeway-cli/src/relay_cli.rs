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

use crate::cli_flags;
use crate::serve_cli::{load_config_strict, FileConfig};

/// relay 装配参数（config 单表映射；Q-H F1——relay 侧不再有分节私表，
/// `FileServeIgnore`/`parse_relay_section` 随之删除；值域已在 `load_config_strict` 层）。
pub(crate) struct RelayCfg {
    pub listen: SocketAddr,
    pub listen_str: String,
    pub advertise: String,
}

/// 纯映射：`FileConfig` → relay 装配参数（listen 缺省 `:41741`）。
pub(crate) fn relay_config_of(fc: &FileConfig) -> Result<RelayCfg, String> {
    let listen_str = fc.relay.listen.clone().unwrap_or_else(|| ":41741".to_owned());
    let listen = parse_listen(&listen_str)
        .filter(|a| a.port() >= 1)
        .ok_or_else(|| format!("relay.listen {listen_str:?} 非法（[host:]port，端口 1–65535，如 \":41741\"）"))?;
    Ok(RelayCfg {
        listen,
        listen_str,
        advertise: fc.relay.advertise.clone().unwrap_or_default(),
    })
}

#[derive(Default)]
struct RelayFlags {
    state: Option<PathBuf>,
    listen: Option<String>,
    advertise: Option<String>,
    open: bool,
    no_hints: bool,
    extra: Vec<String>,
}

fn relay_usage() {
    eprintln!("用法：homeway-cli relay [--state DIR] [--listen :41741] [--advertise IP:port] [--open] [--no-hints]");
    eprintln!("  = 前台单中继（Ctrl-C 收工）；启停/查询用 `homeway-cli relay start|stop|restart|status|token`（控制面）。");
}

fn parse_flags(args: &[String]) -> Result<RelayFlags, String> {
    let mut f = RelayFlags::default();
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
        match name {
            "state" => {
                f.state = Some(cli_flags::take_state_or_exit("state", inline, next));
                if inline.is_none() {
                    adv = 2;
                }
            }
            "listen" => {
                f.listen = Some(cli_flags::take_value_or_exit("listen", inline, next, false));
                if inline.is_none() {
                    adv = 2;
                }
            }
            "advertise" => {
                f.advertise = Some(cli_flags::take_value_or_exit("advertise", inline, next, false));
                if inline.is_none() {
                    adv = 2;
                }
            }
            // 测试形态开关（不进生产命令面文档——FIX-89 同款「显式开放」哲学）
            "open" => f.open = cli_flags::take_bool_or_exit("open", inline, true),
            // 测试形态：不递送 hint（升级条纹的中继驻留前提；见 Config::no_hints 注释）
            "no-hints" => f.no_hints = cli_flags::take_bool_or_exit("no-hints", inline, true),
            "help" | "h" => {
                relay_usage();
                std::process::exit(0);
            }
            other => return Err(format!("未知参数：--{other}")),
        }
        i += adv;
    }
    Ok(f)
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

/// relay 角色装配产物（统一进程与前台单角色共用）：停止管道（两端 RAII）+ 运行线程句柄。
///
/// Q-G F3：**每次装配新建管道**（去 `OnceLock` 单例——单例复用已关写端是「stop 后
/// start 起来即自退」的根因，实测复现见 `docs/reviews/QG-design.md` §0.5）；
/// `shutdown` 幂等（`stop()`/`Drop` 共用，`Option::take` 保证不可重入 ⇒ 结构上
/// 消除「写已复用 fd 号」与「double-close」）。
pub struct RelayProc {
    stop: Option<homeway_core::relay::StopPipe>,
    join: Option<std::thread::JoinHandle<()>>,
    /// run 线程在世位（线程出口清零——panic 也清；统一进程 supervisor 的运行期
    /// 失败判据）。
    exited: Arc<std::sync::atomic::AtomicBool>,
}

impl RelayProc {
    /// 停止并收线程（确定性 closeAll——Ctrl-C/SIGTERM 的统一收口；幂等）。
    pub fn stop(mut self) {
        self.shutdown();
    }

    /// 收口（`stop()` 与 `Drop` 共用）：disarm（`STOP_FD = -1`）→ 恢复信号处置
    /// （仅当本 proc 曾 arm——见下）→ 写停止字节 → join → 关两端。
    ///
    /// **不变式（借用契约）**：`run` 借用读端裸值（[`StopPipe::read_fd`]）且从不
    /// close ⇒ 必须**先 join 再关两端**（本函数顺序即此）。
    ///
    /// **SIG_DFL 恢复的适用范围**（实现期对设计 §2-F3.2 的收窄，双处登记见
    /// `docs/reviews/QG.md` §2 与 §4）：只在本 proc 曾 `arm_signal`（前台单角色形态）
    /// 时恢复——统一进程不装 relay 自己的 handler（宿主统一安装），无条件恢复会
    /// **打掉宿主的 SIGINT/SIGTERM handler**（Ctrl-C 从「优雅收工」退化为「默认处置」）。
    /// 注：SIG_DFL 防的是**后续**信号；in-flight handler 的窗口由「`swap(-1)` 先于
    /// close + `join` 先于 close」兜住（N9 的原归因在此订正——见 QG.md §4）。
    fn shutdown(&mut self) {
        let Some(stop) = self.stop.take() else {
            return; // 已收口（幂等）
        };
        // disarm：先断 handler 的写面（handler 里 `fd >= 0` 才写）
        let armed_here = STOP_FD.swap(-1, std::sync::atomic::Ordering::SeqCst) >= 0;
        if armed_here {
            unsafe {
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
                libc::signal(libc::SIGINT, libc::SIG_DFL);
            }
        }
        stop.signal();
        if let Some(h) = self.join.take() {
            let _ = h.join();
        }
        // 两端随 StopPipe drop 关（join 已完成、handler 已 disarm ⇒ 关闭顺序无副作用）。
        // **窗口归因订正（代码门⑤）**：in-flight handler 的「写已关号」窗口由
        // 「`swap(-1)` 先于 close + join 先于 close」这对顺序兜住；SIG_DFL 只防
        // **后续**信号落到宿主/relay handler 上（不是关 in-flight 窗口的手段）。
    }

    /// 把停止管道写端交给信号 handler（**只在前台单角色调用**；armed 状态由
    /// `STOP_FD >= 0` 单源表达——不设 `armed` 布尔字段）。
    /// 未 armed 窗口（装配后 ~ 本调用前）由前台循环的「`exited()` 判失败 +
    /// `SIGNAL_HIT` 判信号」兜底，不会漏 Ctrl-C。
    pub fn arm_signal(&self) {
        STOP_FD.store(
            self.stop.as_ref().map(|s| s.write_fd()).unwrap_or(-1),
            std::sync::atomic::Ordering::SeqCst,
        );
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
    /// F3：与 `stop()` 共用幂等 `shutdown`——不再有 double-write/double-close）。
    fn drop(&mut self) {
        self.shutdown();
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
    // 装配期覆盖宿主 handler 会造成「relay 收工、主线程挂等」的半死窗口，评审 r1-Z5）。
    // Q-G F3：管道**每次装配新建**（去单例）；读端裸值交 run（借用契约），
    // 两端所有权归 RelayProc（shutdown 先 join 再关）。
    let stop = homeway_core::relay::StopPipe::new()
        .map_err(|e| format!("relay stop 管道建立失败：{e}"))?;
    let stop_r = stop.read_fd();
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
    Ok(RelayProc { stop: Some(stop), join: Some(join), exited })
}

/// `homeway-cli relay [...]`：前台中继（Ctrl-C / SIGTERM 收工——确定性 closeAll）。
pub fn cmd_relay(args: &[String]) {
    let f = match parse_flags(args) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(2);
        }
    };
    if !f.extra.is_empty() {
        eprintln!("relay 不接受位置参数（得 {:?}）——启停/查询命令组不在本 CLI 裁剪面", f.extra);
        std::process::exit(2);
    }
    let state_dir = f.state.clone().unwrap_or_else(crate::unified_cli::default_state_dir);
    // 单实例锁（Go 全形态共用 <state>/lock；form=relay）
    let _lock = acquire_lock_or_exit(&state_dir, "relay");

    // config.toml：Q-H F1 单表**严格读**（缺省 = 默认；整文件非法 = 拒启——Go
    // nodeconfig.Load 同形；此前只手工切 [relay] 节，serve 节的键表不归本命令校验）。
    let fc = match load_config_strict(&state_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let mut rc = match relay_config_of(&fc) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{}: {e}", state_dir.join("config.toml").display());
            std::process::exit(1);
        }
    };
    if let Some(v) = &f.listen {
        rc.listen_str = v.clone();
        rc.listen = match parse_listen(&rc.listen_str) {
            Some(a) => a,
            None => {
                eprintln!("--listen {:?} 不是合法监听地址（:port / ip:port）", rc.listen_str);
                std::process::exit(2);
            }
        };
    }
    if let Some(v) = &f.advertise {
        rc.advertise = v.clone();
    }
    let listen = rc.listen;
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let proc = match assemble_relay(
        &state_dir,
        RelayAssemble { listen, advertise: rc.advertise, no_hints: f.no_hints, open: f.open },
        Arc::clone(&logf),
    ) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    println!("（relay 前台运行中——Ctrl-C 收工）");
    // 前台形态：装 handler（**单通道**——信号 → 写 stop 管道 → run 醒 → 收工；
    // F3 删掉「前台另读同一根管道」的双读者竞态），随后把写端交给 handler。
    install_relay_stop_signal();
    proc.arm_signal();
    // 评审 r2-4：run 失败不再 exit(1) 后，前台等待必须双看——只等信号会把
    // 「run 早失败」退化成无输出挂死（RelayLog::logf 只进 relay.log 不进终端）。
    // Q-G F3：等待面 = 信号标志 + `exited()` 位（不再 poll stop 管道——那是 run
    // 的通道，双读者会抢字节）。
    loop {
        if SIGNAL_HIT.load(std::sync::atomic::Ordering::SeqCst) {
            break; // 正常 Ctrl-C/SIGTERM 路径（run 侧已由 handler 写醒）
        }
        if proc.exited() {
            eprintln!("relay: 运行失败已收线（细节见 cache/relay.log）——前台退出");
            proc.stop();
            std::process::exit(1);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
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

// ---------- stop 管道（与 serve_cli 同款形态） ----------

static STOP_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

/// 前台形态收到 SIGINT/SIGTERM 的标志（单通道形态的「正常收工」判据——handler
/// 与前台等待循环的唯一共享面；统一进程不装该 handler，故此标志恒 false）。
static SIGNAL_HIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_stop_signal(_sig: i32) {
    // handler 只做两件事：置命中标志 + （若已 armed）写停止字节。
    SIGNAL_HIT.store(true, std::sync::atomic::Ordering::SeqCst);
    let fd = STOP_FD.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        unsafe {
            let b = b"x";
            libc::write(fd, b.as_ptr().cast(), 1);
        }
    }
}

/// 前台单角色形态：装信号 handler（Ctrl-C/SIGTERM → relay stop 管道）。
/// **统一进程不调用**（handler 由宿主统一安装——装配期覆盖宿主 handler 会造成
/// 「relay 收工、主线程挂等」的半死窗口，评审 r1-Z5）。停止管道本体在
/// `assemble_relay` 建（`RelayProc` 持有），此处只把写端交给 handler。
fn install_relay_stop_signal() {
    unsafe {
        let h = on_stop_signal as extern "C" fn(i32) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering as AOrd;

    /// Q-G F3（CLI 层）：`STOP_FD` 的 arm/disarm 单源 + `shutdown` 幂等。
    ///
    /// - `arm_signal()` → `STOP_FD == 写端号`；
    /// - `shutdown()` → `STOP_FD == -1`（disarm，且此后 handler 写面关闭）；
    /// - 再 `shutdown()`（幂等）与随后的 `drop` 不 panic、不双写不双关。
    ///
    /// **单函数内跑**：`STOP_FD`/`SIGNAL_HIT` 是进程级 static（拆两个测试会互踩）。
    #[test]
    fn relay_proc_shutdown_is_idempotent_and_disarms_stop_fd() {
        let stop = homeway_core::relay::StopPipe::new().unwrap();
        let w_fd = stop.write_fd();
        let mut proc = RelayProc {
            stop: Some(stop),
            join: None, // 无 run 线程：只测 arm/shutdown 结构面
            exited: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        };
        proc.arm_signal();
        assert_eq!(STOP_FD.load(AOrd::SeqCst), w_fd, "arm 后 STOP_FD = 写端号");
        assert!(proc.exited(), "exited 位透传");
        proc.shutdown();
        assert_eq!(STOP_FD.load(AOrd::SeqCst), -1, "shutdown 后必须 disarm");
        assert!(proc.stop.is_none(), "两端已 take（结构断言，不用 F_GETFD 判 EBADF）");
        proc.shutdown(); // 幂等：再来一次不 panic
        drop(proc); // Drop 走同一 shutdown：同样幂等
        assert_eq!(STOP_FD.load(AOrd::SeqCst), -1);
    }
}
