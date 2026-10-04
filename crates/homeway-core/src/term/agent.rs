//! agent — 会话「在跑哪个 agent CLI / 任务在干什么」的判定（R6 6f；行为真源 =
//! baseline 克隆 `pkg/term/agent.go` + `term_state.go` + `agent_unix.go`）。
//!
//! 五路证据、权威顺序（design D6）：
//! ① 前台进程组 + 进程树（**身份腿**）② 近几秒 PTY 输出字节数（**输出腿**）
//! ③ CPU 时间增量（**只对 shell/other**——agent 的 CPU 信号全是噪声）④ 屏幕证据
//! （manifest 引擎，blocked/idle 权威）⑤ OSC 21337 直报（**最高权威**）。
//!
//! 判定是**纯函数**（喂进程表 + 上轮采样 + 屏幕证据，出 agent/state/quiet/依据），
//! 平台差异只在「怎么拿进程表」（[`read_procs`] / [`foreground_pgid`]）。
//!
//! 状态机卫生（[`Hygiene`]，常量与 herdr 一致）：working→普通 idle 确认窗 3 拍/700ms、
//! 空闲会话零开销短路、blocked 800ms 定期重发。

use std::collections::HashMap;
use std::time::Duration;
use std::time::Instant;

/// agent 字节枚举（wire 值 = frames::agent）。
pub use super::frames::agent as agent_code;
use super::frames::state_v2;

/// 判定阈值（Go 侧 2026-09-18 实测标定，Mac mini / codex 1.x / opencode 1.18）：
/// agent 空闲时输出 0 B/s、任务期 2~5KB/s；MCP server 保活烧 20~70ms/s CPU
/// （假阳性）；codex 任务期 CPU 增量常为 0（假阴性）⇒ agent 判定**只用输出量**；
/// zsh 空闲唤醒 <10ms/s、真跑安静命令 ≥100ms/s ⇒ shell/other 保留 CPU 腿。
/// cpu 刻度 1 = 10ms（darwin ps time×100 / linux jiffies）。
pub const OUT_WINDOW_SEC: u64 = 3;
pub const OUT_THRESHOLD: i64 = 300;
pub const SHELL_CPU_BUSY_DELTA: i64 = 10;
/// running 降级磁滞：窗口排空后连续安静拍数（升级即时、降级要 quiet > 此值）。
pub const QUIET_DEGRADE: i32 = 2;

/// 一条进程记录（平台进程表拍平成这个样子）。
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcInfo {
    pub pid: i32,
    pub ppid: i32,
    pub pgid: i32,
    /// 累计 CPU 时间的**单调刻度**（linux: jiffies；darwin: ps time×100）。只做差值。
    pub cpu: i64,
    /// 命令行（argv 拼接）。
    pub args: String,
}

/// 一轮采样需要的输入。
pub struct AgentProbe<'a> {
    pub procs: &'a [ProcInfo],
    /// 会话前台进程组（0 = 拿不到）。
    pub fg_pgid: i32,
    /// 上轮同前台组的 CPU 累计刻度（None = 没有上轮；只对 shell/other 生效）。
    pub prev_cpu: Option<i64>,
    /// 近 [`OUT_WINDOW_SEC`] 秒的 PTY 输出字节数（秒桶求和后喂入）。
    pub out_bytes: i64,
    /// 磁滞输入：上一拍状态与截至上一拍的连续安静拍数。
    pub prev_state: u8,
    pub prev_quiet: i32,
    /// 会话 shell 的 pid（判「前台就是 shell 本身」）。
    pub shell_pid: i32,
    /// 屏幕证据（None = 本拍没扫屏〔短路〕或引擎不可用）。
    pub screen: Option<&'a ScreenEvidence>,
    /// OSC 21337 直报状态值（空 = 无；**最高权威**）。
    pub osc_status: &'a str,
}

/// 一次屏幕证据判定（manifest 引擎输出的判定需要项）。
#[derive(Debug, Clone, Default)]
pub struct ScreenEvidence {
    pub state: u8,
    pub visible_idle: bool,
    pub visible_blocker: bool,
    pub visible_working: bool,
    /// 命中规则带 skip_state_update（历史查看器类覆盖屏）⇒ 状态冻结。
    pub skip_update: bool,
    /// 依据（规则 id / manifest 版本 / 来源 / 回落标签）。
    pub rule_id: String,
    pub version: String,
    pub source: String,
    pub fallback: String,
}

impl ScreenEvidence {
    /// 可读依据（规格要求状态行日志带规则/版本/来源）。
    pub fn describe(&self) -> String {
        let mut parts = Vec::new();
        if !self.rule_id.is_empty() {
            parts.push(format!("rule={}", self.rule_id));
        }
        if !self.version.is_empty() {
            parts.push(format!("ver={}", self.version));
        }
        if !self.source.is_empty() {
            parts.push(format!("src={}", self.source));
        }
        if !self.fallback.is_empty() {
            parts.push(format!("fallback={}", self.fallback));
        }
        if parts.is_empty() {
            "no-rule".to_string()
        } else {
            parts.join(",")
        }
    }
}

/// 判定结果。
pub struct AgentVerdict {
    pub agent: u8,
    /// 状态枚举（working/blocked/idle/unknown；STATE 帧与 LIST JSON 共用）。
    pub state_v2: u8,
    /// 本轮采到的 CPU 刻度（下次采样当 prev_cpu 用；-1 = 无效）。
    pub cpu: i64,
    /// 截至本轮的连续安静拍数（working 清零；调用方存下来当 prev_quiet）。
    pub quiet: i32,
    /// 判定依据（哪条腿 + 规则/版本/来源），进状态行日志。
    pub evidence: String,
}

/// 已知 agent 的可执行名（不含扩展名）。新增一种 CLI 只加这里一行。
const AGENT_NAMES: &[(&str, u8)] = &[
    ("codex", agent_code::CODEX),
    ("claude", agent_code::CLAUDE),
    ("opencode", agent_code::OPENCODE),
    ("openclaw", agent_code::OPENCLAW),
];

/// 从进程表判断前台进程组「正在跑什么」。
pub fn classify_agent(p: &AgentProbe) -> AgentVerdict {
    let mut v = AgentVerdict {
        agent: agent_code::SHELL,
        state_v2: state_v2::IDLE,
        cpu: -1,
        quiet: 0,
        evidence: String::new(),
    };
    if p.procs.is_empty() || p.fg_pgid == 0 {
        v.agent = agent_code::UNKNOWN;
        v.state_v2 = state_v2::UNKNOWN;
        v.evidence = "no-procs".into();
        return v;
    }

    // 前台进程组成员。
    let fg: Vec<&ProcInfo> = p.procs.iter().filter(|pr| pr.pgid == p.fg_pgid).collect();
    if fg.is_empty() {
        v.agent = agent_code::UNKNOWN;
        v.state_v2 = state_v2::UNKNOWN;
        v.evidence = "no-fg".into();
        return v;
    }

    // 谁在跑 agent：命中名字的进程里**最深**的那个（包一层 npx/node 也认）。
    let mut matched: Option<(u8, &ProcInfo)> = None;
    for pr in &fg {
        let Some(a) = agent_of_command(&pr.args) else { continue };
        let deeper = match matched {
            None => true,
            Some((_, mp)) => is_descendant_of(pr.pid, mp.pid, p.procs),
        };
        if deeper {
            matched = Some((a, pr));
        }
    }
    let is_agent = matched.is_some();
    v.agent = if let Some((a, _)) = matched {
        a
    } else if any_shell_in_group(&fg, p.shell_pid) {
        agent_code::SHELL
    } else {
        agent_code::OTHER
    };

    // CPU 累计：前台组全部成员之和（含被 exec 换掉的子进程）。无论判定用不用都采出来。
    let mut cpu: i64 = 0;
    for pr in &fg {
        if pr.cpu > 0 {
            cpu += pr.cpu;
        }
    }
    v.cpu = cpu;

    // 输出腿：窗口内输出量 ≥ 阈值 = 在说话（闪烁级重绘被滤掉）。
    let out_talking = p.out_bytes >= OUT_THRESHOLD;
    // CPU 腿（仅 shell/other）。
    let cpu_delta = match (p.prev_cpu, cpu) {
        (Some(prev), cur) if cur > prev => cur - prev,
        _ => 0,
    };
    let mut working = out_talking || (!is_agent && cpu_delta >= SHELL_CPU_BUSY_DELTA);

    // 磁滞：安静拍计数（working 清零；否则累加，供降级判断与下一拍）。
    let mut quiet = 0;
    if !working {
        quiet = (p.prev_quiet + 1).min(99);
        // 降级保护：上一拍 working 且安静拍数未超限 → 维持 working。
        if p.prev_state == state_v2::WORKING && quiet <= QUIET_DEGRADE {
            working = true;
        }
    }
    v.quiet = quiet;

    let (st, ev) = fuse_state(&FuseInput {
        agent: v.agent,
        is_agent,
        working,
        quiet,
        osc_status: p.osc_status,
        screen: p.screen,
        prev_state: p.prev_state,
    });
    v.state_v2 = st;
    v.evidence = ev;
    v
}

/// 融合的纯输入（夹具单测不碰进程表、不碰引擎）。
struct FuseInput<'a> {
    agent: u8,
    is_agent: bool,
    /// 输出腿（shell/other 已叠加 CPU 腿）。
    working: bool,
    #[allow(dead_code)] // 与 Go fuseInput 对齐保留（降级磁滞在 classify 段已消费）
    quiet: i32,
    osc_status: &'a str,
    screen: Option<&'a ScreenEvidence>,
    prev_state: u8,
}

/// 按权威顺序融合五路证据（design D6）：
/// 1. CLI 直报（OSC 21337）= 最高权威；2. blocked 屏幕证据优先于输出腿（输出腿无法
///    否决——批准表单等用户时 TUI 仍在闪烁重绘）；3. working 输出腿；4. idle 屏幕证据；
/// 5. 回落：已知 agent 无 working 证据 ⇒ idle。
fn fuse_state(in_: &FuseInput) -> (u8, String) {
    // ① CLI 直报。
    if !in_.osc_status.is_empty() {
        if let Some(v2) = direct_report_state(in_.osc_status) {
            return (v2, format!("osc21337:{}", in_.osc_status));
        }
    }
    // ② blocked 优先于输出腿。
    if let Some(sc) = in_.screen {
        if sc.visible_blocker {
            return (state_v2::BLOCKED, format!("screen:{}", sc.describe()));
        }
    }
    // ③ 输出腿。
    if in_.working {
        return (state_v2::WORKING, "output".into());
    }
    // ④ 屏幕证据的 idle。
    if let Some(sc) = in_.screen {
        if sc.visible_idle {
            return (state_v2::IDLE, format!("screen:{}", sc.describe()));
        }
    }
    // ⑤ 回落。
    if in_.is_agent {
        if let Some(sc) = in_.screen {
            if !sc.fallback.is_empty() {
                return (state_v2::IDLE, format!("fallback:{}", sc.fallback));
            }
        }
        return (state_v2::IDLE, "agent-idle-fallback".into());
    }
    let _ = in_.agent; // 身份腿已在上游消费（agent 枚举本身不参与状态仲裁）
    let _ = in_.prev_state;
    (state_v2::IDLE, "shell-idle".into())
}

/// OSC 21337 status 值 → stateV2（认不出返回 None，不当证据）。
pub fn direct_report_state(status: &str) -> Option<u8> {
    match status.trim().to_lowercase().as_str() {
        "working" | "busy" | "running" | "in_progress" | "in-progress" => Some(state_v2::WORKING),
        "blocked" | "waiting" | "needs_input" | "needs-input" | "waiting_for_input"
        | "permission" => Some(state_v2::BLOCKED),
        "idle" | "ready" | "done" | "complete" | "completed" => Some(state_v2::IDLE),
        _ => None,
    }
}

/// 判断一条命令行是不是已知 agent。识别刻意保守（宁可不认，不要乱认）：
/// argv[0] 可执行名命中 / 带路径 token 的 basename 命中 / 裸名字只在前面是 runner 时
/// 命中 ⇒ `grep codex /var/log/x` 不误判。
pub fn agent_of_command(cmdline: &str) -> Option<u8> {
    let fields: Vec<&str> = cmdline.split_whitespace().collect();
    let first = *fields.first()?;
    if let Some(a) = agent_name_of_token(first) {
        return Some(a);
    }
    for i in 1..fields.len() {
        let tok = fields[i];
        if tok.starts_with('-') {
            continue;
        }
        let Some(a) = agent_name_of_token(tok) else { continue };
        if tok.contains('/') || is_runner_token(fields[i - 1]) {
            return Some(a);
        }
    }
    None
}

/// 单 token 的可执行名（去扩展名）→ agent 枚举。
fn agent_name_of_token(tok: &str) -> Option<u8> {
    let mut base = tok.rsplit(['/']).next().unwrap_or("");
    for ext in [".js", ".mjs", ".cjs", ".exe"] {
        base = base.strip_suffix(ext).unwrap_or(base);
    }
    if base.is_empty() {
        return None;
    }
    AGENT_NAMES.iter().find(|(n, _)| *n == base).map(|(_, a)| *a)
}

/// 包一层跑 agent 的常见 runner（npx/bunx/yarn dlx/pnpm dlx/npm exec）。
fn is_runner_token(tok: &str) -> bool {
    let base = tok.rsplit(['/']).next().unwrap_or("");
    matches!(base, "npx" | "bunx" | "dlx" | "exec")
}

/// 前台组是不是「就是那个登录 shell」（没有别的前台程序）。
fn any_shell_in_group(fg: &[&ProcInfo], shell_pid: i32) -> bool {
    for pr in fg {
        if shell_pid != 0 && pr.pid == shell_pid {
            return true;
        }
    }
    // shellPID 拿不到时退化为「命令行首 token 是常见 shell 名」。
    let Some(first) = fg.first().map(|pr| pr.args.split_whitespace().next()) else {
        return false;
    };
    let Some(first) = first else { return false };
    matches!(
        first.rsplit(['/']).next().unwrap_or(""),
        "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "tcsh" | "csh"
    )
}

/// pid 是不是 anc 的后代（多命中取「最深」）。
fn is_descendant_of(pid: i32, anc: i32, procs: &[ProcInfo]) -> bool {
    if anc == 0 {
        return false;
    }
    let mut ppid: HashMap<i32, i32> = HashMap::with_capacity(procs.len());
    for pr in procs {
        ppid.insert(pr.pid, pr.ppid);
    }
    let mut cur = pid;
    for _ in 0..16 {
        let Some(&p) = ppid.get(&cur) else { return false };
        if p <= 1 {
            return false;
        }
        if p == anc {
            return true;
        }
        cur = p;
    }
    false
}

/// 前台组里「最像已知 agent」的可执行名原文（known 决定什么算已知——由规则表
/// index.toml 决定，新增 agent 不必改代码）。与身份腿同一套取最深规则。
pub fn foreground_agent_name(
    procs: &[ProcInfo],
    fg_pgid: i32,
    known: impl Fn(&str) -> bool,
) -> String {
    if fg_pgid == 0 {
        return String::new();
    }
    let mut best = String::new();
    let mut best_pid = 0i32;
    let mut best_valid = false;
    for pr in procs {
        if pr.pgid != fg_pgid {
            continue;
        }
        let name = agent_token_name(&pr.args, &known);
        if name.is_empty() {
            continue;
        }
        if !best_valid || is_descendant_of(pr.pid, best_pid, procs) {
            best = name;
            best_pid = pr.pid;
            best_valid = true;
        }
    }
    best
}

/// 从命令行取出「已知 agent 的可执行名」原文（认不出返回空；口径与
/// [`agent_of_command`] 一致：argv[0] 的 basename、带路径 token、runner 后裸名）。
fn agent_token_name(cmdline: &str, known: &impl Fn(&str) -> bool) -> String {
    let fields: Vec<&str> = cmdline.split_whitespace().collect();
    let Some(&first) = fields.first() else { return String::new() };
    let mut cand = vec![first];
    for i in 1..fields.len() {
        let tok = fields[i];
        if tok.starts_with('-') {
            continue;
        }
        if tok.contains('/') || is_runner_token(fields[i - 1]) {
            cand.push(tok);
        }
    }
    for tok in cand {
        let mut base = tok.rsplit(['/']).next().unwrap_or("");
        for ext in [".js", ".mjs", ".cjs", ".exe"] {
            base = base.strip_suffix(ext).unwrap_or(base);
        }
        if known(base) {
            return base.to_string();
        }
    }
    String::new()
}

/// agent 枚举词面（LIST JSON / 日志用；与 frames::agent::name 同源）。
pub fn agent_name(a: u8) -> &'static str {
    agent_code::name(a)
}

// ---------------------------------------------------------------------------
// 平台面：进程表 + 前台进程组（darwin 走 ps，linux 走 /proc）
// ---------------------------------------------------------------------------

/// 抓一份进程表快照。失败返回空（调用方降级 unknown，不报错、不阻塞）。
pub fn read_procs() -> Vec<ProcInfo> {
    if cfg!(target_os = "linux") {
        read_procs_linux()
    } else {
        read_procs_ps()
    }
}

/// 取终端前台进程组；0 = 拿不到（会话刚建、已结束等）。
pub fn foreground_pgid(fd: std::os::fd::RawFd) -> i32 {
    // TIOCGPGRP：linux 族（含 OHOS——target_env=ohos，libc 面把 request 记为
    // c_int）= 0x540F；darwin = 0x40047477
    #[cfg(target_env = "ohos")]
    let req: libc::c_int = 0x540F;
    #[cfg(not(target_env = "ohos"))]
    let req: libc::c_ulong = if cfg!(any(target_os = "linux", target_os = "android")) {
        0x540F
    } else {
        0x40047477
    };
    let mut pgid: libc::c_int = 0;
    let rc = unsafe { libc::ioctl(fd, req, &mut pgid) };
    if rc == 0 {
        pgid
    } else {
        0
    }
}

/// 给整个进程组发信号（KILL 路径；SIGHUP 给会话 leader 的组）。
pub fn signal_pgid(pgid: i32, sig: libc::c_int) {
    if pgid <= 0 {
        return;
    }
    unsafe {
        libc::kill(-pgid, sig);
    }
}

fn read_procs_linux() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/proc") else { return out };
    for e in entries.flatten() {
        let Ok(pid) = e.file_name().to_string_lossy().parse::<i32>() else { continue };
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", pid)) else { continue };
        // comm 里可能有空格/括号，取最后一个 ')' 之后才是字段区。
        let Some(i) = stat.rfind(')') else { continue };
        let f: Vec<&str> = stat[i + 2..].split_whitespace().collect();
        if f.len() < 13 {
            continue;
        }
        let (Ok(ppid), Ok(pgid)) = (f[1].parse::<i32>(), f[2].parse::<i32>()) else { continue };
        let (Ok(utime), Ok(stime)) = (f[11].parse::<i64>(), f[12].parse::<i64>()) else { continue };
        out.push(ProcInfo {
            pid,
            ppid,
            pgid,
            cpu: utime + stime,
            args: read_cmdline(pid),
        });
    }
    out
}

fn read_cmdline(pid: i32) -> String {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/cmdline")) else { return String::new() };
    if raw.is_empty() {
        return String::new();
    }
    let trimmed = raw.strip_suffix(&[0]).unwrap_or(&raw);
    let mut out = String::with_capacity(trimmed.len());
    for (i, part) in trimmed.split(|&b| b == 0).enumerate() {
        if i > 0 {
            out.push(' ');
        }
        out.push_str(&String::from_utf8_lossy(part));
    }
    out
}

/// 走 ps（darwin；字段：pid ppid pgid time command）。time 形态 [[dd-]hh:]mm:ss[.ss]
/// → 换算成「百分之一秒」刻度。
fn read_procs_ps() -> Vec<ProcInfo> {
    let Ok(out) = std::process::Command::new("ps")
        .args(["-axo", "pid=,ppid=,pgid=,time=,command="])
        .output()
    else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut procs = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        if f.len() < 5 {
            continue;
        }
        let (Ok(pid), Ok(ppid), Ok(pgid)) = (f[0].parse::<i32>(), f[1].parse::<i32>(), f[2].parse::<i32>())
        else {
            continue;
        };
        procs.push(ProcInfo {
            pid,
            ppid,
            pgid,
            cpu: parse_ps_time(f[3]),
            args: f[4..].join(" "),
        });
    }
    procs
}

fn parse_ps_time(s: &str) -> i64 {
    let mut days = 0i64;
    let mut s = s;
    if let Some(i) = s.find('-') {
        if let Ok(d) = s[..i].parse::<i64>() {
            days = d;
        }
        s = &s[i + 1..];
    }
    let parts: Vec<&str> = s.split(':').collect();
    let (hh, mm, ss) = match parts.as_slice() {
        [a, b, c] => (a.parse().unwrap_or(0), b.parse().unwrap_or(0), c.parse().unwrap_or(0.0)),
        [a, b] => (0, a.parse().unwrap_or(0), b.parse().unwrap_or(0.0)),
        [a] => (0, 0, a.parse().unwrap_or(0.0)),
        _ => return 0,
    };
    let total = (days * 86400 + hh * 3600 + mm * 60) as f64 + ss;
    (total * 100.0) as i64
}

// ---------------------------------------------------------------------------
// 状态机卫生（term_state.go；纯状态 + 纯判定，时间注入）
// ---------------------------------------------------------------------------

/// working→普通 idle 确认窗需要的连续确认拍数。
pub const PENDING_IDLE_CONFIRMATIONS: i32 = 3;
/// 确认窗封顶时长（超时即放行）。
pub const PENDING_IDLE_CAP: Duration = Duration::from_millis(700);
/// blocked 持续期间的定期重发间隔。
pub const STABLE_VISIBLE_REFRESH: Duration = Duration::from_millis(800);

/// 一台会话的状态机卫生状态（由会话锁保护）。
#[derive(Default)]
pub struct Hygiene {
    pending_idle_since: Option<Instant>,
    pending_idle_confirms: i32,
    last_blocked_publish: Option<Instant>,
}

impl Hygiene {
    /// 当前是否处于「按住 working→idle」的确认窗内。
    fn pending_idle(&self, now: Instant) -> bool {
        matches!(self.pending_idle_since, Some(s) if now.duration_since(s) < PENDING_IDLE_CAP)
    }

    fn clear_pending(&mut self) {
        self.pending_idle_since = None;
        self.pending_idle_confirms = 0;
    }

    /// 判定是否要**按住**这次 working→普通 idle 的发布（true = 先不发）。
    /// 纯函数式：只读入参与自身状态，时间注入（夹具可测）。
    /// 参数面与 Go shouldHoldWorkingToIdle 一一对应（8 参对齐真源签名）。
    #[allow(clippy::too_many_arguments)]
    pub fn should_hold_working_to_idle(
        &mut self,
        prev: u8,
        next: u8,
        visible_idle: bool,
        visible_blocker: bool,
        agent_changed: bool,
        process_exited: bool,
        now: Instant,
    ) -> bool {
        let plain_idle = prev == state_v2::WORKING
            && next == state_v2::IDLE
            && !visible_idle
            && !visible_blocker
            && !agent_changed
            && !process_exited;
        if !plain_idle {
            self.clear_pending();
            return false;
        }
        match self.pending_idle_since {
            None => {
                self.pending_idle_since = Some(now);
                self.pending_idle_confirms = 0;
                true // 第 1 拍开窗
            }
            Some(s) => {
                if now.duration_since(s) >= PENDING_IDLE_CAP {
                    self.clear_pending();
                    return false;
                }
                self.pending_idle_confirms += 1;
                if self.pending_idle_confirms >= PENDING_IDLE_CONFIRMATIONS {
                    self.clear_pending();
                    return false;
                }
                true
            }
        }
    }

    /// 判定是否跳过本拍的屏幕扫描（空闲会话零开销）。条件：idle + agent 已知 +
    /// 不在确认窗 + agent 没换 + 进程没退 + 内容序号与上次扫屏相同（真的没动才短路）。
    /// 参数面与 Go shouldSkipScreenScan 一一对应。
    #[allow(clippy::too_many_arguments)]
    pub fn should_skip_screen_scan(
        &self,
        state: u8,
        agent_known: bool,
        agent_changed: bool,
        process_exited: bool,
        cur_seq: u64,
        last_scan_seq: u64,
        now: Instant,
    ) -> bool {
        if state != state_v2::IDLE || !agent_known || agent_changed || process_exited {
            return false;
        }
        if self.pending_idle(now) {
            return false;
        }
        cur_seq == last_scan_seq
    }

    /// 判定 blocked 持续期间是否该重发状态（保持消费方新鲜）。
    pub fn should_republish_blocked(&mut self, state: u8, now: Instant) -> bool {
        if state != state_v2::BLOCKED {
            return false;
        }
        match self.last_blocked_publish {
            None => {
                self.last_blocked_publish = Some(now);
                true
            }
            Some(t) if now.duration_since(t) >= STABLE_VISIBLE_REFRESH => {
                self.last_blocked_publish = Some(now);
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// 测试（agent 判定是纯函数——夹具单测，不依赖真跑 CLI）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: i32, ppid: i32, pgid: i32, cpu: i64, args: &str) -> ProcInfo {
        ProcInfo { pid, ppid, pgid, cpu, args: args.into() }
    }

    fn probe<'a>(procs: &'a [ProcInfo], fg: i32) -> AgentProbe<'a> {
        AgentProbe {
            procs,
            fg_pgid: fg,
            prev_cpu: None,
            out_bytes: 0,
            prev_state: state_v2::IDLE,
            prev_quiet: 0,
            shell_pid: 1,
            screen: None,
            osc_status: "",
        }
    }

    #[test]
    fn classify_no_procs_or_no_fg() {
        let v = classify_agent(&AgentProbe {
            procs: &[],
            fg_pgid: 0,
            prev_cpu: None,
            out_bytes: 0,
            prev_state: 0,
            prev_quiet: 0,
            shell_pid: 0,
            screen: None,
            osc_status: "",
        });
        assert_eq!((v.agent, v.state_v2), (agent_code::UNKNOWN, state_v2::UNKNOWN));
        assert_eq!(v.evidence, "no-procs");
        let procs = [proc(1, 0, 1, 100, "zsh -l")];
        let v = classify_agent(&probe(&procs, 999));
        assert_eq!(v.evidence, "no-fg");
    }

    #[test]
    fn classify_agent_identity_deepest_wins() {
        // npx → node → codex 三层；node 的 args 带 @openai/codex/bin/codex.js
        let procs = [
            proc(1, 0, 1, 50, "zsh -l"),
            proc(10, 1, 10, 10, "npx codex"),
            proc(11, 10, 10, 20, "node /x/@openai/codex/bin/codex.js"),
        ];
        let v = classify_agent(&probe(&procs, 10));
        assert_eq!(v.agent, agent_code::CODEX, "取最深命中");
        // 已知 agent 不吃 CPU 腿：CPU 增量 30 也不算 working（prev=0 → 增量 30）
        let mut p = probe(&procs, 10);
        p.prev_cpu = Some(0);
        let v2 = classify_agent(&p);
        assert_eq!(v2.state_v2, state_v2::IDLE, "agent 无输出 ⇒ idle（回落）");
        assert_eq!(v2.evidence, "agent-idle-fallback");
        assert_eq!(v2.cpu, 30, "CPU 刻度仍采出供下拍");
    }

    #[test]
    fn classify_shell_cpu_leg() {
        let procs = [proc(1, 0, 1, 50, "zsh -l")];
        let mut p = probe(&procs, 1);
        p.prev_cpu = Some(0);
        p.shell_pid = 1;
        let v = classify_agent(&p);
        assert_eq!(v.agent, agent_code::SHELL);
        assert_eq!(v.state_v2, state_v2::WORKING, "shell CPU 增量 50 刻度（500ms）≥ 10");
        assert_eq!(v.evidence, "output");
        // 输出腿
        let mut p2 = probe(&procs, 1);
        p2.out_bytes = 500;
        assert_eq!(classify_agent(&p2).state_v2, state_v2::WORKING);
        // 无腿证据：shell-idle
        let mut p3 = probe(&procs, 1);
        p3.prev_cpu = Some(50);
        let v = classify_agent(&p3);
        assert_eq!(v.state_v2, state_v2::IDLE);
        assert_eq!(v.evidence, "shell-idle");
    }

    #[test]
    fn classify_quiet_hysteresis() {
        let procs = [proc(1, 0, 1, 50, "zsh -l")];
        // 上一拍 working、本拍安静 → quiet=1 ≤ 2 维持 working
        let mut p = probe(&procs, 1);
        p.prev_state = state_v2::WORKING;
        p.prev_quiet = 0;
        let v = classify_agent(&p);
        assert_eq!(v.state_v2, state_v2::WORKING, "降级磁滞维持");
        assert_eq!(v.quiet, 1);
        // quiet=3 > 2 → 降级
        p.prev_quiet = 2;
        let v = classify_agent(&p);
        assert_eq!(v.state_v2, state_v2::IDLE);
        assert_eq!(v.quiet, 3);
        // 封顶 99
        p.prev_quiet = 150;
        assert_eq!(classify_agent(&p).quiet, 99);
    }

    #[test]
    fn fuse_authority_order() {
        let ev = |vi: bool, vb: bool, vf: &str| ScreenEvidence {
            visible_idle: vi,
            visible_blocker: vb,
            fallback: vf.into(),
            ..Default::default()
        };
        // ① OSC 直报最高（压过 blocked 屏幕证据）
        let sc = ev(false, true, "");
        let v = classify_agent(&AgentProbe {
            procs: &[proc(1, 0, 1, 0, "zsh -l")],
            fg_pgid: 1,
            osc_status: "working",
            screen: Some(&sc),
            ..probe(&[proc(1, 0, 1, 0, "zsh -l")], 1)
        });
        assert_eq!(v.state_v2, state_v2::WORKING);
        assert_eq!(v.evidence, "osc21337:working");
        // 直报词面族
        for (s, want) in [
            ("BUSY", state_v2::WORKING),
            ("in-progress", state_v2::WORKING),
            ("needs_input", state_v2::BLOCKED),
            ("permission", state_v2::BLOCKED),
            ("Ready", state_v2::IDLE),
            ("completed", state_v2::IDLE),
            ("wat", 255u8), // 认不出 → 不当证据（走后续腿）
        ] {
            let got = direct_report_state(s);
            if want == 255 {
                assert!(got.is_none(), "{s} 认不出");
            } else {
                assert_eq!(got, Some(want), "{s}");
            }
        }
        // ② blocked 屏幕证据压过输出腿
        let sc = ev(false, true, "");
        let zsh = [proc(1, 0, 1, 0, "zsh -l")];
        let mut p = probe(&zsh, 1);
        p.out_bytes = 5000; // 输出腿在说话
        p.screen = Some(&sc);
        assert_eq!(classify_agent(&p).state_v2, state_v2::BLOCKED);
        // ④ idle 屏幕证据
        let sc = ev(true, false, "");
        let zsh2 = [proc(1, 0, 1, 0, "zsh -l")];
        let mut p = probe(&zsh2, 1);
        p.screen = Some(&sc);
        let v = classify_agent(&p);
        assert_eq!(v.state_v2, state_v2::IDLE);
        assert_eq!(v.evidence, "screen:no-rule");
        // ⑤ 回落标签
        let sc = ev(false, false, "default_known_agent_idle_fallback");
        let procs = [proc(1, 0, 1, 0, "zsh -l"), proc(9, 1, 9, 0, "codex")];
        let mut p = probe(&procs, 9);
        p.screen = Some(&sc);
        assert_eq!(classify_agent(&p).evidence, "fallback:default_known_agent_idle_fallback");
    }

    #[test]
    fn agent_of_command_conservative() {
        assert_eq!(agent_of_command("codex"), Some(agent_code::CODEX));
        assert_eq!(agent_of_command("/usr/local/bin/claude"), Some(agent_code::CLAUDE));
        assert_eq!(agent_of_command("node /x/@openai/codex/bin/codex.js"), Some(agent_code::CODEX));
        assert_eq!(agent_of_command("npx codex"), Some(agent_code::CODEX));
        assert_eq!(agent_of_command("npm exec claude"), Some(agent_code::CLAUDE));
        // 名字当参数用 = 不认
        assert_eq!(agent_of_command("grep codex /var/log/x"), None);
        assert_eq!(agent_of_command("echo codex"), None, "裸名字前面不是 runner");
        assert_eq!(agent_of_command(""), None);
    }

    #[test]
    fn foreground_agent_name_for_manifest() {
        // known = 规则表进程名（manifest index 的 processes 列）
        let known = |n: &str| matches!(n, "codex" | "opencode" | "kimi code");
        let procs = [
            proc(1, 0, 1, 0, "zsh -l"),
            proc(10, 1, 10, 0, "npx opencode"),
            proc(11, 10, 10, 0, "node /x/opencode/bin/opencode.mjs"),
        ];
        assert_eq!(foreground_agent_name(&procs, 10, known), "opencode");
        assert_eq!(foreground_agent_name(&procs, 1, known), "", "前台 shell 无名");
        assert_eq!(foreground_agent_name(&procs, 0, known), "");
    }

    #[test]
    fn hygiene_hold_window_and_skip() {
        let mut h = Hygiene::default();
        let t0 = Instant::now();
        // 第 1 拍开窗
        assert!(h.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, false, false, false, false, t0
        ));
        // 强证据（visible_idle）立即放行 + 清窗
        assert!(!h.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, true, false, false, false, t0
        ));
        // 重新开窗后 3 拍确认
        assert!(h.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, false, false, false, false, t0
        ));
        assert!(h.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, false, false, false, false, t0
        ));
        assert!(h.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, false, false, false, false, t0
        ));
        assert!(!h.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, false, false, false, false, t0
        ), "确认满 3 拍放行");
        // 确认窗封顶：开窗后跳过 700ms+ 直接放行
        let mut h2 = Hygiene::default();
        let t1 = Instant::now();
        assert!(h2.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, false, false, false, false, t1
        ));
        let t2 = t1 + Duration::from_millis(701);
        assert!(!h2.should_hold_working_to_idle(
            state_v2::WORKING, state_v2::IDLE, false, false, false, false, t2
        ), "超窗放行");
        // 空闲短路：idle + agent 已知 + 序号没变
        let h3 = Hygiene::default();
        let now = Instant::now();
        assert!(h3.should_skip_screen_scan(state_v2::IDLE, true, false, false, 42, 42, now));
        assert!(!h3.should_skip_screen_scan(state_v2::IDLE, true, false, false, 43, 42, now), "序号变了必扫");
        assert!(!h3.should_skip_screen_scan(state_v2::WORKING, true, false, false, 42, 42, now));
        // blocked 重发
        let mut h4 = Hygiene::default();
        let t = Instant::now();
        assert!(h4.should_republish_blocked(state_v2::BLOCKED, t));
        assert!(!h4.should_republish_blocked(state_v2::BLOCKED, t + Duration::from_millis(100)));
        assert!(h4.should_republish_blocked(state_v2::BLOCKED, t + Duration::from_millis(850)));
        assert!(!h4.should_republish_blocked(state_v2::IDLE, t + Duration::from_secs(2)));
    }

    /// 平台面烟囱：进程表非空、字段自洽（CI 上跑真实 ps；失败零 panic）。
    #[test]
    fn read_procs_platform_smoke() {
        let procs = read_procs();
        assert!(!procs.is_empty(), "进程表不该为空");
        assert!(procs.iter().any(|p| p.pid == 1), "1 号进程在场");
        assert!(procs.iter().all(|p| p.cpu >= 0));
    }

    #[test]
    fn parse_ps_time_shapes() {
        assert_eq!(parse_ps_time("1:02.03"), 6203);
        assert_eq!(parse_ps_time("10:00"), 60000);
        assert_eq!(parse_ps_time("0:30"), 3000);
        assert_eq!(parse_ps_time("1-2:03:04.05"), (86400 + 2 * 3600 + 3 * 60 + 4) * 100 + 5);
        assert_eq!(parse_ps_time(""), 0);
    }
}
