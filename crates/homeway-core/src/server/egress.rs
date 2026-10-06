//! 出口的上行网卡选择与探测（R3；语义真源 `pkg/egress/{egress,stun}.go` +
//! `pkg/ifaceutil`）。
//!
//! - **WG socket 钉卡**：默认路由可能被 TUN 型代理抢走——不钉的话 STUN 观测到的是
//!   代理的 NAT 映射 ⇒ 打洞与公网端点公布都不成立。钉哪张卡由**探针**决定：
//!   `physical_candidates` 枚举候选（up/非回环/非虚拟/有 IPv4），`select_best`
//!   逐张发 anycast DNS 探针（字面 IP——规则型代理会把域名解析成 fake-IP）取最快
//!   探通的，默认路由那张卡探通就优先它。
//! - **STUN Binding 客户端**（RFC 5389 最小面）：判定「默认路径能不能承载**通用
//!   UDP（非 53）」的可校验探针（TUN 代理对 UDP 按端口区别对待——只测 53 会得出
//!   「UDP 可用」的错误结论）；校验事务 ID + magic cookie，不「收到包就算通」。
//!   监听 socket 上的 STUN 观测（同 socket 收发）在 `bind.rs` 的 stun_query 面。

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::fd::AsRawFd as _;
use std::time::{Duration, Instant};

/// egress 错误面（R3-M23 类型化）：Display 文案与既有日志/判据行**同串**（消费面
/// `{e}` 透传打印，文案即契约——重构不给文案漂移留口子）。
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// 底层 IO（bind/setsockopt/读写）——errno 上下文透传。
    #[error(transparent)]
    Io(#[from] io::Error),
    /// 网卡名含 NUL（setsockopt 的 C 串边界）。
    #[error("网卡名含 NUL")]
    IfaceNameNul,
    /// 探针绝对期限烧完（per-recv 续命已封——probe_with 的 M7 注记）。
    #[error("egress: 探针预算耗尽")]
    ProbeBudget,
    /// 无候选物理网卡（up/非虚拟/有 IPv4 的前置全不满足）。
    #[error("egress: 没有候选物理网卡（都 up/非虚拟/有 IPv4？）")]
    NoCandidates,
    /// 候选全探不通（明细以「；」连接——E21 失败形态行素材）。
    #[error("egress: 所有候选网卡都探不通：{0}")]
    AllUnreachable(String),
}

/// STUN magic cookie（RFC 5389）。
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;
const STUN_BINDING_REQ: u16 = 0x0001;
const STUN_BINDING_RESP: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;

/// 探针目标（anycast DNS，**字面 IP**——DefaultProbeTargets）。
pub fn default_probe_targets() -> Vec<SocketAddrV4> {
    vec![
        SocketAddrV4::new(Ipv4Addr::new(223, 5, 5, 5), 53),
        SocketAddrV4::new(Ipv4Addr::new(1, 1, 1, 1), 53),
    ]
}

/// 通用 UDP（非 53）探针目标（DefaultSTUNTargets——Cloudflare/Google 的 STUN 都在
/// 3478 系端口上，且都是「随便什么 UDP 都能到」的服务）。
pub fn default_stun_targets() -> Vec<SocketAddrV4> {
    vec![
        SocketAddrV4::new(Ipv4Addr::new(162, 159, 207, 1), 3478),
        SocketAddrV4::new(Ipv4Addr::new(74, 125, 250, 129), 19302),
    ]
}

/// 公网单播地址吗（v4/v6 都判）。**token 里只放公网地址**用这个判据：私网（RFC1918）、
/// CGNAT(100.64/10)、回环、链路本地、ULA(fc00::/7)、未指定、组播一律不算。
pub fn is_public_addr(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_multicast()
                || v4.is_broadcast()
                // 100.64/10（CGNAT，运营商大内网）也不是公网
                || (u32::from(v4) & 0xFFC0_0000) == 0x6440_0000)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                // v4-mapped 按其 v4 本值判（Go `Unmap()` 同义——评审 M21）
                return is_public_addr(IpAddr::V4(v4));
            }
            !(v6.is_loopback()
                || is_ula(v6)
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xffc0) == 0xfe80) // 链路本地
        }
    }
}

/// ULA（fc00::/7）：首字节高 7 位为 1111110。
fn is_ula(v6: std::net::Ipv6Addr) -> bool {
    (v6.segments()[0] & 0xfe00) == 0xfc00
}

/// 名字像隧道/虚拟网卡吗（ifaceutil.IsVirtual 的前缀并集——多跳过一张虚拟卡只会让
/// 候选少一张，不会选错上行）。
pub fn is_virtual_iface(name: &str) -> bool {
    const PREFIXES: [&str; 19] = [
        "lo", "utun", "ipsec", "gif", "stf", "awdl", "llw", "anpi", "ap", "bridge", "vmnet",
        "vmenet", "tap", "tun", "tailscale", "docker", "br-", "veth", "virbr",
    ];
    let l = name.to_ascii_lowercase();
    PREFIXES.iter().any(|p| l.starts_with(p))
}

/// 一张网卡的可用信息（getifaddrs 面）。
#[derive(Clone, Debug)]
pub struct IfaceInfo {
    pub name: String,
    pub index: u32,
    pub addrs: Vec<Ipv4Addr>,
    /// `ip/prefix` 形态（netmask 换算——E21 判据行的 `addrs=[192.168.3.12/24]` 面）。
    pub cidrs: Vec<String>,
    pub up: bool,
    pub loopback: bool,
}

/// 枚举系统网卡（getifaddrs FFI；失败返回空表——候选面走「不绑」）。
pub fn interfaces() -> Vec<IfaceInfo> {
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return Vec::new();
        }
        let mut order: Vec<String> = Vec::new();
        let mut map: std::collections::HashMap<String, IfaceInfo> = std::collections::HashMap::new();
        let mut cur = ifap;
        while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_name.is_null() {
                let name = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
                let flags = ifa.ifa_flags;
                let e = map.entry(name.clone()).or_insert_with(|| {
                    order.push(name.clone());
                    IfaceInfo { name: name.clone(), index: 0, addrs: Vec::new(), cidrs: Vec::new(), up: false, loopback: false }
                });
                e.up = e.up || (flags & libc::IFF_UP as u32) != 0;
                e.loopback = e.loopback || (flags & libc::IFF_LOOPBACK as u32) != 0;
                if !ifa.ifa_addr.is_null()
                    && (*ifa.ifa_addr).sa_family == libc::AF_INET as libc::sa_family_t
                {
                    let sa = ifa.ifa_addr as *const libc::sockaddr_in;
                    let raw = (*sa).sin_addr.s_addr;
                    e.addrs.push(Ipv4Addr::from(raw.swap_bytes()));
                    if !ifa.ifa_netmask.is_null()
                        && (*ifa.ifa_netmask).sa_family == libc::AF_INET as libc::sa_family_t
                    {
                        let nm = ifa.ifa_netmask as *const libc::sockaddr_in;
                        let mask = (*nm).sin_addr.s_addr.swap_bytes();
                        e.cidrs.push(format!(
                            "{}/{}",
                            Ipv4Addr::from(raw.swap_bytes()),
                            mask.count_ones()
                        ));
                    }
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        let mut out: Vec<IfaceInfo> = order.into_iter().filter_map(|n| map.remove(&n)).collect();
        for e in &mut out {
            e.index = libc::if_nametoindex(
                std::ffi::CString::new(e.name.as_bytes()).expect("网卡名无 NUL").as_ptr(),
            );
        }
        out
    }
}

/// 候选上行网卡（up、非回环、非虚拟、至少有一个 IPv4——PhysicalCandidates）。
pub fn physical_candidates() -> Vec<IfaceInfo> {
    interfaces()
        .into_iter()
        .filter(|i| i.up && !i.loopback && !is_virtual_iface(&i.name) && !i.addrs.is_empty())
        .collect()
}

/// 把 UDP socket 钉在网卡上（darwin IP_BOUND_IF+IPV6_BOUND_IF 两族 / linux
/// SO_BINDTODEVICE——ifacebind_*.go 同义；unix 面由本仓仅服务 macOS/linux）。
///
/// darwin **两族都设、各自单栈容错**（Go `pinToFD` 语义）：v4-only socket 上
/// IPV6_BOUND_IF 报 ENOPROTOOPT、v6-only socket 上 IP_BOUND_IF 报错——都属预期，
/// 另一族成立即算钉上；**两族都失败才报错**（错误文案 = Go 同串
/// `IP_BOUND_IF/IPV6_BOUND_IF: %v / %v`——文案即契约）。半边缺失的形态会让人
/// 「v6 钉卡失败拖死 v4」（GAP-AUDIT P0-2 的根因），两族独立容错后 v6 路径可用性
/// 不再受 v4 面牵连（反之亦然）。
pub fn pin_socket_to_iface(fd: std::os::fd::RawFd, index: u32, name: &str) -> io::Result<()> {
    unsafe {
        #[cfg(target_os = "macos")]
        {
            let _ = name;
            let idx = index as libc::c_int;
            let r4 = libc::setsockopt(
                fd,
                libc::IPPROTO_IP,
                libc::IP_BOUND_IF,
                &idx as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
            // errno 即取即存（第二次 setsockopt 会覆盖 errno——分开取防串）
            let e4 = (r4 != 0).then(io::Error::last_os_error);
            let r6 = libc::setsockopt(
                fd,
                libc::IPPROTO_IPV6,
                libc::IPV6_BOUND_IF,
                &idx as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
            let e6 = (r6 != 0).then(io::Error::last_os_error);
            match (e4, e6) {
                (Some(e4), Some(e6)) => Err(io::Error::other(format!(
                    "IP_BOUND_IF/IPV6_BOUND_IF: {e4} / {e6}"
                ))),
                _ => Ok(()),
            }
        }
        #[cfg(target_os = "linux")]
        {
            let _ = index; // linux 走 SO_BINDTODEVICE（按名），index 不参与
            let cname = std::ffi::CString::new(name)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "网卡名含 NUL"))?;
            let r = libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_BINDTODEVICE,
                cname.as_ptr().cast(),
                cname.as_bytes().len() as u32,
            );
            if r != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        }
        #[cfg(not(any(target_os = "macos", target_os = "linux")))]
        {
            let _ = (fd, index, name);
            Ok(())
        }
    }
}

/// 找出拥有该地址的网卡（Go `IfaceForAddr`——IP 字面量绑定形态的**附带钉卡**：
/// 绑源地址还要把 socket 钉在该网卡上，否则默认路由被 TUN 型代理抢走时观测仍被
/// 污染；取不到 = None，调用方按未钉卡处理）。
pub fn iface_for_addr(ip: IpAddr) -> Option<IfaceInfo> {
    interfaces().into_iter().find(|i| {
        i.addrs.iter().any(|a| IpAddr::V4(*a) == ip)
            || i.cidrs.iter().any(|c| c.split('/').next() == Some(&ip.to_string()))
    })
}

/// 系统默认路由会从哪张卡出去（UDP dial 只做路由查询，不发包）；拿不到 None。
pub fn preferred_iface() -> Option<IfaceInfo> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("223.5.5.5:53").ok()?;
    let local = match s.local_addr() {
        Ok(SocketAddr::V4(v4)) => *v4.ip(),
        _ => return None,
    };
    interfaces().into_iter().find(|i| i.addrs.contains(&local))
}

// ---------- STUN Binding 客户端（egress/stun.go 平移） ----------

/// 12 字节事务 ID。
pub fn new_txid() -> [u8; 12] {
    let mut id = [0u8; 12];
    getrandom::getrandom(&mut id).expect("系统随机源不可用");
    id
}

/// 组一条 Binding 请求（带 SOFTWARE 属性；部分服务端只认带属性的请求）。
pub fn stun_request(txid: &[u8; 12], software: &str) -> Vec<u8> {
    let mut attrs = Vec::new();
    if !software.is_empty() {
        attrs.extend_from_slice(&[0x80, 0x22]);
        attrs.extend_from_slice(&(software.len() as u16).to_be_bytes());
        attrs.extend_from_slice(software.as_bytes());
        while attrs.len() % 4 != 0 {
            attrs.push(0);
        }
    }
    let mut out = Vec::with_capacity(20 + attrs.len());
    out.extend_from_slice(&STUN_BINDING_REQ.to_be_bytes());
    out.extend_from_slice(&(attrs.len() as u16).to_be_bytes());
    out.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    out.extend_from_slice(txid);
    out.extend_from_slice(&attrs);
    out
}

/// 解析 Binding 应答并取出 XOR-MAPPED-ADDRESS（缺省回退 MAPPED-ADDRESS）。
/// txID 不匹配或不是成功应答都算「不是我们的应答」（返回 None——调用方继续等）。
pub fn parse_stun_response(b: &[u8]) -> Option<([u8; 12], SocketAddr)> {
    if b.len() < 20 {
        return None;
    }
    if u16::from_be_bytes([b[0], b[1]]) != STUN_BINDING_RESP {
        return None;
    }
    if u32::from_be_bytes(b[4..8].try_into().expect("定长")) != STUN_MAGIC_COOKIE {
        return None;
    }
    let txid: [u8; 12] = b[8..20].try_into().expect("定长");
    let mut length = u16::from_be_bytes([b[2], b[3]]) as usize;
    if 20 + length > b.len() {
        length = b.len() - 20;
    }
    let attrs = &b[20..20 + length];
    let mut xor = None;
    let mut plain = None;
    let mut off = 0usize;
    while attrs.len() >= off + 4 {
        let atype = u16::from_be_bytes([attrs[off], attrs[off + 1]]);
        let alen = u16::from_be_bytes([attrs[off + 2], attrs[off + 3]]) as usize;
        if off + 4 + alen > attrs.len() {
            break;
        }
        let val = &attrs[off + 4..off + 4 + alen];
        match atype {
            ATTR_XOR_MAPPED_ADDRESS => {
                if let Some(ap) = parse_mapped(val, &txid, true) {
                    xor = Some(ap);
                }
            }
            ATTR_MAPPED_ADDRESS => {
                if let Some(ap) = parse_mapped(val, &txid, false) {
                    plain = Some(ap);
                }
            }
            _ => {}
        }
        let pad = (4 - alen % 4) % 4;
        off += 4 + alen + pad;
    }
    xor.or(plain).map(|ap| (txid, ap))
}

fn parse_mapped(v: &[u8], txid: &[u8; 12], xor: bool) -> Option<SocketAddr> {
    if v.len() < 4 {
        return None;
    }
    let family = v[1];
    let mut port = u16::from_be_bytes([v[2], v[3]]);
    if xor {
        port ^= (STUN_MAGIC_COOKIE >> 16) as u16;
    }
    match family {
        0x01 => {
            if v.len() < 8 {
                return None;
            }
            let mut a = [v[4], v[5], v[6], v[7]];
            if xor {
                let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
                for (i, b) in a.iter_mut().enumerate() {
                    *b ^= cookie[i];
                }
            }
            Some(SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::from(a), port)))
        }
        0x02 => {
            if v.len() < 20 {
                return None;
            }
            let mut a = [0u8; 16];
            a.copy_from_slice(&v[4..20]);
            if xor {
                let mut key = [0u8; 16];
                key[..4].copy_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
                key[4..].copy_from_slice(txid);
                for (b, k) in a.iter_mut().zip(key) {
                    *b ^= k;
                }
            }
            Some(SocketAddr::V6(std::net::SocketAddrV6::new(a.into(), port, 0, 0)))
        }
        _ => None,
    }
}

// ---------- 探针（probeWith / ProbeSTUN 平移） ----------

/// 一条最小 A 查询（随机名 + RD）——收到 ID 匹配且来源正确的包即证明这条路能承载
/// 我们自己的 UDP 往返（应答内容无关紧要）。
fn dns_probe_query(txid: [u8; 2]) -> Vec<u8> {
    let mut b = Vec::new();
    b.extend_from_slice(&txid);
    b.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
    let mut rnd = [0u8; 6];
    getrandom::getrandom(&mut rnd).expect("系统随机源不可用");
    let label = format!("p{}", hex(&rnd));
    b.push(label.len() as u8);
    b.extend_from_slice(label.as_bytes());
    for s in ["probe", "invalid"] {
        b.push(s.len() as u8);
        b.extend_from_slice(s.as_bytes());
    }
    b.push(0);
    b.extend_from_slice(&1u16.to_be_bytes()); // A
    b.extend_from_slice(&1u16.to_be_bytes()); // IN
    b
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 判据三件套（事务 ID 匹配、来源 == 所查服务器、是合法 DNS 应答——应答形只查
/// 「ID 对 + 来源对」：Rcode/QDCOUNT 形态无关紧要，Go probeWith 同口径）。
fn probe_with(
    pin: Option<&IfaceInfo>,
    targets: &[SocketAddrV4],
    timeout: Duration,
) -> Result<Duration, EgressError> {
    let targets = if targets.is_empty() { &default_probe_targets()[..] } else { targets };
    let s = UdpSocket::bind("0.0.0.0:0")?;
    if let Some(ifi) = pin {
        pin_socket_to_iface(s.as_raw_fd(), ifi.index, &ifi.name)?;
    }
    let deadline = Instant::now() + timeout;
    s.set_read_timeout(Some(timeout))?;
    let mut txid = [0u8; 2];
    getrandom::getrandom(&mut txid).expect("系统随机源不可用");
    let query = dns_probe_query(txid);
    let start = Instant::now();
    for t in targets {
        let _ = s.send_to(&query, t);
    }
    let mut buf = [0u8; 1500];
    loop {
        // **绝对期限**（评审 M7：per-recv 超时会被不匹配包续命——任意 LAN 主机
        // 灌包即可让探针永不返回、select_best 的 join 无限等）
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return Err(EgressError::ProbeBudget);
        }
        s.set_read_timeout(Some(remain))?;
        let (n, from) = s.recv_from(&mut buf)?;
        if n < 12 || buf[0] != txid[0] || buf[1] != txid[1] {
            continue; // 不是我们这条查询的应答
        }
        let from_v4 = match from {
            SocketAddr::V4(v4) => *v4.ip(),
            SocketAddr::V6(_) => continue,
        };
        if !targets.iter().any(|t| t.ip() == &from_v4) {
            continue; // 被劫持/代答（来源不是我们查的服务器）⇒ 不算探通
        }
        return Ok(start.elapsed());
    }
}

/// 从系统默认路由发一次 DNS 探针（不绑卡）——判定「经默认路径转发出去的 UDP 到底
/// 能不能回来」（udpcap 的 DNS:53 位）。
pub fn probe_default(targets: &[SocketAddrV4], timeout: Duration) -> Result<Duration, EgressError> {
    probe_with(None, targets, timeout)
}

/// 从指定网卡发一次 DNS 探针（绑卡后发——select_best 的判据）。
pub fn probe_iface(ifi: &IfaceInfo, targets: &[SocketAddrV4], timeout: Duration) -> Result<Duration, EgressError> {
    probe_with(Some(ifi), targets, timeout)
}

/// 发一次 STUN Binding 探针（默认路由或指定网卡）。返回 STUN 服务器看到的映射地址
/// 与 RTT——证明「非 53 的通用 UDP 能出去、也能回来」（udpcap 的通用位）。
pub fn probe_stun(
    pin: Option<&IfaceInfo>,
    targets: &[SocketAddrV4],
    timeout: Duration,
) -> Result<(SocketAddr, Duration), EgressError> {
    let targets = if targets.is_empty() { &default_stun_targets()[..] } else { targets };
    let s = UdpSocket::bind("0.0.0.0:0")?;
    if let Some(ifi) = pin {
        pin_socket_to_iface(s.as_raw_fd(), ifi.index, &ifi.name)?;
    }
    let deadline = Instant::now() + timeout;
    s.set_read_timeout(Some(timeout))?;
    let txid = new_txid();
    let req = stun_request(&txid, "homeway-probe");
    let start = Instant::now();
    for t in targets {
        let _ = s.send_to(&req, t);
    }
    let mut buf = [0u8; 1500];
    loop {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return Err(EgressError::ProbeBudget);
        }
        s.set_read_timeout(Some(remain))?;
        let (n, from) = s.recv_from(&mut buf)?;
        let from_ip = match from.ip() {
            IpAddr::V4(v4) => v4,
            IpAddr::V6(_) => continue,
        };
        if !targets.iter().any(|t| t.ip() == &from_ip) {
            continue;
        }
        if let Some((got_tx, mapped)) = parse_stun_response(&buf[..n]) {
            if got_tx == txid {
                return Ok((mapped, start.elapsed()));
            }
        }
    }
}

/// 并发探测所有候选，返回**最快探通**的那张卡（默认路由那张卡探通就优先它——
/// 「探得通」不等于「钉对了」：docker 网桥也能探通 DNS，但收不到入向 WG 包）。
/// 全不通返回每张卡的结论明细（E21 的失败形态行素材）。
pub fn select_best(
    cands: &[IfaceInfo],
    targets: &[SocketAddrV4],
    timeout: Duration,
    logf: &dyn Fn(&str),
) -> Result<IfaceInfo, EgressError> {
    if cands.is_empty() {
        return Err(EgressError::NoCandidates);
    }
    let prefer = preferred_iface();
    let mut results: Vec<(IfaceInfo, Result<Duration, EgressError>)> = Vec::new();
    std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for c in cands {
            let targets = targets.to_vec();
            let probe_c = c.clone();
            handles.push(scope.spawn(move || (probe_c.clone(), probe_iface(&probe_c, &targets, timeout))));
        }
        for h in handles {
            if let Ok(r) = h.join() {
                results.push(r);
            }
        }
    });
    let mut details = Vec::new();
    let mut best: Option<(IfaceInfo, Duration)> = None;
    let mut prefer_hit: Option<IfaceInfo> = None;
    for (ifi, r) in results {
        match r {
            Ok(rtt) => {
                details.push(format!("{} 通（{}ms）", ifi.name, rtt.as_millis()));
                if let Some(p) = &prefer {
                    if p.index == ifi.index {
                        prefer_hit = Some(ifi.clone());
                    }
                }
                if best.is_none() || rtt < best.as_ref().unwrap().1 {
                    best = Some((ifi, rtt));
                }
            }
            Err(e) => details.push(format!("{} 不通（{}）", ifi.name, e)),
        }
    }
    logf(&format!("网卡探测：{}", details.join("；")));
    let mut best = match best {
        Some(b) => b,
        None => {
            return Err(EgressError::AllUnreachable(details.join("；")))
        }
    };
    if let Some(p) = prefer_hit {
        best.0 = p; // 默认路由那张卡才是外面看到的入口，探通就优先它
    }
    Ok(best.0)
}


// ---------- P2：出向接口 MTU（内层 MTU 升档的启动自检面） ----------

/// 读一张网卡的 MTU（P2 L4 自检用）。平台分派（评审 P2-r1-2(i)）：
/// macOS = getifaddrs 的 `ifa_data`→`if_data.ifi_mtu`；Linux/OHOS = `ioctl
/// (SIOCGIFMTU)`（libc 0.2.189 未给 generic linux 导出该常量，本地钉 0x8921——
/// Linux/Android 系统头同值）。失败（名字不在/调用错）= None（调用方按
/// 「自检跳过」处理，不拒启）。
pub fn iface_mtu(name: &str) -> Option<u32> {
    #[cfg(target_os = "macos")]
    unsafe {
        let mut ifap: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut ifap) != 0 {
            return None;
        }
        let mut out = None;
        let mut cur = ifap;
        'walk: while !cur.is_null() {
            let ifa = &*cur;
            if !ifa.ifa_name.is_null() && !ifa.ifa_data.is_null() {
                let n = std::ffi::CStr::from_ptr(ifa.ifa_name).to_string_lossy();
                if n == name {
                    out = Some((*ifa.ifa_data.cast::<libc::if_data>()).ifi_mtu as u32);
                    break 'walk;
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(ifap);
        out
    }
    #[cfg(not(target_os = "macos"))]
    {
        // ifreq：IFNAMSIZ(16) 名字 + union（MTU = c_int，紧随其后）。40B 覆盖
        // 全平台 ifr 尺寸；只用前 16+4。
        // request 参数类型随 libc 平台面不同（glibc=c_ulong / musl/OHOS=c_int）——
        // 常量钉 c_int、调用点 `as _` 适配两者。
        const SIOCGIFMTU: libc::c_int = 0x8921;
        let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) };
        if fd < 0 {
            return None;
        }
        let mut ifr = [0u8; 40];
        let bytes = name.as_bytes();
        if bytes.len() >= libc::IFNAMSIZ {
            unsafe { libc::close(fd) };
            return None;
        }
        ifr[..bytes.len()].copy_from_slice(bytes);
        let rc = unsafe { libc::ioctl(fd, SIOCGIFMTU as _, &mut ifr as *mut u8) };
        let mtu = if rc == 0 {
            Some(u32::from(ifr[16]) | (u32::from(ifr[17]) << 8) | (u32::from(ifr[18]) << 16) | (u32::from(ifr[19]) << 24))
        } else {
            None
        };
        unsafe { libc::close(fd) };
        mtu
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// STUN 编解码：请求形状 + XOR 解码（对拍 RFC 5389 示例形态）。
    #[test]
    fn stun_request_shape() {
        let txid = [7u8; 12];
        let req = stun_request(&txid, "homeway-probe");
        assert_eq!(u16::from_be_bytes([req[0], req[1]]), STUN_BINDING_REQ);
        assert_eq!(u32::from_be_bytes(req[4..8].try_into().unwrap()), STUN_MAGIC_COOKIE);
        assert_eq!(&req[8..20], &txid[..]);
        // SOFTWARE 属性 + 4 字节对齐
        let alen = u16::from_be_bytes([req[2], req[3]]) as usize;
        assert_eq!(alen % 4, 0);
        assert!(req[20..].starts_with(&[0x80, 0x22]));
    }

    /// XOR-MAPPED-ADDRESS 解码（构造一条 v4 应答）。
    #[test]
    fn stun_parse_xor_v4() {
        let txid = new_txid();
        let ip = Ipv4Addr::new(203, 0, 113, 9);
        let port: u16 = 41641;
        let mut attr = vec![0x00, 0x01]; // family v4
        attr.extend_from_slice(&(port ^ (STUN_MAGIC_COOKIE >> 16) as u16).to_be_bytes());
        let xored = ip.octets().map2_cookie();
        attr.extend_from_slice(&xored);
        let mut resp = Vec::new();
        resp.extend_from_slice(&STUN_BINDING_RESP.to_be_bytes());
        resp.extend_from_slice(&((attr.len() + 4) as u16).to_be_bytes());
        resp.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        resp.extend_from_slice(&txid);
        resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
        resp.extend_from_slice(&(attr.len() as u16).to_be_bytes());
        resp.extend_from_slice(&attr);
        let (got_tx, ap) = parse_stun_response(&resp).expect("应解出");
        assert_eq!(got_tx, txid);
        assert_eq!(ap, SocketAddr::V4(SocketAddrV4::new(ip, port)));
    }

    /// 探针目标 + 公网判定 + 虚拟网卡判定（纯函数面）。
    #[test]
    fn public_and_virtual() {
        assert!(is_public_addr(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9))));
        assert!(!is_public_addr(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 2))), "RFC1918");
        assert!(!is_public_addr(IpAddr::V4(Ipv4Addr::new(100, 64, 255, 1))), "CGNAT");
        assert!(!is_public_addr(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(!is_public_addr(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))), "链路本地");
        assert!(is_virtual_iface("utun3"));
        assert!(is_virtual_iface("docker0"));
        assert!(!is_virtual_iface("en0"));
        assert!(!is_virtual_iface("eth0"));
        // 100.64/10 的另一端（100.127.x）也不算；100.128 起算公网段
        assert!(!is_public_addr(IpAddr::V4(Ipv4Addr::new(100, 127, 255, 254))));
        assert!(is_public_addr(IpAddr::V4(Ipv4Addr::new(100, 128, 0, 1))));
    }

    /// getifaddrs 枚举 + 候选过滤（真机面：至少有 lo0 被虚拟判定滤掉或存在一张物理卡）。
    #[test]
    fn iface_enumeration() {
        let ifis = interfaces();
        assert!(!ifis.is_empty(), "系统至少有一张网卡");
        for i in &ifis {
            assert!(!i.name.is_empty());
        }
        // 物理候选里没有 lo/utun
        for i in physical_candidates() {
            assert!(!is_virtual_iface(&i.name), "{} 不应是虚拟卡", i.name);
            assert!(!i.loopback);
        }
    }

    /// 两族钉卡（真 socket 面）：v4 socket 上 v6 族 setsockopt 报错被容错、v4 族
    /// 成立 ⇒ Ok；v6 socket 同理；**两族都失败才 Err**（文案 = Go 同串）。无物理
    /// 网卡的形态（CI 容器）跳过。
    #[test]
    fn pin_socket_dual_family_tolerant() {
        use std::os::fd::AsRawFd as _;
        let Some(ifi) = physical_candidates().into_iter().next() else {
            return;
        };
        let s4 = UdpSocket::bind("0.0.0.0:0").unwrap();
        assert!(
            pin_socket_to_iface(s4.as_raw_fd(), ifi.index, &ifi.name).is_ok(),
            "v4 socket：v6 族失败应被单栈容错"
        );
        // 真 v6 单栈（V6ONLY=1——std bind 的 [::] 在 macOS 默认双栈，测不到 v4 族
        // 失败容错；r1-L10）：v4 族 setsockopt 报错被容错、v6 族成立
        let s6 = crate::udpbatch::bind_v6_only(std::net::Ipv6Addr::LOCALHOST, 0).unwrap();
        assert!(
            pin_socket_to_iface(s6.as_raw_fd(), ifi.index, &ifi.name).is_ok(),
            "v6 单栈 socket：v4 族失败应被单栈容错"
        );
        // 双栈 socket（服务端主 socket 形态）：两族都成立
        let dual = UdpSocket::bind("[::]:0").unwrap();
        unsafe {
            let off: libc::c_int = 0;
            libc::setsockopt(
                dual.as_raw_fd(),
                libc::IPPROTO_IPV6,
                libc::IPV6_V6ONLY,
                &off as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
        }
        assert!(pin_socket_to_iface(dual.as_raw_fd(), ifi.index, &ifi.name).is_ok());
        // 负例分平台（首跑 cb64fce CI 实测红在 ubuntu——两平台的前提根本不同）：
        // - macOS：按 index 钉卡，「非法 index ⇒ 两族都失败」成立 ⇒ Err + Go 同串文案；
        // - linux：SO_BINDTODEVICE 按名、index 不参与——「非法 index」前提不成立，且
        //   **已钉过的 socket 重复设置在无特权下恒 EPERM**（内核只放行首次绑定，
        //   容器实测 fresh=OK / re-set-same=EPERM / 坏名=ENODEV）——复用 s4 会把
        //   权限形态误判成钉卡语义。等价负例 = **新 socket** + 不存在网卡名 ⇒ ENODEV
        //   （内核先查名后查权，特权/无特权同值）。
        #[cfg(target_os = "macos")]
        {
            let e = pin_socket_to_iface(s4.as_raw_fd(), u32::MAX, &ifi.name).unwrap_err();
            assert!(
                e.to_string().starts_with("IP_BOUND_IF/IPV6_BOUND_IF:"),
                "错误文案应与 Go 同串：{e}"
            );
        }
        #[cfg(target_os = "linux")]
        {
            let s = UdpSocket::bind("0.0.0.0:0").unwrap();
            let e = pin_socket_to_iface(s.as_raw_fd(), ifi.index, "hwtest-nodev0").unwrap_err();
            assert_eq!(
                e.raw_os_error(),
                Some(libc::ENODEV),
                "不存在网卡名应 ENODEV：{e}"
            );
        }
    }

    /// 本地 fake STUN 服务器：probe_stun 全链（事务 ID/XOR/来源校验）。
    #[test]
    fn probe_stun_against_local_fake() {
        let srv = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let srv_addr = srv.local_addr().unwrap();
        let port = srv_addr.port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, from)) = srv.recv_from(&mut buf) else { return };
                let Some((txid, _)) = parse_stun_request(&buf[..n]) else { continue };
                // 回 XOR-MAPPED-ADDRESS 127.0.0.1:port
                let ip = Ipv4Addr::LOCALHOST;
                let mut attr = vec![0x00, 0x01];
                attr.extend_from_slice(&(port ^ (STUN_MAGIC_COOKIE >> 16) as u16).to_be_bytes());
                let xored = ip.octets().map2_cookie();
                attr.extend_from_slice(&xored);
                let mut resp = Vec::new();
                resp.extend_from_slice(&STUN_BINDING_RESP.to_be_bytes());
                resp.extend_from_slice(&((attr.len() + 4) as u16).to_be_bytes());
                resp.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
                resp.extend_from_slice(&txid);
                resp.extend_from_slice(&ATTR_XOR_MAPPED_ADDRESS.to_be_bytes());
                resp.extend_from_slice(&(attr.len() as u16).to_be_bytes());
                resp.extend_from_slice(&attr);
                let _ = srv.send_to(&resp, from);
            }
        });
        let target = match srv_addr {
            SocketAddr::V4(v4) => v4,
            _ => panic!("v4"),
        };
        let (mapped, rtt) = probe_stun(None, &[target], Duration::from_secs(3)).expect("应探通");
        assert_eq!(mapped, SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)));
        assert!(rtt < Duration::from_secs(3));
    }

/// 测试面：解析请求拿 txid。
#[cfg(test)]
fn parse_stun_request(b: &[u8]) -> Option<([u8; 12], ())> {
    if b.len() < 20 || u16::from_be_bytes([b[0], b[1]]) != STUN_BINDING_REQ {
        return None;
    }
    Some((b[8..20].try_into().expect("定长"), ()))
}

#[cfg(test)]
trait Map2Cookie {
    fn map2_cookie(self) -> [u8; 4];
}

#[cfg(test)]
impl Map2Cookie for [u8; 4] {
    fn map2_cookie(self) -> [u8; 4] {
        let cookie = STUN_MAGIC_COOKIE.to_be_bytes();
        [
            self[0] ^ cookie[0],
            self[1] ^ cookie[1],
            self[2] ^ cookie[2],
            self[3] ^ cookie[3],
        ]
    }
}
}
