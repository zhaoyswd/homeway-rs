//! 跨平台 socket/poll 底座（WIN 批 2026-10-13）。
//!
//! 形态：**unix 臂 = libc 直通**（调用点与改动前逐调用同形——行为零变化，由既有
//! 测试面守）；**windows 臂 = winsock**（`WSASocketW`/`ioctlsocket(FIONBIO)`/
//! `WSAPoll`/`send/recv`）。设计见 `docs/reviews/WIN-design.md` §D4。
//!
//! 为什么不用 std：reactor 需要**非阻塞 connect**（std 的 connect 把 EINPROGRESS 当
//! Err、`connect_timeout` 阻塞）与**多 fd 就绪轮询**（std 无 poll 面）；两平台原生
//! API 形状一致（poll vs WSAPoll、fcntl vs ioctlsocket），薄封装即可双臂同源。
//!
//! 语义钉（windows 臂与 unix 的已知等价/近似，评审载体）：
//! - `WSAPoll` 电平触发、timeout=0 即刻返回——与本 reactor 的用法（每拍零超时轮询）
//!   同义；winsock 无 `POLLRDHUP`（本面未消费该位）。
//! - windows 非阻塞 connect 的在途码 = `WSAEWOULDBLOCK`（unix = `EINPROGRESS`）——
//!   [`ConnectErr`] 统一形态。
//! - winsock 错误经 `WSAGetLastError` 取（**不是** `GetLastError`——`io::Error::
//!   last_os_error` 在 socket 调用后不可靠）；[`last_sock_err`] 是唯一取错入口。
//! - 句柄不继承性：windows 句柄默认不可继承（CreateProcess 需显式 bInheritHandles）
//!   ⇒ unix 的 CLOEXEC 纪律在 windows 是空操作。

use std::io;
use std::net::{SocketAddr, UdpSocket};

// ---------- 句柄类型（unix = fd / windows = SOCKET） ----------

#[cfg(unix)]
pub use std::os::fd::{AsFd as AsSysFd, OwnedFd as SysSock};

#[cfg(windows)]
pub use std::os::windows::io::OwnedSocket as SysSock;

/// 裸句柄（poll/send/recv 的入参形态）。windows = `RawSocket` 的 u64 宽度
/// （winsock API 的 SOCKET 收窄 cast——64 位进程内两种宽度等值，收窄不丢信息）。
#[cfg(unix)]
pub type RawSock = std::os::fd::RawFd;
#[cfg(windows)]
pub type RawSock = u64;

/// 从持有句柄取裸句柄（reactor 驱动线程独占——裸值只在当拍内使用）。
#[cfg(unix)]
pub fn sock_raw(s: &SysSock) -> RawSock {
    std::os::fd::AsRawFd::as_raw_fd(s)
}
#[cfg(windows)]
pub fn sock_raw(s: &SysSock) -> RawSock {
    std::os::windows::io::AsRawSocket::as_raw_socket(s)
}

/// 本地 TCP 连接的 RST 收口（`SO_LINGER(l_onoff=1, l_linger=0)`——close 时不再走
/// 优雅 FIN 队列而是直接复位；尽力而为，失败静默）。语义/注释承 `sysfd::rst_close_tcp`
/// （Q-F-B F2-3/D13 单源——WIN 批随 socket 底座迁入；unix 行为零变化）。
///
/// **只设选项、不关 fd**：fd 的生命周期归调用方（drop 即 close ⇒ 生效）。
pub fn rst_close_tcp(stream: &std::net::TcpStream) {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        unsafe {
            let linger = libc::linger {
                l_onoff: 1,
                l_linger: 0,
            };
            let _ = libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                &linger as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::linger>() as libc::socklen_t,
            );
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{LINGER, SO_LINGER};
        let linger = LINGER {
            l_onoff: 1,
            l_linger: 0,
        };
        let _ = setsockopt_bytes(
            tcp_stream_raw(stream),
            SOL_SOCKET,
            SO_LINGER as i32,
            (&linger as *const LINGER).cast(),
            std::mem::size_of::<LINGER>(),
        );
    }
}

/// 句柄有效性（unix：fd ≥ 0；windows：句柄非 u64::MAX 哨兵——poll 集构建的防御位）。
#[cfg(unix)]
pub fn is_valid(raw: RawSock) -> bool {
    raw >= 0
}
#[cfg(windows)]
pub fn is_valid(raw: RawSock) -> bool {
    raw != u64::MAX
}

/// socket 调用后的错误（winsock 经 WSAGetLastError）。
#[cfg(unix)]
pub fn last_sock_err() -> io::Error {
    io::Error::last_os_error()
}
#[cfg(windows)]
pub fn last_sock_err() -> io::Error {
    io::Error::from_raw_os_error(unsafe {
        windows_sys::Win32::Networking::WinSock::WSAGetLastError()
    })
}

// ---------- 兴趣位与 poll ----------

#[cfg(unix)]
pub use libc::{POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT};
#[cfg(windows)]
pub use windows_sys::Win32::Networking::WinSock::{
    POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT,
};

/// poll 条目（跨平台字段名：unix `pollfd.fd` / windows `WSAPOLLFD.socket`）。
/// 布局钉（评审 M5）：unix 臂把整片 `PollEnt` 直接 reinterpret 成 `libc::pollfd`
/// ⇒ 必须 `#[repr(C)]` 且字段序/宽度一致（编译期断言）；加字段/重排会被断言拦下
/// 而不是静默给错 `revents`。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollEnt {
    pub raw: RawSock,
    pub events: i16,
    pub revents: i16,
}
#[cfg(unix)]
const _: () = {
    assert!(std::mem::size_of::<PollEnt>() == std::mem::size_of::<libc::pollfd>());
    assert!(std::mem::align_of::<PollEnt>() == std::mem::align_of::<libc::pollfd>());
    // 字段偏移逐个对（fd/events/revents 三段的位形）
    assert!(std::mem::offset_of!(PollEnt, raw) == std::mem::offset_of!(libc::pollfd, fd));
    assert!(std::mem::offset_of!(PollEnt, events) == std::mem::offset_of!(libc::pollfd, events));
    assert!(std::mem::offset_of!(PollEnt, revents) == std::mem::offset_of!(libc::pollfd, revents));
};

/// 零超时轮询（reactor 每拍形态：就绪集是提示不是契约——电平触发，漏看下拍重报）。
/// 返回就绪条数；`Err` = 系统调用失败（调用方按拍级失败处理——与 unix 臂现状同形）。
pub fn poll_slice(ents: &mut [PollEnt]) -> io::Result<i32> {
    poll_slice_timeout(ents, 0)
}

/// 带超时 poll（毫秒；负值 = 无限等——POSIX poll 口径）。`Err(Interrupted)` 形态
/// 由系统调用如实透传（调用方按需重试）。
pub fn poll_slice_timeout(ents: &mut [PollEnt], timeout_ms: i32) -> io::Result<i32> {
    #[cfg(unix)]
    unsafe {
        let rc = libc::poll(
            ents.as_mut_ptr().cast(),
            ents.len() as libc::nfds_t,
            timeout_ms,
        );
        if rc < 0 {
            return Err(last_sock_err());
        }
        Ok(rc)
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Networking::WinSock::{WSAPOLLFD, WSAPoll};
        let mut pfds: Vec<WSAPOLLFD> = ents
            .iter()
            .map(|e| WSAPOLLFD {
                fd: e.raw as usize,
                events: e.events,
                revents: e.revents,
            })
            .collect();
        let rc = WSAPoll(pfds.as_mut_ptr(), pfds.len() as u32, timeout_ms);
        if rc == windows_sys::Win32::Networking::WinSock::SOCKET_ERROR {
            return Err(last_sock_err());
        }
        for (e, p) in ents.iter_mut().zip(pfds.iter()) {
            e.revents = p.revents;
        }
        Ok(rc)
    }
}

// ---------- 地址（sockaddr 组装；按目标族选 domain） ----------

/// 地址缓冲（sockaddr_storage 同容量；[`pack_addr`] 组装）。
#[derive(Clone, Copy)]
pub struct SockAddrBuf {
    bytes: [u8; 128],
    len: usize,
    /// 域（AF_INET/AF_INET6——socket 创建入参）。
    pub domain: i32,
}

/// 组装 sockaddr（v4/v6；网络字节序与各平台布局一致——sin_family/sin_port/sin_addr
/// 三段 wire 形态跨平台同构）。
pub fn pack_addr(a: &SocketAddr) -> SockAddrBuf {
    #[cfg(unix)]
    fn put(a: &SocketAddr, out: &mut [u8; 128]) -> usize {
        match a {
            SocketAddr::V4(v4) => {
                let s: libc::sockaddr_in = libc::sockaddr_in {
                    sin_family: libc::AF_INET as libc::sa_family_t,
                    sin_port: v4.port().to_be(),
                    sin_addr: libc::in_addr {
                        s_addr: u32::from_ne_bytes(v4.ip().octets()),
                    },
                    sin_zero: [0; 8],
                    #[cfg(any(target_os = "macos", target_os = "ios"))]
                    sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                };
                let n = std::mem::size_of::<libc::sockaddr_in>();
                // SAFETY：s 是本栈上的 POD sockaddr_in，按字节读出 n 个（wire 布局）。
                unsafe {
                    out[..n].copy_from_slice(std::slice::from_raw_parts(
                        (&s as *const libc::sockaddr_in).cast(),
                        n,
                    ));
                }
                n
            }
            SocketAddr::V6(v6) => {
                let mut s: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
                s.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                s.sin6_port = v6.port().to_be();
                s.sin6_addr = libc::in6_addr {
                    s6_addr: v6.ip().octets(),
                };
                s.sin6_flowinfo = v6.flowinfo();
                s.sin6_scope_id = v6.scope_id();
                #[cfg(any(target_os = "macos", target_os = "ios"))]
                {
                    s.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
                }
                let n = std::mem::size_of::<libc::sockaddr_in6>();
                // SAFETY：同上（sockaddr_in6 的 POD 字节面）。
                unsafe {
                    out[..n].copy_from_slice(std::slice::from_raw_parts(
                        (&s as *const libc::sockaddr_in6).cast(),
                        n,
                    ));
                }
                n
            }
        }
    }
    #[cfg(windows)]
    fn put(a: &SocketAddr, out: &mut [u8; 128]) -> usize {
        use windows_sys::Win32::Networking::WinSock::{
            AF_INET, AF_INET6, IN_ADDR, IN6_ADDR, SOCKADDR_IN, SOCKADDR_IN6, ADDRESS_FAMILY,
        };
        match a {
            SocketAddr::V4(v4) => {
                let s = SOCKADDR_IN {
                    sin_family: AF_INET as ADDRESS_FAMILY,
                    sin_port: v4.port().to_be(),
                    sin_addr: IN_ADDR {
                        S_un: windows_sys::Win32::Networking::WinSock::IN_ADDR_0 {
                            S_addr: u32::from_ne_bytes(v4.ip().octets()),
                        },
                    },
                    sin_zero: [0; 8],
                };
                let n = std::mem::size_of::<SOCKADDR_IN>();
                // SAFETY：s 是本栈上的 POD sockaddr，按字节读出 n 个（wire 布局）。
                unsafe {
                    out[..n].copy_from_slice(std::slice::from_raw_parts(
                        (&s as *const SOCKADDR_IN).cast(),
                        n,
                    ));
                }
                n
            }
            SocketAddr::V6(v6) => {
                let s = SOCKADDR_IN6 {
                    sin6_family: AF_INET6 as ADDRESS_FAMILY,
                    sin6_port: v6.port().to_be(),
                    sin6_flowinfo: v6.flowinfo(),
                    sin6_addr: IN6_ADDR {
                        u: windows_sys::Win32::Networking::WinSock::IN6_ADDR_0 {
                            Byte: v6.ip().octets(),
                        },
                    },
                    Anonymous: windows_sys::Win32::Networking::WinSock::SOCKADDR_IN6_0 {
                        sin6_scope_id: v6.scope_id(),
                    },
                };
                let n = std::mem::size_of::<SOCKADDR_IN6>();
                // SAFETY：同上（sockaddr_in6 的 POD 字节面）。
                unsafe {
                    out[..n].copy_from_slice(std::slice::from_raw_parts(
                        (&s as *const SOCKADDR_IN6).cast(),
                        n,
                    ));
                }
                n
            }
        }
    }
    let mut buf = SockAddrBuf {
        bytes: [0; 128],
        len: 0,
        domain: match a {
            SocketAddr::V4(_) => AF_INET_VAL,
            SocketAddr::V6(_) => AF_INET6_VAL,
        },
    };
    buf.len = put(a, &mut buf.bytes);
    buf
}

#[cfg(unix)]
const AF_INET_VAL: i32 = libc::AF_INET;
#[cfg(unix)]
const AF_INET6_VAL: i32 = libc::AF_INET6;
#[cfg(windows)]
const AF_INET_VAL: i32 = windows_sys::Win32::Networking::WinSock::AF_INET as i32;
#[cfg(windows)]
const AF_INET6_VAL: i32 = windows_sys::Win32::Networking::WinSock::AF_INET6 as i32;

impl SockAddrBuf {
    pub fn as_ptr(&self) -> *const u8 {
        self.bytes.as_ptr()
    }
    /// wire 长度（sockaddr 实际字节数——bind/connect 的第三参）。
    pub fn sockaddr_len(&self) -> usize {
        self.len
    }
}

/// socket 类型（创建入参）。
pub const SOCK_STREAM: i32 = {
    #[cfg(unix)]
    {
        libc::SOCK_STREAM
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::SOCK_STREAM as i32
    }
};
pub const SOCK_DGRAM: i32 = {
    #[cfg(unix)]
    {
        libc::SOCK_DGRAM
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::SOCK_DGRAM as i32
    }
};

/// 建 socket（unix 经 `sysfd::socket_cloexec`——CLOEXEC 纪律单源；windows =
/// `WSASocketW`，句柄默认不可继承 + 显式 WSAStartup 一次）。
pub fn socket(domain: i32, ty: i32) -> io::Result<SysSock> {
    #[cfg(unix)]
    {
        crate::sysfd::socket_cloexec(domain, ty, 0)
    }
    #[cfg(windows)]
    {
        wsa_init();
        use std::os::windows::io::FromRawSocket as _;
        use windows_sys::Win32::Networking::WinSock::{WSASocketW, WSA_FLAG_OVERLAPPED};
        let s = unsafe { WSASocketW(domain, ty, 0, std::ptr::null_mut(), 0, WSA_FLAG_OVERLAPPED) };
        if s == windows_sys::Win32::Networking::WinSock::INVALID_SOCKET {
            return Err(last_sock_err());
        }
        // SAFETY：WSASocketW 成功即未占用的有效 socket（所有权移交 OwnedSocket）。
        Ok(unsafe { std::os::windows::io::OwnedSocket::from_raw_socket(s as u64) })
    }
}

/// winsock 初始化（幂等；std::net 自身会惰性初始化，但裸 WSASocketW 不保证在其后
/// ——显式 Once 收口，重复调用无害）。
#[cfg(windows)]
fn wsa_init() {
    use std::sync::Once;
    static INIT: Once = Once::new();
    INIT.call_once(|| unsafe {
        use windows_sys::Win32::Networking::WinSock::{WSADATA, WSAStartup};
        let mut data: WSADATA = std::mem::zeroed();
        // MAKEWORD(2,2) 的字面值（windows-sys 未导出该宏——0.59 实测）。
        const WINSOCK_2_2: u16 = 0x0202;
        // 返回值非零 = 初始化失败（已初始化形态不会走到——Once 内首跑）。
        let _ = WSAStartup(WINSOCK_2_2, &mut data);
    });
}

/// 非阻塞位切换（unix = fcntl O_NONBLOCK；windows = ioctlsocket FIONBIO）。
/// unix 臂同时保 FD_CLOEXEC（`sysfd` 纪律——见 intercept 的 set_fd_flags 原注释）。
pub fn set_nonblocking(s: &SysSock, on: bool) -> io::Result<()> {
    raw_set_nonblocking(sock_raw(s), on)
}

/// 非阻塞位切换的**裸句柄值**形态（所有权在调用方的唤醒对/停止管道面——句柄生命
/// 周期不归 SysSock 时用这个）。unix 臂的 CLOEXEC 位由 `SysSock` 创建期承担，本形态
/// 只动非阻塞位。
pub fn raw_set_nonblocking(raw: RawSock, on: bool) -> io::Result<()> {
    #[cfg(unix)]
    {
        unsafe {
            let fl = libc::fcntl(raw, libc::F_GETFL);
            if fl < 0 {
                return Err(io::Error::last_os_error());
            }
            let want = if on { fl | libc::O_NONBLOCK } else { fl & !libc::O_NONBLOCK };
            if libc::fcntl(raw, libc::F_SETFL, want) < 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Networking::WinSock::{ioctlsocket, FIONBIO};
        let mut mode: u32 = u32::from(on);
        let rc = unsafe { ioctlsocket(raw as usize, FIONBIO, &mut mode) };
        if rc != 0 {
            return Err(last_sock_err());
        }
        Ok(())
    }
}

/// bind（裸地址；UDP 临时端口绑定面）。
pub fn bind(s: &SysSock, addr: &SockAddrBuf) -> io::Result<()> {
    #[cfg(unix)]
    {
        let rc = unsafe {
            libc::bind(
                sock_raw(s),
                addr.as_ptr().cast(),
                addr.sockaddr_len() as libc::socklen_t,
            )
        };
        if rc != 0 {
            return Err(last_sock_err());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let rc = unsafe {
            windows_sys::Win32::Networking::WinSock::bind(
                sock_raw(s) as usize,
                addr.as_ptr().cast(),
                addr.sockaddr_len() as i32,
            )
        };
        if rc != 0 {
            return Err(last_sock_err());
        }
        Ok(())
    }
}

/// connect 的三态结论（EINTR 重试由 [`connect`] 内部消化）。
pub enum ConnectOutcome {
    /// 即时成功（UDP bind+connect 恒此态；TCP 回环偶发）。
    Connected,
    /// 非阻塞在途（unix EINPROGRESS/EALREADY；windows WSAEWOULDBLOCK）。
    InProgress,
    /// unix EAGAIN（AF_UNIX 队列满重拨形态；windows 不可达——保留枚举面）。
    Retry,
}

/// 非阻塞 connect（EINTR 内部重试；其它错误如实返回）。
pub fn connect(s: &SysSock, addr: &SockAddrBuf) -> io::Result<ConnectOutcome> {
    loop {
        #[cfg(unix)]
        let rc = unsafe {
            libc::connect(sock_raw(s), addr.as_ptr().cast(), addr.sockaddr_len() as libc::socklen_t)
        };
        #[cfg(windows)]
        let rc = unsafe {
            windows_sys::Win32::Networking::WinSock::connect(
                sock_raw(s) as usize,
                addr.as_ptr().cast(),
                addr.sockaddr_len() as i32,
            )
        };
        if rc == 0 {
            return Ok(ConnectOutcome::Connected);
        }
        let e = last_sock_err();
        match e.raw_os_error() {
            #[cfg(unix)]
            Some(libc::EINTR) => continue,
            #[cfg(windows)]
            Some(windows_sys::Win32::Networking::WinSock::WSAEINTR) => continue,
            #[cfg(unix)]
            Some(libc::EINPROGRESS) | Some(libc::EALREADY) => {
                return Ok(ConnectOutcome::InProgress)
            }
            #[cfg(windows)]
            Some(windows_sys::Win32::Networking::WinSock::WSAEWOULDBLOCK)
            | Some(windows_sys::Win32::Networking::WinSock::WSAEINPROGRESS)
            | Some(windows_sys::Win32::Networking::WinSock::WSAEALREADY) => {
                return Ok(ConnectOutcome::InProgress)
            }
            #[cfg(unix)]
            Some(libc::EAGAIN) => return Ok(ConnectOutcome::Retry),
            _ => return Err(e),
        }
    }
}

/// 发送（unix 带 MSG_NOSIGNAL——SIGPIPE 默认处置下的进程保护；windows 无该语义，
/// flags=0 等价）。返回发送字节数；`Err(WouldBlock)` = 发送缓冲满。
pub fn send(s: RawSock, buf: &[u8]) -> io::Result<usize> {
    #[cfg(unix)]
    let flags = libc::MSG_NOSIGNAL;
    #[cfg(windows)]
    let flags = 0;
    let rc = unsafe {
        #[cfg(unix)]
        {
            libc::send(s, buf.as_ptr().cast(), buf.len(), flags)
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::send(s as usize, buf.as_ptr(), buf.len() as i32, flags)
        }
    };
    #[cfg(unix)]
    if rc < 0 {
        return Err(last_sock_err());
    }
    #[cfg(windows)]
    if rc == windows_sys::Win32::Networking::WinSock::SOCKET_ERROR {
        return Err(last_sock_err());
    }
    Ok(rc as usize)
}

/// 接收（flags=0；`Ok(0)` = EOF，`Err(WouldBlock)` = 无数据）。
pub fn recv(s: RawSock, buf: &mut [u8]) -> io::Result<usize> {
    let rc = unsafe {
        #[cfg(unix)]
        {
            libc::recv(s, buf.as_mut_ptr().cast(), buf.len(), 0)
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::recv(s as usize, buf.as_mut_ptr(), buf.len() as i32, 0)
        }
    };
    #[cfg(unix)]
    if rc < 0 {
        return Err(last_sock_err());
    }
    #[cfg(windows)]
    if rc == windows_sys::Win32::Networking::WinSock::SOCKET_ERROR {
        return Err(last_sock_err());
    }
    Ok(rc as usize)
}

// ---------- sockopt（i32 值族：SO_ERROR/SO_SNDBUF/TCP_NODELAY/IPV6_V6ONLY…） ----------

pub const SOL_SOCKET: i32 = {
    #[cfg(unix)]
    {
        libc::SOL_SOCKET
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::SOL_SOCKET as i32
    }
};
pub const SO_ERROR: i32 = {
    #[cfg(unix)]
    {
        libc::SO_ERROR
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::SO_ERROR as i32
    }
};
pub const SO_SNDBUF: i32 = {
    #[cfg(unix)]
    {
        libc::SO_SNDBUF
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::SO_SNDBUF as i32
    }
};
pub const SO_RCVBUF: i32 = {
    #[cfg(unix)]
    {
        libc::SO_RCVBUF
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::SO_RCVBUF as i32
    }
};
pub const IPPROTO_IPV6: i32 = {
    #[cfg(unix)]
    {
        libc::IPPROTO_IPV6
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::IPPROTO_IPV6 as i32
    }
};
pub const IPV6_V6ONLY: i32 = {
    #[cfg(unix)]
    {
        libc::IPV6_V6ONLY
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::IPV6_V6ONLY as i32
    }
};
pub const IPPROTO_TCP: i32 = {
    #[cfg(unix)]
    {
        libc::IPPROTO_TCP
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::IPPROTO_TCP as i32
    }
};
pub const TCP_NODELAY: i32 = {
    #[cfg(unix)]
    {
        libc::TCP_NODELAY
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::TCP_NODELAY as i32
    }
};
pub const IPPROTO_IP: i32 = {
    #[cfg(unix)]
    {
        libc::IPPROTO_IP
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::IPPROTO_IP as i32
    }
};
pub const IP_MULTICAST_IF: i32 = {
    #[cfg(unix)]
    {
        libc::IP_MULTICAST_IF
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::IP_MULTICAST_IF as i32
    }
};
pub const IP_MULTICAST_TTL: i32 = {
    #[cfg(unix)]
    {
        libc::IP_MULTICAST_TTL
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Networking::WinSock::IP_MULTICAST_TTL as i32
    }
};
/// windows 钉卡（IP_UNICAST_IF/IPV6_UNICAST_IF；unix 无此面——值域不冲突）。
#[cfg(windows)]
pub const IP_UNICAST_IF: i32 = windows_sys::Win32::Networking::WinSock::IP_UNICAST_IF as i32;
#[cfg(windows)]
pub const IPV6_UNICAST_IF: i32 = windows_sys::Win32::Networking::WinSock::IPV6_UNICAST_IF as i32;

/// getsockopt 的 i32 取值（SO_ERROR/IPV6_V6ONLY 面）。
pub fn getsockopt_i32(s: RawSock, level: i32, opt: i32) -> io::Result<i32> {
    let mut v: i32 = 0;
    #[cfg(unix)]
    let mut len = std::mem::size_of::<i32>() as u32;
    #[cfg(windows)]
    let mut len = std::mem::size_of::<i32>() as i32; // winsock 的 len 参数是 *mut i32
    let rc = unsafe {
        #[cfg(unix)]
        {
            libc::getsockopt(
                s,
                level,
                opt,
                (&mut v as *mut i32).cast(),
                &mut len as *mut u32 as *mut libc::socklen_t,
            )
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::getsockopt(
                s as usize,
                level,
                opt,
                (&mut v as *mut i32).cast(),
                &mut len,
            )
        }
    };
    #[cfg(unix)]
    if rc != 0 {
        return Err(last_sock_err());
    }
    #[cfg(windows)]
    if rc != 0 {
        return Err(last_sock_err());
    }
    Ok(v)
}

/// setsockopt 的 i32 设值。返回是否成功（失败细节由调用面按需取 last_sock_err）。
pub fn setsockopt_i32(s: RawSock, level: i32, opt: i32, val: i32) -> io::Result<()> {
    setsockopt_bytes(s, level, opt, (&val as *const i32).cast(), std::mem::size_of::<i32>())
}

/// setsockopt 的 u32 设值（IP_MULTICAST_IF 的 in_addr / 钉卡 index 面）。
pub fn setsockopt_u32(s: RawSock, level: i32, opt: i32, val: u32) -> io::Result<()> {
    setsockopt_bytes(s, level, opt, (&val as *const u32).cast(), std::mem::size_of::<u32>())
}

fn setsockopt_bytes(s: RawSock, level: i32, opt: i32, ptr: *const u8, len: usize) -> io::Result<()> {
    let rc = unsafe {
        #[cfg(unix)]
        {
            libc::setsockopt(
                s,
                level,
                opt,
                ptr.cast(),
                len as u32,
            )
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::setsockopt(
                s as usize,
                level,
                opt,
                ptr,
                len as i32,
            )
        }
    };
    if rc != 0 {
        return Err(last_sock_err());
    }
    Ok(())
}

/// std `UdpSocket` 的裸句柄（unix fd / windows SOCKET）。
pub fn udp_raw(s: &UdpSocket) -> RawSock {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s)
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
}

/// std `TcpListener` 的裸句柄。
pub fn tcp_listener_raw(s: &std::net::TcpListener) -> RawSock {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s)
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
}

/// std `TcpStream` 的裸句柄。
pub fn tcp_stream_raw(s: &std::net::TcpStream) -> RawSock {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s)
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
}

// ---------- 自唤醒对（self-pipe 的跨平台形态；relay/停止面共用） ----------

/// 自唤醒对：unix = `pipe(2)`（`sysfd::pipe_cloexec`——CLOEXEC 纪律单源）；windows =
/// 回环 TCP 对（bind 127.0.0.1:0 → listen → connect → accept）。语义：写端投一字节
/// ⇒ 读端可读（poll 面）；读端排空见 [`wake_drain`]。
pub struct WakePair {
    pub read: SysSock,
    pub write: SysSock,
}

impl WakePair {
    pub fn new() -> io::Result<Self> {
        #[cfg(unix)]
        {
            let (r, w) = crate::sysfd::pipe_cloexec()?;
            Ok(Self { read: r, write: w })
        }
        #[cfg(windows)]
        {
            use std::net::{TcpListener, TcpStream};
            let ln = TcpListener::bind(("127.0.0.1", 0))?;
            let a = ln.local_addr()?;
            let c1 = TcpStream::connect(a)?;
            let c2 = ln.accept()?.0;
            drop(ln);
            let _ = c1.set_nodelay(true);
            let _ = c2.set_nodelay(true);
            use std::os::windows::io::{FromRawSocket as _, IntoRawSocket, RawSocket};
            let r: RawSocket = c2.into_raw_socket();
            let w: RawSocket = c1.into_raw_socket();
            // SAFETY：into_raw 交出的有效 socket 句柄，所有权移交 SysSock（RAII 关）。
            unsafe {
                Ok(Self {
                    read: std::os::windows::io::OwnedSocket::from_raw_socket(r),
                    write: std::os::windows::io::OwnedSocket::from_raw_socket(w),
                })
            }
        }
    }
}

/// 写端投一个唤醒字节（非阻塞语义：写满/EAGAIN 丢弃——合并位语义，已有待收信号）。
/// **unix 的唤醒对是管道**：socket 系 send(2) 对 pipe fd 报 ENOTSOCK ⇒ 走 write(2)
/// （ENOTSOCK 实测钉——WIN 批）。
pub fn wake_signal(w: RawSock) {
    #[cfg(unix)]
    unsafe {
        let b = b"x";
        let _ = libc::write(w, b.as_ptr().cast(), 1);
    }
    #[cfg(windows)]
    {
        let _ = send(w, b"x");
    }
}

/// 读端排空（读到 EAGAIN/0 为止——事件面清账）。unix = 管道 read(2)（同上）。
pub fn wake_drain(r: RawSock) {
    let mut b = [0u8; 64];
    #[cfg(unix)]
    {
        loop {
            let n = unsafe { libc::read(r, b.as_mut_ptr().cast(), b.len()) };
            if n <= 0 {
                break;
            }
        }
    }
    #[cfg(windows)]
    {
        while let Ok(n) = recv(r, &mut b) {
            if n == 0 {
                break;
            }
        }
    }
}

/// SO_ERROR 取连接结论（0 = 成功；非零 = 连接失败的原始 errno/WSA 码）。
pub fn so_error(s: RawSock) -> io::Result<i32> {
    getsockopt_i32(s, SOL_SOCKET, SO_ERROR)
}

/// 是否「已连接」码（unix EISCONN；windows WSAEISCONN——同义形态）。
pub fn is_eisconn(e: &io::Error) -> bool {
    match e.raw_os_error() {
        #[cfg(unix)]
        Some(libc::EISCONN) => true,
        #[cfg(windows)]
        Some(windows_sys::Win32::Networking::WinSock::WSAEISCONN) => true,
        _ => false,
    }
}

/// `SO_ERROR` 取值面里的 EISCONN 判定值（unix = errno EISCONN；windows 的 SO_ERROR
/// 域是 winsock 码、验收路径不会出现「已连接」值——给不可命中哨兵）。
#[cfg(unix)]
pub const SO_ERROR_EISCONN: i32 = libc::EISCONN;
#[cfg(windows)]
pub const SO_ERROR_EISCONN: i32 = -1;

/// 是否 EAGAIN（unix；windows 上 socket 面的 WouldBlock 走 kind() 判）。
pub fn is_eagain(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::WouldBlock
}

// ---------- 归 wrap（裸句柄 → std 类型） ----------

/// UDP socket 包装（unix = From<OwnedFd>；windows = From<OwnedSocket>）。
pub fn into_udp_socket(s: SysSock) -> UdpSocket {
    #[cfg(unix)]
    {
        UdpSocket::from(s)
    }
    #[cfg(windows)]
    {
        UdpSocket::from(s)
    }
}
