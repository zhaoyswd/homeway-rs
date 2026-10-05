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
    // 只扫 flag 形态 token：--state 的值位跳过，--state=DIR 归并。
    let mut state_dir = default_state_dir();
    let mut verbose = false;
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if !a.starts_with('-') {
            eprintln!("统一进程不接受位置参数（{a:?}）——角色命令见 `homeway-cli serve` / `homeway-cli relay` / 客户端域命令");
            std::process::exit(2);
        }
        let name = a.trim_start_matches('-');
        if name.starts_with("state") && !name.contains('=') {
            if let Some(v) = args.get(i + 1) {
                state_dir = PathBuf::from(v);
            }
            i += 1; // --state 的值位
            i += 1;
            continue;
        }
        let name = name.split('=').next().unwrap_or(name);
        match name {
            "state" | "verbose" => verbose = name == "verbose" || verbose,
            "help" | "h" => {
                println!("homeway-cli [统一进程] —— 零参起；只认 --state <dir> / --verbose（角色参数写 config.toml，或用 serve/relay 前台单角色形态）");
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

    // ② 三层布局 + config 缺失生成默认 + events 最小面
    let ns = match open_node_state(&state_dir) {
        Ok(ns) => ns,
        Err(e) => {
            eprintln!("打开 state {} 失败：{e}", state_dir.display());
            std::process::exit(1);
        }
    };
    let events = Arc::new(ns.events);
    if ns.config_generated {
        events.eventf("config.toml 缺失——已生成默认（serve.enabled=true）");
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

    // events tee：engine 的摘要判据行进 cache/events.log（完整三级轮转归 B0-2）
    let logf: Arc<dyn Fn(&str) + Send + Sync> = {
        let ev = Arc::clone(&events);
        Arc::new(move |s: &str| ev.eventf(s))
    };
    let dlogf: Arc<dyn Fn(&str) + Send + Sync> = {
        let ev = Arc::clone(&events);
        Arc::new(move |s: &str| {
            if verbose {
                ev.eventf(s);
            }
        })
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
        match ServeEngine::start(cfg, Arc::clone(&logf), Arc::clone(&dlogf)) {
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

    // ⑧ 等信号收工（SIGTERM/SIGINT——launchd 的 KeepAlive 停止/nohup 的 kill 走这里）
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
