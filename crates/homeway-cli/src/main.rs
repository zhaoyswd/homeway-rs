//! homeway-cli —— 测试/运维命令面。
//!
//! R2 实装 `connect`（Session 服务会话形态：暖机/巡检/恢复阶梯/整会话重建都在
//! Session 内）+ 故障注入与时间窗测试钩子（`--inject`/`--recover-from`，test-seams）。
//!
//! 判据行输出对齐 `docs/INTEROP-CRITERIA.md`（模板逐串）；**Session 的一切行带
//! `服务会话: ` 前缀**（R2 前缀口径硬规则——与 Go 客户端真日志逐字一致）；C8
//! （`warmup pong: 就绪（判据=wg）`）是 APP 核形态文案，按 R1 登记在 CLI 层打出。

use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::session::{Level, Session, SessionConfig, SessState};
use homeway_core::speedtest::{self, Params};
use homeway_core::token;
use homeway_core::wgcore::ConnErr;

mod carriers_cli;
mod cli_flags;
mod daemon_cli;
mod relay_cli;
mod serve_cli;
mod term_cli;
mod unified_cli;

/// 版本串（构建方注入：release 流水线 `HOMEWAY_CLI_VERSION=<tag>`——与核面
/// `HOMEWAY_CORE_VERSION`（facade::ClientCore::version）同机制；本地直构建回落
/// devel 形态。发版烟囱判据 = `homeway-cli --version` 能 grep 到 tag）。
pub(crate) fn cli_version() -> &'static str {
    option_env!("HOMEWAY_CLI_VERSION").unwrap_or("(devel)")
}

fn main() {
    // 低-8（B0-2a 登记→D-1 收口）：SIGPIPE 恢复默认处置——Go 运行时同语义（fd1/2
    // 写断管 = 进程静默退出）；Rust 默认忽略 SIGPIPE 会把 println! 变成 panic 链
    //（`homeway-cli … | head` 形态）。恢复默认后全仓 println 面无需逐处改写。
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    let args: Vec<String> = std::env::args().collect();
    // 零参/仅全局 flag形态 = 统一进程（Go `homeway` 零参同义——B0-1 部署最小面）
    let first_is_flag = args.get(1).map(|a| a.starts_with('-')).unwrap_or(false);
    if args.len() <= 1 || (first_is_flag && !matches!(args[1].as_str(), "--version" | "version")) {
        unified_cli::cmd_unified(&args[1..]);
        return;
    }
    match args.get(1).map(String::as_str) {
        Some("--version") | Some("version") => {
            println!("homeway-cli {}", cli_version());
        }
        Some("token") => cmd_token(&args[2..]),
        // serve 命令组（start/stop/restart/status/token）= 控制面往返；动词之外的
        // 形态 = 前台单角色（`serve token [list|revoke]` 纯读命令保留直连 state 形态）。
        Some("serve") => match args.get(2).map(String::as_str) {
            Some("start") | Some("stop") | Some("restart") | Some("status") => {
                daemon_cli::cmd_serve_group(&args[2..])
            }
            Some("token") => {
                // `serve token` = reveal：控制面运行态优先（Go 同款命令组形态，认
                // --state）；`serve token list|revoke` = 纯读/纯文件操作（不经控制面，
                // 直连 state——local-rust-exit.sh 等脚本契约）。
                match args.get(3).map(String::as_str) {
                    Some("list") | Some("revoke") => serve_cli::cmd_serve_token(&args[3..]),
                    Some(other) if other.starts_with('-') || other.len() >= 64 => {
                        // --state 等(flag)/疑似 token 直给形态：控制面 reveal。
                        daemon_cli::cmd_serve_group(&args[2..])
                    }
                    Some(other) => {
                        eprintln!("serve token 不认识的动词 {other:?}（可用：list / revoke <id>；无动词 = reveal）");
                        std::process::exit(2);
                    }
                    None => daemon_cli::cmd_serve_group(&args[2..]),
                }
            }
            Some("relay") | Some("ddns") => daemon_cli::cmd_serve_group(&args[2..]),
            _ => serve_cli::cmd_serve(&args[2..]),
        },
        // relay 命令组同款分流。
        Some("relay") => match args.get(2).map(String::as_str) {
            Some("start") | Some("stop") | Some("restart") | Some("status") | Some("token") => {
                daemon_cli::cmd_relay_group(&args[2..])
            }
            _ => relay_cli::cmd_relay(&args[2..]),
        },
        Some("host") => daemon_cli::cmd_host(&args[2..]),
        Some("status") => daemon_cli::cmd_status(&args[2..]),
        Some("term") => term_cli::cmd_term(&args[2..]),
        Some("export") => daemon_cli::cmd_export(&args[2..]),
        Some("import") => daemon_cli::cmd_import(&args[2..]),
        Some("reset") => daemon_cli::cmd_reset(&args[2..]),
        Some("connect") => cmd_connect(&args[2..]),
        Some("forward") => carriers_cli::cmd_forward(&args[2..]),
        Some("socks") => carriers_cli::cmd_socks(&args[2..]),
        Some("speedtest") => cmd_speedtest_dispatch(&args[2..]),
        Some("files") => cmd_files(&args[2..]),
        Some("dnstest") => cmd_dnstest(&args[2..]),
        Some("portfwd") => cmd_portfwd(&args[2..]),
        _ => {
            eprintln!(
"homeway-cli——可用：\n  零参 = 统一进程（--state DIR / --verbose；client/control 恒开 + serve/relay 按期望态）\n  host add [--name N] [--force] <token> / host list [--json] / host status [name] / host delete <name|id> [--yes]\n  status [--json] [--watch]（daemon.status 聚合面）\n  term <list|new|attach|delete|explain> […]（本地面 term.sock；--host <ref> = 经控制面远程接入）\n  export [dest.tar] / import <file> / reset cache [--state D]（状态工件面：不变量四件打包/落位/清 cache）\n  serve <start|stop|restart|status|token> / relay <start|stop|restart|status|token>（控制面命令组）\n  serve [flags]（前台单角色）/ relay [flags]（前台单角色）\n  token <hmw1…> [--dead-direct]（改写输出：Direct 端点 → 死端口——矩阵中继段注入缝）\n  connect --token <hmw1…> [--identity-dir <dir>] [--endpoint-cache-dir <dir>]\n      [--speedtest] [--dial <ip:port>] [--hold <secs>] [--probe N] [--status-json]\n      [--recover-from <1|2|3> [--recover-cause <s>]] [--inject poison-socket|relay-lock]（注入需 test-seams 构建）\n  speedtest [--host <ref>] [--json] [--down/--up 10s] [--streams 4] [--wait 60s]（守护托管：全主机轮转或单台；Ctrl-C 终止轮转）\n  speedtest --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D] [--rounds N] [--hold]（直连形态：同会话 N 轮——A/B 轮次口径）\n  forward <add|list|delete> / socks <on|off|status>（承载面：端口转发规则/SOCKS5 监听，--state 恒指 daemon state）"
            );
            std::process::exit(2);
        }
    }
}

/// Q-H F8/CA13（代码门 L1）：动词形态的 `--help`/`-h` 短路判据（**任意位置**——
/// 非首位也应短路；此前只认 args[0]）。
fn wants_help(args: &[String]) -> bool {
    args.iter().any(|a| a == "--help" || a == "-h")
}

fn cmd_token(args: &[String]) {
    // Q-H F8/CA13：`--help`/`-h` 短路（用法 + exit 0；任意位置）。
    if wants_help(args) {
        eprintln!("用法：homeway-cli token <hmw1…> [--dead-direct] [--loopback-only] [--v6-only]");
        eprintln!("  无 flag = 解析并打印 peer_id/secret/端点；--dead-direct = Direct 端点改死端口（矩阵中继段注入缝）。");
        std::process::exit(0);
    }
    // `token <hmw1…> --dead-direct`：解析后把 Direct 端点改指 127.0.0.1:1 重编码输出
    //（矩阵中继段的 Go 客户端注入缝——Go host add 无 dead-direct flag；crc4 无密钥
    // 重算即被两侧接受，评审确认可行）。与 --token 的注入语义一致（connect 侧）。
    // `--loopback-only`：Direct 端点的非回环 IPv4 换 127.0.0.1 同端口（R6 前置批 ⑤
    // 对照实验缝：同机拓扑里客户端赛跑可能采纳本机 LAN IP，出口发往它的 UDP 走
    // en0 环回路径——实测 18.3µs/包 vs lo0 5.5µs/包（3.3 倍），强制回环可分离
    //「endpoint 路径成本」与「实现栈成本」）。
    let mut dead_direct = false;
    let mut loopback_only = false;
    // v6-only：Direct 的 **v4** 端点改死端口（v6 保留）——压出「只有 v6 直连可达」
    // 形态（B0-1 验证缝：v6 路径不被 LAN v4 赛跑掩盖；中继端点不动）
    let mut v6_only = false;
    let mut input: Option<String> = None;
    for a in args {
        let Some((name, inline)) = cli_flags::split_flag(a) else {
            if input.is_none() {
                input = Some(a.clone());
            }
            continue;
        };
        match name {
            // Q-H F7b：布尔显式值真生效/非法值 fail-fast（此前 `=false` 被静默忽略）。
            "dead-direct" => dead_direct = cli_flags::take_bool_or_exit("dead-direct", inline, true),
            "loopback-only" => {
                loopback_only = cli_flags::take_bool_or_exit("loopback-only", inline, true)
            }
            "v6-only" => v6_only = cli_flags::take_bool_or_exit("v6-only", inline, true),
            _ => {}
        }
    }
    let Some(s) = input else {
        eprintln!("用法：homeway-cli token <hmw1…> [--dead-direct] [--loopback-only] [--v6-only]");
        std::process::exit(2);
    };
    if v6_only {
        if dead_direct || loopback_only {
            eprintln!("--v6-only 与 --dead-direct/--loopback-only 互斥（各自的改写目标重叠）");
            std::process::exit(2);
        }
        let mut t = match token::decode(&s) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("解析失败：{e}");
                std::process::exit(1);
            }
        };
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct && e.addr.parse::<std::net::SocketAddr>().is_ok_and(|a| a.is_ipv4()) {
                e.addr = "127.0.0.1:1".to_owned();
            }
        }
        let eps: Vec<token::EndpointRef<'_>> =
            t.endpoints.iter().map(|e| token::EndpointRef::new(&e.addr, e.kind)).collect();
        let spec = token::TokenSpec { peer_id: &t.peer_id, secret: &t.secret, endpoints: &eps };
        match token::encode(&spec) {
            Ok(out) => {
                println!("{out}");
                return;
            }
            Err(e) => {
                eprintln!("重编码失败：{e}");
                std::process::exit(1);
            }
        }
    }
    if dead_direct || loopback_only {
        let mut t = match token::decode(&s) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("解析失败：{e}");
                std::process::exit(1);
            }
        };
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                if dead_direct {
                    e.addr = "127.0.0.1:1".to_owned();
                } else if let Some((_, port)) = e.addr.rsplit_once(':') {
                    // 非回环 IPv4 → 127.0.0.1 同端口；IPv6 端点（含 ':'）不动——本缝
                    // 只针对同机 LAN 形态
                    if e.addr.split('.').count() == 4 && !e.addr.starts_with("127.0.0.1:") {
                        e.addr = format!("127.0.0.1:{port}");
                    }
                }
            }
        }
        let eps: Vec<token::EndpointRef<'_>> =
            t.endpoints.iter().map(|e| token::EndpointRef::new(&e.addr, e.kind)).collect();
        let spec = token::TokenSpec { peer_id: &t.peer_id, secret: &t.secret, endpoints: &eps };
        match token::encode(&spec) {
            Ok(out) => {
                println!("{out}");
                return;
            }
            Err(e) => {
                eprintln!("重编码失败：{e}");
                std::process::exit(1);
            }
        }
    }
    match token::decode(&s) {
        Ok(t) => {
            println!("peer_id  = {}", hex_str(t.peer_id.as_bytes()));
            println!("secret   = {}", hex_str(t.secret.as_bytes()));
            for e in &t.endpoints {
                println!("endpoint = {} ({:?})", e.addr, e.kind);
            }
        }
        Err(e) => {
            eprintln!("解析失败：{e}");
            std::process::exit(1);
        }
    }
}

struct ConnectArgs {
    tok: Option<String>,
    identity_dir: Option<PathBuf>,
    cache_dir: Option<PathBuf>,
    do_speedtest: bool,
    dial: Option<SocketAddrV4>,
    hold: u64,
    probe: u32,
    status_json: bool,
    recover_from: Option<i64>,
    recover_cause: String,
    recover_delay: u64,
    inject: Option<String>,
    /// 测试注入：token 的直连端点改指死端口（127.0.0.1:1）——压出「只有中继可达」
    /// 形态（DirectFirst 解锁 + via=relay）。
    dead_direct: bool,
    /// 跳过 identity 会话锁（矩阵脚本/刻意并发测试的逃生口——R7-7e）。
    no_session_lock: bool,
}

fn parse_connect(args: &[String]) -> ConnectArgs {
    let mut a = ConnectArgs {
        tok: None,
        identity_dir: None,
        cache_dir: None,
        do_speedtest: false,
        dial: None,
        hold: 0,
        probe: 0,
        status_json: false,
        recover_from: None,
        recover_cause: "测试注入".to_owned(),
        recover_delay: 0,
        inject: None,
        dead_direct: false,
        no_session_lock: false,
    };
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(raw) else {
            eprintln!("未知参数：{raw}");
            std::process::exit(2);
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        let take_next = |adv: &mut usize| {
            if inline.is_none() {
                *adv = 2;
            }
        };
        match name {
            "token" => {
                a.tok = Some(cli_flags::take_value_or_exit("token", inline, next, false));
                take_next(&mut adv);
            }
            "identity-dir" => {
                a.identity_dir = Some(PathBuf::from(cli_flags::take_value_or_exit(
                    "identity-dir",
                    inline,
                    next,
                    false,
                )));
                take_next(&mut adv);
            }
            "endpoint-cache-dir" => {
                a.cache_dir = Some(PathBuf::from(cli_flags::take_value_or_exit(
                    "endpoint-cache-dir",
                    inline,
                    next,
                    false,
                )));
                take_next(&mut adv);
            }
            "speedtest" => a.do_speedtest = cli_flags::take_bool_or_exit("speedtest", inline, true),
            "dial" => {
                let v = cli_flags::take_value_or_exit("dial", inline, next, false);
                take_next(&mut adv);
                a.dial = v.parse().ok();
                if a.dial.is_none() {
                    eprintln!("--dial 需要 <ipv4:port>（如 192.168.3.12:9999）");
                    std::process::exit(2);
                }
            }
            // Q-H F7a：数值 flag 非法/缺值 fail-fast（此前静默回退 0）。
            "hold" => {
                a.hold = cli_flags::take_num_or_exit::<u64>("hold", inline, next, "秒数，如 30");
                take_next(&mut adv);
            }
            "probe" => {
                a.probe = cli_flags::take_num_or_exit::<u32>("probe", inline, next, "次数，如 1");
                take_next(&mut adv);
            }
            "status-json" => a.status_json = cli_flags::take_bool_or_exit("status-json", inline, true),
            "recover-from" => {
                let v = cli_flags::take_value_or_exit("recover-from", inline, next, false);
                take_next(&mut adv);
                a.recover_from = match v.parse::<i64>() {
                    Ok(n) => Some(n),
                    Err(_) => {
                        eprintln!("--recover-from {v:?} 非法（档位数字 1/2/3）");
                        std::process::exit(2);
                    }
                };
            }
            "recover-delay" => {
                a.recover_delay =
                    cli_flags::take_num_or_exit::<u64>("recover-delay", inline, next, "秒数，如 5");
                take_next(&mut adv);
            }
            // carve-out（§6-12）：自由文本，值可以 '-' 开头。
            "recover-cause" => {
                a.recover_cause = cli_flags::take_value_or_exit("recover-cause", inline, next, true);
                take_next(&mut adv);
            }
            "dead-direct" => a.dead_direct = cli_flags::take_bool_or_exit("dead-direct", inline, true),
            "no-session-lock" => {
                a.no_session_lock = cli_flags::take_bool_or_exit("no-session-lock", inline, true)
            }
            "inject" => {
                a.inject = Some(cli_flags::take_value_or_exit("inject", inline, next, false));
                take_next(&mut adv);
            }
            other => {
                eprintln!("未知参数：--{other}");
                std::process::exit(2);
            }
        }
        i += adv;
    }
    a
}


// ---------------------------------------------------------------------------
// identity 会话锁（R7-7e：同 identity 并发会话 = WG keypair 互踢形态——R6 前置批 ①
// 根因的 CLI 面防线；App 侧第 2 棒复用同一模块落实「隧道/服务会话不得并发」）
// ---------------------------------------------------------------------------

/// 拿 identity 会话锁（verb 自述进错误信息）。`--no-session-lock` 已解析为 false 时直通。
/// 锁存活到返回的守卫 drop（= 本动词会话生命周期）。
fn session_lock_or_exit(identity_dir: &Option<PathBuf>, verb: &str) -> Option<homeway_core::session_lock::SessionLock> {
    let dir = identity_dir.clone().or_else(|| Some(PathBuf::from("identity")))?;
    match homeway_core::session_lock::acquire(&dir, verb) {
        Ok(lock) => Some(lock),
        Err(homeway_core::session_lock::LockError::Held(h)) => {
            eprintln!("!! {h}");
            eprintln!("   同 identity 的另一个会话正在跑（{verb} 想用同一身份）：wireguard 单 peer 只有一条 keypair 链，第二个会话的握手会顶掉第一个 ⇒ 双向黑洞 + 15s 互踢（rekey 螺旋）。");
            eprintln!("   先停掉在跑的会话；或 --identity-dir 指别的目录；或确知无害时 --no-session-lock。");
            std::process::exit(1);
        }
        // Q-H F9：**IO 失败 fail-fast**（只读/异常 identity 目录下锁拿不到 =
        // 「同 identity 不并发」防线静默消失——Rust 独有防线的静默降级正是本批
        // 禁的形态；Go 无对应物）。`--no-session-lock` 仍是显式逃生口。
        Err(homeway_core::session_lock::LockError::Io(io)) => {
            eprintln!("identity 会话锁不可用（{verb}）：{io}");
            eprintln!("   拿不到锁 = 防「同 identity 并发会话互踢 keypair」的防线缺失——拒绝继续（不静默降级）。");
            eprintln!("   把 --identity-dir 指到可写目录，或确知无害时 --no-session-lock。");
            std::process::exit(1);
        }
    }
}

fn cmd_connect(args: &[String]) {
    // Q-H F8/CA13：`--help`/`-h` 短路（用法 + exit 0；任意位置）。
    if wants_help(args) {
        eprintln!("用法：homeway-cli connect --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D] [--speedtest] [--dial <ip:port>]");
        eprintln!("       [--hold S] [--probe N] [--status-json] [--recover-from 1|2|3 [--recover-cause S] [--recover-delay S]] [--inject poison-socket|relay-lock] [--dead-direct] [--no-session-lock]");
        std::process::exit(0);
    }
    let a = parse_connect(args);
    let Some(tok) = a.tok.clone() else {
        eprintln!("用法：homeway-cli connect --token <hmw1…> […]");
        std::process::exit(2);
    };
    let mut t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    if a.dead_direct {
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                e.addr = "127.0.0.1:1".to_owned();
            }
        }
        println!("inject: token 直连端点已改指死端口（127.0.0.1:1）——只有中继可达");
    }
    let _session_lock = (!a.no_session_lock).then(|| session_lock_or_exit(&a.identity_dir, "connect"));

    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let session = match Session::start(SessionConfig {
        token: t,
        identity_dir: a.identity_dir.clone().or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: a.cache_dir.clone(),
        logf: Arc::clone(&logf),
        relay_only: a.inject.as_deref() == Some("relay-lock"),
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    let snap = session.snapshot();
    if snap.state == SessState::Failed {
        eprintln!("会话失败收工：{}", snap.reason);
        std::process::exit(1);
    }
    // C8（APP 核形态判据，按 R1 登记在 CLI 层打出、一次会话一次）
    println!("warmup pong: 就绪（判据=wg）");

    // ---- 故障注入（test-seams 构建；普通构建给出可判定错误）----
    if let Some(what) = &a.inject {
        inject(&session, what);
    }

    // ---- 恢复钩子（时间窗实测：阶梯直测面；delay = 先给外部注入留窗口）----
    if let Some(from) = a.recover_from {
        if a.recover_delay > 0 {
            println!("recover: 等待 {}s 后起跑（注入窗口）", a.recover_delay);
            std::thread::sleep(Duration::from_secs(a.recover_delay));
        }
        let lvl = Level::clamp(from);
        let t0 = Instant::now();
        let rc = session.recover(lvl, &a.recover_cause);
        println!(
            "recover: from={} rc={} 耗时={}ms（ladder 判据行见上）",
            from,
            rc.as_rc(),
            t0.elapsed().as_millis()
        );
    }

    // ---- status-json（2f 契约面）----
    if a.status_json {
        println!("{}", homeway_core::status_json::snapshot_json(&session.snapshot()));
    }

    // ---- 手动探测拍（恢复性验证用）----
    for _ in 0..a.probe {
        match session.client().path_probe(Duration::from_secs(10)) {
            Ok(()) => println!("probe: ok"),
            Err(e) => println!("probe: 失败（{e}）"),
        }
    }

    // ---- transit 产出步骤（--dial 非环回）----
    if let Some(dst) = a.dial {
        match transit_dial(&session, dst) {
            Ok(n) => println!("transit: 经隧道拨 {dst} 成功（收 {n} 字节）"),
            Err(e) => eprintln!("transit: 经隧道拨 {dst} 回读未成（{e}）——出口侧 transit 行已产出，继续"),
        }
    }

    // ---- speedtest（可选；直连面，不走 healing）----
    if a.do_speedtest {
        let client = session.client();
        match speedtest::run(&client, Params::default(), &|s| println!("{s}")) {
            Ok(r) => {
                println!(
                    "speedtest: 摘要 down={:.0}Mbps up={:.0}Mbps",
                    r.down_bps * 8.0 / 1e6,
                    r.up_bps * 8.0 / 1e6
                );
            }
            Err(e) => {
                eprintln!("speedtest 失败：{e}");
                session.stop();
                std::process::exit(1);
            }
        }
    }

    // ---- 保持（巡检在 Session 内跑；缺省 0 = 立即收工）----
    if a.hold > 0 {
        let deadline = Instant::now() + Duration::from_secs(a.hold);
        while Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(500));
        }
    }

    let e = session.client().snapshot();
    println!("收工：WG 传输层累计 rx={}B tx={}B", e.rx, e.tx);
    session.stop();
}

fn inject(session: &Session, what: &str) {
    match what {
        "poison-socket" => {
            #[cfg(feature = "test-seams")]
            {
                session.client().debug_poison_socket();
                println!("inject: UDP socket 已置为失效形态（EBADF 模拟）");
            }
            #[cfg(not(feature = "test-seams"))]
            {
                let _ = session;
                eprintln!("inject: 需要 test-seams 构建（cargo build -p homeway-cli --features homeway-core/test-seams）");
                std::process::exit(2);
            }
        }
        "relay-lock" => {
            #[cfg(feature = "test-seams")]
            {
                session.debug_suppress_hints();
                println!("inject: 中继锁定（非中继源按从未到达处理——模拟直连全断的真机中继形态）");
            }
            #[cfg(not(feature = "test-seams"))]
            {
                let _ = session;
                eprintln!("inject: 需要 test-seams 构建（cargo build -p homeway-cli --features homeway-core/test-seams）");
                std::process::exit(2);
            }
        }
        other => {
            eprintln!("未知注入：{other}（可用：poison-socket / relay-lock）");
            std::process::exit(2);
        }
    }
}

/// 经隧道拨任意 v4 目标（transit 判据产出步骤）：建连 → 读到对端数据或 EOF 即证通。
fn transit_dial(session: &Session, dst: SocketAddrV4) -> Result<usize, ConnErr> {
    let id = session.client().connect(dst)?;
    // 先写一段载荷：echo 类目标只在收到数据后回显（只 connect+read 会一直阻塞——
    // R1 旧形态的目标是主动发横幅的服务）；写完读首块回显即证通收工
    let probe = b"transit-probe-payload-64b-0123456789abcdef0123456789abcdef";
    let mut off = 0usize;
    let mut zero = 0u32;
    while off < probe.len() {
        match session.client().write(id, probe[off..].to_vec()) {
            Ok(w) if w.n > 0 => off += w.n,
            // Err 不重试直接失败（评审 r2-自补2：回执超时的那笔可能仍在引擎队列里
            // 并最终执行——重试会双投；零接纳（Ok(0)）才重试）。
            Ok(_) => {
                zero += 1;
                if zero > 100_000 {
                    eprintln!("transit: 写探测载荷无进展");
                    let _ = session.client().close(id);
                    return Err(ConnErr::Timeout);
                }
                std::thread::yield_now();
            }
            Err(e) => {
                eprintln!("transit: 写探测载荷失败（{e}）");
                let _ = session.client().close(id);
                return Err(e);
            }
        }
    }
    // 证通即收：echo 类目标不主动 EOF——读到首块回显（或对端 FIN）就关（原「读到
    // 16MB 或 EOF」形态在 echo 目标上会阻塞到会话收工，E11 关闭行拖 5 分钟）
    let got = match session.client().read(id) {
        Ok(chunk) => chunk.len(),
        Err(ConnErr::Closed) => 0, // 对端 FIN——连接本身已证通
        Err(e) => {
            let _ = session.client().close(id);
            return Err(e);
        }
    };
    let _ = session.client().close(id);
    Ok(got)
}

// ---------- speedtest 独立动词（A/B 轮次口径：轮 = 同一会话内一次 run；评审 ③-1） ----------

/// speedtest 双模式分流：`--token` = 直连旧形态（R1 起 matrix/perf-ab 脚本契约，
/// 会话内 N 轮）；无 `--token` = 守护托管形态（D-1：`homeway speedtest` 的 Go 对齐
/// 面——runner 状态机 + 全主机轮转 + 双口径输出）。
fn cmd_speedtest_dispatch(args: &[String]) {
    // 等号形也算直连形态（Q-H 代码门 M4：只认空格形会让 `--token=hmw1…` 落守护托管 →
    // 「未知参数」exit 2）。
    if args.iter().any(|a| a == "--token" || a.starts_with("--token=")) {
        cmd_speedtest(args);
    } else {
        carriers_cli::cmd_speedtest_hosted(args);
    }
}

/// `homeway-cli speedtest --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D]
///  [--rounds N] [--hold]`——建一次会话跑 N 轮（每轮自带 2s warmup，与 Go daemon
/// `speedtest --state -host` 常驻同会话口径一致）。`--hold` = 跑完保持会话（RSS 采样）。
fn cmd_speedtest(args: &[String]) {
    // Q-H F8/CA13：`--help`/`-h` 短路（用法 + exit 0；守护托管形态的 help 在 carriers_cli）。
    if wants_help(args) {
        eprintln!("用法：homeway-cli speedtest --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D] [--rounds N] [--hold] [--dead-direct] [--no-session-lock]");
        eprintln!("  无 --token = 守护托管形态（homeway-cli speedtest [--host <ref>] [--json] …）。");
        std::process::exit(0);
    }
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut cache_dir: Option<PathBuf> = None;
    let mut rounds: u32 = 3;
    let mut hold = false;
    let mut dead_direct = false;
    let mut no_session_lock = false;
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(raw) else {
            eprintln!("未知参数：{raw}");
            std::process::exit(2);
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        match name {
            "token" => {
                tok = Some(cli_flags::take_value_or_exit("token", inline, next, false));
                if inline.is_none() { adv = 2; }
            }
            "identity-dir" => {
                identity_dir = Some(PathBuf::from(cli_flags::take_value_or_exit("identity-dir", inline, next, false)));
                if inline.is_none() { adv = 2; }
            }
            "endpoint-cache-dir" => {
                cache_dir = Some(PathBuf::from(cli_flags::take_value_or_exit("endpoint-cache-dir", inline, next, false)));
                if inline.is_none() { adv = 2; }
            }
            "rounds" => {
                // Q-H F7a：非法/缺值 fail-fast（此前静默回落 3 轮）。
                rounds = cli_flags::take_num_or_exit::<u32>("rounds", inline, next, "轮数，如 3");
                if inline.is_none() { adv = 2; }
            }
            "hold" => hold = cli_flags::take_bool_or_exit("hold", inline, true),
            "dead-direct" => dead_direct = cli_flags::take_bool_or_exit("dead-direct", inline, true),
            "no-session-lock" => {
                no_session_lock = cli_flags::take_bool_or_exit("no-session-lock", inline, true)
            }
            other => { eprintln!("未知参数：--{other}"); std::process::exit(2); }
        }
        i += adv;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli speedtest --token <hmw1…> [--identity-dir D] [--endpoint-cache-dir D] [--rounds N] [--hold]");
        std::process::exit(2);
    };
    let mut t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => { eprintln!("token 解析失败：{e}"); std::process::exit(1); }
    };
    if dead_direct {
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                e.addr = "127.0.0.1:1".to_owned();
            }
        }
        println!("inject: token 直连端点已改指死端口（127.0.0.1:1）——只有中继可达");
    }
    let _session_lock = (!no_session_lock).then(|| session_lock_or_exit(&identity_dir, "speedtest"));
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let session = match Session::start(SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: cache_dir,
        logf: Arc::clone(&logf),
        relay_only: false,
    }) {
        Ok(s) => s,
        Err(e) => { eprintln!("会话建立失败：{e}"); std::process::exit(1); }
    };
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    println!("warmup pong: 就绪（判据=wg）");
    let client = session.client();
    for r in 1..=rounds {
        match speedtest::run(&client, Params::default(), &|s| println!("{s}")) {
            Ok(res) => {
                println!(
                    "round {}/{}: down={:.0}Mbps up={:.0}Mbps",
                    r, rounds,
                    res.down_bps * 8.0 / 1e6,
                    res.up_bps * 8.0 / 1e6
                );
            }
            Err(e) => {
                eprintln!("round {r} 失败（reason={}）：{e}", e.reason());
                session.stop();
                std::process::exit(1);
            }
        }
    }
    if hold {
        println!("speedtest: 会话保持中（Ctrl-C 收工）");
        loop {
            std::thread::sleep(Duration::from_secs(3600));
        }
    }
    let e = session.client().snapshot();
    println!("收工：WG 传输层累计 rx={}B tx={}B", e.rx, e.tx);
    session.stop();
}

// ---------- files 动词（每命令一条流；拨号走 healing） ----------

fn cmd_files(args: &[String]) {
    // Q-H F8/CA13：`--help`/`-h` 短路（用法 + exit 0；任意位置）。
    if wants_help(args) {
        eprintln!("用法：homeway-cli files <list|stat|mkdir|read|get|put|download|upload> --token <hmw1…> […] <路径> [<本地>]");
        eprintln!("  远程形态：homeway-cli files <verb> --host <ref> [--state D] [--timeout T] <远端路径> [<本地路径>]");
        std::process::exit(0);
    }
    // files <verb> --token <hmw1> [--identity-dir D] [--dead-direct] [--inject no-hint]
    //   [--rate-limit <bytes/s>] <path> [<local>]（--rate-limit 缺省 2MiB/s 发送端速率
    //   义务；0 = 不限、风险自担——对齐 Go files-cli 1.4）
    // files <verb> --host <ref> [--state D] [--timeout T] [--no-spawn] <path> [<local>]
    //   （--host = 远程形态：寻址与拨号都经 control.sock 的 stream.open{kind:files}
    //   透传腿，与 term --host 同一底座——D-1）
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut dead_direct = false;
    let mut inject_what: Option<String> = None;
    let mut no_session_lock = false;
    let mut rate_limit: Option<i64> = None;
    let mut host_ref: Option<String> = None;
    let mut host_state: Option<PathBuf> = None;
    let mut host_timeout: Option<Duration> = None;
    let mut no_spawn = false;
    let mut rest: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(raw) else {
            rest.push(raw.to_owned());
            i += 1;
            continue;
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        match name {
            "token" => {
                tok = Some(cli_flags::take_value_or_exit("token", inline, next, false));
                if inline.is_none() { adv = 2; }
            }
            "identity-dir" => {
                identity_dir = Some(PathBuf::from(cli_flags::take_value_or_exit(
                    "identity-dir", inline, next, false)));
                if inline.is_none() { adv = 2; }
            }
            "dead-direct" => dead_direct = cli_flags::take_bool_or_exit("dead-direct", inline, true),
            "inject" => {
                inject_what = Some(cli_flags::take_value_or_exit("inject", inline, next, false));
                if inline.is_none() { adv = 2; }
            }
            "no-session-lock" => {
                no_session_lock = cli_flags::take_bool_or_exit("no-session-lock", inline, true)
            }
            "host" => {
                host_ref = Some(cli_flags::take_value_or_exit("host", inline, next, false));
                if inline.is_none() { adv = 2; }
            }
            "state" => {
                host_state = Some(cli_flags::take_state_or_exit("state", inline, next));
                if inline.is_none() { adv = 2; }
            }
            "timeout" => {
                let v = cli_flags::take_value_or_exit("timeout", inline, next, false);
                if inline.is_none() { adv = 2; }
                host_timeout = term_cli::parse_duration(&v);
                if host_timeout.is_none() {
                    eprintln!("--timeout {v:?} 不是时长（如 10s / 1500ms）");
                    std::process::exit(2);
                }
            }
            "no-spawn" => no_spawn = cli_flags::take_bool_or_exit("no-spawn", inline, true),
            "rate-limit" => {
                let v = cli_flags::take_value_or_exit("rate-limit", inline, next, false);
                if inline.is_none() { adv = 2; }
                match v.parse::<i64>() {
                    Ok(n) if n >= 0 => rate_limit = Some(n),
                    Ok(_) => {
                        eprintln!("--rate-limit 不接受负值（Go 同款拒绝）——0 = 不限、正数 = bytes/s");
                        std::process::exit(2);
                    }
                    Err(_) => {
                        eprintln!("--rate-limit 需要非负整数 bytes/s（如 250000；0 = 不限）");
                        std::process::exit(2);
                    }
                }
            }
            // 动词级 flag（如 `-o`）与位置参数留在 rest 交二段解析。
            _ => rest.push(raw.to_owned()),
        }
        i += adv;
    }
    if tok.is_none() && host_ref.is_none() {
        eprintln!("用法：homeway-cli files <list|stat|mkdir|read|get|put|download|upload> --token <hmw1> [--identity-dir D] [--rate-limit B/s（缺省 2MiB/s；0 不限）] <远端路径> [<本地路径>]");
        eprintln!("  远程形态：homeway-cli files <verb> --host <ref> [--state D] [--timeout T] <远端路径> [<本地路径>]（经控制面 stream.open 转发——D-1）");
        eprintln!("  get <远端> [-o 本地] [--force]（目标缺省 = basename，已存在默认拒）；put <本地> <远端>——Go 契约参数序，与 download/upload（<远端> [<本地>]）不同");
        std::process::exit(2);
    }
    let verb = rest.first().cloned().unwrap_or_default();
    // ---- 动词感知解析（dsh r1 高-1 整改）：get/put 是 Go 契约形态，参数序/flag 面
    // 与 download/upload（<远端> [<本地>]）**不同**——按动词分别解析，别混用：
    //   get <远端> [-o 本地] [--force] [--quiet]   （目标缺省 = basename；已存在默认拒）
    //   put <本地> <远端> [--rate-limit N] [--quiet]（本地在前——Go files_cli 同序）
    // download/upload 保留 Rust 原生序（<远端> [<本地>]），两侧脚本契约都不破。
    let (path, local): (String, String) = match verb.as_str() {
        "get" => {
            let mut remote = String::new();
            let mut o_local: Option<String> = None;
            let mut force = false;
            let mut i = 1;
            while i < rest.len() {
                let a = rest[i].as_str();
                match a {
                    "-o" => {
                        i += 1;
                        match rest.get(i) {
                            Some(v) if !v.starts_with('-') => o_local = Some(v.clone()),
                            _ => {
                                eprintln!("-o 需要本地路径（get <远端> [-o 本地] [--force] [--quiet]；下一个是 flag/已到末尾）");
                                std::process::exit(2);
                            }
                        }
                    }
                    _ if a == "--force" || a.starts_with("--force=") => {
                        let inline = cli_flags::split_flag(a).and_then(|(_, v)| v);
                        force = cli_flags::take_bool_or_exit("force", inline, true);
                    }
                    _ if a == "--quiet" || a.starts_with("--quiet=") => {
                        let inline = cli_flags::split_flag(a).and_then(|(_, v)| v);
                        let _ = cli_flags::take_bool_or_exit("quiet", inline, true);
                    }
                    other if other.starts_with('-') => {
                        eprintln!("未知参数：{other}（get 认 -o <本地>、--force、--quiet）");
                        std::process::exit(2);
                    }
                    other => {
                        if !remote.is_empty() {
                            eprintln!("只能给一个远端路径（已有 {remote:?}）");
                            std::process::exit(2);
                        }
                        remote = other.to_owned();
                    }
                }
                i += 1;
            }
            if remote.is_empty() {
                eprintln!("get 需要远端路径：homeway-cli files get <远端> [-o 本地] [--force]");
                std::process::exit(2);
            }
            let target = match o_local {
                Some(t) => t,
                None => {
                    let base = std::path::Path::new(remote.trim_end_matches('/'))
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("");
                    if base.is_empty() || base == "." {
                        eprintln!("从远端路径 {remote:?} 推不出本地文件名；用 -o 显式指定");
                        std::process::exit(2);
                    }
                    base.to_owned()
                }
            };
            // 已存在默认拒、--force 覆盖（Go get 契约）
            if !force && std::path::Path::new(&target).exists() {
                eprintln!("本地目标 {target} 已存在；覆盖请加 --force");
                std::process::exit(2);
            }
            (remote, target)
        }
        "put" => {
            let mut p_local = String::new();
            let mut p_remote = String::new();
            let mut i = 1;
            while i < rest.len() {
                let a = rest[i].as_str();
                match a {
                    _ if a == "--quiet" || a.starts_with("--quiet=") => {
                        let inline = cli_flags::split_flag(a).and_then(|(_, v)| v);
                        let _ = cli_flags::take_bool_or_exit("quiet", inline, true);
                    }
                    other if other.starts_with('-') => {
                        eprintln!("未知参数：{other}（put 认 --rate-limit <bytes/s>、--quiet）");
                        std::process::exit(2);
                    }
                    other => {
                        if p_local.is_empty() {
                            p_local = other.to_owned();
                        } else if p_remote.is_empty() {
                            p_remote = other.to_owned();
                        } else {
                            eprintln!("只能给本地与远端两个路径（已有 {p_local:?} {p_remote:?}）");
                            std::process::exit(2);
                        }
                    }
                }
                i += 1;
            }
            if p_local.is_empty() || p_remote.is_empty() {
                eprintln!("put 需要两个路径：homeway-cli files put <本地> <远端>");
                std::process::exit(2);
            }
            // Go 同义前置校验：本地必须是可读文件（不存在/目录 = 可行动错误）
            match std::fs::metadata(&p_local) {
                Ok(m) if m.is_file() => {}
                Ok(_) => {
                    eprintln!("{p_local} 是目录（put 只支持文件）");
                    std::process::exit(2);
                }
                Err(e) => {
                    eprintln!("读本地文件 {p_local}：{e}");
                    std::process::exit(1);
                }
            }
            (p_remote, p_local)
        }
        "list" | "stat" | "mkdir" | "read" | "download" | "upload" => {
            let p = rest.get(1).cloned().unwrap_or_default();
            let l = rest.get(2).cloned().unwrap_or_default();
            if p.is_empty() {
                eprintln!("用法：homeway-cli files <verb> --token <hmw1> [--rate-limit B/s] <远端路径> [<本地路径>]");
                std::process::exit(2);
            }
            (p, l)
        }
        other => {
            eprintln!("未知动词：{other}");
            std::process::exit(2);
        }
    };
    if let Some(host_ref) = host_ref {
        cmd_files_remote(
            &host_ref,
            FilesRemoteArgs {
                verb: verb.clone(),
                path: path.clone(),
                local: local.clone(),
                state: host_state,
                timeout: host_timeout,
                no_spawn,
                rate_limit,
            },
        );
        return;
    }
    let Some(tok) = tok else {
        eprintln!("homeway: files 需要 --token <hmw1> 或 --host <ref>（远程形态）");
        std::process::exit(2);
    };
    let mut t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    if dead_direct {
        for e in &mut t.endpoints {
            if e.kind == token::EndpointKind::Direct {
                e.addr = "127.0.0.1:1".to_owned();
            }
        }
        println!("inject: token 直连端点已改指死端口（127.0.0.1:1）——只有中继可达");
    }
    let _session_lock = (!no_session_lock).then(|| session_lock_or_exit(&identity_dir, "files"));
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let session = match Session::start(SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: None,
        logf: Arc::clone(&logf),
        relay_only: inject_what.as_deref() == Some("relay-lock"),
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    if let Some(what) = &inject_what {
        inject(&session, what);
    }
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    let budget = Duration::from_secs(30);
    let r: Result<(), homeway_core::files::FilesError> = match verb.as_str() {
        "list" => homeway_core::files::list(&session, budget, &path).map(|entries| {
            for e in entries {
                let kind = if e.is_dir { "dir " } else { "file" };
                println!("{kind} {:>12}  {}", e.size, e.name);
            }
        }),
        "stat" => homeway_core::files::stat(&session, budget, &path).map(|e| {
            println!("{} {} size={} mtimeMs={} mode={}", if e.is_dir { "dir" } else { "file" }, e.name, e.size, e.mtime_ms, e.mode);
        }),
        "mkdir" => homeway_core::files::mkdir(&session, budget, &path).map(|_| println!("已建目录 {path}")),
        "read" => homeway_core::files::read(&session, budget, &path, "", 1 << 20).map(|r| {
            print!("{}", r.text);
        }),
        "download" | "get" => {
            // get = Go 脚本契约名（files_cli 六动词 list/stat/mkdir/read/get/put）；
            // download 保留为别名——两侧脚本都不破（GAP-AUDIT P1-7 动词别名半边）。
            let mut out: Box<dyn std::io::Write> = if local == "-" {
                Box::new(std::io::stdout())
            } else {
                Box::new(std::fs::File::create(&local).unwrap_or_else(|e| {
                    eprintln!("本地文件建不了（{local}）：{e}");
                    std::process::exit(1);
                }))
            };
            homeway_core::files::download(&session, budget, &path, &mut out, |size| {
                eprintln!("服务端声明 {size} 字节");
            })
            .map(|n| println!("下载完成 {n} 字节 → {local}"))
        }
        "upload" | "put" => {
            let mut f = std::fs::File::open(&local).unwrap_or_else(|e| {
                eprintln!("本地文件打不开（{local}）：{e}");
                std::process::exit(1);
            });
            let size = f.metadata().map(|m| m.len() as i64).unwrap_or(0);
            // 发送端速率义务：缺省 2MiB/s（对齐 Go files-cli；--rate-limit 可改/0 不限）
            let rate = rate_limit.unwrap_or(homeway_core::files::DEFAULT_RATE_LIMIT);
            let mut limiter = homeway_core::files::UploadLimiter::new(rate);
            homeway_core::files::upload(&session, budget, &path, &mut f, size, |done| {
                if done % (64 << 20) == 0 {
                    eprintln!("上传进度 {done}/{size}");
                }
            }, limiter.as_mut())
            .map(|n| println!("上传完成 {n} 字节 → {path}"))
        }
        other => {
            eprintln!("未知动词：{other}");
            std::process::exit(2);
        }
    };
    session.stop();
    if let Err(e) = r {
        eprintln!("files {verb} 失败：{e}");
        std::process::exit(1);
    }
}


// ---------- files 远程形态（--host：控制面 stream.open{kind:files} 透传腿——D-1） ----------

/// 远程形态：寻址（host.list + resolve——与 term/host delete 同规则）→ 控制面
/// stream.open → files 协议端到端原样承载（零 wire 改动）。`--timeout` 缺省 10s
/// （解析与打开各一次预算——Go defaultRemoteTimeout 同义）。
/// 远程形态的参数束（cmd_files 转发面收敛）。
struct FilesRemoteArgs {
    verb: String,
    path: String,
    local: String,
    state: Option<PathBuf>,
    timeout: Option<Duration>,
    no_spawn: bool,
    rate_limit: Option<i64>,
}

fn cmd_files_remote(host_ref: &str, a: FilesRemoteArgs) {
    let FilesRemoteArgs { verb, path, local, state, timeout, no_spawn, rate_limit } = a;
    use homeway_core::daemon::vocab;
    let timeout = timeout.unwrap_or(Duration::from_secs(10));
    let state_dir = state.unwrap_or_else(crate::unified_cli::default_state_dir);
    let ref_trim = host_ref.trim();
    if ref_trim.is_empty() {
        eprintln!("homeway: 空寻址串不可用——homeway-cli files <子命令> --host 需要 <name|id>（homeway-cli host list 查看在表主机）");
        std::process::exit(2);
    }
    // ① 解析（拉起面共用；host.list 寻址——与 host delete/term 同一份规则文案）。
    let c = match daemon_cli::dial_control_spawn(&state_dir, "homeway-files", no_spawn) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    let briefs = match c.request(vocab::OpName::HostList.as_str(), None, timeout) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("homeway: host.list 失败：{}", daemon_cli::op_err_text(&e));
            std::process::exit(1);
        }
    };
    let id = match daemon_cli::resolve_host(&briefs, ref_trim) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    // ② 打开流（解析与打开各一次 --timeout 预算）。
    let st = match c.open_stream(vocab::STREAM_KIND_FILES, &id, timeout) {
        Ok(st) => st,
        Err(e) => {
            eprintln!("homeway: {}", files_stream_open_err_text(&e));
            std::process::exit(1);
        }
    };
    // ③ 动词（远程壳——协议与本地同一条实现）。
    let r: Result<(), homeway_core::files::FilesError> = match verb.as_str() {
        "list" => homeway_core::files::list_remote(c.clone(), st.clone(), timeout, &path).map(|entries| {
            for e in entries {
                let kind = if e.is_dir { "dir " } else { "file" };
                println!("{kind} {:>12}  {}", e.size, e.name);
            }
        }),
        "stat" => homeway_core::files::stat_remote(c.clone(), st.clone(), timeout, &path).map(|e| {
            println!("{} {} size={} mtimeMs={} mode={}", if e.is_dir { "dir" } else { "file" }, e.name, e.size, e.mtime_ms, e.mode);
        }),
        "mkdir" => homeway_core::files::mkdir_remote(c.clone(), st.clone(), timeout, &path).map(|_| println!("已建目录 {path}")),
        "read" => homeway_core::files::read_remote(c.clone(), st.clone(), timeout, &path, "", 1 << 20).map(|r| {
            print!("{}", r.text);
        }),
        "download" | "get" => {
            let mut out: Box<dyn std::io::Write> = if local == "-" {
                Box::new(std::io::stdout())
            } else {
                Box::new(std::fs::File::create(&local).unwrap_or_else(|e| {
                    eprintln!("本地文件建不了（{local}）：{e}");
                    std::process::exit(1);
                }))
            };
            homeway_core::files::download_remote(c.clone(), st.clone(), timeout, &path, &mut out, |size| {
                eprintln!("服务端声明 {size} 字节");
            })
            .map(|n| println!("下载完成 {n} 字节 → {local}"))
        }
        "upload" | "put" => {
            let mut f = std::fs::File::open(&local).unwrap_or_else(|e| {
                eprintln!("本地文件打不开（{local}）：{e}");
                std::process::exit(1);
            });
            let size = f.metadata().map(|m| m.len() as i64).unwrap_or(0);
            let rate = rate_limit.unwrap_or(homeway_core::files::DEFAULT_RATE_LIMIT);
            let mut limiter = homeway_core::files::UploadLimiter::new(rate);
            homeway_core::files::upload_remote(
                c.clone(),
                st.clone(),
                timeout,
                &path,
                &mut f,
                size,
                |done| {
                    if done % (64 << 20) == 0 {
                        eprintln!("上传进度 {done}/{size}");
                    }
                },
                limiter.as_mut(),
            )
            .map(|n| println!("上传完成 {n} 字节 → {path}"))
        }
        other => {
            eprintln!("未知动词：{other}");
            std::process::exit(2);
        }
    };
    // 收口：先 Client.Close（唯一可靠逃生口——closed 解阻塞读腿），stream.close
    // 尽力而为（上传未提交 = 取消语义由流终结承载）。
    let _ = c.stream_close(&st, Duration::from_secs(3));
    c.close();
    if let Err(e) = r {
        eprintln!("files {verb} 失败：{e}");
        std::process::exit(1);
    }
}

/// stream.open 错误码 → files 面可行动文案（Go filesStreamOpenErr：bad_request 的
/// 代际文案——kind 值域外 = 新 CLI 配旧 daemon 的唯一兼容断点，design D5）。
fn files_stream_open_err_text(e: &homeway_core::daemon::proto::OpError) -> String {
    let detail = {
        let d = e.detail();
        if d.is_empty() { String::new() } else { format!("：{d}") }
    };
    match e.code.as_str() {
        homeway_core::daemon::vocab::CODE_BAD_REQUEST => {
            "守护进程代际过旧，不识 files 流；请同批升级 daemon（bad_request：stream.open 的 kind 值域外）".to_owned()
        }
        homeway_core::daemon::vocab::CODE_NO_HOST => {
            "主机不在守护进程表中（no_host）；homeway-cli host list 查看在表主机".to_owned()
        }
        homeway_core::daemon::vocab::CODE_NOT_READY => {
            "守护进程注册表未就绪（not_ready；client 角色启动中/重建窗口），稍后重试".to_owned()
        }
        homeway_core::daemon::vocab::CODE_STREAM_REFUSED => format!(
            "与主机的流打开被拒（stream_refused）——主机离线、隧道未通或主机会话不可用；用 homeway-cli host status <name> 核对会话与链路态{detail}"
        ),
        "timeout" => "连接/打开超预算（--timeout）：daemon 未应答或目标主机拨号黑洞；可用 --timeout 加大预算后重试".to_owned(),
        other => format!("stream.open 失败：{other}{detail}"),
    }
}

// ---------- dnstest（DNS 代答实测 + E12 出口 UDP 面采样；R3-3f 判据产出步骤） ----------

/// `homeway-cli dnstest --token <hmw1> [--identity-dir D] [--mode tcp5300|udp53|leg] <域名>`
///   tcp5300：隧道 IP:<解析腿端口> TCP（客户端远程解析腿——qtcp 计数面）
///   udp53：隧道 IP:53 UDP（手机声明的 DNS——栈内 listener 面，q 计数）
///   leg：8.8.8.8:53 UDP（非隧道 IP 的 :53——拦截层进程内腿 + E12 dns 会话行）
fn cmd_dnstest(args: &[String]) {
    // Q-H F8/CA13：`--help`/`-h` 短路（用法 + exit 0；任意位置）。
    if wants_help(args) {
        eprintln!("用法：homeway-cli dnstest --token <hmw1…> [--identity-dir D] [--mode tcp5300|udp53|leg] <域名>");
        std::process::exit(0);
    }
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut mode = "tcp5300".to_owned();
    let mut name = String::new();
    let mut no_session_lock = false;
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        let Some((fname, inline)) = cli_flags::split_flag(raw) else {
            name = raw.to_owned();
            i += 1;
            continue;
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        match fname {
            "token" => {
                tok = Some(cli_flags::take_value_or_exit("token", inline, next, false));
                if inline.is_none() { adv = 2; }
            }
            "identity-dir" => {
                identity_dir = Some(PathBuf::from(cli_flags::take_value_or_exit("identity-dir", inline, next, false)));
                if inline.is_none() { adv = 2; }
            }
            "mode" => {
                mode = cli_flags::take_value_or_exit("mode", inline, next, false);
                if inline.is_none() { adv = 2; }
            }
            "no-session-lock" => {
                no_session_lock = cli_flags::take_bool_or_exit("no-session-lock", inline, true)
            }
            other => {
                eprintln!("未知参数：--{other}");
                std::process::exit(2);
            }
        }
        i += adv;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli dnstest --token <hmw1…> [--mode tcp5300|udp53|leg] <域名>");
        std::process::exit(2);
    };
    if name.is_empty() {
        eprintln!("dnstest 需要 <域名>（如 example.com）");
        std::process::exit(2);
    }
    let t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    let _session_lock = (!no_session_lock).then(|| session_lock_or_exit(&identity_dir, "dnstest"));
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let session = match homeway_core::session::Session::start(homeway_core::session::SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: None,
        logf: Arc::clone(&logf),
        relay_only: false,
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    // 构造 A 查询
    let mut q = Vec::new();
    q.extend_from_slice(&0x1234u16.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);

    let client = session.client();
    let r = match mode.as_str() {
        "tcp5300" => dns_tcp(&client, &q),
        "udp53" => dns_udp(&client, &q, homeway_core::wgcore::SERVER_TUNNEL_IP, 53),
        "leg" => dns_udp(&client, &q, std::net::Ipv4Addr::new(8, 8, 8, 8), 53),
        other => {
            eprintln!("未知 --mode {other:?}（可用：tcp5300 / udp53 / leg）");
            session.stop();
            std::process::exit(2);
        }
    };
    session.stop();
    match r {
        Ok(resp) => {
            let rcode = resp.get(3).map(|b| b & 0x0F).unwrap_or(9);
            let anc = resp.get(6).and_then(|h| resp.get(7).map(|l| (u16::from(*h) << 8) | u16::from(*l))).unwrap_or(0);
            println!("dnstest[{mode}] {name}: rcode={rcode} answers={anc} bytes={}", resp.len());
            if rcode == 0 && anc > 0 {
                println!("（出口侧应见 dns: 计数行——E22 判据）");
            }
        }
        Err(e) => {
            eprintln!("dnstest[{mode}] 失败：{e}");
            std::process::exit(1);
        }
    }
}

fn dns_tcp(client: &homeway_core::wgcore::Client, q: &[u8]) -> Result<Vec<u8>, String> {
    let dst = SocketAddrV4::new(homeway_core::wgcore::SERVER_TUNNEL_IP, 5300);
    let id = client.connect(dst).map_err(|e| e.to_string())?;
    let mut frame = Vec::with_capacity(2 + q.len());
    frame.extend_from_slice(&(q.len() as u16).to_be_bytes());
    frame.extend_from_slice(q);
    // 写
    let mut off = 0;
    while off < frame.len() {
        let n = client.write(id, frame[off..].to_vec()).map_err(|e| e.to_string())?;
        if n.n == 0 {
            std::thread::yield_now();
            continue;
        }
        off += n.n;
    }
    // 读：2B 长度 + 报文
    let mut buf = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(6);
    while buf.len() < 2 && Instant::now() < deadline {
        match client.read(id) {
            Ok(chunk) if !chunk.is_empty() => buf.extend_from_slice(&chunk),
            Err(homeway_core::wgcore::ConnErr::Closed) => break,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    if buf.len() < 2 {
        let _ = client.close(id);
        return Err("6s 内未读到响应长度前缀".into());
    }
    let mlen = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    while buf.len() < 2 + mlen && Instant::now() < deadline {
        match client.read(id) {
            Ok(chunk) if !chunk.is_empty() => buf.extend_from_slice(&chunk),
            Err(homeway_core::wgcore::ConnErr::Closed) => break,
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let _ = client.close(id);
    if buf.len() < 2 + mlen {
        return Err(format!("响应不完整（{}/{}）", buf.len() - 2, mlen));
    }
    Ok(buf[2..2 + mlen].to_vec())
}

fn dns_udp(
    client: &homeway_core::wgcore::Client,
    q: &[u8],
    ip: std::net::Ipv4Addr,
    port: u16,
) -> Result<Vec<u8>, String> {
    let (id, local_port) = client.udp_open().map_err(|e| e.to_string())?;
    println!("dnstest: UDP 源端口 {local_port} → {ip}:{port}");
    client
        .udp_send(id, SocketAddrV4::new(ip, port), q.to_vec())
        .map_err(|e| e.to_string())?;
    // 收（整体预算 6s——recv 阻塞面由看门狗线程兜）
    let (tx, rx) = std::sync::mpsc::channel();
    let c2 = unsafe_client(client);
    std::thread::spawn(move || {
        let r = c2.udp_recv(id).map_err(|e| e.to_string());
        let _ = tx.send(r);
    });
    match rx.recv_timeout(Duration::from_secs(6)) {
        Ok(Ok((data, from))) => {
            let _ = client.udp_close(id);
            println!("dnstest: 应答来自 {from}（{} 字节）", data.len());
            Ok(data)
        }
        Ok(Err(e)) => Err(e),
        Err(_) => {
            let _ = client.udp_close(id);
            Err("6s 内无应答".into())
        }
    }
}

/// udp_recv 的跨线程调用面（Client 是 & 引用——Send 边界用原始指针横传；调用方
/// 保证生命周期（session 活到函数尾）——测试动词专用，不进 core）。
fn unsafe_client(c: &homeway_core::wgcore::Client) -> &'static homeway_core::wgcore::Client {
    unsafe { &*(c as *const _) }
}

// ---------- portfwd（本地 127.0.0.1 监听 → 经隧道拨目标；CLI 测试动词） ----------

fn cmd_portfwd(args: &[String]) {
    // Q-H F8/CA13：`--help`/`-h` 短路（用法 + exit 0；任意位置）。
    if wants_help(args) {
        eprintln!("用法：homeway-cli portfwd --token <hmw1…> --map 15432:5432 [--map 15433:1.2.3.4:5432] [--identity-dir D] [--no-session-lock]");
        std::process::exit(0);
    }
    let mut tok: Option<String> = None;
    let mut identity_dir: Option<PathBuf> = None;
    let mut maps: Vec<(u16, Option<SocketAddrV4>)> = Vec::new();
    let mut no_session_lock = false;
    let mut i = 0;
    while i < args.len() {
        let raw = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(raw) else {
            eprintln!("未知参数：{raw}");
            std::process::exit(2);
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        match name {
            "token" => {
                tok = Some(cli_flags::take_value_or_exit("token", inline, next, false));
                if inline.is_none() { adv = 2; }
            }
            "identity-dir" => {
                identity_dir = Some(PathBuf::from(cli_flags::take_value_or_exit(
                    "identity-dir", inline, next, false)));
                if inline.is_none() { adv = 2; }
            }
            "no-session-lock" => {
                no_session_lock = cli_flags::take_bool_or_exit("no-session-lock", inline, true)
            }
            "map" => {
                let spec = cli_flags::take_value_or_exit("map", inline, next, false);
                if inline.is_none() { adv = 2; }
                let parts: Vec<&str> = spec.split(':').collect();
                let parsed = match parts.as_slice() {
                    [l, tport] => (
                        l.parse().ok(),
                        tport.parse().ok().map(|p: u16| {
                            SocketAddrV4::new(homeway_core::wgcore::SERVER_TUNNEL_IP, p)
                        }),
                    ),
                    // v6 形态暂不支持（本地实例恒 v4 字面量；`ip:port` 已由 3 段分支覆盖）
                    [l, ip, tport] => (
                        l.parse().ok(),
                        ip.parse().ok().zip(tport.parse().ok()).map(|(a, p)| SocketAddrV4::new(a, p)),
                    ),
                    _ => (None, None),
                };
                match parsed {
                    (Some(listen), target) if listen > 0 => maps.push((listen, target)),
                    _ => {
                        eprintln!("--map 需要 <listen>:<ip:port|port> 形态（得 {spec}）");
                        std::process::exit(2);
                    }
                }
            }
            other => {
                eprintln!("未知参数：--{other}");
                std::process::exit(2);
            }
        }
        i += adv;
    }
    let Some(tok) = tok else {
        eprintln!("用法：homeway-cli portfwd --token <hmw1> --map 15432:5432 [--map 15433:1.2.3.4:5432]");
        std::process::exit(2);
    };
    if maps.is_empty() {
        eprintln!("至少一条 --map");
        std::process::exit(2);
    }
    let t = match token::decode(&tok) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("token 解析失败：{e}");
            std::process::exit(1);
        }
    };
    let _session_lock = (!no_session_lock).then(|| session_lock_or_exit(&identity_dir, "portfwd"));
    let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|s: &str| println!("{s}"));
    let session = match Session::start(SessionConfig {
        token: t,
        identity_dir: identity_dir.or_else(|| Some(PathBuf::from("identity"))),
        endpoint_cache_dir: None,
        logf: Arc::clone(&logf),
        relay_only: false,
    }) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("会话建立失败：{e}");
            std::process::exit(1);
        }
    };
    if session.snapshot().state == SessState::Failed {
        eprintln!("会话失败收工");
        std::process::exit(1);
    }
    // CLI 测试动词形态：Session 泄漏成 'static（本命令永不返回；单条失败只记
    // 状态、不阻断其它映射——Go setPortForwards 同义）
    let sess: &'static Session = Box::leak(Box::new(session));
    for (listen, target) in maps {
        // N7（Q-F F1-5）：目标文案收敛到 `pf_target_text` 单一真源——此前这里第三份
        // 拷贝与 Go `pfTargetText` 有两处不等（`L:IP:0` 打 `IP:0`；`L:PORT` 打
        // 「主机（同端口）」）；语义：出口自己（空 ip）⇒「主机…」，port 0 ⇒ 同监听端口。
        let rule = homeway_core::facade::portfwd::PortForwardRule {
            listen,
            target_ip: match &target {
                Some(t) if t.ip() != &homeway_core::wgcore::SERVER_TUNNEL_IP => t.ip().to_string(),
                _ => String::new(),
            },
            target_port: target.as_ref().map(|t| t.port()).unwrap_or(0),
        };
        let target_text = homeway_core::facade::portfwd::pf_target_text(&rule);
        let target = target.map(|t| {
            // port 0 = 同监听端口（文案与拨号目标同一语义）
            let port = if t.port() == 0 { listen } else { t.port() };
            SocketAddrV4::new(*t.ip(), port)
        });
        let listener = match std::net::TcpListener::bind(("127.0.0.1", listen)) {
            Ok(l) => l,
            Err(e) => {
                println!("port-forward: 监听 127.0.0.1:{listen} 失败（{e}）——该条映射不可用，不影响隧道 [code={}]", homeway_core::PortfwdErr::BindFailed);
                continue;
            }
        };
        println!("port-forward: 127.0.0.1:{listen} -> {target_text} 监听中");
        let dst = target.expect("解析期已保证 Some");
        std::thread::spawn(move || {
            for conn in std::iter::repeat_with(|| listener.accept().map(|(c, _)| c)) {
                let local_conn = match conn {
                    Ok(c) => c,
                    Err(_) => continue,
                };
                // 拨远端（恢复感知；15s = Go portfwd dialTimeout）
                let Ok(remote_id) = sess.healing_dial_addr(dst, Duration::from_secs(15)) else {
                    continue;
                };
                let w_conn = match local_conn.try_clone() {
                    Ok(c) => c,
                    Err(_) => {
                        let _ = sess.client().close(remote_id);
                        continue;
                    }
                };
                // 上行：本地 → 隧道
                std::thread::spawn(move || {
                    let mut lc = local_conn;
                    let mut buf = [0u8; 16 * 1024];
                    loop {
                        match std::io::Read::read(&mut lc, &mut buf) {
                            Ok(0) | Err(_) => {
                                let _ = sess.client().shutdown(remote_id);
                                return;
                            }
                            Ok(n) => {
                                let mut off = 0;
                                while off < n {
                                    match sess.client().write(remote_id, buf[off..n].to_vec()) {
                                        Ok(w) if w.n > 0 => off += w.n,
                                        _ => {
                                            let _ = lc.shutdown(std::net::Shutdown::Both);
                                            return;
                                        }
                                    }
                                }
                            }
                        }
                    }
                });
                // 下行：隧道 → 本地
                std::thread::spawn(move || {
                    let mut wc = w_conn;
                    loop {
                        match sess.client().read(remote_id) {
                            Ok(chunk) if !chunk.is_empty() => {
                                let mut off = 0;
                                while off < chunk.len() {
                                    match std::io::Write::write(&mut wc, &chunk[off..]) {
                                        Ok(w) if w > 0 => off += w,
                                        _ => {
                                            let _ = sess.client().close(remote_id);
                                            return;
                                        }
                                    }
                                }
                            }
                            _ => {
                                let _ = wc.shutdown(std::net::Shutdown::Both);
                                return;
                            }
                        }
                    }
                });
            }
        });
    }
    println!("portfwd: 全部映射已起（Ctrl-C 收工）");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}
