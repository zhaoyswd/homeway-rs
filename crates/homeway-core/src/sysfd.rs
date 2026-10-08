//! `sysfd`：**平台系统事实**单源（fd 标志与 `sockaddr_un` 上限）。
//!
//! Q-G F1/F4 的落点：全仓**自建 fd 一律 CLOEXEC** 的唯一收口（Go 侧拦截层用
//! `net.Dialer`、私钥/台账用 `os.OpenFile(...,0o600)`——fd 天然 CLOEXEC ⇒ 本仓
//! libc 手建点是**移植回退**面），以及 `sun_path` 的**平台真实可用长度**单源。
//!
//! 平台分派（三目标编译探针实测，设计 `docs/reviews/QG-design.md` §0.4）：
//! - **linux（含 OHOS：其 `target_os="linux"`）**：`SOCK_CLOEXEC` / `pipe2(O_CLOEXEC)`
//!   原子位可用 ⇒ **创建即带标志**（零窗口）；
//! - **darwin**：libc 未导出上述绑定 ⇒ 创建后立即 fcntl 补标志（窗口 ≈ 数十纳秒；
//!   本仓 exec 面仅 3 处 std `Command`——`ps`/`dscl`/daemon 自 exec ⇒ 残余登记）。
//!
//! **所有权契约**：本模块只服务**本仓自建**的 fd（`OwnedFd`/`BorrowedFd` 签名）。
//! 外部传入的 fd（App tun fd 经 capi）**不得**改 flags——所有权在扩展。

use std::io;
use std::os::fd::{AsRawFd as _, BorrowedFd, FromRawFd as _, OwnedFd};

/// `sockaddr_un.sun_path` 的**可用路径长度上限**（= 容量 − 1：NUL 结尾）。
///
/// darwin **103** / linux·OHOS **107**（实测：darwin `bind` 99–103 OK、104+ 报
/// `InvalidInput "path must be shorter than SUN_LEN"`；linux 为容量 − 1 的口径，
/// 三目标编译期 `size_of − offset_of − 1` 已独立复现）。判据统一为 `len > SUN_PATH_MAX`。
///
/// **新平台注记**：若某 unix 目标的 `sockaddr_un` 布局不同（无 `sun_path` 字段），
/// 本表达式不能编译——须显式核对并更新本常量（不要写平台白名单断言）。
pub const SUN_PATH_MAX: usize =
    std::mem::size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1;

/// 本地 TCP 连接的 RST 收口（`SO_LINGER(l_onoff=1, l_linger=0)`——close 时不再走
/// 优雅 FIN 队列而是直接复位；尽力而为，失败静默）。
///
/// 为什么收在这里（Q-F-B F2-3/D13）：这是**失败路径收口**的共享语义——「连接已建立
/// 但上游不可达」时必须 RST（优雅 FIN 会让客户端把「连上后立刻 EOF」当成响应结束而
/// 静默挂住）。daemon 承载面（forward/socks）三处调用点与 portfwd 拨号失败面共用同一
/// 单源（此前只有 `daemon::carriers` 的一份私有副本，pf 会成第四份）。
///
/// **只设选项、不关 fd**：fd 的生命周期归调用方（drop 即 close ⇒ 生效）。
pub(crate) fn rst_close_tcp(stream: &std::net::TcpStream) {
    use std::os::fd::AsRawFd as _;
    unsafe {
        let linger = libc::linger {
            l_onoff: 1,
            l_linger: 0,
        };
        let _ = libc::setsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_LINGER,
            &linger as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::linger>() as libc::socklen_t,
        );
    }
}

/// 给 fd 打 `FD_CLOEXEC`（幂等——`F_SETFD` 的 flags 字只有这一位）。
pub fn set_cloexec(fd: BorrowedFd<'_>) -> io::Result<()> {
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// 建管道，两端带 CLOEXEC（linux/OHOS 走 `pipe2(O_CLOEXEC)` 原子位；darwin 建后补）。
pub fn pipe_cloexec() -> io::Result<(OwnedFd, OwnedFd)> {
    #[cfg(target_os = "linux")]
    {
        let mut fds = [-1i32; 2];
        if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY：pipe2 成功即两个未占用的有效 fd（所有权移交 OwnedFd）。
        Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }
    #[cfg(not(target_os = "linux"))]
    {
        let mut fds = [-1i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY：pipe 成功即两个有效 fd；出错路径由 OwnedFd drop 关两端（无泄漏）。
        let (r, w) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        set_cloexec(std::os::fd::AsFd::as_fd(&r))?; // 失败 ⇒ r/w 随 drop 关
        set_cloexec(std::os::fd::AsFd::as_fd(&w))?;
        Ok((r, w))
    }
}

/// 建 socket，带 CLOEXEC（linux/OHOS 走 `SOCK_CLOEXEC` 原子位，darwin 建后补）。
pub fn socket_cloexec(domain: libc::c_int, ty: libc::c_int, proto: libc::c_int) -> io::Result<OwnedFd> {
    #[cfg(target_os = "linux")]
    let raw = unsafe { libc::socket(domain, ty | libc::SOCK_CLOEXEC, proto) };
    #[cfg(not(target_os = "linux"))]
    // darwin 无 SOCK_CLOEXEC 绑定（编译探针实证）——第二参只认类型位
    let raw = unsafe { libc::socket(domain, ty, proto) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY：raw 为刚创建的有效 fd（所有权移交 OwnedFd——后续错误路径随 drop 关）。
    let fd = unsafe { OwnedFd::from_raw_fd(raw) };
    #[cfg(not(target_os = "linux"))]
    set_cloexec(std::os::fd::AsFd::as_fd(&fd))?;
    Ok(fd)
}

/// 建 socketpair，两端带 CLOEXEC（分派同 [`socket_cloexec`]）。
pub fn socketpair_cloexec(
    domain: libc::c_int,
    ty: libc::c_int,
    proto: libc::c_int,
) -> io::Result<(OwnedFd, OwnedFd)> {
    let mut fds = [-1i32; 2];
    #[cfg(target_os = "linux")]
    let rc = unsafe { libc::socketpair(domain, ty | libc::SOCK_CLOEXEC, proto, fds.as_mut_ptr()) };
    #[cfg(not(target_os = "linux"))]
    let rc = unsafe { libc::socketpair(domain, ty, proto, fds.as_mut_ptr()) };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY：socketpair 成功即两个有效 fd。
    let (a, b) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    #[cfg(not(target_os = "linux"))]
    {
        // 第二个 fcntl 失败 ⇒ 两个都关（OwnedFd drop；不留半带标志的一对）。
        set_cloexec(std::os::fd::AsFd::as_fd(&a))?;
        set_cloexec(std::os::fd::AsFd::as_fd(&b))?;
    }
    Ok((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd as _;

    fn cloexec_set(fd: BorrowedFd<'_>) -> bool {
        let f = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
        assert!(f >= 0, "F_GETFD 失败：{}", io::Error::last_os_error());
        f & libc::FD_CLOEXEC != 0
    }

    /// F1：三种 helper 的产物**都带** `FD_CLOEXEC`；`set_cloexec` 幂等。
    /// （修前红：原 `libc::pipe`/`socket`/`socketpair` 直建点不带此位。）
    #[test]
    fn created_fds_carry_cloexec_and_set_is_idempotent() {
        let (r, w) = pipe_cloexec().unwrap();
        assert!(cloexec_set(r.as_fd()) && cloexec_set(w.as_fd()), "管道两端带 CLOEXEC");

        let s = socket_cloexec(libc::AF_UNIX, libc::SOCK_STREAM, 0).unwrap();
        assert!(cloexec_set(s.as_fd()), "socket 带 CLOEXEC");

        let (a, b) = socketpair_cloexec(libc::AF_UNIX, libc::SOCK_DGRAM, 0).unwrap();
        assert!(cloexec_set(a.as_fd()) && cloexec_set(b.as_fd()), "socketpair 两端带 CLOEXEC");

        // 幂等（重复设置不失败、位不丢）
        set_cloexec(s.as_fd()).unwrap();
        set_cloexec(s.as_fd()).unwrap();
        assert!(cloexec_set(s.as_fd()));
    }

    /// `SUN_PATH_MAX` 的平台值（若无 `sun_path` 布局差异须显式核对——见常量文档）。
    #[test]
    fn sun_path_max_matches_platform() {
        #[cfg(target_os = "macos")]
        assert_eq!(SUN_PATH_MAX, 103);
        #[cfg(target_os = "linux")]
        assert_eq!(SUN_PATH_MAX, 107);
        // 结构自洽：容量 − 1
        assert_eq!(SUN_PATH_MAX, std::mem::size_of::<libc::sockaddr_un>() - std::mem::offset_of!(libc::sockaddr_un, sun_path) - 1);
    }

    /// F1 端到端**真继承探针**：helper 产出的 fd 不得出现在 exec 后的子进程 fd 表里。
    ///
    /// 形态：`/bin/sh -c '[ -e /dev/fd/<N> ] && exit 3'`——`[` 是 shell 内建（子进程
    /// 不会新开 fd 复用号），故「号不出现」= 真未继承。
    ///
    /// **为什么不用「自重入 test harness + dup2 高位号」**（设计 U6 的形态）：
    /// `dup2` 按 POSIX **清除** 新号的 `FD_CLOEXEC` ⇒ 探针恒绿（连修前也绿），与
    /// §4.2 的「修前同探针红」自相矛盾；而直接用低位号 + 自重入 harness 又有
    /// harness 复用号面。真 exec 一个 shell 并只看内建 `[` 的判定，两个坑同时避开。
    #[test]
    fn created_fd_not_inherited_across_exec() {
        let (r, w) = pipe_cloexec().unwrap();
        let n = w.as_raw_fd();
        let st = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("[ -e /dev/fd/{n} ] && exit 3; exit 0"))
            .status()
            .expect("/bin/sh 起不来");
        assert!(
            st.success(),
            "CLOEXEC fd（号 {n}）出现在子进程 /dev/fd 里（exit={:?}）—— 未被 exec 关闭",
            st.code()
        );
        drop(r);
        drop(w);
    }
}
