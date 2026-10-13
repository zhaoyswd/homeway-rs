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
use std::time::Duration;

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
