//! `homeway-cli term` 命令面（B0-2b 第 2 棒：P1-3 五动词 + `--host` 远程模式——
//! 语义真源 `baseline:pkg/term/term_cli.go` + `term_cli_attach.go`〔raw 腿〕+
//! `internal/daemon/term_remote.go`〔远程拨号与三态归因〕）。
//!
//! 命令面（与 App 同一份会话注册表；本地面经 `<state>/term.sock` 直连、不经隧道；
//! `--host` 模式经 control.sock 的 stream.open{kind:term} → 隧道 → 对端 term 服务，
//! term 帧协议端到端原样承载）：
//!
//!  homeway-cli term list [--json] [--host <ref>] [--state D] [--timeout T]
//!  homeway-cli term new [name] [-d] [-A] […]        # 默认创建并接入；-d 只创建不接入
//!  homeway-cli term attach [name] [-d] [--detach-key K] […]
//!  homeway-cli term delete <name> […]
//!  homeway-cli term explain --file <屏幕文本> --agent <label> | <会话名> […]
//!
//! raw 腿生命周期硬约束（spec「raw 终端接入」）：任何退出路径都还原本地终端设置
//! （收尾闭包统一收口 + 信号处理）；非 TTY 拒绝；TERM_SESSION_ID 指向目标会话时
//! 拒绝（输出回环防护）；断腿服务端不发 ENDED ⇒ 远程面按三态归因给可行动文案。
//!
//! 与 Go 的形态差异（登记 B0-2b.md）：Go 侧 dialControlSpawn 的「按需拉起统一进程」
//! （role-management 4.1）不在本实现——守护不在跑 = 可行动错误（先起统一进程）。

use std::io::Write as IoWrite;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use homeway_core::daemon::client::{ClientErr, ClientStream, ControlClient};
use homeway_core::daemon::vocab;
use homeway_core::term::frames::{self, caps, ended_code, hello_flags, Op};
use homeway_core::term::wire::FrameIo;

use crate::unified_cli::default_state_dir;

/// OSC 21 标题查询总预算（不应答的终端 ≤ 这么久后进入正常透传；Go 同值）。
const TITLE_QUERY_BUDGET: Duration = Duration::from_millis(150);
/// 远程「解析」与「连接 + stream.open」各一次的预算缺省（Go defaultRemoteTimeout）。
const DEFAULT_REMOTE_TIMEOUT: Duration = Duration::from_secs(10);
/// 自动命名连续重试上限（Go 同值）。
const AUTO_NAME_TRIES: usize = 8;

// ---------------------------------------------------------------------------
// 拨号目标（本地面 term.sock / 远端 --host 经控制面 stream.open）
// ---------------------------------------------------------------------------

/// `--host` 的名称/全长 hex/无歧义短前缀解析（寻址 = 与 host delete 同一规则与文案）。
/// `timeout` = 本次解析的预算（Go：解析与打开各用一次 --timeout，默认各 10s）。
fn resolve_host_ref(state_dir: &std::path::Path, r#ref: &str, timeout: Duration) -> Result<String, String> {
    if r#ref.trim().is_empty() {
        return Err("空寻址串不可用——homeway-cli term <子命令> --host 需要 <name|id>（homeway-cli host list 查看在表主机）".to_owned());
    }
    let c = dial_control_spawn_term(state_dir, false)?;
    let briefs = c
        .request(vocab::OpName::HostList.as_str(), None, timeout)
        .map_err(|e| format!("host.list 失败：{}", crate::daemon_cli::op_err_text(&e)))?;
    let r = crate::daemon_cli::resolve_host(&briefs, r#ref);
    c.close(); // 显式关（评审 r2-10：ControlClient 无 Drop——不关则 fd+reader 线程滞留到进程退出）
    r
}

/// term 面的控制面拨号：先试拨（其它错误保留 term 面的误指提示文案），未运行族
/// 走按需拉起（D-1：Go dialCarrierCLI → dialControlSpawn 同义；`--no-spawn` =
/// fail-fast——Go FIX-57 共享位）。
fn dial_control_spawn_term(state_dir: &std::path::Path, no_spawn: bool) -> Result<Arc<ControlClient>, String> {
    let sock = state_dir.join("control.sock");
    match ControlClient::dial(&sock, "cli", "homeway-term") {
        Ok((c, _)) => Ok(c),
        Err(e) => {
            if crate::daemon_cli::sock_not_running(&sock) {
                crate::daemon_cli::dial_control_spawn(state_dir, "homeway-term", no_spawn)
            } else {
                Err(control_dial_err(&sock, &e))
            }
        }
    }
}

/// 控制面连接层错误 → 可行动文案（Go controlDialErr 同义：ENOENT 带出口 state 误指提示）。
fn control_dial_err(sock: &std::path::Path, e: &ClientErr) -> String {
    let dir = sock.parent().unwrap_or(std::path::Path::new("."));
    match e {
        ClientErr::Dial(inner)
            if inner.contains("No such file or directory")
                || inner.contains("os error 2")
                || inner.contains("not found") =>
        {
            let mut hint = String::new();
            for svc in ["term.sock", "files.sock"] {
                if dir.join(svc).exists() {
                    hint = format!("；该目录有 {svc} 无 control.sock——像是把 --state 指到了出口 state 目录");
                    break;
                }
            }
            format!(
                "homeway 统一进程未在运行（{} 不存在{hint}）\n--host 模式下 --state 指统一 state 根（control.sock 所在）；先启动：homeway-cli --state {}",
                sock.display(),
                dir.display()
            )
        }
        ClientErr::Dial(inner) if inner.contains("Connection refused") => {
            format!("连接被拒：{} 像是残留 socket（daemon 进程已退出）；确认 daemon 在跑，或删除该文件后重试", sock.display())
        }
        ClientErr::Dial(inner) if inner.contains("Permission denied") => {
            format!("无权连接 {}（control.sock 仅属主可用；命令须与 daemon 同一用户运行）", sock.display())
        }
        other => format!("连不上 daemon 控制面（{}）：{other}", sock.display()),
    }
}

/// stream.open 错误码 → 可行动文案（Go streamOpenErr 同义）。
fn stream_open_err_text(e: &homeway_core::daemon::proto::OpError) -> String {
    let detail = {
        let d = e.detail();
        if d.is_empty() { String::new() } else { format!("：{d}") }
    };
    match e.code.as_str() {
        vocab::CODE_NO_HOST => "主机不在守护进程表中（no_host）；homeway-cli host list 查看在表主机".to_owned(),
        vocab::CODE_NOT_READY => "守护进程注册表未就绪（not_ready；client 角色启动中/重建窗口），稍后重试".to_owned(),
        vocab::CODE_STREAM_REFUSED => format!(
            "与主机的流打开被拒（stream_refused）——主机离线、隧道未通或主机会话不可用；用 homeway-cli host status <name> 核对会话与链路态{detail}"
        ),
        "timeout" => "连接/打开超预算（--timeout）：daemon 未应答或目标主机拨号黑洞；可用 --timeout 加大预算后重试".to_owned(),
        other => format!("stream.open 失败：{other}{detail}"),
    }
}

/// 拨号目标：本地面（连 `<state>/term.sock`）或远端（--host 经控制面）。
struct TermTarget {
    state_dir: PathBuf,
    host_ref: String,
    host_id: Option<String>,
    timeout: Duration,
    /// 守护未跑时不按需拉起（Go FIX-57 共享位；D-1 评审补齐）。
    no_spawn: bool,
}

impl TermTarget {
    fn remote(&self) -> bool {
        !self.host_ref.is_empty()
    }
}

// ---------------------------------------------------------------------------
// TermConn：本地面 FrameIo / 远端 stream.open 适配器（读/写两半可分归线程）
// ---------------------------------------------------------------------------

/// 远程流终结的三态（Go streamend.Error/RemoteEndError 同义：closed|gone|conn）。
#[derive(Debug, Clone, PartialEq, Eq)]
struct RemoteEndError {
    reason: String,
}

/// 远程 attach 的流终结三态文案（Go remoteEndMessage 同串）。
fn remote_end_message(name: &str, reason: &str) -> String {
    match reason {
        "gone" => format!(
            "与主机的流被收尾（主机会话不可达或上行过快）；会话 {name} 仍在目标主机运行，可重新 attach：homeway-cli term attach {name} --host <ref>"
        ),
        "closed" => "对端已关闭连接（term 服务退出或会话收工；也可能是本端长时间停止读取、出口侧慢腿自治收尾了本腿——停滞超 60s 断腿不发 ENDED）".to_owned(),
        "conn" => "与守护进程的连接断开；可重新执行命令重连".to_owned(),
        other => format!("与主机的流已终结（{other}）；可重新 attach：homeway-cli term attach {name} --host <ref>"),
    }
}

/// 远程腿的帧抽取（r2-17 去重：块边界 ≠ 帧边界——`[op:len2LE]` 帧按缓冲拼装到
/// 帧齐；终结三态折 RemoteEndError）。
fn pull_remote_frame(st: &Arc<ClientStream>, buf: &mut Vec<u8>) -> Result<frames::Frame, TermReadErr> {
    loop {
        if buf.len() >= 3 {
            let n = u16::from_le_bytes([buf[1], buf[2]]) as usize;
            if buf.len() >= 3 + n {
                let f = frames::Frame { op: Op(buf[0]), payload: buf[3..3 + n].to_vec() };
                buf.drain(..3 + n);
                return Ok(f);
            }
        }
        match st.recv_wait() {
            Some(chunk) => buf.extend_from_slice(&chunk),
            None => {
                let reason = st.end_reason().unwrap_or_else(|| "conn".to_owned());
                return Err(TermReadErr::Ended(RemoteEndError { reason }));
            }
        }
    }
}

enum TermConn {
    Local(FrameIo),
    Remote {
        client: Arc<ControlClient>,
        st: Arc<ClientStream>,
        buf: Vec<u8>,
    },
}

/// 读帧错误的两类面：IO 失败 / 远程流终结（三态）。
enum TermReadErr {
    Io(String),
    Ended(RemoteEndError),
}

impl TermReadErr {
    /// 归因文案（远程面三态；本地面断链单一文案）。
    fn message(&self, name: &str) -> String {
        match self {
            TermReadErr::Ended(e) => remote_end_message(name, &e.reason),
            TermReadErr::Io(e) => {
                format!("与出口的连接被断开（{e}）；会话仍在运行，可用 homeway-cli term attach {name} 重新接入")
            }
        }
    }
}

/// 读/写两半（attach 主循环形态：读腿线程 + 主线程写；本地面经 UDS 克隆共享 socket）。
enum TermReadHalf {
    Local(FrameIo),
    Remote { st: Arc<ClientStream>, buf: Vec<u8> },
}

enum TermWriteHalf {
    Local(FrameIo),
    Remote { client: Arc<ControlClient>, st: Arc<ClientStream> },
}

impl TermConn {
    /// 终结哨兵（写侧 stream_send 的 StreamEnded 折入读侧同款归因）。
    fn ended_err(reason: String) -> TermReadErr {
        TermReadErr::Ended(RemoteEndError { reason })
    }

    fn write_frame(&mut self, op: Op, payload: &[u8]) -> Result<(), TermReadErr> {
        match self {
            TermConn::Local(io) => io
                .write_frame(op, payload, Duration::from_secs(30))
                .map_err(|e| TermReadErr::Io(e.to_string())),
            TermConn::Remote { client, st, .. } => client
                .stream_send(st, &frames::encode_frame(op, payload))
                .map_err(|e| match e {
                    ClientErr::StreamEnded(reason) => Self::ended_err(reason),
                    other => TermReadErr::Io(other.to_string()),
                }),
        }
    }

    fn read_frame(&mut self) -> Result<frames::Frame, TermReadErr> {
        match self {
            TermConn::Local(io) => io.read_frame().map_err(|e| TermReadErr::Io(e.to_string())),
            TermConn::Remote { st, buf, .. } => pull_remote_frame(st, buf),
        }
    }

    /// 收口（Go streamConn.Close 的死锁序：先 Client.Close〔唯一可靠逃生口——closed
    /// 解阻塞读腿的阻塞投递〕，stream.close 尽力而为、错误忽略；本地面直接关）。
    fn close(&mut self) {
        match self {
            TermConn::Local(io) => {
                if let Ok(clone) = io.try_clone_stream() {
                    let _ = clone.shutdown(std::net::Shutdown::Both);
                }
            }
            TermConn::Remote { client, .. } => client.close(),
        }
    }

    /// 分读/写两半（attach 主循环形态）。
    fn split(self) -> Result<(TermReadHalf, TermWriteHalf), String> {
        match self {
            TermConn::Local(io) => {
                let clone = io
                    .try_clone_stream()
                    .map_err(|e| format!("克隆 term 连接失败：{e}"))?;
                Ok((
                    TermReadHalf::Local(FrameIo::new(clone)),
                    TermWriteHalf::Local(io),
                ))
            }
            TermConn::Remote { client, st, buf } => {
                let read = TermReadHalf::Remote { st: Arc::clone(&st), buf };
                let write = TermWriteHalf::Remote { client, st };
                Ok((read, write))
            }
        }
    }
}

impl TermReadHalf {
    fn read_frame(&mut self) -> Result<frames::Frame, TermReadErr> {
        match self {
            TermReadHalf::Local(io) => io.read_frame().map_err(|e| TermReadErr::Io(e.to_string())),
            TermReadHalf::Remote { st, buf } => pull_remote_frame(st, buf),
        }
    }
}

impl TermWriteHalf {
    fn write_frame(&mut self, op: Op, payload: &[u8]) -> Result<(), TermReadErr> {
        match self {
            TermWriteHalf::Local(io) => io
                .write_frame(op, payload, Duration::from_secs(30))
                .map_err(|e| TermReadErr::Io(e.to_string())),
            TermWriteHalf::Remote { client, st } => client
                .stream_send(st, &frames::encode_frame(op, payload))
                .map_err(|e| match e {
                    ClientErr::StreamEnded(reason) => {
                        TermReadErr::Ended(RemoteEndError { reason })
                    }
                    other => TermReadErr::Io(other.to_string()),
                }),
        }
    }

    /// 关连接（幂等；远程 = Client.Close 逃生口）。
    fn close(&self) {
        match self {
            TermWriteHalf::Local(io) => {
                if let Ok(clone) = io.try_clone_stream() {
                    let _ = clone.shutdown(std::net::Shutdown::Both);
                }
            }
            TermWriteHalf::Remote { client, .. } => client.close(),
        }
    }
}

/// 拨号并读 GREETING（list/new/delete/attach/explain 共用的唯一拨号缝）。
/// 返回出口声明的 features 位（attach 的版本声明位由它决定，FIX-29）。
fn dial_term(t: &mut TermTarget) -> Result<(TermConn, u32), String> {
    let conn = if t.remote() {
        let id = match &t.host_id {
            Some(id) => id.clone(),
            None => {
                let id = resolve_host_ref(&t.state_dir, &t.host_ref, t.timeout)?;
                t.host_id = Some(id.clone());
                id
            }
        };
        let client = dial_control_spawn_term(&t.state_dir, t.no_spawn)?;
        let st = client.open_stream(vocab::STREAM_KIND_TERM, &id, t.timeout).map_err(|e| stream_open_err_text(&e))?;
        TermConn::Remote { client, st, buf: Vec::new() }
    } else {
        let sock = t.state_dir.join("term.sock");
        let stream = UnixStream::connect(&sock).map_err(|e| local_dial_err(&sock, e))?;
        TermConn::Local(FrameIo::new(stream))
    };
    read_greeting(conn)
}

/// 本地面连接层错误 → 可行动文案（Go dialErrText 同义：两态 + 合并提示）。
fn local_dial_err(sock: &std::path::Path, e: std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!(
            "出口未在运行、或终端服务被关闭（HOMEWAY_TERM=off），或 --state 指错目录（{} 不存在）；检查 --state 与出口状态",
            sock.display()
        ),
        std::io::ErrorKind::ConnectionRefused => format!(
            "连接被拒：{} 像是残留 socket（出口进程已退出）；确认出口在跑，或删除该文件后重试",
            sock.display()
        ),
        std::io::ErrorKind::PermissionDenied => format!(
            "无权连接 {}（term.sock 仅属主可用；命令面须与出口同一用户运行）",
            sock.display()
        ),
        _ => format!("连不上 term 服务（{}）：{e}", sock.display()),
    }
}

/// GREETING 读取 + 版本门（CLI 侧，FIX-29：ver != 本端当场拒）。
fn read_greeting(mut conn: TermConn) -> Result<(TermConn, u32), String> {
    let f = conn.read_frame().map_err(|e| format!("读 GREETING：{}", e.message("?")))?;
    if f.op != Op::GREETING {
        conn.close();
        return Err(format!("首帧应为 GREETING，收到 op 0x{:02x}", f.op.0));
    }
    let (ver, feats) = frames::dec_greeting(&f.payload)
        .map_err(|e| {
            conn.close();
            format!("GREETING 体非法：{e}")
        })?;
    if ver != frames::PROTO_VER {
        conn.close();
        return Err(format!(
            "出口终端服务协议版本 {ver} 与本端 {} 不符：出口与客户端需同批升级",
            frames::PROTO_VER
        ));
    }
    Ok((conn, feats))
}

// ---------------------------------------------------------------------------
// CLI 入口与参数
// ---------------------------------------------------------------------------

pub fn cmd_term(args: &[String]) {
    let Some(sub) = args.first() else {
        term_usage();
        std::process::exit(2);
    };
    if matches!(sub.as_str(), "--help" | "-h" | "help") {
        term_usage();
        return;
    }
    let rest = &expand_flag_eq(&args[1..]);
    let r = match sub.as_str() {
        "explain" => cli_explain(rest),
        "list" => cli_list(rest),
        "new" => cli_new(rest),
        "attach" => cli_attach(rest),
        "delete" => cli_delete(rest),
        other => {
            eprintln!("不认识的子命令 {other:?}（可用：list、new、attach、delete、explain）");
            std::process::exit(1);
        }
    };
    if let Err(e) = r {
        eprintln!("homeway term: {e}");
        std::process::exit(1);
    }
}

fn term_usage() {
    println!(
        "homeway-cli term —— 终端服务的主机命令面（与 App 同一份会话注册表）\n  \
list [--json] [--host <ref>] [--state D] [--timeout T]   列会话\n  \
new [name] [-d] [-A] […]   新建并接入；-d 只创建不接入；省略名字自动命名 host-<4hex>\n  \
attach [name] [-d] [--detach-key K] […]   接入（raw 字节模式）；省略名字 = 最近活跃；-d 显式接管\n  \
delete <name> […]   结束会话\n  \
explain --file <屏幕文本> --agent <label> | <会话名> […]   规则判定（离线 / 在线）\n\
远程模式：--host <name|id>（经 daemon 控制面转发；--state 指统一 state 根）；\n  \
--timeout = 解析与打开各一次的预算（默认各 10s，仅 --host 可用）。\n\
attach 分离键：Ctrl-b d 分离 / Ctrl-b r 重对齐 / Ctrl-b Ctrl-b 字面量；--detach-key none 关闭。"
    );
}

/// `--flag=value` → `--flag value`（Go expandFlagEq 同义；cmd_term 入口统一归一）。
pub(crate) fn expand_flag_eq(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    for a in args {
        if let Some(rest) = a.strip_prefix("--") {
            if let Some(i) = rest.find('=') {
                if i > 0 {
                    out.push(format!("--{}", &rest[..i]));
                    out.push(rest[i + 1..].to_owned());
                    continue;
                }
            }
        }
        out.push(a.clone());
    }
    out
}

/// 公共参数（--state/--host/--timeout）。
#[derive(Default)]
struct TermCommon {
    state_dir: Option<PathBuf>,
    host_ref: String,
    timeout: Option<Duration>,
    no_spawn: bool,
}

impl TermCommon {
    /// 构造拨号目标（Go newTermTarget 同义缺省决策：远程 timeout 缺省补 10s、
    /// 本地 state 缺省补默认 state 根）+ **本地面拒绝 --timeout**（Go exec-r1 L6：
    /// 静默忽略与「不认识的参数」严格风格不一致——显式报错）。
    fn into_target(self) -> Result<TermTarget, String> {
        if self.host_ref.is_empty() && self.timeout.is_some() {
            return Err(
                "--timeout 仅 --host 模式可用（远程的解析/打开预算）；本地面请去掉 --timeout".to_owned(),
            );
        }
        Ok(TermTarget {
            state_dir: self.state_dir.unwrap_or_else(default_state_dir),
            host_ref: self.host_ref,
            host_id: None,
            timeout: self.timeout.unwrap_or(DEFAULT_REMOTE_TIMEOUT),
            no_spawn: self.no_spawn,
        })
    }
}

/// `--timeout <10s|1500ms>` 解析（Go time.ParseDuration 的 CLI 子集；无单位拒绝——Go 同款）。
pub(crate) fn parse_duration(v: &str) -> Option<Duration> {
    let split = v.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(v.len());
    let (num, unit) = v.split_at(split);
    let n: f64 = num.parse().ok()?;
    let secs = match unit {
        "ns" => n / 1e9,
        "us" | "µs" => n / 1e6,
        "ms" => n / 1e3,
        "s" => n,
        "m" => n * 60.0,
        "h" => n * 3600.0,
        _ => return None,
    };
    // 溢出防护（评审 r2-D：from_secs_f64 超界 panic）。
    (secs > 0.0 && secs < 86400.0 * 365.0).then(|| Duration::from_secs_f64(secs))
}

/// 解析产物：公共参数 + 出现过的已知名 flag（动词自行消化）+ 带值 flag 的值对 + 位置会话名。
struct ParsedTermArgs {
    common: TermCommon,
    flags: Vec<String>,
    values: Vec<(String, String)>,
    name: String,
}

impl ParsedTermArgs {
    /// 带值 flag 的值（如 --detach-key；多次给 = 最后一次生效，Go 同款覆盖语义）。
    fn value_of(&self, flag: &str) -> Option<&str> {
        self.values.iter().rev().find(|(k, _)| k == flag).map(|(_, v)| v.as_str())
    }
}

/// 通用 flag 位解析（--state/--host/--timeout + 已知 flag 白名单 + 带值 flag；unknown flag 报错）。
fn parse_term_args(
    args: &[String],
    known_flags: &[&str],
    value_flags: &[&str],
    allow_name: bool,
) -> Result<ParsedTermArgs, String> {
    let mut out = ParsedTermArgs {
        common: TermCommon::default(),
        flags: Vec::new(),
        values: Vec::new(),
        name: String::new(),
    };
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        let take = |i: &mut usize, flag: &str| -> Result<String, String> {
            *i += 1;
            args.get(*i).cloned().ok_or_else(|| format!("{flag} 后面缺参数"))
        };
        match a.as_str() {
            "--state" => out.common.state_dir = Some(PathBuf::from(take(&mut i, &a)?)),
            "--host" => {
                let v = take(&mut i, &a)?;
                if v.trim().is_empty() {
                    return Err("--host 需要主机名或 ID（homeway-cli host list 查看在表主机）".to_owned());
                }
                out.common.host_ref = v;
            }
            "--timeout" => {
                let v = take(&mut i, &a)?;
                out.common.timeout = Some(parse_duration(&v).filter(|d| !d.is_zero()).ok_or_else(
                    || format!("--timeout {v:?} 不是合法时长（如 10s、1500ms）"),
                )?);
            }
            "--no-spawn" => out.common.no_spawn = true,
            other if value_flags.contains(&other) => {
                let v = take(&mut i, other)?;
                out.values.push((other.to_owned(), v));
            }
            other if other.starts_with('-') => {
                if !known_flags.contains(&other) {
                    let mut usable = String::from("--state <dir>、--host <name|id>、--timeout <时长>、--no-spawn");
                    if !known_flags.is_empty() {
                        usable.push_str(&format!("、{}", known_flags.join("、")));
                    }
                    if !value_flags.is_empty() {
                        usable.push_str(&format!("、{}", value_flags.join("、")));
                    }
                    return Err(format!("不认识的参数 {other:?}（可用：{usable}）"));
                }
                out.flags.push(other.to_owned());
            }
            other => {
                if !allow_name {
                    return Err(format!(
                        "list 不接受会话名（{other:?}）；省略名字接入最近活跃会话请用 attach"
                    ));
                }
                if !out.name.is_empty() {
                    return Err(format!("只能给一个会话名（已有 {:?}）", out.name));
                }
                out.name = other.to_owned();
            }
        }
        i += 1;
    }
    Ok(out)
}

fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(format!("会话名 {name:?} 不合法：只能是 [A-Za-z0-9._-]{{1,64}}"))
    }
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct SessionInfo {
    name: String,
    #[serde(rename = "lastActiveMs")]
    last_active_ms: i64,
    #[serde(rename = "stateV2")]
    state_v2: String,
    agent: String,
    title: String,
    cols: u16,
    rows: u16,
    clients: Vec<ClientInfo>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct ClientInfo {
    kind: String,
    active: bool,
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct SessionsWrap {
    sessions: Vec<SessionInfo>,
}

fn cli_list(args: &[String]) -> Result<(), String> {
    let p = parse_term_args(args, &["--json"], &[], false)?;
    let json_out = p.flags.iter().any(|f| f == "--json");
    let mut t = p.common.into_target()?;
    let (mut conn, _) = dial_term(&mut t)?;
    let raw = round_trip(&mut conn, Op::LIST, &[])?;
    conn.close();
    if json_out {
        let v: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|e| format!("LIST-REPLY 不是合法 JSON：{e}"))?;
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
        return Ok(());
    }
    let out: SessionsWrap =
        serde_json::from_slice(&raw).map_err(|e| format!("LIST-REPLY 不是合法 JSON：{e}"))?;
    print_sessions(&out.sessions);
    Ok(())
}

/// 取 LIST（表格与「attach 省略名字」的最近活跃解析共用）。
fn list_fetch(t: &mut TermTarget) -> Result<Vec<SessionInfo>, String> {
    let (mut conn, _) = dial_term(t)?;
    let raw = round_trip(&mut conn, Op::LIST, &[])?;
    conn.close();
    let out: SessionsWrap =
        serde_json::from_slice(&raw).map_err(|e| format!("LIST-REPLY 不是合法 JSON：{e}"))?;
    Ok(out.sessions)
}

fn print_sessions(entries: &[SessionInfo]) {
    if entries.is_empty() {
        println!("（当前没有会话；homeway-cli term new 可创建）");
        return;
    }
    // 最近活跃在前（与 attach 省略名字的选取一致）。
    let mut sorted: Vec<&SessionInfo> = entries.iter().collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(s.last_active_ms));
    println!("{:<20} {:<9} {:<9} {:<10} {:<24} CLIENTS", "NAME", "SIZE", "STATE", "AGENT", "TITLE");
    for s in sorted {
        let mut title = if s.title.is_empty() { "-".to_owned() } else { s.title.clone() };
        let r: Vec<char> = title.chars().collect();
        if r.len() > 22 {
            title = r[..21].iter().collect::<String>() + "…";
        }
        let clients = if s.clients.is_empty() {
            "-".to_owned()
        } else {
            s.clients
                .iter()
                .map(|cl| if cl.active { format!("{}*", cl.kind) } else { cl.kind.clone() })
                .collect::<Vec<_>>()
                .join(",")
        };
        println!(
            "{:<20} {:<9} {:<9} {:<10} {:<24} {}",
            s.name,
            format!("{}x{}", s.cols, s.rows),
            s.state_v2,
            s.agent,
            title,
            clients
        );
    }
}

/// 一锤子往返：发一帧、读应答；ERROR 帧翻协议错误文案。
fn round_trip(conn: &mut TermConn, op: Op, payload: &[u8]) -> Result<Vec<u8>, String> {
    conn.write_frame(op, payload).map_err(|e| e.message("?"))?;
    let f = conn.read_frame().map_err(|e| e.message("?"))?;
    if f.op == Op::ERROR {
        let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
        return Err(proto_error_text(&code, &msg));
    }
    Ok(f.payload)
}

/// protoError 文案（Go 同义：no_session 带刷新提示）。
fn proto_error_text(code: &str, msg: &str) -> String {
    match code {
        "no_session" => format!("{msg}；用 homeway-cli term list 查看当前会话"),
        _ if !msg.is_empty() => msg.to_owned(),
        _ => code.to_owned(),
    }
}

// ---------------------------------------------------------------------------
// new / delete
// ---------------------------------------------------------------------------

fn cli_new(args: &[String]) -> Result<(), String> {
    let p = parse_term_args(args, &["-d", "-A"], &[], true)?;
    let detached = p.flags.iter().any(|f| f == "-d");
    let reuse = p.flags.iter().any(|f| f == "-A");
    let mut name = p.name.clone();
    let mut auto_named = false;
    if name.is_empty() {
        auto_named = true;
    } else {
        validate_name(&name)?;
    }
    let timeout_given = p.common.timeout.is_some();
    let mut t = p.common.into_target()?;
    if detached {
        // `new -d`：CREATE op 创建不接入（bit0 = reuse-if-exists，极性与 HELLO bit1 相反）。
        let flags = if reuse { frames::create_flags::REUSE_IF_EXISTS } else { 0 };
        if auto_named {
            for _ in 0..AUTO_NAME_TRIES {
                let n = gen_auto_name();
                if create_once(&mut t, &n, flags, true)? {
                    println!("已创建会话 {n}（不接入）");
                    return Ok(());
                }
            }
            return Err("自动命名连续重名 8 次（运气太差）；请显式给名字：homeway-cli term new <名字> -d".to_owned());
        }
        create_once(&mut t, &name, flags, false)?;
        println!("已创建会话 {name}（不接入）");
        return Ok(());
    }
    // 默认：创建并接入（tmux 式 new-session）。
    if auto_named {
        name.clear();
    }
    attach_cmd(AttachOpts {
        name,
        host_ref: t.host_ref.clone(),
        state_dir: t.state_dir.clone(),
        timeout: t.timeout,
        timeout_given,
        no_spawn: t.no_spawn,
        create: true,
        reuse,
        takeover: false,
        auto_named,
        detach_key: String::new(),
    })
}

/// CREATE 一锤子（already_exists 判定走码面——自动命名重试的依据）。
/// Ok(false) = already_exists（仅 retry=true 时返回）。
fn create_once(t: &mut TermTarget, name: &str, flags: u8, retry: bool) -> Result<bool, String> {
    let (mut conn, _) = dial_term(t)?;
    conn.write_frame(Op::CREATE, &frames::enc_create(flags, name))
        .map_err(|e| e.message(name))?;
    let f = conn.read_frame().map_err(|e| e.message(name))?;
    conn.close();
    if f.op == Op::ERROR {
        let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
        if retry && code == "already_exists" {
            return Ok(false);
        }
        return Err(proto_error_text(&code, &msg));
    }
    if f.op != Op::OK {
        return Err(format!("期望 OK，收到 op 0x{:02x}", f.op.0));
    }
    Ok(true)
}

fn cli_delete(args: &[String]) -> Result<(), String> {
    let p = parse_term_args(args, &[], &[], true)?;
    if p.name.is_empty() {
        return Err("delete 需要会话名：homeway-cli term delete <name>".to_owned());
    }
    validate_name(&p.name)?;
    let name = p.name.clone();
    let mut t = p.common.into_target()?;
    let (mut conn, _) = dial_term(&mut t)?;
    conn.write_frame(Op::KILL, &frames::enc_name(&name))
        .map_err(|e| e.message(&name))?;
    let f = conn.read_frame().map_err(|e| e.message(&name))?;
    conn.close();
    if f.op == Op::ERROR {
        let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
        return Err(proto_error_text(&code, &msg));
    }
    if f.op != Op::OK {
        return Err(format!("期望 OK，收到 op 0x{:02x}", f.op.0));
    }
    println!("会话 {name} 已结束");
    Ok(())
}

/// 自动命名 host-<4hex>（与 App 的 <dev8>-<rand8> 可区分，Go 同源）。
fn gen_auto_name() -> String {
    let mut b = [0u8; 2];
    if getrandom::getrandom(&mut b).is_err() {
        b.fill(0);
    }
    format!("host-{:02x}{:02x}", b[0], b[1])
}

// ---------------------------------------------------------------------------
// attach / new（接入形态）——raw 终端客户端主体
// ---------------------------------------------------------------------------

struct AttachOpts {
    name: String,
    state_dir: PathBuf,
    host_ref: String,
    timeout: Duration,
    /// 用户显式给了 --timeout（本地面拒绝用；缺省补的 10s 不算）。
    timeout_given: bool,
    /// 守护未跑时不按需拉起（D-1 评审补齐——远程 attach 直组 TermTarget 的透传面）。
    no_spawn: bool,
    create: bool,
    reuse: bool,
    takeover: bool,
    auto_named: bool,
    detach_key: String,
}

fn cli_attach(args: &[String]) -> Result<(), String> {
    let p = parse_term_args(args, &["-d"], &["--detach-key"], true)?;
    let takeover = p.flags.iter().any(|f| f == "-d");
    let detach_key = p.value_of("--detach-key").unwrap_or_default().to_owned();
    attach_cmd(AttachOpts {
        name: p.name,
        state_dir: p.common.state_dir.clone().unwrap_or_else(default_state_dir),
        host_ref: p.common.host_ref.clone(),
        timeout: p.common.timeout.unwrap_or(DEFAULT_REMOTE_TIMEOUT),
        timeout_given: p.common.timeout.is_some(),
        no_spawn: p.common.no_spawn,
        create: false,
        reuse: false,
        takeover,
        auto_named: false,
        detach_key,
    })
}

fn attach_cmd(o: AttachOpts) -> Result<(), String> {
    let mut name = o.name.clone();
    if !name.is_empty() {
        if in_session(&name) {
            return Err(format!(
                "已在会话 {name} 里面（TERM_SESSION_ID 相同）：接入自身会让输出回环；换个会话，或先退出当前会话"
            ));
        }
        validate_name(&name)?;
    }
    // 非 TTY 拒绝（spec「非交互终端拒绝」）。
    if !is_tty(0) || !is_tty(1) {
        return Err("attach 需要交互终端（stdin/stdout 都是 TTY）；在管道/脚本里请用 list / new -d / delete".to_owned());
    }
    let tty = Tty::open().map_err(|e| format!("读终端属性失败：{e}"))?;
    let mut target = TermTarget {
        state_dir: o.state_dir.clone(),
        host_ref: o.host_ref.clone(),
        host_id: None,
        timeout: o.timeout,
        no_spawn: o.no_spawn,
    };
    // 本地面拒绝 --timeout（Go exec-r1 L6；远程缺省在 TermCommon::into_target 补，attach
    // 直组 TermTarget——此处同款校验）。
    if target.host_ref.is_empty() && o.timeout_given {
        return Err("--timeout 仅 --host 模式可用（远程的解析/打开预算）；本地面请去掉 --timeout".to_owned());
    }
    if name.is_empty() && !o.auto_named {
        // attach 省略名字 = 最近活跃（远程走同一拨号缝的远程 LIST）。
        name = pick_recent_session(&mut target)?;
        if in_session(&name) {
            return Err(format!(
                "最近活跃的会话就是当前会话（{name}）：接入自身会让输出回环；用 homeway-cli term attach <别的会话>"
            ));
        }
    }
    if o.auto_named {
        // 自动命名在这里重试（already_exists ⇒ 换名重来，最多 8 次）。
        for _ in 0..AUTO_NAME_TRIES {
            let n = gen_auto_name();
            match attach_dial(&mut target, &o, &tty, &n) {
                Ok((conn, attached)) => return attach_run(conn, &tty, attached, &n, &o),
                Err(e) if e == "already_exists" => continue,
                Err(e) => return Err(e),
            }
        }
        return Err("自动命名连续重名 8 次（运气太差）；请显式给名字：homeway-cli term new <名字>".to_owned());
    }
    let (conn, attached) = attach_dial(&mut target, &o, &tty, &name)?;
    attach_run(conn, &tty, attached, &name, &o)
}

/// TERM_SESSION_ID 回环检测（服务端在会话 env 里置 `tailcat-<名>`）。
fn in_session(name: &str) -> bool {
    std::env::var("TERM_SESSION_ID").as_deref() == Ok(format!("tailcat-{name}").as_str())
}

/// 最近活跃会话（attach 省略名字 = LIST 取 lastActiveMs 最大者）。
fn pick_recent_session(t: &mut TermTarget) -> Result<String, String> {
    let entries = list_fetch(t)?;
    let Some(best) = entries.iter().max_by_key(|s| s.last_active_ms) else {
        return Err("当前没有会话；homeway-cli term new 可创建一个，或 homeway-cli term list 查看".to_owned());
    };
    Ok(best.name.clone())
}

/// 经拨号缝接入 → GREETING → HELLO（caps=raw + 实例标识尾随 + 可选版本字节）→ ATTACHED。
fn attach_dial(
    t: &mut TermTarget,
    o: &AttachOpts,
    tty: &Tty,
    name: &str,
) -> Result<(TermConn, Vec<u8>), String> {
    let (mut conn, feats) = dial_term(t)?;
    let (cols, rows) = tty.size();
    let mut flags = 0u8;
    if o.create {
        flags |= hello_flags::CREATE;
        if !o.reuse {
            flags |= hello_flags::ONLY_IF_ABSENT; // `new <名字>`：重名报错、不静默接入
        }
    }
    if o.takeover {
        flags |= hello_flags::TAKEOVER;
    }
    let mut cap_bits = caps::RAW_TERMINAL;
    if feats & frames::features::PROTO_VER != 0 {
        cap_bits |= caps::PROTO_VER;
    }
    let hello = frames::enc_hello(
        cols,
        rows,
        flags,
        name,
        &frames::enc_hello_tail(cap_bits, true, &client_id()),
    );
    conn.write_frame(Op::HELLO, &hello)
        .map_err(|e| {
            conn.close();
            format!("发 HELLO：{}", e.message(name))
        })?;
    let f = conn
        .read_frame()
        .map_err(|e| {
            conn.close();
            format!("读 ATTACHED：{}", e.message(name))
        })?;
    if f.op == Op::ERROR {
        conn.close();
        let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
        // already_exists 以码面原样上抛（自动命名重试判定）。
        return Err(if code == "already_exists" { code } else { proto_error_text(&code, &msg) });
    }
    if f.op != Op::ATTACHED {
        conn.close();
        return Err(format!("首帧应为 ATTACHED，收到 op 0x{:02x}（服务端与 CLI 不是同代？）", f.op.0));
    }
    Ok((conn, f.payload))
}

// ---------------------------------------------------------------------------
// tty（termios / 尺寸 / 标题查询 / 实例标识）
// ---------------------------------------------------------------------------

struct Tty {
    saved: libc::termios,
}

impl Tty {
    fn open() -> std::io::Result<Self> {
        let mut t = std::mem::MaybeUninit::uninit();
        if unsafe { libc::tcgetattr(0, t.as_mut_ptr()) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Tty { saved: unsafe { t.assume_init() } })
    }

    /// raw 双向透传（cfmakeraw 等价；ISIG 关 ⇒ Ctrl-C 成为普通字节直达会话——
    /// 多路复用器的正确语义；进程自身的退出信号来自 kill 等，handler 兜底）。
    fn make_raw(&self) -> std::io::Result<()> {
        let mut raw = self.saved;
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &raw) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn restore(&self) -> std::io::Result<()> {
        if unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.saved) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn size(&self) -> (u16, u16) {
        let mut ws = libc::winsize { ws_row: 0, ws_col: 0, ws_xpixel: 0, ws_ypixel: 0 };
        let r = unsafe { libc::ioctl(0, libc::TIOCGWINSZ, &mut ws) };
        if r != 0 || ws.ws_col == 0 || ws.ws_row == 0 {
            (80, 24)
        } else {
            (ws.ws_col, ws.ws_row)
        }
    }
}

fn is_tty(fd: i32) -> bool {
    unsafe { libc::isatty(fd) == 1 }
}

/// 查询窗口内收应答 + 剩余字节回投（OSC 21；总预算 150ms；必须在 raw 模式下调）。
/// 返回 (标题, 非应答字节)——剩余字节 = 窗口内收到但不属于应答的部分（抢敲首键），
/// 调用方必须当正常输入回投（「首键不丢」）。
fn tty_query_title() -> (String, Vec<u8>) {
    let mut out = std::io::stdout();
    if out.write_all(b"\x1b]21;?\x07").is_err() || out.flush().is_err() {
        return (String::new(), Vec::new());
    }
    let deadline = Instant::now() + TITLE_QUERY_BUDGET;
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 512];
    while Instant::now() < deadline {
        let ms = deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .clamp(1, 150) as i32;
        let mut pfd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n <= 0 {
            break; // 超时/错误：放弃查询，已收字节当输入回投
        }
        let m = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
        if m > 0 {
            acc.extend_from_slice(&buf[..m as usize]);
        }
        if m <= 0 || acc.len() > 1024 {
            break;
        }
        let (title, rest, ok) = parse_title_reply(&acc);
        if ok {
            return (title, rest);
        }
    }
    // 超时：回投已剥掉可识别应答的余量（不应答/半应答的终端不把应答字节灌进会话）。
    let (_, rest, _) = parse_title_reply(&acc);
    (String::new(), rest)
}

/// OSC 21 应答解析（剥掉所有可识别的标题/图标应答序列；ST 与 BEL 结束符都认）。
/// 返回 (标题, 非应答余量, 是否收齐标题应答)。
fn parse_title_reply(acc: &[u8]) -> (String, Vec<u8>, bool) {
    let mut keep = Vec::new();
    let mut buf = acc;
    let mut title = String::new();
    let mut saw_title = false;
    while !buf.is_empty() {
        let Some(i) = buf.windows(2).position(|w| w == b"\x1b]") else {
            keep.extend_from_slice(buf);
            break;
        };
        keep.extend_from_slice(&buf[..i]);
        let body = &buf[i + 2..];
        if body.is_empty() {
            break; // OSC 起始恰在末尾：没收齐
        }
        let kind = body[0];
        if kind != b'L' && kind != b'l' {
            keep.extend_from_slice(buf); // 其它 OSC 序列不是标题/图标应答：剩余当输入回投
            break;
        }
        let Some((payload_end, next)) = osc_reply_end(&body[1..]) else {
            break; // 无结束符：没收齐（窗口内继续等后续分片）
        };
        if kind == b'L' {
            saw_title = true;
            if payload_end <= 128 {
                title = String::from_utf8_lossy(&body[1..1 + payload_end]).into_owned();
            }
        }
        // kind == 'l'：图标名应答，剥掉不回投（不是用户输入）。
        buf = &body[1 + next..];
    }
    (title, keep, saw_title)
}

/// OSC 应答载荷结束位置（先到者 ST（2B）或 BEL（1B））；None = 没收齐。
fn osc_reply_end(b: &[u8]) -> Option<(usize, usize)> {
    let bel = b.iter().position(|&c| c == 0x07);
    let st = b.windows(2).position(|w| w == b"\x1b\\");
    match (bel, st) {
        (None, None) => None,
        (Some(bel), None) => Some((bel, bel + 1)),
        (None, Some(st)) => Some((st, st + 2)),
        (Some(bel), Some(st)) if bel < st => Some((bel, bel + 1)),
        (_, Some(st)) => Some((st, st + 2)),
    }
}

/// 客户端实例标识：主机名 + uid + tty 设备路径的短哈希（同一终端重跑 ⇒ 同一标识
/// ⇒ 重连替换自身旧腿、不被自己的旧腿锁在腿数上限外；无 tty 退化为每进程随机）。
fn client_id() -> String {
    let tty_path = tty_name();
    if tty_path.is_empty() {
        let mut b = [0u8; 4];
        if getrandom::getrandom(&mut b).is_err() {
            b.fill(0);
        }
        return format!("rnd-{:02x}{:02x}{:02x}{:02x}", b[0], b[1], b[2], b[3]);
    }
    use sha2::{Digest, Sha256};
    let hostname = hostname_str();
    let uid = unsafe { libc::getuid() };
    let mut sum = Sha256::new();
    sum.update(format!("{hostname}\0{uid}\0{tty_path}"));
    let d = sum.finalize();
    format!("host-{:02x}{:02x}{:02x}{:02x}", d[0], d[1], d[2], d[3])
}

fn hostname_str() -> String {
    // gethostname 是唯一可靠面（/etc/hostname 是 debian 惯例，darwin 无）。
    let mut buf = [0u8; 256];
    let r = unsafe { libc::gethostname(buf.as_mut_ptr().cast(), buf.len()) };
    if r == 0 {
        let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        return String::from_utf8_lossy(&buf[..end]).into_owned();
    }
    String::new()
}

/// stdin 对应的 tty 设备路径（linux = /proc/self/fd/0 readlink；darwin = Rdev 反查
/// /dev/ttys*——Go cliTTYName 同路径）。
fn tty_name() -> String {
    if let Ok(s) = std::fs::read_link("/proc/self/fd/0") {
        let s = s.to_string_lossy().into_owned();
        if s.starts_with("/dev/tty") || s.starts_with("/dev/pts/") {
            return s;
        }
    }
    let mut st = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(0, st.as_mut_ptr()) } != 0 {
        return String::new();
    }
    let st = unsafe { st.assume_init() };
    if st.st_mode & libc::S_IFMT != libc::S_IFCHR {
        return String::new();
    }
    let Ok(entries) = std::fs::read_dir("/dev") else {
        return String::new();
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let Some(name) = name.to_str() else { continue };
        if !name.starts_with("ttys") {
            continue;
        }
        let mut dev = std::mem::MaybeUninit::<libc::stat>::uninit();
        let path = format!("/dev/{name}");
        if unsafe { libc::lstat(path.as_ptr().cast(), dev.as_mut_ptr()) } == 0 {
            let dev = unsafe { dev.assume_init() };
            if dev.st_rdev == st.st_rdev {
                return path;
            }
        }
    }
    String::new()
}

// ---------------------------------------------------------------------------
// 分离键状态机（Go detachMachine 同语义）
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetachAction {
    None,
    Detach,
    Realign,
}

fn detach_key_spec(s: &str) -> Result<(u8, bool), String> {
    match s.trim().to_ascii_lowercase().as_str() {
        "" | "^b" => return Ok((0x02, true)),
        "none" | "off" => return Ok((0, false)),
        _ => {}
    }
    let b = s.as_bytes();
    if b.len() == 2 && b[0] == b'^' {
        return Ok((caret_byte(b[1]), true));
    }
    if b.len() == 1 && b[0] > 0x1f && b[0] != 0x7f {
        return Ok((b[0], true));
    }
    Err(format!(
        "--detach-key {s:?} 不认识：可用 '^x'（如 ^b、^]）、单字符（如 x）、none（关闭）"
    ))
}

/// '^' 后一字符 → 控制字节（^@=0 … ^A–^Z … ^[=ESC ^\\ ^] ^^ ^_ ^?=DEL）。
fn caret_byte(c: u8) -> u8 {
    match c {
        b'a'..=b'z' => c - b'a' + 1,
        b'A'..=b'Z' => c - b'A' + 1,
        b'@' => 0,
        b'['..=b'_' => c - b'[' + 0x1b,
        b'?' => 0x7f,
        _ => c,
    }
}

/// 分离键状态机：前缀键默认 Ctrl-b（tmux 同款）——d=分离、r=重对齐、前缀前缀=字面量；
/// 前缀后跟其它字节 = 前缀与该字节**都透传**（screen 风格，绝不吞用户输入）；
/// 状态跨 feed 调用保留。
struct DetachMachine {
    prefix: u8,
    enabled: bool,
    saw_prefix: bool,
}

impl DetachMachine {
    fn new(prefix: u8, enabled: bool) -> Self {
        DetachMachine { prefix, enabled, saw_prefix: false }
    }

    fn feed(&mut self, input: &[u8]) -> (Vec<u8>, DetachAction) {
        let mut pass = Vec::new();
        let mut action = DetachAction::None;
        for &b in input {
            if !self.enabled {
                pass.push(b);
                continue;
            }
            if self.saw_prefix {
                self.saw_prefix = false;
                if b == self.prefix {
                    // 前缀前缀 = 字面量。匹配**前置于** d/r：--detach-key 恰为字面
                    // d/r 时 dd/rr 发字面量而不是动作（病态选键下动作不可达、字面量可达）。
                    pass.push(self.prefix);
                } else if b == b'd' {
                    return (pass, DetachAction::Detach);
                } else if b == b'r' {
                    if action == DetachAction::None {
                        action = DetachAction::Realign; // 输入不透传；同段后续字节继续处理
                    }
                } else {
                    pass.push(self.prefix);
                    pass.push(b);
                }
                continue;
            }
            if b == self.prefix {
                self.saw_prefix = true;
                continue;
            }
            pass.push(b);
        }
        (pass, action)
    }
}

// ---------------------------------------------------------------------------
// 信号面（SIGWINCH → 尺寸同步；退出族 → 干净退出）
// ---------------------------------------------------------------------------

static SIG_W: AtomicI32 = AtomicI32::new(-1);

extern "C" fn on_sigwinch(_sig: i32) {
    let fd = SIG_W.load(Ordering::SeqCst);
    if fd >= 0 {
        let b = b"w";
        unsafe { libc::write(fd, b.as_ptr().cast(), 1) };
    }
}

extern "C" fn on_sig_exit(_sig: i32) {
    let fd = SIG_W.load(Ordering::SeqCst);
    if fd >= 0 {
        let b = b"x";
        unsafe { libc::write(fd, b.as_ptr().cast(), 1) };
    }
}

struct SigPipe {
    read_fd: i32,
    write_fd: i32,
}

impl Copy for SigPipe {}

impl Clone for SigPipe {
    fn clone(&self) -> Self {
        *self
    }
}

fn install_signal_pipes() -> SigPipe {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        let e = std::io::Error::last_os_error();
        eprintln!("homeway term: 信号管道建立失败（{e}）——退出");
        std::process::exit(1);
    }
    SIG_W.store(fds[1], Ordering::SeqCst);
    unsafe {
        let h_winch = on_sigwinch as extern "C" fn(i32) as libc::sighandler_t;
        let h_exit = on_sig_exit as extern "C" fn(i32) as libc::sighandler_t;
        libc::signal(libc::SIGWINCH, h_winch);
        libc::signal(libc::SIGTERM, h_exit);
        libc::signal(libc::SIGINT, h_exit);
        libc::signal(libc::SIGHUP, h_exit);
        libc::signal(libc::SIGQUIT, h_exit);
    }
    SigPipe { read_fd: fds[0], write_fd: fds[1] }
}

fn drop_signal_pipes(p: SigPipe) {
    SIG_W.store(-1, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGWINCH, libc::SIG_DFL);
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGINT, libc::SIG_DFL);
        libc::signal(libc::SIGHUP, libc::SIG_DFL);
        libc::signal(libc::SIGQUIT, libc::SIG_DFL);
        libc::close(p.read_fd);
        libc::close(p.write_fd);
    }
}

// ---------------------------------------------------------------------------
// attach 主体（raw 模式 + 双向透传 + 分离键 + 尺寸同步 + 收尾）
// ---------------------------------------------------------------------------

/// ENDED 码 → 可行动文案（Go endedMessage 同串；前缀「homeway term: 」由 finish
/// 的收尾打印统一加——Go 侧 endedMessage 自带前缀而 detach/信号 note 不带、各打印
/// 点单加一次，Rust 侧收敛为 printer 单加，避免双前缀）。
fn ended_message(name: &str, code: i32, reason: &str) -> String {
    match (code, reason) {
        (c, r) if c == ended_code::REPLACED && r == frames::ended_reason::SELF_RECONNECT => {
            format!("本实例的新连接替换了这条腿（self_reconnect）；重新接入：homeway-cli term attach {name}")
        }
        (c, r) if c == ended_code::REPLACED && r == frames::ended_reason::REPLACED => {
            format!("会话 {name} 已被另一客户端接管（replaced）；重新接入：homeway-cli term attach {name}")
        }
        (c, r) if c == ended_code::REPLACED => {
            format!("会话 {name} 的这条腿被服务端结束（{r}）")
        }
        (c, _) if c == ended_code::KILLED => {
            format!("会话 {name} 已被关闭（App 或 homeway-cli term delete）")
        }
        (c, _) if c == ended_code::SERVICE_STOPPED => {
            format!("出口服务正在退出，会话 {name} 已结束")
        }
        (c, _) if c >= 0 => format!("会话 {name} 已结束（退出码 {c}）"),
        (c, r) => format!("会话 {name} 已结束（code={c} reason={r}）"),
    }
}

/// 标题行（会话 · agent · 状态〔· 标题〕——stateV2 词面）。
fn title_line(name: &str, agent: u8, state: u8, title: &str) -> String {
    let mut parts = vec![
        name.to_owned(),
        frames::agent::name(agent).to_owned(),
        frames::state_v2::name(state).to_owned(),
    ];
    let t = title.trim();
    if !t.is_empty() {
        let r: Vec<char> = t.chars().collect();
        parts.push(if r.len() > 24 { r[..23].iter().collect::<String>() + "…" } else { t.to_owned() });
    }
    parts.join(" · ")
}

fn set_title(line: &str) {
    let mut out = std::io::stdout();
    let _ = out.write_all(format!("\x1b]2;{line}\x07").as_bytes());
    let _ = out.flush();
}

/// attach 主体：raw 模式 + 双向透传 + 分离键 + 尺寸同步 + 任何退出路径还原终端。
fn attach_run(
    mut conn: TermConn,
    tty: &Tty,
    attached: Vec<u8>,
    name: &str,
    o: &AttachOpts,
) -> Result<(), String> {
    let (prefix, key_enabled) = detach_key_spec(&o.detach_key)?;
    let title_on = !std::env::var("HOMEWAY_TERM_TITLE")
        .unwrap_or_default()
        .eq_ignore_ascii_case("off");

    if let Err(e) = tty.make_raw() {
        conn.close();
        return Err(format!("切换 raw 模式失败：{e}"));
    }
    // 信号管道先装（查询窗之前——SIGTERM 落在窗内不能被 poll 吞掉）。
    let pipes = install_signal_pipes();
    // OSC 21 查询（150ms 预算；剩余字节回投——首键不丢）。
    let (old_title, query_leftover) = if title_on { tty_query_title() } else { (String::new(), Vec::new()) };

    // 数据到达管道（读腿 poke —— 主循环 poll 面的第三源）。写端非阻塞（评审
    // r2-7/r2-12：满管 EAGAIN 丢弃无害；建立失败 = 可行动错误退出——裸 -1 会把
    // SIG_W/poke 指到 fd 0〔stdin〕）。
    let mut poke_fds = [0i32; 2];
    if unsafe { libc::pipe(poke_fds.as_mut_ptr()) } != 0 {
        let e = std::io::Error::last_os_error();
        conn.close();
        let _ = tty.restore();
        return Err(format!("poke 管道建立失败：{e}"));
    }
    unsafe {
        let fl = libc::fcntl(poke_fds[1], libc::F_GETFL);
        libc::fcntl(poke_fds[1], libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
    // 连接分两半：读腿线程持读半（帧 → 通道 + 数据管道 poke），主线程持写半。
    let (read_half, write_half) = match conn.split() {
        Ok(v) => v,
        Err(e) => {
            drop_signal_pipes(pipes);
            let _ = tty.restore();
            return Err(e);
        }
    };
    let write_half = Mutex::new(write_half);
    struct ReaderShared {
        stop: AtomicBool,
        fail: Mutex<Option<TermReadErr>>,
    }
    let shared = Arc::new(ReaderShared { stop: AtomicBool::new(false), fail: Mutex::new(None) });
    let (data_tx, data_rx) = mpsc::channel::<frames::Frame>();
    let reader = {
        let shared = Arc::clone(&shared);
        let poke_w = poke_fds[1];
        std::thread::Builder::new()
            .name("hw-term-read".to_owned())
            .spawn(move || {
                let mut rh = read_half;
                loop {
                    if shared.stop.load(Ordering::SeqCst) {
                        return;
                    }
                    match rh.read_frame() {
                        Ok(f) => {
                            if data_tx.send(f).is_err() {
                                return;
                            }
                            poke(poke_w); // 非阻塞唤醒主循环
                        }
                        Err(e) => {
                            *shared.fail.lock().unwrap() = Some(e);
                            poke(poke_w);
                            return;
                        }
                    }
                }
            })
            .expect("线程创建不可失败")
    };
    // join 句柄交给 finish（Option 承载——join 消耗值）。
    let mut reader = Some(reader);

    // 统一收尾：停读腿 → 关连接 → 还原信号面/终端/标题。
    let mut finish = |note: Option<String>| -> Result<(), String> {
        shared.stop.store(true, Ordering::SeqCst);
        write_half.lock().unwrap().close(); // 连接级断开解阻塞读腿
        if let Some(h) = reader.take() {
            let _ = h.join();
        }
        drop_signal_pipes(pipes);
        unsafe {
            libc::close(poke_fds[0]);
            libc::close(poke_fds[1]);
        }
        if title_on && !old_title.is_empty() {
            set_title(&old_title);
        }
        if let Err(rerr) = tty.restore() {
            eprintln!("\rhomeway term: 终端还原失败（{rerr}）；画面混乱时执行 reset 修复\r");
        }
        if let Some(n) = note {
            eprintln!("\rhomeway term: {n}\r");
        }
        Ok(())
    };

    let send_resize = |wh: &mut TermWriteHalf| {
        let (cols, rows) = tty.size();
        let _ = wh.write_frame(Op::RESIZE, &frames::enc_resize(cols, rows));
    };
    if title_on {
        let (agent, state) = attached_agent_state(&attached);
        set_title(&title_line(name, agent, state, ""));
    }

    // 输入路径状态机 + 查询窗抢敲字节回投（首键不丢）。
    let mut dm = DetachMachine::new(prefix, key_enabled);
    if !query_leftover.is_empty() {
        let (pass, action) = dm.feed(&query_leftover);
        if !pass.is_empty() {
            if let Err(e) = send_data(&write_half, &pass) {
                let msg = e.message(name);
                return finish(None).and(Err(msg));
            }
        }
        match action {
            DetachAction::Realign => send_resize(&mut write_half.lock().unwrap()),
            DetachAction::Detach => {
                let note = format!("已分离（会话 {name} 继续在出口运行）");
                return finish(Some(note));
            }
            DetachAction::None => {}
        }
    }

    // 主循环：poll([stdin, 信号管道, 数据管道]) —— 输入/输出/尺寸三源低延迟。
    let mut in_buf = [0u8; 8192];
    loop {
        let mut pfds = [
            libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: pipes.read_fd, events: libc::POLLIN, revents: 0 },
            libc::pollfd { fd: poke_fds[0], events: libc::POLLIN, revents: 0 },
        ];
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), 3, -1) };
        if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            let msg = format!("poll 失败：{e}");
            return finish(None).and(Err(msg));
        }
        // ① term 帧（DATA → stdout；STATE → 标题；ENDED → 文案退出；ERROR → 协议错误）。
        if pfds[2].revents & libc::POLLIN != 0 {
            drain_poke(poke_fds[0]);
            loop {
                match data_rx.try_recv() {
                    Ok(f) => {
                        let mut out = std::io::stdout();
                        match f.op {
                            Op::DATA => {
                                if out.write_all(&f.payload).is_err() || out.flush().is_err() {
                                    let msg = "写本地终端失败".to_owned();
                                    return finish(None).and(Err(msg));
                                }
                            }
                            Op::ATTACHED | Op::REPLAY_DONE => {} // 回放/重复握手帧：不做事
                            Op::STATE => {
                                if title_on {
                                    let (agent, state, title) = frames::dec_state(&f.payload);
                                    set_title(&title_line(name, agent, state, &title));
                                }
                            }
                            Op::ENDED => {
                                let (code, reason) = frames::dec_ended(&f.payload);
                                let msg = ended_message(name, code, &reason);
                                return finish(Some(msg));
                            }
                            Op::ERROR => {
                                let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
                                let text = proto_error_text(&code, &msg);
                                return finish(None).and(Err(text));
                            }
                            other => {
                                // 防御：raw 腿只该收 DATA/STATE/ENDED/ERROR（surface 区段
                                // 帧绝不发给 raw 腿）。
                                eprintln!("\rhomeway term: 忽略未知帧 op 0x{:02x}（{}B）\r", other.0, f.payload.len());
                            }
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // 读腿终结：远程面三态归因；本地面断链文案。
                        let e = shared
                            .fail
                            .lock()
                            .unwrap()
                            .take()
                            .unwrap_or_else(|| TermReadErr::Io("连接已断开".to_owned()));
                        let msg = e.message(name);
                        return finish(None).and(Err(msg));
                    }
                }
            }
        }
        // ② 信号（SIGWINCH → RESIZE；退出族 → 干净退出）。
        if pfds[1].revents & libc::POLLIN != 0 {
            let mut b = [0u8; 8];
            let m = unsafe { libc::read(pipes.read_fd, b.as_mut_ptr().cast(), 8) };
            let mut exit_sig = false;
            let mut resize = false;
            for &c in &b[..m.max(0) as usize] {
                if c == b'x' {
                    exit_sig = true;
                } else {
                    resize = true;
                }
            }
            if resize {
                send_resize(&mut write_half.lock().unwrap());
            }
            if exit_sig {
                let note = "收到信号退出（会话仍在运行）".to_owned();
                return finish(Some(note));
            }
        }
        // ③ stdin（分离键状态机 → DATA / 动作）。
        if pfds[0].revents & libc::POLLIN != 0 {
            let n = unsafe { libc::read(0, in_buf.as_mut_ptr().cast(), in_buf.len()) };
            if n > 0 {
                let (pass, action) = dm.feed(&in_buf[..n as usize]);
                if !pass.is_empty() {
                    if let Err(e) = send_data(&write_half, &pass) {
                        // 写失败：读端也会失败并给三态文案——统一归因收尾。
                        let msg = e.message(name);
                        return finish(None).and(Err(msg));
                    }
                }
                match action {
                    DetachAction::Realign => send_resize(&mut write_half.lock().unwrap()),
                    DetachAction::Detach => {
                        let note = format!("已分离（会话 {name} 继续在出口运行）");
                        return finish(Some(note));
                    }
                    DetachAction::None => {}
                }
            } else if n == 0 {
                // stdin 关了（tty master 被关）：干净收尾。
                return finish(None);
            }
        }
    }
}

/// 读腿 poke（非阻塞写——主循环停转时由管道容量背压，EAGAIN 忽略）。
fn poke(fd: i32) {
    // 非阻塞写（评审 r2-7：注释写「非阻塞」但管道是阻塞的——积压满时会挂住读腿）；
    // EAGAIN 丢弃无害（管道里已有未消费的唤醒字节）。
    let b = b"d";
    unsafe { libc::write(fd, b.as_ptr().cast(), 1) };
}

/// 排空唤醒管道。**读一次即返**（评审 r2-7：原「读到 <64 才收手」在恰好整段时
/// 会对空管道再读一次 = 主循环永久挂死；唤醒语义只需清一次——data_rx 队列由
/// try_recv 循环自己抽干，poke 只承担唤醒）。
fn drain_poke(fd: i32) {
    let mut b = [0u8; 256];
    unsafe {
        libc::read(fd, b.as_mut_ptr().cast(), b.len());
    }
}

/// 终端输入按 ≤16KiB 分片发 DATA（与 App 侧契约一致；远程腿经 stream_send 逐帧查
/// 流终结——死流在首个失败帧返回，余量不推进）。
fn send_data(wh: &Mutex<TermWriteHalf>, p: &[u8]) -> Result<(), TermReadErr> {
    let mut wh = wh.lock().unwrap();
    for chunk in p.chunks(frames::DATA_CHUNK) {
        wh.write_frame(Op::DATA, chunk)?;
    }
    Ok(())
}

/// ATTACHED 载荷取 agent/state。
fn attached_agent_state(p: &[u8]) -> (u8, u8) {
    match frames::dec_attached(p) {
        Ok((_, _, _, agent, state, _)) => (agent, state),
        Err(_) => (frames::agent::UNKNOWN, frames::state_v2::UNKNOWN),
    }
}

// ---------------------------------------------------------------------------
// explain（离线 manifest 判定 / 在线 EXPLAIN 往返）
// ---------------------------------------------------------------------------

fn cli_explain(args: &[String]) -> Result<(), String> {
    // explain 的语法自成一格（--file/--agent 带值、<会话名> 位置参数）——独立解析，
    // 不与 list/new/attach 的公共 flag 面混用。
    let mut file = String::new();
    let mut agent = String::new();
    let mut session = String::new();
    let mut common = TermCommon::default();
    let mut json_out = false;
    let mut i = 0;
    while i < args.len() {
        let a = args[i].clone();
        let take = |i: &mut usize, flag: &str| -> Result<String, String> {
            *i += 1;
            args.get(*i).cloned().ok_or_else(|| format!("{flag} 后面缺参数"))
        };
        match a.as_str() {
            "--file" => file = take(&mut i, &a)?,
            "--agent" => agent = take(&mut i, &a)?,
            "--state" => common.state_dir = Some(PathBuf::from(take(&mut i, &a)?)),
            "--host" => {
                let v = take(&mut i, &a)?;
                if v.trim().is_empty() {
                    return Err("--host 需要主机名或 ID（homeway-cli host list 查看在表主机）".to_owned());
                }
                common.host_ref = v;
            }
            "--timeout" => {
                let v = take(&mut i, &a)?;
                common.timeout = Some(parse_duration(&v).filter(|d| !d.is_zero()).ok_or_else(|| {
                    format!("--timeout {v:?} 不是合法时长（如 10s、1500ms）")
                })?);
            }
            "--json" => json_out = true,
            other if other.starts_with('-') => return Err(format!("不认识的参数 {other:?}")),
            other => {
                if !session.is_empty() {
                    return Err(format!("只能给一个会话名（已有 {session:?}）"));
                }
                session = other.to_owned();
            }
        }
        i += 1;
    }
    if !file.is_empty() {
        if !common.host_ref.is_empty() {
            return Err("--file 是本地面（规则判定在本地 manifest），不接受 --host；对远程主机的会话取实时判定：homeway-cli term explain <会话名> --host <name|id>".to_owned());
        }
        if common.timeout.is_some() {
            return Err("--timeout 仅 --host 模式可用（远程的解析/打开预算）；--file 离线模式请去掉 --timeout".to_owned());
        }
        if agent.is_empty() {
            return Err("--file 模式必须给 --agent（哪份 manifest）".to_owned());
        }
        return explain_file(&file, &agent, common.state_dir.as_deref(), json_out);
    }
    if session.is_empty() {
        return Err("需要 --file <屏幕文本> 或 <会话名>（见 homeway-cli term --help）".to_owned());
    }
    let mut t = common.into_target()?;
    explain_session(&session, &mut t, json_out)
}

fn explain_file(file: &str, agent: &str, state_dir: Option<&std::path::Path>, json_out: bool) -> Result<(), String> {
    let data = std::fs::read_to_string(file).map_err(|e| format!("读屏幕文件：{e}"))?;
    let dir = state_dir.map(PathBuf::from).unwrap_or_else(default_state_dir);
    let loader = homeway_core::term::manifest::Loader::new(Some(
        &dir.join(homeway_core::term::manifest::OVERRIDE_DIR_NAME),
    ));
    let comp = loader.for_process(agent).or_else(|| loader.for_id(agent)).ok_or_else(|| {
        let ids = loader.ids().join(" ");
        format!("认不出 agent {agent:?}（可用：{ids}）")
    })?;
    let input = homeway_core::term::manifest::region::Input {
        screen: data,
        ..Default::default()
    };
    let res = comp.evaluate(&input);
    if json_out {
        // JSON 字段名与 Go explainOutput 对齐（脚本契约面）。
        let matched = res.matched_rule.as_ref().map(|r| {
            serde_json::json!({
                "id": r.id, "priority": r.priority, "region": r.region,
                "state": r.state.as_str(),
            })
        });
        let rules: Vec<serde_json::Value> = res
            .rules
            .iter()
            .map(|r| {
                serde_json::json!({
                    "id": r.id, "priority": r.priority, "region": r.region,
                    "state": r.state.as_str(), "matched": r.matched,
                    "regionBytes": r.region_bytes,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "agent": agent,
                "manifestSource": comp.manifest.source.as_str(),
                "manifestVersion": comp.manifest.version,
                "state": res.state.as_str(),
                "fallbackReason": res.fallback_reason,
                "matchedRule": matched,
                "visibleIdle": res.visible_idle,
                "visibleBlocker": res.visible_blocker,
                "visibleWorking": res.visible_working,
                "skipStateUpdate": res.skip_state_update,
                "rules": rules,
                "warnings": loader.warnings(),
                "screenBytes": input.screen.len(),
            })
        );
        return Ok(());
    }
    println!("agent：{agent}（manifest={} 来源={}）", comp.manifest.version, comp.manifest.source.as_str());
    println!("屏幕文本：{} 字节", input.screen.len());
    println!(
        "判定：{}{}",
        res.state.as_str(),
        if res.fallback_reason.is_empty() {
            String::new()
        } else {
            format!("（回落：{}）", res.fallback_reason)
        }
    );
    match &res.matched_rule {
        Some(r) => println!(
            "命中规则：{}（priority={} region={} state={}）",
            r.id, r.priority, r.region, r.state.as_str()
        ),
        None => println!("命中规则：无"),
    }
    println!(
        "可见证据位：idle={} blocker={} working={}；冻结状态={}",
        res.visible_idle, res.visible_blocker, res.visible_working, res.skip_state_update
    );
    let warnings = loader.warnings();
    if !warnings.is_empty() {
        println!("告警：{}", warnings.join("；"));
    }
    println!("\n全部规则评估轨迹（{} 条，★=命中）：", res.rules.len());
    for r in &res.rules {
        println!(
            " {} {:<36} p={:<5} region={:<30} state={:<7} region字节={}",
            if r.matched { "★" } else { " " },
            r.id,
            r.priority,
            r.region,
            r.state.as_str(),
            r.region_bytes
        );
    }
    Ok(())
}

fn explain_session(session: &str, t: &mut TermTarget, json_out: bool) -> Result<(), String> {
    let (mut conn, _) = dial_term(t)?;
    conn.write_frame(Op::EXPLAIN, &frames::enc_name(session))
        .map_err(|e| e.message(session))?;
    let f = conn
        .read_frame()
        .map_err(|e| {
            conn.close();
            format!("读 EXPLAIN 应答：{}", e.message(session))
        })?;
    conn.close();
    if f.op == Op::ERROR {
        let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
        return Err(format!("{code}：{msg}"));
    }
    if f.op != Op::EXPLAIN {
        return Err(format!("期望 EXPLAIN 应答，收到 op 0x{:02x}", f.op.0));
    }
    let v: serde_json::Value =
        serde_json::from_slice(&f.payload).map_err(|e| format!("EXPLAIN 应答不是合法 JSON：{e}"))?;
    if json_out {
        println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
    } else {
        println!("会话：{session}");
        println!("agent：{}", v["agent"].as_str().unwrap_or("?"));
        println!("判定：{}", v["state"].as_str().unwrap_or("?"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detach_key_spec_forms() {
        assert_eq!(detach_key_spec("").unwrap(), (0x02, true));
        assert_eq!(detach_key_spec("^b").unwrap(), (0x02, true));
        assert_eq!(detach_key_spec("^B").unwrap(), (0x02, true));
        assert_eq!(detach_key_spec("none").unwrap(), (0, false));
        assert_eq!(detach_key_spec("x").unwrap(), (b'x', true));
        assert_eq!(detach_key_spec("^]").unwrap(), (0x1d, true));
        assert_eq!(detach_key_spec("^?").unwrap(), (0x7f, true));
        assert_eq!(detach_key_spec("^@").unwrap(), (0, true));
        // Go caretByte default 分支：未知插入字符原样接受（^1 = 字面量 0x31）。
        assert_eq!(detach_key_spec("^1").unwrap(), (b'1', true));
        assert!(detach_key_spec("ab").is_err());
    }

    #[test]
    fn detach_machine_semantics() {
        // d 分离
        let mut m = DetachMachine::new(0x02, true);
        let (pass, a) = m.feed(&[0x02, b'd']);
        assert!(pass.is_empty());
        assert_eq!(a, DetachAction::Detach);
        // r 重对齐 + 后续字节继续处理（会话不收尾）
        let mut m = DetachMachine::new(0x02, true);
        let (pass, a) = m.feed(&[0x02, b'r', b'a']);
        assert_eq!(pass, vec![b'a']);
        assert_eq!(a, DetachAction::Realign);
        // 前缀前缀 = 字面量
        let mut m = DetachMachine::new(0x02, true);
        let (pass, a) = m.feed(&[0x02, 0x02, b'z']);
        assert_eq!(pass, vec![0x02, b'z']);
        assert_eq!(a, DetachAction::None);
        // 未绑定组合：前缀与字节都透传（screen 风格，绝不吞输入）
        let mut m = DetachMachine::new(0x02, true);
        let (pass, a) = m.feed(&[0x02, b'q']);
        assert_eq!(pass, vec![0x02, b'q']);
        assert_eq!(a, DetachAction::None);
        // 跨 feed 状态保留（前缀字节与后续字节分属两次 read）
        let mut m = DetachMachine::new(0x02, true);
        let (_, a) = m.feed(&[b'a', 0x02]);
        assert_eq!(a, DetachAction::None);
        let (_, a) = m.feed(b"d");
        assert_eq!(a, DetachAction::Detach);
        // 关闭：全透传
        let mut m = DetachMachine::new(0, false);
        let (pass, a) = m.feed(&[0x02, b'd']);
        assert_eq!(pass, vec![0x02, b'd']);
        assert_eq!(a, DetachAction::None);
        // 病态选键：前缀恰为字面量 d 时 dd 发字面量（前缀前缀匹配前置于动作）
        let mut m = DetachMachine::new(b'd', true);
        let (pass, a) = m.feed(b"dd");
        assert_eq!(pass, vec![b'd']);
        assert_eq!(a, DetachAction::None);
    }

    #[test]
    fn caret_byte_table() {
        assert_eq!(caret_byte(b'a'), 1);
        assert_eq!(caret_byte(b'Z'), 26);
        assert_eq!(caret_byte(b'['), 0x1b);
        assert_eq!(caret_byte(b'_'), 0x1f);
        assert_eq!(caret_byte(b'?'), 0x7f);
        assert_eq!(caret_byte(b'@'), 0);
    }

    #[test]
    fn title_reply_parse() {
        // BEL 结尾的标题应答 + 抢敲首键
        let (title, rest, ok) = parse_title_reply(b"\x1b]Lmy-title\x07x");
        assert_eq!((title.as_str(), ok), ("my-title", true));
        assert_eq!(rest, b"x".to_vec());
        // ST 结尾 + 图标应答（先 l 后 L——xterm 序）
        let (title, rest, ok) = parse_title_reply(b"\x1b]licon\x1b\\\x1b]Lreal\x1b\\z");
        assert_eq!((title.as_str(), ok), ("real", true));
        assert_eq!(rest, b"z".to_vec());
        // 未收齐（无结束符）→ ok=false
        let (_, _, ok) = parse_title_reply(b"\x1b]Lhalf");
        assert!(!ok);
        // 非 l/L OSC：不吞（当输入回投）
        let (_, rest, _) = parse_title_reply(b"\x1b]0;other\x07k");
        assert_eq!(rest, b"\x1b]0;other\x07k".to_vec());
        // 荒谬长标题（>128）：不当标题、但仍是应答（剥掉，不回投）
        let long: Vec<u8> = [b"\x1b]L".as_slice(), &[b'A'; 200][..], b"\x07".as_slice()].concat();
        let (title, rest, ok) = parse_title_reply(&long);
        assert_eq!(title, "");
        assert!(ok);
        assert!(rest.is_empty());
    }

    #[test]
    fn osc_reply_end_ordering() {
        assert_eq!(osc_reply_end(b"abc\x07de"), Some((3, 4)));
        assert_eq!(osc_reply_end(b"abc\x1b\\de"), Some((3, 5)));
        assert_eq!(osc_reply_end(b"a\x07b\x1b\\c"), Some((1, 2))); // BEL 先到
        assert_eq!(osc_reply_end(b"a\x1b\\b\x07c"), Some((1, 3))); // ST 先到
        assert_eq!(osc_reply_end(b"abc"), None);
    }

    #[test]
    fn duration_parsing() {
        assert_eq!(parse_duration("10s"), Some(Duration::from_secs(10)));
        assert_eq!(parse_duration("1500ms"), Some(Duration::from_millis(1500)));
        assert_eq!(parse_duration("2m"), Some(Duration::from_secs(120)));
        assert!(parse_duration("0s").is_none());
        assert!(parse_duration("abc").is_none());
        // 无单位拒绝（Go time.ParseDuration 同款——「missing unit」）。
        assert!(parse_duration("10").is_none());
    }

    #[test]
    fn flag_eq_expansion() {
        let eq = |s: &str| -> Vec<String> {
            expand_flag_eq(&s.split_whitespace().map(str::to_owned).collect::<Vec<_>>())
        };
        assert_eq!(
            eq("attach --detach-key=^] s1"),
            vec!["attach", "--detach-key", "^]", "s1"]
        );
        assert_eq!(eq("list --json"), vec!["list", "--json"]);
        // `--=x`（= 恰在 -- 后）不拆——i > 0 条件不满足。
        assert_eq!(eq("--=x"), vec!["--=x"]);
    }

    #[test]
    fn attach_arg_parsing_with_value_flag() {
        // --detach-key 的值不是 flag、不能落进会话名位（收编批修复的回归）。
        let args = ["s1".to_owned(), "--detach-key".to_owned(), "^]".to_owned()];
        let p = parse_term_args(&args, &["-d"], &["--detach-key"], true).unwrap();
        assert_eq!(p.name, "s1");
        assert_eq!(p.value_of("--detach-key"), Some("^]"));
        // 缺值报错。
        let args = ["--detach-key".to_owned()];
        assert!(parse_term_args(&args, &["-d"], &["--detach-key"], true).is_err());
        // 未知 flag 报错（带值 flag 进可用清单提示）。
        let args = ["-z".to_owned()];
        assert!(parse_term_args(&args, &["-d"], &["--detach-key"], true).is_err());
    }

    #[test]
    fn local_mode_rejects_timeout() {
        // Go exec-r1 L6：本地面给 --timeout = 显式报错（不静默忽略）。
        let c = TermCommon { state_dir: None, host_ref: String::new(), timeout: Some(Duration::from_secs(5)), no_spawn: false };
        assert!(c.into_target().is_err());
        let c = TermCommon { state_dir: None, host_ref: String::new(), timeout: None, no_spawn: false };
        assert!(c.into_target().is_ok());
        let c = TermCommon {
            state_dir: None,
            host_ref: "exit1".to_owned(),
            timeout: Some(Duration::from_secs(5)),
            no_spawn: false,
        };
        assert!(c.into_target().is_ok());
    }

    #[test]
    fn ended_message_vocab() {
        assert!(ended_message("s1", ended_code::REPLACED, "self_reconnect").contains("self_reconnect"));
        assert!(ended_message("s1", ended_code::REPLACED, "replaced").contains("replaced"));
        assert!(ended_message("s1", ended_code::KILLED, "").contains("已被关闭"));
        assert!(ended_message("s1", 42, "").contains("42"));
        assert!(ended_message("s1", ended_code::SERVICE_STOPPED, "service_stopped").contains("正在退出"));
    }

    #[test]
    fn remote_end_three_states() {
        assert!(remote_end_message("s", "gone").contains("仍在目标主机运行"));
        assert!(remote_end_message("s", "closed").contains("对端已关闭连接"));
        assert!(remote_end_message("s", "conn").contains("与守护进程的连接断开"));
    }

    #[test]
    fn name_validation() {
        assert!(validate_name("abc-1_2.3").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("a b").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
    }
}
