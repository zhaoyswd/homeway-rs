//! 本机条目（design D5 / tasks 3.2）：daemon 主机表只有远端后端（键 = token
//! peer id）——本机条目由面板合成。可用性判据 = **本机终端服务 socket 可连接**
//! （serve 运行但 `HOMEWAY_TERM=off` 时 sock 不存在 ⇒ 标记终端服务不可用而非
//! 可用，r2 R-8）；连接形态恒 `local`（spec 状态行）。路径 = `<state>/serve/
//! term.sock`（`serve_dir = state_dir.join("serve")` 恒定；**不复用 term_cli
//! 本地 dial 段**——其 `--state` 约定指向 serve 子目录，照抄会连错，r1 D-14）。


// 模型先于接线落地（tasks 3.2 的 serve.status 取数接线在下一批）——期间豁免 dead_code，接线后移除。
#![allow(dead_code)]
use std::os::unix::net::UnixStream;
use std::path::Path;

use super::hosts::HostView;

/// 本机条目的主机 id 哨兵（选中文件、显示序共用）。
pub const LOCAL_HOST_ID: &str = "local";

/// serve.status 的 state 值域（proto.rs:483 真源；非 running 四态统一归不可用）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ServeState {
    Running,
    Stopping,
    Stopped,
    Failed,
    Absent,
}

impl ServeState {
    pub fn from_wire(s: &str) -> Option<ServeState> {
        Some(match s {
            "running" => ServeState::Running,
            "stopping" => ServeState::Stopping,
            "stopped" => ServeState::Stopped,
            "failed" => ServeState::Failed,
            "absent" => ServeState::Absent,
            _ => return None,
        })
    }

    fn label(self) -> &'static str {
        match self {
            ServeState::Running => "running",
            ServeState::Stopping => "stopping",
            ServeState::Stopped => "stopped",
            ServeState::Failed => "failed",
            ServeState::Absent => "absent",
        }
    }
}

/// 本机终端服务可达性探测（连接即判可达，随即丢弃句柄——探测不承载协议）。
/// 本地 UDS connect 没有网络超时语义（不存在跨网等待），拒绝/缺失立即返回；
/// 病态的监听队列饱和由调用方线程隔离兜底（backend 的探测在后台线程）。
pub(crate) fn term_sock_reachable(serve_dir: &Path) -> bool {
    let sock = serve_dir.join("term.sock");
    UnixStream::connect(sock).is_ok()
}

/// 本机主机名（gethostname 失败回落「本机」——名字只影响显示）。
pub(crate) fn hostname() -> String {
    let mut buf = [0u8; 256];
    unsafe {
        if libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) == 0 {
            let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
            if let Ok(s) = std::str::from_utf8(&buf[..end]) {
                if !s.is_empty() {
                    return s.to_owned();
                }
            }
        }
    }
    "本机".to_owned()
}

/// 合成本机条目（恒有条目——serve 未运行时保留并标记不可用，spec 需求 2）。
///
/// - running + sock 可连 ⇒ 可用（state=ready）；
/// - running + sock 不可连 ⇒ 终端服务不可用（state=failed；HOMEWAY_TERM=off 面）；
/// - 非 running ⇒ 不可用（state=stopped，reason 带 serve 态）。
pub(crate) fn compose_local(hostname: &str, serve: ServeState, sock_reachable: bool) -> HostView {
    let (state, reason) = if serve == ServeState::Running {
        if sock_reachable {
            ("ready".to_owned(), None)
        } else {
            ("failed".to_owned(), Some("本机终端服务不可用".to_owned()))
        }
    } else {
        ("stopped".to_owned(), Some(format!("本机 serve {}", serve.label())))
    };
    HostView {
        id: LOCAL_HOST_ID.to_owned(),
        name: hostname.to_owned(),
        state,
        reason,
        via: Some("local".into()),
        rtt_ms: None,
        link_at: None,
        rx_bytes: None,
        tx_bytes: None,
        added_at: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serve_state_from_wire_all_five_plus_unknown() {
        for (w, want) in [
            ("running", ServeState::Running),
            ("stopping", ServeState::Stopping),
            ("stopped", ServeState::Stopped),
            ("failed", ServeState::Failed),
            ("absent", ServeState::Absent),
        ] {
            assert_eq!(ServeState::from_wire(w), Some(want));
        }
        assert_eq!(ServeState::from_wire("weird"), None, "未知态拒绝（归不可用前的护栏）");
    }

    #[test]
    fn compose_running_reachable_is_ready() {
        let v = compose_local("my-mac", ServeState::Running, true);
        assert_eq!((v.id.as_str(), v.state.as_str(), v.via.as_deref()), ("local", "ready", Some("local")));
        assert_eq!(v.reason, None);
        assert_eq!(v.name, "my-mac");
    }

    #[test]
    fn compose_running_but_sock_dead_marks_term_unavailable() {
        // HOMEWAY_TERM=off 面：serve 在跑、条目保留、标记终端服务不可用（非 ready）。
        let v = compose_local("my-mac", ServeState::Running, false);
        assert_eq!((v.state.as_str(), v.reason.as_deref()), ("failed", Some("本机终端服务不可用")));
        assert_eq!(v.via.as_deref(), Some("local"));
    }

    #[test]
    fn compose_not_running_is_unavailable_but_kept() {
        for s in [ServeState::Stopped, ServeState::Failed, ServeState::Absent, ServeState::Stopping] {
            let v = compose_local("my-mac", s, false);
            assert_eq!(v.state, "stopped", "{s:?} 统一归不可用");
            assert!(v.reason.as_deref().unwrap().contains("serve"));
        }
    }

    #[test]
    fn term_sock_probe_real_listener_and_missing() {
        let dir = std::env::temp_dir().join(format!("hwtui-local-{}-{}", std::process::id(), unique()));
        std::fs::create_dir_all(&dir).expect("临时目录");
        // 不存在的 sock = 不可达（不 panic）。
        assert!(!term_sock_reachable(&dir));
        // 真监听 = 可达。
        let l = std::os::unix::net::UnixListener::bind(dir.join("term.sock")).expect("bind");
        assert!(term_sock_reachable(&dir));
        drop(l);
    }

    #[test]
    fn hostname_non_empty() {
        assert!(!hostname().is_empty());
    }

    fn unique() -> u64 {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }
}
