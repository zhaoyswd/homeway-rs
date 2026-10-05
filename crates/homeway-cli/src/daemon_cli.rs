//! daemon CLI 族（B0-2b 第 1 棒：host / status / serve 命令组 / relay 命令组——
//! 语义真源 `baseline:internal/daemon/{host_cli,status_cli,servegroup_cli}.go`；
//! 全部经 control.sock 控制面往返，`homeway-core::daemon::client` 为消费面）。
//!
//! 动词面（Go 对齐子集）：`host add/list/status/delete`、`status [--json]`、
//! `serve start/stop/restart/status/token`、`relay start/stop/restart/status/token`。
//! `serve relay`/`serve ddns`、`status --watch`、承载面（forward/socks/speedtest）
//! 与 `term/files --host` 远程模式归 B0-2 后续棒（挂账见 docs/reviews/B0-2b.md）。

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use homeway_core::daemon::client::ControlClient;
use homeway_core::daemon::proto::OpError;
use homeway_core::daemon::vocab;

use crate::unified_cli::default_state_dir;

const TIMEOUT: Duration = Duration::from_secs(10);

/// host.add 的请求预算（Go 缺省 10s；Rust 侧同键刷新的会话停止有界 6s〔patrol
/// 阻塞探针不可中断——join 烧满 STOP_WAIT〕+ 探测 3.5s + 余量 ⇒ 放宽到 20s。
/// spec 的 ≤3.5s MUST 约束的是服务端**探测**预算，不是 CLI 请求预算）。
const HOST_ADD_TIMEOUT: Duration = Duration::from_secs(20);

/// 控制面错误 → 人类可读文案（码不进契约，文案按码分支——Go host_cli 同纪律）。
fn op_err_text(e: &OpError) -> String {
    let extra = {
        let d = e.detail();
        if d.is_empty() { String::new() } else { format!("：{d}") }
    };
    match e.code.as_str() {
        "unknown_op" => format!("守护进程不认识该操作（版本偏斜？）{extra}"),
        "not_ready" => format!("守护进程未就绪（client 角色重建窗口？）{extra}"),
        "shutting_down" => format!("守护进程收工中{extra}"),
        "host_exists" => format!("该后端已在主机表（同 token 重复添加）{extra}"),
        "no_host" => format!("主机不在表中{extra}"),
        "bad_token" => format!("token 非法{extra}（token 形如 hmw1…，从出口启动日志现场获取后重新粘贴）"),
        "host_unreachable" => format!("全部端点探测无应答（host 全不可达）{extra}；确认出口在跑，或用 --force 跳过验证直接入表"),
        "stream_refused" => format!("流打开被拒{extra}"),
        "cursor_stale" => format!("订阅游标过旧/代际失配（需全量重快照）{extra}"),
        "timeout" => format!("等待应答超时{extra}"),
        "conn_closed" => format!("控制面连接已关闭{extra}"),
        "io_error" => format!("控制面 IO 错误{extra}"),
        other => format!("{other}{extra}"),
    }
}

fn exit_op_err(e: &OpError) -> ! {
    eprintln!("homeway: {}", op_err_text(e));
    std::process::exit(1);
}

fn dial_control(state_dir: &std::path::Path) -> Arc<ControlClient> {
    let sock = state_dir.join("control.sock");
    ControlClient::dial(&sock, "cli", "homeway")
        .map(|(c, _)| c)
        .unwrap_or_else(|e| {
            eprintln!(
                "homeway: 连不上 {}（{e}）——守护进程未运行？先起统一进程：homeway-cli --state {}（host add 的按需拉起归后续棒）",
                sock.display(),
                state_dir.display()
            );
            std::process::exit(1)
        })
}

/// 控制面连通则 Some，否则 None（reveal 族回落台账直读用）。
fn try_dial_control(state_dir: &std::path::Path) -> Option<Arc<ControlClient>> {
    let sock = state_dir.join("control.sock");
    match ControlClient::dial(&sock, "cli", "homeway") {
        Ok((c, w)) => {
            eprintln!(
                "（已连 control.sock：serverVersion={} generation={} seq={}）",
                w.server_version, w.generation, w.server_seq
            );
            Some(c)
        }
        Err(_) => None,
    }
}

/// serve/relay token 的 reveal：控制面运行态优先，**守护不在跑回落台账直读**
/// （Go CLI 无此回落〔报可行动错误〕；保留是 local-rust-exit.sh 等脚本契约——
/// 前台单角色形态无 control.sock，token 直读是该形态的唯一取法；登记 B0-2b.md）。
fn token_reveal_with_fallback(state: &std::path::Path, op: vocab::OpName) {
    if let Some(c) = try_dial_control(state) {
        match c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT) {
            Ok(v) => {
                println!("{}", v["token"].as_str().unwrap_or("?"));
                if let Some(eps) = v["endpoints"].as_array() {
                    let eps: Vec<_> = eps.iter().filter_map(|e| e.as_str()).collect();
                    if !eps.is_empty() {
                        eprintln!("（端点：{}；来源={}）", eps.join("、"), v["source"].as_str().unwrap_or("?"));
                    }
                }
            }
            Err(e) => exit_op_err(&e),
        }
        return;
    }
    // 回落：serve = 台账末行直读（serve_cli 同源）；relay = 前台形态日志取。
    match op {
        vocab::OpName::ServeToken => {
            crate::serve_cli::cmd_serve_token(&[
                "--state".to_owned(),
                state.display().to_string(),
            ]);
        }
        _ => {
            eprintln!("homeway: 守护进程未运行——relay token 的离线推算请用 `homeway-cli relay` 前台形态日志");
            std::process::exit(1);
        }
    }
}

/// 统一 flag 解析产物：--state（两种取值形态等价）+ 位置参数；未知 flag 报错退出
/// （CLI-1 整改：此前 `--state=DIR` 在 status/serve 组/relay 组被静默忽略 → 命令打
/// 到默认（生产）state——统一进程侧 r1-H2 同坑的 CLI 面复发）。
struct ParsedArgs {
    state: PathBuf,
    positional: Vec<String>,
    json: bool,
    yes: bool,
    force: bool,
    name: Option<String>,
}

fn parse_args(usage: &str, args: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs {
        state: default_state_dir(),
        positional: Vec::new(),
        json: false,
        yes: false,
        force: false,
        name: None,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let (name, inline) = match a.strip_prefix("--").unwrap_or(a).split_once('=') {
            Some((k, v)) => (k.to_owned(), Some(v.to_owned())),
            None => (a.trim_start_matches('-').to_owned(), None),
        };
        match name.as_str() {
            "state" => {
                if let Some(v) = inline {
                    out.state = PathBuf::from(v);
                } else {
                    i += 1;
                    let Some(v) = args.get(i) else {
                        eprintln!("--state 需要目录参数");
                        std::process::exit(2);
                    };
                    out.state = PathBuf::from(v.clone());
                }
            }
            "json" => out.json = true,
            "yes" => out.yes = true,
            "force" => out.force = true,
            "name" => {
                i += 1;
                let Some(v) = args.get(i) else {
                    eprintln!("--name 需要值");
                    std::process::exit(2);
                };
                out.name = Some(v.clone());
            }
            "h" | "help" => {}
            _other => {
                if a.starts_with('-') {
                    eprintln!("未知参数：{a}（{usage}）");
                    std::process::exit(2);
                }
                out.positional.push(a.to_owned());
            }
        }
        i += 1;
    }
    out
}

/// 主机寻址（Go resolveHostTarget 同义：空串拒绝、名称/唯一前缀、歧义列候选、
/// 全长 64 hex 直用——CLI-2 整改：`starts_with("")` 恒真曾可静默删第一台）。
fn resolve_host(briefs: &serde_json::Value, want: &str) -> Result<String, String> {
    if want.is_empty() {
        return Err("寻址串为空（给 name、id 前缀或全长 id）".to_owned());
    }
    let hosts = briefs["hosts"].as_array().cloned().unwrap_or_default();
    if want.len() == 64 && want.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Ok(want.to_owned());
    }
    let mut hits: Vec<String> = hosts
        .iter()
        .filter(|b| {
            let bid = b["id"].as_str().unwrap_or("");
            bid.starts_with(want) || b["name"].as_str() == Some(want)
        })
        .map(|b| {
            format!(
                "{}（{}）",
                &b["id"].as_str().unwrap_or("?")[..12.min(b["id"].as_str().unwrap_or("?").len())],
                b["name"].as_str().unwrap_or("-")
            )
        })
        .collect();
    if hits.len() == 1 {
        let b = hosts
            .iter()
            .find(|b| {
                let bid = b["id"].as_str().unwrap_or("");
                bid.starts_with(want) || b["name"].as_str() == Some(want)
            })
            .unwrap();
        return Ok(b["id"].as_str().unwrap_or("?").to_owned());
    }
    if hits.len() > 1 {
        hits.sort();
        hits.dedup();
        return Err(format!("{want:?} 匹配多台主机（给更长前缀）：{}", hits.join("、")));
    }
    Err(format!("没有匹配 {want:?} 的主机（host list 看全表）"))
}

// ---------- host 命令族 ----------

pub fn cmd_host(args: &[String]) {
    let Some(verb) = args.first() else {
        eprintln!("host 需要子命令：add / list / status / delete（全部可加 --state DIR）");
        std::process::exit(2);
    };
    match verb.as_str() {
        "add" => host_add(&args[1..]),
        "list" => host_list(&args[1..]),
        "status" => host_status(&args[1..]),
        "delete" => host_delete(&args[1..]),
        other => {
            eprintln!("host 不认识的子命令 {other:?}（可用：add / list / status / delete）");
            std::process::exit(2);
        }
    }
}

fn host_add(args: &[String]) {
    let p = parse_args("host add [--name N] [--force] <token> [--state DIR]", args);
    let name = p.name.clone().unwrap_or_default();
    let force = p.force;
    let state = p.state.clone();
    if p.positional.len() != 1 {
        eprintln!("host add 需要 <token>（token 形如 hmw1…，从出口日志现场获取）");
        std::process::exit(2);
    }
    let token = p.positional[0].trim().to_owned();
    // ①本地语法校验前置（spec host-cli「token 非法就地报错」：不探测、不连 daemon、不入表）。
    if let Err(e) = homeway_core::token::decode(&token) {
        eprintln!("token 非法：{e}（token 形如 hmw1…，从出口启动日志现场获取后重新粘贴）");
        std::process::exit(1);
    }
    let c = dial_control(&state);
    let v = c
        .request(
            vocab::OpName::HostAdd.as_str(),
            Some(serde_json::json!({
                "token": token,
                "name": if name.is_empty() { serde_json::Value::Null } else { serde_json::json!(name) },
                "force": force,
            })),
            HOST_ADD_TIMEOUT,
        )
        .unwrap_or_else(|e| exit_op_err(&e));
    let id = v["id"].as_str().unwrap_or("?").to_owned();
    let shown_name = v["name"].as_str().unwrap_or(&name).to_owned();
    println!("已添加 {shown_name}（id={}…）", &id[..16.min(id.len())]);
    println!(
        "验证：{}（reach.tier={}；端点未实测时首次连接补全）",
        match v["reach"]["tier"].as_str().unwrap_or("skipped") {
            "direct" => "有直连端点应答",
            "relay" => "直连全无应答、中继有应答",
            _ => "跳过探测（--force）",
        },
        v["reach"]["tier"].as_str().unwrap_or("skipped"),
    );
    if let Some(tested) = v["reach"]["tested"].as_array() {
        for t in tested {
            println!(
                "  端点 {}（{}）rtt={}ms",
                t["ep"].as_str().unwrap_or("?"),
                if t["relay"].as_bool().unwrap_or(false) { "中继" } else { "直连" },
                t["rttMs"].as_i64().unwrap_or(0),
            );
        }
    }
}

fn host_list(args: &[String]) {
    let p = parse_args("host list [--json] [--state DIR]", args);
    let json = p.json;
    let state = p.state.clone();
    let c = dial_control(&state);
    // host.list 给静态面；动态面（state/link/stats）走 snapshot.get——Go host list
    // 输出会话态/链路态，两查合一表。
    let briefs = c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    let snap = c.request(vocab::OpName::SnapshotGet.as_str(), None, TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    if json {
        println!("{}", serde_json::to_string_pretty(&snap).unwrap());
        return;
    }
    println!("{:<18} {:<10} {:<8} {:<24} {:>8} {:>12}", "ID", "NAME", "STATE", "ENDPOINT", "RTT", "RX/TX");
    let mut by_id = std::collections::HashMap::new();
    if let Some(hosts) = snap["hosts"].as_array() {
        for h in hosts {
            by_id.insert(h["id"].as_str().unwrap_or("").to_owned(), h.clone());
        }
    }
    if let Some(hosts) = briefs["hosts"].as_array() {
        for b in hosts {
            let id = b["id"].as_str().unwrap_or("");
            let h = by_id.get(id).cloned().unwrap_or(serde_json::Value::Null);
            let link = &h["link"];
            let ep = link["ep"].as_str().unwrap_or("-");
            let via = link["via"].as_str().unwrap_or("-");
            let rtt = link["rttMs"].as_i64().unwrap_or(0);
            let (rx, tx) = (
                h["stats"]["rxBytes"].as_i64().unwrap_or(0),
                h["stats"]["txBytes"].as_i64().unwrap_or(0),
            );
            println!(
                "{:<18} {:<10} {:<8} {:<24} {:>7}ms {:>6}/{:<6}",
                &id[..16.min(id.len())],
                b["name"].as_str().unwrap_or("-"),
                h["state"].as_str().unwrap_or("-"),
                format!("{via} {ep}"),
                rtt,
                fmt_bytes(rx),
                fmt_bytes(tx),
            );
        }
    }
}

fn fmt_bytes(n: i64) -> String {
    if n >= 1 << 20 {
        format!("{:.1}M", n as f64 / (1 << 20) as f64)
    } else if n >= 1 << 10 {
        format!("{:.1}K", n as f64 / (1 << 10) as f64)
    } else {
        format!("{n}")
    }
}

fn host_status(args: &[String]) {
    let p = parse_args("host status [name] [--json] [--state DIR]", args);
    let state = p.state.clone();
    let id: Option<String> = p.positional.first().cloned();
    let c = dial_control(&state);
    let snap = c.request(vocab::OpName::SnapshotGet.as_str(), None, TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    let json = p.json;
    let hosts = snap["hosts"].as_array().cloned().unwrap_or_default();
    let sel: Vec<_> = hosts
        .into_iter()
        .filter(|h| match &id {
            Some(want) => {
                let hid = h["id"].as_str().unwrap_or("");
                let hname = h["name"].as_str().unwrap_or("");
                hid == want || hid.starts_with(want.as_str()) || hname == want
            }
            None => true,
        })
        .collect();
    if sel.is_empty() {
        eprintln!("没有匹配的主机（host list 看全表）");
        std::process::exit(1);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&sel).unwrap());
        return;
    }
    for h in sel {
        println!(
            "{}（{}）state={} reason={}",
            &h["id"].as_str().unwrap_or("?")[..16.min(h["id"].as_str().unwrap_or("?").len())],
            h["name"].as_str().unwrap_or("-"),
            h["state"].as_str().unwrap_or("?"),
            h["reason"].as_str().unwrap_or("-"),
        );
        if let Some(l) = h["link"].as_object() {
            println!(
                "  link: via={} ep={} rtt={}ms at={}",
                l.get("via").and_then(|v| v.as_str()).unwrap_or("-"),
                l.get("ep").and_then(|v| v.as_str()).unwrap_or("-"),
                l.get("rttMs").and_then(|v| v.as_i64()).unwrap_or(0),
                l.get("at").and_then(|v| v.as_i64()).unwrap_or(0),
            );
        }
        if let Some(s) = h["stats"].as_object() {
            println!(
                "  stats: rx={}B tx={}B",
                s.get("rxBytes").and_then(|v| v.as_i64()).unwrap_or(0),
                s.get("txBytes").and_then(|v| v.as_i64()).unwrap_or(0),
            );
        }
    }
}

fn host_delete(args: &[String]) {
    let p = parse_args("host delete <name|id> [--yes] [--state DIR]", args);
    let state = p.state.clone();
    let yes = p.yes;
    let Some(want) = p.positional.first().cloned() else {
        eprintln!("host delete 需要 <name|id>");
        std::process::exit(2);
    };
    let c = dial_control(&state);
    // name/id 解析：空串拒绝、唯一前缀、歧义列候选、全长 64 hex 直用。
    let briefs = c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    let full = match resolve_host(&briefs, &want) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    if !yes {
        eprintln!("将删除主机 {want}（id={}…）——确认请加 --yes", &full[..16.min(full.len())]);
        std::process::exit(2);
    }
    c.request(
        vocab::OpName::HostRemove.as_str(),
        Some(serde_json::json!({"host": full})),
        TIMEOUT,
    )
    .unwrap_or_else(|e| exit_op_err(&e));
    println!("已删除 {want}（id={}…）", &full[..16.min(full.len())]);
}

// ---------- status（聚合状态面） ----------

pub fn cmd_status(args: &[String]) {
    let p = parse_args("status [--json] [--state DIR]", args);
    let json = p.json;
    let state = p.state.clone();
    let c = dial_control(&state);
    let v = c.request(vocab::OpName::DaemonStatus.as_str(), None, TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    if json {
        println!("{}", serde_json::to_string_pretty(&v).unwrap());
        return;
    }
    println!("守护进程：serverVersion={} generation={} seq={} pid={}", 
        v["serverVersion"].as_str().unwrap_or("?"),
        v["generation"].as_str().unwrap_or("?"),
        v["seq"].as_u64().unwrap_or(0),
        v["pid"].as_i64().unwrap_or(0));
    if let Some(roles) = v["roles"].as_array() {
        println!("角色：");
        for r in roles {
            println!(
                "  {:<8} {:<8} restarts={} {}",
                r["name"].as_str().unwrap_or("?"),
                r["state"].as_str().unwrap_or("?"),
                r["restarts"].as_i64().unwrap_or(0),
                r["reason"].as_str().unwrap_or(""),
            );
        }
    }
    if let Some(hosts) = v["hosts"].as_array() {
        println!("主机（{} 台）：", hosts.len());
        for h in hosts {
            let id = h["id"].as_str().unwrap_or("?");
            println!(
                "  {}… {:<10} {:<8} via={} ep={}",
                &id[..12.min(id.len())],
                h["name"].as_str().unwrap_or("-"),
                h["state"].as_str().unwrap_or("?"),
                h["link"]["via"].as_str().unwrap_or("-"),
                h["link"]["ep"].as_str().unwrap_or("-"),
            );
        }
    }
}

// ---------- serve / relay 命令组（角色管理五件对称） ----------

pub fn cmd_serve_group(args: &[String]) {
    let Some(verb) = args.first() else {
        eprintln!("serve 命令组需要动词：start / stop / restart / status / token");
        std::process::exit(2);
    };
    let op = match verb.as_str() {
        "start" => vocab::OpName::ServeStart,
        "stop" => vocab::OpName::ServeStop,
        "restart" => vocab::OpName::ServeRestart,
        "status" => vocab::OpName::ServeStatus,
        "token" => vocab::OpName::ServeToken,
        "relay" | "ddns" => {
            eprintln!("serve {verb} 归 B0-2 后续棒（serve relay set / ddns 管理）");
            std::process::exit(2);
        }
        other => {
            eprintln!("serve 不认识的动词 {other:?}（可用：start / stop / restart / status / token）");
            std::process::exit(2);
        }
    };
    let p = parse_args("serve <start|stop|restart|status|token> [--state DIR]", &args[1..]);
    let state = p.state.clone();
    if op == vocab::OpName::ServeToken {
        token_reveal_with_fallback(&state, op);
        return;
    }
    let c = dial_control(&state);
    let v = c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    match op {
        vocab::OpName::ServeStatus => {
            println!(
                "serve：enabled={} state={} listenPort={}",
                v["enabled"].as_bool().unwrap_or(false),
                v["state"].as_str().unwrap_or("?"),
                v["listenPort"].as_u64().unwrap_or(0),
            );
            if let Some(m) = v["tokenMask"].as_str() {
                println!("  token：{m}");
            }
            if let Some(eps) = v["endpoints"].as_array() {
                let eps: Vec<_> = eps.iter().filter_map(|e| e.as_str()).collect();
                if !eps.is_empty() {
                    println!("  端点：{}", eps.join("、"));
                }
            }
            println!("  peers：{}（观测面后续棒）", v["peers"].as_array().map(|a| a.len()).unwrap_or(0));
        }
        vocab::OpName::ServeToken => {
            println!("{}", v["token"].as_str().unwrap_or("?"));
            if let Some(eps) = v["endpoints"].as_array() {
                let eps: Vec<_> = eps.iter().filter_map(|e| e.as_str()).collect();
                if !eps.is_empty() {
                    eprintln!("（端点：{}；来源={}）", eps.join("、"), v["source"].as_str().unwrap_or("?"));
                }
            }
        }
        _ => println!("serve：{}", v["action"].as_str().unwrap_or("?")),
    }
}

pub fn cmd_relay_group(args: &[String]) {
    let Some(verb) = args.first() else {
        eprintln!("relay 命令组需要动词：start / stop / restart / status / token");
        std::process::exit(2);
    };
    let op = match verb.as_str() {
        "start" => vocab::OpName::RelayStart,
        "stop" => vocab::OpName::RelayStop,
        "restart" => vocab::OpName::RelayRestart,
        "status" => vocab::OpName::RelayStatus,
        "token" => vocab::OpName::RelayToken,
        other => {
            eprintln!("relay 不认识的动词 {other:?}（可用：start / stop / restart / status / token）");
            std::process::exit(2);
        }
    };
    let p = parse_args("relay <start|stop|restart|status|token> [--state DIR]", &args[1..]);
    let state = p.state.clone();
    if op == vocab::OpName::RelayToken {
        token_reveal_with_fallback(&state, op);
        return;
    }
    let c = dial_control(&state);
    let v = c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    match op {
        vocab::OpName::RelayStatus => {
            println!(
                "relay：enabled={} state={} listen={}",
                v["enabled"].as_bool().unwrap_or(false),
                v["state"].as_str().unwrap_or("?"),
                v["listen"].as_str().unwrap_or("-"),
            );
            if let Some(m) = v["tokenMask"].as_str() {
                println!("  token：{m}");
            }
        }
        vocab::OpName::RelayToken => {
            println!("{}", v["token"].as_str().unwrap_or("?"));
            eprintln!("（来源={}）", v["source"].as_str().unwrap_or("?"));
        }
        _ => println!("relay：{}", v["action"].as_str().unwrap_or("?")),
    }
}
