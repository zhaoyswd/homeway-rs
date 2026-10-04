//! wire — term 连接的帧 I/O（R6 6f-3b）：poll(2) 驱动的非阻塞读写 + 断尾续写。
//!
//! 行为真源 = baseline 克隆 `pkg/term/term_leg.go` 的 `writeFrameOnce`（进度感知续写，
//! 2026-09-30 linux CI「意外帧 op 0x6e」根因修复的同一语义）：
//!
//! - **写超时 ≠ 失败**：deadline 到点时帧可能已有前半进内核。断尾（torn/torn_whole）
//!   挂在本连接上**跨调用续完**——重试同一帧（与断尾原帧逐字节相等 ⇒ 续完 = 整帧已
//!   交付）与换帧（先续旧尾、再写新帧，wire 上没有「跳过半帧」的选项）都归一到先续尾；
//! - 部分进展（写出了 n < len）立刻续写剩余，deadline 顺延；
//! - 零进展超时 / 硬错误上抛给调用方的停滞判定（raw 腿退避重试）或断腿。
//!
//! 实现面：std 的 `Write` 在超时错误里**不带回写字节数**（Go `(n, err)` 有），
//! 断尾进度只能靠自管——置非阻塞 + poll(2)（仓 AGENTS「自管线程 + poll(2) 不引 tokio」
//! 的既定路线），每次 `write(2)` 的返回值即真实进度。

use std::io;
use std::io::Read as _;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::time::Duration;
use std::time::Instant;

use super::frames::Frame;
use super::frames::Op;

/// 写路径的失败分类（raw 腿的停滞判定输入：Timeout 退避重试、Hard 断腿）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteFail {
    /// deadline 到点（含部分进展后的那次返回）——停滞语义。
    Timeout,
    /// 对端已不可达（EPIPE/ECONNRESET/EBADF…）。
    Hard(io::ErrorKind),
}

impl std::fmt::Display for WriteFail {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteFail::Timeout => f.write_str("write timeout"),
            WriteFail::Hard(k) => write!(f, "write hard error: {k}"),
        }
    }
}

/// 读路径的失败分类（读循环退出 / HELLO 超时面）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ReadFail {
    #[error("read timeout")]
    Timeout,
    #[error("read hard error: {0}")]
    Hard(#[from] io::Error),
    #[error("term: bad frame: {0}")]
    BadFrame(String),
}

/// 一条 term 连接的帧 I/O 面（本连接独占；读、写可各归一个线程——内核 socket
/// 全双工，读侧 poll POLLIN、写侧 poll POLLOUT 互不干扰）。
pub struct FrameIo {
    stream: UnixStream,
    /// 断尾：帧的前半已进内核、剩余待续（只归**写者**读写）。
    torn: Vec<u8>,
    /// 断尾所属的整帧（「调用方重试的是否同一帧」的逐字节判等）。
    torn_whole: Vec<u8>,
}

impl FrameIo {
    pub fn new(stream: UnixStream) -> Self {
        // 非阻塞 + poll 是断尾进度的前提（见模块头）；读写两半都走 poll。
        let _ = stream.set_nonblocking(true);
        FrameIo { stream, torn: Vec::new(), torn_whole: Vec::new() }
    }

    /// 拿到底层流（收尾时关连接用；poll 面不收）。
    pub fn into_stream(self) -> UnixStream {
        self.stream
    }

    /// 克隆底层流（读/写两半分归两个线程的形态——UDS 克隆共享同一 socket）。
    pub fn try_clone_stream(&self) -> std::io::Result<UnixStream> {
        self.stream.try_clone()
    }

    /// 是否有未续完的断尾（观测/测试面）。
    pub fn has_torn(&self) -> bool {
        !self.torn.is_empty()
    }

    // ---- 写 ----

    /// 写一帧（Go `writeFrameOnce` 同义）。`Ok(())` = 整帧已交付（含断尾续完）；
    /// `Err` = 超时（断尾保留、下次续写）或硬错误。
    pub fn write_frame(&mut self, op: Op, payload: &[u8], timeout: Duration) -> Result<(), WriteFail> {
        let enc = super::frames::encode_frame(op, payload);
        let same_frame = !self.torn.is_empty() && self.torn_whole == enc;
        while !self.torn.is_empty() {
            let deadline = Instant::now() + timeout;
            match poll_write(&self.stream, &self.torn, deadline)? {
                PollWrite::Done => {
                    self.torn.clear();
                    self.torn_whole.clear();
                }
                PollWrite::Partial(rem) => {
                    // 部分进展：立刻续写剩余（deadline 已按本帧重置）
                    let used = self.torn.len() - rem.len();
                    self.torn.drain(..used);
                }
            }
        }
        if same_frame {
            return Ok(()); // 调用方重试的就是断尾所属帧：续完 = 整帧已交付
        }
        let deadline = Instant::now() + timeout;
        match poll_write(&self.stream, &enc, deadline)? {
            PollWrite::Done => Ok(()),
            PollWrite::Partial(rem) => {
                // 断尾（零进展时 = 整帧，下次仍从这里续）
                let n = enc.len() - rem.len();
                self.torn = enc[n..].to_vec();
                self.torn_whole = enc;
                Err(WriteFail::Timeout)
            }
        }
    }

    // ---- 读 ----

    /// 阻塞读一帧（deadline 到点报 [`ReadFail::Timeout`]）。
    pub fn read_frame_deadline(&mut self, timeout: Duration) -> Result<Frame, ReadFail> {
        let deadline = Instant::now() + timeout;
        let mut hdr = [0u8; 3];
        self.read_exact_deadline(&mut hdr, deadline)?;
        let n = u16::from_le_bytes([hdr[1], hdr[2]]) as usize;
        let mut payload = vec![0u8; n];
        if n > 0 {
            self.read_exact_deadline(&mut payload, deadline)?;
        }
        Ok(Frame { op: Op(hdr[0]), payload })
    }

    /// 无限阻塞读一帧（读循环形态；EOF/硬错误上抛）。
    pub fn read_frame(&mut self) -> Result<Frame, ReadFail> {
        // 上限兜一个很大的 deadline（等价无限；避免 Instant 溢出面）
        self.read_frame_deadline(Duration::from_secs(86400 * 365))
    }

    fn read_exact_deadline(&mut self, buf: &mut [u8], deadline: Instant) -> Result<(), ReadFail> {
        let mut off = 0usize;
        while off < buf.len() {
            if poll_read(&self.stream, deadline)? == 0 {
                return Err(ReadFail::Timeout); // deadline 到点
            }
            match self.stream.read(&mut buf[off..]) {
                Ok(0) => return Err(ReadFail::Hard(io::ErrorKind::UnexpectedEof.into())),
                Ok(n) => off += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue, // poll 竞态：再来
                Err(e) => return Err(ReadFail::Hard(e)),
            }
        }
        Ok(())
    }
}

enum PollWrite<'a> {
    /// 全部写出。
    Done,
    /// 部分写出（返回**剩余未写**切片）。
    Partial(&'a [u8]),
}

/// poll(2) POLLOUT + write(2) 循环（deadline 到点且仍有剩余 ⇒ Partial）。
fn poll_write<'a>(stream: &UnixStream, mut buf: &'a [u8], deadline: Instant) -> Result<PollWrite<'a>, WriteFail> {
    let fd = stream.as_raw_fd();
    while !buf.is_empty() {
        let now = Instant::now();
        if now >= deadline {
            return Ok(PollWrite::Partial(buf));
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLOUT, revents: 0 };
        let ms = (deadline - now).as_millis().min(u32::MAX as u128) as i32;
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(WriteFail::Hard(err.kind()));
        }
        if r == 0 {
            return Ok(PollWrite::Partial(buf)); // 超时（零进展或进展后）
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            // POLLHUP 仍可能可写（对端半关）——交给 write(2) 判定真伪
            if pfd.revents & libc::POLLNVAL != 0 {
                return Err(WriteFail::Hard(io::ErrorKind::NotConnected));
            }
        }
        match unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) } {
            n if n > 0 => buf = &buf[n as usize..],
            0 => return Err(WriteFail::Hard(io::ErrorKind::WriteZero)),
            _ => {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::WouldBlock {
                    continue; // 竞态：重 poll
                }
                return Err(WriteFail::Hard(err.kind()));
            }
        }
    }
    Ok(PollWrite::Done)
}

/// poll(2) POLLIN 就绪等待（返回 0 = deadline 到点；>0 = 可读）。
fn poll_read(stream: &UnixStream, deadline: Instant) -> Result<i32, ReadFail> {
    let fd = stream.as_raw_fd();
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Ok(0);
        }
        let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
        let ms = (deadline - now).as_millis().min(u32::MAX as u128) as i32;
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err.into());
        }
        return Ok(r);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (FrameIo, FrameIo) {
        let (a, b) = UnixStream::pair().unwrap();
        (FrameIo::new(a), FrameIo::new(b))
    }

    /// 正常往返 + 大帧（分多次 poll 写完；对端并发排空——UDS 缓冲吞不下 64KiB 帧，
    /// 写读必须重叠）。
    #[test]
    fn frame_roundtrip() {
        let (io_a, mut io_b) = pair();
        let payload = vec![0xabu8; 70000]; // 超 MAX_PAYLOAD → 截断为 65535
        let writer = std::thread::spawn(move || {
            let mut io_a = io_a;
            io_a.write_frame(Op::DATA, &payload, Duration::from_secs(5)).unwrap();
            // 双向：等对端回一帧再收尾（写半与读半分线程的形态验证）
            io_a
        });
        let f = io_b.read_frame_deadline(Duration::from_secs(5)).unwrap();
        assert_eq!(f.op, Op::DATA);
        assert_eq!(f.payload.len(), super::super::frames::MAX_PAYLOAD);
        let mut io_a = writer.join().unwrap();
        io_b.write_frame(Op::OK, &[], Duration::from_secs(2)).unwrap();
        let f = io_a.read_frame_deadline(Duration::from_secs(2)).unwrap();
        assert_eq!((f.op, f.payload.len()), (Op::OK, 0));
    }

    /// 读超时（对端不写）。
    #[test]
    fn read_timeout_fires() {
        let (mut io_a, _io_b) = pair();
        let r = io_a.read_frame_deadline(Duration::from_millis(50));
        assert!(matches!(r, Err(ReadFail::Timeout)));
    }

    /// 停滞写超时：对端不读、灌满发送缓冲后 write_frame 报 Timeout 且断尾保留。
    /// （UDS 缓冲较大；本机快路径可能全部写完——那也必须 Done 而不是误报。）
    #[test]
    fn write_timeout_keeps_torn_tail() {
        let (mut io_a, mut io_b) = pair();
        let big = vec![7u8; 2 << 20];
        let mut timed_out = false;
        for _ in 0..16 {
            match io_a.write_frame(Op::DATA, &big, Duration::from_millis(120)) {
                Ok(()) => continue,      // 缓冲还在吞：继续灌
                Err(WriteFail::Timeout) => {
                    timed_out = true;
                    break;
                }
                Err(e) => panic!("意外错误 {e}"),
            }
        }
        if !timed_out {
            return; // 本机缓冲吞下了全部帧（快路径）——无断尾面可测，跳过
        }
        assert!(io_a.has_torn(), "部分进展超时必须留断尾");
        // 对端开始排空 + 重试同帧（同载荷）⇒ 续完 = Ok，且对端逐帧可读（帧界不撕裂）
        let drain = std::thread::spawn(move || {
            let mut count = 0usize;
            while let Ok(f) = io_b.read_frame_deadline(Duration::from_secs(10)) {
                assert!(f.payload.len() <= super::super::frames::MAX_PAYLOAD);
                count += 3 + f.payload.len();
                if count > (2 << 20) * 2 {
                    break;
                }
            }
        });
        let mut retries = 0;
        loop {
            match io_a.write_frame(Op::DATA, &big, Duration::from_secs(5)) {
                Ok(()) => break,
                Err(WriteFail::Timeout) => {
                    retries += 1;
                    assert!(retries < 200, "续写不应永远超时");
                }
                Err(e) => panic!("{e}"),
            }
        }
        assert!(!io_a.has_torn(), "整帧交付后断尾清空");
        let _ = drain.join();
    }
}
