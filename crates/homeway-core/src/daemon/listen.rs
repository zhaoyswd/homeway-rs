//! control.sock 监听（语义真源 `baseline:internal/control/listen.go`，spec「端点与
//! 认证」）：凭证语义 = socket 属主（0600 + 目录 0700——权限位在 linux 与 darwin
//! 都参与 connect 判定）；协议内无 token 类凭证字段；MUST NOT 在任何物理网络接口
//! 新增监听（UDS 之外无监听面）。
//!
//! 残留 socket 处理（launchd/kill -9 残留场景）：connect 探测——
//!   - 探测连通 = 另一活实例占着 → 报错退出（不接管活监听点）；
//!   - ENOENT / ECONNREFUSED = 死残留（监听者已死只剩文件）→ 清除后重 bind；
//!   - 其它模糊结果（超时等）→ 按占用不明处理：不删状态不明的东西，报错。

use std::io::ErrorKind;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// state 目录下的控制面 socket 文件名。
pub const CONTROL_SOCK_NAME: &str = "control.sock";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ListenError {
    #[error("socket 路径超长（{0} 字节 ≥ 100，sun_path 上限）")]
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

/// 在 state 目录监听 control.sock：残留探测 → 清除/报错 → bind → 显式 chmod 0600
/// （不依赖 umask 的偶然值）→ 目录收紧 0700（双层防御：第二层同时护住 state 里的
/// 身份密钥等非 socket 文件）。
pub fn listen_control(state_dir: &Path) -> Result<(PathBuf, UnixListener), ListenError> {
    let sock = state_dir.join(CONTROL_SOCK_NAME);
    let sock_len = sock.as_os_str().len();
    if sock_len >= 100 {
        // sockaddr_un.sun_path 保守上限（darwin 104 / linux 108）。
        return Err(ListenError::PathTooLong(sock_len));
    }
    // 残留探测：连通 = 活实例占用。
    match std::os::unix::net::UnixStream::connect(&sock) {
        Ok(c) => {
            drop(c);
            return Err(ListenError::Occupied);
        }
        Err(e) => match e.kind() {
            ErrorKind::NotFound | ErrorKind::ConnectionRefused => {} // 死残留/不存在 → 清除后 bind
            _ => return Err(ListenError::Ambiguous(e.to_string())),
        },
    }
    match std::fs::remove_file(&sock) {
        Ok(()) => {}
        Err(e) if e.kind() == ErrorKind::NotFound => {}
        Err(e) => return Err(ListenError::RemoveFailed(e.to_string())),
    }
    let ln = UnixListener::bind(&sock).map_err(|e| ListenError::Bind(e.to_string()))?;
    // listen 后显式 chmod 0600（0600 即拦非属主 connect；不依赖 umask）。
    if let Err(e) = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)) {
        let msg = e.to_string();
        drop(ln);
        let _ = std::fs::remove_file(&sock);
        return Err(ListenError::ChmodSock(msg));
    }
    // 目录 0700（第二层；OpenNodeState 已建，这里幂等再收紧并作为装配断言）。
    // 与 Go 口径一致：目录收紧失败告警不阻断（socket 0600 那层还兜着）。
    if let Err(e) = std::fs::set_permissions(state_dir, std::fs::Permissions::from_mode(0o700)) {
        eprintln!(
            "homeway: ⚠️ state 目录 {} 收紧 0700 失败（{e}）——socket 0600 仍是边界",
            state_dir.display()
        );
    }
    Ok((sock, ln))
}

use std::os::unix::fs::PermissionsExt;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
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
            let ln0 = UnixListener::bind(&sock).unwrap();
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
}
