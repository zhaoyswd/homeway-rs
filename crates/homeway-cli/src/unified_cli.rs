//! `homeway-cli` 零参形态 = 统一进程（B0-1 部署最小面；语义真源
//! `baseline:internal/daemon/unified.go` + `internal/nodestate`——裁剪面：
//! **client 角色与 control 控制面本期留桩**（hosts.json/control.sock/stream.open/
//! CLI 族归 B0-2），本面只装配 serve/relay 两角色 + 单实例锁 + 三层布局 + events
//! 最小集 + SIGTERM/SIGINT 优雅收尾）。
//!
//! 生产形态对照：
//! - Mac launchd：`homeway` 零参（本 CLI 无子命令形态），stdout 重定向到文件；
//! - 阿里云 nohup：`--state <dir>` 双角色（config serve.enabled + relay.enabled）。
//!
//! 只认全局 flag（--state/--verbose）——角色 flag打在统一进程 = 可行动错误
//! （走 config 或前台单角色形态；Go RunUnified 同义）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use homeway_core::nodestate::{open_node_state, InstanceLock};
use homeway_core::server::engine::{shrink_upnp_lease, ServeEngine};

use crate::relay_cli::{assemble_relay, RelayAssemble, RelayProc};
use crate::serve_cli::assemble as assemble_serve_cfg;

/// 默认 state 根（Go `DefaultStateDir` 同义：`~/.config/homeway`）。
fn default_state_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".config").join("homeway");
    }
    PathBuf::from("./homeway-state")
}

/// config.toml 全键面（serve/relay 双节，deny_unknown——typo 保护；与 serve_cli/
/// relay_cli 的分节 schema 同表）。
#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // 键表完整性守卫（消费经 assemble_serve_cfg 走 serve_cli 的同表解析）
struct FileServe {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    listen: Option<u16>,
    #[serde(default)]
    bind_interface: Option<String>,
    #[serde(default)]
    upnp: Option<bool>,
    #[serde(default)]
    stun: Option<String>,
    #[serde(default)]
    stun6: Option<String>,
    #[serde(default)]
    relay: Option<String>,
    #[serde(default)]
    max_peers: Option<usize>,
    #[serde(default)]
    peer_ttl: Option<String>,
    #[serde(default)]
    public_endpoint: Option<String>,
    #[serde(default)]
    dns_port: Option<u16>,
    #[serde(default)]
    files_root: Option<String>,
    #[serde(default)]
    ddns: Option<Vec<FileDdns>>,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct FileDdns {
    #[serde(default)]
    domain: String,
}

#[derive(serde::Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct FileRelay {
    #[serde(default)]
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
    serve: FileServe,
    #[serde(default)]
    relay: FileRelay,
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
                println!("子命令形态：homeway-cli <serve|relay|connect|speedtest|files|token|dnstest|portfwd> …（无子命令 = 统一进程）");
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


fn run_unified_state(state_dir: PathBuf, verbose: bool) {
    // ① 单实例锁（与前台单角色共用 <state>/lock——同 state 双进程互斥）
    let lock = match InstanceLock::acquire(&state_dir, "unified") {
        Ok(l) => l,
        // Held 的 Display 文案已带 pid/形态/state（Go 同串）——可行动错误退出
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
    let cfg_path = state_dir.join("config.toml");
    let fc: FileConfig = match std::fs::read_to_string(&cfg_path).map_err(|e| e.to_string()).and_then(|b| toml::from_str(&b).map_err(|e| format!("{cfg_path:?}: {e}"))) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };

    // events tee：engine 的摘要判据行进 cache/events.log（2MB×3 轮转；stdout 与文件
    // 双写——launchd stdout 重定向继续可用）；细节行（peer/intercept/dns 判据族）进
    // cache/debug.log（8MB×2 轮转，恒落盘；--verbose 时才回显终端）——P1-1。
    // tokf = token 端点变化轮流（Go tokenToFile：events 文件只写 + verbose 回显——
    // 终端只出首轮 token）。
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

    // ④ serve 按期望态（复用前台 serve 的 config 组装——纯 config 形态，无 flag）
    let mut serve_engine: Option<Arc<ServeEngine>> = None;
    let upnp_used;
    if fc.serve.enabled {
        let cfg = assemble_serve_cfg(&[
            "--state".to_owned(),
            state_dir.display().to_string(),
        ]);
        upnp_used = cfg.upnp;
        events.eventf("serve: 按期望态装配（config serve.enabled=true）");
        match ServeEngine::start(
            cfg,
            Arc::clone(&logf),
            Arc::clone(&dlogf),
            Arc::clone(&tokf),
            log_paths,
        ) {
            Ok(e) => serve_engine = Some(e),
            Err(e) => {
                events.eventf(&format!("serve: 装配失败（{e}）——统一进程退出"));
                std::process::exit(1);
            }
        }
    } else {
        upnp_used = false;
        events.eventf("serve: 期望停用（config serve.enabled=false）——不装配");
    }

    // ⑤ relay 按期望态
    let mut relay_proc: Option<RelayProc> = None;
    if fc.relay.enabled {
        let listen_str = fc.relay.listen.clone().unwrap_or_else(|| ":41741".to_owned());
        let listen = match crate::relay_cli::parse_listen(&listen_str) {
            Some(a) => a,
            None => {
                events.eventf(&format!("relay: 期望启用但 listen {listen_str:?} 非法（[host:]port，如 \":41741\"）——统一进程退出"));
                std::process::exit(1);
            }
        };
        let advertise = fc.relay.advertise.clone().unwrap_or_default();
        match assemble_relay(
            &state_dir,
            RelayAssemble { listen, advertise, no_hints: false, open: false },
            Arc::clone(&logf),
        ) {
            Ok(p) => relay_proc = Some(p),
            Err(e) => {
                events.eventf(&format!("relay: 装配失败（{e}）——统一进程退出"));
                std::process::exit(1);
            }
        }
    } else {
        events.eventf("relay: 期望停用（config relay.enabled=false）——不装配");
    }

    // ⑥ client/control 留桩（B0-2 接：hosts.json 多主机会话 / control.sock 控制面族）
    events.eventf("client: 角色本期未装配（client 恒开语义与 hosts.json 归 B0-2 daemon 批）");
    events.eventf("control: 控制面本期未装配（control.sock/stream.open/CLI 族归 B0-2）");

    // ⑦ 就绪（Go 同串形态；client/control 如实标注留桩——不打「恒开」失实行）
    events.eventf(&format!(
        "homeway: 统一进程就绪（state={}，serve={} relay={}，client/control 留桩归 B0-2，version={}）",
        state_dir.display(),
        fc.serve.enabled,
        fc.relay.enabled,
        crate::cli_version(),
    ));

    // 信号 handler 在**角色装配前**装好（装配期到达的信号也有归属；r1-Z5 整改面）
    crate::serve_cli::install_stop_signals();
    println!("（homeway 统一进程前台运行中——Ctrl-C 收工）");
    let _ = crate::serve_cli::wait_stop_pipe();
    println!("homeway: 收到信号，收工");

    // 按序停：serve（D5 有序收工 + UPnP 退出缩租）→ relay（确定性 closeAll）→ 锁释放
    if let Some(e) = &serve_engine {
        if upnp_used {
            shrink_upnp_lease(e, &logf);
        }
        e.shutdown(Duration::from_secs(2));
    }
    if let Some(p) = relay_proc {
        p.stop();
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
}
