//! CLI 停止信号面（WIN 批 2026-10-13；设计 `docs/reviews/WIN-design.md` §D8）。
//!
//! 收敛点：serve 前台 / relay 前台（arm/disarm）/ daemon status watch 三处此前各持一份
//! 「STOP_FD 静态 + extern handler + 自写管道」的重复实现——本模块单源化，并给 windows
//! 臂（控制台事件 + 回环 TCP 唤醒对）同形语义：
//!
//! - **unix**：`libc::signal(SIGTERM/SIGINT)` + `pipe(2)`（CLOEXEC 经 `sysfd`）——与
//!   改前三处实现逐调用同形（行为零变化）。
//! - **windows**：`SetConsoleCtrlHandler`（CTRL_C/CTRL_CLOSE/BREAK/LOGOFF/SHUTDOWN——
//!   handler 跑在系统线程：置命中标志 + 向 armed 句柄 `send` 1B；socket 写是 Windows 上
//!   的 async-signal 安全面）；停止管道 = 回环 TCP 对（`sysock::WakePair`）。**SIGTERM
//!   形态不可捕获**（`taskkill` 走硬终止）——服务管理请走控制面/控制台事件。
//!
//! arm/disarm 纪律（沿 relay_cli 原不变式，QG.md §4）：disarm = `ARMED` 先置 `DISARMED`
//! （in-flight handler 的写窗口由「先 swap 再 join/close」顺序兜住）；恢复默认处置只在
//! 「本面曾 armed」时做（统一进程的宿主 handler 不被打掉）。

use homeway_core::sysock;

/// 裸句柄值（unix fd / windows SOCKET 句柄）。
pub type Raw = sysock::RawSock;

/// 「未 armed」哨兵（unix = -1 / windows = u64::MAX）。
#[cfg(unix)]
pub const DISARMED: Raw = -1;
#[cfg(windows)]
pub const DISARMED: Raw = u64::MAX;

/// armed 句柄（handler 的写目标；进程级单源——三处消费面共用，同一时刻只有一面
/// 真正 arm）。`DISARMED` = 不写。
static ARMED: std::sync::atomic::AtomicUsize = {
    #[cfg(unix)]
    {
        std::sync::atomic::AtomicUsize::new(-1i32 as usize)
    }
    #[cfg(windows)]
    {
        std::sync::atomic::AtomicUsize::new(u64::MAX as usize)
    }
};

/// 命中标志（信号/控制台事件到过——relay 前台「正常收工」的判据面）。
pub static HIT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// handler 本体（unix = extern "C" 信号语境；windows 控制台 handler 复用同主体）。
#[cfg(unix)]
extern "C" fn on_stop_signal(_sig: i32) {
    on_stop();
}

#[cfg(windows)]
unsafe extern "system" fn on_console_event(_ctrl: u32) -> i32 {
    on_stop();
    1 // TRUE = 已处理（不落默认处置——默认会立即杀进程）
}

/// handler 主体：置命中 + （若 armed）写一字节（失败静默——合并位语义）。
/// 写面走平台分派的 [`write1`]（**unix 的停止管道是管道**——socket 系 send(2) 对
/// pipe fd 报 ENOTSOCK 且被静默吞掉；评审 H1 实测钉，勿改回 send）。
fn on_stop() {
    HIT.store(true, std::sync::atomic::Ordering::SeqCst);
    let fd = ARMED.load(std::sync::atomic::Ordering::SeqCst) as Raw;
    if fd != DISARMED {
        write1(fd);
    }
}

// ---------- 原语（两平台分派） ----------

/// 建停止管道（unix = `pipe(2)` 两端 CLOEXEC；windows = 回环 TCP 对）。返回
/// (读端, 写端) 的**裸值**——所有权转移给调用方（`close` 收口；WakePair 的 RAII
/// 端在本函数内显式 forget，句柄只经裸值存续）。
pub fn pipe() -> std::io::Result<(Raw, Raw)> {
    let wp = sysock::WakePair::new()?;
    let r = sysock::sock_raw(&wp.read);
    let w = sysock::sock_raw(&wp.write);
    std::mem::forget(wp);
    Ok((r, w))
}

/// 写一个停止字节（unix `write(2)` / windows `send`；失败静默——「写满 = 已有
/// 待收信号」的合并位语义。测试面也用它注入）。
#[allow(dead_code)]
pub fn write1(fd: Raw) {
    #[cfg(unix)]
    unsafe {
        let b = b"x";
        let _ = libc::write(fd, b.as_ptr().cast(), 1);
    }
    #[cfg(windows)]
    {
        let _ = sysock::send(fd, b"x");
    }
}

/// 阻塞读（serve 前台的 `wait_stop_pipe`）。**unix 的停止管道是管道**：socket 系
/// recv(2) 对 pipe fd 报 ENOTSOCK ⇒ 走 read(2)（与 `sysock::wake_drain` 同钉）。
pub fn read(fd: Raw, buf: &mut [u8]) -> std::io::Result<usize> {
    #[cfg(unix)]
    unsafe {
        let n = libc::read(fd, buf.as_mut_ptr().cast(), buf.len());
        if n >= 0 {
            Ok(n as usize)
        } else {
            Err(std::io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        sysock::recv(fd, buf)
    }
}

/// 非阻塞位（watch 循环的读端）。
pub fn set_nonblocking(fd: Raw, on: bool) -> std::io::Result<()> {
    sysock::raw_set_nonblocking(fd, on)
}

/// 关闭裸句柄（drop 收口）。
pub fn close(fd: Raw) {
    #[cfg(unix)]
    unsafe {
        libc::close(fd);
    }
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::Networking::WinSock::closesocket(fd as usize);
    }
}

// ---------- arm/disarm 与安装 ----------

/// arm：handler 的写目标（`DISARMED` = disarm）。**属主纪律**（评审 H2）：ARMED 是
/// 进程级共享槽——只有「本面调过 `arm`」的消费方才可在收线时 `disarm_and_restore`，
/// 否则只 `arm(DISARMED)` 断写面（统一进程的宿主 handler 不被打掉）。
pub fn arm(fd: Raw) {
    ARMED.store(fd as usize, std::sync::atomic::Ordering::SeqCst);
}

/// 当前的 armed 值（测试断言面）。
#[allow(dead_code)]
pub fn armed() -> Raw {
    ARMED.load(std::sync::atomic::Ordering::SeqCst) as Raw
}

/// 装停止信号（unix = SIGTERM/SIGINT；windows = 控制台事件）。幂等（unix 的
/// `libc::signal` 重复装无害；windows 的 handler 常驻）。
pub fn install() {
    #[cfg(unix)]
    unsafe {
        let h = on_stop_signal as extern "C" fn(i32) as libc::sighandler_t;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGINT, h);
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::System::Console::SetConsoleCtrlHandler;
        // 幂等：重复注册同地址在 Windows 上返回 ERROR_INVALID_PARAMETER——用一次性
        // 标记防重复装（首次失败按静默降级：无控制台事件面时进程仍可控制面停止）。
        use std::sync::Once;
        static ONCE: Once = Once::new();
        ONCE.call_once(|| {
            SetConsoleCtrlHandler(Some(on_console_event), 1);
        });
    }
}

/// disarm + 恢复默认处置（仅 unix 有恢复面；windows 的 handler 常驻无害——HIT 置位
/// 但无消费者时自然空转）。返回「此前是否 armed」（relay 的「只在本面曾 arm 时恢复」
/// 语义）。
pub fn disarm_and_restore() -> bool {
    let was = ARMED.swap(DISARMED as usize, std::sync::atomic::Ordering::SeqCst)
        != DISARMED as usize;
    #[cfg(unix)]
    if was {
        unsafe {
            libc::signal(libc::SIGTERM, libc::SIG_DFL);
            libc::signal(libc::SIGINT, libc::SIG_DFL);
        }
    }
    was
}
