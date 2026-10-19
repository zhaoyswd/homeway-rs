//! 平台 io 底座（WIN 批 2026-10-13；**叶子 crate 的同款复刻**——与
//! `homeway-core::sysock` 平行，本 crate 不得依赖 core，复刻先例 =
//! `exit/socket.rs` ↔ `core::udpbatch`）。
//!
//! 形态：unix 臂 = libc 直通（与改前逐调用同形）；windows 臂 = winsock
//! （`WSASocketW`/`WSAPoll`/`ioctlsocket`）。winsock 取错经 `WSAGetLastError`
//! （`io::Error::last_os_error` 在 socket 调用后不可靠）。

use std::io;

// ---------- 服务承载流别名（unix = UDS / windows = 回环 TCP） ----------

/// 服务承载流：服务流泵（`exit/pump.rs`）的 socketpair 两端与 intake 队列的元素类型。
/// unix = `UnixStream`（`UnixStream::pair()`）；windows = 回环 TCP 对（`pair()`——
/// AF_UNIX 无 std 面，回环对语义等价：字节流 + 双向 + 半关）。
#[cfg(unix)]
pub type SvcStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
pub type SvcStream = std::net::TcpStream;

/// 建一对同型流（unix = `UnixStream::pair`；windows = 回环 TCP 对）。
pub fn pair() -> io::Result<(SvcStream, SvcStream)> {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::pair()
    }
    #[cfg(windows)]
    {
        let ln = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let a = ln.local_addr()?;
        let c1 = std::net::TcpStream::connect(a)?;
        let c2 = ln.accept()?.0;
        drop(ln);
        // 评审 M9：关 Nagle（承载对要的是字节流管道语义——与 core::sysock::WakePair
        // 的回环对同款；不关会让小帧路径凑包）。
        let _ = c1.set_nodelay(true);
        let _ = c2.set_nodelay(true);
        Ok((c2, c1))
    }
}

// ---------- 裸句柄与 sockopt ----------

#[cfg(unix)]
pub type RawIo = std::os::fd::RawFd;
/// windows 的裸句柄（`RawSocket` 的 u64 宽度；winsock API 的 SOCKET 收窄 cast——
/// 同 64 位进程内两种宽度等值，收窄不丢信息）。
#[cfg(windows)]
pub type RawIo = u64;

pub fn udp_raw(s: &std::net::UdpSocket) -> RawIo {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s)
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
}

pub fn svc_raw(s: &SvcStream) -> RawIo {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s)
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
}

#[allow(dead_code)] // windows 臂消费（unix 构建静音）
pub fn tcp_stream_raw(s: &std::net::TcpStream) -> RawIo {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s)
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
}

#[allow(dead_code)] // windows 臂消费（unix 构建静音）
pub fn tcp_listener_raw(s: &std::net::TcpListener) -> RawIo {
    #[cfg(unix)]
    {
        std::os::fd::AsRawFd::as_raw_fd(s)
    }
    #[cfg(windows)]
    {
        std::os::windows::io::AsRawSocket::as_raw_socket(s)
    }
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

pub fn setsockopt_i32(s: RawIo, level: i32, opt: i32, val: i32) -> io::Result<()> {
    let r = unsafe {
        #[cfg(unix)]
        {
            libc::setsockopt(s, level, opt, std::ptr::addr_of!(val).cast(), 4)
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::setsockopt(
                s as usize,
                level,
                opt,
                std::ptr::addr_of!(val).cast(),
                4,
            )
        }
    };
    if r != 0 {
        return Err(last_sock_err());
    }
    Ok(())
}

pub fn getsockopt_i32(s: RawIo, level: i32, opt: i32) -> io::Result<i32> {
    let mut v: i32 = 0;
    let mut len = 4u32;
    let r = unsafe {
        #[cfg(unix)]
        {
            libc::getsockopt(s, level, opt, std::ptr::addr_of_mut!(v).cast(), &mut len)
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::getsockopt(
                s as usize,
                level,
                opt,
                std::ptr::addr_of_mut!(v).cast(),
                &mut len as *mut u32 as *mut i32,
            )
        }
    };
    if r != 0 {
        return Err(last_sock_err());
    }
    Ok(v)
}

// ---------- poll ----------

#[cfg(unix)]
pub use libc::{POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT};
#[cfg(windows)]
pub use windows_sys::Win32::Networking::WinSock::{POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT};

/// 布局钉（评审 M5，与 core::sysock 同钉）：unix 臂整片 reinterpret 成
/// `libc::pollfd`——repr(C) + 编译期布局断言。
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollEnt {
    pub raw: RawIo,
    pub events: i16,
    pub revents: i16,
}
#[cfg(unix)]
const _: () = {
    assert!(std::mem::size_of::<PollEnt>() == std::mem::size_of::<libc::pollfd>());
    assert!(std::mem::align_of::<PollEnt>() == std::mem::align_of::<libc::pollfd>());
    assert!(std::mem::offset_of!(PollEnt, raw) == std::mem::offset_of!(libc::pollfd, fd));
    assert!(std::mem::offset_of!(PollEnt, events) == std::mem::offset_of!(libc::pollfd, events));
    assert!(std::mem::offset_of!(PollEnt, revents) == std::mem::offset_of!(libc::pollfd, revents));
};

/// 带超时 poll（毫秒）。Err = 系统调用失败（含 Interrupted——调用方按需重试）。
pub fn poll(ents: &mut [PollEnt], timeout_ms: i32) -> io::Result<i32> {
    #[cfg(unix)]
    unsafe {
        let rc = libc::poll(ents.as_mut_ptr().cast(), ents.len() as libc::nfds_t, timeout_ms);
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
            .map(|e| WSAPOLLFD { fd: e.raw as usize, events: e.events, revents: e.revents })
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

// ---------- 裸 fd 读写/非阻塞（tun 面：句柄来自外部，只做 io 不做所有权） ----------

/// 非阻塞位切换（unix = fcntl O_NONBLOCK；windows = ioctlsocket FIONBIO——句柄按
/// socket 处理：tun 面在 windows 无运行期来源，仅编译面保通）。
pub fn set_nonblocking(raw: RawIo, on: bool) -> io::Result<()> {
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
        let mut mode: u32 = u32::from(on);
        let rc = unsafe {
            windows_sys::Win32::Networking::WinSock::ioctlsocket(
                raw as usize,
                windows_sys::Win32::Networking::WinSock::FIONBIO,
                &mut mode,
            )
        };
        if rc != 0 {
            return Err(last_sock_err());
        }
        Ok(())
    }
}

/// 裸句柄读（unix = read(2)；windows = recv——winsock socket 上 recv 即读）。
pub fn read(raw: RawIo, buf: &mut [u8]) -> io::Result<usize> {
    #[cfg(unix)]
    unsafe {
        let n = libc::read(raw, buf.as_mut_ptr().cast(), buf.len());
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
    #[cfg(windows)]
    {
        recv(raw, buf)
    }
}

/// 裸句柄写（unix = write(2)；windows = send）。
pub fn write(raw: RawIo, buf: &[u8]) -> io::Result<usize> {
    #[cfg(unix)]
    unsafe {
        let n = libc::write(raw, buf.as_ptr().cast(), buf.len());
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(n as usize)
    }
    #[cfg(windows)]
    {
        send(raw, buf)
    }
}

/// 裸句柄 recv（unix = recv(2)；windows = winsock recv）。
#[allow(dead_code)] // windows 臂消费（unix 构建静音）
pub fn recv(raw: RawIo, buf: &mut [u8]) -> io::Result<usize> {
    let r = unsafe {
        #[cfg(unix)]
        {
            libc::recv(raw, buf.as_mut_ptr().cast(), buf.len(), 0)
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::recv(raw as usize, buf.as_mut_ptr(), buf.len() as i32, 0)
        }
    };
    #[cfg(unix)]
    if r < 0 {
        return Err(last_sock_err());
    }
    #[cfg(windows)]
    if r == windows_sys::Win32::Networking::WinSock::SOCKET_ERROR {
        return Err(last_sock_err());
    }
    Ok(r as usize)
}

/// 裸句柄 send。unix 带 `MSG_NOSIGNAL`——与 `core::sysock::send` 同语义对齐
/// （评审 M6：修前两份同型助手在一批内语义分叉）；windows 无 SIGPIPE 概念，
/// flags=0 等价。（unix 构建下仅 `write()` 的 windows 臂间接可达——静音。）
#[allow(dead_code)]
pub fn send(raw: RawIo, buf: &[u8]) -> io::Result<usize> {
    #[cfg(unix)]
    let flags = libc::MSG_NOSIGNAL;
    #[cfg(windows)]
    let flags = 0;
    let r = unsafe {
        #[cfg(unix)]
        {
            libc::send(raw, buf.as_ptr().cast(), buf.len(), flags)
        }
        #[cfg(windows)]
        {
            windows_sys::Win32::Networking::WinSock::send(raw as usize, buf.as_ptr(), buf.len() as i32, flags)
        }
    };
    #[cfg(unix)]
    if r < 0 {
        return Err(last_sock_err());
    }
    #[cfg(windows)]
    if r == windows_sys::Win32::Networking::WinSock::SOCKET_ERROR {
        return Err(last_sock_err());
    }
    Ok(r as usize)
}



/// 非阻塞 `sendto`（unix 裸调用；Windows 走 `tokio::net::UdpSocket::try_send_to`）。
///
/// 为什么不用 tokio 的 `try_send_to`：macOS 上**新建的 v6 socket 首次发送**会返回
/// WouldBlock（route/绑定惰性完成），而 tokio 的写就绪不保证再发一次唤醒 ⇒ quinn 的
/// 首包可能永久等不到重试。裸 `sendto` 在非阻塞 fd 上首次即可成功（实测 std 同形）。
#[cfg(unix)]
pub fn send_to(raw: RawIo, buf: &[u8], dst: std::net::SocketAddr) -> io::Result<usize> {
    let (sa, len) = pack_addr(dst);
    let r = unsafe {
        libc::sendto(
            raw,
            buf.as_ptr().cast(),
            buf.len(),
            libc::MSG_NOSIGNAL,
            sa.as_ptr().cast(),
            len as libc::socklen_t,
        )
    };
    if r < 0 {
        Err(last_sock_err())
    } else {
        Ok(r as usize)
    }
}

/// `SocketAddr` → wire 形态的 sockaddr（unix）。
#[cfg(unix)]
fn pack_addr(a: std::net::SocketAddr) -> ([u8; 128], usize) {
    use std::net::SocketAddr;
    let mut out = [0u8; 128];
    let n = match a {
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
            unsafe {
                out[..n].copy_from_slice(std::slice::from_raw_parts(
                    (&s as *const libc::sockaddr_in6).cast(),
                    n,
                ));
            }
            n
        }
    };
    (out, n)
}

// ---------- UDP 地址族（双栈 socket 的发送归一 / 接收还原） ----------

/// 发送目标归一：dual 时裸 v4 → v4-mapped（v4-mapped / 纯 v6 原样）；非 dual 原样。
///
/// 语义与 `homeway-core::udpbatch::xmit_addr` 对齐（本 crate 是叶子，不得依赖 core）。
/// 使用者 = **出口**的双栈直连 socket（`exit/socket.rs`）；客户端每个端点只装一族，
/// 不做映射。
pub fn xmit_addr(dst: std::net::SocketAddr, dual: bool) -> std::net::SocketAddr {
    use std::net::{SocketAddr, SocketAddrV6};
    if !dual {
        return dst;
    }
    match dst {
        SocketAddr::V4(v4) => {
            SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0))
        }
        v6 => v6,
    }
}

/// 接收还原：v4-mapped v6 → v4（其余原样）。quinn 的路径远端 = 候选形态；v4 候选
/// 在双栈 socket 上收到的是 v4-mapped，不还原会与候选地址对不上。
pub fn unmap_v4_mapped(addr: std::net::SocketAddr) -> std::net::SocketAddr {
    use std::net::{SocketAddr, SocketAddrV4};
    match addr {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::V4(SocketAddrV4::new(v4, v6.port())),
            None => addr,
        },
        v4 => v4,
    }
}

/// socket 是否 v6 双栈（AF_INET6 且 `IPV6_V6ONLY=0`；v4 socket 上 ENOPROTOOPT ⇒ false）。
pub fn is_dual_stack(sock: &std::net::UdpSocket) -> bool {
    matches!(
        getsockopt_i32(udp_raw(sock), IPPROTO_IPV6, IPV6_V6ONLY),
        Ok(0)
    )
}

// ---------- 发送错误的「本地不可达族」errno 值（windows = WSAE* 值域） ----------

/// `classify_send_err` 的本地族白名单（unix = errno；windows = WSA errno——数值上
/// winsock 沿用 BSD 面并加 10000 偏移）。
pub fn is_local_net_err(e: &io::Error) -> bool {
    let code = e.raw_os_error();
    let codes: &[i32] = {
        #[cfg(unix)]
        {
            &[
                libc::ENETUNREACH,
                libc::EHOSTUNREACH,
                libc::EADDRNOTAVAIL,
                libc::ENETDOWN,
                libc::EINVAL,
            ]
        }
        #[cfg(windows)]
        {
            // WSAENETUNREACH=10051 / WSAEHOSTUNREACH=10065 / WSAEADDRNOTAVAIL=10049 /
            // WSAENETDOWN=10050 / WSAEINVAL=10022（send 参数错形态）。
            &[10051, 10065, 10049, 10050, 10022]
        }
    };
    code.is_some_and(|c| codes.contains(&c))
}

/// `classify_send_err` 的**系统禁发族**：进程/UID 级网络拒绝（`EPERM`/`EACCES`）。
///
/// 与 [`is_local_net_err`] 的语义相反：本地族换 socket 可能救；禁发族换 socket/新连接
/// 都在同一权限面，M/R 只会产生毫秒级瞬败（2026-10-19 现场：13 轮 M/R 全败）。
pub fn is_denied_net_err(e: &io::Error) -> bool {
    let code = e.raw_os_error();
    let codes: &[i32] = {
        #[cfg(unix)]
        {
            &[libc::EPERM, libc::EACCES]
        }
        #[cfg(windows)]
        {
            // ERROR_ACCESS_DENIED=5 / WSAEACCES=10013。
            &[5, 10013]
        }
    };
    code.is_some_and(|c| codes.contains(&c))
}
