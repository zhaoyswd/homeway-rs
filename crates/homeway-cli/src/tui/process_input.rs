//! 独立Web TUI进程的输入worker：隔离crossterm在PTY EOF中的不可取消读取。
//! 普通TUI仍在原主线程读输入。该worker仅存在于CLI进程内，不作为库后台服务。
use std::io;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread::JoinHandle;
use std::time::Duration;

use crossterm::event::{self, Event};

pub(super) struct ProcessInput {
    events: Option<mpsc::Receiver<io::Result<Event>>>,
    stop: Arc<AtomicBool>,
    finished: mpsc::Receiver<()>,
    worker: Option<JoinHandle<()>>,
}

impl ProcessInput {
    pub fn start() -> io::Result<Self> {
        Self::spawn(|| {
            if event::poll(Duration::from_millis(25))? {
                event::read().map(Some)
            } else {
                Ok(None)
            }
        })
    }

    fn spawn(
        mut read: impl FnMut() -> io::Result<Option<Event>> + Send + 'static,
    ) -> io::Result<Self> {
        let (tx, events) = mpsc::sync_channel(4);
        let (done_tx, finished) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let worker = std::thread::Builder::new()
            .name("homeway-web-input".into())
            .spawn(move || {
                struct Finished(mpsc::Sender<()>);
                impl Drop for Finished {
                    fn drop(&mut self) {
                        let _ = self.0.send(());
                    }
                }
                let _finished = Finished(done_tx);
                while !stopping.load(Ordering::Acquire) {
                    let item = match read() {
                        Ok(Some(event)) => Ok(event),
                        Ok(None) => continue,
                        Err(error) => Err(error),
                    };
                    let failed = item.is_err();
                    // 有界且顺序、不丢键鼠/Paste；ProcessInput Drop先关receiver唤醒send。
                    if tx.send(item).is_err() || failed {
                        break;
                    }
                }
            })?;
        Ok(Self {
            events: Some(events),
            stop,
            finished,
            worker: Some(worker),
        })
    }

    pub fn recv(&self, budget: Duration) -> io::Result<Option<Event>> {
        match self
            .events
            .as_ref()
            .expect("输入源仍在作用域内")
            .recv_timeout(budget)
        {
            Ok(event) => event.map(Some),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "Web TUI输入已关闭",
            )),
        }
    }
}

impl Drop for ProcessInput {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        drop(self.events.take());
        if let Some(worker) = self.worker.take() {
            if self
                .finished
                .recv_timeout(Duration::from_millis(75))
                .is_ok()
            {
                let _ = worker.join();
            }
            // crossterm若困在PTY EOF内部，不能在UI线程无限join。
            // 主路径仍析构TerminalGuard并结束此独立CLI进程；OS随即终止其输入线程。
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn cancellation_unblocks_a_full_ordered_input_queue() {
        let input = ProcessInput::spawn(|| Ok(Some(Event::FocusGained))).unwrap();
        assert!(matches!(
            input.recv(Duration::from_secs(1)).unwrap(),
            Some(Event::FocusGained)
        ));
        let started = Instant::now();
        drop(input);
        assert!(started.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn stuck_input_reader_cannot_block_control_checks_or_main_scope_exit() {
        let (release, blocked) = mpsc::channel();
        let (done, completed) = mpsc::channel();
        let input = ProcessInput::spawn(move || {
            let _ = blocked.recv();
            let _ = done.send(());
            Ok(None)
        })
        .unwrap();
        let started = Instant::now();
        assert!(input.recv(Duration::from_millis(25)).unwrap().is_none());
        drop(input);
        assert!(started.elapsed() < Duration::from_millis(500));
        // 单元测试进程不退出，故主动释放模拟reader，不在测试宿主留下后台线程。
        release.send(()).unwrap();
        completed.recv_timeout(Duration::from_secs(1)).unwrap();
    }
}
