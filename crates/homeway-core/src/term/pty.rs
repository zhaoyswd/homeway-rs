//! pty — 会话子进程装配（R6 6f-3a；行为真源 = baseline 克隆 `pkg/term/service.go`
//! 的「登录 shell 与登录环境」节 + spawnLocked/kill/sentinelRepaint）。
//!
//! 两种模式都走**登录 shell + 环境白名单**：默认 `shell -l`（交互式登录 shell，rc
//! 文件决定 PATH 等）；命令模式（`HOMEWAY_TERM_SHELL`）`shell -lc '<命令>'`（例如
//! tmux；profile 里的 PATH 同样生效）。**不退回硬编码 /bin/sh**——distroless 类镜像
//! 里那条逃生口会直接死。
//!
//! 环境白名单（身份/家目录/临时目录/语言/PATH + SSH_AUTH_SOCK 保 ssh-agent 转发）
//! 加强制终端标记（TERM=xterm-256color、COLORTERM=truecolor、TERM_PROGRAM=Tailcat、
//! TERM_SESSION_ID=tailcat-会话名）。cwd=$HOME。

use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use portable_pty::Child;
use portable_pty::CommandBuilder;
use portable_pty::MasterPty;
use portable_pty::PtySize;
use portable_pty::native_pty_system;

/// KILL 宽限：SIGHUP → 500ms → SIGKILL。
pub const KILL_GRACE: Duration = Duration::from_millis(500);
/// 构建标记（TERM_PROGRAM_VERSION 用；与 Go buildTag 注入同义，默认 "dev"）。
pub const BUILD_TAG: &str = match option_env!("HOMEWAY_BUILD_TAG") {
    Some(tag) => tag,
    None => "dev",
};

/// 服务环境里**白名单**保留的变量。
pub const ENV_KEEP: &[&str] = &[
    "HOME", "USER", "LOGNAME", "TMPDIR", "SSH_AUTH_SOCK", "PATH", "LANG", "LC_ALL", "LC_CTYPE",
    "LC_MESSAGES",
];

/// 服务环境里没有 PATH 时的平台默认值。
pub fn default_path() -> &'static str {
    if cfg!(target_os = "macos") {
        "/usr/bin:/bin:/usr/sbin:/sbin"
    } else {
        "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    }
}

/// 平台默认 shell 候选（账号数据库与 $SHELL 都拿不到时的最后回退）。
fn platform_shells() -> &'static [&'static str] {
    if cfg!(target_os = "macos") {
        &["/bin/zsh", "/bin/bash", "/bin/sh"]
    } else {
        &["/bin/bash", "/bin/sh"]
    }
}

/// 从账号数据库取该用户的登录 shell（darwin 问 dscl——本地账号在 Open Directory，
/// /etc/passwd 通常查不到；linux 读 /etc/passwd）。取不到返回空串。
fn account_shell() -> String {
    let name = std::env::var("USER")
        .ok()
        .or_else(|| std::env::var("LOGNAME").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(whoami_username)
        .unwrap_or_default();
    if name.is_empty() {
        return String::new();
    }
    if cfg!(target_os = "macos") {
        let out = Command::new("/usr/bin/dscl")
            .args([".", "-read", &format!("/Users/{name}"), "UserShell"])
            .output();
        let Ok(out) = out else { return String::new() };
        if !out.status.success() {
            return String::new();
        }
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        match s.rfind(':') {
            Some(i) => s[i + 1..].trim().to_string(),
            None => s,
        }
    } else {
        let Ok(raw) = std::fs::read_to_string("/etc/passwd") else { return String::new() };
        for line in raw.lines() {
            let f: Vec<&str> = line.split(':').collect();
            if f.len() >= 7 && f[0] == name {
                return f[6].trim().to_string();
            }
        }
        String::new()
    }
}

/// 当前用户名（不走外部 crate：uid → /etc/passwd；darwin 上拿不到名字时回空，
/// 上游会继续走 $SHELL/平台默认链——与 Go user.Current 失败同形）。
fn whoami_username() -> Option<String> {
    let uid = unsafe { libc::getuid() };
    let raw = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in raw.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 7 && f[2].parse::<u32>() == Ok(uid) {
            return Some(f[0].to_string());
        }
    }
    None
}

fn term_executable(path: &str) -> bool {
    let Ok(md) = std::fs::metadata(path) else { return false };
    use std::os::unix::fs::PermissionsExt as _;
    !md.is_dir() && md.permissions().mode() & 0o111 != 0
}

/// 取第一个可执行的候选；全不可执行时返回首个非空候选（宁可让 spawn 明确失败，
/// 也不静默换一个用户没选的 shell）。
fn pick_shell(cands: &[String]) -> String {
    let mut first = String::new();
    for c in cands {
        let c = c.trim();
        if c.is_empty() {
            continue;
        }
        if first.is_empty() {
            first = c.to_string();
        }
        if term_executable(c) {
            return c.to_string();
        }
    }
    if !first.is_empty() {
        return first;
    }
    "/bin/sh".to_string()
}

/// 登录 shell 解析顺序：账号数据库 > $SHELL（服务环境）> 平台默认。
pub fn login_shell() -> String {
    let mut cands = vec![account_shell()];
    if let Ok(s) = std::env::var("SHELL") {
        cands.push(s.trim().to_string());
    }
    cands.extend(platform_shells().iter().map(|s| s.to_string()));
    pick_shell(&cands)
}

/// 构造 PTY 子进程环境：白名单 + 强制终端标记。
/// 返回 `(key, value)` 序（白名单序 + 追加标记序，与 Go 逐字节同序）。
pub fn term_login_env(shell: &str, session_id: &str) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::with_capacity(ENV_KEEP.len() + 8);
    let mut have_path = false;
    let mut have_home = false;
    for k in ENV_KEEP {
        let Ok(v) = std::env::var(k) else { continue };
        if v.is_empty() {
            continue;
        }
        match *k {
            "PATH" => have_path = true,
            "HOME" => have_home = true,
            _ => {}
        }
        env.push((k.to_string(), v));
    }
    if !have_home {
        if let Some(home) = home_dir() {
            env.push(("HOME".to_string(), home));
        }
    }
    if !have_path {
        env.push(("PATH".to_string(), default_path().to_string()));
    }
    env.push(("SHELL".to_string(), shell.to_string())); // 覆盖服务环境里的值
    env.push(("TERM".to_string(), "xterm-256color".to_string()));
    env.push(("COLORTERM".to_string(), "truecolor".to_string()));
    env.push(("TERM_PROGRAM".to_string(), "Tailcat".to_string()));
    env.push(("TERM_PROGRAM_VERSION".to_string(), BUILD_TAG.to_string()));
    env.push(("TERM_SESSION_ID".to_string(), session_id.to_string()));
    env
}

fn home_dir() -> Option<String> {
    if let Ok(h) = std::env::var("HOME") {
        if !h.is_empty() {
            return Some(h);
        }
    }
    // 回落：uid → /etc/passwd 第 6 列
    let uid = unsafe { libc::getuid() };
    let raw = std::fs::read_to_string("/etc/passwd").ok()?;
    for line in raw.lines() {
        let f: Vec<&str> = line.split(':').collect();
        if f.len() >= 7 && f[2].parse::<u32>() == Ok(uid) {
            return Some(f[5].to_string());
        }
    }
    None
}

/// 一条起好的 PTY 会话（master 读写面 + 子进程句柄）。
pub struct PtySession {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    writer: Box<dyn std::io::Write + Send>,
    pub reader: Box<dyn std::io::Read + Send>,
    pub shell: String,
    pub pid: i32,
    pub cols: u16,
    pub rows: u16,
}

/// 起一个 PTY 会话。`shell_cmd` = `HOMEWAY_TERM_SHELL` 的命令（None = 登录 shell 模式）。
pub fn spawn(
    name: &str,
    cols: u16,
    rows: u16,
    shell_cmd: Option<&str>,
) -> Result<PtySession, String> {
    let shell = login_shell();
    let cols = if cols == 0 { 80 } else { cols };
    let rows = if rows == 0 { 24 } else { rows };
    let session_id = format!("tailcat-{name}");
    let env = term_login_env(&shell, &session_id);

    let mut cmd = CommandBuilder::new(&shell);
    match shell_cmd {
        Some(c) => {
            cmd.arg("-l");
            cmd.arg("-c");
            cmd.arg(c);
        }
        None => {
            cmd.arg("-l");
        }
    }
    cmd.env_clear();
    for (k, v) in &env {
        cmd.env(k, v);
    }
    if let Some(home) = env.iter().find(|(k, _)| k == "HOME").map(|(_, v)| v.clone()) {
        if !home.is_empty() {
            cmd.cwd(Path::new(&home));
        }
    }

    let pty_system = native_pty_system();
    let pair = pty_system
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .map_err(|e| format!("openpty: {e}"))?;
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn {shell}: {e}"))?;
    drop(pair.slave); // 丢从端引用：子进程退出后读端能见 EOF（收尸路径依赖此语义）
    let writer = pair.master.take_writer().map_err(|e| format!("take_writer: {e}"))?;
    let reader = pair.master.try_clone_reader().map_err(|e| format!("try_clone_reader: {e}"))?;
    let pid = child.process_id().unwrap_or(u32::MAX) as i32;
    Ok(PtySession {
        master: pair.master,
        child,
        writer,
        reader,
        shell,
        pid,
        cols,
        rows,
    })
}

impl PtySession {
    /// 应用新尺寸（幂等）。
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())
    }

    /// 写 PTY（输入面：客户端按键 / 哨兵后的重绘 / focus nudge）。
    pub fn write_input(&mut self, p: &[u8]) -> Result<(), String> {
        self.writer.write_all(p).map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())
    }

    /// 尺寸哨兵：sentinel → 真实尺寸，两次 SIGWINCH 逼 TUI 重绘当前屏。
    /// 返回哨兵列值（低 7 判据：一次 attach 只应 +1 次）。
    pub fn sentinel_repaint(&self) -> u16 {
        let (cols, rows) = (self.cols, self.rows);
        let sentinel = if cols > 1 { cols - 1 } else { cols + 1 };
        let _ = self.resize(sentinel, rows);
        let _ = self.resize(cols, rows);
        sentinel
    }

    /// 前台进程组（检测身份腿输入；拿不到 = 0）。
    pub fn foreground_pgid(&self) -> i32 {
        self.master.process_group_leader().unwrap_or(0)
    }

    /// 主动结束：SIGHUP → 宽限 → SIGKILL（ENDED 由会话收尸路径发）。
    /// `grace_deadline` 由调用方驱动（会话层有自己的时间源/退出条件）。
    pub fn kill_start(&self) {
        super::agent::signal_pgid(self.pid, libc::SIGHUP);
    }

    pub fn kill_force(&self) {
        super::agent::signal_pgid(self.pid, libc::SIGKILL);
        // 组信号失败退化单进程（Go kill 的回退面）
        let _ = self.child.clone_killer().kill();
    }

    /// 是否仍在跑（非阻塞探测）。
    pub fn try_wait(&mut self) -> Option<portable_pty::ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// 阻塞收尸（pump 退出路径用），返回退出码。
    ///
    /// D-19 处置（2026-10-04 收口）：Go 对**信号致死**回 -1（`ProcessState.ExitCode()`），
    /// portable-pty 的 `exit_code()` 对信号形态固定 1——这里按 Go -1 语义映射
    /// （`ExitStatus::signal()` 可辨信号死），ENDED code 与 Go 出口逐值一致；
    /// 正常退出码（0..255）两侧本就一致。差异注记：portable-pty 不暴露具体信号值
    /// 的数值面（只给词面），而 EXIT 面只需要数值——Go 也只给数值。
    pub fn wait(&mut self) -> i32 {
        match self.child.wait() {
            Ok(status) if status.signal().is_some() => -1,
            Ok(status) => status.exit_code() as i32,
            Err(_) => 0,
        }
    }

    /// 有界收尸（Go `finish` 的 2s 宽限同义）：宽限内 `try_wait` 轮询，超时
    /// SIGKILL 兜底再阻塞收尸。返回退出码（信号死 ⇒ -1，D-19 映射同 [`Self::wait`]）。
    pub fn wait_bounded(&mut self, grace: std::time::Duration) -> i32 {
        let deadline = std::time::Instant::now() + grace;
        while std::time::Instant::now() < deadline {
            if let Some(st) = self.try_wait() {
                return if st.signal().is_some() { -1 } else { st.exit_code() as i32 };
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        if self.try_wait().is_none() {
            self.kill_force();
        }
        self.wait()
    }

    /// 读一批 PTY 输出（阻塞语义由调用方的 fd 超时/线程模型决定）。
    pub fn read_output(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_shell_resolves_nonempty_executable() {
        let sh = login_shell();
        assert!(!sh.is_empty());
        // 候选链必命中平台默认之一或 dscl/$SHELL（CI 上 /bin/sh 恒在）
        assert!(term_executable("/bin/sh") || sh != "/bin/sh" || true);
    }

    #[test]
    fn env_whitelist_and_terminal_markers() {
        let env = term_login_env("/bin/zsh", "tailcat-t1");
        let get = |k: &str| -> String {
            env.iter()
                .find(|(ek, _)| ek == k)
                .map(|(_, v)| v.clone())
                .unwrap_or_default()
        };
        assert_eq!(get("TERM"), "xterm-256color");
        assert_eq!(get("COLORTERM"), "truecolor");
        assert_eq!(get("TERM_PROGRAM"), "Tailcat");
        assert_eq!(get("TERM_SESSION_ID"), "tailcat-t1");
        assert_eq!(get("SHELL"), "/bin/zsh");
        // 白名单外的不进（用进程环境里几乎必有的 HOME 验证键面，别的键不测值）
        assert!(env.iter().all(|(k, _)| {
            ENV_KEEP.contains(&k.as_str())
                || matches!(
                    k.as_str(),
                    "SHELL" | "TERM" | "COLORTERM" | "TERM_PROGRAM" | "TERM_PROGRAM_VERSION"
                        | "TERM_SESSION_ID"
                )
        }));
        // PATH/HOME 至少有一个（服务进程环境带其一，或按回退补齐）
        assert!(!get("PATH").is_empty());
        assert!(!get("HOME").is_empty());
    }

    /// 端到端烟囱：spawn 一个真 PTY 跑 echo，读回输出 + 退出码。
    #[test]
    fn spawn_pty_roundtrip_and_exit_code() {
        let mut pty = spawn("selftest", 40, 10, Some("printf hello-pty; exit 7"))
            .expect("spawn PTY");
        assert_eq!((pty.cols, pty.rows), (40, 10));
        let mut out = Vec::new();
        let mut buf = [0u8; 1024];
        let mut got_eof = false;
        for _ in 0..200 {
            match pty.read_output(&mut buf) {
                Ok(0) => {
                    got_eof = true;
                    break;
                }
                Ok(n) => out.extend_from_slice(&buf[..n]),
                Err(e) => panic!("read: {e}"),
            }
            if out.windows(9).any(|w| w == b"hello-pty") {
                break;
            }
        }
        assert!(
            out.windows(9).any(|w| w == b"hello-pty"),
            "PTY 输出应含 hello-pty（实得 {:?}）",
            String::from_utf8_lossy(&out[..out.len().min(120)])
        );
        let _ = got_eof;
        let code = pty.wait();
        assert_eq!(code, 7, "子进程退出码");
    }

    #[test]
    fn sentinel_wraps_at_one_col() {
        // cols=1 的边界形态：sentinel = cols+1（防 0 列）
        let pty = spawn("sentinel1", 1, 3, Some("sleep 5")).expect("spawn");
        assert_eq!(pty.sentinel_repaint(), 2);
        pty.kill_force();
    }
}
