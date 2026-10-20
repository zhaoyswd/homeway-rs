//! doh — 出口侧公网观测腿的**域名解析面**（2026-10-20 排障引入）。
//!
//! 背景（外网手机落中继的排查结论）：STUN 的 UDP 发包早已钉物理网卡（不受 TUN 型
//! 代理影响），但 `stun.cloudflare.com` 的解析走系统 getaddrinfo——被本机代理工具的
//! fake-IP 模式污染成 198.18.x（虚拟地址只在代理的 TUN 内有意义，物理网络无人应答）
//! ⇒ STUN「解析失败或无应答」、公网端点不公布。UPnP 的组播同理被默认路由抢走。
//!
//! 本模块的职责 = 把解析从系统解析器挪到**国内 DoH（默认阿里双 IP）**，且 DoH 的 TCP
//! 连接本身钉物理网卡（socket2：setsockopt 在 connect 之前，三平台——darwin
//! `IP_BOUND_IF` / linux `SO_BINDTODEVICE` / windows `IP_UNICAST_IF`）——解析腿不进
//! TUN、不吃 fake-IP，也不受运营商 UDP 53 劫持影响。
//!
//! 形态：`host@ip[,host@ip]` 候选列表（host = TLS SNI 与证书主体；ip = 钉卡 TCP
//! 直连目标——**本身绕开解析**，这是整条腿成立的根基）。HTTP/1.1 POST
//! `application/dns-message`（RFC 8484），根证书用 webpki-roots（不碰系统钥匙串，
//! OHOS/交叉构建同样可用）。

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
#[cfg(unix)]
use std::os::fd::AsRawFd as _;
#[cfg(windows)]
use std::os::windows::io::AsRawSocket as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rustls::pki_types::ServerName;
use rustls::{ClientConfig, ClientConnection, RootCertStore, StreamOwned};

use crate::Logf;

/// 单次 DoH 查询的**总**预算（连接 + TLS + HTTP 全程共享一个 deadline——r1#3：
/// 修前 connect/read/write 各设 3s，单服务器最坏 ~9s；总预算让慢/黑洞环境快速换候选）。
const QUERY_BUDGET: Duration = Duration::from_secs(3);

/// DoH 腿的错误分类（r1#11：仓约定 thiserror——日志/调用侧按 Display 渲染，
/// 分类面供后续按类处置）。
#[derive(Debug, thiserror::Error)]
pub(crate) enum DohError {
    #[error("连接失败（{0}）")]
    Connect(String),
    #[error("TLS 失败（{0}）")]
    Tls(String),
    #[error("HTTP 应答（{0}）")]
    Http(String),
    #[error("DNS wire（{0}）")]
    Wire(String),
}

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
        // ip 剥成对 []（`host@[2408::1]` 是用户自然写法；与 STUN 侧 trim 口径一致——r1#9）
        let ip = ip.trim().trim_matches(|c| c == '[' || c == ']');
        Some(Self { host: host.to_owned(), addr: ip.parse().ok()? })
    }
}

/// 解析候选列表（逗号分隔；坏项**收集并返回**——不静默，调用方经
/// [`DohResolver::from_list`] 记行；不给严格读层添新非法面）。
pub fn parse_list(s: &str) -> (Vec<DohServer>, Vec<String>) {
    let mut ok = Vec::new();
    let mut bad = Vec::new();
    for spec in s.split(',').map(str::trim).filter(|c| !c.is_empty()) {
        match DohServer::parse(spec) {
            Some(v) => ok.push(v),
            None => bad.push(spec.to_owned()),
        }
    }
    (ok, bad)
}

/// **污染闸**（解析结果与 STUN 观测结果共用）：任何观测腿拿到非公网地址（fake-IP、
/// 私网、CGNAT、ULA……）都只可能是「本机代理/TUN 的投影」，公布出去必然误导客户端。
/// 判据**单源复用** `egress`（评审 r1#5：本文件曾自写一份，CGNAT `/10` 写成了 `/16`，
/// 口径漂移）——`is_fake_ip`（198.18.0.0/15，Surge/Clash fake-IP 池）∪ 非 `is_public_addr`
/// （后者含 v4-mapped 按 v4 本值判、CGNAT /10、ULA、链路本地等全值域）。
pub fn poisoned_ip(ip: &IpAddr) -> bool {
    // v4-mapped 先 unmap 成 v4 本值再判 fake 段（r1#5）——`is_fake_ip` 的窄契约是
    // 「v6 恒假」（F3），unmap 在本闸做；`is_public_addr` 自身已按 M21 unmap。
    let fake_probe = match ip {
        IpAddr::V6(v6) => v6.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(*ip),
        v4 => *v4,
    };
    crate::server::egress::is_fake_ip(fake_probe)
        || !crate::server::egress::is_public_addr(*ip)
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

    /// [`parse_list`] 的记行形态（r1#6）：坏项静默丢弃会让「配错 doh」退化成「没配
    /// DoH」且无从分辨——装配点用本函数构造，坏项落一行、全无效给醒目告警。
    pub fn from_list(
        list: (Vec<DohServer>, Vec<String>),
        iface: Arc<Mutex<Option<(u32, String)>>>,
        logf: Logf,
    ) -> Self {
        let (servers, bad) = list;
        if !bad.is_empty() {
            (*logf)(&format!(
                "DoH：候选串有 {} 项无法解析（{:?}）—— 已丢弃（形态应为 host@ip，逗号分隔）",
                bad.len(),
                bad
            ));
            if servers.is_empty() {
                (*logf)(
                    "⚠️ DoH：全部候选无效——例外回退系统解析器（仍过污染闸；至少一个候选有效时为 fail-closed 不回退），请检查 serve.doh 配置",
                );
            }
        }
        Self::new(servers, iface, logf)
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
        let Ok(q) = build_query(name, qtype) else {
            (*self.logf)(&format!("DoH：查询名 {name} 非法（label 空/超 63/超长）——拒绝"));
            return Vec::new();
        };
        for (i, srv) in self.servers.iter().enumerate() {
            match self.query_one(srv, &q) {
                Ok(mut addrs) => {
                    // r1#13：NODATA（answer 空）与「被污染」是两回事——先判空，别把
                    // 「没有记录」报成「污染」带偏排障。
                    if addrs.is_empty() {
                        (*self.logf)(&format!(
                            "DoH：{} 应答 {} 无 A/AAAA 记录（NODATA）——换下一个候选",
                            srv.host, name
                        ));
                        continue;
                    }
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
                        if last {
                            "（候选已尽——**不退回系统解析器**：配了 DoH 即 fail-closed，防 fake-IP 环境下被系统解析绕过；需回退请显式置空 serve.doh）"
                        } else {
                            "——换下一个候选"
                        }
                    ));
                }
            }
        }
        Vec::new()
    }

    /// 单服务器一次查询：DNS wire → HTTPS POST → wire 解析。
    fn query_one(&self, srv: &DohServer, q: &[u8]) -> Result<Vec<IpAddr>, DohError> {
        let deadline = std::time::Instant::now() + QUERY_BUDGET;
        let tcp = self.connect_pinned(srv.addr, deadline)?;
        // r2#3/r2#15：socket 超时 = **单次** syscall 的上界（connect 后按剩余量设一次）；
        // **总期限**由 read_http_response 内的 Instant 检查承载（慢速滴流不能续命）。
        let remain = |tcp: &TcpStream| {
            let r = deadline.saturating_duration_since(std::time::Instant::now());
            if r.is_zero() {
                Err(DohError::Http("总预算耗尽".into()))
            } else {
                tcp.set_read_timeout(Some(r))
                    .and_then(|()| tcp.set_write_timeout(Some(r)))
                    .map_err(|e| DohError::Http(e.to_string()))
            }
        };
        let tls = ClientConnection::new(
            Arc::new(doh_client_config()),
            ServerName::try_from(srv.host.clone()).map_err(|e| DohError::Tls(e.to_string()))?,
        )
        .map_err(|e| DohError::Tls(e.to_string()))?;
        // TLS 握手与请求写发生在同一 TcpStream 上——单次 syscall 上界由 connect 后
        // 设置的 socket 超时承载（见 remain 注释）。
        let mut stream = StreamOwned::new(tls, tcp);

        let req = format!(
            "POST /dns-query HTTP/1.1\r\nHost: {}\r\nContent-Type: application/dns-message\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            srv.host,
            q.len()
        );
        remain(stream.get_ref())?;
        stream
            .write_all(req.as_bytes())
            .and_then(|()| stream.write_all(q))
            .and_then(|()| stream.flush())
            .map_err(|e| DohError::Tls(e.to_string()))?;

        let body = read_http_response(&mut stream, deadline)?;
        parse_answers(&body).map_err(DohError::Wire)
    }

    /// 钉卡 TCP（socket2：setsockopt 必须在 connect 之前——std 没有这个口子）。
    /// `deadline` = 本次查询的总预算（connect 与后续读写共享剩余量）。
    fn connect_pinned(&self, ip: IpAddr, deadline: std::time::Instant) -> Result<TcpStream, DohError> {
        let domain = match ip {
            IpAddr::V4(_) => socket2::Domain::IPV4,
            IpAddr::V6(_) => socket2::Domain::IPV6,
        };
        let sock = socket2::Socket::new(domain, socket2::Type::STREAM, Some(socket2::Protocol::TCP))
            .map_err(|e| DohError::Connect(e.to_string()))?;
        // 钉卡全平台（评审 r1#2：修前只有 unix 钉、Windows 静默走默认路由——TUN+fake-IP
        // 环境下 Windows 出口的解析腿同样被污染；`pin_socket_to_iface` 本就三平台实现——
        // darwin IP_BOUND_IF / linux SO_BINDTODEVICE / windows IP_UNICAST_IF）。
        if let Some((index, name)) = self.iface.lock().ok().and_then(|g| g.clone()) {
            if index != 0 {
                #[cfg(unix)]
                let raw = sock.as_raw_fd();
                #[cfg(windows)]
                let raw = sock.as_raw_socket();
                if let Err(e) = crate::server::egress::pin_socket_to_iface(raw, index, &name) {
                    // 钉卡失败不致命：按默认路由连（行为退化为普通 DoH——记行可排查）
                    (*self.logf)(&format!("DoH：钉卡 {name} 失败（{e}）—— 按默认路由连接"));
                }
            }
        }
        let sa: socket2::SockAddr = SocketAddr::new(ip, 443).into();
        let remaining = || deadline.saturating_duration_since(std::time::Instant::now());
        sock.connect_timeout(&sa, remaining()).map_err(|e| DohError::Connect(e.to_string()))?;
        sock.set_read_timeout(Some(remaining())).map_err(|e| DohError::Connect(e.to_string()))?;
        sock.set_write_timeout(Some(remaining())).map_err(|e| DohError::Connect(e.to_string()))?;
        sock.set_tcp_nodelay(true).ok();
        Ok(sock.into())
    }
}

/// HTTP/1.1 应答读取（r2#15：`Read` 泛型 + 函数内 Instant 总期限——Cursor/滴流
/// Read 均可单测；socket 超时只作单次 syscall 上界）。头读满 `\r\n\r\n` 后按
/// Content-Length 精确读体并截断；chunked 显式拒绝；状态码按字段。
fn read_http_response<R: Read>(r: &mut R, deadline: std::time::Instant) -> Result<Vec<u8>, DohError> {
    let expired = || DohError::Http("总预算耗尽（慢速滴流被总期限截断）".into());
    let mut raw = Vec::with_capacity(1024);
    let mut buf = [0u8; 4096];
    let head_end = loop {
        if let Some(pos) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        if std::time::Instant::now() >= deadline {
            return Err(expired());
        }
        match r.read(&mut buf) {
            Ok(0) => return Err(DohError::Http("响应头不完整（EOF 先于头部结束）".into())),
            Ok(n) => {
                raw.extend_from_slice(&buf[..n]);
                if raw.len() > 64 * 1024 {
                    return Err(DohError::Http("响应超限".into()));
                }
            }
            Err(e) => return Err(DohError::Http(e.to_string())),
        }
    };
    let (status, clen, chunked) =
        http_head_facts(&raw[..head_end + 4]).ok_or(DohError::Http("响应头解析失败".into()))?;
    if !(200..300).contains(&status) {
        return Err(DohError::Http(format!("状态 {status} 非 2xx")));
    }
    if chunked {
        return Err(DohError::Http("Transfer-Encoding: chunked 不受支持（显式失败——见上游形态）".into()));
    }
    let want = clen.ok_or(DohError::Http("无 Content-Length 且非 chunked（应答形态不支持）".into()))?;
    if want > 64 * 1024 {
        return Err(DohError::Http("Content-Length 超限".into()));
    }
    let mut body = raw[head_end + 4..].to_vec();
    while body.len() < want {
        if std::time::Instant::now() >= deadline {
            return Err(expired());
        }
        match r.read(&mut buf) {
            Ok(0) => return Err(DohError::Http("响应体不完整（EOF 先于 Content-Length）".into())),
            Ok(n) => body.extend_from_slice(&buf[..n]),
            Err(e) => return Err(DohError::Http(e.to_string())),
        }
    }
    body.truncate(want); // CL 之外的多发字节一律截掉（r2#4）
    Ok(body)
}

/// 响应头三元事实（状态码 / Content-Length / chunked）——`read_http_response` 的
/// 纯函数面（单测见 `http_head_parse_forms`）。头不完整返回 None。
fn http_head_facts(raw: &[u8]) -> Option<(u16, Option<usize>, bool)> {
    let pos = raw.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&raw[..pos]).ok()?;
    let status = head
        .lines()
        .next()?
        .split_whitespace()
        .nth(1)?
        .parse::<u16>()
        .ok()?;
    let mut clen = None;
    let mut chunked = false;
    for l in head.lines().skip(1) {
        if let Some((k, v)) = l.split_once(':') {
            if k.eq_ignore_ascii_case("content-length") {
                clen = v.trim().parse::<usize>().ok();
            } else if k.eq_ignore_ascii_case("transfer-encoding")
                && v.to_ascii_lowercase().contains("chunked")
            {
                chunked = true;
            }
        }
    }
    Some((status, clen, chunked))
}

/// rustls 客户端配置（webpki-roots 根束；无客户端证书；TLS1.2/1.3 默认面）。
fn doh_client_config() -> ClientConfig {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    ClientConfig::builder().with_root_certificates(roots).with_no_client_auth()
}

/// 构造 DNS 查询报文（RFC 1035 最小面：头 12B + 一个 question；RD=1）。
/// 非法域名**显式拒绝**（r1#12：修前 `clamp` 会把空 label / >63 label 构造成
/// 错位报文——错误不可见）。
fn build_query(name: &str, qtype: u16) -> Result<Vec<u8>, String> {
    let mut m = Vec::with_capacity(17 + name.len());
    m.extend_from_slice(&[0x00, 0x00, 0x01, 0x00]); // id=0, RD=1（无 DNSSEC）
    m.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // qd=1
    // 尾点只剥一个（r2#12：`a..` 的第二个点是空 label，须显式拒绝而非静默吃掉）
    let host = name.strip_suffix('.').unwrap_or(name);
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return Err(format!("{name}: label 空/超 63（{label:?}）"));
        }
        m.push(label.len() as u8);
        m.extend_from_slice(label.as_bytes());
    }
    // RFC 1035：编码 qname（labels + 首长字节 + 终止 0）≤ 255B
    if m.len() - 12 > 255 {
        return Err(format!("{name}: 域名编码超 255B"));
    }
    m.push(0);
    m.extend_from_slice(&qtype.to_be_bytes());
    m.extend_from_slice(&[0, 1]); // IN
    Ok(m)
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
        let (v, bad) = parse_list("dns.alidns.com@223.5.5.5, doh.pub@1.12.12.12, 坏项, no-at");
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].host, "dns.alidns.com");
        assert_eq!(v[0].addr, IpAddr::V4(Ipv4Addr::new(223, 5, 5, 5)));
        assert_eq!(v[1].addr, IpAddr::V4(Ipv4Addr::new(1, 12, 12, 12)));
        // 坏项被收集（r1#6：不静默）而非无声丢弃
        assert_eq!(bad, vec!["坏项".to_owned(), "no-at".to_owned()]);
        // IPv6 字面量带括号（r1#9：与 STUN 侧 trim 口径一致）
        let (v6, bad6) = parse_list("dns6.example@[2408:8207::1]");
        assert_eq!(v6.len(), 1);
        assert_eq!(v6[0].addr, "2408:8207::1".parse::<IpAddr>().unwrap());
        assert!(bad6.is_empty());
        assert!(parse_list("").0.is_empty());
    }

    #[test]
    fn poison_gate_covers_fakeip_and_private() {
        // fake-IP（Surge/Clash 默认池 198.18.0.0/15 的两端与中点）
        for ip in ["198.18.0.1", "198.18.255.1", "198.19.0.1"] {
            assert!(poisoned_ip(&ip.parse::<IpAddr>().unwrap()), "{ip} 须判污染");
        }
        // 私网/环回/CGNAT（/10 全段——r1#5 修前自写 /16 漏 100.65–100.127）/链路本地/组播
        for ip in [
            "10.0.0.1",
            "192.168.3.12",
            "127.0.0.1",
            "100.64.255.1",
            "100.100.1.1",
            "100.127.255.254",
            "169.254.1.1",
            "224.0.0.1",
        ] {
            assert!(poisoned_ip(&ip.parse::<IpAddr>().unwrap()), "{ip} 须判污染");
        }
        // CGNAT 段之外不误伤（100.128.0.0 起 = 公网）
        assert!(!poisoned_ip(&"100.128.0.1".parse::<IpAddr>().unwrap()));
        // v4-mapped 的 v6 按其 v4 本值判（单源 is_public_addr 的口径）
        assert!(poisoned_ip(&"::ffff:198.18.0.1".parse::<IpAddr>().unwrap()));
        assert!(poisoned_ip(&"::ffff:10.0.0.1".parse::<IpAddr>().unwrap()));
        assert!(!poisoned_ip(&"::ffff:114.242.60.128".parse::<IpAddr>().unwrap()));
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
        let q = build_query("stun.miwifi.com", 1).expect("合法名");
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
        let q6 = build_query("stun.hitv.com", 28).expect("合法名");
        r6.extend_from_slice(&q6[12..]);
        r6.extend_from_slice(&[0xc0, 0x0c, 0, 28, 0, 1, 0, 0, 0, 60, 0, 16]);
        r6.extend_from_slice(&[0x24, 0x0e, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x16]);
        let addrs6 = parse_answers(&r6).expect("应可解析 v6");
        assert_eq!(
            addrs6,
            vec![IpAddr::V6("240e::16".parse::<Ipv6Addr>().unwrap())]
        );

        // rcode 面
        let mut bad = r.clone();
        bad[3] = 0x83; // rcode=3 NXDOMAIN
        assert!(parse_answers(&bad).is_err());
    }

    #[test]
    fn build_query_rejects_malformed_names() {
        // r1#12：空 label / >63 label / 层数超限显式拒绝（修前 clamp 会造出错位报文）
        assert!(build_query("a..b", 1).is_err(), "空 label 拒绝");
        assert!(build_query(&"x".repeat(64), 1).is_err(), ">63 label 拒绝");
        assert!(build_query("a..", 1).is_err(), "双尾点 = 空 label，拒绝（不再静默剥）");
        // RFC 上限：编码 qname 恰 255B 的合法名不误拒
        let max_name = format!("{}.", vec!["a".repeat(61); 4].join("."));
        assert_eq!(max_name.len(), 61 * 4 + 3 + 1);
        assert!(build_query(&max_name, 1).is_ok(), "合法 255B 名不误拒");
        assert!(build_query(&vec!["a".repeat(63); 5].join("."), 1).is_err(), "总长超限拒绝");
        assert!(build_query("stun.miwifi.com", 1).is_ok());
        assert!(build_query("stun.miwifi.com.", 28).is_ok(), "尾点剥除");
    }

    /// r2#15：read_http_response 的 Reader 级单测（无需 TLS/443——Cursor 与受控 Read）。
    #[test]
    fn read_http_response_reader_forms() {
        use std::io::Cursor;
        let far = std::time::Instant::now() + Duration::from_secs(30);
        // ① 正常：头 + CL 精确体 + 超发字节截断
        let mut c = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcdEXTRA".to_vec());
        assert_eq!(read_http_response(&mut c, far).unwrap(), b"abcd".to_vec());
        // ② 无 reason phrase
        let mut c2 = Cursor::new(b"HTTP/1.1 200\r\nContent-Length: 1\r\n\r\nx".to_vec());
        assert_eq!(read_http_response(&mut c2, far).unwrap(), b"x".to_vec());
        // ③ 头/体分读（一次只给 1 字节的滴流 Read——头读满 + 体读满都走多轮）
        struct Trickle(Vec<u8>, usize);
        impl Read for Trickle {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.1 >= self.0.len() || buf.is_empty() {
                    return Ok(0);
                }
                buf[0] = self.0[self.1];
                self.1 += 1;
                Ok(1)
            }
        }
        let mut t = Trickle(b"HTTP/1.1 200 OK\r\nContent-Length: 3\r\n\r\nabc".to_vec(), 0);
        assert_eq!(read_http_response(&mut t, far).unwrap(), b"abc".to_vec());
        // ④ 慢滴流 + 已过期 deadline ⇒ 总期限截断（不无限续命——r2#3 回归）
        let mut t2 = Trickle(b"HTTP/1.1 200 OK\r\nContent-Length: 99\r\n\r\naaaa".to_vec(), 0);
        let past = std::time::Instant::now() - Duration::from_secs(1);
        assert!(read_http_response(&mut t2, past).is_err(), "过期 deadline 必须截断滴流");
        // ⑤ EOF 先于头结束 / 先于体
        let mut c5 = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Len".to_vec());
        assert!(read_http_response(&mut c5, far).is_err());
        let mut c6 = Cursor::new(b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\n\r\nabc".to_vec());
        assert!(read_http_response(&mut c6, far).is_err());
        // ⑥ chunked 显式拒绝；CL:0 与缺 CL 区分；非 2xx 拒
        let mut c7 = Cursor::new(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc".to_vec());
        assert!(read_http_response(&mut c7, far).is_err());
        let mut c8 = Cursor::new(b"HTTP/1.1 204\r\nContent-Length: 0\r\n\r\n".to_vec());
        assert!(read_http_response(&mut c8, far).unwrap().is_empty(), "CL:0 = 空体（由上层按 NODATA 收）");
        let mut c9 = Cursor::new(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\nzz".to_vec());
        assert!(read_http_response(&mut c9, far).is_err(), "缺 CL 拒绝");
        let mut c10 = Cursor::new(b"HTTP/1.1 503 x\r\nContent-Length: 2\r\n\r\nno".to_vec());
        assert!(read_http_response(&mut c10, far).is_err());
    }

    /// r2#16/#13/#6：构造期坏项记行 + NODATA 文案（logf 收集断言）。
    #[test]
    fn from_list_and_nodata_log_lines() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let logf: crate::Logf = Arc::new(move |l: &str| {
            let _ = tx.send(l.to_owned());
        });
        let _r = DohResolver::from_list(parse_list("good@1.1.1.1, 坏项"), Arc::new(Mutex::new(None)), Arc::clone(&logf));
        let lines: Vec<String> = rx.try_iter().collect();
        assert!(lines.iter().any(|l| l.contains("1 项无法解析") && l.contains("坏项")), "{lines:?}");
        // 全无效 ⇒ ⚠️ 例外回退行
        let _r2 = DohResolver::from_list(parse_list("坏A, 坏B"), Arc::new(Mutex::new(None)), logf);
        let lines2: Vec<String> = rx.try_iter().collect();
        assert!(lines2.iter().any(|l| l.starts_with("⚠️ DoH：全部候选无效")), "{lines2:?}");
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
            parse_list("dns.alidns.com@223.5.5.5,dns.alidns.com@223.6.6.6").0,
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

    /// HTTP 响应头判定的纯函数面（r1#4 重写后从 query_one 抽出，可单测）。
    #[test]
    fn http_head_parse_forms() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\n\r\nabcd";
        let (st, len, ch) = http_head_facts(ok).unwrap();
        assert_eq!((st, len, ch), (200u16, Some(4usize), false));
        // 无 reason phrase 的状态行（r1#4：contains(" 200 ") 会误拒）
        let bare = b"HTTP/1.1 200\r\nContent-Length: 1\r\n\r\nx";
        assert_eq!(http_head_facts(bare).unwrap().0, 200);
        // 3xx/5xx 拒
        assert_eq!(http_head_facts(b"HTTP/1.1 503 x\r\nContent-Length: 2\r\n\r\nno").unwrap().0, 503);
        // chunked 显式识别
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc";
        assert!(http_head_facts(chunked).unwrap().2);
        // 大小写不敏感的头部名
        let ci = b"HTTP/1.1 200 OK\r\nCONTENT-LENGTH: 2\r\n\r\nab";
        assert_eq!(http_head_facts(ci).unwrap().1, Some(2));
        // 头部不完整
        assert!(http_head_facts(b"HTTP/1.1 200 OK\r\nContent-Len").is_none());
    }
}
