//! 拦截层（R3；语义真源 `pkg/intercept`；执行结构 = 单线程 reactor——
//! `docs/reviews/reactor-design.md` v2）。
//!
//! 挂在出口隧道侧栈上，把「WG 解密后的明文 IP 包」按目的地址分流（tun2socks 同款语义，
//! smoltcp 形态 = 包级 NAT 重写——见 `nat.rs` 头注释）：
//!
//!    dst == 隧道IP → 豁免：LocalServices 命中端口转投 UDS、其余回环同端口重拨
//!    dst == 其它   → 过境：终结（栈内 TCP 状态机）+ 本机 socket 重拨
//!
//! **拨号先行**（设计 §4.1 / 评审 H2）：TCP SYN 不立即回 SYN-ACK——先建映射缓存 SYN、
//! 非阻塞 connect upstream 成功后才建栈内 socket 注入缓存（SYN-ACK 由此产生）；失败
//! 构造 RST 回客户端。三态（建立时点/失败可见性/黑洞 10s）与 Go（Forwarder 先拨号后
//! CreateEndpoint）等价。
//!
//! **单线程 reactor**（2026-10-07 简化批）：重拨 OS socket 不再有 worker 池——每流
//! 一个 `ReactorIo`（读/写兴趣位 + 待写缓冲），`pump()` 内 `poll(2, timeout=0)` 自查
//! 就绪（就绪集是提示不是契约：fd 全非阻塞 + poll 电平触发，漏看下拍重报、误看读出
//! EAGAIN 自然跳过）。无锁无跨线程流队列——R3 池族三 bug（fd 属主表泄漏/Adopt 时序/
//! 池形态）、Written 差额补报、Ack 短返清账、收工 Closed 回执竞态、worker 饿死族的
//! 代码路径整体不存在。

pub mod dnsface;
pub mod nat;
pub mod reasm;

use std::collections::{HashMap, VecDeque};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4};
use std::os::fd::{AsFd as _, AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smoltcp::iface::{Config as IfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::socket::udp::{self, Socket as UdpSocket};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpCidr, IpEndpoint, Ipv4Address};

use crate::wgcore::stackb::TunDevice;
use crate::Logf;

use self::dnsface::{DnsFaces, DnsRoute};
use self::nat::Ipv4View;
use self::reasm::{DropReason, Dropped, Reassembler};
use crate::server::dnsproxy::{DnsProxy, DnsReply, SubmitOutcome};

/// TCP 并发上限（serve 装配层覆盖包内默认 4096——FIX-63 生产值）。
pub const MAX_CONNS: usize = 1024;
/// TCP 空闲回收（生产值；豁免腿上的长会话别收太短）。
pub const TCP_IDLE: Duration = Duration::from_secs(5 * 60);
/// UDP 会话空闲回收（与手机侧空闲回收对齐）。
pub const UDP_IDLE: Duration = Duration::from_secs(60);
/// DNS :53 会话的短回收（一问一答即闲，防挤占会话表）。
pub const DNS_IDLE: Duration = Duration::from_secs(10);
/// TCP DNS 腿的每消息空闲期限（Go ServeStream 的 tcpIdle=30s——挂住不发的客户端
/// 不能无限占连接；UDP 腿仍用 DNS_IDLE=10s）。
const TCP_DNS_IDLE: Duration = Duration::from_secs(30);
/// TCP DNS 腿并发上限（Go MaxTCPConns=64——connreg 与隧道内 listener 共表；Rust
/// 拦截腿与隧道面各自 64，合计边界差异登记）。
const MAX_TCP_DNS_LEGS: usize = 64;
/// UDP 会话上限（保险阀）。
pub const MAX_UDP_SESSIONS: usize = 4096;
/// DNS 待答路由表在途量告警阈值（F2/[门-A4] 观测面：正常随应答回投归零；持续高于
/// 本阈值 = 回执回收失灵——丢弃路径漏回收 tag / 回投通道无人 drain）。**低于
/// `MAX_IN_FLIGHT=256`**（评审 L2：取 256 时「worker panic ⇒ in_flight 有界滞留」
/// 这条残余永远到不了阈值，信号面失效）。
const DNS_PENDING_WARN: usize = 128;
/// 单流栈→upstream 待写**数据报条数**上限（F3：UDP 语义丢新+计数——不阻塞不排队）。
/// 64 = 与栈内 socket 的 tx 元数据槽数同量级；字节上限复用 `WATERMARK`。
const MAX_OUT_UDP_PKTS: usize = 64;
/// UDP 栈→upstream **读侧门**阈值（F3/[门-B7]）。**严格低于字节上限 `WATERMARK`**：
/// 达阈值即停读该流（栈 rx 缓冲填满 ⇒ 栈层丢新），使门成为真正的背压面；`WATERMARK`
/// 上限则拦「单拍读尽」造成的越界。两者同阈值时上限会先拦、门成死代码（评审 M3）。
const UDP_OUT_GATE: usize = WATERMARK / 2;
// 门阈值须严格低于字节上限（评审 M3 的编译期守卫）。
const _: () = assert!(UDP_OUT_GATE < WATERMARK);
/// 非阻塞 connect 死线（Go dialTimeout 同值；reap_idle 先判）。
const DIAL_TIMEOUT: Duration = Duration::from_secs(10);
/// upstream 读块大小（旧 worker 的 Go bufSize 同值）。
const READ_CHUNK: usize = 64 * 1024;
/// 栈内 socket 接收缓冲（过境 TCP：通告窗口面——有意放宽 vs Go rcvWnd=4096，登记差异表）。
const FLOW_BUF: usize = 256 * 1024;
/// 栈内 socket 发送缓冲（吞吐面：发端每 RTT 能维持的在途字节——对端（客户端）
/// smoltcp 延迟 ACK ~25ms ⇒ 256KB 只能维持 ~80Mbps；1MB 对齐客户端通告窗
/// （Go gVisor 无此上限）【2026-10-02 实测：20MB 下载 41.7s→2.6s】。
const FLOW_TX_BUF: usize = 1024 * 1024;
/// 背压高水位（per-flow 未确认字节；双向）。
const WATERMARK: usize = 256 * 1024;
/// 建连窗口的 SYN/重复包缓存上限。
const SYN_CACHE_MAX: usize = 4;
/// 拨号失败降噪键（kind + 原始目的；源端点不进键——见 dial_fail_seen 注释）。
type DialFailKey = (&'static str, (Ipv4Addr, u16));
/// 非阻塞 connect 的在途形态。
enum ConnState {
    /// EINPROGRESS/EALREADY：POLLOUT/POLLERR/POLLHUP 任一 → SO_ERROR 验收。
    InProgress,
    /// EAGAIN（Linux AF_UNIX 监听队列满）：每拍主动重拨 connect（不挂 poll 位——
    /// 未连接 socket 恒可写，POLLOUT 等不到有用信号且可能 SO_ERROR=0 误验收）。
    Retry,
}

/// 单流的重拨 OS socket 面（reactor 所有——驱动线程独占，无锁）。
struct ReactorIo {
    /// 类型承担不变量：drop 恰关一次。无 OS socket（DNS 进程内腿）= Flow.io 为 None。
    fd: OwnedFd,
    udp: bool,
    /// 栈→upstream 字节流待写（TCP/UDS；前缀消费——部分写余量续传）。
    out_tcp: VecDequeLite,
    /// 栈→upstream 数据报队列（UDP；整取整发——帧界显式，不与字节流混用）。
    out_udp: VecDeque<Vec<u8>>,
    /// 非阻塞 connect 在途（None = 已就绪）。
    conn: Option<ConnState>,
    /// connect 死线（Go dialTimeout 10s 同值；reap_idle 先判）。
    dial_deadline: Instant,
    /// upstream EOF：待写缓冲排空后才关 fd（尾数据不丢——对齐旧 worker「排空即收
    /// fd、流记录留栈侧收尾」的时点语义）。
    dead: bool,
}

/// upstream 的拨号目标（豁免/过境/DNS 的建流决策面）。
enum DialTarget {
    /// 本机 TCP（transit；exempt 未命中 LocalServices 时的回环同端口）。
    Tcp(std::net::SocketAddr),
    /// LocalServices UDS（files/term/speedtest）。
    Unix(String),
    /// 本机已连接 UDP（transit/exempt 的 UDP 重拨）。
    Udp(std::net::SocketAddr),
}

/// 兴趣位纯函数（reactor-design §二——单测面）：`backlog` = 该流 tx_backlog 现存
/// 字节（下行背压门控）。`None` = 本拍不注册该 fd。
fn interests_for(io: &ReactorIo, backlog: usize) -> Option<libc::c_short> {
    match io.conn {
        Some(ConnState::InProgress) => Some(libc::POLLOUT),
        Some(ConnState::Retry) => None, // 每拍主动重拨，无兴趣位
        None => {
            if io.dead && io.out_tcp.remaining().is_empty() && io.out_udp.is_empty() {
                return None; // 排空即收的窗口（下一拍已不在表内）
            }
            let mut ev = 0;
            if !io.dead && backlog < WATERMARK {
                ev |= libc::POLLIN;
            }
            if !io.out_tcp.remaining().is_empty() || !io.out_udp.is_empty() {
                ev |= libc::POLLOUT;
            }
            Some(ev)
        }
    }
}

/// 简洁起见用 Vec 做字节写缓冲（块级追加；头部消费）。
///
/// Q-I 尾段起 `pub(crate)`：`files` 客户端 `Stream.buf`（F6.2）复用同一偏移式实现
/// （消 `Vec::drain` 尾部整搬；摊还压缩阈值见 `consume`）。
pub(crate) struct VecDequeLite {
    buf: Vec<u8>,
    off: usize,
}

impl VecDequeLite {
    pub(crate) fn new() -> Self {
        Self { buf: Vec::new(), off: 0 }
    }
    /// 预分配形态（files 客户端沿用旧 `Vec::with_capacity(16KiB)` 的分配预期）。
    pub(crate) fn with_capacity(n: usize) -> Self {
        Self { buf: Vec::with_capacity(n), off: 0 }
    }
    pub(crate) fn push(&mut self, data: &[u8]) {
        if self.off > 0 && self.off == self.buf.len() {
            self.buf.clear();
            self.off = 0;
        }
        self.buf.extend_from_slice(data);
    }
    pub(crate) fn remaining(&self) -> &[u8] {
        &self.buf[self.off..]
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.off >= self.buf.len()
    }
    pub(crate) fn consume(&mut self, n: usize) {
        // Q-I 代码门 L1：消费量不得超余量（旧 `Vec::drain(..w)` 越界会 panic；
        // 静默多消费 = 上游字节流错位——调试构建在此暴露误用）。调用点恒满足。
        debug_assert!(
            n <= self.buf.len().saturating_sub(self.off),
            "consume 超余量（n={n} remaining={}）——上游字节流错位",
            self.buf.len().saturating_sub(self.off)
        );
        self.off += n;
        if self.off >= self.buf.len() {
            self.buf.clear();
            self.off = 0;
        } else if self.off * 2 >= self.buf.len() {
            // 死前缀原地压缩（P0-1）：上游持续 `push` 与部分写交错时 `off` 难命中
            // `buf.len()`，死前缀 `buf[..off]` 若不回收 ⇒ backing `Vec` 随累计传输量
            // 线性增长。压缩阈值 `off*2 >= len` ⇒ 每字节摊还 ≤1 次 `copy_within`
            // （无分配），且不变量 `len = off + remaining < 2*remaining` 保 backing 有界。
            let len = self.buf.len();
            self.buf.copy_within(self.off.., 0);
            self.buf.truncate(len - self.off);
            self.off = 0;
        }
    }
}

/// `dns_tcp_feed` 的返回形态（F2）：`Ok` = 正常（含帧未到齐）；`CloseLeg` = 读到
/// 空 TCP 消息（`mlen == 0`）——调用方须在 `for flow` 循环**外**统一拆流（[门-A1]）。
enum FeedOutcome {
    Ok,
    CloseLeg,
}

/// 非阻塞 connect 的返回形态（即时成功也走统一验收收口——R-1：即时成功不产生
/// 任何 poll 事件，若验收只挂 POLLOUT，流会卡到死线）。
enum DialOutcome {
    /// connect 返回 0（UDS 常态/TCP 回环偶发/UDP 恒）。
    Connected(OwnedFd),
    /// EINPROGRESS/EALREADY。
    InProgress(OwnedFd),
    /// EAGAIN（Linux AF_UNIX 监听队列满）：保留 fd 每拍重拨 connect。
    Retry(OwnedFd),
}

/// 非阻塞拨号（reactor-design §二：**禁止 std 的 connect/connect_timeout**——std 在
/// 非阻塞 socket 上把 EINPROGRESS 当 Err 返回；`SOCK_NONBLOCK` 在 libc 的 apple 目标
/// 未定义，非阻塞一律 fcntl）。**前提**：UDP（bind+connect 皆本地操作）恒即时成功
/// ——「pending 窗口坍缩为零」（设计 §三）的依据；SOCK_DGRAM 上 EINPROGRESS/EAGAIN
/// 不可达，出现即按 fail 收口。
fn dial_nonblocking(target: &DialTarget) -> std::io::Result<DialOutcome> {
    let (domain, ty, addr, len, bind_any) = target_sockaddr(target)?;
    unsafe {
        // F1：CLOEXEC 在**创建时**落下（linux/OHOS 走 `SOCK_CLOEXEC` 原子位；darwin
        // 建后立即补）——Go 侧 `net.Dialer` 的 fd 天然 CLOEXEC，此为移植回退面收口。
        let fd = crate::sysfd::socket_cloexec(domain, ty, 0)?;
        set_fd_flags(fd.as_fd(), true)?;
        if let Some((baddr, blen)) = bind_any {
            // UDP：先绑临时端口（Go「bind ephemeral + connect」同形态）
            if libc::bind(fd.as_raw_fd(), &baddr as *const _ as *const libc::sockaddr, blen) != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        // EINTR 重试（连接调用被信号打断 = 立即重拨本调用，不当失败）
        loop {
            let rc = libc::connect(fd.as_raw_fd(), &addr as *const _ as *const libc::sockaddr, len);
            if rc == 0 {
                return Ok(DialOutcome::Connected(fd));
            }
            let e = std::io::Error::last_os_error();
            match e.raw_os_error() {
                Some(libc::EINTR) => continue,
                Some(libc::EINPROGRESS) | Some(libc::EALREADY) => return Ok(DialOutcome::InProgress(fd)),
                Some(libc::EAGAIN) => return Ok(DialOutcome::Retry(fd)),
                _ => return Err(e),
            }
        }
    }
}

/// 拨号目标的 sockaddr 打包（v4/v6/UDS 三形态）。
type SockAddrPack = (
    libc::c_int,
    libc::c_int,
    libc::sockaddr_storage,
    libc::socklen_t,
    Option<(libc::sockaddr_storage, libc::socklen_t)>,
);

fn target_sockaddr(target: &DialTarget) -> std::io::Result<SockAddrPack> {
    fn pack_ip(a: &SocketAddr) -> (libc::c_int, libc::sockaddr_storage, libc::socklen_t) {
        let mut ss: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let len = match a {
            SocketAddr::V4(v4) => {
                let s: &mut libc::sockaddr_in = unsafe { &mut *(&mut ss as *mut _ as *mut _) };
                s.sin_family = libc::AF_INET as libc::sa_family_t;
                s.sin_port = v4.port().to_be();
                // octets 内存序即网络序——u32 原生重解释写回同一字节形态
                s.sin_addr = libc::in_addr { s_addr: u32::from_ne_bytes(v4.ip().octets()) };
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t
            }
            SocketAddr::V6(v6) => {
                let s: &mut libc::sockaddr_in6 = unsafe { &mut *(&mut ss as *mut _ as *mut _) };
                s.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                s.sin6_port = v6.port().to_be();
                s.sin6_addr = libc::in6_addr { s6_addr: v6.ip().octets() };
                s.sin6_scope_id = v6.scope_id();
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t
            }
        };
        (ss.ss_family as libc::c_int, ss, len)
    }
    // UDP 先绑的任意地址 = 目标同族的 UNSPECIFIED:0（pack_ip 直推）
    fn any_of(a: &SocketAddr) -> SocketAddr {
        match a {
            SocketAddr::V4(_) => SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)),
            SocketAddr::V6(_) => SocketAddr::from((Ipv6Addr::UNSPECIFIED, 0)),
        }
    }
    match target {
        DialTarget::Tcp(addr) => {
            let (domain, ss, len) = pack_ip(addr);
            Ok((domain, libc::SOCK_STREAM, ss, len, None))
        }
        DialTarget::Udp(addr) => {
            let (domain, ss, len) = pack_ip(addr);
            let (d2, bind_any, blen) = pack_ip(&any_of(addr));
            debug_assert_eq!(d2, domain);
            Ok((domain, libc::SOCK_DGRAM, ss, len, Some((bind_any, blen))))
        }
        DialTarget::Unix(path) => {
            // 装配约定 UDS 路径 < 100B（R3 §4.1 LocalServices 组装边界）；上界按
            // 平台结构判（macOS sun_path=104B、Linux=108B——写死 108 会越界）。
            // **与 `sysfd::SUN_PATH_MAX` 同源同义**（`len >= sun_path.len()` ≡
            // `len > SUN_PATH_MAX`）——只注明，不字面改调（照字面改会因 off-by-one
            // 静默收紧；Q-G F4.3）。
            let bytes = path.as_bytes();
            let mut ss: libc::sockaddr_un = unsafe { std::mem::zeroed() };
            if bytes.len() >= ss.sun_path.len() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "UDS 路径超长",
                ));
            }
            ss.sun_family = libc::AF_UNIX as libc::sa_family_t;
            // sun_path 在部分平台是 [i8]——按字节指针拷；尾零靠 zeroed
            unsafe {
                std::ptr::copy_nonoverlapping(
                    bytes.as_ptr(),
                    ss.sun_path.as_mut_ptr().cast::<u8>(),
                    bytes.len(),
                );
            }
            let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            // 长度自洽 = family 段 + 实际路径 + NUL（不依赖零填充到全结构长）
            let len = (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1)
                as libc::socklen_t;
            unsafe {
                std::ptr::copy_nonoverlapping(
                    &ss as *const _ as *const u8,
                    &mut storage as *mut _ as *mut u8,
                    len as usize,
                );
            }
            Ok((libc::AF_UNIX, libc::SOCK_STREAM, storage, len, None))
        }
    }
}

/// fcntl 建非阻塞 + `FD_CLOEXEC`（`SOCK_NONBLOCK`/`SOCK_CLOEXEC` 在 libc 的 apple
/// 目标均未定义——darwin 唯一形态；linux/OHOS 创建期已走原子位，此处幂等重设）。
/// 失败按 Err 返回（「循环内禁阻塞」的防御闭合：置不上非阻塞的 fd 会阻塞驱动
/// 线程——评审 r2-低6；CLOEXEC 缺失 = 子进程继承悬挂 fd——Q-G F1）。
fn set_fd_flags(fd: std::os::fd::BorrowedFd<'_>, nonblocking: bool) -> std::io::Result<()> {
    unsafe {
        if nonblocking {
            let fl = libc::fcntl(fd.as_raw_fd(), libc::F_GETFL);
            if fl < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFL, fl | libc::O_NONBLOCK) < 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        let fc = libc::fcntl(fd.as_raw_fd(), libc::F_GETFD);
        if fc < 0 || libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, fc | libc::FD_CLOEXEC) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

// ---------- 出口发送整形（R8-3 8i；设计 = docs/reviews/R8.md §九） ----------
//
// 归因背景（PERF-AB §9）：Rust 出口单 poll 把拦截栈排空的 ≤2379 包（≈3.1MB）一次
// 倾泻上线——团块在接收端/空口成组丢失（冷 WiFi 电源态尤甚，§9.5 同步悬崖）。本整形
// = **字节令牌桶 + 下拍续传**：pump 尾把本拍产物并入滞留 FIFO，按令牌从头释放；余量
// 留给后续 pump 拍（驱动线程每拍必调——poll 5ms 上界 + ACK 到达即醒；5ms 续水
// 320KB ≥ 2×突发额度 ⇒ 桶不会连续两拍枯竭）。只削峰不平率：稳态到达 < 速率时令牌
// 常满、零延迟直通；平均率仍由 ACK 时钟（栈内 CUBIC）决定——整形器不注入流量，
// 滞留深度 ≤ Σcwnd（TCP 在途记账自钳制），**不设显式上限**（上限 = 整形器丢包 =
// 伪修复禁区）。

/// 整形速率默认 200 MiB/s（D-2 8n② 修订；`HOMEWAY_TX_RATE_MBPS` 覆盖）。
/// R8-3 的 64MiB/s（层 0 天花板 94 的 0.68×）在 40MB/s 级需求下**把稳态吞吐钳进
/// 桶并注入 RTT**：滞留队列常驻 ~500KB ⇒ 排队延迟 ≈ 深度/64MiB/s ≈ 8ms ⇒ 真机
/// TCP RTT 17-27ms（Go 出口同刻 6ms）⇒ 吞吐 = 窗/RTT 同比塌到 17.5MB/s。真机
/// 梯度（同小时同机）：64/160KiB=17.5 稳、128/256 与 160/512=锯齿带（拍频-ACK 团
/// 耦合）、200/2048=30-40 稳（Go 出口同刻 41-46）；冷连 200/2048 平滑爬坡无悬崖
/// （冷/热 0.77 ≥ 0.70 门）。**R 的职责修正**：本整形器的存在意义是团块钳制
/// （冷连悬崖），不是速率限制——R 取 ~2× 层 0 天花板（均值面对 3MB/s 级极端场景
/// 仍有钳制），路径容量由 ACK 时钟（栈内 CUBIC）自管。
pub const TX_SHAPE_RATE: u64 = 200 * 1024 * 1024;
/// 突发额度默认 256 KiB = **桶容量 = 单拍放行上界**（D-2 8n③；`HOMEWAY_TX_BURST_KB`
/// 覆盖；两义同值——见 shape_slice 的不变量注释）。梯度数据：2048KB 形态消除了
/// 排队延迟但团块 1-2MB 在空口成组丢失（dup 260-657/5s → CUBIC 反复砍窗，B 稳
/// 19-40 波动）；256KB = 修复前 3.1MB 倾泻的 1/12、≈ Go 出口自然发送团的量级——
/// 团块钳制与无排队同时成立（B 热态 25-28 平稳无锯齿）。
pub const TX_SHAPE_BURST: usize = 256 * 1024;
/// `tx_deferred` 滞留字节上限（F4：无窗流丢新；TCP 不丢）。
///
/// 取值依据：`16 × TX_SHAPE_BURST = 4 MiB`——≫ 单拍突发额度（256KiB，16 拍才会触及），
/// 又远小于 OOM 阈值；非 TCP（UDP/DNS/ICMP）并入会超上限时**丢新 + 计数**，TCP 保持
/// 不丢（字节流丢字节 = 流错位，比内存增长更严重）。TCP 侧上界仍是「Σcwnd +
/// Σ(FLOW_TX_BUF 未释放部分) ≤ MAX_CONNS(1024) × ~1MB ≈ 1GB」的全局最坏（登记口径，
/// 见 `docs/reviews/QB.md`）。
const TX_DEFER_MAX_BYTES: usize = 16 * TX_SHAPE_BURST;

// ---- Q-K F5-d：出口 TX 侧分片表（反重写的分片感知）----

/// TX 分片表上限（正常态 ≤1 项——smoltcp 的 `Fragmenter` 是单缓冲：一个 Interface
/// 同时只有 1 条在途分片报文，剩余片在后续 poll 续发；TTL/上限只兜异常）。
const TX_FRAG_MAX: usize = 64;
/// TX 分片表 TTL（跨 poll 续发的片间隔 1–5ms，10s 是量级余量；超时兜「首片丢失」）。
const TX_FRAG_TTL: Duration = Duration::from_secs(10);

/// TX 分片表键：`dst` = 客户端隧道 IP（每 peer 唯一）、`ident` = smoltcp 的
/// `next_ipv4_frag_ident()`（按 Interface 递增 ⇒ 一个在途报文内唯一）、`proto`。
type TxFragKey = (Ipv4Addr, u16, u8);

/// TX 分片表值：首片命中反重写时登记的值——`orig` = 该报文的原始目的（`None` =
/// 本就不需重写，如真 listener 应答）；后续片按它取**同一个改写后 IP 源**
/// （同报文全部分片必须携带同一 IP 源，否则客户端重组键分裂）。
struct TxFragVal {
    orig: Option<(Ipv4Addr, u16)>,
    created: Instant,
}

/// 出口发送整形参数（字节令牌桶——团块钳制面；v0.2.2 简洁化批起逐包时刻表
/// 已删除，只剩令牌桶本体 + `HOMEWAY_TX_SHAPING` 逃生口）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TxShape {
    /// 续水速率（B/s）。
    pub rate: u64,
    /// 突发额度 = 令牌容量（B）：任意窗口 w 内线上深度 ≤ burst + rate·w。
    pub burst: usize,
}

/// config.toml `[serve.tx_shape]` 节（D-3 反过拟合约束 3：参数 config 化；env 臂
/// 保留为测试缝，覆盖序 **env > config > 产品默认**）。deny_unknown = typo 保护。
#[derive(serde::Deserialize, serde::Serialize, Default, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
pub struct TxShapeCfg {
    /// 桶均值钳制速率（MiB/s，默认 200——~2× 层 0 天花板；家宽/无线出口按默认即可）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rate_mbps: Option<u64>,
    /// 单拍放行上界/桶容量（KiB，默认 256）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst_kb: Option<usize>,
}

/// 整形参数解析（config 面 + env 测试缝，覆盖序 **env > config > 产品默认**）。
///
/// - 总开关 `HOMEWAY_TX_SHAPING`（**值匹配**：未设/`on`/`1`/`true` = 开，`off`/`0`/
///   `false` = 整套关——评审 r2 自补1 的惯例：presence 语义会把消融方向搞反）；
/// - `HOMEWAY_TX_RATE_MBPS` / `HOMEWAY_TX_BURST_KB` 覆盖桶参数；
/// - config 面 = `TxShapeCfg`（[serve.tx_shape]）；
/// - 非法值静默回落默认。
pub(crate) fn tx_shape_resolve(cfg: Option<TxShapeCfg>) -> Option<TxShape> {
    match std::env::var("HOMEWAY_TX_SHAPING").as_deref() {
        Ok("off") | Ok("0") | Ok("false") => return None, // 消融臂（整套显式关）
        Ok(_) => {}                                      // on/1/true/其它 = 开
        Err(_) => {}                                     // 未设 = 产品默认开
    }
    let cfg = cfg.unwrap_or_default();
    let mut rate = TX_SHAPE_RATE;
    let mut burst = TX_SHAPE_BURST;
    if let Some(r) = cfg.rate_mbps {
        if r > 0 {
            rate = r * 1024 * 1024;
        }
    }
    if let Some(b) = cfg.burst_kb {
        if b > 0 {
            burst = b * 1024;
        }
    }
    if let Ok(v) = std::env::var("HOMEWAY_TX_RATE_MBPS") {
        if let Ok(n) = v.trim().parse::<u64>() {
            if n > 0 {
                rate = n * 1024 * 1024;
            }
        }
    }
    if let Ok(v) = std::env::var("HOMEWAY_TX_BURST_KB") {
        if let Ok(n) = v.trim().parse::<usize>() {
            if n > 0 {
                burst = n * 1024;
            }
        }
    }
    Some(TxShape { rate, burst })
}

/// shape_slice 的运行态进出（credit/时刻表/滞留字节——调用方持有、按值进出，
/// 纯函数可注入）。
#[derive(Clone, Copy)]
struct ShapeRun {
    credit: f64,
    /// 并入前的队存字节（与 tx_deferred_bytes 同源的 O(1) 维护量）。
    deferred_bytes: usize,
}

/// 令牌桶释放的一拍（纯函数面——单测可注入时间）：`produce` 并入 `deferred`
/// 尾部（FIFO 保序），先按 `dt` 续水（容量 = burst），再从头释放桶 credit 放得
/// 下的包。返回 (本拍释放, ShapeRun 余态〔credit/滞留字节〕)。大于 burst 的包
/// 防御性直通（内层 IP 恒 ≤ 64KB < 默认 burst；防极小 burst 配置把队列头部卡死）。
///
/// burst 语义（评审 1.1 认账修订）：**桶容量 = 单拍放行上界（同值）**——不变量
/// 由 credit 逐包扣减自带：每拍开始 credit ≤ burst、每放一包 credit -= fl ⇒ 单拍
/// 释放总量恒 ≤ burst。**调大 burst 等于同时放开累积额度与单拍倾泻**（真机梯度：
/// 桶 2MB 时团块 1-2MB 在空口成组丢失、dup 260-657/5s、CUBIC 反复砍窗）——
/// 256KiB = 修复前 3.1MB 倾泻的 1/12 ≈ Go 出口的自然发送团量级。
fn shape_slice(
    deferred: &mut std::collections::VecDeque<Vec<u8>>,
    produce: Vec<Vec<u8>>,
    run_in: ShapeRun,
    p: TxShape,
    dt: f64,
) -> (Vec<Vec<u8>>, ShapeRun) {
    let TxShape { rate, burst } = p;
    let ShapeRun { mut credit, deferred_bytes } = run_in;
    // 并入字节先记账（评审 r2-4.6：释放路径的字节维护量改增量——深滞留形态下
    // 每拍 O(队深) 重扫 + 按队深预分配在高拍频下是 GB/s 级 churn）。滞留字节并入
    // 侧自增、释放侧自减、余量随 ShapeRun 出——O(1)。
    let mut deferred_bytes =
        deferred_bytes + produce.iter().map(|p| p.len()).sum::<usize>();
    deferred.extend(produce);
    credit = (credit + rate as f64 * dt).min(burst as f64);
    let mut out = Vec::with_capacity(deferred.len());
    while let Some(front) = deferred.front() {
        let fl = front.len() as f64;
        if fl > burst as f64 {
            // 防死锁直通 ≠ 参与记账（评审 r2-1.2 同口径）：极小 burst 配置下直通
            // 分支不扣 credit——扣了会打成负值，后续包要等续水补回才放行。
        } else {
            if credit < fl {
                break;
            }
            credit -= fl;
        }
        let fl_usize = front.len();
        out.push(deferred.pop_front().expect("front 已判"));
        deferred_bytes -= fl_usize;
    }
    (out, ShapeRun { credit, deferred_bytes })
}

// 「真丢包」检测与发送塑形的历史注记（R6.6 应用层 CC 垫片，R8-8a 随 smoltcp
// 0.11→0.14 迁移**整体退役**）：拥塞控制/重传退避/零窗探测现由栈内
// `CongestionControl::Cubic`（RFC 合规）承担——cwnd 门、pacing、seq 回退检测、
// ACK 停滞判据全部删除（ROADMAP「R7 前置批 smoltcp 0.14 工单」闭环）。

/// 转发面计数器（拦截层是唯一生产写入方；键名 = 观测面契约：dialok/dialfail/flows/rejected）。
#[derive(Default)]
pub struct Stats {
    dial_ok: AtomicU64,
    dial_fail: AtomicU64,
    flows: AtomicU64,
    rejected: AtomicU64,
    /// transit UDP 会话的归宿（收到过回包 / 只有上行——udpcap 实测位）。
    udp_replied: AtomicU64,
    udp_no_reply: AtomicU64,
    /// UDP 应用层丢新（F3：栈→upstream 门/上限丢新 + 回投客户端失败；F10：UDP :53
    /// 应答写回失败——同族静默失败，独立计数不改既有计数器语义）。
    udp_drop: AtomicU64,
    /// 发送整形滞留上限丢新（F4：无窗流并入会超 `TX_DEFER_MAX_BYTES` 时丢新）。
    shape_drop: AtomicU64,
    // ---- Q-K（F1/F2/F5-d）：分片面 ----
    /// **重定义**（Q-K）：被丢弃的分片包**总数**（RX 侧）。恒等式
    /// **`fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`** 由
    /// `note_frag_drop` 的单次记账结构保证（四处独立自增会漂移）。
    frag_drop: AtomicU64,
    /// 成功重组并交付的**报文（datagram）数**——唯一非「片」单位。
    frag_reasm: AtomicU64,
    /// 非法分片包（偏移越界 / 非末片非 8 倍 / 空片）连带丢弃数。
    frag_bad: AtomicU64,
    /// 重叠/冲突整条丢弃所连带的分片包数（攻击面信号）。
    frag_overlap: AtomicU64,
    /// 超时淘汰所连带的分片包数（丢包/占用信号）。
    frag_timeout: AtomicU64,
    /// 上限（上下文数 / 每源 / 片数 / 字节）拒绝或淘汰所连带的分片包数。
    frag_limit: AtomicU64,
    /// **TX 侧**（F5-d）：无首片对应 / TX 分片表未命中而丢弃的分片包数（对照面）。
    tx_frag_drop: AtomicU64,
}

impl Stats {
    pub fn incr_ok(&self) {
        self.dial_ok.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_fail(&self) {
        self.dial_fail.fetch_add(1, Ordering::Relaxed);
    }
    fn incr_flow(&self) {
        self.flows.fetch_add(1, Ordering::Relaxed);
    }
    fn decr_flow(&self) {
        self.flows.fetch_sub(1, Ordering::Relaxed);
    }
    pub fn incr_reject(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }
    pub fn rejects(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
    /// UDP 会话归宿上报（关闭时；只 transit 会话）。
    pub fn incr_udp_session(&self, replied: bool) {
        if replied {
            self.udp_replied.fetch_add(1, Ordering::Relaxed);
        } else {
            self.udp_no_reply.fetch_add(1, Ordering::Relaxed);
        }
    }
    /// UDP 应用层丢新/回投失败（F3/F10）。
    fn incr_udp_drop(&self) {
        self.udp_drop.fetch_add(1, Ordering::Relaxed);
    }
    /// 发送整形滞留上限丢新（F4）。
    fn incr_shape_drop(&self) {
        self.shape_drop.fetch_add(1, Ordering::Relaxed);
    }
    /// 分片包丢弃的**唯一记账点**（Q-K F2）：`fragDrop` 与四分类计数在**同一次调用**
    /// 内自增 ⇒ 恒等式 `fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`
    /// 结构成立（调用方不得绕开本函数直改任一计数）。
    pub(crate) fn note_frag_drop(&self, reason: DropReason, packets: u64) {
        self.frag_drop.fetch_add(packets, Ordering::Relaxed);
        match reason {
            DropReason::Bad => self.frag_bad.fetch_add(packets, Ordering::Relaxed),
            DropReason::Overlap => self.frag_overlap.fetch_add(packets, Ordering::Relaxed),
            DropReason::Timeout => self.frag_timeout.fetch_add(packets, Ordering::Relaxed),
            DropReason::Limit => self.frag_limit.fetch_add(packets, Ordering::Relaxed),
        };
    }
    /// 成功重组交付计数（F2：单位 = 报文，非片）。
    pub(crate) fn incr_frag_reasm(&self) {
        self.frag_reasm.fetch_add(1, Ordering::Relaxed);
    }
    /// TX 侧分片丢片计数（F5-d：表未命中/无首片）。
    pub(crate) fn incr_tx_frag_drop(&self) {
        self.tx_frag_drop.fetch_add(1, Ordering::Relaxed);
    }
    /// 观测快照（键名 = 观测面契约；新增计数一律**追加末位**——保既有索引断言）。
    pub fn snapshot(&self) -> [(&'static str, u64); 15] {
        [
            ("dialok", self.dial_ok.load(Ordering::Relaxed)),
            ("dialfail", self.dial_fail.load(Ordering::Relaxed)),
            ("flows", self.flows.load(Ordering::Relaxed)),
            ("rejected", self.rejected.load(Ordering::Relaxed)),
            ("udpReplied", self.udp_replied.load(Ordering::Relaxed)),
            ("udpNoReply", self.udp_no_reply.load(Ordering::Relaxed)),
            ("udpDrop", self.udp_drop.load(Ordering::Relaxed)),
            ("shapeDrop", self.shape_drop.load(Ordering::Relaxed)),
            ("fragDrop", self.frag_drop.load(Ordering::Relaxed)),
            ("fragReasm", self.frag_reasm.load(Ordering::Relaxed)),
            ("fragBad", self.frag_bad.load(Ordering::Relaxed)),
            ("fragOverlap", self.frag_overlap.load(Ordering::Relaxed)),
            ("fragTimeout", self.frag_timeout.load(Ordering::Relaxed)),
            ("fragLimit", self.frag_limit.load(Ordering::Relaxed)),
            ("txFragDrop", self.tx_frag_drop.load(Ordering::Relaxed)),
        ]
    }
}

/// 拦截层配置（装配层注入）。
pub struct Config {
    pub tunnel_ip: Ipv4Addr,
    /// 豁免端口 → Unix socket 路径（LocalServices；UDP 不查——Go 同口径）。
    pub local_services: HashMap<u16, String>,
    /// :53 进程内代答腿 + 隧道栈内 DNS 面（None = 关闭代答，:53 按原目标过境重拨）。
    pub dns: Option<Arc<DnsProxy>>,
    /// DNS worker 的应答回投通道（与 dns 同生共死）。
    pub dns_events: Option<std::sync::mpsc::Receiver<DnsReply>>,
    /// 客户端远程解析腿端口（隧道 IP:<它> TCP；0 = 不建该面）。
    pub dns_resolve_port: u16,
    /// 出口发送整形（R8-3 8i）：None = 关（消融臂/单测直通面），Some = 字节令牌桶
    /// 参数。产品装配面由 `tx_shape_default()`（env 消融臂）填充。
    pub tx_shape: Option<TxShape>,
    pub logf: Logf,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Transit,
    Exempt,
    Dns,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Transit => "transit",
            Kind::Exempt => "exempt",
            Kind::Dns => "dns",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Proto {
    Tcp,
    Udp,
}

enum Phase {
    /// TCP：upstream 拨号中（SYN 缓存）；UDP：建会话窗口（pending 队列）。
    Dialing {
        cache: Vec<Vec<u8>>,
    },
    Established,
}

/// R8-4 8n 第二瓶颈归因插桩：单 TCP 流的收发观测（**驱动线程独占，无锁**）。
/// TX 累计点 = on_tx 反重写命中；RX 累计点 = on_plain 流命中（客户端 ACK）。
/// cc_stats_line 5s 窗差分消费报出。判别目标：
/// - 对端通告窗原始 u16（min~max/last）——检测 rwnd 门限（CUBIC cwnd 上限 =
///   max(64×536, 对端窗)，smoltcp 只升不降）与窗口更新稀疏度；
/// - ACK 段数/确认推进字节——ACK 时钟密度（每 2 段一个 ACK + 10ms delayed 兜底）；
/// - dup ACK——手机侧乱序/丢包代理观测；
/// - 发送段最大载荷（≈实际协商 MSS）；
/// - 在途 ≈ Σ发送 - Σ确认（含重传量，配 dup 解读）。
#[derive(Default, Clone, Copy)]
struct TcpObs {
    tx_seg: u64,
    tx_data_seg: u64,
    tx_bytes: u64,
    tx_max_seg: usize,
    ack_seg: u64,
    ack_bytes: u64,
    ack_dup: u64,
    win_min: Option<u32>,
    win_max: u32,
    win_last: u32,
    ack_last: Option<u32>,
    inflight_est: i64,
}

impl TcpObs {
    /// 客户端→出口 ACK 段累计（on_plain 点；v.proto 已判 6）。单段推进截在
    /// 1MiB（真 ACK 的推进 ≤ 对端通告窗——快路径 ~1MB 量级；超出的是五元组
    /// 复用/回绕的假推进——截断防累计面污染）。
    fn note_rx_ack(&mut self, v: &Ipv4View) {
        self.ack_seg += 1;
        let ack = v.tcp_ack;
        match self.ack_last {
            Some(prev) => {
                let d = ack.wrapping_sub(prev);
                if d == 0 && v.payload.is_empty() {
                    self.ack_dup += 1;
                } else if d < 0x8000_0000 {
                    // 单调推进（回绕安全半窗内）才计确认字节；1MiB 截断见函数头
                    let adv = (d as u64).min(1024 * 1024);
                    self.ack_bytes += adv;
                    self.inflight_est -= adv as i64;
                }
            }
            None => {
                // 首 ACK 基线：不含 SYN 计数，从第二次起算推进
            }
        }
        self.ack_last = Some(ack);
        let w = v.tcp_win as u32;
        self.win_min = Some(self.win_min.map_or(w, |m| m.min(w)));
        self.win_max = self.win_max.max(w);
        self.win_last = w;
    }

    /// 出口→客户端段累计（on_tx 点；载荷长度按反重写前的包体）。
    fn note_tx_seg(&mut self, v: &Ipv4View) {
        self.tx_seg += 1;
        let n = v.payload.len();
        if n > 0 {
            self.tx_data_seg += 1;
            self.tx_bytes += n as u64;
            self.tx_max_seg = self.tx_max_seg.max(n);
            self.inflight_est += n as i64;
        }
    }
}

struct Flow {
    kind: Kind,
    proto: Proto,
    /// 客户端侧端点（栈内 socket 的对端 / 反重写时的目的）。
    client: (Ipv4Addr, u16),
    /// 原始目的（豁免/过境判定依据；TX 反重写的源地址）。
    orig_dst: (Ipv4Addr, u16),
    rw_port: u16,
    sock: Option<SocketHandle>,
    phase: Phase,
    last_active: Instant,
    /// 重拨 OS socket（reactor 所有；None = 无 OS socket——DNS 进程内腿）。
    /// 栈→upstream 在途字节 = io 待写缓冲现存（unacked_out 记账已坍缩为直读）。
    io: Option<ReactorIo>,
    /// upstream→栈内 socket 写不下的余量（**部分写回补**——send_slice 只写前缀时
    /// 余量必须留住：静默丢字节 = 下游流错位【2026-10-02 实测抓出：speedtest 下行
    /// 大流量下帧错位】；poll 开窗后在 service_sockets 续写）。
    /// Q-I F1：`Vec` + `drain(..w)` 的尾部整体前移是引擎忙时第一热点（实测占 32%），
    /// 换 `VecDequeLite` 前缀偏移 + 摊还压缩（`consume` 内 `off*2 >= len` 触发；
    /// 不变量 `len < 2*remaining`，摊还 ≤2B 搬移/B 消费）。
    tx_backlog: VecDequeLite,
    /// upstream EOF 后待补的 FIN（**backlog 排空后才 close**：close 会把 FIN 排进
    /// socket 发送队列——backlog 里的数据若在 FIN 之后才写就永远出不去，客户端看到
    /// 「数据 + FIN + 丢尾」的流错位【2026-10-02 实测抓出：speedtest report 帧丢失】）。
    fin_pending: bool,
    /// transit UDP：是否收到过回包（udpcap 实测位）。
    udp_replied: bool,
    /// UDP 会话号（E12 关闭行用——建立时分配、关闭时回放，评审 M4）。
    udp_seq_of: u64,
    /// flows gauge 配对守卫（F5）：`establish` 置位并 `incr_flow`，`retire` 仅在此位
    /// 为真时 `decr_flow` 并清零。防「未 incr 即 decr」（listen 失败、close() 期在途
    /// Dialing 流）导致的 gauge 回绕。守卫状态由 establish/retire 转移伴生。
    counted: bool,
    /// TCP：建流 SYN 的 seq（拨号失败构造 RST|ACK 的 ack 依据）。
    syn_seq: u32,
    /// TCP DNS 腿的 RFC1035 分帧积攒（2B 长度前缀 + 报文；跨读保留不完整帧）。
    dns_rx: Vec<u8>,
    /// TCP 归因观测（8n；UDP 流恒零值闲置）。
    obs: TcpObs,
}

/// cc_stats_line 观测行的上一窗快照（差分用；字段 = TcpObs 的累计子集）。
#[derive(Clone, Copy)]
struct TcpObsSnap {
    tx_seg: u64,
    tx_bytes: u64,
    ack_seg: u64,
    ack_bytes: u64,
    ack_dup: u64,
}

impl From<&TcpObs> for TcpObsSnap {
    fn from(o: &TcpObs) -> Self {
        Self {
            tx_seg: o.tx_seg,
            tx_bytes: o.tx_bytes,
            ack_seg: o.ack_seg,
            ack_bytes: o.ack_bytes,
            ack_dup: o.ack_dup,
        }
    }
}

/// 拦截层本体（**驱动线程独占**——RX/TX/流表/栈全在一条线程）。
pub struct Interceptor {
    cfg: Config,
    stats: Arc<Stats>,
    iface: Interface,
    sockets: SocketSet<'static>,
    device: TunDevice,
    flows: HashMap<u64, Flow>,
    by_five: HashMap<(Ipv4Addr, u16, Ipv4Addr, u16, u8), u64>,
    by_rw_port: HashMap<u16, u64>,
    next_flow: u64,
    next_rw_port: u16,
    halted: bool,
    /// reactor 的 pollfd 复用缓冲（每拍 clear 重建——评审 R-6：不重建 Vec 只重填）。
    pollfds: Vec<libc::pollfd>,
    /// pollfds 的同序流号索引（revents → 流回指）。
    poll_index: Vec<u64>,
    /// reactor 等待者位（connect 在途 ∨ 任一流待写缓冲非空——引擎 wait_hint 消费；
    /// **pump 尾刷新**，评审 R-5：pump 开头算会漏本拍 service_sockets 新攒的待写量）。
    reactor_waiters: bool,
    /// reactor 观测（5s 窗：pump 拍数/名下 fd 峰值/单拍 wall 峰值——剂量面，评审 R-6/R-9）。
    reactor_win_pumps: u64,
    reactor_win_peak_fds: usize,
    reactor_win_peak_wall: Duration,
    /// 栈内真 listener 的端口集（demux 优先面；3d 的 DNS listener 登记）。
    served_ports: std::collections::HashSet<u16>,
    /// 出站明文包队列（TX 反重写后待 encap——pump 返回给引擎）。
    tx_out: Vec<Vec<u8>>,
    // ---- 发送整形（R8-3 8i；驱动线程独占——与 tx_out 同生命周期） ----
    /// 滞留队列（令牌不够时的未释放出站包，FIFO 保序；TCP 面深度 ≤ Σcwnd
    /// 自钳制——UDP/DNS/ICMP 等无窗记账面的口径见 §九注记，评审 r2-3.1）。
    tx_deferred: std::collections::VecDeque<Vec<u8>>,
    /// 滞留字节数（维护量——评审 r2-自补5：峰值统计不再每拍 O(n) 扫全队列）。
    tx_deferred_bytes: usize,
    /// 当前令牌余量（B）。
    tx_credit: f64,
    /// 上次续水时刻。
    tx_last_refill: Instant,
    /// 本观察窗整形拍数（cc 5s 行消费清零——「每次唤醒放行包数」的判读面，
    /// 评审 r1-1.1：间隔低于循环固定成本时有效放行率由循环容量界定）。
    tx_win_pumps: u64,
    /// 本观察窗释放包数（cc_stats_line 5s 消费清零——窗语义）。
    tx_win_released: u64,
    /// 本观察窗滞留深度峰值 (包, B)（同窗消费清零）。
    tx_win_defer_peak: (usize, usize),
    /// DNS 的隧道栈内监听面（:53 UDP/TCP + 解析腿 TCP；dns 开才建）。
    dns_faces: Option<DnsFaces>,
    /// DNS worker 应答回投通道（pump 拍内 drain）。
    dns_rx: Option<std::sync::mpsc::Receiver<DnsReply>>,
    /// UDP 会话号（判据行 #N——进程级递增，对齐旧 udp relay 口径）。
    udp_seq: u64,
    /// 拨号失败日志的降噪表（形态 → (累计次数, 是否已记过首行)；R6.6 P2）。
    /// 键 = (kind, 原始目的)——**不含源端点**：手机核自连探测每次换临时源端口，
    /// 键含源则每条都成「首行」，降噪失效（评审 r1 低危整改）。
    dial_fail_seen: HashMap<DialFailKey, (u64, bool)>,
    // ---- Q-K：分片重组（F1）与出口 TX 分片感知（F5-d）----
    /// RX 分片重组器（驱动线程独占；超时清扫在 `pump`/`pump_hold` 每拍开头）。
    reasm: Reassembler,
    /// TX 分片表（首片登记 / 末片精确回收 / TTL + 上限兜底）。
    tx_frag: HashMap<TxFragKey, TxFragVal>,
    /// 分片限频日志表（F2：键 = **纯 kind**——不含 `src`、**不用 HashMap**（两个封闭
    /// 具名字段从结构上杜绝无界增长）；节流 = 首行 + 每 100 次一条累计行，与
    /// `dial_fail_seen` 同节奏）。重叠/冲突/非法**不记行**（可被对端逐包诱发——
    /// 逐条记行 = 自我放大；有意取舍，已登记）。
    frag_limit_seen: (u64, bool),
    frag_timeout_seen: (u64, bool),
    /// CC 观测行的上次打印时刻。
    last_cc_stats: Option<Instant>,
    /// 观测行上一窗快照（8n；流 id → 累计快照——差分本窗增量）。
    obs_snaps: HashMap<u64, TcpObsSnap>,
    time0: Instant,
    smol_now: SmolInstant,
    /// Q-I F3：`service_sockets` 的栈读复用缓冲（TCP/UDP 两读循环共用；构造期一次
    /// 分配 `Box`——此前每读循环迭代各一次 64KB 栈缓冲零初始化，实测 1.5%）。
    rx_scratch: Box<[u8; 64 * 1024]>,
}

impl Interceptor {
    /// 装配：拦截栈（地址 = 隧道 IP）+ worker 池。E5 判据行在此打出。
    pub fn attach(mut cfg: Config, stats: Arc<Stats>) -> Self {
        let mut device = TunDevice::new();
        let mut iface = Interface::new(
            IfaceConfig::new(HardwareAddress::Ip),
            &mut device,
            SmolInstant::from_millis(0),
        );
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(cfg.tunnel_ip.into(), 32))
                .expect("唯一地址必入表");
        });
        // 默认路由（TX 包目的 = 客户端地址；Medium::Ip 不做邻居解析，网关仅是锚点）
        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(100, 64, 255, 254))
            .expect("路由表默认空");
        (cfg.logf)(&format!(
            "intercept: 过境拦截就绪（隧道IP {}；豁免=转投本机同端口；TCP 并发上限 {}）",
            cfg.tunnel_ip, MAX_CONNS
        ));
        // 整形状态**独立行**（评审 r2-1.4：E5 是与 Go 基线逐串对齐的判据行——
        // Rust 侧扩展后缀会让 INTEROP-CRITERIA 的串匹配面失效）。
        match cfg.tx_shape {
            Some(s) => {
                (cfg.logf)(&format!(
                    "intercept: 发送整形=开（rate={}MiB/s burst={}KiB；单拍倾泻钳突发内、余量下拍续传）",
                    s.rate / (1024 * 1024),
                    s.burst / 1024
                ))
            }
            None => (cfg.logf)(
                "intercept: 发送整形=关（HOMEWAY_TX_SHAPING 消融臂或单测直通面）",
            ),
        }
        let dns_rx = cfg.dns_events.take();
        let tx_credit0 = cfg.tx_shape.map(|s| s.burst as f64).unwrap_or(0.0);
        Self {
            cfg,
            stats,
            iface,
            sockets: SocketSet::new(vec![]),
            device,
            flows: HashMap::new(),
            by_five: HashMap::new(),
            by_rw_port: HashMap::new(),
            next_flow: 1,
            next_rw_port: 20000,
            halted: false,
            pollfds: Vec::new(),
            poll_index: Vec::new(),
            reactor_waiters: false,
            reactor_win_pumps: 0,
            reactor_win_peak_fds: 0,
            reactor_win_peak_wall: Duration::ZERO,
            served_ports: std::collections::HashSet::new(),
            tx_out: Vec::new(),
            tx_deferred: std::collections::VecDeque::new(),
            tx_deferred_bytes: 0,
            tx_credit: tx_credit0,
            tx_last_refill: Instant::now(),
            tx_win_pumps: 0,
            tx_win_released: 0,
            tx_win_defer_peak: (0, 0),
            dns_faces: None,
            dns_rx,
            udp_seq: 0,
            dial_fail_seen: HashMap::new(),
            reasm: Reassembler::new(),
            tx_frag: HashMap::new(),
            frag_limit_seen: (0, false),
            frag_timeout_seen: (0, false),
            last_cc_stats: None,
            obs_snaps: HashMap::new(),
            time0: Instant::now(),
            smol_now: SmolInstant::from_millis(0),
            rx_scratch: Box::new([0u8; 64 * 1024]),
        }
    }

    /// DNS 面装配（attach 后单独调——监听 socket 要落在本拦截栈的 SocketSet 里）。
    /// 对应 Go `listenTunnelDNS`（FIX-60：监听面在隧道栈内，不占 host 端口）。
    pub fn attach_dns(&mut self) {
        if self.cfg.dns.is_none() {
            return;
        }
        let mut served = std::mem::take(&mut self.served_ports);
        self.dns_faces = Some(DnsFaces::attach(
            self.cfg.tunnel_ip,
            self.cfg.dns_resolve_port,
            &mut self.sockets,
            &mut served,
        ));
        self.served_ports = served;
    }

    fn now_smol(&mut self) -> SmolInstant {
        self.smol_now = SmolInstant::from_millis(self.time0.elapsed().as_millis() as i64);
        self.smol_now
    }

    /// 分片丢弃的统一处置点（F2/F4）：记账（恒等式结构保证）→ 限频日志（仅超限/超时）
    /// → 超时且首片在位 ⇒ 产 ICMP type 11 code 1。
    fn note_frag_drop(&mut self, d: Dropped) {
        self.stats.note_frag_drop(d.reason, d.packets);
        match d.reason {
            DropReason::Limit => self.log_frag_limit(d.src),
            DropReason::Timeout => {
                self.log_frag_timeout(d.packets);
                if let Some(orig) = d.icmp_orig.as_deref() {
                    // 只回已认证 peer（走 tx_out → route_encap 的隧道路径，不经公网）。
                    if let Some(icmp) = nat::build_icmp_reassembly_timeout(orig) {
                        self.tx_out.push(icmp);
                    }
                }
            }
            // 重叠/冲突/非法：**纯计数不记行**（可被对端逐包诱发——自我放大；已登记）
            DropReason::Overlap | DropReason::Bad => {}
        }
    }

    /// 超限日志（节流：首行 + 每 100 次一条；键 = 纯 kind——**src 只进行文不进键**）。
    fn log_frag_limit(&mut self, src: Ipv4Addr) {
        let ctxs = self.reasm.len();
        let per = self.reasm.count_src(src);
        let (log, seen) = {
            let e = &mut self.frag_limit_seen;
            e.0 += 1;
            let log = !e.1 || e.0.is_multiple_of(100);
            e.1 = true;
            (log, e.0)
        };
        if log {
            (self.cfg.logf)(&format!(
                "intercept: 分片重组超上限（上下文 {ctxs}/{}，源 {src} {per}/{}）——新报文暂不可重组（累计丢 {seen} 片）",
                reasm::REASM_MAX_CTX,
                reasm::REASM_MAX_PER_SRC,
            ));
        }
    }

    /// 超时日志（节流同款；`secs` = 超时窗，供排障直接读）。
    fn log_frag_timeout(&mut self, packets: u64) {
        let (log, seen) = {
            let e = &mut self.frag_timeout_seen;
            e.0 += 1;
            let log = !e.1 || e.0.is_multiple_of(100);
            e.1 = true;
            (log, e.0)
        };
        if log {
            (self.cfg.logf)(&format!(
                "intercept: 分片重组超时（{packets} 片，{}s）——整条丢弃（累计 {seen}）",
                reasm::REASM_TIMEOUT.as_secs(),
            ));
        }
    }

    /// 重组超时清扫（F1.5）：`pump`/`pump_hold` **每拍开头**各一次（上下文 ≤64 ⇒ 线性
    /// 扫可忽略）+ TX 分片表的 TTL/上限兜底。`close()` 走 `reasm.clear()`（静默）。
    fn sweep_reasm(&mut self, now: Instant) {
        for d in self.reasm.sweep(now) {
            self.note_frag_drop(d);
        }
        self.sweep_tx_frag(now);
    }

    /// TX 分片表清扫（F5-d）：TTL（首片丢失时兜底）+ 上限（淘汰最老）。正常态表内 ≤1 项。
    fn sweep_tx_frag(&mut self, now: Instant) {
        self.tx_frag
            .retain(|_, v| now.duration_since(v.created) < TX_FRAG_TTL);
        while self.tx_frag.len() > TX_FRAG_MAX {
            if !self.evict_oldest_tx_frag() {
                break;
            }
        }
    }

    /// 淘汰最老的 TX 分片表项（返回是否真淘汰）。
    fn evict_oldest_tx_frag(&mut self) -> bool {
        let oldest = self.tx_frag.iter().min_by_key(|(_, v)| v.created).map(|(k, _)| *k);
        match oldest {
            Some(k) => {
                self.tx_frag.remove(&k);
                true
            }
            None => false,
        }
    }

    /// RX：WG decap 出的明文包（源校验已过）。
    ///
    /// Q-K F1：分片语义的**唯一入口**——只解 IP 头（`Ipv4FragHdr`，不碰 L4）：
    /// - 畸形 ⇒ 静默丢（现状口径）；
    /// - 非分片（含 DF-only） ⇒ [`Self::route_plain`]（与现状逐字节同路径）；
    /// - 分片 ⇒ 有界重组；**完成**的整包**走同一个** `route_plain`（结构上不存在
    ///   「分片旁路」）；未完成/被丢弃 ⇒ 只碰 `self.reasm` 与计数。
    ///
    /// Q-B F7 的性质在此**保持**：分片自身绝不进入 demux / `by_five` / 建流——建流
    /// 唯一入口仍是 `route_plain` 的 `tcp_new`/`udp_new`。
    pub fn on_plain(&mut self, pkt: Vec<u8>) {
        let Some(frag) = nat::Ipv4FragHdr::parse(&pkt) else {
            return; // 畸形：静默丢（IP 层）
        };
        if !frag.hdr.is_fragment() {
            self.route_plain(pkt);
            return;
        }
        let res = self.reasm.push(frag, Instant::now());
        for d in res.dropped {
            self.note_frag_drop(d);
        }
        if let Some(full) = res.done {
            self.stats.incr_frag_reasm();
            self.route_plain(full);
        }
    }

    /// `on_plain` 的另一半：原 `on_plain` 的 parse 之后**全部逻辑**（demux → `by_five`
    /// → `rewrite_dst` → 新建）——**唯一投递入口**（正常包与重组完成的整包同路）。
    fn route_plain(&mut self, pkt: Vec<u8>) {
        let Some(v) = Ipv4View::parse(&pkt) else {
            return; // 畸形：静默丢（IP 层）
        };
        // F7 分支已上移到 `on_plain` 的 `Ipv4FragHdr` 门（唯一分片判定点）。此断言是
        // **绊线**（测试期暴露不一致，release 无行为）：两处读的是同一对字节
        // `pkt[6..8]`、判定式同为 `off != 0 || mf`。
        debug_assert!(!v.is_fragment(), "route_plain 收到分片（分片门在 on_plain）");
        let l4_off = if v.proto == 6 {
            20
        } else if v.proto == 17 {
            8
        } else {
            0
        };
        let payload_start = v.header_len + l4_off;
        let snapshot = View5 {
            src: v.src,
            src_port: v.src_port,
            dst: v.dst,
            dst_port: v.dst_port,
            proto: v.proto,
            tcp_flags: v.tcp_flags,
            tcp_seq: v.tcp_seq,
            tcp_ack: v.tcp_ack,
            udp_payload: (payload_start, v.total_len),
        };
        if v.dst == self.cfg.tunnel_ip && self.served_ports.contains(&v.dst_port) {
            // demux 先投栈内**真 listener**（DNS :53/:5300——3d 建并登记）；
            // 未登记的隧道 IP 端口走 NAT 豁免路径（files/term/speedtest 经拦截层转投——
            // Go 的 SetTransportProtocolHandler 也在 demux 未命中后才接手，同序）
            self.device.rx_push(&pkt);
            return;
        }
        // F9：仅 TCP(6)/UDP(17) 建会话；其它协议（ICMP 等）丢弃——不再走 udp_new 建
        // 端口 0 会话（Go 仅注册 TCP/UDP handler，其余交 netstack 兜底；本层直接丢弃）。
        let proto = match v.proto {
            6 => Proto::Tcp,
            17 => Proto::Udp,
            _ => return,
        };
        let five = (v.src, v.src_port, v.dst, v.dst_port, v.proto);
        if let Some(&flow) = self.by_five.get(&five) {
            let Some(f) = self.flows.get_mut(&flow) else {
                return;
            };
            let rw = f.rw_port;
            let is_dialing = matches!(f.phase, Phase::Dialing { .. });
            if is_dialing {
                // TCP 建连窗口：包进缓存（重放用）——上限丢最新（≤4）。UDP 无此窗口
                //（拨号同步完成，后续包恒见 Established——reactor-design §三）。
                if proto == Proto::Udp {
                    return;
                }
                let item = pkt;
                if let Phase::Dialing { cache } = &mut f.phase {
                    if cache.len() < SYN_CACHE_MAX {
                        cache.push(item);
                    }
                }
                return;
            }
            // 8n 归因插桩：客户端→出口 ACK 段累计（含通告窗原始 u16）——在 pkt move
            // 前观测（v 借用 pkt）。
            if proto == Proto::Tcp {
                if let Some(f) = self.flows.get_mut(&flow) {
                    f.obs.note_rx_ack(&v);
                }
            }
            let mut p = pkt;
            nat::rewrite_dst(&mut p, self.cfg.tunnel_ip, rw);
            self.device.rx_push(&p);
            if let Some(f) = self.flows.get_mut(&flow) {
                f.last_active = Instant::now();
            }
            return;
        }
        match proto {
            Proto::Tcp => self.tcp_new(&snapshot, pkt),
            Proto::Udp => self.udp_new(&snapshot, pkt),
        }
    }

    /// TCP 新流：拨号先行（设计 §4.1——SYN 缓存不注栈）。
    fn tcp_new(&mut self, v: &View5, pkt: Vec<u8>) {
        if !v.is_tcp_syn() {
            // 未知四元组的非 SYN：回 RST（gVisor HandleUnknownDestinationPacket 同义）
            let rst = build_rst_for(v);
            self.tx_out.push(rst);
            return;
        }
        if self.halted {
            self.tx_out.push(build_rst_for(v));
            return;
        }
        let tcp_flows = self
            .flows
            .values()
            .filter(|f| f.proto == Proto::Tcp)
            .count();
        if tcp_flows >= MAX_CONNS {
            self.stats.incr_reject();
            let rejects = self.stats.rejects();
            (self.cfg.logf)(&format!(
                "intercept: tcp 拒绝 {}:{} ← {}:{}（并发上限 {}，在册 {}，累计拒绝 {}）",
                v.dst,
                v.dst_port,
                v.src,
                v.src_port,
                MAX_CONNS,
                tcp_flows + 1,
                rejects
            ));
            self.tx_out.push(build_rst_for(v));
            return;
        }
        let (kind, target) = self.route_upstream(v.dst, v.dst_port, Proto::Tcp);
        let flow = self.alloc_flow(v, kind, Proto::Tcp, vec![pkt]);
        if kind == Kind::Dns {
            // M3：TCP DNS 腿不走 worker 拨号——直接建栈内 listen + 注入缓存
            //（SYN-ACK 即刻可产）；数据面走进程内代答（dns_tcp_feed）。
            let legs = self
                .flows
                .values()
                .filter(|f| f.kind == Kind::Dns && f.proto == Proto::Tcp)
                .count();
            if legs >= MAX_TCP_DNS_LEGS {
                (self.cfg.logf)(&format!(
                    "intercept: tcp dns {}:{} ← {}:{} 拒绝（并发上限 {}）",
                    v.dst, v.dst_port, v.src, v.src_port, MAX_TCP_DNS_LEGS
                ));
                self.tx_out.push(build_rst_for(v));
                self.remove_flow(flow);
                return;
            }
            self.dns_tcp_establish(flow);
            return;
        }
        self.dial_upstream(flow, target);
    }

    /// TCP DNS 腿建立（Go serveDNSTCP 同义：CreateEndpoint → 「进程内代答」判据行 →
    /// ServeStream；此处 = 建 listen + 注入缓存，后续每拍 service_sockets 喂数据）。
    fn dns_tcp_establish(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let rw = f.rw_port;
        let mut sock = TcpSocket::new(
            tcp::SocketBuffer::new(vec![0u8; FLOW_BUF]),
            tcp::SocketBuffer::new(vec![0u8; FLOW_TX_BUF]),
        );
        sock.set_nagle_enabled(false);
        sock.set_congestion_control(tcp::CongestionControl::Cubic); // R8-8a CUBIC
        sock.set_timeout(Some(smoltcp::time::Duration::from_secs(
            TCP_DNS_IDLE.as_secs(),
        )));
        if let Err(e) = sock.listen(IpEndpoint::new(self.cfg.tunnel_ip.into(), rw)) {
            (self.cfg.logf)(&format!("intercept: tcp listen rw_port {rw} 失败：{e:?}"));
            self.teardown_flow(flow);
            return;
        }
        let h = self.sockets.add(sock);
        let cache = {
            let f = self.flows.get_mut(&flow).expect("刚判存在");
            match std::mem::replace(&mut f.phase, Phase::Established) {
                Phase::Dialing { cache } => cache,
                Phase::Established => Vec::new(), // 防御：alloc_flow 后必为 Dialing
            }
        };
        self.establish(flow, Some(h));
        for mut p in cache {
            nat::rewrite_dst(&mut p, self.cfg.tunnel_ip, rw);
            self.device.rx_push(&p);
        }
        let (orig_dst, client) = {
            let f = self.flows.get(&flow).expect("刚判存在");
            (f.orig_dst, f.client)
        };
        (self.cfg.logf)(&format!(
            "intercept: tcp dns {}:{} ← {}:{}（进程内代答）",
            orig_dst.0, orig_dst.1, client.0, client.1
        ));
    }

    /// TCP DNS 腿数据面：RFC1035 分帧积攒 → 完整报文投 DNS worker（qtcp 计数面；
    /// 应答经 DnsRoute::TcpFlow 回投）。超长帧（>64KB+2B 缓冲界）按对端异常收线。
    ///
    /// 返回 `FeedOutcome::CloseLeg` 表示读到**空 TCP 消息**（`mlen == 0`）——与隧道面
    /// `decode_tcp_frame` + `service_face` 的空帧判异常**同口径收线**（Go 读失败即
    /// `return`、不计 qtcp）。**收线必须由调用方在 `for flow` 循环外统一做**（[门-A1]：
    /// 本函数由 `service_sockets` 在同一次迭代内调用，若就地 `teardown_flow` →
    /// `remove_flow` → `sockets.remove(h)`，该臂后续用快照 `h` 再 `get_mut` 会 panic）。
    fn dns_tcp_feed(&mut self, flow: u64, data: &[u8]) -> FeedOutcome {
        let Some(f) = self.flows.get_mut(&flow) else {
            return FeedOutcome::Ok;
        };
        f.dns_rx.extend_from_slice(data);
        f.last_active = Instant::now();
        // 循环取完整帧（一条读可能含多条报文）
        loop {
            let query = match self.flows.get_mut(&flow) {
                Some(f) => {
                    if f.dns_rx.len() < 2 {
                        return FeedOutcome::Ok;
                    }
                    let mlen = u16::from_be_bytes([f.dns_rx[0], f.dns_rx[1]]) as usize;
                    if mlen == 0 {
                        return FeedOutcome::CloseLeg; // 空 TCP 消息：收线（对齐 Go）
                    }
                    if f.dns_rx.len() < 2 + mlen {
                        return FeedOutcome::Ok;
                    }
                    let query = f.dns_rx[2..2 + mlen].to_vec();
                    f.dns_rx.drain(..2 + mlen);
                    query
                }
                None => return FeedOutcome::Ok,
            };
            let dns = self.cfg.dns.clone().expect("kind=Dns 必有腿");
            let tag = self
                .dns_faces
                .as_mut()
                .map(|faces| faces.route_tag(DnsRoute::TcpFlow(flow)))
                .unwrap_or(0);
            if tag != 0 && dns.submit_tcp(tag, query) == SubmitOutcome::Dropped {
                // F2：丢弃路径回收 tag（否则 pending 项永不回收）
                if let Some(faces) = self.dns_faces.as_mut() {
                    faces.take_route(tag);
                }
            }
        }
    }

    /// 豁免/过境/DNS 的 upstream 决策（Go serveTCP 的 target/LocalServices 同口径）。
    fn route_upstream(&self, dst: Ipv4Addr, port: u16, proto: Proto) -> (Kind, DialTarget) {
        if self.cfg.dns.is_some() && port == 53 {
            // DNS 腿（FIX-60）：UDP 与 **TCP**（M3，R5 补）都不落地真实网络——
            // 进程内代答，应答源地址 = 原目的（如 8.8.8.8:53）。
            return match proto {
                Proto::Udp => (Kind::Dns, DialTarget::Udp(loopback(port))),
                Proto::Tcp => (Kind::Dns, DialTarget::Tcp(loopback(port))), // 占位：腿不拨号
            };
        }
        if dst == self.cfg.tunnel_ip {
            // 豁免：LocalServices 命中 → UDS（UDP 不查表——Go 同口径）；未命中 → 回环同端口
            if proto == Proto::Tcp {
                if let Some(sock) = self.cfg.local_services.get(&port) {
                    return (Kind::Exempt, DialTarget::Unix(sock.clone()));
                }
            }
            let target = loopback(port);
            return if proto == Proto::Tcp {
                (Kind::Exempt, DialTarget::Tcp(target))
            } else {
                (Kind::Exempt, DialTarget::Udp(target))
            };
        }
        let target = SocketAddrV4::new(dst, port).into();
        if proto == Proto::Tcp {
            (Kind::Transit, DialTarget::Tcp(target))
        } else {
            (Kind::Transit, DialTarget::Udp(target))
        }
    }

    fn alloc_flow(&mut self, v: &View5, kind: Kind, proto: Proto, cache: Vec<Vec<u8>>) -> u64 {
        let flow = self.next_flow;
        self.next_flow += 1;
        let rw = self.alloc_rw_port();
        self.by_rw_port.insert(rw, flow);
        self.by_five
            .insert((v.src, v.src_port, v.dst, v.dst_port, v.proto), flow);
        self.flows.insert(
            flow,
            Flow {
                kind,
                proto,
                client: (v.src, v.src_port),
                orig_dst: (v.dst, v.dst_port),
                rw_port: rw,
                sock: None,
                phase: Phase::Dialing { cache },
                last_active: Instant::now(),
                io: None,
                tx_backlog: VecDequeLite::new(),
                fin_pending: false,
                udp_seq_of: 0,
                counted: false,
                udp_replied: false,
                syn_seq: v.tcp_seq,
                dns_rx: Vec::new(),
                obs: TcpObs::default(),
            },
        );
        flow
    }

    /// rw_port 分配（避开在用与低端口；环形递增）。
    fn alloc_rw_port(&mut self) -> u16 {
        loop {
            let p = self.next_rw_port;
            self.next_rw_port = if self.next_rw_port >= 61000 {
                20000
            } else {
                self.next_rw_port + 1
            };
            if !self.by_rw_port.contains_key(&p) {
                return p;
            }
        }
    }

    /// 非阻塞拨号（reactor-design §二三分类）：即时成功与在途都收进 ReactorIo；
    /// **即时成功直接进 `dial_accept`**（与 POLLOUT 验收统一收口——评审 R-1：即时
    /// 成功不产生任何 poll 事件，若验收只挂 POLLOUT，流会卡到 10s 死线）。
    fn dial_upstream(&mut self, flow: u64, target: DialTarget) {
        let udp = matches!(target, DialTarget::Udp(_));
        let (fd, conn) = match dial_nonblocking(&target) {
            Ok(DialOutcome::Connected(fd)) => {
                self.place_io(flow, fd, udp, None, Instant::now());
                self.dial_accept(flow);
                return;
            }
            Ok(DialOutcome::InProgress(fd)) => (fd, ConnState::InProgress),
            Ok(DialOutcome::Retry(fd)) => (fd, ConnState::Retry),
            Err(_) => {
                self.on_dial_failed(flow);
                return;
            }
        };
        self.place_io(flow, fd, udp, Some(conn), Instant::now() + DIAL_TIMEOUT);
    }

    /// ReactorIo 落位（流已不在表 = 已被收口——fd 随 drop 关闭；deadline 只在
    /// conn=Some 时被读，即时成功路径传 now() 占位）。
    fn place_io(&mut self, flow: u64, fd: OwnedFd, udp: bool, conn: Option<ConnState>, deadline: Instant) {
        if let Some(f) = self.flows.get_mut(&flow) {
            if f.io.is_none() {
                f.io = Some(ReactorIo {
                    fd,
                    udp,
                    out_tcp: VecDequeLite::new(),
                    out_udp: VecDeque::new(),
                    conn,
                    dial_deadline: deadline,
                    dead: false,
                });
            }
        }
    }

    /// InProgress 态的 connect 验收（POLLOUT/POLLERR/POLLHUP 任一触发）。
    /// conn 的清零点两处（评审 r2-低8 口径订正）：本函数（InProgress 验收）与
    /// retry_connect 的 Accepted（Retry 重拨成功）——都要立即走 dial_accept，
    /// 否则 POLLIN 兴趣挂不上 = 流死。
    fn on_connect_done(&mut self, flow: u64) {
        let fd_ok = {
            let Some(f) = self.flows.get_mut(&flow) else { return };
            let Some(io) = f.io.as_mut() else { return };
            if io.conn.is_none() {
                return;
            }
            let mut soerr: libc::c_int = 0;
            let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
            let rc = unsafe {
                libc::getsockopt(
                    io.fd.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_ERROR,
                    &mut soerr as *mut _ as *mut libc::c_void,
                    &mut len,
                )
            };
            if rc != 0 {
                false
            } else if soerr == 0 || soerr == libc::EISCONN {
                io.conn = None;
                true
            } else {
                false // SO_ERROR≠0 = 拨号失败（ECONNREFUSED 等）
            }
        };
        if fd_ok {
            self.dial_accept(flow);
        } else {
            self.on_dial_failed(flow);
        }
    }

    /// Retry 态重拨 connect（EAGAIN——Linux UDS 监听队列满；每拍一次、共用死线、
    /// 同一 fd；目标按流的原始目的重推导——route_upstream 是纯决策面）。
    fn retry_connect(&mut self, flow: u64) {
        // 两段借用：先只读取目标（route_upstream 要 &self），再可变重拨
        let pack = {
            let Some(f) = self.flows.get(&flow) else { return };
            let Some(io) = f.io.as_ref() else { return };
            if !matches!(io.conn, Some(ConnState::Retry)) {
                return;
            }
            let target = self.route_upstream(f.orig_dst.0, f.orig_dst.1, f.proto).1;
            target_sockaddr(&target).ok()
        };
        let Some((_, _, addr, len, _)) = pack else {
            return;
        };
        // 三态：连上（验收收口）/ 仍在队列外（下拍再试）/ 真 fail（同 DialFailed 收口）
        enum Step {
            Accepted,
            Still,
            Failed,
        }
        let step = {
            let Some(f) = self.flows.get_mut(&flow) else { return };
            let Some(io) = f.io.as_mut() else { return };
            if !matches!(io.conn, Some(ConnState::Retry)) {
                return;
            }
            let mut step = Step::Still;
            loop {
                let rc = unsafe {
                    libc::connect(io.fd.as_raw_fd(), &addr as *const _ as *const libc::sockaddr, len)
                };
                if rc == 0 {
                    step = Step::Accepted;
                    break;
                }
                match std::io::Error::last_os_error().raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(libc::EAGAIN) => break, // 仍在队列外——下拍再试
                    Some(libc::EISCONN) => {
                        step = Step::Accepted;
                        break;
                    }
                    _ => {
                        step = Step::Failed;
                        break;
                    }
                }
            }
            if matches!(step, Step::Accepted) {
                io.conn = None;
            }
            step
        };
        match step {
            Step::Accepted => self.dial_accept(flow),
            Step::Failed => self.on_dial_failed(flow),
            Step::Still => {}
        }
    }

    /// UDP 新会话（首包）：置在建位 + pending；DNS 腿直接就绪，其余投 worker 拨号。
    fn udp_new(&mut self, v: &View5, pkt: Vec<u8>) {
        if self.halted {
            if let Some(icmp) = nat::build_icmp_unreachable(&pkt) {
                self.tx_out.push(icmp);
            }
            return;
        }
        let udp_flows = self
            .flows
            .values()
            .filter(|f| f.proto == Proto::Udp)
            .count();
        if udp_flows >= MAX_UDP_SESSIONS {
            self.stats.incr_reject();
            (self.cfg.logf)(&format!(
                "intercept: udp 会话上限 {} 已满，丢 {}:{} ← {}:{}",
                MAX_UDP_SESSIONS, v.dst, v.dst_port, v.src, v.src_port
            ));
            if let Some(icmp) = nat::build_icmp_unreachable(&pkt) {
                self.tx_out.push(icmp);
            }
            return;
        }
        let (kind, target) = self.route_upstream(v.dst, v.dst_port, Proto::Udp);
        // 首包缓存**纯载荷**（DNS 腿的就绪重放取它）；拨号同步完成（bind+connect 本地
        // 即时）——pending 重放窗口坍缩为零（reactor-design §三）
        let (pa, pb) = v.udp_payload;
        let first = pkt[pa.min(pkt.len())..pb.min(pkt.len())].to_vec();
        let flow = self.alloc_flow(v, kind, Proto::Udp, vec![first]);
        if kind == Kind::Dns {
            // 进程内腿：无拨号——直接「就绪」+ 重放
            self.udp_ready(flow);
            return;
        }
        self.dial_upstream(flow, target);
    }

    /// UDP 会话就绪（拨号验收或 DNS 腿）：重放 first+pending（upstream 直投，不注栈）。
    fn udp_ready(&mut self, flow: u64) {
        let (replays, kind, orig_dst, client) = {
            let Some(f) = self.flows.get_mut(&flow) else {
                return;
            };
            let Phase::Dialing { cache } = &f.phase else {
                return;
            };
            (cache.clone(), f.kind, f.orig_dst, f.client)
        };
        // 建立收口（incr_flow 唯一入口——F5）：置 counted + phase→Established。
        self.establish(flow, None);
        // 栈内 udp socket 就位（DNS 进程内腿无 OS socket——dial_accept 之外也要建；
        // ensure 幂等，验收路径重复调用无害）
        self.ensure_udp_socket(flow);
        // 会话号 + 判据行（E12 建立）——先记后写（Rust 现状口径，与 Go 真源不同序，
        // reactor-design §三 R-4：不得借对齐之名重排）
        self.udp_seq += 1;
        let seq = self.udp_seq;
        // F5-1：把真实会话号写回 flow——E12 关闭行打本会话号（此前恒 #0 是缺陷，
        // 见 `docs/INTEROP-CRITERIA.md` 判据变更记录）。
        if let Some(f) = self.flows.get_mut(&flow) {
            f.udp_seq_of = seq;
        }
        (self.cfg.logf)(&format!(
            "udp intercept: 会话 #{seq} {} 建立（{}:{} ← {}:{}）",
            kind.as_str(),
            orig_dst.0,
            orig_dst.1,
            client.0,
            client.1
        ));
        if kind == Kind::Dns {
            // DNS 腿：逐包投进程内代答（**异步**——H3 整改：阻塞面全长 2.5s/查询，
            // 不得占驱动线程；应答经回投通道在 pump 拍内路由回该 flow）。
            // Go `Answer()` 直调口径：不计 q 计数（q 只在隧道栈 UDP listener 面计）。
            let dns = self.cfg.dns.clone().expect("kind=Dns 必有腿");
            for q in replays {
                let tag = self
                    .dns_faces
                    .as_mut()
                    .map(|f| f.route_tag(DnsRoute::UdpFlow(flow)))
                    .unwrap_or(0);
                if tag != 0 && dns.submit_leg(tag, q) == SubmitOutcome::Dropped {
                    // F2：丢弃路径回收 tag
                    if let Some(faces) = self.dns_faces.as_mut() {
                        faces.take_route(tag);
                    }
                }
            }
            return;
        }
        // 重放进待写数据报队列 + 立即试写（reactor——无 worker 命令面）；首包写失败
        // → on_upstream_eof → finish_udp（E12 关闭 + udpNoReply——§三三段次序③）
        for p in replays {
            if let Some(f) = self.flows.get_mut(&flow) {
                if let Some(io) = f.io.as_mut() {
                    io.out_udp.push_back(p);
                }
            }
        }
        self.flush_out(flow);
    }

    /// TCP DNS 腿应答回投：RFC1035 帧化（2B BE 长度 + 报文）进 tx_backlog——
    /// service_sockets 的 backlog 续写会把它排进栈内 socket（与 worker 上行同路径，
    /// 背压/部分写语义一致）。
    fn dns_tcp_send(&mut self, flow: u64, resp: &[u8]) {
        // F8：长度域 u16 是协议事实——超 65535 回绕会错位；对齐 Go
        //（`writeTCPMessage` 返错 → `ServeStream` 返回 → 连接关闭）= 收线。
        if resp.len() > u16::MAX as usize {
            self.teardown_flow(flow);
            return;
        }
        let Some(f) = self.flows.get_mut(&flow) else {
            return;
        };
        let mut frame = Vec::with_capacity(2 + resp.len());
        frame.extend_from_slice(&(resp.len() as u16).to_be_bytes());
        frame.extend_from_slice(resp);
        f.tx_backlog.push(&frame);
        f.last_active = Instant::now();
    }

    /// 把一段数据经栈内 udp socket 回投客户端（DNS 应答/UpstreamData 共用）。
    fn udp_send_to_client(&mut self, flow: u64, data: &[u8]) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let Some(h) = f.sock else { return };
        let ep = IpEndpoint::new(f.client.0.into(), f.client.1);
        let sock = self.sockets.get_mut::<UdpSocket>(h);
        if sock.send_slice(data, ep).is_err() {
            // F3：栈 tx 满（应答/回投写不进）不再静默——计数（丢新）。
            self.stats.incr_udp_drop();
        }
        if let Some(f) = self.flows.get_mut(&flow) {
            f.last_active = Instant::now();
        }
    }

    /// 驱动拍：reactor 就绪处理 → DNS 应答回投 → 栈 poll → TX 反重写 → idle/水位 →
    /// DNS 面服务 → 返回出站明文包（引擎 encap）。
    pub fn pump(&mut self) -> Vec<Vec<u8>> {
        // ⓪ 分片超时清扫（F1.5：每拍开头；本拍起的重组判定看到的是已回收的槽位状态）
        self.sweep_reasm(Instant::now());
        // ① reactor 拍（upstream fd：Retry 重拨/读/写续传/connect 验收/EOF）
        self.reactor_turn();
        // ①' DNS 应答回投（DnsProxy worker 异步产出；H3——驱动线程只做路由写回）
        self.drain_dns();
        self.cc_stats_line();
        // ② 栈 poll
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        // ③ TX：反重写
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        // ④ 栈内 socket 数据面（读→待写缓冲/关闭推进）+ DNS 面服务 + idle/死线看门狗
        self.service_sockets();
        self.service_dns();
        self.reap_idle();
        self.refresh_waiters(); // pump 尾刷新（R-5：本拍新攒的待写量下轮引擎即见）
        // ⑤ 再 poll 一轮（③④ 产生的状态变化让 ACK/数据尽早在本拍出站）
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        // ⑥ 发送整形（R8-3 8i）：本拍产物并入滞留 FIFO，按字节令牌桶从头释放——
        // 余量下拍续传（驱动线程每拍必调本函数）。None = 关臂直通（行为与
        // 整形前逐字节等价）。
        self.tx_shape_release(false)
    }

    /// 高水位背压拍（P1 两级前置背压；引擎面在发送 ring 高水位时以本函数替代
    /// pump）：与 pump 同拍序，唯一差别 = ⑥ 的整形释放退化为「只并入不释放」
    ///——本拍产物并入滞留 FIFO 后**不扣 credit、不推时刻表**（整形状态原样，
    /// 下拍 ring 水位回落后照常释放——无双重记账）。包留 FIFO = 真背压不丢包，
    /// 发送 ring 的满丢成为最后兜底。整形关臂（无 FIFO）保持直通——该臂满丢
    /// 即唯一兜底（消融态接受）。
    pub fn pump_hold(&mut self) -> Vec<Vec<u8>> {
        // ⓪ 分片清扫（与 pump 同拍序）
        self.sweep_reasm(Instant::now());
        // ① reactor 拍
        self.reactor_turn();
        self.drain_dns();
        self.cc_stats_line();
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        self.service_sockets();
        self.service_dns();
        self.reap_idle();
        self.refresh_waiters();
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        // ⑥' 只并入不释放（tx_shape_release 的并路径同款记账；整形关 = 直通）
        match self.cfg.tx_shape {
            None => std::mem::take(&mut self.tx_out),
            Some(_) => {
                let produce = std::mem::take(&mut self.tx_out);
                let produce = self.filter_defer(produce);
                if produce.is_empty() {
                    return Vec::new();
                }
                let n = produce.len();
                let b = produce.iter().map(|p| p.len()).sum::<usize>();
                self.tx_win_defer_peak = (
                    self.tx_win_defer_peak.0.max(self.tx_deferred.len() + n),
                    self.tx_win_defer_peak.1.max(self.tx_deferred_bytes + b),
                );
                self.tx_deferred.extend(produce);
                self.tx_deferred_bytes += b;
                Vec::new()
            }
        }
    }

    /// 并入 `tx_deferred` 前的非 TCP 丢新预筛（F4）。逐包取内层 IP proto——
    /// **`pkt.get(9)` 而非裸索引**（`tx_out` 允许非 IPv4/短包：`on_tx` parse 失败
    /// 原样 push，测试也 push 过 8B vec；裸索引会 panic）。
    /// - TCP（proto 6）：恒不丢（字节流丢字节 = 流错位，比内存增长更严重）；
    /// - 非 TCP（UDP/DNS/ICMP 等）：并入后总字节会超 `TX_DEFER_MAX_BYTES` ⇒ 丢新 + 计数；
    /// - 取不到 proto（短包/非 IPv4）：放行不计数（不属数据面）。
    fn filter_defer(&mut self, produce: Vec<Vec<u8>>) -> Vec<Vec<u8>> {
        let mut out = Vec::with_capacity(produce.len());
        let mut bytes = self.tx_deferred_bytes;
        for p in produce {
            if let Some(&proto) = p.get(9) {
                if proto != 6 && bytes + p.len() > TX_DEFER_MAX_BYTES {
                    self.stats.incr_shape_drop();
                    continue;
                }
            }
            bytes += p.len();
            out.push(p);
        }
        out
    }

    /// 整形释放（`flush_all` = 收工宽限形态——评审 r2-1.1：宽限路径的原始语义是
    /// 「尽快把尾数据/FIN 送出去」（M2 判据），整形在这里没有收益还会丢尾包——
    /// 原实现每拍只拿令牌放得下的部分，宽限循环在流清空/到点即 break，滞留余量
    /// 随对象 drop = 静默丢尾数据）。返回本拍上线包集。
    fn tx_shape_release(&mut self, flush_all: bool) -> Vec<Vec<u8>> {
        match self.cfg.tx_shape {
            None => std::mem::take(&mut self.tx_out),
            Some(shape) => {
                let now = Instant::now();
                let dt = now.duration_since(self.tx_last_refill).as_secs_f64();
                self.tx_last_refill = now;
                let produce = std::mem::take(&mut self.tx_out);
                if flush_all {
                    // 宽限全量释放：deadline 由收工侧兜底，滞留清空（记账面同步）。
                    // FIFO 保序：滞留（更早产出）在前、本拍产物在后。宽限面**不套**
                    // F4 上限——尾数据/FIN 尽快上线（M2）优先于内存钳制。
                    let n_defer = self.tx_deferred.len() + produce.len();
                    let b_defer = self.tx_deferred_bytes
                        + produce.iter().map(|p| p.len()).sum::<usize>();
                    self.tx_win_defer_peak = (
                        self.tx_win_defer_peak.0.max(n_defer),
                        self.tx_win_defer_peak.1.max(b_defer),
                    );
                    let mut out = Vec::with_capacity(self.tx_deferred.len() + produce.len());
                    for p in self.tx_deferred.drain(..) {
                        out.push(p);
                    }
                    out.extend(produce);
                    self.tx_deferred_bytes = 0;
                    self.tx_win_released += out.len() as u64;
                    out
                } else {
                    self.tx_win_pumps += 1;
                    // F4：并入前非 TCP 丢新预筛（TCP 不丢；短包放行不计数）
                    let produce = self.filter_defer(produce);
                    // 峰值统计在过滤之后（L1：否则峰值可读到 > TX_DEFER_MAX_BYTES）
                    let n_defer = self.tx_deferred.len() + produce.len();
                    let b_defer = self.tx_deferred_bytes
                        + produce.iter().map(|p| p.len()).sum::<usize>();
                    self.tx_win_defer_peak = (
                        self.tx_win_defer_peak.0.max(n_defer),
                        self.tx_win_defer_peak.1.max(b_defer),
                    );
                    let (out, run) = shape_slice(
                        &mut self.tx_deferred,
                        produce,
                        ShapeRun {
                            credit: self.tx_credit,
                            deferred_bytes: self.tx_deferred_bytes,
                        },
                        shape,
                        dt,
                    );
                    self.tx_deferred_bytes = run.deferred_bytes;
                    self.tx_credit = run.credit;
                    self.tx_win_released += out.len() as u64;
                    out
                }
            }
        }
    }

    /// 发送侧吞吐观测行（verbose/dlogf 面；仅存在在途 TCP 流时打——真机吞吐排障的
    /// 关键窗口：栈内 CUBIC 的在途/未收账面 + backlog）。R8-8a：垫片退役后 cwnd/
    /// 减窗计数不再可观测（栈内私有），改看 send_queue（tx_buffer 存量 = 上线在途
    /// + 待发）与 backlog。
    fn cc_stats_line(&mut self) {
        let now = Instant::now();
        if self
            .last_cc_stats
            .map(|t| now.duration_since(t) < Duration::from_secs(5))
            .unwrap_or(false)
        {
            return;
        }
        self.last_cc_stats = Some(now);
        // reactor 剂量面（评审 R-6/R-9）：每拍 O(N) poll 扫描与整循环停顿的观测窗
        //（名下 fd 峰值/拍频/单拍 wall 峰值；与 TCP 无关——纯 UDP 也打）。
        {
            let (pumps, fds, wall) = (
                self.reactor_win_pumps,
                self.reactor_win_peak_fds,
                self.reactor_win_peak_wall,
            );
            (self.reactor_win_pumps, self.reactor_win_peak_fds, self.reactor_win_peak_wall) =
                (0, 0, Duration::ZERO);
            if pumps > 0 && fds > 0 {
                (self.cfg.logf)(&format!(
                    "intercept: reactor 观测 pump={pumps}/5s（均周期 {:.2}ms）名下fd峰={fds} 单拍峰={}µs",
                    5000.0 / pumps as f64,
                    wall.as_micros(),
                ));
            }
        }
        // F2/[门-A4] 观测面：DNS 待答路由表在途量（阈值化——正常在途不噪声；持续高位 =
        // 回执回收失灵）。**独立 additive 行**，不改任何既有行文。
        if let Some(faces) = self.dns_faces.as_ref() {
            let pending = faces.pending_len();
            if pending > DNS_PENDING_WARN {
                (self.cfg.logf)(&format!(
                    "intercept: dns 待答路由表在途 {pending} 条（> {DNS_PENDING_WARN}——回执回收失灵信号面）"
                ));
            }
        }
        // 整形窗口计数器先取先清（评审 r2-5.3：此前清零在 busiest 分支内——
        // 无活跃 TCP 的阶段峰值跨窗累计，首个 TCP 窗口会报出跨分钟的「本窗」）。
        let shape_cur = if self.cfg.tx_shape.is_some() {
            let cur = self.tx_deferred.len();
            let peak = self.tx_win_defer_peak;
            let rel = self.tx_win_released;
            let pumps = self.tx_win_pumps;
            self.tx_win_defer_peak = (0, 0);
            self.tx_win_released = 0;
            self.tx_win_pumps = 0;
            Some((cur, peak, rel, pumps))
        } else {
            None
        };
        let mut active = 0usize;
        let mut busiest: Option<(usize, usize)> = None; // (send_queue 存量, backlog)
        for f in self.flows.values() {
            if f.proto != Proto::Tcp || f.sock.is_none() {
                continue;
            }
            active += 1;
            let Some(h) = f.sock else { continue };
            let sq = self.sockets.get_mut::<TcpSocket>(h).send_queue();
            let cur = (sq, f.tx_backlog.remaining().len());
            if busiest.as_ref().map(|b| cur.0 > b.0).unwrap_or(true) {
                busiest = Some(cur);
            }
        }
        // 整形观测独立行（评审 r2-4.4：cc 行只在有活跃 TCP 流时打——纯 UDP transit
        // 场景 pacing 同样生效，est/pace 读数不能跟着 TCP 走；且 Fixed 档的 est 无
        // 驱动关系，混打易误读）。
        if let Some((cur, peak, rel, pumps)) = shape_cur {
            // 「每次唤醒放行包数」= 有效放行率的判读面（评审 r1-1.1：间隔低于驱动
            // 循环固定成本时，有效放行率由循环容量界定——均包/拍 贴 1 即该形态）。
            let per_pump = rel as f64 / pumps.max(1) as f64;
            (self.cfg.logf)(&format!(
                "intercept: 整形观测 滞留={cur}包/{}KB 峰值={}包/{}KB 窗释={rel}(均{per_pump:.1}包/拍)",
                self.tx_deferred_bytes / 1024,
                peak.0,
                peak.1 / 1024,
            ));
        }
        if let Some((sq, backlog)) = busiest {
            let shape_note = shape_cur
                .map(|(cur, _peak, _rel, _pumps)| {
                    format!(
                        " 整形滞留={cur}包/{}KB",
                        self.tx_deferred_bytes / 1024,
                    )
                })
                .unwrap_or_default();
            (self.cfg.logf)(&format!(
                "intercept: cc 活跃TCP={active} 最大流 txq={sq}B backlog={backlog}B（smoltcp 0.14 CUBIC）{shape_note}"
            ));
            // 8n 归因观测行：累计发送字节最大的 TCP 流（bulk 测速场景即最大吞吐流）。
            // 窗差分（快照失配/首窗 = 报累计 + RAW 标记）；通告窗 min~max 为流生命周期
            // 值、在途/maxSeg 为窗末现值。判据面注意：本行带「tcp 观测」前缀，与 dialok
            // 判据行（「tcp transit/exempt …（dialok）」）不同前缀不互扰。
            let mut pick: Option<(u64, TcpObs)> = None;
            for (id, f) in self.flows.iter() {
                if f.proto != Proto::Tcp || f.sock.is_none() {
                    continue;
                }
                let obs = f.obs;
                if pick.as_ref().map(|(_, o)| obs.tx_bytes > o.tx_bytes).unwrap_or(true) {
                    pick = Some((*id, obs));
                }
            }
            if let Some((id, obs)) = pick.filter(|(_, o)| o.tx_seg > 0) {
                let snap = self.obs_snaps.get(&id).copied();
                let (dbytes, dack, dackb, ddup, raw) = match snap {
                    Some(s) if s.tx_seg <= obs.tx_seg => (
                        obs.tx_bytes - s.tx_bytes,
                        obs.ack_seg - s.ack_seg,
                        obs.ack_bytes - s.ack_bytes,
                        obs.ack_dup - s.ack_dup,
                        "",
                    ),
                    _ => (obs.tx_bytes, obs.ack_seg, obs.ack_bytes, obs.ack_dup, " RAW"),
                };
                self.obs_snaps.insert(id, TcpObsSnap::from(&obs));
                let win_min = obs.win_min.unwrap_or(0);
                // RAW = 首窗（快照缺失）——流 id 单调不复用，不存在「失配回退」形态。
                (self.cfg.logf)(&format!(
                    "intercept: tcp 观测 发={}KB(maxSeg={}B) ACK={}({}KB确认) dup={} 通告窗u16[min~max/末]={}~{}/{} 在途≈{}KB{raw}",
                    dbytes / 1024,
                    obs.tx_max_seg,
                    dack,
                    dackb / 1024,
                    ddup,
                    win_min,
                    obs.win_max,
                    obs.win_last,
                    obs.inflight_est.max(0) / 1024,
                ));
            }
        }
    }

    /// DNS 应答路由（回投通道 → 栈内 socket / 拦截腿 flow）。
    fn drain_dns(&mut self) {
        loop {
            let reply = match self.dns_rx.as_ref() {
                Some(rx) => match rx.try_recv() {
                    Ok(r) => r,
                    Err(_) => return,
                },
                None => return,
            };
            let Some(faces) = self.dns_faces.as_mut() else {
                continue;
            };
            let Some(route) = faces.take_route(reply.tag) else {
                continue;
            };
            let Some(resp) = reply.resp else { continue }; // 畸形不回包
            match route {
                DnsRoute::UdpFlow(flow) => self.udp_send_to_client(flow, &resp),
                DnsRoute::TcpFlow(flow) => self.dns_tcp_send(flow, &resp),
                DnsRoute::Udp53(from) => {
                    if !faces.deliver_udp53(&mut self.sockets, from, &resp) {
                        // F10：栈 tx 满/无 socket 时写回失败不再静默——计 udp_drop。
                        self.stats.incr_udp_drop();
                    }
                }
                DnsRoute::Tcp(h) => faces.deliver_tcp(&mut self.sockets, h, &resp),
            }
        }
    }

    /// DNS 面服务拍（读查询 → submit worker；监听池推进）。
    fn service_dns(&mut self) {
        let Some(dns) = self.cfg.dns.clone() else {
            return;
        };
        if let Some(faces) = self.dns_faces.as_mut() {
            // F3：TCP 连接读缓冲穿参——`dns_faces`/`sockets`/`rx_scratch` 是不相交字段。
            faces.service(&dns, &mut self.sockets, &mut self.rx_scratch[..]);
            faces.reap(&mut self.sockets);
        }
    }

    /// TX：出口包反重写（Q-K F5-d 起**分片感知**）。
    ///
    /// 三分支（设计 §2-F5-d）：
    /// - **非分片**（`off == 0 && !mf`）⇒ 现状逐字节不变（`by_rw_port` 查找 +
    ///   `rewrite_src` 全量重算）；
    /// - **首片**（`off == 0 && mf`）⇒ 现行查找；命中则改 IP 源 + UDP 源端口，L4 校验和
    ///   用 **RFC 1624 增量更新**（整报文载荷不在本片内——不能全量重算）；随后登记
    ///   TX 分片表；未命中（真 listener 应答）⇒ 不改写、同样登记（值 = 不重写）；
    /// - **非首片**（`off > 0`）⇒ **只改 IP 源 + IP 校验和**（L4 头不在此片）；源地址取值
    ///   查 TX 分片表（同报文全部分片必须携带**同一个**改写后 IP 源，否则客户端重组键
    ///   分裂）。**表未命中 ⇒ 丢片 + 计数**（首片既丢，报文本就无法重组；继续发只会占
    ///   客户端重组槽——丢片比发坏片干净）。
    ///
    /// 修复的缺陷（设计门【高-1】）：修复前无分片门 ⇒ 首片被 `fix_l4_checksum` 按
    /// **首片长度**重算覆盖（校验和坏 ⇒ 客户端静默丢）；非首片的 `src_port` 是载荷
    /// 垃圾、命中流表时会把 IP 源改成**另一条流的原始目的**（重组键分裂 + 载荷前
    /// 2 字节被改写）。
    fn on_tx(&mut self, mut pkt: Vec<u8>) {
        // 头解析先取**副本**（`Ipv4FragHdr` 是 Copy）——随后要可变借用整包做重写。
        let hdr = nat::Ipv4FragHdr::parse(&pkt).map(|f| f.hdr);
        let keep = match hdr {
            None => true,
            Some(h) if !h.is_fragment() => {
                self.tx_rewrite_whole(&mut pkt);
                true
            }
            Some(h) if h.frag_off == 0 => {
                self.tx_rewrite_first_frag(&mut pkt, &h);
                true
            }
            // 非首片：表未命中 ⇒ 丢片（不发坏片）
            Some(h) => self.tx_rewrite_later_frag(&mut pkt, &h),
        };
        if keep {
            self.tx_out.push(pkt);
        }
    }

    /// 非分片（含 `Ipv4FragHdr` 通过但 `Ipv4View` 不可解的形态——**与修复前逐字节
    /// 同行为**：parse 失败原样 push）。
    fn tx_rewrite_whole(&mut self, pkt: &mut [u8]) {
        let Some(v) = Ipv4View::parse(pkt) else {
            return; // 调用方照原样 push（现状口径）
        };
        // 反重写：src=(隧道IP, rw_port) 命中 → src=(orig_dst)；真 listener 应答不重写
        if v.src == self.cfg.tunnel_ip && self.by_rw_port.contains_key(&v.src_port) {
            if let Some(&flow) = self.by_rw_port.get(&v.src_port) {
                if let Some(f) = self.flows.get_mut(&flow) {
                    // 8n 归因插桩：出口→客户端段累计（载荷长度按包体）。
                    if v.proto == 6 {
                        f.obs.note_tx_seg(&v);
                    }
                    let (ip, port) = f.orig_dst;
                    nat::rewrite_src(pkt, ip, port);
                }
            }
        }
    }

    /// 分片首片：反重写（IP 源 + UDP 源端口 + RFC 1624 增量校验和）+ 登记 TX 分片表。
    /// 仅 `proto == 17` 触发重写（TCP 永不触发——MSS 由 MTU 推导，R8；ICMP 等无
    /// 流可命中）；未命中/未重写一律登记 `orig = None`（后续片按「不重写」直通，
    /// 不丢片）。
    fn tx_rewrite_first_frag(&mut self, pkt: &mut [u8], hdr: &nat::Ipv4FragHdr) {
        let mut orig = None;
        if hdr.proto == 17 {
            if let Some(v) = Ipv4View::parse(pkt) {
                if v.src == self.cfg.tunnel_ip && v.proto == 17 {
                    if let Some(&flow) = self.by_rw_port.get(&v.src_port) {
                        if let Some(f) = self.flows.get(&flow) {
                            let (ip, port) = f.orig_dst;
                            nat::rewrite_src_first_fragment(pkt, ip, port);
                            orig = Some((ip, port));
                        }
                    }
                }
            }
        }
        let key = (hdr.dst, hdr.ident, hdr.proto);
        if !self.tx_frag.contains_key(&key) && self.tx_frag.len() >= TX_FRAG_MAX {
            // 上限兜底（正常态 ≤1 项；淘汰最老——不拒新）。**先清 TTL、再淘汰到
            // < 上限**（为本次 insert 留位——否则 insert 后瞬时可达 上限+1）。
            self.sweep_tx_frag(Instant::now());
            while self.tx_frag.len() >= TX_FRAG_MAX {
                if !self.evict_oldest_tx_frag() {
                    break;
                }
            }
        }
        self.tx_frag.insert(key, TxFragVal { orig, created: Instant::now() });
    }

    /// 分片非首片：**只改 IP 源 + IP 校验和**（L4 头不在此片——不碰端口、不碰 L4
    /// 校验和：整报文校验和已由首片的增量更新改好，本片只贡献载荷字节）。返回
    /// `false` = 表未命中（丢片 + 计数）。
    fn tx_rewrite_later_frag(&mut self, pkt: &mut [u8], hdr: &nat::Ipv4FragHdr) -> bool {
        let key = (hdr.dst, hdr.ident, hdr.proto);
        let Some(val) = self.tx_frag.get(&key) else {
            self.stats.incr_tx_frag_drop();
            return false;
        };
        if let Some((ip, _port)) = val.orig {
            pkt[12..16].copy_from_slice(&ip.octets());
            nat::fix_ip_checksum(pkt);
        }
        if !hdr.mf {
            // 末片：精确回收（该报文最后一片——不再有后续片需要表项）
            self.tx_frag.remove(&key);
        }
        true
    }

    /// reactor 一拍（reactor-design §一/§二）：Retry 重拨 → 建兴趣集 → poll(0) →
    /// 就绪分发。就绪集是提示不是契约（fd 全非阻塞 + 电平触发：漏看下拍重报、误看
    /// 读出 EAGAIN 跳过）；每拍处理全部就绪 fd——无饥饿不依赖序。
    ///
    /// **Q-I 尾段 F2 尝试与回退**（2026-10-08）：曾把兴趣集并入引擎唯一 poll
    /// （快照直派）以消本函数每拍的零超时 poll syscall——实测**负收益**（合并后上游
    /// fd 就绪成为引擎唤醒源 ⇒ 拍频 13.2k→21.4k/s，每拍全量 pump 的固定成本被放大；
    /// 两项 poll 样本合计 +18%、进程 CPU +14.5%、up 吞吐 −5.9%），按设计 §4.3 止损
    /// 闸门（<20% 即回退）**整条回退**，恢复本形态。证据见 `docs/reviews/QIt.md`。
    fn reactor_turn(&mut self) {
        let t0 = Instant::now();
        // ① Retry 态重拨（EAGAIN 形态不占 poll 位——评审 R-2）
        let retries: Vec<u64> = self
            .flows
            .iter()
            .filter(|(_, f)| {
                matches!(
                    f.io.as_ref().and_then(|io| io.conn.as_ref()),
                    Some(ConnState::Retry)
                )
            })
            .map(|(id, _)| *id)
            .collect();
        for flow in retries {
            self.retry_connect(flow);
        }
        // ② 兴趣集（复用缓冲——评审 R-6；Retry/已排空 dead 的 fd 不注册）
        self.pollfds.clear();
        self.poll_index.clear();
        for (id, f) in &self.flows {
            let Some(io) = f.io.as_ref() else { continue };
            let Some(ev) = interests_for(io, f.tx_backlog.remaining().len()) else { continue };
            self.pollfds.push(libc::pollfd {
                fd: io.fd.as_raw_fd(),
                events: ev,
                revents: 0,
            });
            self.poll_index.push(*id);
        }
        let n_fds = self.pollfds.len();
        if n_fds > 0 {
            let rc = unsafe { libc::poll(self.pollfds.as_mut_ptr(), n_fds as libc::nfds_t, 0) };
            if rc > 0 {
                let ready: Vec<(u64, libc::c_short)> = self
                    .pollfds
                    .iter()
                    .zip(self.poll_index.iter())
                    .filter(|(pf, _)| pf.revents != 0)
                    .map(|(pf, flow)| (*flow, pf.revents))
                    .collect();
                for (flow, revents) in ready {
                    if revents & libc::POLLNVAL != 0 {
                        // 所有权模型下不可达（fd 命 = io 在）——防御：宁可泄漏不误关
                        if let Some(f) = self.flows.get_mut(&flow) {
                            if let Some(io) = f.io.take() {
                                std::mem::forget(io.fd);
                                (self.cfg.logf)(&format!(
                                    "intercept: reactor POLLNVAL（flow {flow}）——防御摘除"
                                ));
                            }
                        }
                        continue;
                    }
                    let readable = revents & libc::POLLIN != 0;
                    let writable = revents & libc::POLLOUT != 0;
                    let errish = revents & (libc::POLLERR | libc::POLLHUP) != 0;
                    self.on_fd_ready(flow, readable, writable, errish);
                }
            }
        }
        // 剂量面（5s 窗——评审 R-6/R-9：每拍 O(N) 扫描成本与整循环停顿的观测窗）
        self.reactor_win_pumps += 1;
        self.reactor_win_peak_fds = self.reactor_win_peak_fds.max(n_fds);
        self.reactor_win_peak_wall = self.reactor_win_peak_wall.max(t0.elapsed());
    }

    /// 就绪分发（reactor-design §二 revents 表）：connect 在途 → 验收；
    /// POLLOUT → 待写续传；POLLIN / ERR·HUP（读侧 EOF 的两条发现路径）→ 读。
    fn on_fd_ready(&mut self, flow: u64, readable: bool, writable: bool, errish: bool) {
        let connecting = self
            .flows
            .get(&flow)
            .and_then(|f| f.io.as_ref())
            .map(|io| io.conn.is_some())
            .unwrap_or(false);
        if connecting {
            self.on_connect_done(flow);
            return;
        }
        if writable {
            self.flush_out(flow);
        }
        if readable || errish {
            self.read_upstream(flow);
        }
    }

    /// 读 upstream：TCP/UDS 读进 tx_backlog（水位门控）后即试灌栈内 socket；UDP
    /// 逐数据报回投（应用层无预算——内核 rcvbuf 界定，reactor-design §四/R-11）。
    fn read_upstream(&mut self, flow: u64) {
        let (fd, is_udp) = {
            let Some(f) = self.flows.get(&flow) else { return };
            let Some(io) = f.io.as_ref() else { return };
            if io.dead || io.conn.is_some() {
                return;
            }
            (io.fd.as_raw_fd(), io.udp)
        };
        if is_udp {
            // downSeen 实测位 + 数据报整包回投（应用层无预算——内核 rcvbuf 界定，§四/R-11）
            // Q-I F3：读缓冲循环外提（每次调用一次零初始化，不再每数据报一次）。
            let mut buf = [0u8; 65536];
            loop {
                let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast(), buf.len(), 0) };
                if n > 0 {
                    if let Some(f) = self.flows.get_mut(&flow) {
                        if f.kind == Kind::Transit {
                            f.udp_replied = true;
                        }
                    }
                    self.udp_send_to_client(flow, &buf[..n as usize]);
                } else if n < 0 {
                    let e = std::io::Error::last_os_error();
                    if e.kind() != std::io::ErrorKind::WouldBlock {
                        // 读错误 = 拆（macOS connected UDP ECONNREFUSED 的发现路径之二）
                        self.on_upstream_eof(flow);
                    }
                    break;
                }
                // n == 0：UDP 空数据报——吞（对齐旧 worker：不算 EOF 不投递）
            }
            if let Some(f) = self.flows.get_mut(&flow) {
                f.last_active = Instant::now();
            }
            return;
        }
        // TCP/UDS：读尽到 EAGAIN / 水位（WATERMARK 门控读端——Ack 清账通道删除后的
        // 直读等价物）。Q-I F3：读缓冲循环外提（每次调用一次零初始化）。
        let mut buf = [0u8; READ_CHUNK];
        loop {
            let gated = self
                .flows
                .get(&flow)
                .map(|f| f.tx_backlog.remaining().len() >= WATERMARK)
                .unwrap_or(false);
            if gated {
                break;
            }
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                let Some(f) = self.flows.get_mut(&flow) else { return };
                f.tx_backlog.push(&buf[..n as usize]);
                f.last_active = Instant::now();
            } else if n == 0 {
                self.on_upstream_eof(flow);
                break;
            } else {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::WouldBlock {
                    self.on_upstream_eof(flow);
                }
                break;
            }
        }
        // 读后即试灌栈内 socket（旧 on_upstream_data 的 flush_backlog 同位）
        self.flush_backlog(flow);
    }

    /// 待写缓冲续写（POLLOUT / 追加后即试写）：TCP 字节流前缀消费（部分写余量续
    /// 传）；UDP 数据报整取整发（POSIX send = 整数据报语义，无部分发）。
    ///
    /// 三态步进（评审 r2-高1/中1 整改）：`Wrote`（写进了——续写）；`Full`（EAGAIN
    /// ——**break 等 POLLOUT**：原地重试 = 驱动线程自旋 = 整出口挂死，单线程收编后
    /// 无任何线程能解围）；`Dead`（硬错误——**先清待写缓冲再走 EOF**：余量静默丢 =
    /// 旧 worker「EOF 后不再写」口径；不清则 `dead ∧ 排空` 永不成立，fd 挂到 5min
    /// idle + 每拍注定失败的 send + wait_hint 恒 1ms 空转）。
    fn flush_out(&mut self, flow: u64) {
        enum Step {
            Wrote,
            Full,
            Dead,
        }
        let is_udp = self
            .flows
            .get(&flow)
            .and_then(|f| f.io.as_ref())
            .map(|io| io.udp)
            .unwrap_or(false);
        if is_udp {
            loop {
                let step = {
                    let Some(f) = self.flows.get_mut(&flow) else { return };
                    let Some(io) = f.io.as_mut() else { return };
                    let Some(dg) = io.out_udp.front() else { break };
                    let n = unsafe {
                        // MSG_NOSIGNAL（评审 D-1 中-4：主进程对 SIGPIPE 已恢复默认
                        // 处置，裸写会打死进程——send 带 flag 面，write 无）。
                        libc::send(io.fd.as_raw_fd(), dg.as_ptr().cast(), dg.len(), libc::MSG_NOSIGNAL)
                    };
                    if n >= 0 {
                        io.out_udp.pop_front();
                        f.last_active = Instant::now();
                        Step::Wrote
                    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                        Step::Full
                    } else {
                        Step::Dead
                    }
                };
                match step {
                    Step::Wrote => continue,
                    Step::Full => break, // 余量留缓冲等 POLLOUT
                    Step::Dead => {
                        self.discard_out(flow);
                        self.on_upstream_eof(flow);
                        break;
                    }
                }
            }
        } else {
            loop {
                let step = {
                    let Some(f) = self.flows.get_mut(&flow) else { return };
                    let Some(io) = f.io.as_mut() else { return };
                    let rem = io.out_tcp.remaining();
                    if rem.is_empty() {
                        break;
                    }
                    // MSG_NOSIGNAL：同上。
                    let n = unsafe {
                        libc::send(io.fd.as_raw_fd(), rem.as_ptr().cast(), rem.len(), libc::MSG_NOSIGNAL)
                    };
                    if n > 0 {
                        io.out_tcp.consume(n as usize);
                        f.last_active = Instant::now();
                        Step::Wrote
                    } else if n == 0 {
                        Step::Dead // send 返 0（流语义下不应发生）——按错误处置
                    } else if std::io::Error::last_os_error().kind() == std::io::ErrorKind::WouldBlock {
                        Step::Full
                    } else {
                        Step::Dead
                    }
                };
                match step {
                    Step::Wrote => continue,
                    Step::Full => break, // 余量留缓冲等 POLLOUT
                    Step::Dead => {
                        self.discard_out(flow);
                        self.on_upstream_eof(flow);
                        break;
                    }
                }
            }
        }
        self.maybe_close_dead_io(flow);
    }

    /// 丢弃待写缓冲（硬写失败收口用——连接已死，余量不可投递：静默丢 =
    /// 旧 worker「EOF 后不再写」口径；之后 `dead ∧ 排空` 成立、maybe_close_dead_io
    /// 即可收 fd）。
    fn discard_out(&mut self, flow: u64) {
        if let Some(io) = self.flows.get_mut(&flow).and_then(|f| f.io.as_mut()) {
            let rem = io.out_tcp.remaining().len();
            io.out_tcp.consume(rem);
            io.out_udp.clear();
        }
    }

    /// `dead ∧ 待写排空` → 立即关 fd、io=None（Flow 保留——栈侧 FIN/idle 收尾照旧，
    /// reactor-design §二 fd 关闭规则表①；旧 worker「排空即收 fd」同位）。
    fn maybe_close_dead_io(&mut self, flow: u64) {
        let drain = self
            .flows
            .get(&flow)
            .map(|f| {
                f.io.as_ref()
                    .map(|io| io.dead && io.out_tcp.is_empty() && io.out_udp.is_empty())
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if drain {
            if let Some(f) = self.flows.get_mut(&flow) {
                f.io = None; // OwnedFd drop = close(2)
            }
        }
    }

    /// reactor 等待者位刷新（pump 尾、reap 之后——本拍 service_sockets 新攒的待写量
    /// 下一轮引擎 poll 即可见；评审 R-5：pump 开头算会漏一拍）。
    fn refresh_waiters(&mut self) {
        self.reactor_waiters = self.flows.values().any(|f| {
            f.io.as_ref().is_some_and(|io| {
                io.conn.is_some() || !io.out_tcp.is_empty() || !io.out_udp.is_empty()
            })
        });
    }

    /// 拨号验收收口（即时成功与 POLLOUT 验收两路统一——R-1）：建栈内 socket +
    /// 注入缓存（SYN-ACK 由此产生）+ E10 判据行；UDP = 建栈内 socket + 会话就绪。
    fn dial_accept(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let Phase::Dialing { .. } = f.phase else {
            return; // 非 Dialing（竞态）：流已被收口路径清，无事可做
        };
        match f.proto {
            Proto::Udp => {
                // UDP：fd 已就位（同步拨号）——建栈内 socket + 会话就绪（重放/判据行）
                self.ensure_udp_socket(flow);
                self.udp_ready(flow);
            }
            Proto::Tcp => {
                // TCP_NODELAY（Go SetDelayOption(false) 同口径——连接完成后设，两路
                // 验收的唯一会合点）
                if let Some(io) = self.flows.get(&flow).and_then(|f| f.io.as_ref()) {
                    let one = libc::c_int::from(1);
                    unsafe {
                        libc::setsockopt(
                            io.fd.as_raw_fd(),
                            libc::IPPROTO_TCP,
                            libc::TCP_NODELAY,
                            &one as *const _ as *const libc::c_void,
                            std::mem::size_of::<libc::c_int>() as u32,
                        );
                    }
                }
                // TCP：建栈内 listen socket + 注入缓存包（SYN-ACK 由此产生）
                let rw = f.rw_port;
                let mut sock = TcpSocket::new(
                    tcp::SocketBuffer::new(vec![0u8; FLOW_BUF]),
                    tcp::SocketBuffer::new(vec![0u8; FLOW_TX_BUF]),
                );
                sock.set_nagle_enabled(false); // Go SetDelayOption(false) 同口径
                sock.set_congestion_control(tcp::CongestionControl::Cubic); // R8-8a CUBIC（下行 bulk 发送方）
                sock.set_timeout(Some(smoltcp::time::Duration::from_secs(TCP_IDLE.as_secs()))); // R2 低-10：精确 idle 回收
                if let Err(e) = sock.listen(IpEndpoint::new(self.cfg.tunnel_ip.into(), rw)) {
                    (self.cfg.logf)(&format!("intercept: tcp listen rw_port {rw} 失败：{e:?}"));
                    self.teardown_flow(flow);
                    return;
                }
                let h = self.sockets.add(sock);
                let cache = {
                    let f = self.flows.get_mut(&flow).expect("刚判存在");
                    match std::mem::replace(&mut f.phase, Phase::Established) {
                        Phase::Dialing { cache } => cache,
                        Phase::Established => Vec::new(), // 防御：上面已判 Dialing
                    }
                };
                self.establish(flow, Some(h));
                // 注入缓存（重写后）——此时 SYN-ACK 会在本拍 poll 产出
                for mut p in cache {
                    nat::rewrite_dst(&mut p, self.cfg.tunnel_ip, rw);
                    self.device.rx_push(&p);
                }
                // 判据行（E10 dialok）
                let (kind, orig_dst, client) = {
                    let f = self.flows.get(&flow).expect("刚判存在");
                    (f.kind, f.orig_dst, f.client)
                };
                self.stats.incr_ok();
                (self.cfg.logf)(&format!(
                    "intercept: tcp {} {}:{} ← {}:{}（dialok）",
                    kind.as_str(),
                    orig_dst.0,
                    orig_dst.1,
                    client.0,
                    client.1
                ));
            }
        }
    }

    /// UDP 会话建立时的栈内 socket（bind rw_port；「connect 语义」用读时校验源）。
    fn ensure_udp_socket(&mut self, flow: u64) {
        let f = self.flows.get(&flow).expect("调用方已判");
        if f.sock.is_some() {
            return;
        }
        let rw = f.rw_port;
        let rx_meta: Vec<udp::PacketMetadata> =
            (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
        let tx_meta: Vec<udp::PacketMetadata> =
            (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
        let mut sock = UdpSocket::new(
            udp::PacketBuffer::new(rx_meta, vec![0u8; 64 * 1024]),
            udp::PacketBuffer::new(tx_meta, vec![0u8; 64 * 1024]),
        );
        if let Err(e) = sock.bind(IpEndpoint::new(self.cfg.tunnel_ip.into(), rw)) {
            (self.cfg.logf)(&format!("intercept: udp bind rw_port {rw} 失败：{e:?}"));
            return;
        }
        let h = self.sockets.add(sock);
        self.flows.get_mut(&flow).expect("刚判存在").sock = Some(h);
    }

    fn on_dial_failed(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let (kind, orig_dst, client, proto, f_syn_seq) =
            (f.kind, f.orig_dst, f.client, f.proto, f.syn_seq);
        self.stats.incr_fail();
        if proto == Proto::Tcp {
            // RST 回客户端（源 = orig dst——Go r.Complete(true) 同义）。ack = 记录的
            // SYN 的 iss+1（SYN-SENT 侧只认这种形态——smoltcp rst_reply 同构）。
            let syn = nat::build_tcp_syn(client.0, client.1, orig_dst.0, orig_dst.1, f_syn_seq);
            let v = Ipv4View::parse(&syn).expect("构造包恒可解析");
            self.tx_out.push(nat::build_tcp_rst(&v));
            // 日志降噪（R6.5 E2E P2-6）：手机核自连探测拨隧道 IP:1，逐次记行会刷屏
            // （dialfail 计数不受影响——判据面是计数器不是日志行）。同形态首行即记、
            // 之后每 100 次记一行汇总。
            let key = (kind.as_str(), orig_dst);
            let (log, seen) = {
                let e = self.dial_fail_seen.entry(key).or_insert((0u64, false));
                e.0 += 1;
                let log = !e.1 || e.0.is_multiple_of(100);
                e.1 = true;
                (log, e.0)
            };
            if self.dial_fail_seen.len() > 1024 {
                self.dial_fail_seen.clear(); // 排障级记忆，满表清空重记（同 src_seen 口径）
            }
            if log {
                (self.cfg.logf)(&format!(
                    "intercept: tcp {} {}:{} ← {}:{} 拨号失败：连接失败{}",
                    kind.as_str(),
                    orig_dst.0,
                    orig_dst.1,
                    client.0,
                    client.1,
                    if seen > 1 {
                        format!("（该形态累计 {seen} 次，此后每 100 次记一行）")
                    } else {
                        String::new()
                    }
                ));
            }
        } else {
            (self.cfg.logf)(&format!(
                "intercept: udp {} {}:{} ← {}:{} 开 socket 失败：连接失败",
                kind.as_str(),
                orig_dst.0,
                orig_dst.1,
                client.0,
                client.1
            ));
        }
        // fd 随 remove_flow 的 io drop 收口（H1 族的属主表/回执收口整体不存在）
        self.remove_flow(flow);
    }

    /// backlog 续写（R8-8a：CC 垫片退役后的发送门形态）：把 upstream 数据写进栈内
    /// socket（部分写留余量），**wire 侧出站节流由栈内 CUBIC 承担**（seq_to_transmit
    /// 按 cwnd_remaining 封顶——tx_buffer 里的存量不受限，只有上线的在途受控）。
    /// backlog 清空且挂起 FIN 时补 close。
    fn flush_backlog(&mut self, flow: u64) {
        let Some(f) = self.flows.get_mut(&flow) else {
            return;
        };
        let Some(h) = f.sock else { return };
        if f.tx_backlog.is_empty() {
            return;
        }
        let w = self
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(f.tx_backlog.remaining())
            .unwrap_or_default();
        if w > 0 {
            f.tx_backlog.consume(w);
        }
        if f.tx_backlog.is_empty() && f.fin_pending {
            self.sockets.get_mut::<TcpSocket>(h).close();
        }
    }

    /// upstream EOF/错误（幂等——dead 已置即返）：TCP = 栈内 socket 发 FIN 的挂起
    /// 语义；UDP = 拆会话；io 侧标 dead（待写排空后收 fd——maybe_close_dead_io）。
    fn on_upstream_eof(&mut self, flow: u64) {
        let already = self
            .flows
            .get(&flow)
            .map(|f| f.io.as_ref().map(|io| io.dead).unwrap_or(true))
            .unwrap_or(true);
        if already {
            return;
        }
        if let Some(f) = self.flows.get_mut(&flow) {
            if let Some(io) = f.io.as_mut() {
                io.dead = true;
            }
        }
        let Some(f) = self.flows.get_mut(&flow) else {
            return;
        };
        match f.proto {
            Proto::Tcp => {
                // 「任一方 EOF 即双向拆」的 upstream 半边：栈内 socket 发 FIN——
                // **backlog 非空时先挂起**（FIN 排队先于 backlog 会把尾数据挤丢）
                if !f.tx_backlog.is_empty() {
                    f.fin_pending = true;
                } else if let Some(h) = f.sock {
                    self.sockets.get_mut::<TcpSocket>(h).close();
                }
            }
            Proto::Udp => {
                // UDP 无 EOF 概念（读错误同拆）
                self.finish_udp(flow);
                return; // finish_udp 已 remove_flow——无 io 可收
            }
        }
        self.maybe_close_dead_io(flow);
    }

    /// 会话建立收口（`incr_flow` **唯一入口**——F5）：置 `counted`、phase→Established、
    /// 计数；`sock = Some(h)` 时同时落位栈内 socket（`None` = **保持现状**——UDP 腿的
    /// socket 由 `ensure_udp_socket` 就位，勿在此清空）。与 `retire` 配对；守卫由
    /// `counted` 位随状态转移伴生（`debug_assert` 兜底重复建立）。
    fn establish(&mut self, flow: u64, sock: Option<SocketHandle>) {
        {
            let Some(f) = self.flows.get_mut(&flow) else {
                return;
            };
            debug_assert!(!f.counted, "重复 establish（flow {flow}）");
            f.phase = Phase::Established;
            if sock.is_some() {
                f.sock = sock;
            }
            f.counted = true;
        }
        self.stats.incr_flow();
    }

    /// 会话退役收口（`decr_flow` **唯一入口**——F5）：仅对已 `establish`（`counted`）的
    /// 流计数。防「未 incr 即 decr」的 gauge 回绕：① `dns_tcp_establish` listen 失败、
    /// ② `dial_accept` listen 失败、③ `close()` 期在途 Dialing 流——三条路径的
    /// `teardown_flow` 都不再扣减未计数的流。
    fn retire(&mut self, flow: u64) {
        let counted = self
            .flows
            .get_mut(&flow)
            .map(|f| std::mem::take(&mut f.counted))
            .unwrap_or(false);
        if counted {
            self.stats.decr_flow();
        }
    }

    /// UDP 会话收尾（判据行 + 计数 + 清流——fd 随 remove_flow 的 io drop 收口）。
    fn finish_udp(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let (kind, orig_dst, client, replied, seq) =
            (f.kind, f.orig_dst, f.client, f.udp_replied, f.udp_seq_of);
        self.retire(flow);
        if kind == Kind::Transit {
            self.stats.incr_udp_session(replied);
        }
        // 关闭行打**本会话号**（评审 M4：此前打全局最新 seq，仅单会话场景凑巧对）
        (self.cfg.logf)(&format!(
            "udp intercept: 会话 #{seq} 关闭（{}:{} ← {}:{}）",
            orig_dst.0, orig_dst.1, client.0, client.1
        ));
        self.remove_flow(flow); // fd 随 io drop 收口（H1 族的属主表/回执面不存在）
    }

    /// 栈内 socket 服务：读数据 → 待写缓冲（水位门控）+ TCP 关闭推进。
    fn service_sockets(&mut self) {
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        // 待拆流收口（F2/[门-A1]）：DNS TCP 腿读到空帧（mlen==0）须收线，但**不得在
        // 本迭代内 `teardown_flow`**——本臂后续还要用快照 `h` 取 socket，`remove_flow`
        // 后再 `get_mut(h)` 会 panic。统一收到循环外。
        let mut close_legs: Vec<u64> = Vec::new();
        for flow in flows {
            let Some(f) = self.flows.get(&flow) else {
                continue;
            };
            let (proto, phase_ready) = (f.proto, matches!(f.phase, Phase::Established));
            if !phase_ready {
                continue;
            }
            let Some(h) = f.sock else { continue };
            // 栈→upstream 在途 = io 待写缓冲现存（unacked_out 记账坍缩为直读）
            let gated = self.pending_out(flow) > WATERMARK;
            match proto {
                Proto::Tcp => {
                    let (can_recv, _may_recv, state, can_send) = {
                        let s = self.sockets.get_mut::<TcpSocket>(h);
                        (s.can_recv(), s.may_recv(), s.state(), s.can_send())
                    };
                    let _ = can_send;
                    // backlog 续写（开窗即写、部分写余量消化、FIN 挂起推进——
                    // 全在 flush_backlog 内；wire 节流归栈内 CUBIC）
                    self.flush_backlog(flow);
                    let is_dns_leg = self
                        .flows
                        .get(&flow)
                        .map(|f| f.kind == Kind::Dns)
                        .unwrap_or(false);
                    if can_recv && !gated {
                        // 读尽 → DNS 腿喂进程内代答 / 其余直进待写缓冲（边读边
                        // push——每方向 1 拷，设计 §二；DNS 腿才需要中间 Vec）
                        let mut total = 0usize;
                        let mut dns_chunks: Vec<Vec<u8>> = Vec::new();
                        loop {
                            // 水位门每块复查（[门-A2]）：门只在读循环前判一次会把上界
                            // 从 WATERMARK 抬到 +FLOW_BUF（读循环会把栈内已缓冲的全部
                            // 读光）；每读一块后复查收紧到 WATERMARK + READ_CHUNK。
                            if self.pending_out(flow) > WATERMARK {
                                break;
                            }
                            let n = self
                                .sockets
                                .get_mut::<TcpSocket>(h)
                                .recv_slice(&mut self.rx_scratch[..])
                                .unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            if is_dns_leg {
                                dns_chunks.push(self.rx_scratch[..n].to_vec());
                            } else if let Some(io) =
                                self.flows.get_mut(&flow).and_then(|f| f.io.as_mut())
                            {
                                io.out_tcp.push(&self.rx_scratch[..n]);
                            }
                            total += n;
                        }
                        if is_dns_leg {
                            let mut closed = false;
                            for c in dns_chunks {
                                if matches!(self.dns_tcp_feed(flow, &c), FeedOutcome::CloseLeg) {
                                    close_legs.push(flow);
                                    closed = true;
                                    break;
                                }
                            }
                            if closed {
                                continue; // 收线在循环外统一做——本迭代不再碰快照 h
                            }
                        } else if total > 0 {
                            if let Some(f) = self.flows.get_mut(&flow) {
                                f.last_active = Instant::now();
                            }
                            // 本拍即试写（EAGAIN 余量留缓冲等 POLLOUT）
                            self.flush_out(flow);
                        }
                    }
                    // 对端 FIN 且缓冲排空 → 本地 close（FIN 推进；CloseWait 不会自发迁移——
                    // **必须限定 CloseWait 态**：Listen/SynSent 等未连接态 may_recv 恒 false，
                    // 无条件 close 会把刚 listen 的 socket 立刻关掉）
                    if state == tcp::State::CloseWait && !can_recv {
                        let has_pending = self.pending_out(flow) > 0;
                        if !has_pending {
                            self.sockets.get_mut::<TcpSocket>(h).close();
                        }
                    }
                    // 彻底关 + 双向无在途 → 收流
                    if state == tcp::State::Closed && !can_recv && !can_send {
                        self.teardown_flow(flow);
                    }
                }
                Proto::Udp => {
                    // 上行读侧门（F3：栈→upstream 方向此前缺门）。阈值 `UDP_OUT_GATE`
                    // 严格低于字节上限 `WATERMARK`——门先拦（达阈值停读 ⇒ 栈 rx 缓冲填满
                    // ⇒ 栈层丢新），上限拦单拍读尽造成的越界（评审 M3）。
                    // DNS 腿 io=None ⇒ `pending_out` 恒 0 ⇒ 门对 DNS 腿天然不生效（勿误伤 F6）。
                    if self.pending_out(flow) > UDP_OUT_GATE {
                        continue;
                    }
                    let is_dns_udp_leg = self
                        .flows
                        .get(&flow)
                        .map(|f| f.kind == Kind::Dns && f.proto == Proto::Udp)
                        .unwrap_or(false);
                    // 读出（读时校验源 = 客户端——「connect 语义」的替代）→ 待写数据报
                    // 队列；DNS 腿走进程内代答（F6），其余进 out_udp。
                    let expect = self
                        .flows
                        .get(&flow)
                        .map(|f| IpEndpoint::new(f.client.0.into(), f.client.1))
                        .unwrap_or_else(|| IpEndpoint::new(Ipv4Addr::UNSPECIFIED.into(), 0));
                    let mut any = false;
                    while let Ok((n, meta)) = self
                        .sockets
                        .get_mut::<UdpSocket>(h)
                        .recv_slice(&mut self.rx_scratch[..])
                    {
                        if meta.endpoint != expect {
                            continue; // 非客户端来源：丢弃（包已取出，继续读）
                        }
                        if is_dns_udp_leg {
                            // F6：DNS 腿会话内**每包**应答（对齐 Go `dnsLeg.Write` 每包
                            // `Answer`）——此前 io=None 静默丢 ⇒ 固定源端口重传全黑洞。
                            if let Some(dns) = self.cfg.dns.clone() {
                                let tag = self
                                    .dns_faces
                                    .as_mut()
                                    .map(|f| f.route_tag(DnsRoute::UdpFlow(flow)))
                                    .unwrap_or(0);
                                if tag != 0
                                    && dns.submit_leg(tag, self.rx_scratch[..n].to_vec())
                                        == SubmitOutcome::Dropped
                                {
                                    // F2：丢弃路径回收 tag
                                    if let Some(faces) = self.dns_faces.as_mut() {
                                        faces.take_route(tag);
                                    }
                                }
                            }
                            if let Some(f) = self.flows.get_mut(&flow) {
                                f.last_active = Instant::now();
                            }
                            continue;
                        }
                        // out_udp 上限（F3：条数 MAX_OUT_UDP_PKTS + 字节 WATERMARK——
                        // 超限丢新+计数，不阻塞不排队）。
                        if self.udp_out_full(flow, n) {
                            self.stats.incr_udp_drop();
                            continue;
                        }
                        if let Some(f) = self.flows.get_mut(&flow) {
                            if let Some(io) = f.io.as_mut() {
                                io.out_udp.push_back(self.rx_scratch[..n].to_vec());
                                any = true;
                            }
                        }
                    }
                    if any {
                        if let Some(f) = self.flows.get_mut(&flow) {
                            f.last_active = Instant::now();
                        }
                        self.flush_out(flow);
                    }
                }
            }
        }
        // DNS TCP 腿空帧收线（[门-A1]：循环外统一拆——避免同迭代悬垂 SocketHandle）。
        for flow in close_legs {
            self.teardown_flow(flow);
        }
    }

    /// 栈→upstream 在途待写字节（unacked_out 的直读等价物——TCP 字节流 + UDP 数据报）。
    fn pending_out(&self, flow: u64) -> usize {
        self.flows
            .get(&flow)
            .and_then(|f| f.io.as_ref())
            .map(|io| {
                io.out_tcp.remaining().len()
                    + io.out_udp.iter().map(|d| d.len()).sum::<usize>()
            })
            .unwrap_or(0)
    }

    /// `out_udp` 是否已达上限（F3：条数 `MAX_OUT_UDP_PKTS` ∨ 字节 `WATERMARK`）——
    /// `incoming` = 待入队数据报长度。无 io（DNS 腿）= 恒不满。
    fn udp_out_full(&self, flow: u64, incoming: usize) -> bool {
        self.flows
            .get(&flow)
            .and_then(|f| f.io.as_ref())
            .map(|io| {
                io.out_udp.len() >= MAX_OUT_UDP_PKTS
                    || io.out_udp.iter().map(|d| d.len()).sum::<usize>() + incoming > WATERMARK
            })
            .unwrap_or(false)
    }

    /// idle 看门狗（TCP 5min / UDP 60s / DNS 10s——共享活跃时间戳）+ connect 死线
    ///（10s——**先判**：`conn != None ∧ now ≥ dial_deadline` 独立判定，不复用
    /// last_active 的 idle 分支，reactor-design §五）。
    fn reap_idle(&mut self) {
        let now = Instant::now();
        let expired: Vec<u64> = self
            .flows
            .iter()
            .filter(|(_, f)| {
                f.io.as_ref()
                    .is_some_and(|io| io.conn.is_some() && now >= io.dial_deadline)
            })
            .map(|(k, _)| *k)
            .collect();
        for flow in expired {
            self.on_dial_failed(flow);
        }
        let victims: Vec<u64> = self
            .flows
            .iter()
            .filter(|(_, f)| {
                let idle = match (f.kind, f.proto) {
                    (Kind::Dns, Proto::Tcp) => TCP_DNS_IDLE,
                    (Kind::Dns, _) => DNS_IDLE,
                    (_, Proto::Udp) => UDP_IDLE,
                    (_, Proto::Tcp) => TCP_IDLE,
                };
                now.duration_since(f.last_active) > idle
            })
            .map(|(k, _)| *k)
            .collect();
        for flow in victims {
            let is_udp = self
                .flows
                .get(&flow)
                .map(|f| f.proto == Proto::Udp)
                .unwrap_or(false);
            if is_udp {
                self.finish_udp(flow);
            } else {
                self.teardown_flow(flow);
            }
        }
    }

    /// 拆流（TCP 关闭路径）：栈 socket close + 判据行；OS fd 随 remove_flow 的 io
    /// drop 收口（「到期 Close{linger_rst} RST」Rust 侧从未接线——close() 一直走
    /// FIN teardown，R3-design §4.1 M13 差异登记；reactor 批 R-15 删除死分支）。
    fn teardown_flow(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let (kind, orig_dst, client, proto) = (f.kind, f.orig_dst, f.client, f.proto);
        if proto == Proto::Tcp {
            if let Some(h) = f.sock {
                self.sockets.get_mut::<TcpSocket>(h).close();
                let _ = h;
            }
            (self.cfg.logf)(&format!(
                "intercept: tcp {} {}:{} ← {}:{} 关闭",
                kind.as_str(),
                orig_dst.0,
                orig_dst.1,
                client.0,
                client.1
            ));
        }
        // F5 收口（评审 L4）：TCP 与 UDP 都退役——`close()` 对在册 UDP 流走本路径，
        // 已建立流的 gauge 须归零（否则 `serve.status` 的 flows 在收工面失真）。
        // UDP 正常收尾走 `finish_udp`（自带 retire），不经本函数，无双重扣减。
        self.retire(flow);
        self.remove_flow(flow);
    }

    /// 清流记录（表 + 栈 socket 槽位）；OS fd 随 io drop 关闭（OwnedFd 恰一次）。
    fn remove_flow(&mut self, flow: u64) {
        // 评审 2.1：观测快照随流回收（键单调不复用 ⇒ 不清 = 长跑出口每天 ~1.5MB
        // 的无界累积）。
        self.obs_snaps.remove(&flow);
        if let Some(f) = self.flows.remove(&flow) {
            if let Some(h) = f.sock {
                self.sockets.remove(h);
                // smoltcp 0.11 的 remove 即 prune——TIME_WAIT 期保留语义由「Closed 才移除」
                // 的调用纪律承载（service_sockets 的 state==Closed 检查）
            }
            self.by_rw_port.remove(&f.rw_port);
            self.by_five.remove(&(
                f.client.0,
                f.client.1,
                f.orig_dst.0,
                f.orig_dst.1,
                if f.proto == Proto::Tcp { 6 } else { 17 },
            ));
        }
    }

    /// 登记栈内真 listener 端口（demux 优先面；3d 的 DNS listener 建时调）。
    pub fn add_served_port(&mut self, port: u16) {
        self.served_ports.insert(port);
    }

    /// 停收新流（HaltNew——新 TCP 回 RST、新 UDP 回 ICMP；在途不受影响）。
    pub fn halt_new(&mut self) {
        self.halted = true;
    }

    /// 在册流数（驱动收工宽限的销账判据）。
    pub fn flow_count(&self) -> usize {
        self.flows.len()
    }

    /// 收工宽限拍（评审 M2）：出站包**带回给引擎走 encap 链**，宽限窗口内存量连接的
    /// FIN/ACK/尾数据不丢。返回本拍出站包；到期侧的 teardown 由 close() 承担。
    pub fn pump_grace(&mut self, _deadline: Instant) -> Vec<Vec<u8>> {
        self.halt_new();
        self.pump_with_flush()
    }

    /// 收工宽限拍（评审 r2-1.1 整改）：整流绕开（全量释放）版 pump——宽限的
    /// 原始语义 = 尾数据/FIN 尽快上线（M2），不适用发送整形。
    pub fn pump_with_flush(&mut self) -> Vec<Vec<u8>> {
        // 与 pump 同拍序（事件 → DNS → 双 poll + TX → 服务），仅释放形态不同。
        let out = self.pump();
        // pump 已按令牌释放了本拍产物；这里把滞留余量一并放净。
        if self.cfg.tx_shape.is_some() && !self.tx_deferred.is_empty() {
            let mut out = out;
            out.reserve(self.tx_deferred.len());
            for p in self.tx_deferred.drain(..) {
                out.push(p);
            }
            self.tx_deferred_bytes = 0; // 余量已计入 pump 侧窗释——不双计
            return out;
        }
        out
    }

    /// 诊断面：在册流数（同 flow_count——命名对齐）。
    pub fn flows_alive(&self) -> usize {
        self.flows.len()
    }

    /// 整形滞留是否非空（R8-3 8i：驱动循环 poll 超时自适应面）。真机实测（2026-10-05
    /// B 臂）：bulk 期 ACK 按团到达 ≈190Hz，驱动拍被 5ms poll 钉死 ⇒ 每拍只放得下
    /// 一个突发额度（160KB/5.3ms ≈ 30MB/s）——**突发额度退化成了速率上限**。滞留
    /// 非空时驱动循环应缩短 poll 超时（1ms：续水 64KB/拍 = 64MiB/s 直通面上限，
    /// 且线上团块随之细化到 ~64KB）。
    pub fn tx_deferred_pending(&self) -> bool {
        self.cfg.tx_shape.is_some() && !self.tx_deferred.is_empty()
    }

    /// 驱动循环的等待提示（reactor 批扩档）：`整形滞留非空 ∨ reactor 存在等待者
    /// （connect 在途 ∨ 任一流待写缓冲非空）` = Some(1ms)（8n③ 拍频形态——每拍续水
    /// rate×1ms、线上团块随之细化；reactor 等待者 = 静默客户端形态的上游事件最晚
    /// 1ms 被发现——reactor-design §一）；否则 None（5ms 常规拍）。
    /// `reactor_waiters` 在 pump 尾刷新（评审 R-5）。
    pub fn wait_hint(&self) -> Option<Duration> {
        (self.tx_deferred_pending() || self.reactor_waiters).then_some(Duration::from_millis(1))
    }

    /// 测试面：reactor 名下 OS socket 数（fd 收口断言——评审 R-7③）。
    #[cfg(test)]
    pub fn reactor_fds(&self) -> usize {
        self.flows.values().filter(|f| f.io.is_some()).count()
    }

    /// 全停（teardown：在途 TCP 立即拆——收工语义）。
    pub fn close(&mut self) {
        if !self.tx_deferred.is_empty() {
            // 防御面记行（评审 r2-1.1）：close 前宽限循环应已 flush——到这还有
            // 滞留 = 宽限窗口被截断，尾数据丢失要有观测面。
            (self.cfg.logf)(&format!(
                "intercept: close 时仍有整形滞留 {} 包/{}B（宽限窗口未排空——尾数据丢弃）",
                self.tx_deferred.len(),
                self.tx_deferred_bytes
            ));
        }
        self.halt_new();
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        for flow in flows {
            self.teardown_flow(flow);
        }
        // Q-K F1.5：重组上下文与 TX 分片表整体清空（**静默**——收工面不发 ICMP、
        // 不计入 fragDrop/fragTimeout）。
        self.reasm.clear();
        self.tx_frag.clear();
        if let Some(faces) = self.dns_faces.as_mut() {
            faces.close_all(&mut self.sockets);
        }
    }
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

/// 入站包视图的值形态（on_plain 移交包所有权时的快照——借用/所有权解耦）。
#[derive(Clone, Copy)]
pub struct View5 {
    pub src: Ipv4Addr,
    pub src_port: u16,
    pub dst: Ipv4Addr,
    pub dst_port: u16,
    pub proto: u8,
    pub tcp_flags: u8,
    pub tcp_seq: u32,
    pub tcp_ack: u32,
    /// UDP 载荷在原包中的字节范围（首包/pending 重放取纯载荷——Go udpPayloadOf 同义）。
    pub udp_payload: (usize, usize),
}

impl View5 {
    pub fn is_tcp_syn(&self) -> bool {
        self.proto == 6 && self.tcp_flags & nat::TCP_SYN != 0 && self.tcp_flags & nat::TCP_ACK == 0
    }
}

/// 取一个明文 IPv4 包的目的地址（引擎路由 encap 用；畸形 = None）。
pub fn nat_view_dst(pkt: &[u8]) -> Option<Ipv4Addr> {
    Ipv4View::parse(pkt).map(|v| v.dst)
}

/// 按视图构造 RST（源 = 视图的目的）。
fn build_rst_for(v: &View5) -> Vec<u8> {
    let syn = nat::build_tcp_syn(v.src, v.src_port, v.dst, v.dst_port, v.tcp_seq);
    match Ipv4View::parse(&syn) {
        Some(view) => nat::build_tcp_rst(&view),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wgcore::stackb::StackB;
    use smoltcp::iface::SocketHandle;
    use smoltcp::socket::tcp::Socket as TcpSocket;
    use smoltcp::time::Instant as SmolInstant;
    use std::io::{Read, Write as _};
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixListener;

    fn noop_logf() -> Logf {
        Arc::new(|_| {})
    }

    /// harness 臂的产品默认整形参数（与 `tx_shape_resolve(None)` 无 env/config 覆盖
    /// 时同值——直接引常量保测试确定性：CI 环境变量不参与）。
    const PRODUCT_SHAPE: Option<TxShape> = Some(TxShape {
        rate: TX_SHAPE_RATE,
        burst: TX_SHAPE_BURST,
    });

    fn cfg_base(tunnel_ip: Ipv4Addr) -> Config {
        Config {
            tunnel_ip,
            local_services: HashMap::new(),
            dns: None,
            dns_events: None,
            dns_resolve_port: 0,
            // 功能单测直通面（整形关闭——pump 在紧循环里跑无墙钟间隔，令牌不续水；
            // 整形行为面在 shape_slice 单测 + 受控 harness 臂）。
            tx_shape: None,
            logf: noop_logf(),
        }
    }

    /// 交叉泵：客户端栈（StackB）↔ 拦截层的包交换（n 轮；拦截层出站包注回客户端栈）。
    fn cross_pump(client: &mut StackB, itc: &mut Interceptor, rounds: usize, tick: &mut i64) {
        for _ in 0..rounds {
            // 客户端 → 拦截层（时间单调推进——栈定时器依赖）
            *tick += 5;
            let t = SmolInstant::from_millis(*tick);
            client
                .iface
                .poll(t, &mut client.device, &mut client.sockets);
            let mut out = Vec::new();
            client.device.drain_tx(&mut out);
            for p in out {
                itc.on_plain(p);
            }
            // 拦截层 → 客户端
            for p in itc.pump() {
                client.inject(&p);
            }
        }
    }

    /// 豁免流端到端：客户端栈 connect(隧道IP:port) → NAT 豁免 → 回环 echo → 数据往返 +
    /// 拨号先行语义（SYN 不提前应答——SYN-ACK 只在 DialOk 后产出）。
    #[test]
    fn exempt_flow_end_to_end() {
        // 回环 echo（豁免 upstream = 127.0.0.1:同端口）
        let echo = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_thread = std::thread::spawn(move || {
            let (mut c, _) = echo.accept().unwrap();
            let mut buf = [0u8; 4096];
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
        });

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));

        // 客户端栈（隧道侧地址 100.64.10.1；默认路由网关随便）
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 1),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, port))
            .unwrap();

        // 少量轮：SYN 已到拦截层，拨号线程可能未完成——SYN-ACK 不应出现在前几轮
        cross_pump(&mut client, &mut itc, 2, &mut 0);

        // 泵到建连（拨号 ≤ 回环即时）
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut established = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            if client.sockets.get::<TcpSocket>(h).state() == tcp::State::Established {
                established = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(
            established,
            "豁免流应建连（state={:?}）",
            client.sockets.get::<TcpSocket>(h).state()
        );
        assert!(stats.snapshot()[0].1 >= 1, "dialok 应计数");

        // 数据往返
        client
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(b"hello-exempt")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            let mut buf = [0u8; 4096];
            let n = client
                .sockets
                .get_mut::<TcpSocket>(h)
                .recv_slice(&mut buf)
                .unwrap_or(0);
            if n > 0 {
                got.extend_from_slice(&buf[..n]);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert_eq!(got, b"hello-exempt", "echo 数据应经豁免流往返");
        drop(echo_thread); // 不 join：echo 在 read 阻塞直到对端断连（测试进程退出即终结）
    }

    /// 拨号失败 → RST（客户端侧 connect 收到 refused）。
    #[test]
    fn dial_failed_gets_rst() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));

        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 2),
            tunnel,
            SmolInstant::from_millis(0),
        );
        // 隧道 IP 上无服务的端口：豁免 upstream = 127.0.0.1:<临时死端口>（绑一个
        // listener 取号再立刻关掉——无人听且不碰特权端口）。**别用 :1**：部分
        // ubuntu CI 沙箱对特权端口的出站策略是 DROP 而非 RST（连接悬死，RST 判据
        // 永远等不到——dd99ae0/7be330c/2f56af2 三轮红 + 同树 rerun 仍红、macos 恒绿、
        // 预算 15s 也不救 ⇒ 非时序面而是投递策略面）；临时端口在回环面上恒
        // ECONNREFUSED，跨平台/跨沙箱稳定。
        let dead_port = {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, dead_port))
            .unwrap();
        // connect 后 socket 即 SynSent——初态直接记（reactor 批起拨号失败快于旧线程
        // 形态：SYN 与 RST 可能在同一 cross_pump 批内往返，批间采样观察不到 SynSent
        // 中间态；断言语义不变 = RST 只能被 SynSent 态的 socket 接受）。
        let was_syn_sent = client.sockets.get::<TcpSocket>(h).state() == tcp::State::SynSent;
        assert!(was_syn_sent, "connect 后应为 SynSent");
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut refused = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            let st = client.sockets.get::<TcpSocket>(h).state();
            // RST 被 smoltcp 接受后 abort：SynSent → Closed 且 endpoint 被清
            if st == tcp::State::Closed {
                refused = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(refused, "拨号失败应回 RST（死端口拨号同款语义）");
        assert!(stats.snapshot()[1].1 >= 1, "dialfail 应计数");
    }

    /// DNS 代答端到端：:53 隧道面（UDP demux → 栈内 listener → DNS worker → 回投
    /// 原源端点）+ 进程内腿（非隧道 IP :53 的拦截兜底）。fake 上游代答。
    #[test]
    fn dns_faces_end_to_end() {
        use crate::server::dnsproxy::{DnsConfig, DnsProxy};
        use smoltcp::socket::udp::Socket as CUdp;
        use smoltcp::time::Instant as SI;

        // fake 上游（A 应答固定 127.0.0.1）
        let up = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, from)) = up.recv_from(&mut buf) else {
                    return;
                };
                let mut r = Vec::new();
                r.extend_from_slice(&buf[..2]); // ID 回显
                r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]);
                r.extend_from_slice(&1u16.to_be_bytes()); // AN=1
                r.extend_from_slice(&[0, 0, 0, 0]); // NS/AR
                r.extend_from_slice(&buf[12..n]); // question 回显
                r.extend_from_slice(&[0xC0, 0x0C]);
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&60u32.to_be_bytes());
                r.extend_from_slice(&4u16.to_be_bytes());
                r.extend_from_slice(&[127, 0, 0, 1]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-itcdns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();

        let (proxy, events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        );
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let cfg = Config {
            tunnel_ip: tunnel,
            local_services: HashMap::new(),
            dns: Some(std::sync::Arc::clone(&proxy)),
            dns_events: Some(events),
            dns_resolve_port: 5300,
            tx_shape: None,
            logf: noop_logf(),
        };
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        itc.attach_dns();
        assert!(itc.served_ports.contains(&53), "demux 面应登记 :53");
        assert!(itc.served_ports.contains(&5300), "解析腿端口应登记");

        // 一条 A 查询（id=0x3344，example.com）
        let mut q = Vec::new();
        q.extend_from_slice(&0x3344u16.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in ["example", "com"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);

        // ① 隧道 IP:53 UDP 面：手搓 UDP 包 → on_plain → demux 投栈 → worker → 回投
        let pkt = nat::build_udp(Ipv4Addr::new(100, 64, 10, 9), 52000, tunnel, 53, &q);
        itc.on_plain(pkt);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut resp53 = None;
        while Instant::now() < deadline {
            for p in itc.pump() {
                if let Some(v) = Ipv4View::parse(&p) {
                    if v.proto == 17 && v.dst == Ipv4Addr::new(100, 64, 10, 9) && v.src == tunnel {
                        resp53 = Some(v.payload.to_vec());
                    }
                }
            }
            if resp53.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let resp = resp53.expect(":53 面应答到达");
        assert_eq!(&resp[..2], &0x3344u16.to_be_bytes(), "ID 回显");
        assert_eq!(resp[3] & 0x0F, 0, "RCODE=0");
        assert!(
            resp.windows(4).any(|w| w == [127, 0, 0, 1]),
            "A 记录 127.0.0.1 在应答里"
        );
        assert!(
            proxy.stats_line().contains("q=1"),
            "隧道 UDP 面计 q：{}",
            proxy.stats_line()
        );
        assert!(proxy.stats_line().contains("resp=1"));

        // ② 进程内腿：dst=8.8.8.8:53（非隧道 IP 的 :53）→ 拦截 dns 会话 → submit_leg
        let pkt2 = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 9),
            52001,
            Ipv4Addr::new(8, 8, 8, 8),
            53,
            &q,
        );
        itc.on_plain(pkt2);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut resp_leg = None;
        while Instant::now() < deadline {
            for p in itc.pump() {
                if let Some(v) = Ipv4View::parse(&p) {
                    if v.proto == 17
                        && v.dst == Ipv4Addr::new(100, 64, 10, 9)
                        && v.src == Ipv4Addr::new(8, 8, 8, 8)
                    {
                        resp_leg = Some(v.payload.to_vec());
                    }
                }
            }
            if resp_leg.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let resp = resp_leg.expect("拦截腿应答到达（源反重写为 8.8.8.8）");
        assert_eq!(&resp[..2], &0x3344u16.to_be_bytes());
        // submit_leg 不计 q（Go Answer 口径）；resp 计数 +1
        assert!(
            proxy.stats_line().contains("q=1"),
            "腿不计 q：{}",
            proxy.stats_line()
        );
        assert!(proxy.stats_line().contains("resp=2"));
        // ③ Q-I F1 回归：同一拍连投两条不同载荷的 UDP 查询（共享收包缓冲 `udp_rx` 的
        // 复用点）——断言各自应答的 ID 与 question 段逐字节对应（防复用缓冲残留/别名）。
        let mk_q = |id: u16, label: &str| -> Vec<u8> {
            let mut q = Vec::new();
            q.extend_from_slice(&id.to_be_bytes());
            q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
            for l in [label, "com"] {
                q.push(l.len() as u8);
                q.extend_from_slice(l.as_bytes());
            }
            q.push(0);
            q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
            q
        };
        let qa = mk_q(0x7788, "alpha");
        let qb = mk_q(0x99AA, "beta");
        itc.on_plain(nat::build_udp(Ipv4Addr::new(100, 64, 10, 9), 52002, tunnel, 53, &qa));
        itc.on_plain(nat::build_udp(Ipv4Addr::new(100, 64, 10, 9), 52003, tunnel, 53, &qb));
        let (mut got_a, mut got_b) = (false, false);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !(got_a && got_b) {
            for p in itc.pump() {
                let Some(v) = Ipv4View::parse(&p) else { continue };
                if v.proto != 17 || v.dst != Ipv4Addr::new(100, 64, 10, 9) || v.src != tunnel {
                    continue;
                }
                let r = v.payload;
                if r.len() < 12 {
                    continue;
                }
                if r[..2] == qa[..2] {
                    assert_eq!(r[12..qa.len()], qa[12..], "alpha 应答 question 应逐字节对应");
                    got_a = true;
                } else if r[..2] == qb[..2] {
                    assert_eq!(r[12..qb.len()], qb[12..], "beta 应答 question 应逐字节对应");
                    got_b = true;
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(got_a && got_b, "同拍两包各自应答（got_a={got_a} got_b={got_b}）");
        let _ = SI::from_millis(0i64);
        let _ = CUdp::new(
            smoltcp::socket::udp::PacketBuffer::new(vec![], vec![]),
            smoltcp::socket::udp::PacketBuffer::new(vec![], vec![]),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M3：非隧道 IP :53 的 **TCP** 进程内代答腿（Go serveDNSTCP 同义）——客户端栈
    /// connect(8.8.8.8:53) → 无拨号直接建立（判据行「进程内代答」）→ RFC1035 帧
    /// 化查询 → 应答帧化回投（源反重写 8.8.8.8:53）。半帧跨读（分两次 send）钉
    /// 分帧积攒边界。
    #[test]
    fn tcp_dns_leg_end_to_end() {
        use crate::server::dnsproxy::{DnsConfig, DnsProxy};
        use smoltcp::time::Instant as SI;

        // fake 上游（A 应答固定 127.0.0.1）
        let up = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, from)) = up.recv_from(&mut buf) else {
                    return;
                };
                let mut r = Vec::new();
                r.extend_from_slice(&buf[..2]);
                r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]);
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&[0, 0, 0, 0]);
                r.extend_from_slice(&buf[12..n]);
                r.extend_from_slice(&[0xC0, 0x0C]);
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&60u32.to_be_bytes());
                r.extend_from_slice(&4u16.to_be_bytes());
                r.extend_from_slice(&[127, 0, 0, 1]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-tcpdnsleg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();

        let (proxy, events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        );
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let cfg = Config {
            tunnel_ip: tunnel,
            local_services: HashMap::new(),
            dns: Some(std::sync::Arc::clone(&proxy)),
            dns_events: Some(events),
            dns_resolve_port: 5300,
            tx_shape: None,
            logf: noop_logf(),
        };
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        itc.attach_dns();

        // 客户端栈 connect(8.8.8.8:53)——非隧道 IP 的 :53 TCP
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 7),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let dst = std::net::SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53);
        let h: SocketHandle = client.connect(dst).unwrap();
        let mut tick = 0i64;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut established = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            if client.sockets.get::<TcpSocket>(h).state() == tcp::State::Established {
                established = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(
            established,
            "TCP DNS 腿应无拨号直接建立（state={:?}）",
            client.sockets.get::<TcpSocket>(h).state()
        );

        // 一条 A 查询（id=0x5566，a.example）——RFC1035 帧化，**分两次 send**（半帧跨读）
        let mut q = Vec::new();
        q.extend_from_slice(&0x5566u16.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in ["a", "example"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        let mut frame = Vec::with_capacity(2 + q.len());
        frame.extend_from_slice(&(q.len() as u16).to_be_bytes());
        frame.extend_from_slice(&q);
        let (cut,) = (frame.len() / 2,);
        client
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(&frame[..cut])
            .unwrap();
        let mut tick = 0i64;
        cross_pump(&mut client, &mut itc, 6, &mut tick); // 半帧进积攒缓冲，不应有应答
        client
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(&frame[cut..])
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got: Vec<u8> = Vec::new();
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            let mut buf = [0u8; 4096];
            let n = client
                .sockets
                .get_mut::<TcpSocket>(h)
                .recv_slice(&mut buf)
                .unwrap_or(0);
            if n > 0 {
                got.extend_from_slice(&buf[..n]);
                if got.len() >= 2 {
                    let mlen = u16::from_be_bytes([got[0], got[1]]) as usize;
                    if got.len() >= 2 + mlen {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(got.len() >= 15, "应答帧应到达（得 {got:?}）");
        let mlen = u16::from_be_bytes([got[0], got[1]]) as usize;
        assert_eq!(got.len(), 2 + mlen, "应答恰一帧");
        let resp = &got[2..];
        assert_eq!(&resp[..2], &0x5566u16.to_be_bytes(), "ID 回显");
        assert_eq!(resp[3] & 0x0F, 0, "RCODE=0");
        assert!(
            resp.windows(4).any(|w| w == [127, 0, 0, 1]),
            "A 记录在应答里"
        );
        // qtcp 单列（Go ServeStream 的 qtcp.Add 口径——M3 腿与隧道内 TCP 面同计数）
        assert!(
            proxy.stats_line().contains("qtcp=1"),
            "TCP 腿计 qtcp：{}",
            proxy.stats_line()
        );
        let _ = SI::from_millis(0i64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 兴趣集纯函数单测（R-7④）：水位摘 POLLIN / InProgress 只 POLLOUT / Retry 无位 /
    /// dead 只 POLLOUT 或不注册。
    #[test]
    fn reactor_interests_matrix() {
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut io = ReactorIo {
            fd: OwnedFd::from(sock),
            udp: false,
            out_tcp: VecDequeLite::new(),
            out_udp: VecDeque::new(),
            conn: None,
            dial_deadline: Instant::now(),
            dead: false,
        };
        // 就绪态：只 POLLIN
        assert_eq!(interests_for(&io, 0), Some(libc::POLLIN));
        // 下行水位门控：backlog ≥ WATERMARK 摘 POLLIN（events=0 = 只监听 ERR/HUP）
        assert_eq!(interests_for(&io, WATERMARK), Some(0));
        assert_eq!(interests_for(&io, WATERMARK - 1), Some(libc::POLLIN));
        // 待写非空 → +POLLOUT
        io.out_tcp.push(b"x");
        assert_eq!(interests_for(&io, 0), Some(libc::POLLIN | libc::POLLOUT));
        assert_eq!(interests_for(&io, WATERMARK), Some(libc::POLLOUT));
        // InProgress：只 POLLOUT（connect 验收位）
        io.conn = Some(ConnState::InProgress);
        assert_eq!(interests_for(&io, 0), Some(libc::POLLOUT));
        // Retry：无位（每拍主动重拨——未连接 socket 恒可写，POLLOUT 无信号量）
        io.conn = Some(ConnState::Retry);
        assert_eq!(interests_for(&io, 0), None);
        // dead：有待写 = 只 POLLOUT（排空即收的窗口）；排空 = 不注册
        io.conn = None;
        io.dead = true;
        assert_eq!(interests_for(&io, 0), Some(libc::POLLOUT));
        io.out_tcp.consume(1);
        assert_eq!(interests_for(&io, 0), None);
    }

    /// UDS 豁免腿 E2E（R-7① / R-1 回归钉）：local_services 映射 → 非阻塞 connect
    /// **即时成功**（UDS 到活 listener 常态返 0——即时路径不产生 poll 事件，若验收
    /// 只挂 POLLOUT 则流卡死到 10s 死线）→ dialok → 数据往返 → 关闭收口（reactor_fds
    /// 归零——R-7③ 的 fd 收口断言面）。
    #[test]
    fn uds_exempt_flow_end_to_end() {
        let dir = std::env::temp_dir().join(format!(
            "homeway-rs-uds-exempt-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock_path = dir.join("svc.sock");
        let listener = UnixListener::bind(&sock_path).unwrap();
        let echo = std::thread::spawn(move || {
            if let Ok((mut c, _)) = listener.accept() {
                let mut buf = [0u8; 4096];
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

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut cfg = cfg_base(tunnel);
        cfg.local_services.insert(47902, sock_path.to_string_lossy().into_owned());
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));

        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 11),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, 47902))
            .unwrap();

        // 泵到建连（UDS 即时 connect + 注入缓存 SYN → SYN-ACK）
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut established = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            if client.sockets.get::<TcpSocket>(h).state() == tcp::State::Established {
                established = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            established,
            "UDS 豁免腿应即时建连（state={:?}）",
            client.sockets.get::<TcpSocket>(h).state()
        );
        assert!(stats.snapshot()[0].1 >= 1, "dialok 应计数");

        // 数据往返
        client
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(b"hello-uds")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            let mut buf = [0u8; 4096];
            let n = client
                .sockets
                .get_mut::<TcpSocket>(h)
                .recv_slice(&mut buf)
                .unwrap_or(0);
            if n > 0 {
                got.extend_from_slice(&buf[..n]);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(got, b"hello-uds", "echo 数据应经 UDS 豁免腿往返");

        // 关闭收口：客户端 close → FIN 双向拆 → 流表清空 + reactor fd 归零
        client.sockets.get_mut::<TcpSocket>(h).close();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            if itc.flow_count() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(itc.flow_count(), 0, "流表应清空");
        assert_eq!(itc.reactor_fds(), 0, "reactor 名下 fd 应归零（收口规则表）");
        drop(echo);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// flush_out 的 EAGAIN 语义单元钉（评审 r2-高1 回归）：写满的 socket（自设
    /// 8KB sndbuf 的 socketpair，对端不读）上 1MB 待写——`flush_out` 必须**立即
    /// 返回**（原地重试 = 驱动线程自旋 = 整出口挂死）且余量保留在待写缓冲。
    /// 看门狗 30s abort 兜底：自旋回归时测试进程死而非无限挂起。
    #[test]
    fn flush_out_eagain_returns_with_backlog() {
        let wd_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let wd = {
            let wd_done = Arc::clone(&wd_done);
            std::thread::spawn(move || {
                let t0 = Instant::now();
                while !wd_done.load(Ordering::Relaxed) {
                    if t0.elapsed() > Duration::from_secs(30) {
                        eprintln!("看门狗：flush_out 疑似 EAGAIN 自旋（评审 r2-高1 回归）——abort");
                        unsafe { libc::abort() };
                    }
                    std::thread::sleep(Duration::from_millis(200));
                }
            })
        };
        // Q-G F1（测试态顺手）：socketpair 也走 sysfd 单源（CLOEXEC）——断言不变。
        let (sock_a, sock_b) = crate::sysfd::socketpair_cloexec(
            libc::AF_UNIX,
            libc::SOCK_STREAM,
            0,
        )
        .expect("socketpair");
        let fds = [
            std::os::fd::IntoRawFd::into_raw_fd(sock_a),
            std::os::fd::IntoRawFd::into_raw_fd(sock_b),
        ];
        unsafe {
            let sz: libc::c_int = 8 * 1024;
            libc::setsockopt(
                fds[0],
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &sz as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
        }
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        // 手搓豁免流 + 8KB socketpair 写端当 upstream（对端〔读端〕不读）
        let v = View5 {
            src: Ipv4Addr::new(100, 64, 10, 13),
            src_port: 40000,
            dst: tunnel,
            dst_port: 47903,
            proto: 6,
            tcp_flags: nat::TCP_SYN,
            tcp_seq: 1,
            tcp_ack: 0,
            udp_payload: (0, 0),
        };
        let flow = itc.alloc_flow(&v, Kind::Exempt, Proto::Tcp, Vec::new());
        let fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        set_fd_flags(fd.as_fd(), true).unwrap(); // place_io 契约：fd 已非阻塞（dial_nonblocking 平时的保证）
        itc.place_io(flow, fd, false, None, Instant::now());
        if let Some(io) = itc.flows.get_mut(&flow).and_then(|f| f.io.as_mut()) {
            io.out_tcp.push(&vec![0x5au8; 1024 * 1024]);
        }
        // 必须在预算内返回（自旋形态 = 挂死 = 看门狗 abort）
        itc.flush_out(flow);
        let left = itc
            .flows
            .get(&flow)
            .and_then(|f| f.io.as_ref())
            .map(|io| io.out_tcp.remaining().len())
            .unwrap_or(0);
        assert!(left > 0, "EAGAIN 后余量应留在待写缓冲（非全写非丢弃）——left={left}");
        assert!(left < 1024 * 1024, "应有部分写出（sndbuf 8KB 已吞）——left={left}");
        wd_done.store(true, Ordering::Relaxed);
        let _ = wd.join();
        itc.remove_flow(flow); // fd 收口
        unsafe { libc::close(fds[1]) };
        assert_eq!(itc.reactor_fds(), 0);
    }

    /// 停读对端 → 开读后完整按序 E2E（R-7② 行为面）：TCP 豁免腿对停读窗口的
    /// upstream——待写缓冲积压/续传不得丢字节/丢序；EAGAIN 语义由
    /// flush_out_eagain_returns_with_backlog 单元钉直测（本机 sndbuf 自动调优过
    /// 大，E2E 层逼不出确定性 EAGAIN——大载荷下客户端内层窗口先关，同属背压正
    /// 常行为）。
    #[test]
    fn upstream_partial_write_continues() {
        const TOTAL: usize = 256 * 1024;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<usize>();
        let origin = std::thread::spawn(move || {
            let (mut c, _) = listener.accept().unwrap();
            // 小接收缓冲（关自调优）+ 停读 400ms——促成待写缓冲积压
            unsafe {
                let sz: libc::c_int = 8 * 1024;
                libc::setsockopt(
                    c.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    &sz as *const _ as *const libc::c_void,
                    std::mem::size_of::<libc::c_int>() as u32,
                );
            }
            std::thread::sleep(Duration::from_millis(400));
            let mut got = 0usize;
            let mut expect = 0u8;
            let mut buf = [0u8; 16384];
            while got < TOTAL {
                let n = c.read(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                // 序校验：载荷 = 循环递增字节（积压续传不得丢字节/丢序）
                for &b in &buf[..n] {
                    assert_eq!(b, expect, "字节流应按序无缺口（got={got}）");
                    expect = expect.wrapping_add(1);
                }
                got += n;
            }
            let _ = done_tx.send(got);
        });

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 12),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, port))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline
            && client.sockets.get::<TcpSocket>(h).state() != tcp::State::Established
        {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            client.sockets.get::<TcpSocket>(h).state(),
            tcp::State::Established
        );

        let payload: Vec<u8> = (0..TOTAL as u32).map(|i| (i % 256) as u8).collect();
        let mut sent = 0usize;
        let send_deadline = Instant::now() + Duration::from_secs(10);
        while sent < TOTAL {
            let n = client
                .sockets
                .get_mut::<TcpSocket>(h)
                .send_slice(&payload[sent..])
                .unwrap();
            sent += n;
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            if Instant::now() >= send_deadline {
                panic!("10s 内未灌完（sent={sent}）");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        // 对端开读后持续泵到收齐（待写余量逐拍续传）
        let deadline = Instant::now() + Duration::from_secs(10);
        let got = loop {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            match done_rx.try_recv() {
                Ok(got) => break got,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    panic!("对端线程提前退出（未收齐）")
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
            if Instant::now() >= deadline {
                panic!("10s 内对端未收齐（余量未续传）");
            }
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(got, TOTAL);
        let _ = origin.join();
    }

    /// UDP 会话端到端：客户端栈（udp socket 手搓包形式）→ transit → 回环 UDP echo →
    /// 回投反重写。用 nat::build_udp 手搓（不引 StackB 的 udp 面）。
    #[test]
    fn udp_session_end_to_end() {
        // 回环 UDP echo
        let echo = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = echo.recv_from(&mut buf) {
                if echo.send_to(&buf[..n], from).is_err() {
                    break;
                }
            }
        });

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));

        // 客户端「假栈」：直接手搓 UDP 包（src=100.64.10.3:50000 → dst=127.0.0.1:port）
        let q = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 3),
            50000,
            Ipv4Addr::LOCALHOST,
            port,
            b"udp-echo-q",
        );
        itc.on_plain(q.clone());

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut resp = None;
        while Instant::now() < deadline {
            for p in itc.pump() {
                if let Some(v) = Ipv4View::parse(&p) {
                    if v.proto == 17
                        && v.dst == Ipv4Addr::new(100, 64, 10, 3)
                        && v.src == Ipv4Addr::LOCALHOST
                    {
                        resp = Some(v.payload.to_vec());
                    }
                }
            }
            if resp.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            resp.as_deref(),
            Some(&b"udp-echo-q"[..]),
            "UDP 回投应反重写到客户端"
        );
        assert!(stats.snapshot()[2].1 >= 1, "flows gauge 应计会话");
        // fd 收口回归（评审 H1）：close() 的 teardown 必须给 worker 发 Close（此前
        // idle/close 路径不发 Close，upstream fd 与属主表永久滞留 ⇒ EMFILE）。
        // 断言面 = close 后 pump 不 panic 且流表清空（fd 关闭由 worker 的 Closed
        // 回执驱动——内部通道不可直达，行为由 speedtest/文件实测覆盖）。
        itc.close();
        let _ = itc.pump();
        assert!(itc.flow_count() == 0, "close 后流表应清空");
        drop(echo_thread);
    }

    /// R6.6 P1-② 回归验收（忽略：真跑 ~5-10s 且独占机器才稳）。见 `run_shaped_download`。
    /// 阈值口径（评审 r1 整改的两轮收敛：绝对阈值在重载下假红〔天花板实测可掉到
    /// 10MB/s〕，纯天花板相对阈值在闲机假红〔天花板可上到 56MB/s 而整形链路封在
    /// 24MB/s〕）：**分母 = min(链路速率, 同轮实测 passthrough 天花板)** = 可达速率
    /// （链路容量与机器能力取小——同进程同负载自校准）。
    /// 判别力注记：深队列 2MB 下单流本就不丢包（修复前后都 ≈ 天花板）——A 单流是
    /// **无回归**判据；真正判别本 bug 的是 A 并发（评审消融：门关 9.3MB/s≈42% 红、
    /// 门开 15.8MB/s≈83% 绿）与 B 浅队列（门关塌到 MB/s 级）。
    /// R8-8a：CC 垫片退役（smoltcp 0.14 CUBIC 接管）后本 harness 仍是同一条门——
    /// 链路模型的突发额度修正见 `DirLink.burst` 注记（mega-burst 额度会把 smoltcp
    /// 接收端的 ACK 时钟拍扁，属模型失真不是 CC 缺陷；内核/gVisor 接收端无此形态）。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~5-10s，验证时 cargo test -- --ignored 显式跑（重负载下阈值随同轮天花板自校准）"]
    fn downlink_lossy_link_recovery() {
        // A：深队列（部署形态）——单流（无回归）+ 并发 6 流（判别）。
        // 传输量口径（R8-8a）：单流 16→64MB / 并发 6×3→6×8MB——上游 CUBIC 的慢启动
        // 爬坡在本 harness 的接收端 ACK 合并形态下需 ~1.5-2s（smoltcp 接收端每 poll
        // 至多一个 ACK ⇒ 爬坡期 ACK 稀疏），16MB 量级的传输被爬坡期支配（实测 5.5MB/s
        // 而稳态 24.8MB/s=满链路）——量级提到稳态支配（塌陷判别语义不变：0.4MB/s
        // 塌陷形态在 64MB 下 160s 超时必红）。
        const LINK_RATE_MB: f64 = 24.0; // DirLink::deep 的速率参数（改一处同步两处）
        let (secs, bytes, _, _) = run_shaped_download(
            1,
            16 * 1024 * 1024,
            DirLink::passthrough(),
            DirLink::passthrough(),
            None, // 天花板臂：无损透传 + 整形关——量的是机器能力，不掺整形开销
        );
        let ceiling = bytes as f64 / secs / (1024.0 * 1024.0);
        let (secs, bytes, down, tail) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::deep(),
            DirLink::deep(),
            PRODUCT_SHAPE,
        );
        let got = bytes as f64 / secs / (1024.0 * 1024.0);
        let tail = tail.expect("64MB 深队列臂实测 >2s（爬坡即 >1.5s）——尾窗必在");
        println!(
            "A 单流：无损天花板 {ceiling:.1}MB/s → 深队列有损 {got:.1}MB/s 尾2s={tail:.1}MB/s（丢 {} 包 / 峰值队列 {}B）",
            down.dropped, down.peak_queue
        );
        let reach1 = LINK_RATE_MB.min(ceiling);
        assert!(
            got >= reach1 * 0.5,
            "深队列形态下单流吞吐 {got:.1}MB/s 应 ≥ 可达速率 {reach1:.1}MB/s（min(链路 24, 天花板 {ceiling:.1})）的 50%（无回归判据）"
        );
        // R8-2 8g 尾窗速率门（评审 F1 登记义务）：最后 2s 均值 ≥ 窗口均值的 50%——
        // 防「前段突发把均值抬过门、尾段已塌」的假绿（真机口径同款判别 = speedtest
        // 结算行的 尾3s 速率）。取 50% 而非 100%：CUBIC 爬坡期均值偏低、稳态尾窗
        // 通常 ≥ 均值，50% 只拦塌陷形态不拦正常波动。
        assert!(
            tail >= got * 0.5,
            "尾窗（最后 2s）速率 {tail:.1}MB/s 应 ≥ 窗口均值 {got:.1}MB/s 的 50%（尾段塌陷 = 假绿拦截）"
        );

        let (secs, bytes, _, _) = run_shaped_download(
            6,
            3 * 1024 * 1024,
            DirLink::passthrough(),
            DirLink::passthrough(),
            None, // 同上：天花板臂
        );
        let ceiling6 = bytes as f64 / secs / (1024.0 * 1024.0);
        let (secs, bytes, down6, _) = run_shaped_download(
            6,
            8 * 1024 * 1024,
            DirLink::deep(),
            DirLink::deep(),
            PRODUCT_SHAPE,
        );
        let got6 = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "A 并发 6 流：无损天花板 {ceiling6:.1}MB/s → 深队列有损 {got6:.1}MB/s（丢 {} 包 / 峰值队列 {}B）",
            down6.dropped, down6.peak_queue
        );
        // R8-8a 重标定：并发臂的「塌陷判别」职责移交 B 臂（修正模型下 CC=None 在浅队列
        // 仍复现 <0.2MB/s 级塌陷实测；深队列 mega-blast 塌陷形态随突发额度修正不再构成
        // 判别信号）。本臂保留为吞吐回归门，阈值取可达速率的 1/6=4.0MB/s：上游 CUBIC
        // 无 pacing，N 流慢启动过冲 + 2MB 尾丢缓冲的同步振荡（harness 接收端 ACK 合并
        // 放大）实测聚合 5.0-11.5MB/s 波动——4.0 门内留 25% 余量，同时仍远高于 0.4MB/s
        // 塌陷基线（10×）防「CC 被意外关掉」类回归漏检（B 臂另有兜底）。
        let reach6 = LINK_RATE_MB.min(ceiling6);
        assert!(
            got6 >= reach6 / 6.0,
            "深队列形态下并发短流聚合 {got6:.1}MB/s 应 ≥ 可达速率 {reach6:.1}MB/s（min(链路 24, 天花板 {ceiling6:.1})）的 1/6（吞吐回归门；塌陷判别在 B 臂）"
        );

        // B：浅队列（192KB ≪ BDP）——修复前真代码（无门控无 pacing）实测 0.4MB/s
        // （2026-10-04，commit c0a244f 前的 4325c1b 基线 + 同链路形态；部分消融
        // 〔仅去 allowed 上限、保留 pacing〕实测 5.2MB/s，介于两者之间——判据下界
        // 取保守的 3.2MB/s = 0.4×8）。⚠️ 绝对门 0 余量（评审 r1-F18 实复现：负载下
        // 3.2 vs 门 3.2 红；闲机 3.2-3.4、旧垫片 8.1）——R8-1 F5 的「自校准分母」
        // 整改挂 R8-3（再动阈值需对照跑先行，8f 已备数据）；引用本臂数字一律带
        // 负载状态（闲机/负载）。
        let (secs, bytes, downb, _) = run_shaped_download(
            1,
            8 * 1024 * 1024,
            DirLink::shallow(),
            DirLink::shallow(),
            PRODUCT_SHAPE,
        );
        let gotb = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "B 浅队列单流（整形 on）：{gotb:.1}MB/s（丢 {} 包 / 峰值队列 {}B；整形前真代码 3.2MB/s 带状 / 塌陷基线 0.4MB/s）",
            downb.dropped, downb.peak_queue
        );
        // R8-3 F5/F18 重标定（r1-F18 两轮复现负载下 0 余量红——旧绝对门 3.2 与实测
        // 带重合）。**为什么不用 A 臂同款自校准分母**：本臂的稳态带不随天花板走——
        // 实测（8f/R8-3 同带）整形前后都钉在 ~3.2-3.4MB/s（此值由 harness 接收端
        // ACK 时钟形态决定——smoltcp 每 poll 至多一个 ACK 的稀疏时钟 + 浅队列小窗
        // 均衡，与链路 24/天花板 25-66 无关：R8-3 实测 A 臂天花板 25.3/65.8 时本臂
        // 仍 3.4）。天花板派生分母会把门耦合到机器负载而带不动——正是 F18 假红的
        // 根因形态。**取而代之：绝对门 = 健康带与塌陷基线的几何中点** sqrt(3.3×0.4)
        // ≈ 1.2（8f 数据：现行 CUBIC 带 3.2 / 旧垫片 8.1 / 塌陷基线 0.4）——两侧各
        // 留 ≥2.7×/3× 余量：负载把带压半（1.6）仍 1.3× 过门；CC 关/pacing 丢失类
        // 塌陷（0.4）仍 3× 红差。引用本臂数据一律带负载状态（闲机/负载）。
        const B_ARM_GATE_MB: f64 = 1.2;
        assert!(
            gotb >= B_ARM_GATE_MB,
            "浅队列压力形态吞吐 {gotb:.1}MB/s 应 ≥ {B_ARM_GATE_MB}MB/s（健康带 3.2-3.4 与塌陷基线 0.4 的几何中点门——两侧 ≥2.7× 判别余量；塌陷回归）"
        );
    }

    /// P1 两级前置背压：pump_hold 只并入不释放（credit/时刻表原样——下拍照常
    /// 释放，无双重记账）；整形关臂直通。
    #[test]
    fn pump_hold_defers_without_double_accounting() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        itc.cfg.tx_shape = Some(TxShape { rate: 1 << 20, burst: 1024 });
        for i in 0..4u8 {
            itc.tx_out.push(vec![i; 8]);
        }
        let out = itc.pump_hold();
        assert!(out.is_empty(), "hold 拍应零释放");
        assert_eq!(itc.tx_deferred.len(), 4, "4 包全部滞留 FIFO");
        assert_eq!(itc.tx_deferred_bytes, 32);
        // credit 不被 hold 拍扣减（tx_shape_release 才扣）——下拍全量释放验证：若
        // hold 拍扣过 credit，释放量会短缺。先攒 credit（续水按 dt——测试瞬间
        // dt≈0 ⇒ credit≈0，sleep 20ms 攒出 rate×0.02s = 20KB ≫ 4×8B 额度）。
        std::thread::sleep(Duration::from_millis(20));
        let out2 = itc.pump();
        assert_eq!(out2.len(), 4, "hold 后首拍应全量释放（credit 完整）");
        let seq: Vec<u8> = out2.iter().map(|p| p[0]).collect();
        assert_eq!(seq, (0..4u8).collect::<Vec<_>>(), "FIFO 保序");
        // 整形关臂：hold = 直通（无 FIFO 可滞留）
        itc.cfg.tx_shape = None;
        itc.tx_out.push(vec![9u8; 8]);
        let out3 = itc.pump_hold();
        assert_eq!(out3.len(), 1, "整形关臂 hold 直通");
    }

    /// 宽限全量释放的清空语义（评审 r2-1.1 整改验收）：滞留队列在 pump_with_flush
    /// 后必须清空——「宽限期尾数据/FIN 不丢」（M2）不因整形回归。构造面 =
    /// 直接驱动 tx_shape_release 的 flush_all 分支（同代码路径）+ pump_with_flush
    /// 的滞留排空断言（无流量面，验证的是清空语义本身）。
    #[test]
    fn grace_flush_drains_deferred() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        itc.cfg.tx_shape = Some(TxShape { rate: 1, burst: 16 }); // 极小额度：一切都会滞留
        // 直接产出一批待发包（模拟拦截栈排空产物——不建真实流）
        for i in 0..8u8 {
            itc.tx_out.push(vec![i; 8]);
        }
        let out = itc.tx_shape_release(false);
        assert!(out.is_empty(), "极小额度下本拍应零释放（全滞留）");
        assert_eq!(itc.tx_deferred.len(), 8, "8 包应全部滞留");
        assert_eq!(itc.tx_deferred_bytes, 64);
        // flush_all：全量释放 + 记账清零
        for i in 8..12u8 {
            itc.tx_out.push(vec![i; 8]);
        }
        let out = itc.tx_shape_release(true);
        assert_eq!(out.len(), 12, "宽限全量释放（滞留 8 + 本拍 4）");
        assert!(itc.tx_deferred.is_empty());
        assert_eq!(itc.tx_deferred_bytes, 0, "字节记账随清空归零");
        let seq: Vec<u8> = out.iter().map(|p| p[0]).collect();
        assert_eq!(seq, (0..12u8).collect::<Vec<_>>(), "FIFO 保序（含滞留部分）");
        // pump_with_flush 幂等（已空 = 空返回）
        let out2 = itc.pump_with_flush();
        assert!(out2.is_empty());
    }

    /// env 值匹配语义（评审 r2-自补1）：`off/0/false` = 关，未设/`on/1/true` = 开。
    /// 注意本测试直接钉 `tx_shape_default` 的匹配逻辑面（env 是进程级——并行
    /// 测试共享环境会互相污染，值面以 match 分支形态为准，不真设 env）。
    #[test]
    fn tx_shaping_env_value_semantics() {
        // 与 tx_shape_default 同款的匹配表（镜像断言——env 本身不在单测里设）
        let parse = |v: Option<&str>| match v {
            Some("off") | Some("0") | Some("false") => false,
            Some(_) | None => true,
        };
        assert!(!parse(Some("off")) && !parse(Some("0")) && !parse(Some("false")));
        assert!(parse(None) && parse(Some("on")) && parse(Some("1")) && parse(Some("true")));
        assert!(parse(Some("1")), "`=1` 必须是开（presence 语义会把消融方向搞反）");
    }

    /// 令牌桶释放的机制单测（R8-3 8i；纯函数面——时间注入，不依赖墙钟）：
    /// ① 突发额度截断本拍释放深度；② 续水后下拍续传（FIFO 保序）；③ dt=0 时
    /// 零续水（紧循环不放大）；④ 大于 burst 的包防御性直通（防极小 burst 死锁）。
    #[test]
    fn shape_slice_budget_and_continuation() {
        // 令牌桶机制面（参数与实现同构）
        let mut deferred = std::collections::VecDeque::new();
        // 5×1000B 倾泻，dt=0（紧拍）：只放 3（额度 3000）
        let (out, run) = shape_slice(
            &mut deferred,
            vec![vec![0u8; 1000]; 5],
            ShapeRun { credit: 3000.0, deferred_bytes: 0 },
            TxShape { rate: 1000, burst: 3000 },
            0.0,
        );
        assert_eq!(out.len(), 3, "突发额度 3000B 截断本拍释放");
        assert_eq!(deferred.len(), 2, "余量滞留");
        assert!(run.credit < 1000.0);
        // dt=1s：续水 1000（cap 3000，余 credit）→ 放 1
        let (out, run) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: run.credit, deferred_bytes: 2000 },
            TxShape { rate: 1000, burst: 3000 },
            1.0,
        );
        assert_eq!(out.len(), 1, "下拍续传一包（续水 1000B）");
        assert_eq!(deferred.len(), 1);
        // 空闲 60s：令牌回满 → 余量全放
        let (out, _) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: run.credit, deferred_bytes: 1000 },
            TxShape { rate: 1000, burst: 3000 },
            60.0,
        );
        assert_eq!(out.len(), 1, "空闲后桶满，滞留清空");
        let _ = &run;
        assert!(deferred.is_empty());
        // 保序：1..=6 依次入，分两拍释放，并起来仍是 1..=6
        let mut deferred = std::collections::VecDeque::new();
        let pkts: Vec<Vec<u8>> = (1..=6u8).map(|i| vec![i; 1000]).collect();
        let (mut out1, run) = shape_slice(
            &mut deferred,
            pkts,
            ShapeRun { credit: 3000.0, deferred_bytes: 0 },
            TxShape { rate: 1000, burst: 3000 },
            0.0,
        );
        let (mut out2, _) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: run.credit, deferred_bytes: 3000 },
            TxShape { rate: 1000, burst: 3000 },
            60.0,
        );
        out1.append(&mut out2);
        let seq: Vec<u8> = out1.iter().map(|p| p[0]).collect();
        assert_eq!(seq, vec![1, 2, 3, 4, 5, 6], "FIFO 释放保序");
        // 极小 burst 配置：大于 burst 的包直通（不死锁）
        let mut deferred = std::collections::VecDeque::new();
        let (out, _) = shape_slice(
            &mut deferred,
            vec![vec![7u8; 500]],
            ShapeRun { credit: 100.0, deferred_bytes: 0 },
            TxShape { rate: 1000, burst: 100 },
            0.0,
        );
        assert_eq!(out.len(), 1, "大于 burst 的包防御性直通");
        assert!(deferred.is_empty());
    }



    /// wait_hint 三态单测（reactor 批扩档）：整形滞留非空 ∨ reactor 等待者（connect
    /// 在途/待写缓冲非空）= Some(1ms)；全空 = None（5ms 常规拍）。reactor_waiters 位
    /// 由 pump 尾刷新——此处直接置位测判定面（刷新逻辑由 E2E 面覆盖）。
    #[test]
    fn wait_hint_states() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        assert_eq!(itc.wait_hint(), None, "整形关且无等待者 = None");
        itc.cfg.tx_shape = Some(TxShape { rate: 10_000_000, burst: 4096 });
        assert_eq!(itc.wait_hint(), None, "滞留空且无等待者 = None");
        itc.tx_deferred.push_back(vec![0u8; 1000]);
        assert_eq!(itc.wait_hint(), Some(Duration::from_millis(1)), "整形滞留 = 1ms");
        itc.tx_deferred.clear();
        itc.reactor_waiters = true;
        assert_eq!(itc.wait_hint(), Some(Duration::from_millis(1)), "reactor 等待者 = 1ms");
        itc.reactor_waiters = false;
        assert_eq!(itc.wait_hint(), None);
    }



    /// tx_shape_resolve 的 config 面表格测试（env>config>默认 的解析面；config
    /// 分支不碰 env，可直测；env 面保留镜像测试并注明局限）。
    #[test]
    fn tx_shape_resolve_config_table() {
        // 默认（无 config）= 产品默认参数
        let r = tx_shape_resolve(None).unwrap();
        assert_eq!(r.rate, TX_SHAPE_RATE);
        assert_eq!(r.burst, TX_SHAPE_BURST);
        // rate/burst 覆盖
        let r = tx_shape_resolve(Some(TxShapeCfg {
            rate_mbps: Some(64),
            burst_kb: Some(128),
        }))
        .unwrap();
        assert_eq!(r.rate, 64 * 1024 * 1024);
        assert_eq!(r.burst, 128 * 1024);
        // 非法值（0）静默回落默认
        let r = tx_shape_resolve(Some(TxShapeCfg {
            rate_mbps: Some(0),
            burst_kb: Some(0),
        }))
        .unwrap();
        assert_eq!(r.rate, TX_SHAPE_RATE);
        assert_eq!(r.burst, TX_SHAPE_BURST);
    }



    /// 冷空口形态验证（R8-3 8i；r2-5.1 复核修正后的口径）。**模型判别力（实测
    /// 2026-10-05，串行跑）**：空口速率 80MiB/s（须高于产品整形 R=64MiB/s——把
    /// 「持续过载」与「团块」两种到达丢失解耦）时，on/off 稳定分离 ~2.5×：
    /// on 9.6-9.8MB/s / **0 到达丢失**（团块钳在产品突发 160KiB < 额度 192KiB），
    /// off 3.9MB/s / 28-32 到达丢失（无钳制团块随拍频变粗）。**边界**：闲机上
    /// harness 泵节奏（500µs/拍）自身就是整形器，off 臂团块也 ≤~190KB ⇒ 无分离
    /// （分离幅度随机器负载变化）——off 臂保数据行不设门，**冷悬崖的终裁 = 真机
    /// 8j 矩阵**（已过：B 冷/热 0.85）。判别面：
    /// ① 10ms 到达额度臂（空口容忍充分——0 到达丢失）= 本链路可达速率自校准分母；
    /// ② 冷形态臂（1.2× 产品突发额度）整形 on：吞吐 ≥ 可达速率 50% 且到达丢失
    ///    ≤ 万分之一（团块事件应零——非零即 R>空口速率的混叠回归）；
    /// ③ off 臂数据行（无门——分离幅度负载依赖，见上）。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~15-20s（三臂 64MB @ 冷链路），验证时 cargo test -- --ignored 显式跑"]
    fn cold_air_form_verification() {
        // ① 可达速率臂：空口容忍充分（10ms 额度）——整形 on，量「这条冷链路+本机」
        //    无到达丢失形态下跑得到的速率（BDP≈2MB > FLOW_TX_BUF 1MB ⇒ 实测
        //    受窗口约束 ~10-15MB/s，分母取实测量而非 80——同轮自校准）
        let (secs, bytes, down, _) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::cold_with(10 * 80 * 1024 * 1024 / 1000),
            DirLink::cold_with(10 * 80 * 1024 * 1024 / 1000),
            PRODUCT_SHAPE,
        );
        let reach = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "冷空口 可达臂（10ms 额度，整形 on）：{reach:.1}MB/s（到达口丢 {} / 队列丢 {}）",
            down.air_dropped, down.dropped
        );
        assert_eq!(down.air_dropped, 0, "可达臂不应有到达丢失（额度 ≫ 线上团块）");
        // ② 冷形态臂：1.2× 产品突发额度（192KiB）
        let (secs, bytes, down, tail) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::cold(),
            DirLink::cold(),
            PRODUCT_SHAPE,
        );
        let got = bytes as f64 / secs / (1024.0 * 1024.0);
        let total_pkgs = (bytes / 1300).max(1);
        println!(
            "冷空口 冷形态臂（192KiB 额度，整形 on）：{got:.1}MB/s（到达口丢 {} / 队列丢 {}；可达 {reach:.1}）",
            down.air_dropped, down.dropped
        );
        assert!(
            got >= reach * 0.5,
            "冷形态臂 {got:.1}MB/s 应 ≥ 可达速率 {reach:.1}MB/s 的 50%（整形下团块不触冷预算——无塌陷）"
        );
        assert!(
            down.air_dropped as f64 <= total_pkgs as f64 / 10000.0,
            "到达丢失 {} 包应 ≤ 万分之一（整形把线上团块钳在产品突发 160KiB < 额度 192KiB）",
            down.air_dropped
        );
        // 尾窗门（与 A 臂同款——拦前段达标尾段塌陷）
        if let Some(tail) = tail {
            assert!(
                tail >= got * 0.5,
                "冷形态臂尾窗 {tail:.1}MB/s 应 ≥ 均值 {got:.1}MB/s 的 50%"
            );
        }
        // ③ off 臂：数据行（无门——见头注「模型表达力边界」）
        let (secs, bytes, down, _) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::cold(),
            DirLink::cold(),
            None,
        );
        let got_off = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "冷空口 off 臂（192KiB 额度，整形 off——数据行）：{got_off:.1}MB/s（到达口丢 {} / 队列丢 {}）",
            down.air_dropped, down.dropped
        );
    }

    /// F2 burst 敏感性矩阵（评审 §五 F2 登记项——「整形参数即该矩阵实践面」的数据
    /// 面；诊断打印、不设门）：冷空口（80MiB/s）到达额度扫 {1,2,5,10}ms =
    /// {80,160,400,800}KiB，产品整形 on——观察额度贴着/低于产品突发额度（160KiB）
    /// 时的劣化拐点。产出表入册 PERF-AB §9（R8-3 终版）。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~6-10s（4 臂各 16MB 量级），验证时 cargo test -- --ignored 显式跑"]
    fn cold_air_burst_allowance_matrix() {
        for ms in [1u64, 2, 5, 10] {
            let allowance = (80 * 1024 * 1024usize / 1000) * (ms as usize);
            let (secs, bytes, down, _) = run_shaped_download(
                1,
                16 * 1024 * 1024,
                DirLink::cold_with(allowance),
                DirLink::cold_with(allowance),
                PRODUCT_SHAPE,
            );
            let got = bytes as f64 / secs / (1024.0 * 1024.0);
            println!(
                "F2 矩阵：到达额度 {ms}ms（{allowance}B，产品突发 160KiB）→ {got:.1}MB/s（到达口丢 {} / 队列丢 {}）",
                down.air_dropped, down.dropped
            );
        }
    }

    /// 三臂 harness（D-3 反过拟合约束 2：调参不得过拟合当前环境——所有真机测量都在
    /// 家宽+Wi-Fi 形态，慢/快路径以 harness 为准）：慢路径（2.5MB/s + 40ms + 64KB
    /// 队列——2.4GHz/蜂窝形态）与快路径（120MB/s + 2ms + 8MB 队列——千兆有线形态）
    /// 上，**adaptive pacing 不得劣化**：on ≥ 0.7×off（慢臂防滴流劣化、快臂防人为
    /// 限速）。现役深队列臂（24MB/s/26ms/2MB）= downlink_lossy_link_recovery 的
    /// A/B 臂（不在此重复）。0.7 门 = 判「结构性劣化」而非噪声（harness 轮间波动
    /// ~10-20%）；两臂同链路同轮对照，链路差异被除掉。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~25-40s（四臂），验证时 cargo test -- --ignored 显式跑（串行——r2-自补7）"]
    fn pacing_three_path_arms() {
        // (名, 队列cap, 速率, 单向时延, 传输量, 说明)
        let arms: &[(&str, usize, usize, Duration, usize)] = &[
            ("慢路径", 64 * 1024, 2_621_440, Duration::from_millis(40), 8 * 1024 * 1024),
            ("快路径", 8 * 1024 * 1024, 125_829_120, Duration::from_millis(2), 48 * 1024 * 1024),
        ];
        // 每臂 3 轮取中位（评审 r2-6.5：单发采样曾在慢臂出现「队丢不变但吞吐差
        // 60%」的不自洽读数——R8-2「单轮数据不可用于 A/B」教训在 harness 面同样成立）
        for (name, cap, rate, delay, bytes) in arms {
            let mut off_meds = Vec::new();
            let mut on_meds = Vec::new();
            let mut drops_off = 0u64;
            let mut drops_on = 0u64;
            for _ in 0..3 {
                let (secs, got, down, _) = run_shaped_download(
                    1,
                    *bytes,
                    DirLink::new(*cap, *rate, *delay),
                    DirLink::new(*cap, *rate, *delay),
                    None,
                );
                off_meds.push(got as f64 / secs / (1024.0 * 1024.0));
                drops_off += down.dropped;
                let (secs, got, down, _) = run_shaped_download(
                    1,
                    *bytes,
                    DirLink::new(*cap, *rate, *delay),
                    DirLink::new(*cap, *rate, *delay),
                    PRODUCT_SHAPE,
                );
                on_meds.push(got as f64 / secs / (1024.0 * 1024.0));
                drops_on += down.dropped;
            }
            off_meds.sort_by(|a, b| a.partial_cmp(b).unwrap());
            on_meds.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (mbps_off, mbps_on) = (off_meds[1], on_meds[1]);
            println!(
                "三臂 {name}（链路 {}MB/s）：off={mbps_off:.1}MB/s（队丢 {}） on(adaptive)={mbps_on:.1}MB/s（队丢 {}）",
                rate / (1024 * 1024),
                drops_off,
                drops_on
            );
            assert!(
                mbps_on >= mbps_off * 0.7,
                "{name} 臂 pacing on={mbps_on:.1} 不应劣化 off={mbps_off:.1} 的 70%（慢臂滴流/快臂限速 = 过拟合回归）"
            );
        }
    }

    /// 单向链路模型：有限 FIFO 队列（超额即丢）+ 速率出队 + 固定传播时延——
    /// 模拟真机 WiFi（出口下行突发超过队列容量 ⇒ 突发规模丢包，E2E 20× 塌陷的形态）。
    struct DirLink {
        queue: std::collections::VecDeque<Vec<u8>>,
        queued: usize,
        cap: usize,
        rate: usize,
        /// 外层 PMTU 瓶颈注入（P2 坑 4 回归专测）：包长超限即丢（DF 语义——
        /// 模拟「外层路径承载不住」的丢包形态，**不覆盖**无 DF 分片可达那条
        /// 真实兜底；评审 P2-r1-7(c)）。None = 不注入。
        mtu_limit: Option<usize>,
        /// mtu_limit 丢包独立计数（不污染 dropped 的冷/浅臂零丢断言）。
        mtu_dropped: u64,
        /// 观测到的最大包长（P2 段尺寸机器判据：1380 档下载数据段 = 1340+40）。
        seen_max: usize,
        credit: f64,
        /// 令牌桶的**突发额度**（R8-8a 修正）：出队许可以 credit 计，空闲期累积的
        /// credit 以此为上限——与队列容量解耦。此前误把额度上限设成队列容量
        /// （2MB），空闲后一次放出 2MB = 24MB/s 链路 85ms 的量——物理链路没有这种
        /// 突发；该 mega-burst 形态把到达拍打成「每 RTT 一大团」，smoltcp 接收端
        /// 每 poll 至多回一个 ACK（顺序数据）⇒ 每团一 ACK ⇒ 发送端 ACK 时钟饿死
        /// （CUBIC 每 RTT 只 +1 MSS——0.14 迁移实测 3.5MB/s 的根因；内核/gVisor
        /// 接收端无此形态——团内每 2 段回 ACK）。额度 = rate×2ms（≈48KB，比一个
        /// BDP 小一个量级、比驱动拍粒度大一个量级——整形器的物理突发参数）。
        burst: usize,
        last: Instant,
        delay: Duration,
        flying: Vec<(Instant, Vec<u8>)>,
        dropped: u64,
        peak_queue: usize,
        // ---- 冷空口形态（R8-3 8i；§9.5 悬崖机制的模型化） ----
        /// 到达侧冷预算 (突发额度 B, 当前 credit, 上次续水)：Some = 启用。瞬时到达
        /// 超过「额度 + rate×经过时间」的部分在**到达口**丢弃（先于队列判满——deep
        /// 队列也丢；物理面对应 WiFi 电源态未热时空口对团块的成组丢失，与队列容量
        /// 无关）。`air_dropped` 单独计数。
        cold: Option<(usize, f64, Instant)>,
        air_dropped: u64,
    }

    impl DirLink {
        /// 无损透传（量天花板用）。
        fn passthrough() -> Self {
            Self::new(usize::MAX, usize::MAX / 2, Duration::from_millis(1))
        }
        /// WiFi 部署形态：24MB/s 速率 + 2MB 队列（WG socket SO_SNDBUF=4MB 的保守
        /// 折半，见 bind.rs）+ 13ms 单向时延。
        fn deep() -> Self {
            Self::new(2 * 1024 * 1024, 24 * 1024 * 1024, Duration::from_millis(13))
        }
        /// 浅队列压力形态：同速率/时延，队列 192KB（≪ BDP 648KB）。
        fn shallow() -> Self {
            Self::new(192 * 1024, 24 * 1024 * 1024, Duration::from_millis(13))
        }
        /// 冷电源态形态（R8-3 8i；r2-5.1 复核后改 80MiB/s 空口）：**速率必须高于
        /// 产品整形 R=64MiB/s**——否则「持续到达 > 空口续水」的到达丢失与团块
        /// 丢失混在同一计数里，硬门随机器负载随机红（负载拖慢 harness 拍频 ⇒
        /// 释放节奏变粗 ⇒ 持续过载面被放大）。80 > 64 ⇒ 到达口丢的只剩团块事件，
        /// 「整形把团块钳在额度内 ⇒ 0 到达丢失」的断言才是纯的。1MB 队列
        /// （bufferbloat 深度——队列本身不构成瓶颈）+ 到达侧冷预算 192KiB =
        /// 产品突发额度 160KiB 的 1.2×（模型假设：空口容忍数百 µs 级团块、不容忍
        /// ms 级团块——即修复论点；真机裁决 = 8j 矩阵冷连形态）。F2 矩阵臂经
        /// `cold_with` 扫额度。
        fn cold() -> Self {
            Self::cold_with(192 * 1024)
        }
        /// 冷形态 + 自定到达额度（F2 burst 敏感性矩阵 {1,2,5,10}ms×rate 用）。
        fn cold_with(allowance: usize) -> Self {
            let mut l = Self::new(1024 * 1024, 80 * 1024 * 1024, Duration::from_millis(13));
            l.cold = Some((allowance, allowance as f64, Instant::now()));
            l
        }
        fn new(cap: usize, rate: usize, delay: Duration) -> Self {
            Self {
                queue: Default::default(),
                queued: 0,
                cap,
                rate,
                credit: 0.0,
                burst: rate / 500, // rate × 2ms（见字段注记；500 = 1s/2ms）
                last: Instant::now(),
                delay,
                flying: Vec::new(),
                dropped: 0,
                peak_queue: 0,
                cold: None,
                air_dropped: 0,
                mtu_limit: None,
                mtu_dropped: 0,
                seen_max: 0,
            }
        }
        fn send(&mut self, pkt: Vec<u8>) {
            // MTU 瓶颈（到达口最先判——形态学 = 外层一跳就丢）
            if let Some(limit) = self.mtu_limit {
                if pkt.len() > limit {
                    self.mtu_dropped += 1;
                    return;
                }
            }
            self.seen_max = self.seen_max.max(pkt.len());
            // 到达口冷预算：先于队列判满（团块敌意与队列容量无关——见 cold 字段注记）
            if let Some((allow, credit, last)) = &mut self.cold {
                let now = Instant::now();
                let dt = now.duration_since(*last).as_secs_f64();
                *last = now;
                *credit = (*credit + self.rate as f64 * dt).min(*allow as f64);
                if pkt.len() as f64 > *credit {
                    self.air_dropped += 1;
                    return;
                }
                *credit -= pkt.len() as f64;
            }
            if self.queued + pkt.len() > self.cap {
                self.dropped += 1;
                return;
            }
            self.queued += pkt.len();
            self.peak_queue = self.peak_queue.max(self.queued);
            self.queue.push_back(pkt);
        }
        fn advance(&mut self, now: Instant) -> Vec<Vec<u8>> {
            let dt = now.duration_since(self.last).as_secs_f64();
            self.last = now;
            self.credit = (self.credit + self.rate as f64 * dt).min(self.burst as f64);
            while let Some(front) = self.queue.front() {
                if self.credit < front.len() as f64 {
                    break;
                }
                self.credit -= front.len() as f64;
                self.queued -= front.len();
                let pkt = self.queue.pop_front().expect("front 已判");
                self.flying.push((now + self.delay, pkt));
            }
            let mut out = Vec::new();
            let mut i = 0;
            while i < self.flying.len() {
                if self.flying[i].0 <= now {
                    let (_, pkt) = self.flying.remove(i);
                    out.push(pkt);
                } else {
                    i += 1;
                }
            }
            out
        }
    }

    /// 尾窗（最后 2s）平均速率，MB/s（R8-2 8g 尾窗速率门的计算面）。`None` =
    /// 传输不足 2s（首采样点在 t≈0 预置下必在；无 ≤ t-2s 的点 ⇒ 窗口太短）——
    /// 类型承担不变量，设门侧必须显式处理（评审 r1-F3：0.0 哨兵会被当真速率）。
    /// span 构造上 ≥2s（取样点 t ≤ t_end-2）——无除零面。
    fn tail_rate_mbps(timeline: &[(f64, usize)], t_end: f64, total: usize) -> Option<f64> {
        let (_, base) = timeline
            .iter()
            .rev()
            .find(|(t, _)| *t <= t_end - 2.0)
            .copied()?;
        let span = t_end - timeline.iter().rev().find(|(t, _)| *t <= t_end - 2.0)?.0;
        Some((total - base) as f64 / span / (1024.0 * 1024.0))
    }

    /// 一轮受控下载：n_flows 条并发流（各一台栈 B 客户端，独立隧道 IP）经共享的上/下
    /// 行链路拉 bytes_each 字节；豁免腿转投回环 origin。返回（耗时秒, 总字节, 下行统计）。
    fn run_shaped_download(
        n_flows: usize,
        bytes_each: usize,
        mut up: DirLink,
        mut down: DirLink,
        tx_shape: Option<TxShape>,
    ) -> (f64, usize, DirLink, Option<f64>) {
        let timeout = Duration::from_secs(120);
        // origin：回环 TCP，每连接写满 bytes_each 后 shutdown 写半边
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let origin = std::thread::spawn(move || {
            use std::io::Write as _;
            let mut conns = Vec::new();
            for _ in 0..n_flows {
                if let Ok((c, _)) = listener.accept() {
                    conns.push(c);
                }
            }
            let mut writers = Vec::new();
            for mut c in conns {
                let n = bytes_each;
                writers.push(std::thread::spawn(move || {
                    let chunk = vec![0x5au8; 128 * 1024];
                    let mut left = n;
                    while left > 0 {
                        let k = chunk.len().min(left);
                        if c.write_all(&chunk[..k]).is_err() {
                            break;
                        }
                        left -= k;
                    }
                    let _ = c.shutdown(std::net::Shutdown::Write);
                    // 排干对端残留（客户端只回 ACK，不占接收缓冲——防 FIN 前 RST）
                    let mut buf = [0u8; 4096];
                    loop {
                        match std::io::Read::read(&mut c, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                }));
            }
            for w in writers {
                let _ = w.join();
            }
        });

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut cfg = cfg_base(tunnel);
        cfg.tx_shape = tx_shape; // 产品臂 = Some(默认参数)；消融臂 = None（harness 自控，不经 env）
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        let mut clients = Vec::new();
        for i in 0..n_flows {
            let mut s = StackB::new(
                Ipv4Addr::new(100, 64, 10, 40 + i as u8),
                tunnel,
                SmolInstant::from_millis(0),
            );
            let h = s
                .connect(std::net::SocketAddrV4::new(tunnel, port))
                .unwrap();
            clients.push((s, h));
        }

        let t0 = Instant::now();
        let mut received = vec![0usize; n_flows];
        // timeout 由参数（见函数头注记）
        let mut last_diag = Instant::now() - Duration::from_secs(2);
        let mut last_drop = 0u64;
        // 尾窗速率的采样线（R8-2 8g 尾窗速率门：每 0.5s 记 (秒, 累计字节)——收尾取
        // 最后 2s 的均值，判「窗口收在稳态」而非靠前段突发达标）。
        let mut timeline: Vec<(f64, usize)> = Vec::new();
        loop {
            let now = Instant::now();
            let tel = now.duration_since(t0);
            if tel > timeout {
                break;
            }
            if now.duration_since(last_diag) >= Duration::from_millis(500) {
                last_diag = now;
                timeline.push((tel.as_secs_f64(), received.iter().sum()));
                let f = itc.flows.values().next();
                if let Some(f) = f {
                    let (sq, bl) = f
                        .sock
                        .map(|h| {
                            let s = itc.sockets.get_mut::<TcpSocket>(h);
                            (s.send_queue(), 0usize)
                        })
                        .unwrap_or((0, 0));
                    println!(
                        "t={:>5}ms recv={:>4}MB txq={:>7} backlog={:>7} dropΔ={}/{}",
                        tel.as_millis(),
                        received.iter().sum::<usize>() / (1024 * 1024),
                        sq,
                        f.tx_backlog.remaining().len(),
                        down.dropped - last_drop,
                        up.dropped
                    );
                    let _ = bl;
                }
                last_drop = down.dropped;
            }
            let smol_now = SmolInstant::from_millis(tel.as_millis() as i64);
            // ① 客户端栈 poll → 上行链路
            for (s, _) in clients.iter_mut() {
                s.iface.poll(smol_now, &mut s.device, &mut s.sockets);
                let mut out = Vec::new();
                s.device.drain_tx(&mut out);
                for p in out {
                    up.send(p);
                }
            }
            // ② 上行到期 → 拦截层
            for p in up.advance(now) {
                itc.on_plain(p);
            }
            // ③ 拦截层拍 → 下行链路
            for p in itc.pump() {
                down.send(p);
            }
            // ④ 下行到期 → 各客户端（按内层目的地址分投）
            for p in down.advance(now) {
                let dst = Ipv4Addr::new(p[16], p[17], p[18], p[19]);
                if let Some((s, _)) = clients.iter_mut().find(|(s, _)| s.tunnel_ip == dst) {
                    s.inject(&p);
                }
            }
            // ⑤ 客户端读
            let mut all_done = true;
            for (i, (s, h)) in clients.iter_mut().enumerate() {
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = s
                        .sockets
                        .get_mut::<TcpSocket>(*h)
                        .recv_slice(&mut buf)
                        .unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    received[i] += n;
                }
                if received[i] < bytes_each {
                    all_done = false;
                }
            }
            if all_done {
                let _ = origin.join();
                let total = received.iter().sum();
                let tail = tail_rate_mbps(&timeline, tel.as_secs_f64(), total);
                return (tel.as_secs_f64(), total, down, tail);
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        let _ = origin.join();
        let total = received.iter().sum();
        let tail = tail_rate_mbps(&timeline, timeout.as_secs_f64(), total);
        (timeout.as_secs_f64(), total, down, tail)
    }

    // ---------- Q-B 批（F1–F10）回归面 ----------

    fn snap_of(stats: &Arc<Stats>, key: &str) -> u64 {
        stats
            .snapshot()
            .iter()
            .find(|(n, _)| *n == key)
            .map(|(_, v)| *v)
            .unwrap_or_else(|| panic!("快照键 {key} 存在"))
    }

    /// F1：`VecDequeLite::consume` 死前缀原地压缩——余量内容不变、backing 不随累计
    /// 传输量增长（P0-1：旧实现 off 只增，交错 push/部分 consume 下死前缀永不回收）。
    #[test]
    fn vecdequelite_compacts_dead_prefix() {
        let mut q = VecDequeLite::new();
        q.push(b"abcdefgh");
        q.consume(4);
        assert_eq!(q.remaining(), b"efgh", "压缩后余量内容不变");
        assert_eq!(q.off, 0, "压缩后前缀偏移归零");
        assert_eq!(q.buf.len(), 4, "死前缀已回收");
        // 部分写交错序列：消费 37 + 推 37（余量恒定），累计传输 7.4MB 后 backing 有界
        let mut q2 = VecDequeLite::new();
        for _ in 0..16 {
            q2.push(&[0u8; 64]); // 预置 ~1KB
        }
        for _ in 0..200_000 {
            let r = q2.remaining().len().min(37);
            q2.consume(r);
            q2.push(&[1u8; 37]);
        }
        assert!(!q2.remaining().is_empty());
        assert!(q2.buf.len() < 8192, "累计传输 7.4MB 后 backing 有界（得 {}）", q2.buf.len());
        assert!(
            q2.buf.capacity() < 16384,
            "capacity 不随累计传输量增长（得 {}）",
            q2.buf.capacity()
        );
    }

    /// Q-I F1：`VecDequeLite` 与 `Vec<u8>` 参照实现随机交错 push/consume 逐字节等价 +
    /// 不变量 `buf.len() < 2*remaining`（`tx_backlog` 换型后水位门/兴趣位/cc 观测读
    /// 的 `remaining` 与旧 `len` 同语义的依据）。
    #[test]
    fn vecdequelite_matches_vec_reference_and_stays_bounded() {
        let mut seed = 0x5eed_1234_5678_9abcu64;
        let mut rnd = move || {
            seed = seed
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        let mut q = VecDequeLite::new();
        let mut r: Vec<u8> = Vec::new();
        for i in 0..5000u32 {
            if rnd() % 3 == 0 {
                let n = rnd() % 97;
                let b: Vec<u8> = (0..n).map(|k| (i as u8).wrapping_add(k as u8)).collect();
                q.push(&b);
                r.extend_from_slice(&b);
            } else {
                let n = (rnd() % 130).min(r.len());
                q.consume(n);
                r.drain(..n);
            }
            assert_eq!(q.remaining(), &r[..], "第 {i} 步内容与参照实现一致");
            assert_eq!(q.is_empty(), r.is_empty(), "第 {i} 步判空一致");
            if !r.is_empty() {
                assert!(
                    q.buf.len() < 2 * r.len(),
                    "不变量 len<2*remaining（第 {i} 步：len={} remaining={}）",
                    q.buf.len(),
                    r.len()
                );
            }
        }
    }

    /// Q-I F1：水门槛/停读门按 `remaining` 计——灌到 `WATERMARK` 时 `interests_for`
    /// 摘 POLLIN、`read_upstream` 一行不读（backlog 不再增长）；消费回门下后读取恢复。
    #[test]
    fn tx_backlog_watermark_gate_uses_remaining() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        // 纯函数边界（read_upstream 的门与 interests_for 同式）
        let devnull: OwnedFd = std::fs::File::open("/dev/null").unwrap().into();
        let io = ReactorIo {
            fd: devnull,
            udp: false,
            out_tcp: VecDequeLite::new(),
            out_udp: VecDeque::new(),
            conn: None,
            dial_deadline: Instant::now(),
            dead: false,
        };
        assert!(
            interests_for(&io, WATERMARK - 1).unwrap() & libc::POLLIN != 0,
            "门下：POLLIN 在"
        );
        assert!(
            interests_for(&io, WATERMARK).unwrap() & libc::POLLIN == 0,
            "达水门：摘 POLLIN"
        );

        // 门实测：socketpair 充 upstream fd；backlog = WATERMARK（死前缀非零——门必须
        // 按 remaining 而非 backing 长度判）
        let (up, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
        up.set_nonblocking(true).unwrap();
        let fd: OwnedFd = up.into();
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        let flow = 1u64;
        let mut tb = VecDequeLite::new();
        tb.push(&vec![0u8; WATERMARK]);
        tb.consume(1); // 死前缀 off=1（1*2 < len——不触发压缩）
        tb.push(&[0u8]); // remaining 回到 WATERMARK，backing = WATERMARK+1
        assert_eq!(tb.remaining().len(), WATERMARK);
        assert_eq!(tb.off, 1, "死前缀非零（门必须按 remaining 而非 backing 长度判）");
        itc.flows.insert(
            flow,
            Flow {
                kind: Kind::Transit,
                proto: Proto::Tcp,
                client: (Ipv4Addr::new(10, 0, 0, 2), 40000),
                orig_dst: (Ipv4Addr::new(93, 184, 216, 34), 80),
                rw_port: 50000,
                sock: None,
                phase: Phase::Established,
                last_active: Instant::now(),
                io: Some(ReactorIo {
                    fd,
                    udp: false,
                    out_tcp: VecDequeLite::new(),
                    out_udp: VecDeque::new(),
                    conn: None,
                    dial_deadline: Instant::now(),
                    dead: false,
                }),
                tx_backlog: tb,
                fin_pending: false,
                udp_replied: false,
                udp_seq_of: 0,
                counted: true,
                syn_seq: 0,
                dns_rx: Vec::new(),
                obs: TcpObs::default(),
            },
        );
        peer.write_all(&[0xABu8; 4096]).unwrap();
        itc.read_upstream(flow);
        assert_eq!(
            itc.flows[&flow].tx_backlog.remaining().len(),
            WATERMARK,
            "满水门：一行不读（backlog 不增长）"
        );
        // 消费回门下 → 恢复读取
        itc.flows.get_mut(&flow).unwrap().tx_backlog.consume(WATERMARK);
        itc.read_upstream(flow);
        assert_eq!(
            itc.flows[&flow].tx_backlog.remaining().len(),
            4096,
            "门下降后读入 4096B"
        );
    }

    /// F4：`filter_defer` 非 TCP 超上限丢新+计数；TCP 恒不丢；短包（取不到 proto）放行不计数。
    #[test]
    fn filter_defer_drops_non_tcp_over_cap() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        // 非 TCP（proto=17）64KB 包 × 80 = 5MB > 上限 4MB
        let mut p = vec![0u8; 64 * 1024];
        p[0] = 0x45;
        p[9] = 17;
        let kept = itc.filter_defer((0..80).map(|_| p.clone()).collect());
        let kept_bytes: usize = kept.iter().map(|p| p.len()).sum();
        assert!(kept.len() < 80, "超上限应丢新（留 {} 包）", kept.len());
        assert!(kept_bytes <= TX_DEFER_MAX_BYTES, "滞留字节不超上限");
        assert_eq!(snap_of(&stats, "shapeDrop"), (80 - kept.len()) as u64, "丢新计数");
        // TCP（proto=6）恒不丢
        let mut t = vec![0u8; 64 * 1024];
        t[0] = 0x45;
        t[9] = 6;
        let kept_tcp = itc.filter_defer((0..80).map(|_| t.clone()).collect());
        assert_eq!(kept_tcp.len(), 80, "TCP 不丢");
        assert_eq!(snap_of(&stats, "shapeDrop"), (80 - kept.len()) as u64, "TCP 不增计数");
        // 短包（<10B，pkt.get(9) 取不到）放行不计数（不 panic）
        let kept_short = itc.filter_defer(vec![vec![0u8; 8]; 100]);
        assert_eq!(kept_short.len(), 100, "短包放行");
        assert_eq!(snap_of(&stats, "shapeDrop"), (80 - kept.len()) as u64, "短包不计数");
    }

    /// Q-K T18/T20：分片**不建会话**（F7 性质保持）+ DF-only 不误判。
    /// 分片现在进重组器（不丢、不计数），但绝不落 `by_five`/`flows`。
    #[test]
    fn fragments_never_touch_flow_table() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let mk = || nat::build_udp(Ipv4Addr::new(100, 64, 10, 1), 50000, tunnel, 47999, &[0u8; 8]);
        // MF 首片（body 16B = 8 的倍数 ⇒ 合法非末片）⇒ 进重组器（不建会话、不算丢弃）
        let mut p = mk();
        p[6] = 0x20;
        itc.on_plain(p);
        assert_eq!(itc.flow_count(), 0, "分片不建会话");
        assert_eq!(snap_of(&stats, "fragDrop"), 0, "未完成的片不是丢弃");
        assert_eq!(itc.reasm.len(), 1, "首片进重组器");
        // 非首片（同键）⇒ 仍不建会话
        let mut p2 = mk();
        p2[6] = 0x20;
        p2[7] = 0x02; // off = 16 字节（同键的次片）
        itc.on_plain(p2);
        assert_eq!(itc.flow_count(), 0, "非首片不建会话");
        assert_eq!(itc.reasm.len(), 1, "同键进同一上下文");
        // DF-only 不误判（照常处理——豁免到回环端口，不出网）
        let mut p3 = mk();
        p3[6] = 0x40;
        itc.on_plain(p3);
        assert_eq!(snap_of(&stats, "fragDrop"), 0, "DF-only 不计数");
        assert_eq!(itc.reasm.len(), 1, "DF-only 不进重组器");
        // 非末片但片长非 8 倍数（9 字节 body、MF=1）⇒ 非法 ⇒ 丢弃该片并清上下文
        let mut p4 = nat::build_udp(Ipv4Addr::new(100, 64, 10, 1), 50000, tunnel, 47999, &[0u8; 1]);
        p4[6] = 0x20; // MF=1 且片载荷 9 字节（非 8 倍）——RFC 791 §3.1 违规
        itc.on_plain(p4);
        assert_eq!(itc.reasm.len(), 0, "非法片清掉整条上下文");
        assert_eq!(snap_of(&stats, "fragBad"), 3, "非法片连带已收 2 片（1 + 2）");
        assert_eq!(snap_of(&stats, "fragDrop"), 3, "恒等式：fragDrop = fragBad");
    }

    /// F9：非 TCP/UDP 协议（ICMP 等）不再当 UDP 建端口 0 会话——丢弃、不产 dialfail 噪声。
    #[test]
    fn non_tcp_udp_proto_not_sessionized() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        // 最小 ICMP 回显请求（IPv4 20B + ICMP 8B）
        let mut p = vec![0u8; 28];
        p[0] = 0x45;
        p[2] = 0;
        p[3] = 28;
        p[9] = 1; // ICMP
        p[12..16].copy_from_slice(&Ipv4Addr::new(100, 64, 10, 1).octets());
        p[16..20].copy_from_slice(&tunnel.octets());
        p[20] = 8; // echo request
        itc.on_plain(p);
        assert_eq!(itc.flow_count(), 0, "ICMP 不建会话");
        assert_eq!(snap_of(&stats, "dialfail"), 0, "ICMP 不产 dialfail 噪声");
    }

    /// F5-2：`close()` 期在途 Dialing（未 incr）的流退役不得使 flows gauge 下溢回绕。
    #[test]
    fn close_does_not_underflow_flow_gauge() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let v = View5 {
            src: Ipv4Addr::new(100, 64, 10, 1),
            src_port: 40000,
            dst: Ipv4Addr::new(1, 2, 3, 4),
            dst_port: 80,
            proto: 6,
            tcp_flags: nat::TCP_SYN,
            tcp_seq: 100,
            tcp_ack: 0,
            udp_payload: (40, 40),
        };
        let _flow = itc.alloc_flow(&v, Kind::Transit, Proto::Tcp, vec![]);
        assert_eq!(snap_of(&stats, "flows"), 0, "Dialing 未计数");
        itc.close();
        assert_eq!(snap_of(&stats, "flows"), 0, "close 未 incr 的 Dialing 流不得下溢");
        assert_eq!(itc.flow_count(), 0);
    }

    /// F5-1：E12 关闭行打**本会话号**（此前恒 #0 是缺陷）——建立行与关闭行同号、非 0。
    #[test]
    fn udp_close_line_reuses_establish_seq() {
        let echo = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = echo.recv_from(&mut buf) {
                if echo.send_to(&buf[..n], from).is_err() {
                    break;
                }
            }
        });
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let logs: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let l2 = Arc::clone(&logs);
        let mut cfg = cfg_base(tunnel);
        cfg.logf = Arc::new(move |s: &str| l2.lock().unwrap().push(s.to_string()));
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        let q = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 3),
            50000,
            Ipv4Addr::LOCALHOST,
            port,
            b"q",
        );
        itc.on_plain(q);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            for _ in itc.pump() {}
            if itc.flows.values().any(|f| f.udp_replied) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        // 手动收尾触发 E12 关闭行
        let flow = *itc.flows.keys().next().expect("会话在册");
        itc.finish_udp(flow);
        let seq_of = |needle: &str| -> String {
            logs.lock()
                .unwrap()
                .iter()
                .find(|l| l.contains("会话 #") && l.contains(needle))
                .and_then(|l| l.split("会话 #").nth(1))
                .and_then(|s| s.split_whitespace().next())
                .map(|s| s.to_string())
                .unwrap_or_else(|| panic!("未找到含 {needle} 的会话行"))
        };
        let est = seq_of("建立");
        let clo = seq_of("关闭");
        assert_eq!(est, clo, "E12 建立行与关闭行同号");
        assert_ne!(est, "0", "会话号非 0（修复前恒 #0）");
        drop(echo_thread);
    }

    /// F3：`udp_out_full` 上限判定（条数/字节）；`udp_send_to_client` 栈 tx 满时计 udp_drop。
    #[test]
    fn udp_out_cap_and_send_failure_counted() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let v = View5 {
            src: Ipv4Addr::new(100, 64, 10, 1),
            src_port: 40000,
            dst: Ipv4Addr::new(8, 8, 8, 8),
            dst_port: 9999,
            proto: 17,
            tcp_flags: 0,
            tcp_seq: 0,
            tcp_ack: 0,
            udp_payload: (28, 28),
        };
        let flow = itc.alloc_flow(&v, Kind::Transit, Proto::Udp, vec![]);
        assert!(!itc.udp_out_full(flow, 1000), "无 io = 不满");
        // 造 io + 填满条数
        let s = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        itc.place_io(flow, OwnedFd::from(s), true, None, Instant::now());
        if let Some(io) = itc.flows.get_mut(&flow).and_then(|f| f.io.as_mut()) {
            for _ in 0..MAX_OUT_UDP_PKTS {
                io.out_udp.push_back(vec![0u8; 10]);
            }
        }
        assert!(itc.udp_out_full(flow, 10), "条数达上限 = 满");
        if let Some(io) = itc.flows.get_mut(&flow).and_then(|f| f.io.as_mut()) {
            io.out_udp.clear();
            io.out_udp.push_back(vec![0u8; WATERMARK]);
        }
        assert!(itc.udp_out_full(flow, 1), "字节达上限 = 满");
        // udp_send_to_client：栈 tx（64KB）写满后失败 → udp_drop 计数
        itc.ensure_udp_socket(flow);
        let before = snap_of(&stats, "udpDrop");
        for _ in 0..5 {
            itc.udp_send_to_client(flow, &vec![0u8; 60_000]);
        }
        assert!(snap_of(&stats, "udpDrop") > before, "栈 tx 满 → udp_drop 计数");
    }

    // ---------- DNS 面回归（F2/F6/F8/A1） ----------

    /// 起一个 fake 上游 + DnsProxy + 挂 DNS 面的拦截器。`respond` 决定上游应答字节。
    fn dns_itc_with_upstream(
        respond: impl Fn(&[u8]) -> Vec<u8> + Send + 'static,
        max_in_flight: usize,
    ) -> (Interceptor, Arc<Stats>, std::path::PathBuf) {
        use crate::server::dnsproxy::{DnsConfig, DnsProxy};
        let up = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        // 大应答（>16KB）测试：macOS 默认 SO_SNDBUF 较小会把大 UDP 数据报丢掉——
        // 显式放大（同机自测面，不涉产品路径）。
        let bufsz: libc::c_int = 1 << 20;
        unsafe {
            libc::setsockopt(
                up.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                &bufsz as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
        }
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 65536];
            loop {
                let Ok((n, from)) = up.recv_from(&mut buf) else {
                    return;
                };
                let r = respond(&buf[..n]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!(
            "homeway-rs-qb-dns-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();
        let (proxy, events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                budget: Duration::from_secs(5),
                max_in_flight,
                ..Default::default()
            },
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        );
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let cfg = Config {
            tunnel_ip: tunnel,
            local_services: HashMap::new(),
            dns: Some(Arc::clone(&proxy)),
            dns_events: Some(events),
            dns_resolve_port: 5300,
            tx_shape: None,
            logf: noop_logf(),
        };
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        itc.attach_dns();
        (itc, stats, dir)
    }

    /// A 查询（id=0x5566，a.example）的 RFC1035 帧。
    fn dns_a_query(id: u16) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&id.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in ["a", "example"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        q
    }

    /// 小 A 应答（回显 question + 一条 A 记录）。
    fn small_a_response(q: &[u8]) -> Vec<u8> {
        let mut r = Vec::new();
        r.extend_from_slice(&q[..2]);
        r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]);
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&[0, 0, 0, 0]);
        r.extend_from_slice(&q[12..]);
        r.extend_from_slice(&[0xC0, 0x0C]);
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&1u16.to_be_bytes());
        r.extend_from_slice(&60u32.to_be_bytes());
        r.extend_from_slice(&4u16.to_be_bytes());
        r.extend_from_slice(&[127, 0, 0, 1]);
        r
    }

    /// F2：DNS 提交在途超限（Dropped）时回收 route tag——`pending` 不增长。
    #[test]
    fn dns_dropped_submit_recycles_tag() {
        // 上游不回（worker 阻塞到 budget）——max_in_flight=1 ⇒ 第二条必 Dropped
        let (mut itc, _stats, dir) = dns_itc_with_upstream(|_q| Vec::new(), 1);
        // 注意：respond 返回空 ⇒ exchange 收不到有效应答；worker 阻塞到 budget（5s）
        let q = dns_a_query(0x7788);
        let p1 = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 9),
            52100,
            Ipv4Addr::new(100, 64, 255, 1),
            53,
            &q,
        );
        let p2 = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 9),
            52101,
            Ipv4Addr::new(100, 64, 255, 1),
            53,
            &q,
        );
        itc.on_plain(p1);
        itc.on_plain(p2);
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut plen = 0;
        while Instant::now() < deadline {
            for _ in itc.pump() {}
            plen = itc.dns_faces.as_ref().map(|f| f.pending_len()).unwrap_or(0);
            if plen >= 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(plen, 1, "两条查询：一条在途、一条丢弃回收（pending 不增长到 2）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A1（[门-A1]）：DNS TCP 腿读到空帧（mlen==0）+ 同段 FIN → **不 panic**、腿被拆、
    /// pending 不增长（收线在 `for flow` 循环外统一做）。
    #[test]
    fn dns_tcp_leg_empty_frame_no_panic() {
        let (mut itc, _stats, dir) = dns_itc_with_upstream(small_a_response, 256);
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 7),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let dst = std::net::SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53);
        let h: SocketHandle = client.connect(dst).unwrap();
        let mut tick = 0i64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            if client.sockets.get::<TcpSocket>(h).state() == tcp::State::Established {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            client.sockets.get::<TcpSocket>(h).state(),
            tcp::State::Established,
            "TCP DNS 腿建立"
        );
        // 空帧（2B 长度 = 0）+ 立即 FIN——同段到达
        client.sockets.get_mut::<TcpSocket>(h).send_slice(&[0x00, 0x00]).unwrap();
        client.sockets.get_mut::<TcpSocket>(h).close();
        // 不 panic 即过（旧实现在同迭代内 teardown 后 get_mut(快照 h) 会 panic）
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            if itc.flow_count() == 0 {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(itc.flow_count(), 0, "空帧腿已拆");
        assert_eq!(
            itc.dns_faces.as_ref().map(|f| f.pending_len()).unwrap_or(0),
            0,
            "空帧不进 submit ⇒ pending 不增长"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F8：隧道 IP:53 TCP 面大应答（>16KB tx buffer）→ per-conn 待写队列分多次续写、
    /// 最终完整送达（帧无错位）。
    #[test]
    fn dns_tcp_face_large_response() {
        // 上游回 ~20KB 大应答（TXT 记录，rdlen 撑大）
        let big = |q: &[u8]| -> Vec<u8> {
            let rdata = 20_000usize;
            let mut r = Vec::with_capacity(64 + rdata);
            r.extend_from_slice(&q[..2]);
            r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]);
            r.extend_from_slice(&1u16.to_be_bytes());
            r.extend_from_slice(&[0, 0, 0, 0]);
            r.extend_from_slice(&q[12..]);
            r.extend_from_slice(&[0xC0, 0x0C]); // name ptr
            r.extend_from_slice(&16u16.to_be_bytes()); // TYPE TXT
            r.extend_from_slice(&1u16.to_be_bytes()); // CLASS IN
            r.extend_from_slice(&60u32.to_be_bytes()); // TTL
            r.extend_from_slice(&(rdata as u16).to_be_bytes()); // RDLENGTH
            r.extend(std::iter::repeat_n(0x41u8, rdata)); // RDATA
            r
        };
        let (mut itc, _stats, dir) = dns_itc_with_upstream(big, 256);
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 8),
            tunnel,
            SmolInstant::from_millis(0),
        );
        // 隧道 IP:53（demux 投栈内 listener）
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, 53))
            .unwrap();
        let mut tick = 0i64;
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            if client.sockets.get::<TcpSocket>(h).state() == tcp::State::Established {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            client.sockets.get::<TcpSocket>(h).state(),
            tcp::State::Established,
            "隧道 IP:53 TCP 面建立"
        );
        // 发一条 A 查询（RFC1035 帧）
        let q = dns_a_query(0x9911);
        let mut frame = Vec::new();
        frame.extend_from_slice(&(q.len() as u16).to_be_bytes());
        frame.extend_from_slice(&q);
        client.sockets.get_mut::<TcpSocket>(h).send_slice(&frame).unwrap();
        // 收应答：完整帧（2 + mlen）
        let mut got: Vec<u8> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            let mut buf = [0u8; 16384];
            let n = client
                .sockets
                .get_mut::<TcpSocket>(h)
                .recv_slice(&mut buf)
                .unwrap_or(0);
            if n > 0 {
                got.extend_from_slice(&buf[..n]);
                if got.len() >= 2 {
                    let mlen = u16::from_be_bytes([got[0], got[1]]) as usize;
                    if got.len() >= 2 + mlen {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(got.len() >= 2, "应答帧头到达");
        let mlen = u16::from_be_bytes([got[0], got[1]]) as usize;
        assert_eq!(got.len(), 2 + mlen, "应答恰一帧（无错位）");
        assert!(mlen > 16 * 1024, "大应答（{mlen}B）分多次续写后完整送达");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F6：DNS 腿（非隧道 IP :53）会话内**每包**应答（固定源端口）——审计 P1「手机
    /// stub resolver 固定源端口重传全黑洞」的回归钉。首包走 `udp_ready` 重放，后续包
    /// 走 `service_sockets` 的 F6 分支。
    #[test]
    fn dns_udp_leg_answers_every_packet() {
        let (mut itc, _stats, dir) = dns_itc_with_upstream(small_a_response, 256);
        let client = Ipv4Addr::new(100, 64, 10, 9);
        let dst = Ipv4Addr::new(8, 8, 8, 8);
        let src_port = 53000; // 固定源端口（同一会话）
        // 首包（建立 + 重放应答）+ 会话内后续包（同五元组 → F6 分支）
        itc.on_plain(nat::build_udp(client, src_port, dst, 53, &dns_a_query(0x1001)));
        itc.on_plain(nat::build_udp(client, src_port, dst, 53, &dns_a_query(0x1002)));
        let mut ids = std::collections::HashSet::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && ids.len() < 2 {
            for p in itc.pump() {
                if let Some(v) = Ipv4View::parse(&p) {
                    if v.proto == 17 && v.dst == client && v.src == dst && v.payload.len() >= 2 {
                        ids.insert(u16::from_be_bytes([v.payload[0], v.payload[1]]));
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(
            ids.contains(&0x1001) && ids.contains(&0x1002),
            "会话内每包均获应答（得 {ids:?}）"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Q-B F7（[门-B6] 计划②）性质保持（Q-K 复审）：**已有流存在**时喂同五元组的 MF
    /// 首片 → 仍不得注入既有流（进重组器、不碰 flow 表）。
    #[test]
    fn fragment_never_injected_into_existing_flow() {
        let echo = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = echo.recv_from(&mut buf) {
                if echo.send_to(&buf[..n], from).is_err() {
                    break;
                }
            }
        });
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let client = Ipv4Addr::new(100, 64, 10, 3);
        let (src_port, dst) = (50000u16, Ipv4Addr::LOCALHOST);
        // 建流（正常包）
        itc.on_plain(nat::build_udp(client, src_port, dst, port, b"q"));
        let flows_before = itc.flow_count();
        assert!(flows_before >= 1, "正常包建会话");
        let frag_before = snap_of(&stats, "fragDrop");
        // 同五元组的 MF 首片 → 进重组器（不注入既有流、不建新流、不计数）
        let mut frag = nat::build_udp(client, src_port, dst, port, &[0u8; 8]);
        frag[6] = 0x20;
        itc.on_plain(frag);
        assert_eq!(snap_of(&stats, "fragDrop"), frag_before, "入重组器不计数");
        assert_eq!(itc.flow_count(), flows_before, "既有流不受影响（无新会话/无注入）");
        assert_eq!(itc.reasm.len(), 1, "进重组器");
        drop(echo_thread);
    }

    // ---------- Q-K 批（F1 重组 / F2 计数 / F4 ICMP / F5-d TX 分片）回归面 ----------

    /// 把一整包手工切成两片（首片 `first_body` 字节、MF=1；末片 MF=0）。
    /// `first_body` 必须是 8 的倍数且小于 body 长（RFC 791 §3.1）。
    fn split_in_two(full: &[u8], first_body: usize) -> (Vec<u8>, Vec<u8>) {
        let ihl = (full[0] & 0x0f) as usize * 4;
        let ident = u16::from_be_bytes([full[4], full[5]]);
        let total = u16::from_be_bytes([full[2], full[3]]) as usize;
        let body = &full[ihl..total];
        let mk = |off: usize, data: &[u8], mf: bool| -> Vec<u8> {
            let mut p = full[..ihl].to_vec();
            p[4..6].copy_from_slice(&ident.to_be_bytes());
            p[2..4].copy_from_slice(&((ihl + data.len()) as u16).to_be_bytes());
            let flags = if mf { 0x2000u16 } else { 0 } | ((off / 8) as u16);
            p[6..8].copy_from_slice(&flags.to_be_bytes());
            p.extend_from_slice(data);
            nat::fix_ip_checksum(&mut p);
            p
        };
        (
            mk(0, &body[..first_body], true),
            mk(first_body, &body[first_body..], false),
        )
    }

    /// 16 位字折叠和（校验和自洽断言：整段折叠 == 0xFFFF）。
    fn sum16(data: &[u8]) -> u32 {
        let mut sum = 0u32;
        let mut i = 0;
        while i + 1 < data.len() {
            sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
            i += 2;
        }
        if i < data.len() {
            sum += (data[i] as u32) << 8;
        }
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        sum
    }

    /// 客户端栈上的 UDP socket（测试面）。
    fn client_udp_socket(client: &mut StackB, port: u16) -> SocketHandle {
        use smoltcp::socket::udp;
        let sock = udp::Socket::new(
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; 64 * 1024]),
            udp::PacketBuffer::new(vec![udp::PacketMetadata::EMPTY; 8], vec![0u8; 64 * 1024]),
        );
        let h = client.sockets.add(sock);
        client
            .sockets
            .get_mut::<udp::Socket>(h)
            .bind(IpEndpoint::new(client.tunnel_ip.into(), port))
            .unwrap();
        h
    }

    /// 造一条 transit UDP 会话（TX 侧测试用）：rw_port 由分配器给（首个 = 20000）。
    fn make_udp_flow(
        itc: &mut Interceptor,
        client: Ipv4Addr,
        client_port: u16,
        orig: (Ipv4Addr, u16),
    ) -> u64 {
        let v = View5 {
            src: client,
            src_port: client_port,
            dst: orig.0,
            dst_port: orig.1,
            proto: 17,
            tcp_flags: 0,
            tcp_seq: 0,
            tcp_ack: 0,
            udp_payload: (28, 28),
        };
        let flow = itc.alloc_flow(&v, Kind::Transit, Proto::Udp, vec![]);
        itc.ensure_udp_socket(flow);
        flow
    }

    /// 泵到某报文的分片全部出完（smoltcp 的 `Fragmenter` 单缓冲：剩余片在后续 poll
    /// 续发），返回沿途产出的全部分片包。
    fn pump_all_fragments(itc: &mut Interceptor) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::new();
        for _ in 0..64 {
            for p in itc.pump() {
                if nat::Ipv4FragHdr::parse(&p).is_some_and(|f| f.hdr.is_fragment()) {
                    out.push(p);
                }
            }
            if out
                .iter()
                .any(|p| nat::Ipv4FragHdr::parse(p).is_some_and(|f| !f.hdr.mf))
            {
                break;
            }
        }
        out
    }

    /// T1/T19/T24/T37（端到端）：客户端 1300B → 真分片 → 出口**重组** → 过境拨号（原目的
    /// 端口）→ 回环 echo 原样回显 → 出口 TX 真分片（F5-a）+ 反重写（F5-d）→ 客户端重组收全。
    #[test]
    fn large_udp_roundtrip_both_directions() {
        let echo = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let (tx_seen, rx_seen) = std::sync::mpsc::channel::<usize>();
        let echo_thread = std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            while let Ok((n, from)) = echo.recv_from(&mut buf) {
                let _ = tx_seen.send(n);
                if echo.send_to(&buf[..n], from).is_err() {
                    break;
                }
            }
        });
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 61),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let h = client_udp_socket(&mut client, 46000);
        let payload: Vec<u8> = (0..1300u32).map(|i| (i % 251) as u8).collect();
        client
            .sockets
            .get_mut::<smoltcp::socket::udp::Socket>(h)
            .send_slice(
                &payload,
                IpEndpoint::new(Ipv4Addr::LOCALHOST.into(), port),
            )
            .unwrap();
        let mut tick = 0i64;
        let mut max_client_frags = 0usize;
        let mut tx_frag_pkts = 0usize;
        let mut got: Option<Vec<u8>> = None;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            tick += 5;
            let t = SmolInstant::from_millis(tick);
            client.iface.poll(t, &mut client.device, &mut client.sockets);
            let mut out = Vec::new();
            client.device.drain_tx(&mut out);
            max_client_frags = max_client_frags.max(out.len());
            for p in out {
                itc.on_plain(p);
            }
            for p in itc.pump() {
                if nat::Ipv4FragHdr::parse(&p).is_some_and(|f| f.hdr.is_fragment()) {
                    tx_frag_pkts += 1;
                }
                client.inject(&p);
            }
            let mut buf = vec![0u8; 65536];
            let n = client
                .sockets
                .get_mut::<smoltcp::socket::udp::Socket>(h)
                .recv_slice(&mut buf)
                .map(|(n, _)| n)
                .unwrap_or(0);
            if n > 0 {
                got = Some(buf[..n].to_vec());
            }
            if got.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(got.as_deref(), Some(&payload[..]), "1300B 大 UDP 双向逐字节往返");
        assert_eq!(
            rx_seen.recv_timeout(Duration::from_secs(2)).unwrap_or(0),
            1300,
            "上游收到原长度（过境拨号到原目的端口 = rewrite_dst 生效）"
        );
        assert!(
            max_client_frags >= 2,
            "客户端 >1252 载荷真分片（R4 回归：得 {max_client_frags}）"
        );
        assert!(tx_frag_pkts >= 2, "出口 TX 大回复真分片（F5-a；得 {tx_frag_pkts}）");
        assert_eq!(snap_of(&stats, "fragReasm"), 1, "出口 RX 重组成功交付 1 报文");
        assert_eq!(snap_of(&stats, "fragDrop"), 0, "无分片丢弃");
        assert_eq!(snap_of(&stats, "txFragDrop"), 0, "TX 无丢片（表命中）");
        drop(echo_thread);
    }

    /// T21：TCP 分片（SYN 切成两片）⇒ 重组后照常建流（协议无关重组，对齐 Go）。
    #[test]
    fn reasm_tcp_fragments_deliver() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let acc2 = Arc::clone(&accepted);
        let t = std::thread::spawn(move || {
            if let Ok((mut c, _)) = listener.accept() {
                acc2.store(true, Ordering::SeqCst);
                let _ = c.write_all(b"ok");
            }
        });
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let syn = nat::build_tcp_syn(
            Ipv4Addr::new(100, 64, 10, 62),
            40000,
            Ipv4Addr::LOCALHOST,
            port,
            1234,
        );
        // 44B 总长（IP20 + TCP24）⇒ body 24B：首片 16B（不足 TCP 头 20B——正因如此
        // 分片判定不得依赖 L4 可解析性）、次片 8B。
        let (f0, f1) = split_in_two(&syn, 16);
        itc.on_plain(f1); // 乱序：末片先到
        itc.on_plain(f0);
        assert_eq!(snap_of(&stats, "fragReasm"), 1, "重组交付 1 报文");
        assert_eq!(itc.flow_count(), 1, "重组后建流（走 route_plain 同一路径）");
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !accepted.load(Ordering::SeqCst) {
            for _ in itc.pump() {}
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(accepted.load(Ordering::SeqCst), "上游收到拨号（transit 过境）");
        let _ = t.join();
    }

    /// T22：超时且首片在位 ⇒ `tx_out` 出 ICMP type 11 code 1（载荷 = 首片头 + 8B、
    /// src/dst 反转、双校验和自洽）。
    #[test]
    fn icmp_time_exceeded_on_timeout() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let udp = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 63),
            41000,
            Ipv4Addr::new(8, 8, 8, 8),
            4444,
            &vec![0u8; 1256],
        );
        let (f0, _f1) = split_in_two(&udp, 1256);
        let f0_ref = f0.clone();
        itc.on_plain(f0);
        assert!(itc.tx_out.is_empty(), "未超时不产 ICMP");
        itc.sweep_reasm(Instant::now() + Duration::from_secs(31));
        assert_eq!(snap_of(&stats, "fragTimeout"), 1);
        assert_eq!(snap_of(&stats, "fragDrop"), 1, "恒等式左侧同步");
        assert_eq!(snap_of(&stats, "fragBad") + snap_of(&stats, "fragLimit") + snap_of(&stats, "fragOverlap"), 0);
        let icmp = itc.tx_out.pop().expect("超时 ⇒ 产 ICMP");
        assert_eq!((icmp[20], icmp[21]), (11, 1), "type 11 code 1");
        assert_eq!(&icmp[12..16], &[8, 8, 8, 8], "src = 首片目的");
        assert_eq!(&icmp[16..20], &[100, 64, 10, 63], "dst = 首片源");
        assert_eq!(&icmp[28..48], &f0_ref[..20], "载荷 = 首片 IP 头");
        assert_eq!(&icmp[48..56], &f0_ref[20..28], "载荷 = 前 8 字节");
        assert_eq!(sum16(&icmp[..20]), 0xFFFF, "IP 校验和自洽");
        assert_eq!(sum16(&icmp[20..]), 0xFFFF, "ICMP 校验和自洽");
        assert!(itc.tx_out.is_empty());
    }

    /// T23：无首片 ⇒ 不产 ICMP（对齐 gVisor `if pkt != nil`）；T24：抑制集三态。
    #[test]
    fn icmp_not_sent_without_first_fragment_or_suppressed() {
        // ① 无首片
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let udp = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 64),
            41001,
            Ipv4Addr::new(8, 8, 8, 8),
            4444,
            &vec![0u8; 1256],
        );
        let (_f0, f1) = split_in_two(&udp, 1256);
        itc.on_plain(f1);
        itc.sweep_reasm(Instant::now() + Duration::from_secs(31));
        assert_eq!(snap_of(&stats, "fragTimeout"), 1);
        assert!(itc.tx_out.is_empty(), "无首片不发 ICMP");
        // ② 抑制集：源 0.0.0.0 / 目的组播 / 目的 255.255.255.255
        for (name, patch) in [
            ("src=0.0.0.0", 0u8),
            ("dst=224.0.0.1", 1),
            ("dst=255.255.255.255", 2),
        ] {
            let mut p = udp.clone();
            match patch {
                0 => p[12..16].copy_from_slice(&[0, 0, 0, 0]),
                1 => p[16..20].copy_from_slice(&[224, 0, 0, 1]),
                _ => p[16..20].copy_from_slice(&[255, 255, 255, 255]),
            }
            nat::fix_ip_checksum(&mut p);
            let (f0, _) = split_in_two(&p, 1256);
            let stats2 = Arc::new(Stats::default());
            let mut itc2 = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats2));
            itc2.on_plain(f0);
            itc2.sweep_reasm(Instant::now() + Duration::from_secs(31));
            assert_eq!(snap_of(&stats2, "fragTimeout"), 1, "{name}：仍计数");
            assert!(itc2.tx_out.is_empty(), "{name}：不发 ICMP（抑制集）");
        }
    }

    /// T25：内核无 proto 门——TCP 分片超时也发 11/1；薄封装 type3/code3 仍只对 UDP。
    #[test]
    fn icmp_covers_non_udp_proto() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let syn = nat::build_tcp_syn(
            Ipv4Addr::new(100, 64, 10, 65),
            40001,
            Ipv4Addr::new(93, 184, 216, 34),
            443,
            7,
        );
        let (f0, _f1) = split_in_two(&syn, 16);
        itc.on_plain(f0);
        itc.sweep_reasm(Instant::now() + Duration::from_secs(31));
        let icmp = itc.tx_out.pop().expect("TCP 分片超时也发（内核无 proto 门）");
        assert_eq!((icmp[20], icmp[21]), (11, 1));
        assert_eq!(&icmp[12..16], &[93, 184, 216, 34]);
        assert!(nat::build_icmp_unreachable(&syn).is_none(), "type3/code3 仍只对 UDP");
    }

    /// T26：`snapshot()` 长度 15、既有键索引 `[0..=8]` 不变；**恒等式**
    /// `fragDrop == fragBad + fragLimit + fragTimeout + fragOverlap`。
    #[test]
    fn stats_snapshot_shape_and_frag_identity() {
        let s = Stats::default();
        let snap = s.snapshot();
        assert_eq!(snap.len(), 15, "数组 9 → 15（追加末位）");
        assert_eq!(
            snap[..9].iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec![
                "dialok", "dialfail", "flows", "rejected", "udpReplied", "udpNoReply", "udpDrop",
                "shapeDrop", "fragDrop"
            ],
            "既有索引语义不变"
        );
        assert_eq!(
            snap[9..].iter().map(|(n, _)| *n).collect::<Vec<_>>(),
            vec!["fragReasm", "fragBad", "fragOverlap", "fragTimeout", "fragLimit", "txFragDrop"]
        );
        s.note_frag_drop(DropReason::Bad, 2);
        s.note_frag_drop(DropReason::Overlap, 3);
        s.note_frag_drop(DropReason::Timeout, 1);
        s.note_frag_drop(DropReason::Limit, 4);
        s.incr_frag_reasm();
        s.incr_tx_frag_drop();
        let snap = s.snapshot();
        let of = |k: &str| snap.iter().find(|(n, _)| *n == k).unwrap().1;
        assert_eq!(of("fragDrop"), 10);
        assert_eq!(
            of("fragDrop"),
            of("fragBad") + of("fragLimit") + of("fragTimeout") + of("fragOverlap"),
            "恒等式"
        );
        assert_eq!((of("fragReasm"), of("txFragDrop")), (1, 1));
    }

    /// T28/T29（真分片）：出口 TX 生成的真分片——IP 源全片一致 = 原目的；重组后的
    /// UDP 校验和 == **独立全量重算**路径（`rewrite_src`）的值。
    #[test]
    fn tx_frag_incremental_checksum_matches_full_recompute() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let client = Ipv4Addr::new(100, 64, 10, 71);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let orig = (Ipv4Addr::new(93, 184, 216, 34), 443);
        let flow = make_udp_flow(&mut itc, client, 47000, orig);
        let rw = itc.flows[&flow].rw_port;
        let payload: Vec<u8> = (0..3000u32).map(|i| (i % 199) as u8).collect();
        itc.udp_send_to_client(flow, &payload);
        let frs = pump_all_fragments(&mut itc);
        assert!(frs.len() >= 3, "3000B ⇒ ≥3 片（得 {}）", frs.len());
        for p in &frs {
            let f = nat::Ipv4FragHdr::parse(p).unwrap();
            assert_eq!(f.hdr.src, orig.0, "全片 IP 源一致 = 原目的（否则客户端重组键分裂）");
            assert_eq!(f.hdr.dst, client);
        }
        // 真分片重组 → 校验和
        let mut r = reasm::Reassembler::new();
        let mut full = None;
        for p in &frs {
            let f = nat::Ipv4FragHdr::parse(p).unwrap();
            if let Some(done) = r.push(f, Instant::now()).done {
                full = Some(done);
            }
        }
        let full = full.expect("真分片应能重组（含乱序/顺序两态）");
        assert_eq!(&full[28..28 + payload.len()], &payload[..], "载荷逐字节");
        // UDP 校验和自洽（伪头 + 段折叠 == 0xFFFF）
        let l4 = &full[20..];
        let pseudo = [
            full[12], full[13], full[14], full[15], full[16], full[17], full[18], full[19], 0, 17,
            (l4.len() >> 8) as u8,
            l4.len() as u8,
        ];
        let mut sum = sum16(&pseudo);
        sum += sum16(l4);
        while sum >> 16 != 0 {
            sum = (sum & 0xffff) + (sum >> 16);
        }
        assert_eq!(sum, 0xFFFF, "重组包 UDP 校验和自洽（RFC 1624 增量更新正确）");
        // 独立对照：同一载荷的未分片包走全量重算路径，校验和应逐字节相等
        let mut ref_pkt = nat::build_udp(tunnel, rw, client, 47000, &payload);
        nat::rewrite_src(&mut ref_pkt, orig.0, orig.1);
        assert_eq!(&full[26..28], &ref_pkt[26..28], "增量 == 全量重算");
        assert_eq!(sum16(&ref_pkt[..20]), 0xFFFF);
        assert_eq!(snap_of(&stats, "txFragDrop"), 0, "表命中：零丢片");
        assert!(itc.tx_frag.is_empty(), "末片精确回收");
    }

    /// T30/T31：非首片**只改 IP 源 + IP 校验和**（载荷逐字节不变、L4 校验和字段不动），
    /// 且载荷前 2 字节即使**恰好等于在册 rw_port** 也不得触发误改写（走表路径）。
    #[test]
    fn tx_non_first_fragment_touches_only_ip_source() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let client = Ipv4Addr::new(100, 64, 10, 72);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let orig = (Ipv4Addr::new(1, 2, 3, 4), 999);
        let flow = make_udp_flow(&mut itc, client, 47001, orig);
        let rw = itc.flows[&flow].rw_port;
        assert_eq!(rw, 20000, "首个 rw_port（分配器起点）");
        // 非首片（末片）：off = 8、16 字节载荷，**前 2 字节 = 在册 rw_port**（若走
        // by_rw_port 查表就会误改写成另一个流的原始目的 + 改写载荷——R15-2 的形态）
        let mut frag = nat::build_udp(tunnel, rw, client, 47001, &[0u8; 16]);
        frag[6..8].copy_from_slice(&1u16.to_be_bytes()); // off = 8 字节、mf = 0（末片）
        frag[20..22].copy_from_slice(&rw.to_be_bytes()); // 载荷前 2 字节 = rw_port（垃圾）
        nat::fix_ip_checksum(&mut frag);
        // 登记表项（模拟首片已过 on_tx，值 = 本报文的原始目的）
        itc.tx_frag
            .insert((client, 0x4321, 17), TxFragVal { orig: Some(orig), created: Instant::now() });
        let mut frag2 = frag.clone();
        frag2[4..6].copy_from_slice(&0x4321u16.to_be_bytes()); // 键 ident 对齐
        let before = frag2.clone();
        itc.on_tx(frag2);
        let out = itc.tx_out.pop().expect("应发出（表命中）");
        assert_eq!(&out[12..16], &orig.0.octets(), "IP 源 = 表内原始目的");
        for i in 0..out.len() {
            if (10..16).contains(&i) {
                continue; // 只允许 IP 校验和 + IP 源变
            }
            assert_eq!(out[i], before[i], "字节 {i} 不得变（L4 头/校验和/载荷）");
        }
        assert_eq!(&out[20..22], &rw.to_be_bytes(), "载荷前 2 字节不被误改写（垃圾端口不命中）");
        assert_eq!(sum16(&out[..20]), 0xFFFF, "IP 校验和自洽");
        assert!(itc.tx_frag.is_empty(), "末片（mf=0）⇒ 精确回收");
        assert_eq!(snap_of(&stats, "txFragDrop"), 0);
    }

    /// T32：TX 分片表未命中 ⇒ 丢片 + `txFragDrop`（不发坏片）。
    #[test]
    fn tx_frag_missing_entry_drops_and_counts() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let mut frag = nat::build_udp(
            tunnel,
            20000,
            Ipv4Addr::new(100, 64, 10, 73),
            47002,
            &[0u8; 16],
        );
        frag[6..8].copy_from_slice(&(0x2000u16 | 1).to_be_bytes()); // 非首片
        nat::fix_ip_checksum(&mut frag);
        itc.on_tx(frag);
        assert!(itc.tx_out.is_empty(), "表未命中 ⇒ 丢片（不发坏片）");
        assert_eq!(snap_of(&stats, "txFragDrop"), 1);
        assert_eq!(snap_of(&stats, "fragDrop"), 0, "RX 面不受影响");
    }

    /// T33：TX 分片表的 TTL 与上限兜底（淘汰最老）。
    #[test]
    fn tx_frag_table_ttl_and_cap() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        let client = Ipv4Addr::new(100, 64, 10, 74);
        let now = Instant::now();
        // TTL：11s 前的条目被清
        itc.tx_frag.insert(
            (client, 1, 17),
            TxFragVal { orig: None, created: now - Duration::from_secs(11) },
        );
        itc.tx_frag
            .insert((client, 2, 17), TxFragVal { orig: None, created: now });
        itc.sweep_tx_frag(now);
        assert_eq!(itc.tx_frag.len(), 1, "TTL 清最老");
        assert!(itc.tx_frag.contains_key(&(client, 2, 17)));
        // 上限：超 TX_FRAG_MAX ⇒ 淘汰最老
        for i in 0..(TX_FRAG_MAX as u16 + 10) {
            itc.tx_frag.insert(
                (client, 100 + i, 17),
                TxFragVal { orig: None, created: now + Duration::from_millis(i as u64) },
            );
        }
        itc.sweep_tx_frag(now);
        assert_eq!(itc.tx_frag.len(), TX_FRAG_MAX, "上限兜底");
        assert!(!itc.tx_frag.contains_key(&(client, 100, 17)), "淘汰最老");
        assert!(itc.tx_frag.contains_key(&(client, 100 + TX_FRAG_MAX as u16 + 9, 17)));
    }

    /// T33 补：**插入路径**的上限（评审低-2——表满且条目全新鲜时，首片登记必须为
    /// 本次 insert 留位，修前会瞬时到 上限+1）。
    #[test]
    fn tx_frag_table_cap_on_insert_path() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let client = Ipv4Addr::new(100, 64, 10, 78);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        let now = Instant::now();
        for i in 0..TX_FRAG_MAX as u16 {
            itc.tx_frag.insert((client, 100 + i, 17), TxFragVal { orig: None, created: now });
        }
        assert_eq!(itc.tx_frag.len(), TX_FRAG_MAX);
        // 首片（MF=1；src 非隧道 IP ⇒ 未命中流表、登记 orig=None）
        let mut frag = nat::build_udp(Ipv4Addr::new(1, 1, 1, 1), 5000, client, 47005, &[0u8; 8]);
        frag[6] = 0x20;
        nat::fix_ip_checksum(&mut frag);
        itc.on_tx(frag);
        assert_eq!(itc.tx_frag.len(), TX_FRAG_MAX, "insert 后仍 ≤ 上限");
        assert!(itc.tx_frag.contains_key(&(client, 0, 17)), "新键已登记（不拒新）");
    }

    /// T34：非分片路径与修复前**逐字节同行为**（反重写仍全量重算）；畸形包原样放行。
    #[test]
    fn tx_non_fragment_path_unchanged() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let client = Ipv4Addr::new(100, 64, 10, 75);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let orig = (Ipv4Addr::new(8, 8, 4, 4), 53);
        let flow = make_udp_flow(&mut itc, client, 47003, orig);
        let rw = itc.flows[&flow].rw_port;
        // 非分片（DF-only 也不算分片）：全量反重写
        let mut pkt = nat::build_udp(tunnel, rw, client, 47003, b"reply");
        pkt[6] = 0x40; // DF-only
        nat::fix_ip_checksum(&mut pkt);
        itc.on_tx(pkt);
        let out = itc.tx_out.pop().unwrap();
        let v = Ipv4View::parse(&out).unwrap();
        assert_eq!((v.src, v.src_port), orig, "全量反重写（src → orig_dst）");
        assert_eq!(v.payload, b"reply");
        assert_eq!(sum16(&out[..20]), 0xFFFF);
        // 畸形（parse 失败）原样放行
        itc.on_tx(vec![1, 2, 3]);
        assert_eq!(itc.tx_out.pop().unwrap(), vec![1u8, 2, 3]);
    }

    /// T38（F5-c）：客户端**并发**重组两条大回复（`reassembly-buffer-count-8`）。
    /// 手工交错注入两报文的真分片——`REASSEMBLY_BUFFER_COUNT=1` 的旧形态下第二条
    /// 报文的首片会顶掉第一条的上下文（本用例即判别面）。
    #[test]
    fn client_multi_concurrent_reasm() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));
        let cip = Ipv4Addr::new(100, 64, 10, 77);
        let mut client = StackB::new(cip, tunnel, SmolInstant::from_millis(0));
        let ha = client_udp_socket(&mut client, 47010);
        let hb = client_udp_socket(&mut client, 47011);
        let fa = make_udp_flow(&mut itc, cip, 47010, (Ipv4Addr::new(8, 8, 8, 8), 53));
        let fb = make_udp_flow(&mut itc, cip, 47011, (Ipv4Addr::new(9, 9, 9, 9), 53));
        let pa: Vec<u8> = (0..3000u32).map(|i| (i % 197) as u8).collect();
        let pb: Vec<u8> = (0..2600u32).map(|i| (i % 193) as u8).collect();
        itc.udp_send_to_client(fa, &pa);
        let frs_a = pump_all_fragments(&mut itc);
        itc.udp_send_to_client(fb, &pb);
        let frs_b = pump_all_fragments(&mut itc);
        assert!(frs_a.len() >= 3 && frs_b.len() >= 3, "两报文各 ≥3 片");
        let mut tick = 0i64;
        for i in 0..frs_a.len().max(frs_b.len()) {
            for p in [frs_a.get(i), frs_b.get(i)].into_iter().flatten() {
                client.inject(p);
                tick += 5;
                client.iface.poll(
                    SmolInstant::from_millis(tick),
                    &mut client.device,
                    &mut client.sockets,
                );
            }
        }
        let get = |client: &mut StackB, h: SocketHandle| -> Option<Vec<u8>> {
            let mut buf = vec![0u8; 65536];
            let n = client
                .sockets
                .get_mut::<smoltcp::socket::udp::Socket>(h)
                .recv_slice(&mut buf)
                .map(|(n, _)| n)
                .unwrap_or(0);
            (n > 0).then(|| buf[..n].to_vec())
        };
        assert_eq!(get(&mut client, ha).as_deref(), Some(&pa[..]), "报文 A 并发重组成功");
        assert_eq!(get(&mut client, hb).as_deref(), Some(&pb[..]), "报文 B 并发重组成功");
    }

    /// T35：客户端发侧 R4 回归——4096B 与 65000B 载荷都真分片发出（此前 >1472 静默丢）。
    #[test]
    fn client_udp_fragments_up_to_64k() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 76),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let h = client_udp_socket(&mut client, 47004);
        for (size, payload) in [(4096usize, vec![0x71u8; 4096]), (65000, vec![0x72u8; 65000])] {
            client
                .sockets
                .get_mut::<smoltcp::socket::udp::Socket>(h)
                .send_slice(
                    &payload,
                    IpEndpoint::new(Ipv4Addr::new(1, 1, 1, 1).into(), 9999),
                )
                .expect("send_slice 进栈 tx 缓冲");
            let mut frags: Vec<Vec<u8>> = Vec::new();
            let mut tick = 0i64;
            for _ in 0..64 {
                tick += 5;
                client
                    .iface
                    .poll(SmolInstant::from_millis(tick), &mut client.device, &mut client.sockets);
                let mut out = Vec::new();
                client.device.drain_tx(&mut out);
                frags.extend(out);
                if frags
                    .iter()
                    .any(|p| nat::Ipv4FragHdr::parse(p).is_some_and(|f| !f.hdr.mf))
                {
                    break;
                }
            }
            assert!(frags.len() >= 2, "{size}B 载荷应真分片（得 {} 片）", frags.len());
            let bytes: usize = frags
                .iter()
                .map(|p| nat::Ipv4FragHdr::parse(p).map(|f| f.payload.len()).unwrap_or(0))
                .sum();
            assert_eq!(bytes, size + 8, "{size}B：线上分片总载荷 = UDP 头 + 载荷");
        }
    }
}

