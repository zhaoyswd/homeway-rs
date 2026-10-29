//! 剪贴板写入在后台完成；OSC52 仅报告“已请求”，不冒充系统确认。
use std::io::{Seek, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;

pub(super) const MAX_BYTES: usize = 32 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Purpose {
    Operation,
    Selection,
    Application,
}

pub(super) enum Copied {
    System,
    Terminal(String),
    Failed(String),
}

pub(super) struct CopyResult {
    pub operation: u64,
    pub purpose: Purpose,
    pub fact: String,
    pub status: Copied,
}

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("剪贴板忙，请稍后重试")]
    Busy,
    #[error("剪贴板任务已退出")]
    Closed,
}

pub(super) struct Clipboard {
    requests: SyncSender<Request>,
    results: Receiver<CopyResult>,
}

struct Request {
    value: String,
    operation: u64,
    purpose: Purpose,
    fact: String,
    active: Option<Arc<AtomicBool>>,
}

impl Request {
    fn active(&self) -> bool {
        self.active
            .as_ref()
            .is_none_or(|active| active.load(Ordering::Acquire))
    }
}

/// Dropping an attachment's pending write cancels work not yet started.
/// A helper already started while current cannot be undone.
pub(super) struct PendingWrite(Arc<AtomicBool>);

impl Drop for PendingWrite {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum PayloadError {
    #[error("剪贴板载荷头不完整")]
    Truncated,
    #[error("剪贴板载荷长度不符")]
    Length,
    #[error("剪贴板内容超过 32 KiB")]
    TooLarge,
    #[error("剪贴板内容不是有效 UTF-8：{0}")]
    Utf8(#[from] std::str::Utf8Error),
}

pub(super) fn application_write(payload: &[u8]) -> Result<Option<&str>, PayloadError> {
    let kind = *payload.first().ok_or(PayloadError::Truncated)?;
    // Never service clipboard reads or unknown kinds, including malformed ones.
    if kind != homeway_core::term::codec::clip_kind::WRITE {
        return Ok(None);
    }
    let header = payload.get(..3).ok_or(PayloadError::Truncated)?;
    let len = usize::from(u16::from_le_bytes([header[1], header[2]]));
    if payload.len() != 3 + len {
        return Err(PayloadError::Length);
    }
    if len > MAX_BYTES {
        return Err(PayloadError::TooLarge);
    }
    Ok(Some(std::str::from_utf8(&payload[3..])?))
}

impl Clipboard {
    pub(super) fn start() -> Self {
        let (requests, rx) = mpsc::sync_channel::<Request>(4);
        let (tx, results) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                let Some(result) = copy_request(request) else {
                    continue;
                };
                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        Self { requests, results }
    }

    #[cfg(test)]
    pub(super) fn recording() -> (Self, Receiver<(String, u64)>) {
        let (requests, rx) = mpsc::sync_channel::<Request>(4);
        let (_, results) = mpsc::sync_channel(4);
        let (tx, recorded) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(request) = rx.recv() {
                if !request.active() {
                    continue;
                }
                if tx.send((request.value, request.operation)).is_err() {
                    break;
                }
            }
        });
        (Self { requests, results }, recorded)
    }

    pub(super) fn copy(&self, value: String, operation: u64, fact: String) -> Result<(), Error> {
        self.request(Request {
            value,
            operation,
            purpose: Purpose::Operation,
            fact,
            active: None,
        })
    }

    pub(super) fn copy_selection(&self, value: String, operation: u64) -> Result<(), Error> {
        self.request(Request {
            value,
            operation,
            purpose: Purpose::Selection,
            fact: "选中内容".into(),
            active: None,
        })
    }

    pub(super) fn copy_application(
        &self,
        value: String,
        operation: u64,
    ) -> Result<PendingWrite, Error> {
        let pending = PendingWrite(Arc::new(AtomicBool::new(true)));
        self.request(Request {
            value,
            operation,
            purpose: Purpose::Application,
            fact: String::new(),
            active: Some(Arc::clone(&pending.0)),
        })?;
        Ok(pending)
    }

    #[cfg(test)]
    pub(super) fn blocked() -> (Self, impl Sized) {
        let (requests, queued) = mpsc::sync_channel(4);
        let (_, results) = mpsc::sync_channel(4);
        (Self { requests, results }, queued)
    }

    fn request(&self, request: Request) -> Result<(), Error> {
        self.requests.try_send(request).map_err(|e| match e {
            TrySendError::Full(_) => Error::Busy,
            TrySendError::Disconnected(_) => Error::Closed,
        })
    }

    pub(super) fn try_result(&self) -> Option<CopyResult> {
        self.results.try_recv().ok()
    }
}

fn copy_request(request: Request) -> Option<CopyResult> {
    if !request.active() {
        return None;
    }
    Some(CopyResult {
        operation: request.operation,
        purpose: request.purpose,
        fact: request.fact,
        status: copy(&request.value),
    })
}

#[derive(Debug, PartialEq, Eq)]
enum Route {
    System,
    Terminal,
}

fn route(ssh: bool, term_session_id: Option<&str>) -> Route {
    if ssh
        || term_session_id.is_some_and(|id| {
            id.strip_prefix("tailcat-")
                .is_some_and(|name| !name.is_empty())
        })
    {
        Route::Terminal
    } else {
        Route::System
    }
}

pub(super) fn remote_terminal() -> bool {
    let ssh = std::env::var_os("SSH_CONNECTION").is_some()
        || std::env::var_os("SSH_TTY").is_some()
        || std::env::var_os("SSH_CLIENT").is_some();
    let term_session_id = std::env::var_os("TERM_SESSION_ID");
    let term_session_id = term_session_id
        .as_ref()
        .map(|value| value.to_string_lossy());
    route(ssh, term_session_id.as_deref()) == Route::Terminal
}

fn copy(value: &str) -> Copied {
    // token/代理 URL 均远小于此上限；界面不能让恶意大载荷阻塞外部剪贴板进程。
    if value.len() > MAX_BYTES {
        return Copied::Failed("剪贴板内容超过 32 KiB".into());
    }
    // SSH 中的系统工具属于远端主机；应把请求发到用户所见终端。
    if remote_terminal() {
        return Copied::Terminal(osc52(value));
    }
    #[cfg(target_os = "macos")]
    let candidates: &[(&str, &[&str])] = &[("pbcopy", &[])];
    #[cfg(not(target_os = "macos"))]
    let candidates: &[(&str, &[&str])] = &[
        ("wl-copy", &[]),
        ("xclip", &["-selection", "clipboard"]),
        ("xsel", &["--clipboard", "--input"]),
    ];
    for (name, args) in candidates {
        if copy_command(name, args, value).unwrap_or(false) {
            return Copied::System;
        }
    }
    Copied::Terminal(osc52(value))
}

fn osc52(value: &str) -> String {
    format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(value)
    )
}

fn copy_command(name: &str, args: &[&str], value: &str) -> std::io::Result<bool> {
    // main 的 SIGPIPE=SIG_DFL 是 CLI 全局契约。不可向提前退出的 helper pipe 写入，
    // 否则会直接杀 TUI 且绕过 RAII。使用立即 unlink 的0600文件作 stdin，无残留秘密。
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(std::io::Error::other)?;
    let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
    let path = std::env::temp_dir().join(format!("homeway-clipboard-{suffix}"));
    let mut stdin = std::fs::OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)?;
    std::fs::remove_file(&path)?;
    stdin.write_all(value.as_bytes())?;
    stdin.rewind()?;
    let mut child = Command::new(name)
        .args(args)
        .stdin(Stdio::from(stdin))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let success = loop {
        if let Some(status) = child.try_wait()? {
            break status.success();
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    Ok(success)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn application_write_validates_exact_utf8_payload_and_ignores_reads() {
        use homeway_core::term::codec::{clip_kind, enc_clipboard};
        for value in ["", "中e\u{301}\n\0\x1b", &"x".repeat(MAX_BYTES)] {
            let payload = enc_clipboard(clip_kind::WRITE, value.as_bytes());
            assert_eq!(application_write(&payload).unwrap(), Some(value));
        }
        for payload in [
            vec![],
            vec![0],
            vec![0, 0],
            vec![0, 2, 0, b'x'],
            vec![0, 0, 0, b'x'],
            vec![0, 1, 0, 0xff],
            enc_clipboard(clip_kind::WRITE, &vec![b'x'; MAX_BYTES + 1]),
        ] {
            assert!(application_write(&payload).is_err());
        }
        for kind in [clip_kind::READ_REQ, clip_kind::READ_ANSWER, 255] {
            assert_eq!(application_write(&[kind]).unwrap(), None);
        }
    }

    #[test]
    fn cancelled_application_write_is_skipped_before_helper_work() {
        let (requests, queued) = mpsc::sync_channel(4);
        let (_, results) = mpsc::sync_channel(4);
        let clipboard = Clipboard { requests, results };
        // Oversized data prevents any real helper side effect even if this regresses.
        let pending = clipboard
            .copy_application("x".repeat(MAX_BYTES + 1), 7)
            .unwrap();
        let request = queued.recv().unwrap();
        assert!(request.active());
        drop(pending);
        assert!(!request.active());
        assert!(copy_request(request).is_none());
    }

    #[test]
    fn osc52_encodes_untrusted_text_without_control_injection() {
        assert_eq!(osc52("a\x1bb"), "\x1b]52;c;YRti\x07");
    }
    #[test]
    fn command_success_requires_exit_and_stdin_delivery() {
        assert!(copy_command("/bin/sh", &["-c", "cat >/dev/null"], "private-test-value").unwrap());
        assert!(!copy_command("/bin/sh", &["-c", "exit 1"], "private-test-value").unwrap());
    }

    #[test]
    fn remote_markers_route_to_the_visible_terminal_without_system_helpers() {
        for (ssh, session, expected) in [
            (false, None, Route::System),
            (false, Some("local-terminal-id"), Route::System),
            (false, Some("tailcat-"), Route::System),
            (false, Some("tailcat-shell-1"), Route::Terminal),
            (false, Some("tailcat-中文会话"), Route::Terminal),
            (true, None, Route::Terminal),
            (true, Some("local-terminal-id"), Route::Terminal),
        ] {
            assert_eq!(route(ssh, session), expected);
        }
    }

    #[test]
    fn oversized_text_is_rejected_before_any_clipboard_side_effect() {
        assert!(matches!(
            copy(&"x".repeat(MAX_BYTES + 1)),
            Copied::Failed(_)
        ));
    }
}
