//! Blocking 通知在单独的有界 worker 中提交；终端请求只由 UI 线程输出。
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::clipboard;

// 本前端通知的预算真源：超长名称不能制造无界队列或控制序列。
const QUEUE_CAPACITY: usize = 16;
const TITLE_BYTES: usize = 128;
const BODY_BYTES: usize = 512;
const HELPER_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(20);
const APPLESCRIPT: &str = "on run argv\n display notification (item 3 of argv) with title (item 1 of argv) subtitle (item 2 of argv)\nend run";

#[derive(Debug, thiserror::Error)]
pub(super) enum Error {
    #[error("通知队列忙")]
    Busy,
    #[error("通知任务已退出")]
    Closed,
}

pub(super) struct Notifications {
    requests: SyncSender<Request>,
    terminal: Receiver<String>,
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    #[cfg(test)]
    recorded: Option<SyncSender<(String, String)>>,
}

struct Request {
    title: String,
    body: String,
}

impl Request {
    fn new(title: &str, body: &str) -> Self {
        Self {
            title: clean(title, TITLE_BYTES),
            body: clean(body, BODY_BYTES),
        }
    }

    fn terminal(&self) -> String {
        // OSC 9 的 ST 使用 ESC\\，不再额外叠加 BEL 或其它通知协议。
        format!("\x1b]9;homeway · {} — {}\x1b\\", self.title, self.body)
    }
}

impl Notifications {
    pub(super) fn start() -> Self {
        Self::spawn(|request, stopping| {
            deliver(request, clipboard::remote_terminal(), stopping, native)
        })
    }

    pub(super) fn notify(&self, title: &str, body: &str) -> Result<(), Error> {
        let request = Request::new(title, body);
        #[cfg(test)]
        if let Some(recorded) = &self.recorded {
            return recorded
                .try_send((request.title, request.body))
                .map_err(send_error);
        }
        self.requests.try_send(request).map_err(send_error)
    }

    pub(super) fn try_terminal(&self) -> Option<String> {
        self.terminal.try_recv().ok()
    }

    fn spawn<F>(mut submit: F) -> Self
    where
        F: FnMut(Request, &AtomicBool) -> Option<String> + Send + 'static,
    {
        let (requests, receiver) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (terminal_tx, terminal) = mpsc::sync_channel(QUEUE_CAPACITY);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = std::thread::spawn(move || {
            while !worker_stopping.load(Ordering::Acquire) {
                let request = match receiver.recv_timeout(POLL_INTERVAL) {
                    Ok(request) => request,
                    Err(RecvTimeoutError::Timeout) => continue,
                    Err(RecvTimeoutError::Disconnected) => break,
                };
                if worker_stopping.load(Ordering::Acquire) {
                    break;
                }
                if let Some(sequence) = submit(request, &worker_stopping) {
                    if !send_terminal(&terminal_tx, sequence, &worker_stopping) {
                        break;
                    }
                }
            }
        });
        Self {
            requests,
            terminal,
            stopping,
            worker: Some(worker),
            #[cfg(test)]
            recorded: None,
        }
    }

    #[cfg(test)]
    pub(super) fn recording() -> (Self, Receiver<(String, String)>) {
        let (tx, recorded) = mpsc::sync_channel(QUEUE_CAPACITY);
        let (requests, _receiver) = mpsc::sync_channel(1);
        let (_terminal_tx, terminal) = mpsc::sync_channel(1);
        let notifications = Self {
            requests,
            terminal,
            stopping: Arc::new(AtomicBool::new(false)),
            worker: None,
            recorded: Some(tx),
        };
        (notifications, recorded)
    }
}

fn send_error<T>(error: TrySendError<T>) -> Error {
    match error {
        TrySendError::Full(_) => Error::Busy,
        TrySendError::Disconnected(_) => Error::Closed,
    }
}

impl Drop for Notifications {
    fn drop(&mut self) {
        self.stopping.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            // Worker 的 recv、helper 和结果背压均每20ms检查取消，不排空旧请求。
            let _ = worker.join();
        }
    }
}

pub(super) fn terminal_request(title: &str, body: &str) -> String {
    Request::new(title, body).terminal()
}

fn clean(value: &str, max_bytes: usize) -> String {
    let mut output = String::with_capacity(value.len().min(max_bytes));
    for character in value.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if output.len() + character.len_utf8() > max_bytes {
            break;
        }
        output.push(character);
    }
    output
}

fn send_terminal(sender: &SyncSender<String>, mut sequence: String, stopping: &AtomicBool) -> bool {
    while !stopping.load(Ordering::Acquire) {
        match sender.try_send(sequence) {
            Ok(()) => return true,
            Err(TrySendError::Full(returned)) => sequence = returned,
            Err(TrySendError::Disconnected(_)) => return false,
        }
        std::thread::sleep(POLL_INTERVAL);
    }
    false
}

fn deliver<F>(request: Request, remote: bool, stopping: &AtomicBool, native: F) -> Option<String>
where
    F: FnOnce(&Request, &AtomicBool) -> bool,
{
    if stopping.load(Ordering::Acquire) {
        return None;
    }
    if remote || !native(&request, stopping) {
        return (!stopping.load(Ordering::Acquire)).then(|| request.terminal());
    }
    None
}

#[derive(Clone, Copy)]
enum Platform {
    Mac,
    Unix,
}

fn helper(request: &Request, platform: Platform) -> Command {
    match platform {
        Platform::Mac => {
            let mut command = Command::new("osascript");
            // 固定首个非option实参；其余用户文本仅作为 run argv，永不拼入脚本。
            command.args(["-e", APPLESCRIPT, "homeway", &request.title, &request.body]);
            command
        }
        Platform::Unix => {
            let mut command = Command::new("notify-send");
            command.args(["--app-name=homeway", "--", &request.title, &request.body]);
            command
        }
    }
}

fn native(request: &Request, stopping: &AtomicBool) -> bool {
    if stopping.load(Ordering::Acquire) {
        return false;
    }
    let platform = if cfg!(target_os = "macos") {
        Platform::Mac
    } else {
        Platform::Unix
    };
    let mut command = helper(request, platform);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let Ok(mut child) = command.spawn() else {
        return false;
    };
    wait_child(&mut child, stopping, Instant::now() + HELPER_TIMEOUT).unwrap_or(false)
}

fn wait_child(
    child: &mut Child,
    stopping: &AtomicBool,
    deadline: Instant,
) -> std::io::Result<bool> {
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status.success()),
            Ok(None) => {}
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        if stopping.load(Ordering::Acquire) || Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(false);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_route_never_calls_native_and_fallback_is_one_safe_osc() {
        let stopping = AtomicBool::new(false);
        let expected = "\x1b]9;homeway · 需要操作 — 主机 · 会话\x1b\\";
        assert_eq!(
            deliver(
                Request::new("需要操作", "主机 · 会话"),
                true,
                &stopping,
                |_, _| { panic!("remote notifications must not use the remote desktop") }
            ),
            Some(expected.into())
        );
        assert_eq!(
            deliver(
                Request::new("需要操作", "主机 · 会话"),
                false,
                &stopping,
                |_, _| false
            ),
            Some(expected.into())
        );
        assert!(deliver(
            Request::new("需要操作", "主机"),
            false,
            &stopping,
            |_, _| true
        )
        .is_none());
    }

    #[test]
    fn notification_payload_is_bounded_and_cannot_inject_another_control_sequence() {
        let request = Request::new("a\x1b]9;fake\x07\u{009c}b", &"中\n".repeat(1_000));
        assert!(request.title.len() <= TITLE_BYTES && request.body.len() <= BODY_BYTES);
        assert!(!request.title.chars().any(char::is_control));
        assert!(!request.body.chars().any(char::is_control));
        let sequence = request.terminal();
        assert_eq!(sequence.matches('\x1b').count(), 2);
        assert_eq!(sequence.matches('\x07').count(), 0);
        assert!(sequence.starts_with("\x1b]9;") && sequence.ends_with("\x1b\\"));
    }

    #[test]
    fn native_arguments_keep_untrusted_text_outside_scripts_and_options() {
        let request = Request::new("-e do shell script \"bad\"", "--evil $(command)\n会话");
        let mac = helper(&request, Platform::Mac);
        let mac_args: Vec<_> = mac
            .get_args()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(mac.get_program(), "osascript");
        assert_eq!(
            mac_args,
            ["-e", APPLESCRIPT, "homeway", &request.title, &request.body]
        );
        assert!(!APPLESCRIPT.contains("do shell script"));
        let unix = helper(&request, Platform::Unix);
        let unix_args: Vec<_> = unix
            .get_args()
            .map(|value| value.to_str().unwrap())
            .collect();
        assert_eq!(unix.get_program(), "notify-send");
        assert_eq!(
            unix_args,
            ["--app-name=homeway", "--", &request.title, &request.body]
        );
    }

    #[test]
    fn slow_submission_has_nonblocking_backpressure_and_drop_cancels_it() {
        let (started_tx, started) = mpsc::channel();
        let notifications = Notifications::spawn(move |_, stopping| {
            let _ = started_tx.send(());
            while !stopping.load(Ordering::Acquire) {
                std::thread::sleep(POLL_INTERVAL);
            }
            None
        });
        notifications.notify("title", "body").unwrap();
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        let start = Instant::now();
        let mut busy = false;
        for _ in 0..100 {
            if matches!(notifications.notify("title", "body"), Err(Error::Busy)) {
                busy = true;
                break;
            }
        }
        assert!(busy);
        drop(notifications);
        assert!(start.elapsed() < HELPER_TIMEOUT);
    }

    #[test]
    fn timed_out_or_cancelled_helper_is_killed_and_reaped() {
        for cancelled in [false, true] {
            let stopping = AtomicBool::new(cancelled);
            let mut child = Command::new("/bin/sleep").arg("10").spawn().unwrap();
            let start = Instant::now();
            let deadline = if cancelled {
                start + HELPER_TIMEOUT
            } else {
                start
            };
            assert!(!wait_child(&mut child, &stopping, deadline).unwrap());
            assert!(child.try_wait().unwrap().is_some());
            assert!(start.elapsed() < HELPER_TIMEOUT);
        }
    }

    #[test]
    fn cancelled_delivery_does_not_submit_or_emit_a_fallback() {
        let stopping = AtomicBool::new(true);
        assert!(
            deliver(Request::new("title", "body"), false, &stopping, |_, _| {
                panic!("cancelled notification must not launch a helper")
            })
            .is_none()
        );
        let (sender, _receiver) = mpsc::sync_channel(1);
        assert!(!send_terminal(&sender, "not sent".into(), &stopping));

        let stopping = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send("already queued".into()).unwrap();
        let (done, completed) = mpsc::channel();
        let worker_stopping = Arc::clone(&stopping);
        let worker = std::thread::spawn(move || {
            let _ = done.send(send_terminal(&sender, "not sent".into(), &worker_stopping));
        });
        assert!(completed.recv_timeout(POLL_INTERVAL * 2).is_err());
        stopping.store(true, Ordering::Release);
        assert!(!completed.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
        assert_eq!(receiver.try_recv().unwrap(), "already queued");
        assert!(receiver.try_recv().is_err());
    }
}
