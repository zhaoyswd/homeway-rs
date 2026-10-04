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
use std::net::SocketAddr;
use std::net::UdpSocket;
use std::os::fd::FromRawFd;
use std::os::fd::RawFd;

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
/// R8-2 归因消融：环境变量 `HOMEWAY_UDP_NO_BATCH`（**设非空即启用**，presence-only
/// ——与 HOMEWAY_WG_DEBUG 同惯例；注释曾写 `=1` 属口径偏差，评审 r1-F7）强制走逐包
/// 回退路径（Linux 上对照 sendmmsg 批量 vs 逐包的收益差——不设即平台默认，产品
/// 行为不变）。
pub fn send_batch(fd: RawFd, msgs: &[OutMsg<'_>]) -> (usize, Option<(SocketAddr, io::Error)>) {
    if msgs.is_empty() {
        return (0, None);
    }
    #[cfg(target_os = "linux")]
    {
        static NO_BATCH: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if *NO_BATCH.get_or_init(|| std::env::var_os("HOMEWAY_UDP_NO_BATCH").is_some()) {
            return send_one_by_one(fd, msgs);
        }
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
        // 评审 r1-补3 的 Linux 真跑兑现时抓到：sendmmsg 的 flags 形参类型两面不同
        // （glibc/musl = c_int，OHOS libc 面 = c_uint）——R8-1 只在 OHOS 面编译验证
        // 过，桌面/服务器 Linux（gnu）一直编不过。按 target_env 分面 cast（OHOS
        // 三段名的 env 段 = "ohos"，target_os 两面都是 "linux"）。
        #[cfg(target_env = "ohos")]
        let flags = libc::MSG_DONTWAIT as libc::c_uint;
        #[cfg(not(target_env = "ohos"))]
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

/// 逐包回退（macOS 平台默认；Linux 消融臂 = HOMEWAY_UDP_NO_BATCH=1）：借用 fd 包
/// 一层 std UdpSocket，逐包 send_to（v4/v6 双栈）。
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
