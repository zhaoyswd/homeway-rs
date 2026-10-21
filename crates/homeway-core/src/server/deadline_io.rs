use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// rustls 会在一次 complete_io 内部多次读写；期限必须放在 TCP 这一层。
/// 短 timeout 只用于轮询取消；每次重试仍受同一个绝对期限约束。
pub(crate) struct DeadlineStream<'a> {
    tcp: TcpStream,
    deadline: Instant,
    stop: &'a AtomicBool,
}

impl<'a> DeadlineStream<'a> {
    pub(crate) fn new(tcp: TcpStream, deadline: Instant, stop: &'a AtomicBool) -> Self {
        Self {
            tcp,
            deadline,
            stop,
        }
    }

    fn remaining(&self) -> std::io::Result<Duration> {
        if self.stop.load(Ordering::Acquire) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "网络观察查询已停止",
            ));
        }
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "网络观察查询总预算耗尽",
            ));
        }
        Ok(left.min(Duration::from_millis(50)))
    }

    fn io<T>(
        &mut self,
        read: bool,
        mut op: impl FnMut(&mut TcpStream) -> std::io::Result<T>,
    ) -> std::io::Result<T> {
        loop {
            let slice = self.remaining()?;
            if read {
                self.tcp.set_read_timeout(Some(slice))?;
            } else {
                self.tcp.set_write_timeout(Some(slice))?;
            }
            let result = op(&mut self.tcp);
            self.remaining()?;
            match result {
                Err(ref e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
                    ) =>
                {
                    continue
                }
                other => return other,
            }
        }
    }
}

impl Read for DeadlineStream<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.io(true, |tcp| tcp.read(buf))
    }
}

impl Write for DeadlineStream<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.io(false, |tcp| tcp.write(buf))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.io(false, Write::flush)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::Arc;

    fn pair() -> (TcpStream, TcpStream) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let client = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (server, _) = listener.accept().unwrap();
        (client, server)
    }

    #[test]
    fn pending_read_obeys_deadline_and_cancellation() {
        for cancel in [false, true] {
            let (client, _server) = pair();
            let stop = Arc::new(AtomicBool::new(false));
            let stopping = Arc::clone(&stop);
            let worker = std::thread::spawn(move || {
                if cancel {
                    std::thread::sleep(Duration::from_millis(20));
                    stopping.store(true, Ordering::Release);
                }
            });
            let start = Instant::now();
            // 取消分支验证取消本身，避免线程调度与 90ms 期限竞争；
            // 下方 350ms 断言仍约束取消速度，非取消分支仍验证 90ms 总预算。
            let budget = if cancel {
                Duration::from_secs(5)
            } else {
                Duration::from_millis(90)
            };
            let mut io = DeadlineStream::new(client, start + budget, &stop);
            let error = io.read(&mut [0]).unwrap_err();
            assert_eq!(
                error.kind(),
                if cancel {
                    std::io::ErrorKind::ConnectionAborted
                } else {
                    std::io::ErrorKind::TimedOut
                }
            );
            assert!(start.elapsed() < Duration::from_millis(350));
            worker.join().unwrap();
        }
    }

    #[test]
    fn stopped_write_all_does_not_retry_or_emit_bytes() {
        let (client, mut server) = pair();
        let stop = AtomicBool::new(true);
        let mut io = DeadlineStream::new(client, Instant::now() + Duration::from_secs(1), &stop);
        assert_eq!(
            io.write_all(b"must not escape").unwrap_err().kind(),
            std::io::ErrorKind::ConnectionAborted
        );
        server.set_nonblocking(true).unwrap();
        assert_eq!(
            server.read(&mut [0]).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }
}
