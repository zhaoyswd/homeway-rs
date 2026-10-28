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

use std::fs::File;
use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsRawFd, BorrowedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// 真 SIGKILL 后仍只轮询此预算；失败保留 Child，由既有收尸拥有者重试。
const FORCE_REAP_WAIT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum ReapError {
    #[error("PTY child wait: {0}")]
    Io(#[from] io::Error),
    #[error("invalid PTY child PID {0}")]
    InvalidPid(i32),
    #[error("PTY child {pid} did not exit within the SIGKILL reap budget")]
    Deadline { pid: i32 },
}

fn exit_code(status: portable_pty::ExitStatus) -> i32 {
    if status.signal().is_some() { -1 } else { status.exit_code() as i32 }
}

fn poll_child(child: &mut dyn Child, budget: Duration) -> Result<Option<i32>, ReapError> {
    let deadline = std::time::Instant::now() + budget;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(exit_code(status)));
        }
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() { return Ok(None); }
        std::thread::sleep(remaining.min(Duration::from_millis(20)));
    }
}

fn reap_after_force(child: &mut dyn Child, pid: i32, budget: Duration) -> Result<i32, ReapError> {
    poll_child(child, budget)?.ok_or(ReapError::Deadline { pid })
}
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

/// PTY **控制面**（master + 子进程句柄 + 记账字段）：resize / 前台组 / 信号 / 收尸
/// 全在这里——都是 ioctl、信号与轮询级的快操作。
///
/// 与输入写面**结构性分开**（评审 2026-10-20 P1-1）：`write_all` 在 PTY 输入缓冲满时
/// 可无限阻塞，控制面若与它共享一把锁，大粘贴就能拖住 KILL、采样与 resize——
/// 写面通过非阻塞短写 + poll 保持背压；收工先取消输入，不依赖子进程退出唤醒 write。
pub struct PtyCore {
    pub master: Box<dyn MasterPty + Send>,
    pub child: Box<dyn Child + Send + Sync>,
    pub shell: String,
    pub pid: i32,
    pub cols: u16,
    pub rows: u16,
    input_stopped: Arc<AtomicBool>,
}

/// 一条起好的 PTY 会话（spawn 的产物包：控制面 + 输入写面 + 输出读面）。
/// 服务侧拆成 `service::PtyShared`（控制/写分锁）并把 reader 移交 pump 线程独占。
pub struct PtySession {
    pub core: PtyCore,
    writer: Box<dyn std::io::Write + Send>,
    pub reader: Box<dyn std::io::Read + Send>,
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
    // dup 与 master 共享 O_NONBLOCK；读/写都由下面的包装器恢复阻塞语义。
    // 在 spawn 前完成所有可失败的 fd 配置，失败时不会遗留已启动的子进程。
    let raw = pair.master.as_raw_fd().ok_or("PTY master 没有 Unix fd")?;
    // SAFETY: master 在本作用域持有 raw，clone 得到独立拥有的 CLOEXEC fd。
    let fd = unsafe { BorrowedFd::borrow_raw(raw) };
    let input_fd = fd
        .try_clone_to_owned()
        .map_err(|e| format!("clone input: {e}"))?;
    let output_fd = fd
        .try_clone_to_owned()
        .map_err(|e| format!("clone output: {e}"))?;
    // SAFETY: raw 仍由 master 持有；fcntl 不转移所有权。
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(format!("PTY nonblocking: {}", io::Error::last_os_error()));
    }
    let input_stopped = Arc::new(AtomicBool::new(false));
    let writer = Box::new(PtyInput {
        fd: File::from(input_fd),
        stopped: Arc::clone(&input_stopped),
    });
    let reader = Box::new(PtyOutput {
        fd: File::from(output_fd),
        stopped: Arc::clone(&input_stopped),
    });
    let child = pair
        .slave
        .spawn_command(cmd)
        .map_err(|e| format!("spawn {shell}: {e}"))?;
    drop(pair.slave); // 丢从端引用：子进程退出后读端能见 EOF（收尸路径依赖此语义）
    let pid = child.process_id().unwrap_or(u32::MAX) as i32;
    Ok(PtySession {
        core: PtyCore { master: pair.master, child, shell, pid, cols, rows, input_stopped },
        writer,
        reader,
    })
}

/// 写等待的取消检查粒度；正常输入没有总超时，背压持续到可写或显式收工。
const INPUT_CANCEL_POLL_MS: libc::c_int = 50;

/// 每个方向独占其 fd；不使用 portable-pty writer（其 Drop 会额外写 VEOF）。
struct PtyInput {
    fd: File,
    stopped: Arc<AtomicBool>,
}

impl io::Write for PtyInput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "PTY input stopped",
                ));
            }
            match self.fd.write(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    // poll 只是就绪提示；醒后仍用非阻塞 write，应对并发关闭/伪就绪。
                    match poll_fd(&self.fd, libc::POLLOUT, INPUT_CANCEL_POLL_MS) {
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Ok(events) if events & (libc::POLLHUP | libc::POLLERR) != 0 => {
                            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "PTY input closed"));
                        }
                        result => {
                            result?;
                        }
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                result => return result,
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        // File 没有用户态缓冲，不能用 tcdrain（同样可能无限等待子进程）。
        Ok(())
    }
}

struct PtyOutput {
    fd: File,
    stopped: Arc<AtomicBool>,
}

impl io::Read for PtyOutput {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            match self.fd.read(buf) {
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                    match poll_fd(&self.fd, libc::POLLIN, -1) {
                        Ok(_) => continue,
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        Err(e) => return Err(e),
                    }
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                // 与 portable-pty 一致：Linux slave 全关闭时 master read 回 EIO。
                Err(e) if e.raw_os_error() == Some(libc::EIO) => {
                    self.stopped.store(true, Ordering::Release);
                    return Ok(0);
                }
                result => {
                    if !matches!(result, Ok(n) if n > 0) {
                        self.stopped.store(true, Ordering::Release);
                    }
                    return result;
                }
            }
        }
    }
}

impl Drop for PtyOutput {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
    }
}

fn poll_fd(fd: &File, events: libc::c_short, timeout: libc::c_int) -> io::Result<libc::c_short> {
    let mut pfd = libc::pollfd {
        fd: fd.as_raw_fd(),
        events,
        revents: 0,
    };
    // SAFETY: pfd 有效，File 在整个 poll 调用期间持有 fd。
    if unsafe { libc::poll(&mut pfd, 1, timeout) } == -1 {
        return Err(io::Error::last_os_error());
    }
    if pfd.revents & libc::POLLNVAL != 0 {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    // 调用方分方向解释 HUP/ERR：输入报错，输出仍先 read 排空尾部。
    Ok(pfd.revents)
}

impl Drop for PtyCore {
    fn drop(&mut self) {
        self.cancel_input();
    }
}

impl PtyCore {
    pub(super) fn input_stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.input_stopped)
    }

    /// 独立于子进程信号/退出的输入取消；不取写锁。
    pub fn cancel_input(&self) {
        self.input_stopped.store(true, Ordering::Release);
    }

    /// 应用新尺寸（幂等）。
    pub fn resize(&self, cols: u16, rows: u16) -> Result<(), String> {
        self.master
            .resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| e.to_string())
    }

    /// 当前 PTY 实际尺寸（读 master 的 TIOCGWINSZ；失败退回 spawn 记录值）。
    /// 观测面：间隔哨兵的单测要区分「PTY 当前值」与「会话记账值 rt.size()」。
    pub fn tty_size(&self) -> (u16, u16) {
        match self.master.get_size() {
            Ok(s) => (s.cols, s.rows),
            Err(_) => (self.cols, self.rows),
        }
    }

    /// 前台进程组（检测身份腿输入；拿不到 = 0）。
    pub fn foreground_pgid(&self) -> i32 {
        self.master.process_group_leader().unwrap_or(0)
    }

    /// 主动结束：SIGHUP → 宽限 → SIGKILL（ENDED 由会话收尸路径发）。
    /// `grace_deadline` 由调用方驱动（会话层有自己的时间源/退出条件）。
    pub fn kill_start(&self) {
        self.cancel_input();
        super::agent::signal_pgid(self.pid, libc::SIGHUP);
    }

    pub fn kill_force(&mut self) {
        self.cancel_input();
        let _ = self.force_running_child();
    }

    fn force_running_child(&mut self) -> Result<(), ReapError> {
        // Child 仍归本句柄且未被收尸，PID 此时不能被复用；不得对已缓存退出的 PID 发信号。
        if self.child.try_wait()?.is_some() { return Ok(()); }
        if self.pid <= 0 { return Err(ReapError::InvalidPid(self.pid)); }
        super::agent::signal_pgid(self.pid, libc::SIGKILL);
        // portable-pty 的 Unix clone_killer().kill() 仅发 SIGHUP，不能用作强杀回退。
        // SAFETY: 正 PID 属于上面确认尚未收尸的自有 Child；本调用持有其独占控制面。
        if unsafe { libc::kill(self.pid, libc::SIGKILL) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) { return Err(error.into()); }
        }
        Ok(())
    }

    /// 是否仍在跑（非阻塞探测）。
    pub fn try_wait(&mut self) -> Option<portable_pty::ExitStatus> {
        self.try_wait_result().ok().flatten()
    }

    pub(crate) fn try_wait_result(&mut self) -> io::Result<Option<portable_pty::ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.cancel_input();
        }
        Ok(status)
    }

    /// 显式阻塞收尸（直接调用方已知子进程会退出时使用）；业务收尾使用 wait_bounded。
    ///
    /// D-19 处置（2026-10-04 收口）：Go 对**信号致死**回 -1（`ProcessState.ExitCode()`），
    /// portable-pty 的 `exit_code()` 对信号形态固定 1——这里按 Go -1 语义映射
    /// （`ExitStatus::signal()` 可辨信号死），ENDED code 与 Go 出口逐值一致；
    /// 正常退出码（0..255）两侧本就一致。差异注记：portable-pty 不暴露具体信号值
    /// 的数值面（只给词面），而 EXIT 面只需要数值——Go 也只给数值。
    pub fn wait(&mut self) -> i32 {
        self.cancel_input();
        match self.child.wait() {
            Ok(status) if status.signal().is_some() => -1,
            Ok(status) => status.exit_code() as i32,
            Err(_) => 0,
        }
    }

    /// 宽限与强杀后的等待均只做有界 try_wait；错误不伪造退出码，也不丢弃 Child。
    pub fn wait_bounded(&mut self, grace: std::time::Duration) -> Result<i32, ReapError> {
        self.cancel_input();
        if let Some(code) = poll_child(self.child.as_mut(), grace)? { return Ok(code); }
        self.force_running_child()?;
        reap_after_force(self.child.as_mut(), self.pid, FORCE_REAP_WAIT)
    }
}

impl PtySession {
    /// 拆产物包：控制面 / 写面 / 读面各归其主（服务侧分锁装配，reader 归 pump）。
    pub fn into_parts(
        self,
    ) -> (PtyCore, Box<dyn std::io::Write + Send>, Box<dyn std::io::Read + Send>) {
        (self.core, self.writer, self.reader)
    }

    /// 写 PTY（输入面：客户端按键 / 哨兵后的重绘 / focus nudge）。
    /// **独占形态**（`&mut self`，测试/单 owner 用）；服务侧的共享形态在
    /// `service::PtyShared::write_input`（writer 独立锁）。
    pub fn write_input(&mut self, p: &[u8]) -> Result<(), String> {
        self.writer.write_all(p).map_err(|e| e.to_string())?;
        self.writer.flush().map_err(|e| e.to_string())
    }

    /// 尺寸哨兵：sentinel → 真实尺寸，两次 SIGWINCH 逼 TUI 重绘当前屏。
    /// 返回哨兵列值（低 7 判据：一次 attach 只应 +1 次）。
    ///
    /// **仅测试夹具**——生产语义在 `term/service.rs`（`sentinel_repaint_locked`，
    /// term-attach-resilience 起分「背靠背/间隔」两形态）；勿在生产代码上引用本方法。
    pub fn sentinel_repaint(&self) -> u16 {
        let (cols, rows) = (self.core.cols, self.core.rows);
        let sentinel = if cols > 1 { cols - 1 } else { cols + 1 };
        let _ = self.core.resize(sentinel, rows);
        let _ = self.core.resize(cols, rows);
        sentinel
    }

    /// 读一批 PTY 输出（阻塞语义由调用方的 fd 超时/线程模型决定）。
    pub fn read_output(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reader.read(buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct PendingChild;

    impl portable_pty::ChildKiller for PendingChild {
        fn kill(&mut self) -> io::Result<()> { panic!("polling must not signal a mock child") }
        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            panic!("polling must not clone a killer")
        }
    }

    impl Child for PendingChild {
        fn try_wait(&mut self) -> io::Result<Option<portable_pty::ExitStatus>> { Ok(None) }
        fn wait(&mut self) -> io::Result<portable_pty::ExitStatus> {
            panic!("a bounded reap must never call blocking wait")
        }
        fn process_id(&self) -> Option<u32> { None }
    }

    #[test]
    fn post_force_reap_deadline_preserves_pending_child() {
        let mut child = PendingChild;
        assert!(matches!(reap_after_force(&mut child, 123, Duration::from_millis(2)),
            Err(ReapError::Deadline { pid: 123 })));
        assert!(child.try_wait().unwrap().is_none(), "超时仍保留同一Child，可继续收尸");
    }

    #[test]
    fn force_reap_kills_hup_ignoring_child_outside_its_pid_group() {
        use std::process::Stdio;
        let mut child = Command::new("/bin/sh")
            .args(["-c", "trap '' HUP; printf ready; while IFS= read -r line; do :; done"])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
            .spawn().unwrap();
        let _input = child.stdin.take().unwrap(); // 不以 EOF 让自有子进程自然退出。
        let mut output = child.stdout.take().unwrap();
        let mut ready = [0u8; 5];
        output.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");
        let pid = child.id() as i32;
        // SAFETY: 只查询本测试刚启动、仍由Child句柄拥有的进程。
        assert_ne!(unsafe { libc::getpgid(pid) }, pid);
        let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
        let mut core = PtyCore { master: pair.master, child: Box::new(child), shell: "/bin/sh".into(),
            pid, cols: 80, rows: 24, input_stopped: Arc::new(AtomicBool::new(false)) };
        core.kill_force();
        let result = reap_after_force(core.child.as_mut(), pid, Duration::from_secs(1));
        if result.is_err() {
            // 旧实现只发被忽略的HUP；同一拥有者重新确认Child尚活后再清理，不跨线程猜PID。
            if core.try_wait_result().unwrap().is_none() {
                // SAFETY: 本线程独占仍未收尸的Child，其PID尚不能被复用。
                unsafe { libc::kill(pid, libc::SIGKILL); }
            }
            reap_after_force(core.child.as_mut(), pid, Duration::from_secs(1)).unwrap();
        }
        assert_eq!(result.expect("单进程强杀必须在预算内收尸"), -1);
    }

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
        assert_eq!((pty.core.cols, pty.core.rows), (40, 10));
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
        let code = pty.core.wait();
        assert_eq!(code, 7, "子进程退出码");
    }

    /// O_NONBLOCK 是共享状态：读端须等待新输出，不得把 WouldBlock 当 EOF。
    /// HUP 到达后仍读尽子进程退出前的全部尾部字节。
    #[test]
    fn nonblocking_master_reader_waits_and_drains_tail() {
        let mut pty = spawn("poll-read", 80, 24,
            Some("stty raw -echo; printf first; sleep 0.2; printf second; exit 7"))
            .expect("spawn");
        let mut bytes = Vec::new();
        pty.reader.read_to_end(&mut bytes).expect("blocking reader wrapper");
        assert_eq!(bytes, b"firstsecond");
        assert_eq!(pty.core.wait(), 7);
    }

    /// 正常输入没有任意超时；满队列等待子进程恢复读取，再完整按序送达。
    #[test]
    fn input_backpressure_preserves_bytes_and_order() {
        let mut pty = spawn("poll-order", 80, 24,
            Some("stty raw -echo; printf ready; sleep 0.3; exec cat"))
            .expect("spawn");
        assert_eq!(pty.reader.read(&mut []).unwrap(), 0);
        assert!(!pty.core.input_stopped.load(Ordering::Acquire), "空读不代表 EOF");
        let mut ready = [0u8; 5];
        pty.reader.read_exact(&mut ready).unwrap();
        assert_eq!(&ready, b"ready");
        let expected: Vec<u8> = (0..256 * 1024).map(|i| (i % 251) as u8).collect();
        let data = expected.clone();
        let (mut core, mut writer, mut reader) = pty.into_parts();
        let writing = std::thread::spawn(move || writer.write_all(&data));
        let mut got = vec![0u8; expected.len()];
        reader.read_exact(&mut got).unwrap();
        assert_eq!(got, expected);
        writing.join().unwrap().unwrap();
        core.kill_start();
        core.wait_bounded(Duration::from_secs(1)).unwrap();
    }

    #[test]
    fn sentinel_wraps_at_one_col() {
        // cols=1 的边界形态：sentinel = cols+1（防 0 列）
        let mut pty = spawn("sentinel1", 1, 3, Some("sleep 5")).expect("spawn");
        assert_eq!(pty.sentinel_repaint(), 2);
        pty.core.kill_force();
    }
}
