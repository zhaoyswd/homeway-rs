//! 跨平台排它文件锁（WIN 批 2026-10-13）。
//!
//! unix = `flock(LOCK_EX|LOCK_NB)`（原样——持有进程死亡即内核释放，无陈旧文件）；
//! windows = `LockFileEx(LOCKFILE_EXCLUSIVE_LOCK|LOCKFILE_FAIL_IMMEDIATELY, 全文件域)`
//! ——进程退出句柄即关、锁即放，判活性同构。[`LockOutcome::Held`] = 他者持有
//! （unix EWOULDBLOCK/EACCES；windows ERROR_LOCK_VIOLATION——两族同义映射）。

use std::fs::File;
use std::io;

/// 拿锁结论。
pub enum LockOutcome {
    /// 拿到（随 File 句柄生命周期释放——drop 即放）。
    Acquired,
    /// 他者持有（内容自述仍可读——错误信息面用）。
    Held,
}

/// 尝试非阻塞排它锁（`file` 以读写打开为前提；不消费句柄——锁跟句柄走）。
pub fn try_lock_exclusive(file: &File) -> io::Result<LockOutcome> {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd as _;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(LockOutcome::Acquired);
        }
        let err = io::Error::last_os_error();
        // EWOULDBLOCK 与 EAGAIN 在 POSIX 同值（11）——只匹配一个（部分目标报
        // unreachable pattern；同 session_lock 原注释）。
        let eagain = libc::EWOULDBLOCK;
        match err.raw_os_error() {
            Some(e) if e == eagain || e == libc::EACCES => Ok(LockOutcome::Held),
            _ => Err(err),
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::Storage::FileSystem::{
            LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
        };
        const ERROR_LOCK_VIOLATION: u32 = 33;
        let mut overlapped: windows_sys::Win32::System::IO::OVERLAPPED = unsafe {
            std::mem::zeroed()
        };
        // 区段 = 字节 0（评审 L2）：`0,0`（len=0 = 「到 EOF」的方言口径）与
        // `!0,!0` 都有争议，锁住**首字节**是各 Windows 锁实现的最小共识互斥面——
        // 所有持有者锁同一字节 ⇒ 互斥成立；解锁随句柄关闭。待 windows 测试面
        // （后续批）加两进程互斥断言钉死。
        let rc = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        };
        if rc != 0 {
            return Ok(LockOutcome::Acquired);
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            Some(e) if e == ERROR_LOCK_VIOLATION as i32 => Ok(LockOutcome::Held),
            _ => Err(err),
        }
    }
}
