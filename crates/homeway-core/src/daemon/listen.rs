//! control.sock 监听（语义真源 `baseline:internal/control/listen.go`，spec「端点与
//! 认证」）：凭证语义 = socket 属主（0600 + 目录 0700——权限位在 linux 与 darwin
//! 都参与 connect 判定）；协议内无 token 类凭证字段；MUST NOT 在任何物理网络接口
//! 新增监听（UDS 之外无监听面）。
//! WIN 批（2026-10-13）：windows = TCP loopback + 端口文件（`ipc.rs`，设计 §D2）——
//! sun_path 长度面与 0600/0700 收紧是 unix 凭证面专属，windows 臂整体跳过。
//!
//! 残留 socket 处理（launchd/kill -9 残留场景）：connect 探测——
//!   - 探测连通 = 另一活实例占着 → 报错退出（不接管活监听点）；
//!   - ENOENT / ECONNREFUSED = 死残留（监听者已死只剩文件）→ 清除后重 bind；
//!   - 其它模糊结果（超时等）→ 按占用不明处理：不删状态不明的东西，报错。

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::ipc::{self, IpcListener};
#[cfg(unix)]
use crate::sysfd::SUN_PATH_MAX;

/// state 目录下的控制面 socket 文件名。
pub const CONTROL_SOCK_NAME: &str = "control.sock";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ListenError {
    /// sun_path 长度上限面（unix 专属——windows TCP loopback 无路径长度限制，WIN 批 §D2）。
    #[cfg(unix)]
    #[error("socket 路径超长（{0} 字节 > {SUN_PATH_MAX}，sun_path 上限〔平台值〕）")]
    PathTooLong(usize),
    #[error("control.sock 已被另一个活实例占用（同 state 双实例？）")]
    Occupied,
    #[error("control.sock 占用状态不明（{0}），不接管")]
    Ambiguous(String),
    #[error("清残留 control.sock 失败：{0}")]
    RemoveFailed(String),
    #[error("监听失败：{0}")]
    Bind(String),
    #[error("control.sock 收紧 0600 失败：{0}")]
    ChmodSock(String),
    #[error("IO：{0}")]
    Io(#[from] std::io::Error),
}

/// 在 state 目录监听 control.sock：残留探测 → 清除/报错 → **目录收紧 0700（bind
/// 之前）** → bind → 显式 chmod 0600（不依赖 umask 的偶然值）。
///
/// Q-G F4：目录 0700 从 bind 之后**前置**到 bind 之前（跨用户暴露面在 socket 存在
/// 之前就关闭）；第二层防御语义不变（同时护住 state 里的身份密钥等非 socket 文件）。
pub fn listen_control(state_dir: &Path) -> Result<(PathBuf, IpcListener), ListenError> {
    let sock = state_dir.join(CONTROL_SOCK_NAME);
    #[cfg(unix)]
    if sock.as_os_str().len() > SUN_PATH_MAX {
        // sun_path 平台真实可用长度（darwin 103 / linux·OHOS 107——单源 `sysfd`）。
        return Err(ListenError::PathTooLong(sock.as_os_str().len()));
    }
    // 残留探测：连通 = 活实例占用（`ipc::probe_alive` 三态收口：Ok(true) = 活占用 /
    // Ok(false) = 死残留·不存在 / Err = 模糊——与改前 connect 探测同语义，windows 臂
    // 走端口文件 + TCP connect）。
    match ipc::probe_alive(&sock) {
        Ok(true) => return Err(ListenError::Occupied),
        Ok(false) => {} // 死残留/不存在 → 清除后 bind
        Err(e) => return Err(ListenError::Ambiguous(e.to_string())),
    }
    match ipc::remove_stale(&sock) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(ListenError::RemoveFailed(e.to_string())),
    }
    // 目录 0700（**bind 之前**；OpenNodeState 已建，这里幂等再收紧并作为装配断言）。
    // 与 Go 口径一致：目录收紧失败告警不阻断（socket 0600 那层还兜着）。
    // WIN 批：unix 凭证面专属（windows = TCP loopback，无 socket 文件可收紧）。
    #[cfg(unix)]
    if let Err(e) = std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700)) {
        eprintln!(
            "homeway: ⚠️ state 目录 {} 收紧 0700 失败（{e}）——socket 0600 仍是边界",
            state_dir.display()
        );
    }
    let ln = IpcListener::bind(&sock).map_err(|e| ListenError::Bind(e.to_string()))?;
    // bind 后显式 chmod 0600（0600 即拦非属主 connect；不依赖 umask）。
    // WIN 批：同上——unix 凭证面专属。
    #[cfg(unix)]
    if let Err(e) = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)) {
        let msg = e.to_string();
        drop(ln);
        let _ = std::fs::remove_file(&sock);
        return Err(ListenError::ChmodSock(msg));
    }
    Ok((sock, ln))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(unix)] // WIN 批：死残留造法（fd 直关不 unlink）与 0600 权限位断言 = unix 专属语义
    fn listen_binds_and_second_listen_refuses() {
        let dir = std::env::temp_dir().join(format!("hw-ctl-listen-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // 死残留（无监听者的 socket 文件——kill -9 留下的形态）：bind 一个真
        // listener 后直接关 fd 不 unlink（macOS 对「connect 非 socket 普通文件」
        // 报 ENOTSOCK ≠ ECONNREFUSED，普通文件模拟不出该场景）。
        let sock = dir.join(CONTROL_SOCK_NAME);
        {
            use std::os::unix::io::IntoRawFd;
            let ln0 = std::os::unix::net::UnixListener::bind(&sock).unwrap();
            let fd = ln0.into_raw_fd();
            unsafe { libc::close(fd) };
        }
        let (path, ln) = listen_control(&dir).unwrap();
        assert_eq!(path, sock);
        // 0600 收紧到位。
        let mode = std::fs::metadata(&sock).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        // 活占用：第二个 listener 探测到连通 → 拒绝。
        assert!(matches!(listen_control(&dir), Err(ListenError::Occupied)));
        drop(ln);
        // 监听者正常收工（drop 会 unlink——ENOENT 形态）→ 直接重 bind。（ECONNREFUSED
        // 死残留清除路径已由本测试首段覆盖；macOS 上「关 fd 留名」的再 bind 有内核
        // 地址滞留形态，不作为断言面。）
        let (path2, _ln2) = listen_control(&dir).unwrap();
        assert_eq!(path2, sock);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Q-G F4：**目录 0700 前置**——在 0755 目录上调 `listen_control`，返回后目录
    /// 已是 0700（旧形态在 bind 之后才收紧）。
    #[test]
    #[cfg(unix)] // WIN 批：目录 0700 权限位断言 = unix 专属语义
    fn listen_control_tightens_dir_to_0700() {
        let dir = std::env::temp_dir().join(format!("hw-ctl-dir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (_, ln) = listen_control(&dir).unwrap();
        let mode = std::fs::metadata(&dir).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "目录必须在 bind 之前/之后恒为 0700");
        drop(ln);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Q-G F4.3：深路径正例/负例——**按完整 socket 路径长度**构造（`state_dir` +
    /// `/control.sock` 后缀 13 B 含分隔符）：`== SUN_PATH_MAX` 可 bind；
    /// `== SUN_PATH_MAX + 1` 必 `PathTooLong`（off-by-one 已订正：放行 103/107）。
    #[test]
    #[cfg(unix)] // WIN 批：SUN_PATH_MAX 边界 = sun_path（unix）专属语义
    fn listen_control_path_limit_boundary() {
        fn dir_with_full_len(total: usize) -> PathBuf {
            // 尾分隔符归一：macOS 的 `TMPDIR` 带尾斜杠（`join` 不再补分隔符）、CI 的 `/tmp`
            // 不带 —— 不归一则 `join` 后的长度差 1 B（ubuntu CI 实测左 95 / 右 94）。
            let mut s = std::env::temp_dir().to_string_lossy().into_owned();
            while s.len() > 1 && s.ends_with('/') {
                s.pop();
            }
            let base = PathBuf::from(s); // 已去尾分隔符 ⇒ join 必补一个 "/"
            let want_dir = total - (CONTROL_SOCK_NAME.len() + 1);
            let base_len = base.as_os_str().len();
            // 目标长度 = base_len + 1（分隔符）+ name_len（1 + pad）
            assert!(want_dir >= base_len + 2, "临时目录太深，构造不出目标长度");
            let pad = want_dir - base_len - 2;
            let mut name = String::from("q");
            name.push_str(&"g".repeat(pad));
            let d = base.join(name);
            assert_eq!(d.as_os_str().len(), want_dir);
            d
        }

        // 正例：完整路径 == SUN_PATH_MAX（darwin 103 / linux 107）可 bind。
        let ok_dir = dir_with_full_len(SUN_PATH_MAX);
        let _ = std::fs::remove_dir_all(&ok_dir);
        std::fs::create_dir_all(&ok_dir).unwrap();
        let (p, ln) = listen_control(&ok_dir).expect("SUN_PATH_MAX 长度必须可 bind");
        assert_eq!(p.as_os_str().len(), SUN_PATH_MAX);
        drop(ln);
        let _ = std::fs::remove_dir_all(&ok_dir);

        // 负例：完整路径 == SUN_PATH_MAX + 1 必拒。
        let bad_dir = dir_with_full_len(SUN_PATH_MAX + 1);
        let _ = std::fs::remove_dir_all(&bad_dir);
        std::fs::create_dir_all(&bad_dir).unwrap();
        match listen_control(&bad_dir) {
            Err(ListenError::PathTooLong(n)) => assert_eq!(n, SUN_PATH_MAX + 1),
            // （IpcListener 无 Debug——映射成路径再格式化，保 panic 文案的可读面）
            other => panic!("超限路径必须 PathTooLong，得 {:?}", other.map(|(p, _)| p)),
        }
        let _ = std::fs::remove_dir_all(&bad_dir);
    }
}
