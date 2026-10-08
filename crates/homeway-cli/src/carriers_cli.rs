//! 承载面 CLI 族（语义真源 `baseline:internal/daemon/{forward_cli,socks_cli,
//! speedtest_cli}.go`——D-1）：`homeway forward add/list/delete`、
//! `homeway socks on/off/status`、`homeway speedtest [--host]`（守护托管形态）。
//! 全部经 control.sock 控制面往返；`--state` 恒指 daemon state；`--host` 寻址与
//! host delete/term/files 同一份 resolve 规则与文案；`--no-spawn` = 守护未跑时
//! 不按需拉起（脚本友好）。
//!
//! speedtest 的 `--token` 直连形态（matrix/perf 脚本契约）保留在 main.rs 的旧
//! 路径——本文件只承接守护托管形态（无 --token 即此路径）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::daemon::client::ControlClient;
use homeway_core::daemon::proto::OpError;
use homeway_core::daemon::vocab;

use crate::daemon_cli::{dial_control_spawn, op_err_text, resolve_host};
use crate::term_cli::{expand_flag_eq, parse_duration};

const TIMEOUT: Duration = Duration::from_secs(10);
/// status 轮询拍（Go CLI 的 250ms）。
const POLL_STEP: Duration = Duration::from_millis(250);
/// status 连续失败上限（250ms × 8 = 2s 短窗——瞬时抖动仍给重试）。
const STATUS_FAIL_STREAK: u32 = 8;

// ---------- 参数解析（承载面共通） ----------

/// 承载面命令的通用参数面（--state/--no-spawn + 带值 flag 白名单 + 位置参数）。
struct CarrierArgs {
    state: std::path::PathBuf,
    no_spawn: bool,
    json: bool,
    quiet: bool,
    values: Vec<(String, String)>,
    positional: Vec<String>,
}

impl CarrierArgs {
    fn value_of(&self, flag: &str) -> Option<&str> {
        self.values.iter().rev().find(|(k, _)| k == flag).map(|(_, v)| v.as_str())
    }

}

/// 解析（`--flag value` 与 `--flag=value` 等价；未知 flag 报错退出 2；Q-H F2：取值
/// 全走 `cli_flags` 唯一取值器——缺值/空值/吞 flag fail-fast）。
fn parse_carrier_args(usage: &str, args: &[String], value_flags: &[&str]) -> CarrierArgs {
    let mut out = CarrierArgs {
        state: crate::unified_cli::default_state_dir(),
        no_spawn: false,
        json: false,
        quiet: false,
        values: Vec::new(),
        positional: Vec::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let Some((name, inline)) = crate::cli_flags::split_flag(a) else {
            out.positional.push(a.to_owned());
            i += 1;
            continue;
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        match name {
            "state" => {
                out.state = crate::cli_flags::take_state_or_exit("state", inline, next);
                if inline.is_none() {
                    adv = 2;
                }
            }
            "no-spawn" => out.no_spawn = crate::cli_flags::take_bool_or_exit("no-spawn", inline, true),
            "json" => out.json = crate::cli_flags::take_bool_or_exit("json", inline, true),
            "quiet" => out.quiet = crate::cli_flags::take_bool_or_exit("quiet", inline, true),
            "h" | "help" => {
                // 低-5① 整改：--help 打用法退 0（此前被吞 → 子命令带缺参错误继续执行）。
                eprintln!("用法：{usage}");
                std::process::exit(0);
            }
            other => {
                if let Some(vf) = value_flags.iter().find(|f| **f == other) {
                    out.values.push((
                        (*vf).to_owned(),
                        crate::cli_flags::take_value_or_exit(other, inline, next, false),
                    ));
                    if inline.is_none() {
                        adv = 2;
                    }
                } else {
                    eprintln!("未知参数：{a}（{usage}）");
                    std::process::exit(2);
                }
            }
        }
        i += adv;
    }
    out
}

/// 连控制面（按需拉起）+ 拉主机表（daemon.status——寻址/名称映射/链路态标签共用）。
fn dial_and_hosts(
    p: &CarrierArgs,
    name: &str,
) -> (Arc<ControlClient>, serde_json::Value) {
    let c = match dial_control_spawn(&p.state, name, p.no_spawn) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    let hosts = match c.request(vocab::OpName::DaemonStatus.as_str(), None, TIMEOUT) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("homeway: daemon.status 失败：{}", op_err_text(&e));
            std::process::exit(1);
        }
    };
    (c, hosts)
}

/// hex → 显示名（无名的用短 id；表外 = 短 id）。
fn name_of_host(hosts: &serde_json::Value, hex_id: &str) -> String {
    if let Some(arr) = hosts["hosts"].as_array() {
        for h in arr {
            if h["id"].as_str() == Some(hex_id) {
                if let Some(n) = h["name"].as_str() {
                    if !n.is_empty() {
                        return n.to_owned();
                    }
                }
                break;
            }
        }
    }
    short_id(hex_id)
}

fn short_id(hex_id: &str) -> String {
    hex_id.chars().take(8).collect()
}

/// 控制面错误码 → 可行动文案（Go carrierOpErr——三命令面共用）。
fn carrier_op_err(op: &str, e: &OpError) -> String {
    let extra = {
        let d = e.detail();
        if d.is_empty() { String::new() } else { format!("：{d}") }
    };
    match e.code.as_str() {
        vocab::CODE_NO_HOST => format!(
            "主机不在守护进程表中（no_host）；homeway-cli host list 查看在表主机{extra}"
        ),
        vocab::CODE_NOT_READY => format!(
            "守护进程注册表未就绪（not_ready；client 角色启动中/重建窗口），稍后重试{extra}"
        ),
        vocab::CODE_UNKNOWN_OP => format!(
            "{op} 得 unknown_op——守护进程代际过旧（无承载面 op），请同批升级 daemon 后重试{extra}"
        ),
        _ => format!("{op} 失败：{}", op_err_text(e)),
    }
}

fn exit_carrier_op(op: &str, e: &OpError) -> ! {
    eprintln!("homeway: {}", carrier_op_err(op, e));
    std::process::exit(1);
}

// ---------- forward add / list / delete ----------

pub fn cmd_forward(args: &[String]) {
    let Some(verb) = args.first() else {
        forward_usage();
        eprintln!("homeway: forward 需要子命令：add / list / delete");
        std::process::exit(2);
    };
    match verb.as_str() {
        "add" => forward_add(&expand_flag_eq(&args[1..])),
        "list" => forward_list(&expand_flag_eq(&args[1..])),
        "delete" => forward_delete(&expand_flag_eq(&args[1..])),
        "--help" | "-h" => forward_usage(),
        other => {
            forward_usage();
            eprintln!("homeway: forward 不认识的子命令 {other:?}（可用：add / list / delete）");
            std::process::exit(2);
        }
    }
}

fn forward_usage() {
    eprintln!("用法：");
    eprintln!("  homeway-cli forward add --host <ref> --listen <P> [--target <ip:P>|:<P>] [--state D] [--no-spawn]");
    eprintln!("                     建规则并立即起监听（127.0.0.1:P；目标缺省 = 该主机出口自己同端口）");
    eprintln!("  homeway-cli forward list [--host <ref>] [--json]");
    eprintln!("                     规则表 + 运行态（listening/failed/在世连接数）");
    eprintln!("  homeway-cli forward delete --host <ref> --listen <P>");
    eprintln!("                     删规则并关监听（在世连接不强关、自然收口）");
    eprintln!("监听端口 1024–65535、每主机 ≤8 条、与全部规则及 socks 监听全局唯一。");
}

/// --target 形态：空 = 出口自己同端口；":P" = 出口自己指定端口；"ip:P" = 任意
/// IPv4 目标（Go parseForwardTarget 同义）。
fn parse_forward_target(s: &str) -> Result<(String, u16), String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok((String::new(), 0));
    }
    let Some(i) = s.rfind(':') else {
        return Err(format!(
            "--target {s:?} 须为 <ip:port> 或 :<port>（目标缺省 = 出口自己同端口）"
        ));
    };
    let port: u16 = s[i + 1..]
        .parse()
        .map_err(|e| format!("--target 端口 {:?} 非法：{e}", &s[i + 1..]))?;
    if i == 0 {
        return Ok((String::new(), port));
    }
    let ip = &s[..i];
    if ip.parse::<std::net::Ipv4Addr>().is_err() {
        return Err(format!("--target {s:?} 非法：目标须为空（出口自己）或 IPv4 字面量"));
    }
    Ok((ip.to_owned(), port))
}

fn forward_add(args: &[String]) {
    let p = parse_carrier_args(
        "forward add --host <ref> --listen <P> [--target <ip:P>|:<P>]",
        args,
        &["host", "listen", "target"],
    );
    let Some(host_ref) = p.value_of("host").map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned) else {
        eprintln!("homeway: forward add 需要 --host <ref>（homeway-cli host list 查看在表主机）");
        std::process::exit(2);
    };
    let Some(listen) = p.value_of("listen").and_then(|v| v.parse::<u16>().ok()) else {
        eprintln!("homeway: forward add 需要 --listen <端口>（1024–65535）");
        std::process::exit(2);
    };
    if listen < homeway_core::facade::portfwd::MIN_PORT {
        eprintln!("homeway: --listen {listen} 越界（监听端口须在 1024–65535，与 socks 同一条）");
        std::process::exit(2);
    }
    let (target_ip, target_port) = match p.value_of("target").map(parse_forward_target) {
        None => (String::new(), 0),
        Some(Ok(v)) => v,
        Some(Err(e)) => {
            eprintln!("homeway: {e}");
            std::process::exit(2);
        }
    };
    if !p.positional.is_empty() {
        eprintln!("homeway: forward add 不接受位置参数（got {:?}）", p.positional);
        std::process::exit(2);
    }
    let (c, hosts) = dial_and_hosts(&p, "homeway-forward");
    let briefs = match c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT) {
        Ok(v) => v,
        Err(e) => exit_carrier_op("host.list", &e),
    };
    let id = match resolve_host(&briefs, host_ref.trim()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    let dname = name_of_host(&hosts, &id);
    let req = serde_json::json!({
        "host": id,
        "listen": listen,
        "targetIp": target_ip,
        "targetPort": target_port,
    });
    let raw = match c.request(vocab::OpName::ForwardAdd.as_str(), Some(req.clone()), TIMEOUT) {
        Ok(v) => v,
        Err(e) => return forward_add_err(&c, &hosts, &id, listen, &target_ip, e),
    };
    let rule = &raw["rule"];
    let desc = describe_target_text(&target_ip, target_port, listen);
    println!(
        "已建转发 {dname} 127.0.0.1:{} → {desc}（{}，目标经 {dname} 出网）",
        rule["listen"].as_u64().unwrap_or(listen as u64),
        rule["state"].as_str().unwrap_or("listening"),
    );
    c.close();
}

/// bad_request 的现场诊断（Go forwardAddErr）：复查 forward.list + socks.status
/// 找占用方；两表都无 → 本机回环试听（守护外进程占用）；再无 → 服务端 detail。
fn forward_add_err(
    c: &Arc<ControlClient>,
    hosts: &serde_json::Value,
    _id: &str,
    listen: u16,
    _target_ip: &str,
    e: OpError,
) {
    if e.code != vocab::CODE_BAD_REQUEST {
        exit_carrier_op("forward.add", &e);
    }
    // ① 两表现场诊断（能指名占用方，最具体）。
    if let Some(owner) = find_port_owner(c, hosts, listen, "") {
        eprintln!(
            "homeway: 监听端口 {listen} 已被 {owner} 占用（forward/socks 全局唯一，非每主机）——可用 --listen 另选"
        );
        std::process::exit(1);
    }
    // ② 守护进程外占用探测：短暂在本机回环试听同端口（立即关闭）。
    if let Err(lerr) = std::net::TcpListener::bind(("127.0.0.1", listen)) {
        eprintln!(
            "homeway: 监听 127.0.0.1:{listen} 失败（bad_request；端口像是被守护进程外的本机进程占用：{lerr}）——换端口或释放后重试，规则未入表"
        );
        std::process::exit(1);
    }
    // ③ FIX-50：服务端 detail（可行动归因原文）优于自建兜底。
    let detail = e.detail();
    if !detail.is_empty() {
        eprintln!(
            "homeway: forward.add 被拒：{detail}\n（监听端口 1024–65535 且与全部规则/socks 全局唯一；规则未入表）"
        );
        std::process::exit(1);
    }
    eprintln!(
        "homeway: forward.add 被拒（bad_request；参数值域/监听失败）——核对 --listen（1024–65535，全局唯一）与 --target 形态后重试"
    );
    std::process::exit(1);
}

/// 端口占用方现场复查（forward.list + socks.status；socks 侧按「规则在册」口径
/// 连 off 记忆端口一起认；exclude_socks_host 排除请求主机自己的条目——on 的缺省
/// 解析落自己记忆端口时别报「被自己占用」）。
fn find_port_owner(
    c: &Arc<ControlClient>,
    hosts: &serde_json::Value,
    port: u16,
    exclude_socks_host: &str,
) -> Option<String> {
    if let Ok(raw) = c.request(vocab::OpName::ForwardList.as_str(), Some(serde_json::json!({})), TIMEOUT) {
        if let Some(list) = raw["forwards"].as_array() {
            for r in list {
                if r["listen"].as_u64() == Some(port as u64) {
                    return Some(format!(
                        "{} 的 forward 规则",
                        name_of_host(hosts, r["host"].as_str().unwrap_or("?"))
                    ));
                }
            }
        }
    }
    if let Ok(raw) = c.request(vocab::OpName::SocksStatus.as_str(), None, TIMEOUT) {
        if let Some(list) = raw["socks"].as_array() {
            for s in list {
                if s["listen"].as_u64() != Some(port as u64) {
                    continue;
                }
                let host = s["host"].as_str().unwrap_or("");
                if host == exclude_socks_host {
                    continue;
                }
                let name = name_of_host(hosts, host);
                return Some(if s["on"].as_bool().unwrap_or(false) {
                    format!("{name} 的 socks 监听")
                } else {
                    format!("{name} 的 socks 监听（含记忆端口）")
                });
            }
        }
    }
    None
}

/// 目标呈现文案（语义唯一源 carriers::forward::describe_target）。
fn describe_target_text(ip: &str, port: u16, listen: u16) -> String {
    homeway_core::daemon::carriers::forward::describe_target(ip, port, listen)
}

fn forward_list(args: &[String]) {
    let p = parse_carrier_args(
        "forward list [--host <ref>] [--json]",
        args,
        &["host"],
    );
    let (c, hosts) = dial_and_hosts(&p, "homeway-forward");
    let mut req = serde_json::json!({});
    if let Some(host_ref) = p.value_of("host").map(str::trim).filter(|s| !s.is_empty()) {
        let briefs = match c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT) {
            Ok(v) => v,
            Err(e) => exit_carrier_op("host.list", &e),
        };
        let id = match resolve_host(&briefs, host_ref) {
            Ok(id) => id,
            Err(e) => {
                eprintln!("homeway: {e}");
                std::process::exit(1);
            }
        };
        req = serde_json::json!({ "host": id });
    }
    let raw = match c.request(vocab::OpName::ForwardList.as_str(), Some(req), TIMEOUT) {
        Ok(v) => v,
        Err(e) => exit_carrier_op("forward.list", &e),
    };
    let forwards = raw["forwards"].as_array().cloned().unwrap_or_default();
    if p.json {
        let out: Vec<serde_json::Value> = forwards
            .iter()
            .map(|r| {
                // 低-4：omitempty 缺键回 null——Go JSON 输出零值（""/""/0），对齐。
                serde_json::json!({
                    "host": r["host"],
                    "name": name_of_host(&hosts, r["host"].as_str().unwrap_or("?")),
                    "listen": r["listen"],
                    "targetIp": r["targetIp"].as_str().unwrap_or(""),
                    "targetPort": r["targetPort"],
                    "state": r["state"],
                    "err": r["err"].as_str().unwrap_or(""),
                    "conns": r["conns"],
                    "rejected": r["rejected"].as_i64().unwrap_or(0),
                })
            })
            .collect();
        println!("{}", serde_json::to_string(&out).unwrap());
        c.close();
        return;
    }
    if forwards.is_empty() {
        println!("无转发规则（homeway-cli forward add --host <ref> --listen <P> 添加）");
        c.close();
        return;
    }
    let header = format!("{:<12} {:<7} {:<24} {:<10} {:<6} {}", "主机", "监听", "目标", "状态", "连接", "错误");
    println!("{header}");
    for r in forwards {
        let listen = r["listen"].as_u64().unwrap_or(0) as u16;
        let desc = describe_target_text(
            r["targetIp"].as_str().unwrap_or(""),
            r["targetPort"].as_u64().unwrap_or(0) as u16,
            listen,
        );
        let conns = if r["rejected"].as_i64().unwrap_or(0) > 0 {
            format!("{}(+{}拒)", r["conns"].as_i64().unwrap_or(0), r["rejected"].as_i64().unwrap_or(0))
        } else {
            format!("{}", r["conns"].as_i64().unwrap_or(0))
        };
        println!(
            "{:<12} {:<7} {:<24} {:<10} {:<6} {}",
            trunc_chars(&name_of_host(&hosts, r["host"].as_str().unwrap_or("?")), 12),
            listen,
            trunc_chars(&desc, 24),
            r["state"].as_str().unwrap_or("?"),
            conns,
            trunc_chars(r["err"].as_str().unwrap_or(""), 40),
        );
    }
    c.close();
}

fn forward_delete(args: &[String]) {
    let p = parse_carrier_args(
        "forward delete --host <ref> --listen <P>",
        args,
        &["host", "listen"],
    );
    let Some(host_ref) = p.value_of("host").map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned) else {
        eprintln!("homeway: forward delete 需要 --host <ref>（homeway-cli forward list 查看）");
        std::process::exit(2);
    };
    let Some(listen) = p.value_of("listen").and_then(|v| v.parse::<u16>().ok()) else {
        eprintln!("homeway: forward delete 需要 --listen <端口>（homeway-cli forward list 查看）");
        std::process::exit(2);
    };
    let (c, hosts) = dial_and_hosts(&p, "homeway-forward");
    let briefs = match c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT) {
        Ok(v) => v,
        Err(e) => exit_carrier_op("host.list", &e),
    };
    let id = match resolve_host(&briefs, host_ref.trim()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    let dname = name_of_host(&hosts, &id);
    let req = serde_json::json!({ "host": id, "listen": listen });
    if let Err(e) = c.request(vocab::OpName::ForwardRemove.as_str(), Some(req), TIMEOUT) {
        if e.code == vocab::CODE_BAD_REQUEST {
            // 规则不存在 vs 其它 bad_request：复查规则表现场区分（不存在报错非静默）。
            if let Ok(raw) =
                c.request(vocab::OpName::ForwardList.as_str(), Some(serde_json::json!({"host": id})), TIMEOUT)
            {
                if let Some(list) = raw["forwards"].as_array() {
                    if list.iter().any(|r| r["listen"].as_u64() == Some(listen as u64)) {
                        eprintln!(
                            "homeway: forward.delete 被拒（bad_request；该条状态 {} err={:?}）",
                            list.iter()
                                .find(|r| r["listen"].as_u64() == Some(listen as u64))
                                .and_then(|r| r["state"].as_str())
                                .unwrap_or("?"),
                            list.iter()
                                .find(|r| r["listen"].as_u64() == Some(listen as u64))
                                .and_then(|r| r["err"].as_str())
                                .unwrap_or("")
                        );
                        std::process::exit(1);
                    }
                }
            }
            eprintln!("homeway: 规则不存在：{dname} 127.0.0.1:{listen}（homeway-cli forward list 查看）");
            std::process::exit(1);
        }
        exit_carrier_op("forward.remove", &e);
    }
    println!("已删转发 {dname} 127.0.0.1:{listen}（在世连接不强关，自然收口）");
    c.close();
}

fn trunc_chars(s: &str, max: usize) -> String {
    let cs: Vec<char> = s.chars().collect();
    if cs.len() > max {
        cs[..max - 1].iter().collect::<String>() + "…"
    } else {
        s.to_owned()
    }
}

// ---------- socks on / off / status ----------

pub fn cmd_socks(args: &[String]) {
    let Some(verb) = args.first() else {
        socks_usage();
        eprintln!("homeway: socks 需要子命令：on / off / status");
        std::process::exit(2);
    };
    match verb.as_str() {
        "on" => socks_on(&expand_flag_eq(&args[1..])),
        "off" => socks_off(&expand_flag_eq(&args[1..])),
        "status" => socks_status(&expand_flag_eq(&args[1..])),
        "--help" | "-h" => socks_usage(),
        other => {
            socks_usage();
            eprintln!("homeway: socks 不认识的子命令 {other:?}（可用：on / off / status）");
            std::process::exit(2);
        }
    }
}

fn socks_usage() {
    eprintln!("用法：");
    eprintln!("  homeway-cli socks on --host <ref> [--listen 1080] [--state D] [--no-spawn]");
    eprintln!("                     该主机开 SOCKS5 监听（127.0.0.1；域名经该主机出口远程解析）");
    eprintln!("  homeway-cli socks off --host <ref>");
    eprintln!("                     关监听并显式关在世连接（端口记忆保留，下次 on 缺省沿用）");
    eprintln!("  homeway-cli socks status [--json]");
    eprintln!("                     每主机开关态 + 端口 + 在世连接数 + 链路态（via/rtt）");
    eprintln!("监听端口 1024–65535、与 forward 规则全局唯一。多主机 = 多端口，浏览器按端口选出口。");
}

fn socks_on(args: &[String]) {
    let p = parse_carrier_args("socks on --host <ref> [--listen P]", args, &["host", "listen"]);
    let Some(host_ref) = p.value_of("host").map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned) else {
        eprintln!("homeway: socks on 需要 --host <ref>（homeway-cli host list 查看在表主机）");
        std::process::exit(2);
    };
    // Q-H F7a：`--listen` 非法值 fail-fast（此前静默回落 0 = 沿用记忆端口）。
    let listen = match p.value_of("listen") {
        None => 0,
        Some(v) => match v.parse::<u16>() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("homeway: --listen {v:?} 非法（端口数字 1024–65535；缺省 = 沿用该主机记忆端口）");
                std::process::exit(2);
            }
        },
    };
    if listen != 0 && listen < homeway_core::facade::portfwd::MIN_PORT {
        eprintln!("homeway: --listen {listen} 越界（监听端口须在 1024–65535，与 forward 同一条）");
        std::process::exit(2);
    }
    let (c, hosts) = dial_and_hosts(&p, "homeway-socks");
    let briefs = match c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT) {
        Ok(v) => v,
        Err(e) => exit_carrier_op("host.list", &e),
    };
    let id = match resolve_host(&briefs, host_ref.trim()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    let dname = name_of_host(&hosts, &id);
    let req = serde_json::json!({ "host": id, "listen": listen });
    let raw = match c.request(vocab::OpName::SocksOn.as_str(), Some(req), TIMEOUT) {
        Ok(v) => v,
        Err(e) => {
            if e.code == vocab::CODE_BAD_REQUEST {
                // 冲突现场诊断（占用复查含 off 记忆端口且排除请求主机自己——防
                // 「被自己占用」的误导文案）。
                let port = if listen == 0 { remembered_socks_port(&c, &id) } else { listen };
                if let Some(owner) = find_port_owner(&c, &hosts, port, &id) {
                    eprintln!("homeway: 监听端口 {port} 已被 {owner} 占用（可用 --listen 另选端口）");
                    std::process::exit(1);
                }
                let detail = e.detail();
                if !detail.is_empty() {
                    eprintln!("homeway: socks.on 被拒：{detail}");
                    std::process::exit(1);
                }
                eprintln!("homeway: socks.on 被拒（bad_request；核对 --listen（1024–65535，与 forward 全局唯一）后重试）");
                std::process::exit(1);
            }
            exit_carrier_op("socks.on", &e);
        }
    };
    let port = raw["listen"].as_u64().unwrap_or(0) as u16;
    println!("socks 已开启：{dname} 127.0.0.1:{port}（域名经 {dname} 出口远程解析，不在本机解析）");
    c.close();
}

/// 该主机记住的端口（无记忆 = 1080；冲突诊断用——缺省 on 抢的就是这个端口）。
fn remembered_socks_port(c: &Arc<ControlClient>, host: &str) -> u16 {
    match c.request(vocab::OpName::SocksStatus.as_str(), None, TIMEOUT) {
        Ok(raw) => {
            if let Some(list) = raw["socks"].as_array() {
                for s in list {
                    if s["host"].as_str() == Some(host) {
                        let p = s["listen"].as_u64().unwrap_or(0) as u16;
                        if p != 0 {
                            return p;
                        }
                    }
                }
            }
            homeway_core::daemon::carriers::socksmgr::SOCKS_DEFAULT_LISTEN
        }
        Err(_) => homeway_core::daemon::carriers::socksmgr::SOCKS_DEFAULT_LISTEN,
    }
}

fn socks_off(args: &[String]) {
    let p = parse_carrier_args("socks off --host <ref>", args, &["host"]);
    let Some(host_ref) = p.value_of("host").map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned) else {
        eprintln!("homeway: socks off 需要 --host <ref>（homeway-cli socks status 查看）");
        std::process::exit(2);
    };
    let (c, hosts) = dial_and_hosts(&p, "homeway-socks");
    let briefs = match c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT) {
        Ok(v) => v,
        Err(e) => exit_carrier_op("host.list", &e),
    };
    let id = match resolve_host(&briefs, host_ref.trim()) {
        Ok(id) => id,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    };
    let dname = name_of_host(&hosts, &id);
    let req = serde_json::json!({ "host": id });
    let raw = match c.request(vocab::OpName::SocksOff.as_str(), Some(req), TIMEOUT) {
        Ok(v) => v,
        Err(e) => {
            if e.code == vocab::CODE_BAD_REQUEST {
                eprintln!("homeway: {dname} 没有开着的 socks 监听（homeway-cli socks status 查看）");
                std::process::exit(1);
            }
            exit_carrier_op("socks.off", &e);
        }
    };
    let listen = raw["listen"].as_u64().unwrap_or(0) as u16;
    let hint = if listen != 0 {
        format!("；端口 {listen} 记忆保留，下次 on 缺省沿用")
    } else {
        String::new()
    };
    println!("socks 已关闭：{dname}（在世连接已 RST 收口{hint}）");
    c.close();
}

fn socks_status(args: &[String]) {
    let p = parse_carrier_args("socks status [--json]", args, &[]);
    let (c, hosts) = dial_and_hosts(&p, "homeway-socks");
    let raw = match c.request(vocab::OpName::SocksStatus.as_str(), None, TIMEOUT) {
        Ok(v) => v,
        Err(e) => exit_carrier_op("socks.status", &e),
    };
    let socks = raw["socks"].as_array().cloned().unwrap_or_default();
    let link_of = |hex: &str| -> (String, i64) {
        if let Some(arr) = hosts["hosts"].as_array() {
            for h in arr {
                if h["id"].as_str() == Some(hex) {
                    return (
                        h["link"]["via"].as_str().unwrap_or("").to_owned(),
                        h["link"]["rttMs"].as_i64().unwrap_or(0),
                    );
                }
            }
        }
        (String::new(), 0)
    };
    if p.json {
        let out: Vec<serde_json::Value> = socks
            .iter()
            .map(|s| {
                let mut m = serde_json::json!({
                    "host": s["host"],
                    "name": name_of_host(&hosts, s["host"].as_str().unwrap_or("?")),
                    "on": s["on"],
                    "listen": s["listen"],
                    "conns": s["conns"],
                });
                if let Some(err) = s["err"].as_str() {
                    if !err.is_empty() {
                        m["err"] = serde_json::json!(err);
                    }
                }
                let (via, rtt) = link_of(s["host"].as_str().unwrap_or(""));
                if !via.is_empty() {
                    m["via"] = serde_json::json!(via);
                    m["rttMs"] = serde_json::json!(rtt);
                }
                m
            })
            .collect();
        println!("{}", serde_json::to_string(&out).unwrap());
        c.close();
        return;
    }
    if socks.is_empty() {
        println!("无 socks 记录（homeway-cli socks on --host <ref> 开启）");
        c.close();
        return;
    }
    let header = format!("{:<12} {:<4} {:<7} {:<6} {:<18} {}", "主机", "开关", "端口", "连接", "链路", "备注");
    println!("{header}");
    for s in socks {
        let on = if s["on"].as_bool().unwrap_or(false) { "on" } else { "off" };
        let (via, rtt) = link_of(s["host"].as_str().unwrap_or(""));
        let link = if via.is_empty() { "-".to_owned() } else { format!("{via} {rtt}ms") };
        println!(
            "{:<12} {:<4} {:<7} {:<6} {:<18} {}",
            trunc_chars(&name_of_host(&hosts, s["host"].as_str().unwrap_or("?")), 12),
            on,
            s["listen"].as_u64().unwrap_or(0),
            s["conns"].as_i64().unwrap_or(0),
            link,
            trunc_chars(s["err"].as_str().unwrap_or(""), 30),
        );
    }
    c.close();
}

// ---------- speedtest（守护托管形态） ----------

/// 轮转目标（id, 显示名）。
type SpeedTarget = (String, String);

/// Ctrl-C 的取消面（信号处理器只写原子位——async-signal-safe）。
static SPD_CANCEL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn on_spd_sig(_sig: i32) {
    SPD_CANCEL.store(true, std::sync::atomic::Ordering::SeqCst);
}

/// 一台主机的结果（--json 的逐主机对象真源——字段名冻结、只增）。
struct SpeedHostResult {
    name: String,
    hex: String,
    ok: bool,
    reason: String,
    msg: String,
    down_bps: f64,
    up_bps: f64,
    usage_down: i64,
    usage_up: i64,
    wall_ms: i64,
    via: String,
    rtt_ms: i64,
}

impl SpeedHostResult {
    fn json(&self) -> serde_json::Value {
        let mut m = serde_json::json!({
            "host": self.hex,
            "name": self.name,
            "ok": self.ok,
        });
        if !self.ok {
            m["reason"] = serde_json::json!(self.reason);
            if !self.msg.is_empty() {
                m["msg"] = serde_json::json!(self.msg);
            }
        } else {
            m["downBps"] = serde_json::json!(self.down_bps);
            m["upBps"] = serde_json::json!(self.up_bps);
            m["usageDown"] = serde_json::json!(self.usage_down);
            m["usageUp"] = serde_json::json!(self.usage_up);
            m["wallMs"] = serde_json::json!(self.wall_ms);
        }
        if !self.via.is_empty() {
            m["via"] = serde_json::json!(self.via);
            m["rttMs"] = serde_json::json!(self.rtt_ms);
        }
        m
    }
}

pub fn cmd_speedtest_hosted(args: &[String]) {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h" | "help")) {
        eprintln!("用法：homeway-cli speedtest [--host <ref>] [--json] [--down 10s] [--up 10s] [--warmup 2s] [--streams 4] [--wait 60s] [--quiet] [--state D] [--no-spawn]");
        eprintln!("  对指定主机（或全部主机顺序轮流）做隧道上下行测速；参数默认与边界 = 手机口径。");
        eprintln!("  --wait = 链路未就绪的有界等待（0 = 不等即报错；默认 60s 覆盖恢复阶梯最坏时长）。");
        eprintln!("  Ctrl-C 终止整个轮转（先取消当前主机再退出，退出码非零）；--quiet 抑制过程提示。");
        return;
    }
    let p = parse_carrier_args(
        "speedtest [--host <ref>] [--json] [--down 10s] [--up 10s] [--warmup 2s] [--streams 4] [--wait 60s] [--quiet]",
        &expand_flag_eq(args),
        &["host", "down", "up", "warmup", "streams", "wait"],
    );
    if !p.positional.is_empty() {
        eprintln!("homeway: speedtest 不接受位置参数（got {:?}）", p.positional);
        std::process::exit(2);
    }
    let dur_of = |flag: &str, default: Duration| -> Duration {
        match p.value_of(flag) {
            None => default,
            Some(v) => parse_duration(v).unwrap_or_else(|| {
                eprintln!("homeway: --{flag} {v:?} 不是时长（如 10s / 1500ms）");
                std::process::exit(2);
            }),
        }
    };
    let down = dur_of("down", Duration::from_secs(10));
    let up = dur_of("up", Duration::from_secs(10));
    let warmup = dur_of("warmup", Duration::from_secs(2));
    let streams: usize = match p.value_of("streams") {
        None => 4,
        Some(v) => match v.parse::<usize>() {
            Ok(n) => n,
            Err(_) => {
                eprintln!("homeway: --streams {v:?} 须为整数（1–6）");
                std::process::exit(2);
            }
        },
    };
    // --wait 0 = 「不等即报错」（Go 明确接受 0；parse_duration 拒零是时长通用面，
    // 此处特判）。
    let wait = match p.value_of("wait") {
        Some("0") | Some("0s") => Duration::ZERO,
        _ => dur_of("wait", Duration::from_secs(60)),
    };
    // 参数边界 = 引擎边界（手机口径；越界就地报错，不连 daemon）。
    let params = homeway_core::speedtest::Params { down, up, warmup, streams };
    if let Err(e) = params.normalized() {
        eprintln!("homeway: 参数越界（{e}）——窗口 ≤15s、预热 ≤5s、流数 1–6（手机口径同边界）");
        std::process::exit(2);
    }

    let (c, hosts) = dial_and_hosts(&p, "homeway-speedtest");
    // 寻址与轮转名单。
    let host_arr = hosts["hosts"].as_array().cloned().unwrap_or_default();
    let targets: Vec<(String, String, String, i64)> = match p.value_of("host").map(str::trim).filter(|s| !s.is_empty()) {
        Some(r) => {
            let briefs = match c.request(vocab::OpName::HostList.as_str(), None, TIMEOUT) {
                Ok(v) => v,
                Err(e) => exit_carrier_op("host.list", &e),
            };
            let id = match resolve_host(&briefs, r) {
                Ok(id) => id,
                Err(e) => {
                    eprintln!("homeway: {e}");
                    std::process::exit(1);
                }
            };
            let h = host_arr.iter().find(|h| h["id"].as_str() == Some(&id)).cloned();
            let name = h
                .as_ref()
                .and_then(|h| h["name"].as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| short_id(&id));
            vec![(id, name, String::new(), 0)]
        }
        None => {
            if host_arr.is_empty() {
                eprintln!("homeway: 主机表为空——先 homeway-cli host add <token> 添加后端，再测速");
                std::process::exit(1);
            }
            host_arr
                .iter()
                .map(|h| {
                    (
                        h["id"].as_str().unwrap_or("?").to_owned(),
                        h["name"].as_str().map(str::to_owned).unwrap_or_else(|| {
                            short_id(h["id"].as_str().unwrap_or("?"))
                        }),
                        h["link"]["via"].as_str().unwrap_or("").to_owned(),
                        h["link"]["rttMs"].as_i64().unwrap_or(0),
                    )
                })
                .collect()
        }
    };
    // via/rtt 从 daemon.status 拉全（--host 单台也从表取）。
    let via_rtt_of = |hex: &str| -> (String, i64) {
        for h in &host_arr {
            if h["id"].as_str() == Some(hex) {
                return (
                    h["link"]["via"].as_str().unwrap_or("").to_owned(),
                    h["link"]["rttMs"].as_i64().unwrap_or(0),
                );
            }
        }
        (String::new(), 0)
    };

    if !p.quiet {
        if targets.len() > 1 {
            let names: Vec<String> = targets.iter().map(|(_, n, _, _)| n.clone()).collect();
            eprintln!("将依次测 {} 台（顺序执行，互不并行）：{}", targets.len(), names.join("、"));
        } else {
            eprintln!("将依次测 1 台：{}", targets[0].1);
        }
    }

    // Ctrl-C：终止整个轮转（先 cancel 当前主机再退出、退出码非零）。
    unsafe {
        libc::signal(libc::SIGINT, on_spd_sig as extern "C" fn(i32) as libc::sighandler_t);
    }

    let mut results: Vec<SpeedHostResult> = Vec::new();
    let mut interrupted = false;
    for (id, name, _, _) in &targets {
        if SPD_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
            interrupted = true;
            break;
        }
        let (via, rtt) = via_rtt_of(id);
        let res = run_speed_host(
            &c,
            &(id.clone(), name.clone()),
            params,
            wait,
            &p,
            (via.clone(), rtt),
        );
        match res {
            Ok(mut r) => {
                r.via = via;
                r.rtt_ms = rtt;
                if !p.json {
                    print_speed_result(&r);
                }
                results.push(r);
            }
            Err(msg) => {
                // Ctrl-C 落在当前主机：run 内已发 cancel——退出轮转。
                if SPD_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
                    interrupted = true;
                    break;
                }
                let r = SpeedHostResult {
                    name: name.clone(),
                    hex: id.clone(),
                    ok: false,
                    reason: "interrupted".to_owned(),
                    msg,
                    down_bps: 0.0,
                    up_bps: 0.0,
                    usage_down: 0,
                    usage_up: 0,
                    wall_ms: 0,
                    via,
                    rtt_ms: rtt,
                };
                if !p.json {
                    print_speed_result(&r);
                }
                results.push(r);
            }
        }
    }
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
    if interrupted {
        c.close();
        eprintln!("homeway: 已按 Ctrl-C 终止轮转（当前主机已取消；未测的主机不再测量）");
        std::process::exit(1);
    }
    if p.json {
        let out: Vec<serde_json::Value> = results.iter().map(SpeedHostResult::json).collect();
        println!("{}", serde_json::to_string(&out).unwrap());
    }
    c.close();
    // 退出码（Go 拍板①）：全失败非零；至少一台成功 = 0。
    if results.iter().all(|r| !r.ok) {
        eprintln!("homeway: 全部主机测速失败（短因见上）");
        std::process::exit(1);
    }
}

/// start 在途的取消守卫（中-6，Go `startSent && !settled` defer 同义）：一切中途
/// 返回路径（status 连败/运行面丢失/错误分支）都补发 speedtest.cancel——用户立刻
/// 重试不再撞 busy 到预算烧满。终态/busy/显式取消的路径置 settled 免补发。
struct CancelGuard<'a> {
    c: &'a Arc<ControlClient>,
    host: &'a str,
    settled: bool,
}

impl Drop for CancelGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            let _ = self.c.request(
                vocab::OpName::SpeedtestCancel.as_str(),
                Some(serde_json::json!({ "host": self.host })),
                TIMEOUT,
            );
        }
    }
}

/// 一台主机的完整轮次：start（waitMs 载荷）→ 250ms 轮询 status 到终态；
/// Ctrl-C/超预算 = 先 cancel 再返回错误。via/rtt 开跑时冻结（daemon.status 的链路态）。
fn run_speed_host(
    c: &Arc<ControlClient>,
    host: &SpeedTarget,
    params: homeway_core::speedtest::Params,
    wait: Duration,
    p: &CarrierArgs,
    link: (String, i64),
) -> Result<SpeedHostResult, String> {
    let (id, name) = (&host.0, &host.1);
    let (via, rtt) = link;
    let mut _cg = CancelGuard { c, host: id, settled: true }; // start 发出前不补发
    let cancel_speed_host = |_why: &str| {
        let _ = c.request(
            vocab::OpName::SpeedtestCancel.as_str(),
            Some(serde_json::json!({ "host": id })),
            TIMEOUT,
        );
    };
    let req = serde_json::json!({
        "host": id,
        "downMs": params.down.as_millis() as i64,
        "upMs": params.up.as_millis() as i64,
        "warmupMs": params.warmup.as_millis() as i64,
        "streams": params.streams as i64,
        "waitMs": wait.as_millis() as i64,
    });
    let raw = match c.request(vocab::OpName::SpeedtestStart.as_str(), Some(req), TIMEOUT) {
        Ok(v) => {
            _cg.settled = false; // start 已受理：从此一切中途返回都要补发 cancel
            v
        }
        Err(e) => {
            if SPD_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("已取消".to_owned());
            }
            // no_host/unknown_op 等控制面错误：直接成为该台失败短因（轮转继续；
            // start 未受理——daemon 侧无我们的轮，免补发）。
            _cg.settled = true;
            return Ok(SpeedHostResult {
                name: name.to_owned(),
                hex: id.to_owned(),
                ok: false,
                reason: "control_error".to_owned(),
                msg: carrier_op_err("speedtest.start", &e),
                down_bps: 0.0,
                up_bps: 0.0,
                usage_down: 0,
                usage_up: 0,
                wall_ms: 0,
                via,
                rtt_ms: rtt,
            });
        }
    };
    if raw["phase"].as_str() == Some("busy") {
        _cg.settled = true; // busy = 另一会话的轮在跑——cancel 会误伤
        return Ok(SpeedHostResult {
            name: name.to_owned(),
            hex: id.to_owned(),
            ok: false,
            reason: "busy".to_owned(),
            msg: "并发满员（同主机已有测速在跑或与手机撞出口上限），稍后再试".to_owned(),
            down_bps: 0.0,
            up_bps: 0.0,
            usage_down: 0,
            usage_up: 0,
            wall_ms: 0,
            via,
            rtt_ms: rtt,
        });
    }
    if !p.quiet {
        let label = if via.is_empty() {
            String::new()
        } else {
            format!("（via={via} rtt={rtt}ms，开跑时冻结）")
        };
        eprintln!("▶ {name}{label}");
    }

    // 轮询预算：wait + 引擎总预算（60s 固定 + 参数）+ 10s 余量（MUST NOT 无限转圈）。
    let poll_budget = wait + Duration::from_secs(60) + params.down + params.up + params.warmup + Duration::from_secs(10);
    let deadline = Instant::now() + poll_budget;
    let mut last_hint = Instant::now() - Duration::from_secs(2);
    let mut fail_streak = 0u32;
    let mut last_bytes: i64 = 0;
    let mut last_at = Instant::now();
    loop {
        if SPD_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
            cancel_speed_host("ctrl-c");
            return Err("已按 Ctrl-C 取消".to_owned());
        }
        if Instant::now() > deadline {
            cancel_speed_host("预算耗尽");
            return Err("status 轮询超预算（MUST NOT 无限转圈）——已取消该主机".to_owned());
        }
        let st = match c.request(
            vocab::OpName::SpeedtestStatus.as_str(),
            Some(serde_json::json!({ "host": id })),
            TIMEOUT,
        ) {
            Ok(v) => v,
            Err(e) => {
                if SPD_CANCEL.load(std::sync::atomic::Ordering::SeqCst) {
                    return Err("已取消".to_owned());
                }
                fail_streak += 1;
                if fail_streak >= STATUS_FAIL_STREAK {
                    // 守卫在 drop 补发 cancel（中-6：status 连败路径同样清掉 daemon
                    // 侧可能在跑的轮——用户重试不再撞 busy）。
                    return Ok(SpeedHostResult {
                        name: name.to_owned(),
                        hex: id.to_owned(),
                        ok: false,
                        reason: "control_error".to_owned(),
                        msg: format!("status 连续 {fail_streak} 次失败：{}", op_err_text(&e)),
                        down_bps: 0.0,
                        up_bps: 0.0,
                        usage_down: 0,
                        usage_up: 0,
                        wall_ms: 0,
                        via,
                        rtt_ms: rtt,
                    });
                }
                std::thread::sleep(POLL_STEP);
                continue;
            }
        };
        fail_streak = 0;
        // Map 索引对缺键 panic（Value 索引才回落 Null）——omitempty 的键一律 get。
        if let Some(r) = st["result"].as_object() {
            _cg.settled = true; // 终态——daemon 侧已收场，不动
            let g = |k: &str| r.get(k).cloned().unwrap_or(serde_json::Value::Null);
            return Ok(SpeedHostResult {
                name: name.to_owned(),
                hex: id.to_owned(),
                ok: g("ok").as_bool().unwrap_or(false),
                reason: g("reason").as_str().unwrap_or("").to_owned(),
                msg: g("msg").as_str().unwrap_or("").to_owned(),
                down_bps: g("downBps").as_f64().unwrap_or(0.0),
                up_bps: g("upBps").as_f64().unwrap_or(0.0),
                usage_down: g("usageDown").as_i64().unwrap_or(0),
                usage_up: g("usageUp").as_i64().unwrap_or(0),
                wall_ms: g("wallMs").as_i64().unwrap_or(0),
                via,
                rtt_ms: rtt,
            });
        }
        // 本台已 start 过——phase=idle 且无终态 = 运行面丢失（daemon 重启等）：
        // 快速失败，不烧满轮询预算。
        if !st["waiting"].as_bool().unwrap_or(false)
            && st["result"].is_null()
            && st["phase"].as_str() == Some("idle")
        {
            return Ok(SpeedHostResult {
                name: name.to_owned(),
                hex: id.to_owned(),
                ok: false,
                reason: "interrupted".to_owned(),
                msg: "运行面丢失（守护进程重启？）——本轮测速已不在，请重试".to_owned(),
                down_bps: 0.0,
                up_bps: 0.0,
                usage_down: 0,
                usage_up: 0,
                wall_ms: 0,
                via,
                rtt_ms: rtt,
            });
        }
        if !p.quiet && last_hint.elapsed() >= Duration::from_secs(1) {
            last_hint = Instant::now();
            if st["waiting"].as_bool().unwrap_or(false) {
                eprintln!(
                    "  等待链路就绪（剩余 {}s）……",
                    st["waitRemainMs"].as_i64().unwrap_or(0) / 1000
                );
            } else {
                let bytes = st["bytes"].as_i64().unwrap_or(0);
                let now = Instant::now();
                let dt = now.duration_since(last_at).as_secs_f64();
                if dt > 0.0 && bytes > last_bytes {
                    let bps = (bytes - last_bytes) as f64 / dt;
                    if bps > 0.0 {
                        eprintln!("  {} {}", speed_dir_text(st["phase"].as_str().unwrap_or("")), human_rate(bps));
                        last_bytes = bytes;
                        last_at = now;
                        std::thread::sleep(POLL_STEP);
                        continue;
                    }
                }
                eprintln!("  {}……", speed_phase_text(st["phase"].as_str().unwrap_or("")));
            }
        }
        std::thread::sleep(POLL_STEP);
    }
}

/// 一台主机的双口径输出（人面；--json 走 SpeedHostResult::json）。
fn print_speed_result(r: &SpeedHostResult) {
    if !r.ok {
        println!("✗ {}：{}（{}）", r.name, speed_short_reason(&r.reason), r.reason);
        return;
    }
    println!("✓ {}：↑{} ↓{}", r.name, human_rate(r.up_bps), human_rate(r.down_bps));
    println!(
        "  精确值：down={:.0}B/s（{:.2}Mbps，{:.2}MB/s） up={:.0}B/s（{:.2}Mbps，{:.2}MB/s） 用量 ↓{} ↑{} 墙钟 {:.1}s",
        r.down_bps,
        r.down_bps * 8.0 / 1e6,
        r.down_bps / 1048576.0,
        r.up_bps,
        r.up_bps * 8.0 / 1e6,
        r.up_bps / 1048576.0,
        human_bytes(r.usage_down),
        human_bytes(r.usage_up),
        r.wall_ms as f64 / 1000.0,
    );
}

/// 手机显示口径（1024 进位、≥1MB/s 用 MB/s 不足用 KB/s、四舍五入整数——进位到
/// 1024KB 的值归 MB 档；单位顺序由调用方排〔上行在前〕）。CLI 与 App 同一口径。
fn human_rate(bps: f64) -> String {
    let kb = bps / 1024.0;
    let rk = kb.round();
    if rk >= 1024.0 {
        format!("{}MB/s", (rk / 1024.0) as i64)
    } else {
        format!("{}KB/s", rk as i64)
    }
}

fn human_bytes(n: i64) -> String {
    if n >= 1024 * 1024 * 1024 {
        format!("{:.2}GB", n as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if n >= 1024 * 1024 {
        format!("{:.2}MB", n as f64 / (1024.0 * 1024.0))
    } else if n >= 1024 {
        format!("{:.1}KB", n as f64 / 1024.0)
    } else {
        format!("{n}B")
    }
}

/// 失败短因（与手机 SpeedTestRules 的短因族对齐的 CLI 版文案）。
fn speed_short_reason(reason: &str) -> &'static str {
    match reason {
        homeway_core::speedtest::REASON_BUSY => "并发满员，稍后再试",
        homeway_core::speedtest::REASON_LINK_DOWN => "链路未就绪（恢复中）",
        homeway_core::speedtest::REASON_NOT_SUPPORTED => "出口没有测速服务（出口需升级）",
        homeway_core::speedtest::REASON_INTERRUPTED => "通道错误",
        homeway_core::speedtest::REASON_TIMEOUT => "超时",
        homeway_core::speedtest::REASON_CANCELLED => "已取消",
        "control_error" => "控制面错误",
        _ => "失败",
    }
}

fn speed_dir_text(dir: &str) -> &'static str {
    if dir == "up" { "上行中" } else { "下行中" }
}

fn speed_phase_text(phase: &str) -> &'static str {
    match phase {
        "waiting" => "等待链路就绪",
        "connecting" => "建立连接",
        "down" => "下行中",
        "up" => "上行中",
        _ => "测速中",
    }
}
