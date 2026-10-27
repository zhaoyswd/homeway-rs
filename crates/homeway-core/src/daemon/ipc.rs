//! 本地 IPC 承载（WIN 批 2026-10-13；设计 `docs/reviews/WIN-design.md` §D2）。
//!
//! **unix = UDS 原样**（路径/权限/判据行为零变化——`listen.rs` 等消费面继续走
//! `sock_path`）；**windows = TCP loopback + 端口文件**：listener 绑 `127.0.0.1:0`，
//! 端口写入同目录 `<名>.port` 文件（`control.sock` → `control.port`），客户端读端口
//! 文件后回连。协议层（自定义帧 over 字节流）传输无关——无 fd 传递、无凭证面依赖。
//!
//! 鉴权面偏离（登记在 WIN-design §D2）：unix 靠 socket 文件属主（0600 + 0700 目录）；
//! windows loopback 无 per-user 隔离（同机任意本地进程可连）——本批接受（单用户桌面
//! 形态），后续批可加握手 token。
//!
//! 监听点语义（两平台同构）：「占位文件/端口文件存在且可连 = 活实例占用」——
//! windows 的残留探测走端口文件 + TCP connect（ECONNREFUSED = 死残留 ⇒ 清除重 bind）。

use std::io::{self, Read, Write};
use std::net::Shutdown;
#[cfg(windows)]
use std::net::{TcpListener, TcpStream, ToSocketAddrs};
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
#[cfg(windows)]
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(unix)]
fn connect_unix_timeout(path: &Path, timeout: Duration) -> io::Result<UnixStream> {
    use std::os::fd::AsRawFd;
    let deadline = Instant::now() + timeout;
    let address = socket2::SockAddr::unix(path)?;
    loop {
        if Instant::now() >= deadline { return Err(io::ErrorKind::TimedOut.into()); }
        let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
        socket.set_nonblocking(true)?;
        match socket.connect(&address) {
            Ok(()) => {},
            // AF_UNIX EAGAIN表示监听队列满、连接尚未排入。不能把SO_ERROR=0当成功；
            // 失败socket由RAII关闭，等待后在同一截止内用新socket重试。
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                drop(socket);
                std::thread::sleep(Duration::from_millis(2).min(deadline.saturating_duration_since(Instant::now())));
                continue;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted || matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EALREADY)) => {
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
                    let mut poll = libc::pollfd { fd: socket.as_raw_fd(), events: libc::POLLIN | libc::POLLOUT, revents: 0 };
                    let milliseconds = remaining.as_millis().max(1).min(i32::MAX as u128) as i32;
                    let ready = unsafe { libc::poll(&mut poll, 1, milliseconds) };
                    if ready < 0 {
                        let error = io::Error::last_os_error();
                        if error.kind() == io::ErrorKind::Interrupted { continue; }
                        return Err(error);
                    }
                    if ready == 0 { continue; }
                    if let Some(error) = socket.take_error()? { return Err(error); }
                    match socket.peer_addr() {
                        Ok(_) => break,
                        Err(error) if error.kind() == io::ErrorKind::NotConnected => {
                            if poll.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "UDS连接未建立"));
                            }
                            std::thread::sleep(Duration::from_millis(1).min(deadline.saturating_duration_since(Instant::now())));
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
            Err(error) => return Err(error),
        }
        // 包括connect直接返回Ok的路径，均验证确有对端，绝不返回未连接socket。
        if Instant::now() >= deadline { return Err(io::ErrorKind::TimedOut.into()); }
        socket.peer_addr()?;
        socket.set_nonblocking(false)?;
        return Ok(socket.into());
    }
}

/// sock 路径 → 端口文件路径（`x.sock` → `x.port`；同目录同名干）。
#[cfg(windows)]
fn port_file_of(sock_path: &Path) -> PathBuf {
    let mut name = sock_path.file_name().map(|s| s.to_string_lossy().into_owned());
    if let Some(n) = name.as_mut() {
        if let Some(stem) = n.strip_suffix(".sock") {
            *n = format!("{stem}.port");
        } else {
            *n = format!("{n}.port");
        }
    }
    name.map(|n| sock_path.with_file_name(n)).unwrap_or_else(|| sock_path.with_extension("port"))
}

/// IPC 监听器。
pub enum IpcListener {
    #[cfg(unix)]
    Unix(UnixListener),
    #[cfg(windows)]
    Tcp(TcpListener),
}

/// IPC 连接流（`Read`/`Write` 已 impl；句柄可 try_clone 分读写两半）。
pub enum IpcStream {
    #[cfg(unix)]
    Unix(UnixStream),
    #[cfg(windows)]
    Tcp(TcpStream),
}

impl IpcListener {
    /// 监听（unix = bind sock 路径——**不含** listen.rs 的残留探测/权限收紧，那部分
    /// 留在调用面；windows = loopback 绑定 + 端口文件写入）。
    pub fn bind(sock_path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::Unix(UnixListener::bind(sock_path)?))
        }
        #[cfg(windows)]
        {
            let ln = TcpListener::bind(("127.0.0.1", 0))?;
            let port = ln.local_addr()?.port();
            let pf = port_file_of(sock_path);
            // 原子写（tmp+rename）：读方不会看到半截端口。
            let tmp = pf.with_extension("port.tmp");
            std::fs::write(&tmp, port.to_string())?;
            std::fs::rename(&tmp, &pf)?;
            Ok(Self::Tcp(ln))
        }
    }

    pub fn accept(&self) -> io::Result<IpcStream> {
        match self {
            #[cfg(unix)]
            Self::Unix(ln) => Ok(IpcStream::Unix(ln.accept()?.0)),
            #[cfg(windows)]
            Self::Tcp(ln) => Ok(IpcStream::Tcp(ln.accept()?.0)),
        }
    }

    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(ln) => ln.set_nonblocking(on),
            #[cfg(windows)]
            Self::Tcp(ln) => ln.set_nonblocking(on),
        }
    }
}

/// 探测监听点是否被活实例占用（listen 面的残留探测收口；返回 Err(e) = 模糊形态）。
pub fn probe_alive(sock_path: &Path) -> io::Result<bool> {
    #[cfg(unix)]
    {
        match UnixStream::connect(sock_path) {
            Ok(_) => Ok(true),
            Err(e) => match e.kind() {
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => Ok(false),
                _ => Err(e),
            },
        }
    }
    #[cfg(windows)]
    {
        let pf = port_file_of(sock_path);
        let Ok(body) = std::fs::read_to_string(&pf) else {
            return Ok(false); // 无端口文件 = 无占用（与 unix ENOENT 同位）
        };
        let Ok(port) = body.trim().parse::<u16>() else {
            return Ok(false); // 坏端口文件按死残留（不清——RemoveFailed 面在调用侧）
        };
        match TcpStream::connect_timeout(
            &("127.0.0.1", port).to_socket_addrs()?.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "loopback 地址解析失败")
            })?,
            Duration::from_millis(500),
        ) {
            Ok(_) => Ok(true),
            Err(e) => match e.kind() {
                io::ErrorKind::ConnectionRefused => Ok(false),
                _ => Err(e),
            },
        }
    }
}

/// 清监听点残留（bind 前调用；windows = 删端口文件，unix = 删 sock 文件——都由
/// 调用面对 NotFound 容错）。
pub fn remove_stale(sock_path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::fs::remove_file(sock_path)
    }
    #[cfg(windows)]
    {
        let _ = sock_path; // sock 路径本身不落盘；端口文件在 bind 内覆盖写
        std::fs::remove_file(port_file_of(sock_path))
    }
}

impl IpcStream {
    /// 连接（unix = UDS connect；windows = 读端口文件回连）。
    pub fn connect(sock_path: &Path) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self::Unix(UnixStream::connect(sock_path)?))
        }
        #[cfg(windows)]
        {
            let pf = port_file_of(sock_path);
            let body = std::fs::read_to_string(&pf).map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("端口文件 {} 缺席（守护进程未运行？）", pf.display()),
                ),
                _ => e,
            })?;
            let port: u16 = body.trim().parse().map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("端口文件 {} 内容非端口：{:?}", pf.display(), body.trim()),
                )
            })?;
            Ok(Self::Tcp(TcpStream::connect(("127.0.0.1", port))?))
        }
    }

    /// 连接阶段也受调用方预算限制；旧connect的缺省行为保持不变。
    pub fn connect_timeout(sock_path: &Path, timeout: Duration) -> io::Result<Self> {
        if timeout.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
        #[cfg(unix)]
        { connect_unix_timeout(sock_path, timeout).map(Self::Unix) }
        #[cfg(windows)]
        {
            let deadline = Instant::now() + timeout;
            let pf = port_file_of(sock_path);
            let body = std::fs::read_to_string(&pf).map_err(|error| match error.kind() {
                io::ErrorKind::NotFound => io::Error::new(io::ErrorKind::NotFound, format!("端口文件 {} 缺席（守护进程未运行？）", pf.display())),
                _ => error,
            })?;
            let port: u16 = body.trim().parse().map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("端口文件 {} 内容非端口：{:?}", pf.display(), body.trim())))?;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() { return Err(io::ErrorKind::TimedOut.into()); }
            TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], port)), remaining).map(Self::Tcp)
        }
    }

    pub fn try_clone(&self) -> io::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => Ok(Self::Unix(s.try_clone()?)),
            #[cfg(windows)]
            Self::Tcp(s) => Ok(Self::Tcp(s.try_clone()?)),
        }
    }

    pub fn set_nonblocking(&self, on: bool) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.set_nonblocking(on),
            #[cfg(windows)]
            Self::Tcp(s) => s.set_nonblocking(on),
        }
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.set_read_timeout(d),
            #[cfg(windows)]
            Self::Tcp(s) => s.set_read_timeout(d),
        }
    }

    pub fn set_write_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.set_write_timeout(d),
            #[cfg(windows)]
            Self::Tcp(s) => s.set_write_timeout(d),
        }
    }

    pub fn shutdown(&self, how: Shutdown) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.shutdown(how),
            #[cfg(windows)]
            Self::Tcp(s) => s.shutdown(how),
        }
    }
}

impl Read for IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
            #[cfg(windows)]
            Self::Tcp(s) => s.read(buf),
        }
    }
}

impl Write for IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
            #[cfg(windows)]
            Self::Tcp(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
            #[cfg(windows)]
            Self::Tcp(s) => s.flush(),
        }
    }
}

impl Read for &IpcStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => (&*s).read(buf),
            #[cfg(windows)]
            IpcStream::Tcp(s) => (&*s).read(buf),
        }
    }
}

impl Write for &IpcStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => (&*s).write(buf),
            #[cfg(windows)]
            IpcStream::Tcp(s) => (&*s).write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            #[cfg(unix)]
            IpcStream::Unix(s) => (&*s).flush(),
            #[cfg(windows)]
            IpcStream::Tcp(s) => (&*s).flush(),
        }
    }
}

#[cfg(all(test, unix))]
mod connect_budget_tests {
    use super::*;
    use std::os::fd::AsRawFd;

    fn path(tag: &str) -> std::path::PathBuf {
        std::path::PathBuf::from("/tmp").join(format!("hw-ipc-{tag}-{}-{}.sock", std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()))
    }

    #[test]
    fn full_unix_backlog_observes_connect_budget_without_false_success() {
        let path = path("backlog");
        let address = socket2::SockAddr::unix(&path).unwrap();
        let listener = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
        listener.bind(&address).unwrap();
        listener.listen(1).unwrap();
        let mut queued = Vec::new();
        let mut full = false;
        for _ in 0..32 {
            let socket = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None).unwrap();
            socket.set_nonblocking(true).unwrap();
            match socket.connect(&address) {
                Ok(()) => queued.push(socket),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock || matches!(error.raw_os_error(), Some(libc::EINPROGRESS | libc::EALREADY)) => {
                    // Darwin可能先回EINPROGRESS；已建立连接保留在队列中，未建立才算满。
                    std::thread::sleep(Duration::from_millis(2));
                    if socket.peer_addr().is_ok() { queued.push(socket); } else { full = true; break; }
                }
                Err(error) if cfg!(target_os = "macos") && error.kind() == io::ErrorKind::ConnectionRefused => { full = true; break; }
                Err(error) => panic!("填充私有UDS backlog失败：{error}"),
            }
        }
        assert!(full, "测试必须确实填满backlog，否则不能验证connect阶段预算");
        let (tx, rx) = std::sync::mpsc::channel();
        let connecting_path = path.clone();
        let connecting = std::thread::spawn(move || {
            tx.send(IpcStream::connect_timeout(&connecting_path, Duration::from_millis(100)).map(|stream| {
                let IpcStream::Unix(stream) = stream;
                stream.peer_addr().is_ok()
            })).unwrap();
        });
        // listener始终不accept；旧阻塞connect在这段期间不会返回。
        let observed = rx.recv_timeout(Duration::from_secs(1));
        drop(listener);
        drop(queued);
        connecting.join().unwrap();
        std::fs::remove_file(path).unwrap();
        let result = observed.expect("connect预算到期前不能依赖listener accept/close才返回");
        // Linux队列满会EAGAIN并等预算；Darwin直接ECONNREFUSED。两者均不能假报连接成功。
        assert!(matches!(result, Err(error) if error.kind() == io::ErrorKind::TimedOut || (cfg!(target_os = "macos") && error.kind() == io::ErrorKind::ConnectionRefused)), "满队列不能把未连接socket伪报成功");
    }

    #[test]
    fn successful_bounded_connect_is_connected_and_blocking() {
        let path = path("ready");
        let listener = UnixListener::bind(&path).unwrap();
        let IpcStream::Unix(mut stream) = IpcStream::connect_timeout(&path, Duration::from_secs(1)).unwrap();
        assert!(stream.peer_addr().is_ok());
        let flags = unsafe { libc::fcntl(stream.as_raw_fd(), libc::F_GETFL) };
        assert_eq!(flags & libc::O_NONBLOCK, 0);
        stream.write_all(b"bounded").unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        let mut bytes = [0; 7];
        accepted.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"bounded");
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
}
