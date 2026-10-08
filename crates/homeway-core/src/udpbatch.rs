//! UDP 批量发送原语（R8-8b：encap/UDP 发送路径优化）。
//!
//! 动机（profile 实证）：bulk 下行/上行 = MTU 1280 小包 × 每包一次 `sendto`——
//! 出口驱动线程 78% 样本在 `__sendto`（2026-10-04 `sample` 实测，与 PERF-AB §8 的
//! 76% 一致）。批量发送把 N 次系统调用折成 1 次（sendmmsg）。
//!
//! 平台分派：
//! - **Linux/OHOS**：`sendmmsg`（libc 面已在 aarch64-unknown-linux-ohos 编译验证）。
//!   分批 ≤64 条/次（栈上 mmsghdr/iovec/sockaddr_storage 数组有界）；短返（非阻塞
//!   下余量 EAGAIN）即停——余量本批丢弃不续发（调用方按成功前缀记账 + 丢弃计数，
//!   见 server/bind.rs 的 tx_dropped）。
//!   EINTR 仅整批未动时整批重试；部分发出后的 EINTR 与 macOS 回退（std send_to
//!   不重试 EINTR）语义不一致——登记差异。
//! - **macOS/其余**：无批量发送系统调用——退化为逐包 `std UdpSocket::send_to`
//!   （与 Go 侧同形态：wireguard-go 在 macOS 亦逐包 sendto ⇒ 同机 A/B 公平）。
//!
//! 线程模型（R1 决议维持）：encap/加密仍在 poll(2) 驱动线程内串行（boringtun
//! `Tunn` 非线程安全、会话 nonce 计数串行），批化只发生在「系统调用收口」——
//! 不引入 encap 卸载线程（拍板记录见 docs/reviews/R8.md）。
//!
//! **fd 所有权约定**：本函数借用 fd（不 close、不 dup）——与驱动线程独占 socket
//! 的既有纪律一致；macOS 回退经 `from_raw_fd/into_raw_fd` 借用同一 fd。

use std::io;

use std::net::Ipv4Addr;
use std::net::SocketAddr;
use std::net::SocketAddrV4;
use std::net::SocketAddrV6;
use std::net::UdpSocket;
#[cfg(not(target_os = "linux"))]
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;

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
/// v4 socket 上该选项报 ENOPROTOOPT ⇒ 非 dual）。
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
/// msg_name 用 sockaddr_in 会 EAFNOSUPPORT——Go 内部 WriteToUDPAddrPort 同义）。
/// 非 dual socket 原样返回（族不匹配的错误如实上报）。
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

/// 单条待发消息：目的端点 + 载荷（载荷借用须存活到本函数返回）。
pub struct OutMsg<'a> {
    pub dst: SocketAddr,
    pub buf: &'a [u8],
}

/// 一次 sendmmsg 的批上限（栈上 mmsghdr+iovec+sockaddr_storage ≈ 64×~200B——有界）。
#[cfg(target_os = "linux")]
const BATCH: usize = 64;

/// 批量发送。返回 (成功包数, 首个失败的 (端点, 错误))——调用方按成功数记账；
/// 失败后的余量不再尝试（非阻塞 socket 的 EAGAIN/不可达类错误对余量同型，
/// 逐包重试只是把同错重复 N 遍）。
///
pub fn send_batch(fd: RawFd, msgs: &[OutMsg<'_>]) -> (usize, Option<(SocketAddr, io::Error)>) {
    if msgs.is_empty() {
        return (0, None);
    }
    #[cfg(target_os = "linux")]
    {
        send_mmsg(fd, msgs)
    }
    #[cfg(not(target_os = "linux"))]
    {
        send_one_by_one(fd, msgs)
    }
}

#[cfg(target_os = "linux")]
fn send_mmsg(fd: RawFd, msgs: &[OutMsg<'_>]) -> (usize, Option<(SocketAddr, io::Error)>) {
    let mut sent_total = 0usize;
    let mut first_err: Option<(SocketAddr, io::Error)> = None;
    let mut off = 0usize;
    'outer: while off < msgs.len() {
        let n = msgs.len().min(off + BATCH) - off;
        let mut iovs = [std::io::IoSlice::new(&[]); BATCH];
        let mut addrs = [std::mem::MaybeUninit::<libc::sockaddr_storage>::uninit(); BATCH];
        let mut hdrs: [libc::mmsghdr; BATCH] = [unsafe { std::mem::zeroed() }; BATCH];
        for i in 0..n {
            let m = &msgs[off + i];
            iovs[i] = std::io::IoSlice::new(m.buf);
            let (ptr, len) = sock_addr_parts(m.dst, &mut addrs[i]);
            hdrs[i].msg_hdr.msg_name = ptr;
            hdrs[i].msg_hdr.msg_namelen = len;
            hdrs[i].msg_hdr.msg_iov = &mut iovs[i] as *mut _ as *mut libc::iovec;
            hdrs[i].msg_hdr.msg_iovlen = 1;
        }
        // 评审 r1-补3 的 Linux 真跑兑现时抓到：sendmmsg 的 flags 形参类型按 libc 面
        // 不同（glibc = c_int；musl 系 = c_uint——musl 与 OHOS 同族；原注释把 musl
        // 归到 c_int 面是错的，转正 A 批 musl 交叉构建实测抓出）——按 target_env 分面
        // cast（OHOS 三段名的 env 段 = "ohos"，target_os 两面都是 "linux"）。
        #[cfg(any(target_env = "ohos", target_env = "musl"))]
        let flags = libc::MSG_DONTWAIT as libc::c_uint;
        #[cfg(not(any(target_env = "ohos", target_env = "musl")))]
        let flags = libc::MSG_DONTWAIT as libc::c_int;
        let r = unsafe { libc::sendmmsg(fd, hdrs.as_mut_ptr(), n as libc::c_uint, flags) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted && sent_total == 0 && off == 0 {
                continue 'outer; // EINTR 且整批未动：整批重试
            }
            first_err = Some((msgs[off].dst, e)); // 首错即弃余量（评审 r1-F8：有意语义）
            break;
        }
        let sent = r as usize;
        sent_total += sent;
        if sent < n {
            break; // 短返：非阻塞余量 EAGAIN——余量留给下一拍
        }
        off += n;
    }
    (sent_total, first_err)
}

/// 端点 → sockaddr_storage（手写换算：std 无公开 API；v4/v6 双形）。
#[cfg(target_os = "linux")]
fn sock_addr_parts(
    dst: SocketAddr,
    storage: &mut std::mem::MaybeUninit<libc::sockaddr_storage>,
) -> (*mut libc::c_void, libc::socklen_t) {
    unsafe {
        match dst {
            SocketAddr::V4(v4) => {
                let sin = libc::sockaddr_in {
                    sin_family: libc::AF_INET as _,
                    sin_port: v4.port().to_be(),
                    sin_addr: libc::in_addr {
                        s_addr: u32::from_ne_bytes(v4.ip().octets()),
                    },
                    sin_zero: [0; 8],
                };
                storage.write(std::mem::zeroed());
                std::ptr::copy_nonoverlapping(
                    std::ptr::addr_of!(sin).cast::<u8>(),
                    storage.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of::<libc::sockaddr_in>(),
                );
                (
                    storage.as_mut_ptr().cast(),
                    std::mem::size_of::<libc::sockaddr_in>() as _,
                )
            }
            SocketAddr::V6(v6) => {
                let sin6 = libc::sockaddr_in6 {
                    sin6_family: libc::AF_INET6 as _,
                    sin6_port: v6.port().to_be(),
                    sin6_addr: libc::in6_addr {
                        s6_addr: v6.ip().octets(),
                    },
                    sin6_flowinfo: v6.flowinfo(),
                    sin6_scope_id: v6.scope_id(),
                };
                storage.write(std::mem::zeroed());
                std::ptr::copy_nonoverlapping(
                    std::ptr::addr_of!(sin6).cast::<u8>(),
                    storage.as_mut_ptr().cast::<u8>(),
                    std::mem::size_of::<libc::sockaddr_in6>(),
                );
                (
                    storage.as_mut_ptr().cast(),
                    std::mem::size_of::<libc::sockaddr_in6>() as _,
                )
            }
        }
    }
}

/// 逐包回退（非 Linux 平台默认；Linux/OHOS 走 sendmmsg 批量）：借用 fd 包
/// 一层 std UdpSocket，逐包 send_to（v4/v6 双栈）。
#[cfg(not(target_os = "linux"))]
fn send_one_by_one(fd: RawFd, msgs: &[OutMsg<'_>]) -> (usize, Option<(SocketAddr, io::Error)>) {
    // 借用形态：from_raw_fd 后立即在 drop 前换回——fd 生命周期不变。
    let sock = unsafe { UdpSocket::from_raw_fd(fd) };
    let mut sent = 0usize;
    let mut first_err = None;
    for m in msgs {
        match sock.send_to(m.buf, m.dst) {
            Ok(_) => sent += 1,
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some((m.dst, e));
                }
                break; // 余量同型错误——不重试（与 send_mmsg 短返口径一致）
            }
        }
    }
    let _ = std::os::fd::IntoRawFd::into_raw_fd(sock); // 归还借用
    (sent, first_err)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::os::fd::AsRawFd as _;

    /// 双端回环：批量发 N 包逐包到达（Linux 形态经 sendmmsg；macOS 回退路径同断言）。
    #[test]
    fn batch_reaches_receiver_in_order() {
        let rx = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let dst = rx.local_addr().unwrap();
        let tx = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        tx.set_nonblocking(true).unwrap();
        // 收侧放大缓冲，防内核 RCVBUF 丢包（批量突发）
        unsafe {
            let sz: libc::c_int = 4 * 1024 * 1024;
            let _ = libc::setsockopt(
                rx.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &sz as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
        }
        let payloads: Vec<Vec<u8>> = (0..80usize).map(|i| vec![i as u8; 64]).collect();
        let msgs: Vec<OutMsg<'_>> = payloads
            .iter()
            .map(|p| OutMsg {
                dst,
                buf: p.as_slice(),
            })
            .collect();
        let (sent, err) = send_batch(tx.as_raw_fd(), &msgs);
        assert_eq!(sent, 80, "应全量发出（首错 = {err:?}）");
        // 超批（>64）分批语义由 sent==80 钉死
        let mut got = Vec::new();
        rx.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut buf = [0u8; 128];
        while got.len() < 80 {
            match rx.recv_from(&mut buf) {
                Ok((n, _)) => {
                    assert_eq!(n, 64);
                    got.push(buf[0]);
                }
                Err(_) => break,
            }
        }
        assert_eq!(got.len(), 80, "应收满 80 包（实收 {}）", got.len());
        assert_eq!(got, (0..80u8).collect::<Vec<_>>(), "顺序到达");
    }
}
