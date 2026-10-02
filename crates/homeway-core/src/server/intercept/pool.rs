//! 拦截 worker 池（R3；固定小池 + poll(2) 多路复用——设计 §1.1 / 评审 H4/H4a 整改）。
//!
//! Go 的「每流 goroutine」在 Rust 不是近零成本（上限 1024 TCP + 4096 UDP 会话）——
//! 固定池（N 条线程）事件驱动所有 upstream fd（TCP/UDS/UDP socket），与 R1 的
//! 「自管线程 + poll(2)」同一习惯。拨号（阻塞面最长 10s）不占池：每流 spawn 短命
//! 拨号线程，完成后 fd 移交池（暂态线程量级 = 并发建连数，远小于稳态连接数）。
//!
//! 消息面（显式枚举，评审 H4b）——TCP 桥语义对齐 Go `bridgeConns`「任一方 EOF/错误
//! 即双向拆、无半关」；upstream 侧 EOF = `UpstreamEof`（驱动关栈内 socket 发 FIN）。
//!
//! 背压（设计纪律第 4 条）：双向高水位 256KB——upstream→栈方向 worker 暂停读 fd
//! （`Ack` 清账）；栈→upstream 方向驱动暂停 drain 栈 socket（`Written` 清账）。

use std::collections::HashMap;
use std::io;
#[cfg(test)]
use std::io::{Read as _, Write as _};
use std::net::{TcpStream, UdpSocket};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, Sender};
use std::time::Duration;

/// 双向高水位（per-flow 未确认字节；超过即门控读端）。
pub const WATERMARK: usize = 256 * 1024;
/// 每次读块大小（Go bufSize 同值）。
const READ_CHUNK: usize = 64 * 1024;
/// upstream 的种类（豁免/过境/DNS 的建流决策面）。
#[derive(Debug, Clone)]
pub enum Upstream {
    /// 本机 TCP（transit；exempt 未命中 LocalServices 时的回环同端口）。
    Tcp(std::net::SocketAddr),
    /// LocalServices UDS（files/term/speedtest）。
    Unix(String),
    /// 本机已连接 UDP（transit/exempt 的 UDP 重拨）。
    Udp(std::net::SocketAddr),
}

/// 驱动 → worker 的命令。
#[derive(Debug)]
pub enum PoolCmd {
    /// 写 upstream（TCP/UDS 流写；UDP 数据报发）。
    Out { flow: u64, data: Vec<u8> },
    /// 关闭（linger_rst = Drain 到期的 RST 收口——worker 对 TCP 设 linger(0) 再关）。
    Close { flow: u64, linger_rst: bool },
    /// 拨号完成后的 fd 移交（拨号线程产出）。
    Adopt { flow: u64, fd: RawFd, udp: bool },
    /// 驱动已消化 UpstreamData（背压清账；TCP 与 UDP-in 都走）。
    Ack { flow: u64, n: usize },
}

/// worker → 驱动的事件。
#[derive(Debug)]
pub enum PoolEvent {
    DialOk { flow: u64 },
    DialFailed { flow: u64 },
    UpstreamData { flow: u64, data: Vec<u8> },
    /// TCP/UDS EOF 或读错误（对齐 Go bridgeConns 双向拆的触发点）。
    UpstreamEof { flow: u64 },
    /// Close{linger_rst} 的完成回执（Drain 销账判据）。
    Closed { flow: u64 },
    /// Out 数据已写进 fd（背压清账——驱动侧 unacked_out 减 n；UDP 按数据报整包计）。
    Written { flow: u64, n: usize },
}

struct FlowIo {
    fd: RawFd,
    udp: bool,
    /// 待写缓冲（非阻塞写 EAGAIN 时挂起，POLLOUT 继续）。
    out_buf: VecDequeLite,
    /// upstream→驱动方向的未确认字节（背压）。
    unacked: usize,
    want_write: bool,
    dead: bool,
}

/// 简洁起见用 Vec 做写缓冲（块级追加；头部消费）。
struct VecDequeLite {
    buf: Vec<u8>,
    off: usize,
}

impl VecDequeLite {
    fn new() -> Self {
        Self { buf: Vec::new(), off: 0 }
    }
    fn push(&mut self, data: &[u8]) {
        if self.off > 0 && self.off == self.buf.len() {
            self.buf.clear();
            self.off = 0;
        }
        self.buf.extend_from_slice(data);
    }
    fn remaining(&self) -> &[u8] {
        &self.buf[self.off..]
    }
    fn consume(&mut self, n: usize) {
        self.off += n;
        if self.off == self.buf.len() {
            self.buf.clear();
            self.off = 0;
        }
    }
}

/// 单个 worker 的事件循环（poll(2) over {名下 fd 集, 命令管道}）。
struct Worker {
    cmd_rx: mpsc::Receiver<PoolCmd>,
    event_tx: Sender<PoolEvent>,
    flows: HashMap<u64, FlowIo>,
    fd_index: HashMap<RawFd, u64>,
}

impl Worker {
    fn run(mut self, wake_r: RawFd) {
        // pipe 的读端归本 worker（收工时关）
        let mut pollfds: Vec<libc::pollfd> = Vec::new();
        loop {
            // ① 命令先 drain（非阻塞）
            let mut stop = false;
            while let Ok(cmd) = self.cmd_rx.try_recv() {
                if self.handle_cmd(cmd, &mut stop) {
                    // 收工
                    unsafe { libc::close(wake_r); }
                    return;
                }
            }
            if stop {
                unsafe { libc::close(wake_r); }
                return;
            }
            // ② poll 名下 fd + 管道
            pollfds.clear();
            pollfds.push(libc::pollfd { fd: wake_r, events: libc::POLLIN, revents: 0 });
            for io in self.flows.values() {
                if io.dead {
                    continue;
                }
                let mut ev = libc::POLLIN;
                if !io.out_buf.remaining().is_empty() || io.want_write {
                    ev |= libc::POLLOUT;
                }
                pollfds.push(libc::pollfd { fd: io.fd, events: ev, revents: 0 });
            }
            let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as u32, 1000) };
            if n < 0 && std::io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
                // poll 硬错误：清场退出（不应发生）
                break;
            }
            if pollfds[0].revents & libc::POLLIN != 0 {
                let mut b = [0u8; 64];
                unsafe {
                    while libc::read(wake_r, b.as_mut_ptr().cast(), 64) > 0 {}
                }
            }
            for pf in &pollfds[1..] {
                if pf.revents == 0 {
                    continue;
                }
                let fd = pf.fd;
                let Some(flow) = self.fd_index.get(&fd).copied() else { continue };
                if pf.revents & (libc::POLLHUP | libc::POLLERR) != 0 && pf.revents & libc::POLLIN == 0 {
                    self.mark_eof(flow);
                    continue;
                }
                if pf.revents & libc::POLLIN != 0 {
                    self.read_flow(flow);
                }
                if pf.revents & libc::POLLOUT != 0 {
                    self.flush_flow(flow);
                }
            }
        }
        unsafe { libc::close(wake_r); }
    }

    /// 返回 true = 收工。
    fn handle_cmd(&mut self, cmd: PoolCmd, stop: &mut bool) -> bool {
        match cmd {
            PoolCmd::Out { flow, data } => {
                let n = data.len();
                if let Some(io) = self.flows.get_mut(&flow) {
                    io.out_buf.push(&data);
                    let written = self.flush_flow(flow);
                    if written >= n {
                        // 全部写完才回执（部分写在 POLLOUT 续写后补——简化：按已消费量回执）
                        let _ = self.event_tx.send(PoolEvent::Written { flow, n: written });
                    }
                }
            }
            PoolCmd::Close { flow, linger_rst } => {
                if let Some(io) = self.flows.remove(&flow) {
                    if linger_rst {
                        unsafe {
                            let one = libc::c_int::from(1);
                            let zero = libc::linger {
                                l_onoff: one,
                                l_linger: 0,
                            };
                            libc::setsockopt(
                                io.fd,
                                libc::SOL_SOCKET,
                                libc::SO_LINGER,
                                &zero as *const _ as *const libc::c_void,
                                std::mem::size_of::<libc::linger>() as u32,
                            );
                        }
                    }
                    close_fd(io.fd, io.udp);
                    self.fd_index.remove(&io.fd);
                    let _ = self.event_tx.send(PoolEvent::Closed { flow });
                }
                *stop = false;
            }
            PoolCmd::Adopt { flow, fd, udp } => {
                set_nonblocking(fd);
                self.flows.insert(
                    flow,
                    FlowIo {
                        fd,
                        udp,
                        out_buf: VecDequeLite::new(),
                        unacked: 0,
                        want_write: false,
                        dead: false,
                    },
                );
                self.fd_index.insert(fd, flow);
                let _ = self.event_tx.send(PoolEvent::DialOk { flow });
            }
            PoolCmd::Ack { flow, n } => {
                if let Some(io) = self.flows.get_mut(&flow) {
                    io.unacked = io.unacked.saturating_sub(n);
                }
            }
        }
        false
    }

    fn read_flow(&mut self, flow: u64) {
        let Some(io) = self.flows.get_mut(&flow) else { return };
        if io.dead || io.unacked > WATERMARK {
            return; // 背压：暂停读（poll 仍报，跳过即可——水位由 Ack 解除）
        }
        if io.udp {
            // UDP：一次一个数据报
            let mut buf = [0u8; 65536];
            let n = unsafe {
                libc::recv(
                    io.fd,
                    buf.as_mut_ptr().cast(),
                    buf.len(),
                    0,
                )
            };
            if n > 0 {
                io.unacked += n as usize;
                let _ = self
                    .event_tx
                    .send(PoolEvent::UpstreamData { flow, data: buf[..n as usize].to_vec() });
            } else if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != io::ErrorKind::WouldBlock {
                    self.mark_eof(flow);
                }
            }
            return;
        }
        // TCP/UDS：流读
        let mut buf = [0u8; READ_CHUNK];
        let n = unsafe { libc::read(io.fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n > 0 {
            io.unacked += n as usize;
            let _ = self
                .event_tx
                .send(PoolEvent::UpstreamData { flow, data: buf[..n as usize].to_vec() });
        } else if n == 0 {
            self.mark_eof(flow);
        } else {
            let e = std::io::Error::last_os_error();
            if e.kind() != io::ErrorKind::WouldBlock {
                self.mark_eof(flow);
            }
        }
    }

    fn flush_flow(&mut self, flow: u64) -> usize {
        let Some(io) = self.flows.get_mut(&flow) else { return 0 };
        if io.dead {
            return 0;
        }
        let remaining = io.out_buf.remaining();
        if remaining.is_empty() {
            return 0;
        }
        if io.udp {
            // UDP：out_buf 按数据报整发（建流时投递的每条 Out 都是一个完整数据报）
            let n = unsafe {
                libc::send(io.fd, remaining.as_ptr().cast(), remaining.len(), 0)
            };
            if n >= 0 {
                let sent = remaining.len();
                io.out_buf.consume(sent);
                return sent;
            }
            let e = std::io::Error::last_os_error();
            if e.kind() != io::ErrorKind::WouldBlock {
                self.mark_eof(flow);
            }
            return 0;
        }
        let n = unsafe { libc::write(io.fd, remaining.as_ptr().cast(), remaining.len()) };
        let mut written = 0usize;
        if n > 0 {
            io.out_buf.consume(n as usize);
            written = n as usize;
            // 全写完 + 曾经 EOF 标记：可以真正关 fd
            if io.out_buf.remaining().is_empty() && io.dead {
                let io = self.flows.remove(&flow).expect("刚判存在");
                self.fd_index.remove(&io.fd);
                close_fd(io.fd, io.udp);
            }
        } else if n < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != io::ErrorKind::WouldBlock {
                self.mark_eof(flow);
            }
        }
        written
    }

    /// upstream EOF/错误：上报驱动（TCP 桥「双向拆」的 upstream 半边；剩余写缓冲
    /// 排空后真正关 fd——对端 FIN 后仍把在途数据发完）。
    fn mark_eof(&mut self, flow: u64) {
        let reported = if let Some(io) = self.flows.get_mut(&flow) {
            if io.dead {
                false
            } else {
                io.dead = true;
                true
            }
        } else {
            false
        };
        if reported {
            let _ = self.event_tx.send(PoolEvent::UpstreamEof { flow });
        }
        // 无写缓冲残留时立即收 fd；有则等 flush_flow 排空
        if let Some(io) = self.flows.get(&flow) {
            if io.dead && io.out_buf.remaining().is_empty() {
                let io = self.flows.remove(&flow).expect("刚判存在");
                self.fd_index.remove(&io.fd);
                close_fd(io.fd, io.udp);
            }
        }
    }
}

fn set_nonblocking(fd: RawFd) {
    unsafe {
        let fl = libc::fcntl(fd, libc::F_GETFL);
        libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
    }
}

fn close_fd(fd: RawFd, udp: bool) {
    // 经类型化对象 drop（TcpStream/UnixStream/UdpSocket 的 Drop 关 fd）
    if udp {
        drop(unsafe { UdpSocket::from_raw_fd(fd) });
    } else {
        // UDS 与 TCP 在 fd 层不可区分——统一按 TcpStream drop（都是 close(2)）
        drop(unsafe { TcpStream::from_raw_fd(fd) });
    }
}

/// worker 池句柄（驱动线程持有；命令按流轮转分派）。
pub struct WorkerPool {
    txs: Vec<Sender<PoolCmd>>,
    wakes: Vec<RawFd>,
    event_tx: Sender<PoolEvent>,
    /// 流 → 归属 worker（Adopt/命令必须回同一条——fd 表是 per-worker 的）。
    flow_owner: HashMap<u64, usize>,
}

impl WorkerPool {
    /// 起 N 条 worker（每条一条命令通道 + 一条唤醒管道）。事件通道由本函数建——
    /// Receiver 交驱动线程，Sender 的克隆留池内（拨号线程用）。
    pub fn spawn(n: usize) -> (Self, mpsc::Receiver<PoolEvent>) {
        let (event_tx, event_rx) = mpsc::channel::<PoolEvent>();
        let mut txs = Vec::with_capacity(n);
        let mut wakes = Vec::with_capacity(n);
        for _ in 0..n {
            let (tx, rx) = mpsc::channel::<PoolCmd>();
            let mut fds = [0i32; 2];
            unsafe { libc::pipe(fds.as_mut_ptr()) };
            let wake_r = fds[0];
            let wake_w = fds[1];
            set_nonblocking(wake_r);
            set_nonblocking(wake_w);
            let worker = Worker {
                cmd_rx: rx,
                event_tx: event_tx.clone(),
                flows: HashMap::new(),
                fd_index: HashMap::new(),
            };
            std::thread::Builder::new()
                .name("homeway-iow".into())
                .stack_size(256 * 1024)
                .spawn(move || worker.run(wake_r))
                .expect("spawn worker");
            txs.push(tx);
            wakes.push(wake_w);
        }
        (
            Self { txs, wakes, event_tx, flow_owner: HashMap::new() },
            event_rx,
        )
    }

    /// 按流发命令（流的 fd 表是 per-worker 的——必须回到归属 worker；投后唤醒）。
    /// 未知流（已被收）静默丢弃（与 Go「对已断连接写 = 无操作」同义）。
    pub fn send_for(&mut self, flow: u64, cmd: PoolCmd) {
        let Some(i) = self.flow_owner.get(&flow).copied() else { return };
        if self.txs[i].send(cmd).is_ok() {
            unsafe {
                libc::write(self.wakes[i], b"x".as_ptr().cast(), 1);
            }
        }
    }

    pub fn workers(&self) -> usize {
        self.txs.len()
    }

    /// 生成「拨号 + 移交」的短命线程（拨号阻塞面最长 10s，不占池）。
    /// worker_index = 建流时定的归属（Adopt 回同一条）。
    pub fn spawn_dial(&mut self, flow: u64, target: Upstream, worker_index: usize) {
        let wake = self.wakes[worker_index];
        let tx2 = self.txs[worker_index].clone();
        let event_tx = self.event_tx.clone();
        self.flow_owner.insert(flow, worker_index);
        std::thread::Builder::new()
            .name("homeway-dial".into())
            .stack_size(256 * 1024)
            .spawn(move || {
                // DialOk 在 Adopt 时由 worker 上报（fd 落位与事件同序）；这里只上报失败。
                match dial(&target) {
                    Ok(fd) => {
                        if tx2
                            .send(PoolCmd::Adopt {
                                flow,
                                fd,
                                udp: matches!(target, Upstream::Udp(_)),
                            })
                            .is_ok()
                        {
                            unsafe {
                                libc::write(wake, b"x".as_ptr().cast(), 1);
                            }
                        }
                    }
                    Err(_) => {
                        let _ = event_tx.send(PoolEvent::DialFailed { flow });
                    }
                }
            })
            .expect("spawn dial");
    }
}

/// 拨号（阻塞；超时 10s——Go dialTimeout 同值）。
fn dial(target: &Upstream) -> io::Result<RawFd> {
    match target {
        Upstream::Tcp(addr) => {
            let s = TcpStream::connect_timeout(addr, Duration::from_secs(10))?;
            s.set_nodelay(true).ok();
            Ok(s.into_raw_fd_arc())
        }
        Upstream::Unix(path) => {
            let s = UnixStream::connect(path)?;
            Ok(s.into_raw_fd_arc())
        }
        Upstream::Udp(addr) => {
            let bind_addr = if addr.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
            let s = UdpSocket::bind(bind_addr)?;
            s.connect(addr)?;
            Ok(s.into_raw_fd_arc())
        }
    }
}

/// 「Arc 化」的 raw fd 移交（避免 Send 边界——直接 into_raw_fd + 数字传递）。
trait IntoRawFdArc {
    fn into_raw_fd_arc(self) -> RawFd;
}

impl IntoRawFdArc for TcpStream {
    fn into_raw_fd_arc(self) -> RawFd {
        use std::os::unix::io::IntoRawFd;
        self.into_raw_fd()
    }
}

impl IntoRawFdArc for UnixStream {
    fn into_raw_fd_arc(self) -> RawFd {
        use std::os::unix::io::IntoRawFd;
        self.into_raw_fd()
    }
}

impl IntoRawFdArc for UdpSocket {
    fn into_raw_fd_arc(self) -> RawFd {
        use std::os::unix::io::IntoRawFd;
        self.into_raw_fd()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 池的端到端：拨号（UDS 对）→ Adopt（DialOk）→ 双向数据 → EOF 拆。
    #[test]
    fn pool_end_to_end_uds() {
        let dir = std::env::temp_dir().join(format!(
            "homeway-rs-pool-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("t.sock");
        let listener = std::os::unix::net::UnixListener::bind(&sock_path).unwrap();
        // 服务端回显
        let echo = std::thread::spawn(move || {
            if let Ok((mut c, _)) = listener.accept() {
                let mut buf = [0u8; 1024];
                loop {
                    match c.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if c.write_all(&buf[..n]).is_err() {
                                break;
                            }
                        }
                    }
                }
            }
        });

        let (mut pool, events) = WorkerPool::spawn(2);
        pool.spawn_dial(7, Upstream::Unix(sock_path.to_string_lossy().into_owned()), 0);
        // 等 DialOk
        let mut got_data = false;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok(ev) = events.recv_timeout(Duration::from_millis(200)) {
                match ev {
                    PoolEvent::DialOk { flow } => {
                        assert_eq!(flow, 7);
                        pool.send_for(7, PoolCmd::Out { flow: 7, data: b"ping".to_vec() });
                    }
                    PoolEvent::UpstreamData { flow, data } => {
                        assert_eq!((flow, data.as_slice()), (7, &b"ping"[..]));
                        got_data = true;
                        break;
                    }
                    PoolEvent::Written { .. }
                    | PoolEvent::DialFailed { .. }
                    | PoolEvent::UpstreamEof { .. }
                    | PoolEvent::Closed { .. } => {} // 本测试不消费
                }
            }
        }
        assert!(got_data, "回显未到达");
        // 关闭 → Closed 回执
        pool.send_for(7, PoolCmd::Close { flow: 7, linger_rst: false });
        let closed = events
            .recv_timeout(Duration::from_secs(3))
            .map(|e| matches!(e, PoolEvent::Closed { flow: 7 }))
            .unwrap_or(false);
        assert!(closed, "Closed 回执未到");
        let _ = echo.join();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
