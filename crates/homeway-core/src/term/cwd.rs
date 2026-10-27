//! Native, read-only working-directory probes for a PTY's shell process.
use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub(crate) enum Error {
    #[error("invalid shell PID")]
    InvalidPid,
    #[error("process cwd: {0}")]
    Io(#[from] std::io::Error),
    #[error("process cwd is empty, truncated or not absolute")]
    InvalidPath,
    #[cfg(target_os = "macos")]
    #[error("proc_pidinfo returned {actual} bytes, expected {expected}")]
    ShortInfo { actual: i32, expected: i32 },
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[error("native process cwd is unavailable on this platform")]
    Unsupported,
}

pub(crate) fn process_cwd(pid: i32) -> Result<PathBuf, Error> {
    if pid <= 0 {
        return Err(Error::InvalidPid);
    }
    let path = platform_cwd(pid)?;
    if !path.is_absolute() {
        return Err(Error::InvalidPath);
    }
    Ok(path)
}

#[cfg(target_os = "linux")]
fn platform_cwd(pid: i32) -> Result<PathBuf, Error> {
    Ok(std::fs::read_link(format!("/proc/{pid}/cwd"))?)
}

#[cfg(target_os = "macos")]
fn platform_cwd(pid: i32) -> Result<PathBuf, Error> {
    use std::os::unix::ffi::OsStringExt;
    let mut info = std::mem::MaybeUninit::<libc::proc_vnodepathinfo>::zeroed();
    let expected = std::mem::size_of::<libc::proc_vnodepathinfo>() as libc::c_int;
    // SAFETY: the writable buffer is exactly the size requested by this flavor.
    let actual = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            info.as_mut_ptr().cast(),
            expected,
        )
    };
    if actual <= 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    if actual != expected {
        return Err(Error::ShortInfo { actual, expected });
    }
    // SAFETY: a successful full-size call initialized the zeroed, integer-only C struct.
    let info = unsafe { info.assume_init() };
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|byte| *byte as u8)
        .take_while(|byte| *byte != 0)
        .collect();
    if bytes.is_empty() || bytes.len() == 1024 {
        return Err(Error::InvalidPath);
    }
    Ok(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn platform_cwd(_pid: i32) -> Result<PathBuf, Error> {
    Err(Error::Unsupported)
}

#[cfg(all(test, any(target_os = "macos", target_os = "linux")))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    struct Fixture {
        child: Child,
        root: PathBuf,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }
    fn wait_file(path: &std::path::Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !path.exists() {
            assert!(
                Instant::now() < deadline,
                "private child did not reach its cwd checkpoint"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn native_cwd_follows_owned_child_cd_without_terminal_reports() {
        let root = PathBuf::from("/tmp").join(format!(
            "homeway-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let first = root.join("initial shell");
        let second = root.join("project-中文");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        let ready = root.join("ready");
        let moved = root.join("moved");
        let child = Command::new("/bin/sh").args(["-c", "printf ready > \"$1\"; IFS= read -r line; cd \"$2\" || exit; printf moved > \"$3\"; IFS= read -r line", "cwd-probe"])
            .arg(&ready).arg(&second).arg(&moved).current_dir(&first)
            .stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
        let mut fixture = Fixture { child, root };
        wait_file(&ready);
        let pid = fixture.child.id() as i32;
        assert_eq!(
            process_cwd(pid).unwrap(),
            std::fs::canonicalize(&first).unwrap()
        );
        fixture
            .child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"change\n")
            .unwrap();
        wait_file(&moved);
        assert_eq!(
            process_cwd(pid).unwrap(),
            std::fs::canonicalize(&second).unwrap()
        );
    }
}
