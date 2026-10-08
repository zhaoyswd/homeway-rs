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

use crate::cli_flags;
use crate::unified_cli::default_state_dir;

const TIMEOUT: Duration = Duration::from_secs(10);

/// host.add 的请求预算（Go 缺省 10s；Rust 侧同键刷新的会话停止有界 6s〔patrol
/// 阻塞探针不可中断——join 烧满 STOP_WAIT〕+ 探测 3.5s + 余量 ⇒ 放宽到 20s。
/// spec 的 ≤3.5s MUST 约束的是服务端**探测**预算，不是 CLI 请求预算）。
const HOST_ADD_TIMEOUT: Duration = Duration::from_secs(20);

/// 控制面错误 → 人类可读文案（码不进契约，文案按码分支——Go host_cli 同纪律；
/// term/files `--host` 远程模式共用同一文案——r2-17 收敛：单一定义）。
pub fn op_err_text(e: &OpError) -> String {
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

/// host/status/serve 组/relay 组的拨号面：按需拉起（Go dialControlSpawn——客户端
/// 域命令共用；未运行 = 拉起统一进程并重拨，--no-spawn 走 fail-fast）。
fn dial_control(state_dir: &std::path::Path, no_spawn: bool) -> Arc<ControlClient> {
    match dial_control_spawn(state_dir, "homeway", no_spawn) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("homeway: {e}");
            std::process::exit(1);
        }
    }
}

/// 裸 socket 三态探测的「未运行」出口（term/files --host 的拉起分流共用）。
pub fn sock_not_running(sock: &std::path::Path) -> bool {
    matches!(probe_sock(sock), SockState::NotRunning)
}

// ---------- 按需拉起（Go internal/daemon/spawn.go——D-1 实装） ----------

/// 拉起节拍（Go design D4：KeepAlive 等待 3–5s、就绪上限 10s）。
const SPAWN_KEEPALIVE_WAIT: Duration = Duration::from_secs(4);
const SPAWN_READY_TIMEOUT: Duration = Duration::from_secs(10);
const SPAWN_POLL_STEP: Duration = Duration::from_millis(100);
/// 就绪超时错误里回显的 spawn.log 尾部行数。
const SPAWN_LOG_TAIL_LINES: usize = 12;

/// 裸 socket 探测三态（dial 失败后的分流依据）。
enum SockState {
    /// 裸连成功（握手面失败——协议/超时类，不按未运行处理）。
    Alive,
    /// ENOENT（socket 不存在）/ ECONNREFUSED（残留 socket 的监听者已消失）。
    NotRunning,
    /// 其它（权限等）。
    Unreachable,
}

fn probe_sock(sock: &std::path::Path) -> SockState {
    match std::os::unix::net::UnixStream::connect(sock) {
        Ok(_) => SockState::Alive,
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused => {
                SockState::NotRunning
            }
            _ => SockState::Unreachable,
        },
    }
}

/// `<state>/lock` 的 flock 试探（补-3 收敛：实现收敛到 nodestate::probe_lock_holder
/// ——此前这里有一份逐字段重复的解析实现）。
fn lock_held_probe(state_dir: &std::path::Path) -> (bool, i32, String) {
    homeway_core::nodestate::probe_lock_holder(state_dir)
}

/// control.sock 可连性轮询（dial 成功即回 true 并关闭）。
fn wait_control_ready(state_dir: &std::path::Path, d: Duration) -> bool {
    let sock = state_dir.join("control.sock");
    let deadline = std::time::Instant::now() + d;
    while std::time::Instant::now() < deadline {
        if let Ok(c) = std::os::unix::net::UnixStream::connect(&sock) {
            drop(c);
            return true;
        }
        std::thread::sleep(SPAWN_POLL_STEP);
    }
    false
}

/// launchd 托管形态检测（darwin）：LaunchAgents 目录里任意 homeway 代理 plist
/// （按文件名泛化——模板 label 与现役 label 都覆盖）。命中返回 label；无 = None
/// （Linux/未安装场景恒 None——直接自 exec）。
fn detect_launchd_agent() -> Option<String> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let home = std::env::var("HOME").ok()?;
    let dirs = [
        std::path::PathBuf::from(&home).join("Library/LaunchAgents"),
        std::path::PathBuf::from("/Library/LaunchAgents"),
    ];
    for d in dirs {
        let Ok(ents) = std::fs::read_dir(&d) else { continue };
        for e in ents.flatten() {
            let n = e.file_name().to_string_lossy().to_string();
            if n.contains("homeway") && n.ends_with(".plist") {
                return Some(n.trim_end_matches(".plist").to_owned());
            }
        }
    }
    None
}

/// 是否应在「未运行」时等 launchd KeepAlive 重拉（Q-H F14）：**仅默认 state**。
///
/// 依据：临时/自定义 state 的探测会命中**别的**部署形态的 launchd 代理（KeepAlive
/// 触发会把那份部署拉起）——非默认 state 直接自拉起（plist 级精确匹配〔多实例/
/// 多 label/自定义 state 的 launchd 形态〕留 Q-J，登记见 `AUDIT` Q-J 节）。
pub(crate) fn launchd_relaunch_relevant(state: &std::path::Path) -> bool {
    state == default_state_dir()
}

/// 拉起子进程 stdio 落点（`<state>/cache/spawn.log`，追加 + 0600）。
fn spawn_log_path(state_dir: &std::path::Path) -> std::path::PathBuf {
    state_dir.join("cache/spawn.log")
}

fn spawn_log_tail(state_dir: &std::path::Path) -> String {
    match std::fs::read_to_string(spawn_log_path(state_dir)) {
        Ok(s) => {
            let lines: Vec<&str> = s.trim_end_matches('\n').split('\n').collect();
            let from = lines.len().saturating_sub(SPAWN_LOG_TAIL_LINES);
            lines[from..].join("\n")
        }
        Err(_) => "（spawn.log 尚无内容/不可读）".to_owned(),
    }
}

/// 自 exec 拉起统一进程：同二进制 + `--state` 透传 + setsid（脱离会话——CLI 退出
/// 不带走它）+ stdio → `<state>/cache/spawn.log`（子进程失败原因发生在启动早期，
/// 接 /dev/null 会让「就绪超时」拿不到失败原因）。返回子进程 pid（不 wait——
/// setsid 后由 init 收养）。
fn start_spawned_process(state_dir: &std::path::Path) -> Result<u32, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let log = spawn_log_path(state_dir);
    if let Some(dir) = log.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("建 {}: {e}", dir.display()))?;
    }
    // Q-G F4（A8）：**创建即 0600**（`.mode` + 拿到 handle 后 fchmod 归一，防 umask
    // 掩码）——旧形态是「默认权限建 → 静默 chmod」，存在短窗口且失败无告警。
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let mut lf = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(&log)
        .map_err(|e| e.to_string())?;
    // 失败**告警不阻断**（代码门②：与 state.rs 的告警形态一致；spawn 日志不含凭据）
    if let Err(e) = lf.set_permissions(std::fs::Permissions::from_mode(0o600)) {
        eprintln!("homeway: ⚠️ spawn 日志 {} 收紧 0600 失败（{e}）——建议手工 chmod", log.display());
    }
    // 分隔行（Go spawn.go 同形态——多次拉起的失败原因累积可查）。
    {
        use std::io::Write as _;
        let _ = writeln!(
            lf,
            "---- spawn {} ----",
            chrono_like_now()
        );
    }
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("--state").arg(state_dir).stdin(std::process::Stdio::null());
    cmd.stdout(lf.try_clone().map_err(|e| e.to_string())?);
    cmd.stderr(lf);
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    use std::os::unix::process::CommandExt as _;
    let child = cmd.spawn().map_err(|e| e.to_string())?;
    Ok(child.id())
}

/// 按需拉起的统一拨号缝（Go dialControlSpawn——客户端域命令共用）：先试拨
/// control.sock；失败且判定「未运行」→ 拉起（提示行走 stderr）→ 就绪后重拨。
/// `--no-spawn`（脚本友好）= fail-fast 可行动错误；「锁被持有但 socket 未就绪」
/// = 另一进程启动窗口——有界等就绪复用。
pub fn dial_control_spawn(
    state_dir: &std::path::Path,
    name: &str,
    no_spawn: bool,
) -> Result<Arc<ControlClient>, String> {
    let sock = state_dir.join("control.sock");
    let first = ControlClient::dial(&sock, "cli", name);
    if let Ok((c, _)) = first {
        return Ok(c);
    }
    let dial_err = first.err().expect("Ok 分支已返回");
    match probe_sock(&sock) {
        SockState::Alive | SockState::Unreachable => {
            // 握手/权限面失败：不按未运行处理——可行动错误。
            Err(format!(
                "连不上 {}：{dial_err}（协议不匹配或守护进程异常；核对版本与 --state 指向）",
                sock.display()
            ))
        }
        SockState::NotRunning => {
            // 「未运行族 + 锁被持有」= 另一进程刚取锁、control.sock 尚未就绪的启动
            // 窗口（两个 CLI 同时冷启动的竞态）——有界等就绪后重拨复用。
            let (held, pid, form) = lock_held_probe(state_dir);
            if held {
                if wait_control_ready(state_dir, SPAWN_READY_TIMEOUT) {
                    if let Ok((c, _)) = ControlClient::dial(&sock, "cli", name) {
                        return Ok(c);
                    }
                }
                return Err(format!(
                    "另一 homeway 进程（pid {pid}，形态 {form}）正持有 state 锁，但控制面 {}s 内未就绪（该进程可能正在启动或卡住）\nstate={}\n可稍后重试；卡住时先停该进程再试",
                    SPAWN_READY_TIMEOUT.as_secs(),
                    state_dir.display()
                ));
            }
            if no_spawn {
                return Err(format!(
                    "守护进程未运行且 --no-spawn 已给定（不拉起）\nsock={}\n先手动启动：homeway-cli --state {}（零参统一进程；Ctrl-C 收工）",
                    sock.display(),
                    state_dir.display()
                ));
            }
            // launchd 托管形态（Q-H F14）：**只在默认 state** 下先短轮询等 KeepAlive
            // 重拉（CLI 自 exec 出的进程不归 launchd 管，KeepAlive 会反复重拉自己的
            // 实例撞锁）；非默认 state = 不等（防临时 state 触发别的部署的 KeepAlive）。
            if launchd_relaunch_relevant(state_dir) {
                if let Some(label) = detect_launchd_agent() {
                    eprintln!("守护进程未运行（launchd 代理 {label} 在册）——等 KeepAlive 重拉…");
                    if wait_control_ready(state_dir, SPAWN_KEEPALIVE_WAIT) {
                        if let Ok((c, _)) = ControlClient::dial(&sock, "cli", name) {
                            return Ok(c);
                        }
                    }
                    eprintln!("KeepAlive {}s 内未重拉——改为自行拉起", SPAWN_KEEPALIVE_WAIT.as_secs());
                }
            } else {
                eprintln!(
                    "（state={} 非默认 state——不等 launchd KeepAlive，直接拉起）",
                    state_dir.display()
                );
            }
            let pid = start_spawned_process(state_dir)
                .map_err(|e| format!("拉起统一进程失败：{e}"))?;
            eprintln!("守护进程未运行，已启动 pid={pid}（state={}）", state_dir.display());
            if !wait_control_ready(state_dir, SPAWN_READY_TIMEOUT) {
                return Err(format!(
                    "拉起的统一进程 {}s 内未就绪（control.sock 未出现/未可连）\n{} 尾部：\n{}\n完整日志：{}",
                    SPAWN_READY_TIMEOUT.as_secs(),
                    spawn_log_path(state_dir).display(),
                    spawn_log_tail(state_dir),
                    spawn_log_path(state_dir).display()
                ));
            }
            // 就绪后重拨（新预算：原预算可能已在首拨里烧掉大半）。
            ControlClient::dial(&sock, "cli", name).map(|(c, _)| c).map_err(|e| {
                format!("拉起后重拨失败（{}）：{e}", sock.display())
            })
        }
    }
}

/// 纯读拨号（不拉起——中-5）：连通则 Some。
fn try_dial_control_named(state_dir: &std::path::Path, name: &str) -> Option<Arc<ControlClient>> {
    let sock = state_dir.join("control.sock");
    match ControlClient::dial(&sock, "cli", name) {
        Ok((c, _)) => Some(c),
        Err(_) => None,
    }
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
    /// 守护未跑时不按需拉起（脚本友好；Go cliBoolFlags 共享位）。
    no_spawn: bool,
    /// 从 stdin 读单行（serve relay set 的凭证输入面——缓解 shell history 落凭证）。
    stdin: bool,
}

fn parse_args(usage: &str, args: &[String]) -> ParsedArgs {
    let mut out = ParsedArgs {
        state: default_state_dir(),
        positional: Vec::new(),
        json: false,
        yes: false,
        force: false,
        name: None,
        no_spawn: false,
        stdin: false,
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let Some((name, inline)) = cli_flags::split_flag(a) else {
            out.positional.push(a.to_owned());
            i += 1;
            continue;
        };
        let next = args.get(i + 1).map(String::as_str);
        let mut adv = 1usize;
        match name {
            "state" => {
                out.state = cli_flags::take_state_or_exit("state", inline, next);
                if inline.is_none() {
                    adv = 2;
                }
            }
            "json" => out.json = cli_flags::take_bool_or_exit("json", inline, true),
            "yes" => out.yes = cli_flags::take_bool_or_exit("yes", inline, true),
            "stdin" => out.stdin = cli_flags::take_bool_or_exit("stdin", inline, true),
            "force" => out.force = cli_flags::take_bool_or_exit("force", inline, true),
            "no-spawn" => out.no_spawn = cli_flags::take_bool_or_exit("no-spawn", inline, true),
            "name" => {
                out.name = Some(cli_flags::take_value_or_exit("name", inline, next, false));
                if inline.is_none() {
                    adv = 2;
                }
            }
            "h" | "help" => {
                // Q-H F8/CA13：`--help`/`-h` = 用法 + exit 0（此前静默吞掉、动作照跑
                // ——`serve stop --help` 会真停出口）。
                eprintln!("用法：{usage}");
                std::process::exit(0);
            }
            other => {
                if a.starts_with('-') {
                    eprintln!("未知参数：{a}（{usage}）");
                    std::process::exit(2);
                }
                out.positional.push(other.to_owned());
            }
        }
        i += adv;
    }
    out
}

/// 主机寻址（Go resolveHostTarget 同义：空串拒绝、名称/唯一前缀、歧义列候选、
/// 全长 64 hex 必须在表内——Q-H F10：CLI-2 整改的 `starts_with("")` 恒真曾可静默删
/// 第一台；F10 起 64-hex 分支改「解码 → 小写 canonical → 与表内 id 比对」，
/// 表外/大写未命中 = **就地报错**（Go `host_cli.go:419-427`：全长 64 hex 必须与
/// 表内 ID 精确相等，否则 `主机 %s 不存在`）；term/files `--host` 与 host delete
/// 共用同一份规则文案）。
pub fn resolve_host(briefs: &serde_json::Value, want: &str) -> Result<String, String> {
    if want.is_empty() {
        return Err("寻址串为空（给 name、id 前缀或全长 id）".to_owned());
    }
    let hosts = briefs["hosts"].as_array().cloned().unwrap_or_default();
    if want.len() == 64 && want.bytes().all(|c| c.is_ascii_hexdigit()) {
        // canonical 小写（大小写等价；承载面 F11 同规则）——命中返回**表内** id。
        let canon = homeway_core::daemon::hosts::decode_peer_id_pub(want).map(|id| {
            id.iter().map(|b| format!("{b:02x}")).collect::<String>()
        });
        if let Some(c) = canon {
            if let Some(b) = hosts.iter().find(|b| {
                b["id"]
                    .as_str()
                    .map(|s| s.eq_ignore_ascii_case(&c))
                    .unwrap_or(false)
            }) {
                return Ok(b["id"].as_str().unwrap_or(&c).to_owned());
            }
        }
        return Err(format!("没有匹配 {want:?} 的主机（host list 看全表）"));
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
        // Q-H F8/CA13：裸名词 --help 短路（此前落「不认识的子命令」exit 2）。
        "--help" | "-h" => {
            eprintln!("用法：homeway-cli host <add|list|status|delete> …");
            eprintln!("  add [--name N] [--force] <token> / list [--json] / status [name] / delete <name|id> [--yes]（均可 --state DIR --no-spawn）");
            std::process::exit(0);
        }
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
    let c = dial_control(&state, p.no_spawn);
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
    let c = dial_control(&state, p.no_spawn);
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
    let c = dial_control(&state, p.no_spawn);
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
    let c = dial_control(&state, p.no_spawn);
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
    // --watch 只属 status（评审 r2-14：进共享 flag 表会让 host list --watch 被静默
    // 吞——先本地剥再走共享解析）。Q-H F7b：`--watch=false` 真生效（此前 =true 恒开）。
    let mut watch = false;
    let mut rest: Vec<String> = Vec::new();
    for a in args {
        if let Some((name, inline)) = cli_flags::split_flag(a) {
            if name == "watch" {
                watch = cli_flags::take_bool_or_exit("watch", inline, true);
                continue;
            }
        }
        rest.push(a.clone());
    }
    let p = parse_args("status [--json] [--watch] [--state DIR]", &rest);
    let json = p.json;
    let state = p.state.clone();
    if watch {
        if json {
            eprintln!("homeway: --json 与 --watch 互斥（--watch = 人类可读 live 渲染；机器可读消费走 --json）");
            std::process::exit(1);
        }
        status_watch(&state);
        return;
    }
    // 纯读不拉起（中-5——Go status_cli 同义：未跑 = 降级呈现，不起进程）。
    let Some(c) = try_dial_control_named(&state, "homeway") else {
        println!(
            "守护进程：未运行（sock={}/control.sock 不存在/不可连）——本命令纯读不拉起；先启动：homeway-cli --state {}（零参统一进程）",
            state.display(),
            state.display()
        );
        return;
    };
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
        eprintln!("serve 命令组需要动词：start / stop / restart / status / token / relay / ddns");
        std::process::exit(2);
    };
    let op = match verb.as_str() {
        "start" => vocab::OpName::ServeStart,
        "stop" => vocab::OpName::ServeStop,
        "restart" => vocab::OpName::ServeRestart,
        "status" => vocab::OpName::ServeStatus,
        "token" => vocab::OpName::ServeToken,
        // Q-H F8/CA13：裸名词 --help 短路（此前落「不认识的动词」exit 2）。
        "--help" | "-h" => {
            eprintln!("用法：homeway-cli serve <start|stop|restart|status|token|relay|ddns> [--state DIR]");
            eprintln!("  start/stop/restart = 角色期望态（写 config serve.enabled + 控制面启停）；status/token 纯读不拉起。");
            eprintln!("  relay set <token>|clear / ddns add <域名>|delete <域名>|list（纯配置写，需 restart 生效）。");
            std::process::exit(0);
        }
        // 一次性直跑（纯文件操作——servegroup_cli.go「serve relay set/clear 与 ddns」
        // 同框：写完打「需 restart 生效」提示）。
        "relay" | "ddns" => {
            serve_ddns_relay_group(verb, &args[1..]);
            return;
        }
        other => {
            eprintln!("serve 不认识的动词 {other:?}（可用：start / stop / restart / status / token / relay / ddns）");
            std::process::exit(2);
        }
    };
    let p = parse_args("serve <start|stop|restart|status|token> [--state DIR]", &args[1..]);
    let state = p.state.clone();
    if op == vocab::OpName::ServeToken {
        token_reveal_with_fallback(&state, op);
        return;
    }
    // 纯读/直改族不拉起（中-5——Go dialGroupRead/roleStopCLI：status 未跑 = 降级
    // 读 config；stop 未跑 = 直改 config 即达期望态；start/restart 才走拉起缝）。
    if op == vocab::OpName::ServeStatus {
        let Some(c) = try_dial_control_named(&state, "homeway-serve") else {
            // Q-H F1：坏 config = 如实报读失败（Go `degradedServe` 的
            // 「config 读取失败：<err>」同形；此前静默按默认 enabled=true 呈现）。
            match crate::unified_cli::config_role_enabled(&state, "serve") {
                Ok(enabled) => println!("serve：进程未运行（本命令纯读不拉起）；config serve.enabled={enabled}"),
                Err(e) => println!("serve：进程未运行（本命令纯读不拉起）；config 读取失败：{e}"),
            }
            return;
        };
        render_serve_status(&c);
        return;
    }
    if op == vocab::OpName::ServeStop {
        if let Some(c) = try_dial_control_named(&state, "homeway-serve") {
            let v = c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
            println!("serve：{}", v["action"].as_str().unwrap_or("?"));
            return;
        }
        // 未跑：直改 config（Go 同义——拉一个进程只为停它本末倒置）。
        if let Err(e) = crate::unified_cli::write_config_enabled(&state, Some(false), None) {
            eprintln!("homeway: 写 config 失败：{e}");
            std::process::exit(1);
        }
        println!("serve：进程未运行——期望已写为停用（config serve.enabled=false），下次启动不再装配");
        return;
    }
    let c = dial_control(&state, p.no_spawn);
    let v = c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    // （start/restart 的应答面；status/stop 已在纯读/直改分支提前返回。）
    println!("serve：{}", v["action"].as_str().unwrap_or("?"));
}

/// serve.status 的渲染单实现（中-5 收敛：在线/拉起两路共用）。
fn render_serve_status(c: &Arc<ControlClient>) {
    let v = c.request(vocab::OpName::ServeStatus.as_str(), Some(serde_json::json!({})), TIMEOUT)
        .unwrap_or_else(|e| exit_op_err(&e));
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
    // ddns 段（Go servegroup_cli.go:483-486 同串——P0-4 的 status 出口；评审 3.5：
    // 此前控制面载荷已对齐但 CLI 侧没有消费口）。
    if let Some(ddns) = v["ddns"].as_array() {
        if !ddns.is_empty() {
            println!("  ddns：");
            for d in ddns {
                let mut line = format!(
                    "    - {}（连续不一致 {} 拍）",
                    d["domain"].as_str().unwrap_or("?"),
                    d["lagStreak"].as_i64().unwrap_or(0),
                );
                if d["warnedLag"].as_bool().unwrap_or(false) {
                    line.push_str("[ 已告警滞后]");
                }
                if d["warnedAAAA"].as_bool().unwrap_or(false) {
                    line.push_str("[ 已告警缺 AAAA]");
                }
                println!("{line}");
            }
        }
    }
    if let Some(peers) = v["peers"].as_array() {
        println!("  peers：{}", peers.len());
        for p in peers {
            println!(
                "    dev={} ip={} 空闲={}s",
                &p["dev"].as_str().unwrap_or("?")[..16.min(p["dev"].as_str().unwrap_or("?").len())],
                p["tunnelIp"].as_str().unwrap_or("?"),
                p["idleMs"].as_i64().unwrap_or(0) / 1000,
            );
        }
    }
    let itc = &v["intercept"];
    println!(
        "  intercept：dialOk={} dialFail={} reject={} flows={}",
        itc["dialOk"].as_u64().unwrap_or(0),
        itc["dialFail"].as_u64().unwrap_or(0),
        itc["reject"].as_u64().unwrap_or(0),
        itc["flows"].as_u64().unwrap_or(0),
    );
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
        // Q-H F8/CA13：裸名词 --help 短路。
        "--help" | "-h" => {
            eprintln!("用法：homeway-cli relay <start|stop|restart|status|token> [--state DIR]");
            eprintln!("  = 本机中继角色的控制面命令组（前台单中继 = `homeway-cli relay [flags]`）。");
            std::process::exit(0);
        }
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
    // 纯读/直改族不拉起（中-5——与 serve 组同款）。
    if op == vocab::OpName::RelayStatus {
        let Some(c) = try_dial_control_named(&state, "homeway-relay") else {
            match crate::unified_cli::config_role_enabled(&state, "relay") {
                Ok(enabled) => println!("relay：进程未运行（本命令纯读不拉起）；config relay.enabled={enabled}"),
                Err(e) => println!("relay：进程未运行（本命令纯读不拉起）；config 读取失败：{e}"),
            }
            return;
        };
        let v = c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
        render_relay_status(&v);
        return;
    }
    if op == vocab::OpName::RelayStop {
        if let Some(c) = try_dial_control_named(&state, "homeway-relay") {
            let v = c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
            println!("relay：{}", v["action"].as_str().unwrap_or("?"));
            return;
        }
        if let Err(e) = crate::unified_cli::write_config_enabled(&state, None, Some(false)) {
            eprintln!("homeway: 写 config 失败：{e}");
            std::process::exit(1);
        }
        println!("relay：进程未运行——期望已写为停用（config relay.enabled=false），下次启动不再装配");
        return;
    }
    let c = dial_control(&state, p.no_spawn);
    let v = c.request(op.as_str(), Some(serde_json::json!({})), TIMEOUT).unwrap_or_else(|e| exit_op_err(&e));
    // （start/restart 的应答面；status/stop 已在纯读/直改分支提前返回。）
    println!("relay：{}", v["action"].as_str().unwrap_or("?"));
}

/// relay.status 的渲染单实现（中-5 收敛）。
fn render_relay_status(v: &serde_json::Value) {
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

// ---------- export / import / reset（状态工件面；P1-6） ----------

/// 工件族三动词的 `--state` 取值（Q-H F2：`--state=DIR` 等号形与空格形等价、缺值/
/// 空值/吞 flag = fail-fast；N2 修复：旧 `normalize_flag_eq` 对 `--state=` **空值**
/// 不归一 ⇒ `homeway export --state=` 会把 `--state=` 当目标文件名）。
fn export_import_state(
    args: &[String],
    i: &mut usize,
) -> Option<std::path::PathBuf> {
    let a = args[*i].as_str();
    let (name, inline) = cli_flags::split_flag(a)?;
    if name != "state" {
        return None;
    }
    let v = cli_flags::take_state_or_exit("state", inline, args.get(*i + 1).map(String::as_str));
    if inline.is_none() {
        *i += 1;
    }
    Some(v)
}


/// `homeway export [--state D] [dest.tar]`——一次性直跑（不连控制面；语义真源
/// internal/daemon/artifact_cli.go）。默认名 homeway-export-<ts>.tar 于当前目录。
pub fn cmd_export(args: &[String]) {
    let mut state: Option<std::path::PathBuf> = None;
    let mut dest: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if let Some(v) = export_import_state(args, &mut i) {
            state = Some(v);
            i += 1;
            continue;
        }
        let other = args[i].as_str();
        if dest.is_some() {
            eprintln!("export 至多一个位置参数（目标文件），got {other:?}");
            std::process::exit(2);
        }
        dest = Some(other.to_owned());
        i += 1;
    }
    let state_dir = state.unwrap_or_else(crate::unified_cli::default_state_dir);
    let dest = dest.unwrap_or_else(|| format!("homeway-export-{}.tar", now_timestamp()));
    if let Err(e) = homeway_core::artifact::export(&state_dir, std::path::Path::new(&dest)) {
        eprintln!("homeway: export 失败：{e}");
        std::process::exit(1);
    }
    println!("已导出：{dest}（不变量四件 = config.toml + serve/ + relay/ + client/；0600、未压缩 tar）");
}

/// `homeway import <file> [--state D]`——布局校验 + 安全解包 + 落位序（目标进程
/// 必须在停——锁试探拒绝）。
pub fn cmd_import(args: &[String]) {
    let mut state: Option<std::path::PathBuf> = None;
    let mut file: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        if let Some(v) = export_import_state(args, &mut i) {
            state = Some(v);
            i += 1;
            continue;
        }
        let other = args[i].as_str();
        if file.is_some() {
            eprintln!("import 只接受一个位置参数（工件文件），got {other:?}");
            std::process::exit(2);
        }
        file = Some(other.to_owned());
        i += 1;
    }
    let Some(file) = file else {
        eprintln!("import 需要 <file>（homeway export 产出的 tar 工件）");
        std::process::exit(2);
    };
    let state_dir = state.unwrap_or_else(crate::unified_cli::default_state_dir);
    if let Err(e) = homeway_core::artifact::import(&state_dir, std::path::Path::new(&file)) {
        eprintln!("homeway: import 失败：{e}");
        std::process::exit(1);
    }
    println!("已导入 {file} → {}（旧四件备份于 .import-old-*；身份与 token 连续）", state_dir.display());
}

/// `homeway reset cache [--state D]`（两词动词；v1 唯一动词 = cache）。
pub fn cmd_reset(args: &[String]) {
    let Some(verb) = args.first() else {
        eprintln!("用法：homeway reset cache [--state D]（清可弃层 cache/；进程在跑拒绝）");
        eprintln!("homeway: reset 需要动词：cache");
        std::process::exit(1);
    };
    if verb != "cache" {
        eprintln!("homeway: reset 不认识的动词 {verb:?}（可用：cache）");
        std::process::exit(1);
    }
    let mut state: Option<std::path::PathBuf> = None;
    let mut i = 1;
    while i < args.len() {
        if let Some(v) = export_import_state(args, &mut i) {
            state = Some(v);
            i += 1;
            continue;
        }
        eprintln!("reset cache 不接受位置参数（got {:?}）", args[i]);
        std::process::exit(2);
    }
    let state_dir = state.unwrap_or_else(crate::unified_cli::default_state_dir);
    if let Err(e) = homeway_core::artifact::reset_cache(&state_dir) {
        eprintln!("homeway: reset cache 失败：{e}");
        std::process::exit(1);
    }
    println!("cache/ 已清（日志/端点缓存可弃层；L1/L2 与 migration-backup-* 不动；下次启动自动重建）");
}

/// RFC3339 近形时间戳（spawn 分隔行用；localtime 面与 now_timestamp 同源）。
fn chrono_like_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}{:+03}:00",
            tm.tm_year as i64 + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec,
            tm.tm_gmtoff / 3600
        )
    }
}

fn now_timestamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&secs, &mut tm);
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year as i64 + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}


// ---------- status --watch（live 渲染：client 域快照 + 订阅续播；P2-5） ----------

/// watch 渲染行（快照初值 + 事件增量；Go watchHost 同形）。
struct WatchHost {
    id: String,
    name: String,
    state: String,
    reason: String,
    via: String,
    ep: String,
    rtt_ms: i64,
}

/// `homeway status --watch`：一次 Dial → snapshot.get → events.subscribe（link+session
/// 域，view=启动时主机集合——参与需求合成，退出后贡献消失）→ 终端原地渲染
/// （state/reason/via/rtt 随事件刷新，Ctrl-C 退出）。⚠️ 观测副作用：watch 期间被
/// 显示主机被视为有需求（订阅视图源，门控不压制其巡检证据）——观测行为改变被观测
/// 系统，watch 不是纯被动观测（Go status_watch.go 同语义）。
fn status_watch(state_dir: &std::path::Path) {
    let sock = state_dir.join("control.sock");
    let (c, welcome) = match ControlClient::dial(&sock, "cli", "homeway-watch") {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "homeway 未在运行（sock={}：{e}）\n--watch 需进程在位（纯读不拉起）；先启动：homeway-cli --state {}（零参统一进程）",
                sock.display(),
                state_dir.display()
            );
            std::process::exit(1);
        }
    };
    let snap = c
        .request(vocab::OpName::SnapshotGet.as_str(), None, TIMEOUT)
        .unwrap_or_else(|e| exit_op_err(&e));
    let seq = snap["seq"].as_u64().unwrap_or(0);
    let mut hosts: Vec<WatchHost> = Vec::new();
    let mut view = String::new();
    if let Some(arr) = snap["hosts"].as_array() {
        for (i, h) in arr.iter().enumerate() {
            hosts.push(WatchHost {
                id: h["id"].as_str().unwrap_or("?").to_owned(),
                name: h["name"].as_str().unwrap_or("-").to_owned(),
                state: h["state"].as_str().unwrap_or("?").to_owned(),
                reason: h["reason"].as_str().unwrap_or("").to_owned(),
                via: h["link"]["via"].as_str().unwrap_or("-").to_owned(),
                ep: h["link"]["ep"].as_str().unwrap_or("-").to_owned(),
                rtt_ms: h["link"]["rttMs"].as_i64().unwrap_or(0),
            });
            if i > 0 {
                view.push(',');
            }
            view.push_str(&format!("host={}", h["id"].as_str().unwrap_or("?")));
        }
    }
    let generation = welcome.generation.clone();
    if let Err(e) = c.subscribe(
        &[vocab::DOMAIN_LINK.to_owned(), vocab::DOMAIN_SESSION.to_owned()],
        Some(seq),
        &view,
        &generation,
        TIMEOUT,
    ) {
        if e.code == vocab::CODE_CURSOR_STALE {
            eprintln!(
                "homeway: 订阅游标失效（cursor_stale：守护进程已重启或事件窗过旧）——重新运行一次 homeway-cli status --watch 即从全量快照开始"
            );
            std::process::exit(1);
        }
        exit_op_err(&e);
    }
    let events = c.take_events().expect("订阅成功后事件通道在位");

    // Ctrl-C/SIGTERM = 正常退出（退订由连接关闭承载——view 的需求贡献消失）。
    let pipes = install_status_watch_signals();
    render_watch(&hosts);
    loop {
        // 事件优先（500ms 心跳复检信号/连接——评审 r2-1：断开判据走 is_closed/
        // goodbye 状态面；通道 Disconnected 永不触发〔发送端随长命 ControlClient 活着〕）。
        match events.recv_timeout(std::time::Duration::from_millis(500)) {
            Ok(ev) => {
                apply_watch_event(&mut hosts, &ev);
                render_watch(&hosts);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {}
        }
        if c.is_closed() {
            if let Some(g) = c.goodbye() {
                eprintln!(
                    "homeway: 守护进程已断开（goodbye={}）——检查守护进程（homeway-cli status）后重新运行本命令",
                    g.reason
                );
            } else {
                eprintln!("homeway: 守护进程连接已断开——守护进程可能已退出（homeway-cli status 确认后重新运行本命令）");
            }
            drop_status_watch_signals(pipes);
            std::process::exit(1);
        }
        if watch_signal_fired(&pipes) {
            // Ctrl-C：正常退出（exit 0——Go 同口径）
            drop_status_watch_signals(pipes);
            return;
        }
    }
}

/// 事件增量应用（渲染行按 host 键幂等覆盖——at-least-once 语义下重复事件无害；
/// 词表只增：未入渲染面的 kind 忽略）。
fn apply_watch_event(hosts: &mut Vec<WatchHost>, ev: &homeway_core::daemon::proto::EventBody) {
    let host = ev.payload.as_ref().and_then(|p| p["host"].as_str()).unwrap_or("").to_owned();
    match ev.kind.as_str() {
        vocab::KIND_SESSION_ADDED => {
            // 新主机建行（渲染随表；视图声明不追溯——Go 同口径）。
            if !hosts.iter().any(|h| h.id == host) {
                hosts.push(WatchHost {
                    id: host.clone(),
                    name: ev.payload.as_ref().and_then(|p| p["name"].as_str()).unwrap_or("-").to_owned(),
                    state: "connecting".to_owned(),
                    reason: String::new(),
                    via: "-".to_owned(),
                    ep: "-".to_owned(),
                    rtt_ms: 0,
                });
            }
            return;
        }
        vocab::KIND_SESSION_REMOVED => {
            // 移除主机删行。
            hosts.retain(|h| h.id != host);
            return;
        }
        _ => {}
    }
    let Some(row) = hosts.iter_mut().find(|h| h.id == host) else {
        return; // 视图外主机
    };
    match ev.kind.as_str() {
        vocab::KIND_LINK_CHANGED => {
            if let Some(p) = &ev.payload {
                row.via = p["via"].as_str().unwrap_or(&row.via).to_owned();
                row.ep = p["ep"].as_str().unwrap_or(&row.ep).to_owned();
                row.rtt_ms = p["rttMs"].as_i64().unwrap_or(row.rtt_ms);
            }
        }
        vocab::KIND_SESSION_STATE_CHANGED => {
            if let Some(p) = &ev.payload {
                row.state = p["state"].as_str().unwrap_or(&row.state).to_owned();
                row.reason = p["reason"].as_str().unwrap_or("").to_owned();
            }
        }
        vocab::KIND_SESSION_DIAG => {
            if let Some(p) = &ev.payload {
                row.reason = p["reason"].as_str().unwrap_or("").to_owned();
            }
        }
        _ => {}
    }
}

fn render_watch(hosts: &[WatchHost]) {
    use std::io::Write as _;
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b[2J\x1b[H");
    let _ = writeln!(out, "homeway status --watch（Ctrl-C 退出；观测副作用：显示中主机被视为有需求）");
    let _ = writeln!(out, "{:<18} {:<10} {:<10} {:<8} {:<22} {:>6}", "HOST", "NAME", "STATE", "VIA", "EP", "RTT");
    for h in hosts {
        // 字符截断（评审 r2-6：字节下标切中文必 panic）。
        let rc: Vec<char> = h.reason.chars().collect();
        let reason = if rc.len() > 20 {
            rc[..19].iter().collect::<String>() + "…"
        } else {
            h.reason.clone()
        };
        let _ = writeln!(
            out,
            "{:<18} {:<10} {:<10} {:<8} {:<22} {:>4}ms {}",
            &h.id[..12.min(h.id.len())],
            h.name,
            h.state,
            h.via,
            h.ep,
            h.rtt_ms,
            reason,
        );
    }
    let _ = out.flush();
}

/// 信号管道（SIGINT/SIGTERM → 'x'；status watch 的干净退出面）。
struct WatchSigPipe {
    read_fd: i32,
    write_fd: i32,
}

static WATCH_SIG_W: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);

extern "C" fn on_watch_sig(_sig: i32) {
    let fd = WATCH_SIG_W.load(std::sync::atomic::Ordering::SeqCst);
    if fd >= 0 {
        let b = b"x";
        unsafe { libc::write(fd, b.as_ptr().cast(), 1) };
    }
}

fn install_status_watch_signals() -> WatchSigPipe {
    // Q-G F1：管道经 `sysfd` 建（两端 CLOEXEC）→ 立即 `into_raw_fd()` 交既有 i32
    // 字段（生命周期由 `drop_status_watch_signals` 收口）。arm/disarm 形态已是
    // 正确先例（先 `store(-1)` 再 close），本批只补标志。
    let (r, w) = match homeway_core::sysfd::pipe_cloexec() {
        Ok(v) => (
            std::os::fd::IntoRawFd::into_raw_fd(v.0),
            std::os::fd::IntoRawFd::into_raw_fd(v.1),
        ),
        Err(e) => {
            eprintln!("homeway: 信号管道建立失败（{e}）——watch 退出");
            std::process::exit(1);
        }
    };
    let fds = [r, w];
    // 读端非阻塞（评审 r2-1：阻塞读端曾令主循环每轮末尾挂死——事件/断开全冻结）。
    unsafe {
        let fl = libc::fcntl(fds[0], libc::F_GETFL);
        libc::fcntl(fds[0], libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    WATCH_SIG_W.store(fds[1], std::sync::atomic::Ordering::SeqCst);
    unsafe {
        let h = on_watch_sig as extern "C" fn(i32) as libc::sighandler_t;
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }
    WatchSigPipe { read_fd: fds[0], write_fd: fds[1] }
}

/// 非阻塞排空信号管道（有 'x' = 退出信号到过）。
fn watch_signal_fired(p: &WatchSigPipe) -> bool {
    let mut b = [0u8; 64];
    let mut got = false;
    loop {
        let n = unsafe { libc::read(p.read_fd, b.as_mut_ptr().cast(), b.len()) };
        if n > 0 {
            if b[..n as usize].contains(&b'x') {
                got = true;
            }
            continue;
        }
        break;
    }
    got
}

fn drop_status_watch_signals(p: WatchSigPipe) {
    WATCH_SIG_W.store(-1, std::sync::atomic::Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::close(p.read_fd);
        libc::close(p.write_fd);
    }
}

// ---------- serve relay set/clear 与 serve ddns（一次性直跑：纯文件操作） ----------
// 语义真源 baseline:internal/daemon/servegroup_cli.go 的 serveRelayCLI/serveDDNSCLI。
// 写 config 用 unified_cli::update_config（重读→只改目标键→原子写 0600）；
// 在跑检测 = 控制面 dial 短试 + lock 试探兜（Go daemonRunning 同义）。

/// 守护进程在跑判定（提示「需 restart 生效」的依据）。
fn daemon_running_probe(state_dir: &std::path::Path) -> bool {
    if try_dial_control_named(state_dir, "homeway-serve").is_some() {
        return true;
    }
    lock_held_probe(state_dir).0
}

fn serve_ddns_relay_group(group: &str, args: &[String]) {
    let Some(verb) = args.first() else {
        match group {
            "ddns" => {
                eprintln!("用法：homeway serve ddns add <domain> | homeway serve ddns delete <domain> | homeway serve ddns list");
                eprintln!("（[[serve.ddns]] = token 叠加域名条目 + 自检；域名记录由你的 DDNS 设施维护）");
                std::process::exit(2);
            }
            _ => {
                eprintln!("用法：homeway serve relay set <token> [--stdin] | homeway serve relay clear");
                eprintln!("（serve.relay = 本出口注册到哪个**上游中继**的 token；[relay] 节 = 本机当中继——同词根不同义）");
                std::process::exit(2);
            }
        }
    };
    match (group, verb.as_str()) {
        ("ddns", "add") | ("ddns", "delete") => serve_ddns_write(verb, &args[1..]),
        ("ddns", "list") => serve_ddns_list(&args[1..]),
        ("relay", "set") => serve_relay_set(&args[1..]),
        ("relay", "clear") => serve_relay_clear(&args[1..]),
        ("ddns", other) => {
            eprintln!("serve ddns 不认识的动词 {other:?}（可用：add / delete / list）");
            std::process::exit(2);
        }
        ("relay", other) => {
            eprintln!("serve relay 不认识的动词 {other:?}（可用：set / clear）");
            std::process::exit(2);
        }
        _ => unreachable!(),
    }
}

fn serve_ddns_write(verb: &str, args: &[String]) {
    let p = parse_args("serve ddns <add|delete> [--state DIR]", args);
    if p.positional.len() != 1 {
        eprintln!("serve ddns {verb} 需要 <domain>（裸域名）");
        std::process::exit(2);
    }
    let domain = p.positional[0].trim().to_owned();
    if domain.is_empty() || domain.contains(':') || domain.contains('/') || domain.contains(' ') {
        eprintln!("serve ddns {verb} 需要 <domain>（裸域名——不带端口/路径）：{domain:?}");
        std::process::exit(2);
    }
    let edit = if verb == "add" {
        crate::unified_cli::ConfigEdit::DdnsAdd(domain.clone())
    } else {
        crate::unified_cli::ConfigEdit::DdnsRemove(domain.clone())
    };
    let r = crate::unified_cli::update_config(&p.state, edit);
    let written = match r {
        Ok(w) => w,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    match written {
        crate::unified_cli::DdnsWrite::AlreadyThere => {
            eprintln!("ddns 条目 {domain} 已在 config（不重复添加）");
            std::process::exit(1);
        }
        crate::unified_cli::DdnsWrite::Absent => {
            println!("ddns 条目 {domain} 不在 config（幂等，无动作）");
        }
        crate::unified_cli::DdnsWrite::Written => {
            if verb == "add" {
                println!("ddns 条目 {domain} 已写入 config");
            } else {
                println!("ddns 条目 {domain} 已从 config 删除");
            }
        }
    }
    if daemon_running_probe(&p.state) {
        println!("⚠️ 不热更：需 `homeway serve restart` 生效");
    }
}

fn serve_ddns_list(args: &[String]) {
    let p = parse_args("serve ddns list [--json] [--state DIR]", args);
    match crate::unified_cli::config_serve_ddns(&p.state) {
        Ok(list) => {
            if p.json {
                println!("{}", serde_json::to_string(&list).unwrap_or_else(|_| "[]".to_owned()));
                return;
            }
            if list.is_empty() {
                println!("（config 无 ddns 条目）");
                return;
            }
            for d in list {
                println!("{d}");
            }
        }
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}

/// CLI 侧 relay token 形态校验（Q-H F1 收敛：单一实现 = `serve_cli::validate_relay_arg`
/// ——config 值域与 CLI 写前校验共用同一口径）。
fn validate_relay_arg(tok: &str) -> Result<(), String> {
    crate::serve_cli::validate_relay_arg(tok)
}

fn serve_relay_set(args: &[String]) {
    // 评审 5.1 整改：位置参数形（`serve relay set <token>`）此前走手写 flag 循环
    // 被判「未知参数」100% 不可用——改 parse_args 统一解析（--state/--stdin 等 flag
    // 之外的内容全进 positional，取第一个为 token；--stdin 从 stdin 读单行）。
    let p = parse_args("serve relay set <token> [--stdin] [--state DIR]", args);
    let token = if p.stdin {
        let mut line = String::new();
        use std::io::BufRead;
        if std::io::stdin().lock().read_line(&mut line).is_err() && line.is_empty() {
            eprintln!("读 stdin 失败");
            std::process::exit(1);
        }
        line.trim().to_owned()
    } else {
        if p.positional.len() != 1 {
            eprintln!("serve relay set 需要 <token>（rl1… 或裸 IP:port）或 --stdin（从 stdin 读单行）");
            std::process::exit(2);
        }
        p.positional[0].trim().to_owned()
    };
    if token.is_empty() {
        eprintln!("token 为空（--stdin 读到空行）");
        std::process::exit(1);
    }
    if let Err(e) = validate_relay_arg(&token) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    let masked = mask_for_hint(&token);
    if let Err(e) = crate::unified_cli::update_config(
        &p.state,
        crate::unified_cli::ConfigEdit::RelaySet(token.clone()),
    ) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    println!("serve.relay 已写入 config（{masked}，0600 原子写）");
    if daemon_running_probe(&p.state) {
        println!("⚠️ 不热更：需 `homeway serve restart` 生效（重启后中继注册腿以新 token 注册）");
    }
}

fn serve_relay_clear(args: &[String]) {
    let p = parse_args("serve relay clear [--state DIR]", args);
    if !p.positional.is_empty() {
        eprintln!("serve relay clear 不接受位置参数（得 {:?}）", p.positional);
        std::process::exit(2);
    }
    if let Err(e) = crate::unified_cli::update_config(
        &p.state,
        crate::unified_cli::ConfigEdit::RelayClear,
    ) {
        eprintln!("{e}");
        std::process::exit(1);
    }
    println!("serve.relay 已从 config 清除");
    if daemon_running_probe(&p.state) {
        println!("⚠️ 不热更：需 `homeway serve restart` 生效（重启后不再注册中继）");
    }
}

/// 写入回显的 token 掩码（凭证纪律：全文在命令行参数里，输出面保持掩码习惯）。
fn mask_for_hint(tok: &str) -> String {
    let n = tok.chars().count();
    if n <= 12 {
        let head: String = tok.chars().take(4).collect();
        format!("{head}…")
    } else {
        let head: String = tok.chars().take(12).collect();
        format!("{head}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Q-H F14：只有**默认 state** 才等 launchd KeepAlive（非默认 = 直接自拉起，
    /// 防临时 state 的探测触发别的部署的 KeepAlive）。
    #[test]
    fn launchd_relaunch_only_for_default_state() {
        assert!(launchd_relaunch_relevant(&default_state_dir()));
        assert!(!launchd_relaunch_relevant(std::path::Path::new("/tmp/hw-not-default")));
    }

    /// Q-H F10：resolve_host 全长 hex 四档（表内小写/表内大写/表外 hex/64 非 hex）
    /// + 名称/前缀仍可用。
    #[test]
    fn resolve_host_full_hex_forms() {
        let id_a = "ab".repeat(32);
        let id_b = "11".repeat(32);
        let briefs = serde_json::json!({"hosts": [
            {"id": id_a, "name": "exit1"},
            {"id": id_b, "name": "exit2"},
        ]});
        assert_eq!(resolve_host(&briefs, &id_a).unwrap(), id_a, "表内小写命中");
        assert_eq!(
            resolve_host(&briefs, &id_a.to_uppercase()).unwrap(),
            id_a,
            "表内大写 → canonical 小写返回表内 id"
        );
        let e = resolve_host(&briefs, &"cd".repeat(32)).unwrap_err();
        assert!(e.contains("没有匹配"), "表外 hex 必须就地报错：{e}");
        let e = resolve_host(&briefs, &"z".repeat(64)).unwrap_err();
        assert!(e.contains("没有匹配"), "64 非 hex 同样无命中：{e}");
        // 名称/唯一前缀路径不受影响。
        assert_eq!(resolve_host(&briefs, "exit2").unwrap(), id_b);
        assert_eq!(resolve_host(&briefs, "ab").unwrap(), id_a);
    }
}
