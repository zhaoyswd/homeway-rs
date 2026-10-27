//! 统一进程持有本地终端，出口只借用：关闭网络出口不会结束本地工作。
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::term::service::TermService;

pub(crate) struct LocalTerm {
    service: Arc<TermService>,
    socket: PathBuf,
    listener: Option<std::thread::JoinHandle<()>>,
}

impl LocalTerm {
    pub(crate) fn start(
        state: &Path,
        log: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> std::io::Result<Option<Self>> {
        Self::start_with(state, log, |service, listener| {
            std::thread::Builder::new()
                .name("homeway-local-term".into())
                .spawn(move || service.serve(listener))
        })
    }

    fn start_with(
        state: &Path,
        log: Arc<dyn Fn(&str) + Send + Sync>,
        spawn: impl FnOnce(
            Arc<TermService>,
            UnixListener,
        ) -> std::io::Result<std::thread::JoinHandle<()>>,
    ) -> std::io::Result<Option<Self>> {
        if homeway_core::term::service::disabled_by_env() {
            return Ok(None);
        }
        let listener =
            homeway_core::files_server::listen_local_service(state, "local-term.sock", &log)?;
        let homeway_core::daemon::ipc::IpcListener::Unix(listener) = listener;
        // 使用既有出口目录的manifest覆盖配置，本地与远端共用同一检测引擎。
        let service = TermService::new(log, Some(&state.join("serve")));
        let serving = Arc::clone(&service);
        let handle = match spawn(serving, listener) {
            Ok(handle) => handle,
            Err(error) => {
                service.close();
                let _ = std::fs::remove_file(state.join("local-term.sock"));
                return Err(error);
            }
        };
        Ok(Some(Self {
            service,
            socket: state.join("local-term.sock"),
            listener: Some(handle),
        }))
    }

    pub(crate) fn service(&self) -> Arc<TermService> {
        Arc::clone(&self.service)
    }
}

pub(crate) fn local_terminal_alive(state: &Path) -> bool {
    homeway_core::daemon::ipc::probe_alive(&state.join("local-term.sock")).unwrap_or(false)
}

pub(crate) fn local_terminal_path(state: &Path) -> PathBuf {
    if local_terminal_alive(state) {
        return state.join("local-term.sock");
    }
    let flat = state.join("term.sock");
    if flat.exists() {
        return flat;
    }
    let nested = state.join("serve/term.sock");
    if nested.exists() {
        nested
    } else {
        flat
    }
}

/// 连接时再次回退，覆盖探测后服务退出的竞态；权限等非“死残留”错误保持可行动。
pub(crate) fn connect_local_terminal(state: &Path) -> std::io::Result<UnixStream> {
    connect_local_terminal_with_budget(state, Duration::from_secs(10))
}

pub(crate) fn connect_local_terminal_with_budget(
    state: &Path,
    budget: Duration,
) -> std::io::Result<UnixStream> {
    let deadline = Instant::now() + budget;
    let mut last = None;
    for path in [
        state.join("local-term.sock"),
        state.join("term.sock"),
        state.join("serve/term.sock"),
    ] {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        match homeway_core::daemon::ipc::IpcStream::connect_timeout(&path, remaining) {
            Ok(homeway_core::daemon::ipc::IpcStream::Unix(stream)) => return Ok(stream),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
                ) =>
            {
                last = Some(error)
            }
            Err(error) => return Err(error),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "本地终端不存在")))
}

impl Drop for LocalTerm {
    fn drop(&mut self) {
        self.service.close();
        // 唤醒阻塞accept；服务已置stop，接受后立即退工，不建立会话。
        let _ = UnixStream::connect(&self.socket);
        if let Some(listener) = self.listener.take() {
            let _ = listener.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeway_core::server::engine::{BindMode, ServeConfig, ServeEngine};
    use homeway_core::term::frames::{self, Op};
    use homeway_core::term::wire::FrameIo;
    use std::time::Duration;

    fn temporary(tag: &str) -> PathBuf {
        let root = PathBuf::from("/tmp").join(format!(
            "hw-local-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn stale_process_socket_falls_back_without_deleting_residual_file() {
        let root = temporary("stale");
        drop(UnixListener::bind(root.join("local-term.sock")).unwrap());
        let listener = UnixListener::bind(root.join("term.sock")).unwrap();
        assert!(!local_terminal_alive(&root));
        assert_eq!(local_terminal_path(&root), root.join("term.sock"));
        let stream = connect_local_terminal(&root).unwrap();
        let _ = listener.accept().unwrap();
        assert!(root.join("local-term.sock").exists());
        drop(stream);
        drop(listener);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_listener_thread_spawn_removes_bound_socket() {
        let root = temporary("spawn-fail");
        let result = LocalTerm::start_with(&root, Arc::new(|_| {}), |_service, _listener| {
            Err(std::io::Error::other("injected spawn failure"))
        });
        assert!(result.is_err());
        assert!(!root.join("local-term.sock").exists());
        std::fs::remove_dir_all(root).unwrap();
    }

    fn request(path: &Path, op: Op, body: &[u8]) -> frames::Frame {
        let mut io = FrameIo::new(UnixStream::connect(path).unwrap());
        assert_eq!(
            io.read_frame_deadline(Duration::from_secs(3)).unwrap().op,
            Op::GREETING
        );
        io.write_frame(op, body, Duration::from_secs(3)).unwrap();
        io.read_frame_deadline(Duration::from_secs(3)).unwrap()
    }

    #[test]
    fn local_sessions_share_exit_registry_and_survive_exit_stop_restart() {
        let root = PathBuf::from("/tmp").join(format!(
            "hw-tui-share-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let log: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
        let local = LocalTerm::start(&root, Arc::clone(&log)).unwrap().unwrap();
        let local_socket = root.join("local-term.sock");
        let created = request(&local_socket, Op::CREATE, &frames::enc_create(0, "shared"));
        assert_eq!(created.op, Op::OK);
        let config = ServeConfig {
            state_dir: root.clone(),
            listen_port: 0,
            quic_listen_port: Some(0),
            bind_iface: BindMode::Addr("127.0.0.1".parse().unwrap()),
            upnp: false,
            stun: String::new(),
            stun6: String::new(),
            files_port: 0,
            speedtest_port: 0,
            dns_port: 0,
            dns_probe_target: Vec::new(),
            stun_probe_target: Vec::new(),
            ..ServeConfig::default()
        };
        let assert_shared = |path: &Path| {
            let listed = request(path, Op::LIST, &[]);
            assert_eq!(listed.op, Op::LIST);
            let value: serde_json::Value = serde_json::from_slice(&listed.payload).unwrap();
            assert!(value["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["name"] == "shared"));
        };
        for _ in 0..2 {
            let engine = ServeEngine::start_with_term_service(
                config.clone(),
                Arc::clone(&log),
                Arc::clone(&log),
                Arc::clone(&log),
                None,
                Some(local.service()),
            )
            .unwrap();
            assert_shared(&root.join("serve/term.sock"));
            engine.shutdown(Duration::ZERO);
            assert_shared(&local_socket);
        }
        assert_eq!(
            request(&local_socket, Op::KILL, &frames::enc_name("shared")).op,
            Op::OK
        );
        drop(local);
        assert!(!local_socket.exists());
        std::fs::remove_dir_all(root).unwrap();
    }
}
