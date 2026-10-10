//! UDP socket 族工具（出口/客户端共用的双栈建连/收发适配面）。
//!
//! 模块史：原名「UDP 批量发送原语」（R8-8b，sendmmsg 批化），其消费者
//! （`wtransport`/`server/bind` 的 WG 发送面）已随 M5 删码退役，sendmmsg 族
//! （`send_batch`/`OutMsg`/`sock_addr_parts`/`send_one_by_one`）于 2026-10-10
//! 全面审查后删除（约 180 行死件）。现存 = socket 族工具：双栈/单栈 bind、
//! 双栈检测、v4-mapped 适配与归一、客户端 socket 统一面——`homeway-quic` 的
//! `exit/socket.rs` 有同款复刻（该 crate 是叶子、不得依赖本 crate）。
//! 模块名保留 `udpbatch`（历史名，改名的 import 面扰动不值当）。

use std::io;

use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::SocketAddrV6;
use std::net::UdpSocket;

// ---------- socket 族工具（出口/客户端共用的双栈收发面） ----------

/// 双栈 UDP socket（AF_INET6 + `IPV6_V6ONLY=0`——Go `net.ListenUDP("udp", …)` 同义：
/// v4/v6 对端都能通、同 socket 的 STUN 观测对两族都成立）。V6ONLY 必须在 bind 前
/// 设（bind 后再设无效），std 的 `UdpSocket::bind` 一体化创建插不进 setsockopt ⇒
/// libc 手建。
pub(crate) fn bind_dual_stack(port: u16) -> io::Result<UdpSocket> {
    // F1：CLOEXEC 创建即落（linux/OHOS 走 `SOCK_CLOEXEC` 原子位；darwin 建后立即补）
    // ——失败路径 fd 随 OwnedFd drop 关，无需手写 close。
    let fd = crate::sysfd::socket_cloexec(libc::AF_INET6, libc::SOCK_DGRAM, 0)?;
    let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
    unsafe {
        let off: libc::c_int = 0;
        let _ = libc::setsockopt(
            raw,
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            &off as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as u32,
        );
        let mut sin6: libc::sockaddr_in6 = std::mem::zeroed();
        sin6.sin6_family = libc::AF_INET6 as _;
        sin6.sin6_port = port.to_be();
        sin6.sin6_addr = libc::in6_addr { s6_addr: [0; 16] };
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            sin6.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
        }
        let r = libc::bind(
            raw,
            &sin6 as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
        );
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(UdpSocket::from(fd))
}

/// v6 **单栈** UDP socket（IP 字面量绑定的 v6 形态——Go `ListenUDP("udp6", …)` 显式
/// `IPV6_V6ONLY=1` 同义：std bind 的 v6 socket 用系统默认（多为 0），须显式设 1）。
pub(crate) fn bind_v6_only(ip: std::net::Ipv6Addr, port: u16) -> io::Result<UdpSocket> {
    // F1：CLOEXEC 创建即落（分派同 bind_dual_stack）。
    let fd = crate::sysfd::socket_cloexec(libc::AF_INET6, libc::SOCK_DGRAM, 0)?;
    let raw = std::os::fd::AsRawFd::as_raw_fd(&fd);
    unsafe {
        let on: libc::c_int = 1;
        let _ = libc::setsockopt(
            raw,
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            &on as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as u32,
        );
        let mut sin6: libc::sockaddr_in6 = std::mem::zeroed();
        sin6.sin6_family = libc::AF_INET6 as _;
        sin6.sin6_port = port.to_be();
        sin6.sin6_addr = libc::in6_addr { s6_addr: ip.octets() };
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        {
            sin6.sin6_len = std::mem::size_of::<libc::sockaddr_in6>() as u8;
        }
        let r = libc::bind(
            raw,
            &sin6 as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
        );
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(UdpSocket::from(fd))
}

/// socket 是否为 v6 双栈（AF_INET6 且 `IPV6_V6ONLY=0`——getsockopt 运行期检测；
/// v4 socket 上该选项报 ENOPROTOOPT ⇒ 非 dual）。`homeway-quic` 的 `exit/socket.rs` 有
/// 同款复刻（该 crate 是叶子、不得依赖本 crate）。
pub(crate) fn is_dual_stack(sock: &UdpSocket) -> bool {
    use std::os::fd::AsRawFd as _;
    unsafe {
        let mut v: libc::c_int = 1;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let r = libc::getsockopt(
            sock.as_raw_fd(),
            libc::IPPROTO_IPV6,
            libc::IPV6_V6ONLY,
            &mut v as *mut _ as *mut libc::c_void,
            &mut len,
        );
        r == 0 && v == 0
    }
}

/// 发送目标的族适配：双栈 socket 发 v4 目标须 map 成 v4-mapped（AF_INET6 的
/// msg_name 用裸 sockaddr_in 会 EINVAL——Darwin 实测 errno 22；Linux/OHOS 内核自行转成
/// v4-mapped 故不现形——Go 内部 WriteToUDPAddrPort 同义）。
/// 非 dual socket 原样返回（族不匹配的错误如实上报）。
/// `homeway-quic` 的 `exit/socket.rs` 有同款复刻（该 crate 是叶子、不得依赖本 crate）。
pub(crate) fn xmit_addr(ep: SocketAddr, dual: bool) -> SocketAddr {
    if !dual {
        return ep;
    }
    match ep {
        SocketAddr::V4(v4) => {
            SocketAddr::V6(SocketAddrV6::new(v4.ip().to_ipv6_mapped(), v4.port(), 0, 0))
        }
        v6 => v6,
    }
}

/// v4-mapped v6 源地址归一成纯 v4（Go `Is4In6 → Unmap` 同义；非 mapped 形态原样）。
pub(crate) fn unmap_v4_in6(a: SocketAddr) -> SocketAddr {
    match a {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::V4(SocketAddrV4::new(v4, v6.port())),
            None => a,
        },
        v4 => v4,
    }
}

/// 客户端/出口建 UDP socket 的统一面：双栈优先（v6 候选可发、v4 对端可达），无 v6
/// 环境回退 v4（Go 在纯 v4 平台同样回落 AF_INET）。返回 (socket, 是否双栈)。
pub(crate) fn open_client_socket() -> io::Result<(UdpSocket, bool)> {
    match bind_dual_stack(0) {
        // dual 以 getsockopt 运行期事实为准（r1-L4：V6ONLY setsockopt 被环境
        // 忽略时会得到「自认双栈的单栈 socket」——v4 发送面全 EINVAL）
        Ok(s) => {
            let dual = is_dual_stack(&s);
            Ok((s, dual))
        }
        Err(_) => {
            let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
            Ok((s, false))
        }
    }
}
