//! 剪贴板写入在后台完成；OSC52 仅报告“已请求”，不冒充系统确认。
use std::io::{Seek, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use base64::Engine as _;

pub(super) enum Copied {
    System,
    Terminal(String),
    Failed(String),
}

pub(super) struct CopyResult {
    pub operation: u64,
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
    requests: SyncSender<(String, u64, String)>,
    results: Receiver<CopyResult>,
}

impl Clipboard {
    pub(super) fn start() -> Self {
        let (requests, rx) = mpsc::sync_channel::<(String, u64, String)>(4);
        let (tx, results) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            while let Ok((value, operation, fact)) = rx.recv() {
                let result = CopyResult {
                    operation,
                    fact,
                    status: copy(&value),
                };
                if tx.send(result).is_err() {
                    break;
                }
            }
        });
        Self { requests, results }
    }

    pub(super) fn copy(&self, value: String, operation: u64, fact: String) -> Result<(), Error> {
        self.requests
            .try_send((value, operation, fact))
            .map_err(|e| match e {
                TrySendError::Full(_) => Error::Busy,
                TrySendError::Disconnected(_) => Error::Closed,
            })
    }

    pub(super) fn try_result(&self) -> Option<CopyResult> {
        self.results.try_recv().ok()
    }
}

fn copy(value: &str) -> Copied {
    // token/代理 URL 均远小于此上限；界面不能让恶意大载荷阻塞外部剪贴板进程。
    if value.len() > 32 * 1024 {
        return Copied::Failed("剪贴板内容超过 32 KiB".into());
    }
    // SSH 中的系统工具属于远端主机；应把请求发到用户所见终端。
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
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
    fn osc52_encodes_untrusted_text_without_control_injection() {
        assert_eq!(osc52("a\x1bb"), "\x1b]52;c;YRti\x07");
    }
    #[test]
    fn command_success_requires_exit_and_stdin_delivery() {
        assert!(copy_command("/bin/sh", &["-c", "cat >/dev/null"], "private-test-value").unwrap());
        assert!(!copy_command("/bin/sh", &["-c", "exit 1"], "private-test-value").unwrap());
    }
}
