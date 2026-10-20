//! doh — 出口侧公网观测腿的**域名解析面**（2026-10-20 排障引入）。
//!
//! 背景（外网手机落中继的排查结论）：STUN 的 UDP 发包早已钉物理网卡（不受 TUN 型
//! 代理影响），但 `stun.cloudflare.com` 的解析走系统 getaddrinfo——被本机代理工具的
//! fake-IP 模式污染成 198.18.x（虚拟地址只在代理的 TUN 内有意义，物理网络无人应答）
//! ⇒ STUN「解析失败或无应答」、公网端点不公布。UPnP 的组播同理被默认路由抢走。
//!
//! 本模块的职责 = 把解析从系统解析器挪到**国内 DoH**（阿里/腾讯），且 DoH 的 TCP
//! 连接本身钉物理网卡（socket2：setsockopt 在 connect 之前）——解析腿不进 TUN、
//! 不吃 fake-IP，也不受运营商 UDP 53 劫持影响。
//!
//! 形态：`host@ip[,host@ip]` 候选列表（host = TLS SNI 与证书主体；ip = 钉卡 TCP
//! 直连目标——**本身绕开解析**，这是整条腿成立的根基）。HTTP/1.1 POST
//! `application/dns-message`（RFC 8484），根证书用 webpki-roots（不碰系统钥匙串，
//! OHOS/交叉构建同样可用）。

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
#[cfg(unix)]
use std::os::fd::AsRawFd as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::Logf;

/// 单次 DoH 查询的连接+TLS+HTTP 总预算（pubep 线程节拍 120s/600s，这里短平快）。
const QUERY_BUDGET: Duration = Duration::from_secs(3);

/// 一台 DoH 服务器：`host`（SNI/证书主体/路径域名）+ `addr`（直连 IP——不经解析）。
#[derive(Debug, Clone)]
pub struct DohServer {
    pub host: String,
    pub addr: IpAddr,
}

impl DohServer {
    pub fn parse(spec: &str) -> Option<Self> {
        let (host, ip) = spec.split_once('@')?;
        let host = host.trim();
        if host.is_empty() {
            return None;
        }
        Some(Self { host: host.to_owned(), addr: ip.trim().parse().ok()? })
    }
}

/// 解析候选列表（逗号分隔；坏项跳过——配置容错，不给严格读层添新非法面）。
pub fn parse_list(s: &str) -> Vec<DohServer> {
    s.split(',').filter_map(DohServer::parse).collect()
}

/// **污染闸**（解析结果与 STUN 观测结果共用）：fake-IP 段（RFC 2544 基准测试段，
/// Surge/Clash 的 fake-IP 默认池）、私网、环回、未指定、链路本地、组播——任何观测
/// 腿拿到这类地址都只可能是「本机代理/TUN 的投影」，公布出去必然误导客户端。
pub fn poisoned_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 198 && (o[1] == 18 || o[1] == 19) // fake-IP 池（198.18.0.0/15 = 198.18.x + 198.19.x）
                || v4.is_private()
                || v4.is_loopback()
                || v4.is_unspecified()
                || v4.is_link_local()
                || v4.is_multicast()
                || v4.is_broadcast()
                || o[0] == 100 && o[1] == 64 // 运营商 CGNAT 段（100.64/10）：不是「公网」
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (v6.segments()[0] & 0xfe00) == 0xfc00 // ULA fc00::/7
                || (v6.segments()[0] & 0xffc0) == 0xfe80 // 链路本地
        }
    }
}

/// DoH 解析器（出口装配点构造；pubep 线程同步调用——该线程本就是阻塞节拍形态）。
pub struct DohResolver {
    servers: Vec<DohServer>,
    /// 钉卡目标（网卡 index/name；None = 未钉——按默认路由连，行为退化为普通 DoH）。
    /// 看护重钉（EngineCmd::Repin）会同步更新——解析腿始终与数据面同卡。
    iface: Arc<Mutex<Option<(u32, String)>>>,
    logf: Logf,
}

impl DohResolver {
    pub fn new(
        servers: Vec<DohServer>,
        iface: Arc<Mutex<Option<(u32, String)>>>,
        logf: Logf,
    ) -> Self {
        Self { servers, iface, logf }
    }

    /// 解析 A 记录（首个非污染地址胜；全败 None）。
    pub fn resolve4(&self, name: &str) -> Option<Ipv4Addr> {
        self.resolve(name, 1).into_iter().find_map(|ip| match ip {
            IpAddr::V4(v4) => Some(v4),
            IpAddr::V6(_) => None,
        })
    }

    /// 解析 AAAA 记录（非 v4-mapped 的 v6；全败 None）。
    pub fn resolve6(&self, name: &str) -> Option<Ipv6Addr> {
        self.resolve(name, 28).into_iter().find_map(|ip| match ip {
            IpAddr::V6(v6) if v6.to_ipv4_mapped().is_none() => Some(v6),
            _ => None,
        })
    }

    fn resolve(&self, name: &str, qtype: u16) -> Vec<IpAddr> {
        if self.servers.is_empty() {
            // 未配 DoH：退回系统解析器（仍过污染闸——fake-IP 环境下等于观测腿作废，
            // 但闸会把它变成**可见**的「全部落污染段」而不是发向虚拟地址的静默超时）。
            use std::net::ToSocketAddrs as _;
            let Ok(it) = (name, 0u16).to_socket_addrs() else { return Vec::new() };
            let want_v6 = qtype == 28;
            let all: Vec<IpAddr> = it.map(|a| a.ip()).filter(|ip| ip.is_ipv6() == want_v6).collect();
            let clean: Vec<IpAddr> = all.iter().copied().filter(|ip| !poisoned_ip(ip)).collect();
            if clean.is_empty() && !all.is_empty() {
                (*self.logf)(&format!(
                    "DoH 未配置，系统解析 {name} 应答全部落在污染段（fake-IP/私网，如 {}）——丢弃；建议配置 serve.doh",
                    all[0]
                ));
            }
            return clean;
        }
        let q = build_query(name, qtype);
        for (i, srv) in self.servers.iter().enumerate() {
            match self.query_one(srv, &q) {
                Ok(mut addrs) => {
                    // 污染闸：把 fake-IP/私网投影挡在观测面之外（记行——它是
                    // 「上游/中间层被污染」的直接证据，比静默跳过可排查）。
                    let clean: Vec<IpAddr> =
                        addrs.drain(..).filter(|a| !poisoned_ip(a)).collect();
                    if clean.is_empty() {
                        (*self.logf)(&format!(
                            "DoH：{} 应答 {} 全部落在污染段（fake-IP/私网）——丢弃",
                            srv.host, name
                        ));
                        continue;
                    }
                    return clean;
                }
                Err(e) => {
                    let last = i + 1 == self.servers.len();
                    (*self.logf)(&format!(
                        "DoH：{} 查询失败（{e}）{}",
                        srv.host,
                        if last { "（候选已尽）" } else { "——换下一个候选" }
                    ));
                }
            }
        }
        Vec::new()
    }

    /// 单服务器一次查询：DNS wire → HTTPS POST → wire 解析。
    fn query_one(&self, srv: &DohServer, q: &[u8]) -> Result<Vec<IpAddr>, String> {
        let tcp = self.connect_pinned(srv.addr)?;
        let tls = ClientConnection::new(
            Arc::new(doh_client_config()),
            ServerName::try_from(srv.host.clone()).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let mut stream = StreamOwned::new(tls, tcp);

        let req = format!(
            "POST /dns-query HTTP/1.1\r\nHost: {}\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            srv.host,
            q.len()
        );
        stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        stream.write_all(q).map_err(|e| e.to_string())?;
        stream.flush().map_err(|e| e.to_string())?;

        let mut raw = Vec::with_capacity(1024);
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    raw.extend_from_slice(&buf[..n]);
                    if raw.len() > 64 * 1024 {
                        return Err("响应超限".to_owned());
                    }
                }
                Err(e) => return Err(e.to_string()),
            }
        }
        let body = http_body(&raw).ok_or("HTTP 响应解析失败")?;
        parse_answers(body).map_err(|e| e.to_string())
    }

    /// 钉卡 TCP（socket2：setsockopt 必须在 connect 之前——std 没有这个口子）。
    fn connect_pinned(&self, ip: IpAddr) -> Result<TcpStream, String> {
        let domain = match ip {
            IpAddr::V4(_) => socket2::Domain::IPV4,
            IpAddr::V6(_) => socket2::Domain::IPV6,
        };
        let sock = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if let Some((index, name)) = self.iface.lock().ok().and_then(|g| g.clone()) {
            if index != 0 {
                if let Err(e) =
                    crate::server::egress::pin_socket_to_iface(sock.as_raw_fd(), index, &name)
                {
                    // 钉卡失败不致命：按默认路由连（行为退化为普通 DoH——记行可排查）
                    (*self.logf)(&format!("DoH：钉卡 {name} 失败（{e}）—— 按默认路由连接"));
                }
            }
        }
        let sa: socket2::SockAddr = SocketAddr::new(ip, 443).into();
        sock.connect_timeout(&sa, QUERY_BUDGET).map_err(|e| e.to_string())?;
        sock.set_read_timeout(Some(QUERY_BUDGET)).map_err(|e| e.to_string())?;
        sock.set_write_timeout(Some(QUERY_BUDGET)).map_err(|e| e.to_string())?;
        sock.set_tcp_nodelay(true).ok();
        Ok(sock.into())
    }
}

/// rustls 客户端配置（webpki-roots 根束；无客户端证书；TLS1.2/1.3 默认面）。
fn doh_client_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()
}

/// HTTP/1.1 响应体提取：头部到 `\r\n\r\n`，按 Content-Length（或读到连接关）。
fn http_body(raw: &[u8]) -> Option<&[u8]> {
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..sep]).ok()?;
    let status = head.lines().next()?;
    if !status.contains(" 200 ") {
        return None;
    }
    let len = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        k.eq_ignore_ascii_case("content-length")
            .then(|| v.trim().parse::<usize>().ok())?
    });
    let body = &raw[sep + 4..];
    match len {
        Some(n) if body.len() >= n => Some(&body[..n]),
        Some(_) => None,
        None if !body.is_empty() => Some(body),
        None => None,
    }
}

/// 构造 DNS 查询报文（RFC 1035 最小面：头 12B + 一个 question；RD=1）。
fn build_query(name: &str, qtype: u16) -> Vec<u8> {
    let mut m = Vec::with_capacity(17 + name.len());
    m.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]); // id=0, RD=1（无 DNSSEC）
    m.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // qd=1
    for label in name.trim_end_matches('.').split('.') {
        m.push(label.len().clamp(1, 63) as u8);
        m.extend_from_slice(label.as_bytes());
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&[0, 1]); // IN
    m
}

/// 解析 DNS 应答的 answer 区（A=1 / AAAA=28；跳过 question；压缩指针按 RFC 1035 跳）。
fn parse_answers(resp: &[u8]) -> Result<Vec<IpAddr>, String> {
    if resp.len() < 12 {
        return Err("短包".into());
    }
    let rcode = resp[3] & 0x0f;
    if rcode != 0 {
        return Err(format!("rcode={rcode}"));
    }
    let qd = u16::from_be_bytes([resp[4], resp[5]]);
    let an = u16::from_be_bytes([resp[6], resp[7]]) as usize;
    let mut off = 12usize;
    // 跳过 question 区（qname + 4B）
    for _ in 0..qd {
        off = skip_name(resp, off)?;
        off = off.checked_add(4).ok_or("question 区越界")?;
    }
    let mut out = Vec::new();
    for _ in 0..an {
        off = skip_name(resp, off)?;
        if off + 10 > resp.len() {
            return Err("answer 区越界".into());
        }
        let ty = u16::from_be_bytes([resp[off], resp[off + 1]]);
        let rdlen = u16::from_be_bytes([resp[off + 8], resp[off + 9]]) as usize;
        let rd = off + 10;
        if rd + rdlen > resp.len() {
            return Err("rd 越界".into());
        }
        match (ty, rdlen) {
            (1, 4) => out.push(IpAddr::V4(Ipv4Addr::new(resp[rd], resp[rd + 1], resp[rd + 2], resp[rd + 3]))),
            (28, 16) => {
                let mut b = [0u8; 16];
                b.copy_from_slice(&resp[rd..rd + 16]);
                out.push(IpAddr::V6(Ipv6Addr::from(b)));
            }
            _ => {}
        }
        off = rd + rdlen;
    }
    Ok(out)
}

/// 跳过一个域名（label 序列或压缩指针——指针即终止）。
fn skip_name(resp: &[u8], mut off: usize) -> Result<usize, String> {
    loop {
        if off >= resp.len() {
            return Err("name 越界".into());
        }
        let len = resp[off] as usize;
        if len & 0xc0 == 0xc0 {
            return Ok(off + 2); // 压缩指针：2 字节，name 结束
        }
        if len & 0xc0 != 0 {
            return Err("保留 label 形态".into());
        }
        off += 1 + len;
        if len == 0 {
            return Ok(off);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doh_server_parse_list() {
        let v = parse_list("dns.alidns.com@223.5.5.5, doh.pub@1.12.12.12, 坏项, no-at");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].host, "dns.alidns.com");
        assert_eq!(v[0].addr, IpAddr::V4(Ipv4Addr::new(223, 5, 5, 5)));
        assert_eq!(v[1].addr, IpAddr::V4(Ipv4Addr::new(1, 12, 12, 12)));
        assert!(parse_list("").is_empty());
    }

    #[test]
    fn poison_gate_covers_fakeip_and_private() {
        // fake-IP（Surge/Clash 默认池 198.18.0.0/15 的两端与中点）
        for ip in ["198.18.0.1", "198.18.255.1", "198.19.0.1"] {
            assert!(poisoned_ip(&ip.parse::<IpAddr>().unwrap()), "{ip} 须判污染");
        }
        // 私网/环回/CGNAT/链路本地/组播
        for ip in ["10.0.0.1", "192.168.3.12", "127.0.0.1", "100.64.255.1", "169.254.1.1", "224.0.0.1"] {
            assert!(poisoned_ip(&ip.parse::<IpAddr>().unwrap()), "{ip} 须判污染");
        }
        // 真公网（含本项目在役出口）不误伤
        for ip in ["114.242.60.128", "123.56.218.212", "8.8.8.8", "223.5.5.5"] {
            assert!(!poisoned_ip(&ip.parse::<IpAddr>().unwrap()), "{ip} 不得误判");
        }
        // v6：ULA/链路本地判污染；全球单播不误伤
        assert!(poisoned_ip(&"fd00::1".parse::<IpAddr>().unwrap()));
        assert!(poisoned_ip(&"fe80::1".parse::<IpAddr>().unwrap()));
        assert!(!poisoned_ip(&"2408:8207::1".parse::<IpAddr>().unwrap()));
    }

    #[test]
    fn dns_wire_roundtrip() {
        let q = build_query("stun.miwifi.com", 1);
        assert_eq!(&q[..4], &[0, 0, 1, 0], "RD=1");
        assert_eq!(&q[q.len() - 5..], &[0, 0, 1, 0, 1][..], "name 终止 0 + A(0,1) + IN(0,1)");

        // 造一个应答：头（qd=1, an=1）+ 原样 question + 一条 A 记录（压缩指针回指）
        let mut r = Vec::new();
        r.extend_from_slice(&[0, 0, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
        r.extend_from_slice(&q[12..]); // question 区原样
        r.extend_from_slice(&[0xc0, 0x0c]); // name 指针回 qname
        r.extend_from_slice(&[0, 1, 0, 1]); // A / IN
        r.extend_from_slice(&[0, 0, 0, 60]); // TTL
        r.extend_from_slice(&[0, 4]); // rdlen
        r.extend_from_slice(&[111, 206, 174, 2]); // 111.206.174.2（实测 miwifi v4）
        let addrs = parse_answers(&r).expect("应可解析");
        assert_eq!(addrs, vec![IpAddr::V4(Ipv4Addr::new(111, 206, 174, 2))]);

        // AAAA 形态 + 污染段过滤（解析器侧的闸另有用例，这里锁 wire 面）
        let mut r6 = Vec::new();
        r6.extend_from_slice(&[0, 0, 0x81, 0x80, 0, 1, 0, 1, 0, 0, 0, 0]);
        let q6 = build_query("stun.hitv.com", 28);
        r6.extend_from_slice(&q6[12..]);
        r6.extend_from_slice(&[0xc0, 0x0c, 0, 28, 0, 1, 0, 0, 0, 60, 0, 16]);
        r6.extend_from_slice(&[0x24, 0x0e, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);
        let addrs6 = parse_answers(&r6).expect("应可解析 v6");
        assert_eq!(
            addrs6,
            vec![IpAddr::V6("240e::16".parse::<Ipv6Addr>().unwrap().into())]
        );

        // rcode 面
        let mut bad = r.clone();
        bad[3] = 0x83; // rcode=3 NXDOMAIN
        assert!(parse_answers(&bad).is_err());
    }

    /// 真网链路验证（`cargo test -p homeway-core --lib doh -- --ignored` 手动跑；
    /// 在**代理工具增强模式开着**的机器上跑才有意义——这正是本模块的存在理由）：
    /// 钉 en0 的 DoH 能解析出真实 A 记录（不被 fake-IP 污染、不进 TUN）。
    #[test]
    #[ignore = "真网验证：需手动跑（且最好在 Surge/Clash 增强模式下验证钉卡面）"]
    fn doh_resolves_for_real_on_pinned_iface() {
        let iface = std::env::var("DOH_TEST_IFACE").unwrap_or_else(|_| "en0".to_owned());
        let idx = iface_index(&iface);
        let r = DohResolver::new(
            parse_list("dns.alidns.com@223.5.5.5,doh.pub@1.12.12.12"),
            std::sync::Arc::new(std::sync::Mutex::new(idx.map(|i| (i, iface.clone())))),
            std::sync::Arc::new(|l: &str| eprintln!("[doh-test] {l}")),
        );
        let a = r.resolve4("stun.miwifi.com").expect("钉卡 DoH 应解析出真实 A 记录");
        assert!(!poisoned_ip(&IpAddr::V4(a)), "不得是 fake-IP/私网（实得 {a}）");
        eprintln!("[doh-test] stun.miwifi.com -> {a}");
        let aaaa = r.resolve6("stun.hitv.com");
        if let Some(v6) = aaaa {
            assert!(!poisoned_ip(&IpAddr::V6(v6)));
            eprintln!("[doh-test] stun.hitv.com -> {v6}");
        }
    }

    #[cfg(unix)]
    fn iface_index(name: &str) -> Option<u32> {
        let c = std::ffi::CString::new(name).ok()?;
        let i = unsafe { libc::if_nametoindex(c.as_ptr()) };
        (i != 0).then_some(i)
    }

    #[cfg(not(unix))]
    fn iface_index(_name: &str) -> Option<u32> {
        None
    }

    #[test]
    fn http_body_extraction() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcd";
        assert_eq!(http_body(ok), Some(&b"abcd"[..]));
        let no_len = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nxyz";
        assert_eq!(http_body(no_len), Some(&b"xyz"[..]));
        let err = b"HTTP/1.1 503 x\r\nContent-Length: 2\r\n\r\nno";
        assert_eq!(http_body(err), None);
        let truncated = b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nabc";
        assert_eq!(http_body(truncated), None);
    }
}
