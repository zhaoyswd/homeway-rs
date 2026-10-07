//! 出口的 UPnP 端口映射（R3；语义真源 `internal/server/upnp.go`——从旧 tailcat fork
//! 移植的等价物）。
//!
//! 为什么不用现成的 portmapper 库：它们打分会校验 GetStatusInfo/GetExternalIPAddress
//! 并在「常见控制路径」上探测；家用路由器常有非标准实现（实测有 GetExternalIPAddress
//! 返回空、控制路径私有 /ctrlu/<uuid>/… 的机型），于是直接放弃且**不报错**。出口真正
//! 需要的只有两件事：SSDP 找到 IGD、AddPortMapping 把 UDP 端口映射出去；失败要明确
//! 打日志让人知道该去路由器上手动转发。
//!
//! ⚠️ **不可本地测登记（R3）**：SSDP 组播（M-SEARCH → 239.255.255.250:1900）在本地
//! 测试机上被 macOS 本地网络隐私静默拒（与现役出口 launchd 形态同款已知问题——
//! tier AGENTS「三台出口」节登记的 M-SEARCH no route to host）⇒ **真实网关判据不可
//! 本地采**。本模块的单测覆盖：SSDP 报文形状（字符串断言）、SOAP 生命周期
//! （本地 mock IGD——HTTP 面）、所有权分类与端口选择（内存面）。

use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, ToSocketAddrs as _, UdpSocket};
use std::os::fd::AsRawFd as _;
use std::time::Duration;

use super::egress::{interfaces, is_virtual_iface};

const SSDP_ADDR: &str = "239.255.255.250:1900";
const SSDP_ST: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:1";
/// 映射描述前缀（路由器表里「我们的映射」的认领判据）。
pub const UPNP_MAP_DESC: &str = "homeway-exit";
const UPNP_TIMEOUT: Duration = Duration::from_secs(5);
/// 公网端点路径的 UPnP 总预算（Go publicendpoint.go:141 的 40s ctx——SOAP/描述文件
/// 只受它约束，Go 无 per-SOAP 超时；M-SEARCH 内部 5s 与它取小）。**全局**预算：
/// 候选循环共享同一 deadline（F10，对齐 Go 单 ctx）。
pub const UPNP_TOTAL_BUDGET: Duration = Duration::from_secs(40);
/// 退出缩租路径的全局预算（Go serve.go:728-739 的 8s ctx——收工不被慢网关拖死；
/// F10：由「每候选 8s」改为**全部候选共享 8s**，且穿透到 SSDP 腿）。
pub const UPNP_SHRINK_TOTAL_BUDGET: Duration = Duration::from_secs(8);
/// 映射表枚举上限（listMappings 的 max）。
const LIST_MAX: usize = 200;
/// `http_call` 的分调用响应上限（Go `io.LimitReader` 同值：描述文件 1 MiB / SOAP 64 KiB）。
/// **超限行为不同**：Go 静默截断（会切掉 `>713<` 表尾判定体 ⇒ 静默改变映射表语义），
/// 本仓**报错**（`RespTooLarge`，诚实且可归因）——登记为已知口径差异。
const HTTP_MAX_DESC: usize = 1 << 20;
const HTTP_MAX_SOAP: usize = 1 << 16;
/// 响应头段上限（无 `\r\n\r\n` 时的独立界——家用路由器头段极小，防头段灌爆内存）。
const HTTP_MAX_HEADER: usize = 64 << 10;

/// upnp 错误面（R3-M23 类型化）：Display 文案与既有日志行**同串**（消费面 `{e}`
/// 透传；soap 错误串保留 body——list_mappings 的 713 表尾判定靠它）。
#[derive(Debug, thiserror::Error)]
pub enum UpnpError {
    /// 底层 IO / HTTP 非 2xx（body 上下文随 io::Error 文案保留）。
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// URL 非 http:// 形态。
    #[error("解析 URL {0:?}")]
    BadUrl(String),
    /// SSDP 三重试后无 IGD 响应（附最后一次错误串）。
    #[error("没有 IGD 响应（路由器未开 UPnP，或组播出不去）: {0}")]
    NoIgdResponse(String),
    /// 描述文件拉取失败（附底层错误串）。
    #[error("取描述文件: {0}")]
    DescFetch(String),
    /// 描述文件里没有 WAN 连接服务。
    #[error("描述文件里没有 WANIPConnection/WANPPPConnection 服务")]
    NoWanService,
    /// SOAP 调用失败（action + 底层错误串——保留 body 上下文）。
    #[error("{action}: {msg}")]
    Soap { action: String, msg: String },
    /// 候选本机地址全都没找到 IGD。
    #[error("没有可用的 IGD（候选 {candidates:?}）：{last}")]
    NoIgd { candidates: String, last: String },
    /// 路由器返回空的外部地址。
    #[error("路由器返回空的外部地址")]
    EmptyExternalIp,
    /// 外部地址非法（原文保留）。
    #[error("外部地址 {0:?} 非法")]
    BadExternalIp(String),
    /// 外部端口候选全部不可用（占用/拒绝计数）。
    #[error("外部端口候选均不可用（{occupied} 个被其它映射占用、{refused} 个被路由器拒绝）")]
    NoPortAvailable { occupied: usize, refused: usize },
    /// 回应（头段或正文）超过分调用上限（F7：Go `io.LimitReader` 是静默截断，本仓报错）。
    #[error("回应超过 {limit} 字节上限（UPnP 分调用体积闸：头段或正文）")]
    RespTooLarge { limit: usize },
    /// `Content-Length` 与实际收到的不一致（F7：新增严格度——非标机型可能不符）。
    #[error("Content-Length 不符：声明 {declared} 字节、实收 {got} 字节")]
    ContentLengthMismatch { declared: usize, got: usize },
    /// UPnP 调用总预算耗尽（F7/F10：绝对期限——滴流/黑洞对端不能永久挂住）。
    #[error("UPnP 调用预算耗尽（超时）")]
    BudgetExhausted,
}

/// M-SEARCH 报文（形状真源——ssdpLocation；`MX: 2` + IGD ST）。
pub fn msearch_message() -> String {
    format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {SSDP_ST}\r\n\r\n")
}

/// 一个可用的 WAN 连接服务（WANIPConnection / WANPPPConnection）。
#[derive(Clone)]
pub struct Igd {
    /// 本实例的操作截止（M11：soap/描述文件的 http_call 以剩余量收紧读/写超时；
    /// 缩租路径构造时传 8s，推断路径 40s——对齐 Go 两类 ctx）。
    deadline: std::time::Instant,
    control_url: String,
    service_type: String,
}

/// 路由器表里的一条映射（GetGenericPortMappingEntry）。
#[derive(Debug, Clone, PartialEq)]
pub struct UpnpMapping {
    pub external_port: u16,
    pub protocol: String,
    pub internal_port: u16,
    pub internal_client: String,
    pub enabled: bool,
    pub description: String,
    pub lease_duration: u32,
}

/// 一条既有映射相对本出口的归属判定结果（classify_mapping）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MappingOwner {
    /// 别人的（另一台出口/第三方/手动）——绝不动。
    Foreign,
    /// 我们自己的（陈旧或当前）——可安全「删了重建」。
    Ours,
    /// 同机器另一活实例的——不动。
    LiveSibling,
    /// 前缀像自己但内网客户端形态无法核验——归属不明（本轮 fail-open）。
    Unknown,
}

/// 所有权三元组（描述前缀 + 内网客户端地址 + 内网端口活监听）的单一实现——
/// CleanMappings / find_our_mapping / select_external_port 三处共用，防口径漂移。
pub fn classify_mapping(m: &UpnpMapping, desc_prefix: &str, client: Ipv4Addr, listen_port: u16) -> MappingOwner {
    if !m.description.starts_with(desc_prefix) {
        return MappingOwner::Foreign;
    }
    // 有些路由器把 NewInternalClient 报成主机名/空/带端口：归属不明——调用方不得
    // 当 foreign 让位（否则自己的映射每轮 +1 漂移到让尽），也不得当 ours 误删别人的。
    // ::ffff:a.b.c.d 的 v4-mapped 形态按其 v4 本值判（Go `Unmap()` 同义，评审 M10）。
    let parsed: Option<Ipv4Addr> = m
        .internal_client
        .parse::<Ipv4Addr>()
        .ok()
        .or_else(|| m.internal_client.parse::<std::net::Ipv6Addr>().ok().and_then(|v6| v6.to_ipv4_mapped()));
    let Some(c) = parsed else {
        return MappingOwner::Unknown;
    };
    if c != client {
        return MappingOwner::Foreign;
    }
    if m.internal_port != listen_port && udp_port_in_use(m.internal_port) {
        return MappingOwner::LiveSibling;
    }
    MappingOwner::Ours
}

/// 这台机器的这个 UDP 端口现在有人监听吗（区分「自己的陈旧映射」与「同机另一活
/// 出口的映射」——两者的描述前缀、内网地址完全一样，只有内网端口能分开）。
pub fn udp_port_in_use(port: u16) -> bool {
    if port == 0 {
        return false;
    }
    match UdpSocket::bind(("0.0.0.0", port)) {
        Ok(c) => {
            drop(c);
            false
        }
        Err(_) => true,
    }
}

// ---------- SSDP 发现 ----------

/// SSDP socket 的组播三件套（M8）：IP_MULTICAST_IF + TTL=2 + IP_BOUND_IF 钉卡。
/// 网卡定位 = 源地址所在的物理卡（egress::interfaces 按 IP 反查）。
fn pin_multicast(conn: &UdpSocket, local_ip: Ipv4Addr) -> Result<(), UpnpError> {
    let ifi = crate::server::egress::interfaces()
        .into_iter()
        .find(|i| i.addrs.contains(&local_ip));
    let fd = conn.as_raw_fd();
    // ① IP_MULTICAST_IF：值 = 本地接口地址（struct in_addr，网络序）
    let in_addr = libc::in_addr { s_addr: u32::from(local_ip).to_be() };
    let r = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_IF,
            &in_addr as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::in_addr>() as u32,
        )
    };
    if r != 0 {
        return Err(UpnpError::Io(std::io::Error::last_os_error()));
    }
    // ② IP_MULTICAST_TTL = 2
    let ttl: libc::c_int = 2;
    let r = unsafe {
        libc::setsockopt(
            fd,
            libc::IPPROTO_IP,
            libc::IP_MULTICAST_TTL,
            &ttl as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as u32,
        )
    };
    if r != 0 {
        return Err(UpnpError::Io(std::io::Error::last_os_error()));
    }
    // ③ 钉卡（网卡在系统表里找得到才钉——Go ifaceForIP 同义找不到就跳过）
    if let Some(ifi) = ifi {
        crate::server::egress::pin_socket_to_iface(fd, ifi.index, &ifi.name)
            .map_err(UpnpError::from)?;
    }
    Ok(())
}

/// SSDP 应答采纳判定（纯函数；F8）：①来源为**私网或环回** IPv4 单播（排除
/// unspecified/multicast/broadcast/公网）；②首行是 `HTTP/1.1 200` / `HTTP/1.0 200`；
/// ③`LOCATION` 存在且**非空**（对齐 Go `upnp.go:161` 的 `loc != ""`）。
/// 不满足者由调用方**继续读**（不返回、不报错）——同 LAN 上任何主机都能把出口的
/// UPnP 控制面指向任意地址（出口随后抓描述文件并发 SOAP = LAN 内 SSRF + 映射表扰动）。
/// **不加**「LOCATION 主机必须与候选 IP 同网段」：真机形态本地不可测，双网段/桥接
/// 机型有误杀风险 ⇒ 保守取三条件（公开地址 LAN 会漏配——已登记可回退）。
fn ssdp_response_ok(from: &std::net::SocketAddr, status_and_headers: &str) -> bool {
    let ip_ok = match from.ip() {
        std::net::IpAddr::V4(ip) => {
            (ip.is_private() || ip.is_loopback()) && !ip.is_unspecified() && !ip.is_multicast() && !ip.is_broadcast()
        }
        std::net::IpAddr::V6(_) => false,
    };
    if !ip_ok {
        return false;
    }
    let Some(first) = status_and_headers.lines().next() else { return false };
    let mut it = first.split_whitespace();
    let version_ok = matches!(it.next(), Some("HTTP/1.1") | Some("HTTP/1.0"));
    if !version_ok || it.next() != Some("200") {
        return false;
    }
    header_value(status_and_headers, "LOCATION").map(|v| !v.is_empty()).unwrap_or(false)
}

/// 发一次 SSDP M-SEARCH，取第一个 IGD 的 LOCATION（重试 3 次：家用路由器/交换机的
/// IGMP 收敛有几秒抖动）。local_ip 用于绑源地址（多网卡机器只有与路由器同网段的那张能用）。
///
/// F10：收 `deadline`（**全局**预算），内部期限 = `min(deadline, now + UPNP_TIMEOUT)`
/// ——修前 SSDP 腿自带 5s 独立期限、不受调用方预算约束（停机路径最坏 ≈ N×(5s+8s)）。
pub fn ssdp_location(local_ip: Option<Ipv4Addr>, deadline: std::time::Instant) -> Result<String, UpnpError> {
    let bind: std::net::SocketAddr = match local_ip {
        Some(ip) => SocketAddrV4::new(ip, 0).into(),
        None => "0.0.0.0:0".parse().expect("合法字面量"),
    };
    let conn = UdpSocket::bind(bind)?;
    // M8 三件套（Go upnp.go:122-127 同义）：指定 local_ip 时——
    // ① IP_MULTICAST_IF（组播出口钉在源地址所在网卡）
    // ② IP_MULTICAST_TTL=2（组播 TTL，默认 1 出不了本网段）
    // ③ IP_BOUND_IF/SO_BINDTODEVICE（整条 socket 钉卡——macOS 上光设
    //    IP_MULTICAST_IF 不够，默认路由被 TUN 型代理抢走时组播按默认路由选路
    //    直接 no route to host，实测 2026-09-19）。
    // 本地不可实测（SSDP 组播被 macOS 本地网络隐私拒——与现役出口 launchd 形态
    // 同款已知问题）：单测只证 setsockopt 调用成功，真机判据留 R7（登记进
    // INTEROP-CRITERIA）。
    if let Some(ip) = local_ip {
        pin_multicast(&conn, ip)?;
    }
    conn.set_read_timeout(Some(Duration::from_millis(1200)))?;
    let msg = msearch_message();
    let deadline = deadline.min(std::time::Instant::now() + UPNP_TIMEOUT);
    let mut last_err = String::new();
    let mut buf = [0u8; 4096];
    let mut attempt = 0;
    while attempt < 3 && std::time::Instant::now() < deadline {
        attempt += 1;
        if let Err(e) = conn.send_to(msg.as_bytes(), SSDP_ADDR) {
            // 发送失败重试（Go upnp.go:150-154——IGMP 收敛抖动下 sendto 偶发
            // no route to host，重发一次通常就通）
            last_err = format!("发送 M-SEARCH: {e}");
            let remain = deadline.saturating_duration_since(std::time::Instant::now());
            std::thread::sleep(remain.min(Duration::from_millis(500)));
            continue;
        }
        loop {
            let remain = deadline.saturating_duration_since(std::time::Instant::now());
            if remain.is_zero() {
                break;
            }
            // 每轮按剩余夹取（修前只设一次 1200ms ⇒ 到点后仍可再阻塞一整份）
            let _ = conn.set_read_timeout(Some(remain.min(Duration::from_millis(1200))));
            match conn.recv_from(&mut buf) {
                Ok((n, from)) => {
                    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
                    // F8：来源 + 状态行 + 非空 LOCATION 三条件（不满足继续读）
                    if ssdp_response_ok(&from, &text) {
                        if let Some(loc) = header_value(&text, "LOCATION") {
                            return Ok(loc);
                        }
                    }
                }
                Err(e) => {
                    last_err = e.to_string();
                    break; // 等下一次重试
                }
            }
        }
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        std::thread::sleep(remain.min(Duration::from_millis(500)));
    }
    Err(UpnpError::NoIgdResponse(last_err))
}

/// HTTP 头域取值（大小写不敏感；pub = fuzz 可达面——SSDP/HTTP 应答的自由文本面，
/// R3-H4 字节比较整改所在）。
pub fn header_value(resp: &str, key: &str) -> Option<String> {
    for line in resp.split('\n') {
        let line = line.trim();
        // **按字节比较**（评审 H4：`line[..key.len()]` 在多字节字符中间 panic——
        // LAN 内任何能发 UDP 到源端口的乱码报文都可远程打崩公网端点线程）
        let lb = line.as_bytes();
        if lb.len() > key.len() + 1
            && lb[..key.len()].eq_ignore_ascii_case(key.as_bytes())
            && lb[key.len()] == b':'
        {
            // 切值侧从字节界安全转回（trim 后的 ASCII 头域面恒为字节安全）
            return Some(line[key.len() + 1..].trim().to_owned());
        }
    }
    None
}

// ---------- 最小 HTTP 客户端（家用路由器的嵌入式 HTTP 服务很挑：显式
// Connection: close + UA + Accept-Encoding: identity——keep-alive/gzip 会被直接关连接） ----------

/// 最小 HTTP 客户端（家用路由器的嵌入式 HTTP 服务很挑：显式 Connection: close + UA +
/// Accept-Encoding: identity——keep-alive/gzip 会被直接关连接）。
///
/// F7：①分调用体积闸 `max_resp`（Go `io.LimitReader` 同值；**超限报错**而非静默截断）；
/// ②`Content-Length` 一致校验（声明 > 上限立即拒，读完实收 ≠ 声明报错）；③读/写**绝对
/// 期限**（成环读 + 每轮按剩余重设 ⇒ 滴流对端不能永久挂——缩租跑在停机主线程）；
/// ④拨号期限（`to_socket_addrs` + 逐地址 `connect_timeout`）。
fn http_call(
    url: &str,
    method: &str,
    content_type: Option<&str>,
    soap_action: Option<&str>,
    body: Option<&str>,
    deadline: Option<std::time::Instant>,
    max_resp: usize,
) -> Result<String, UpnpError> {
    let (host, port, path) = parse_http_url(url)?;
    // M11：读/写超时 = 剩余预算（Go 无 per-SOAP 超时、只受 ctx deadline——SOAP/描述
    // 文件给足 40s；M-SEARCH 的 5s 在 ssdp_location 自己的 deadline 里）。
    let deadline = deadline.unwrap_or_else(|| std::time::Instant::now() + UPNP_TOTAL_BUDGET);
    let remain = deadline.saturating_duration_since(std::time::Instant::now());
    if remain.is_zero() {
        return Err(UpnpError::BudgetExhausted);
    }
    let mut stream = connect_within(&host, port, deadline)?;
    stream.set_write_timeout(Some(remain))?;
    let mut req = format!("{method} {path} HTTP/1.1\r\nHOST: {host}:{port}\r\n");
    req.push_str("Connection: close\r\nUser-Agent: homeway/upnp\r\nAccept-Encoding: identity\r\n");
    if let Some(ct) = content_type {
        req.push_str(&format!("Content-Type: {ct}\r\n"));
    }
    if let Some(sa) = soap_action {
        req.push_str(&format!("SOAPAction: \"{sa}\"\r\n"));
    }
    let body_bytes = body.unwrap_or("");
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body_bytes.len()));
    req.push_str(body_bytes);
    stream.write_all(req.as_bytes())?;
    // 成环读（8KB 复用缓冲）：每轮按剩余重设读超时 + 体积闸（滴流防护 + 有界内存）
    let mut resp: Vec<u8> = Vec::new();
    let mut hdr_end: Option<usize> = None;
    let mut declared: Option<usize> = None;
    let mut buf = [0u8; 8192];
    loop {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            return Err(UpnpError::BudgetExhausted);
        }
        stream.set_read_timeout(Some(remain))?;
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                resp.extend_from_slice(&buf[..n]);
                if hdr_end.is_none() {
                    if let Some(i) = resp.windows(4).position(|w| w == b"\r\n\r\n") {
                        hdr_end = Some(i + 4);
                        let head = String::from_utf8_lossy(&resp[..i]).into_owned();
                        declared = header_value(&head, "Content-Length").and_then(|v| v.trim().parse::<usize>().ok());
                        if let Some(d) = declared {
                            if d > max_resp {
                                return Err(UpnpError::RespTooLarge { limit: max_resp });
                            }
                        }
                    } else if resp.len() > HTTP_MAX_HEADER {
                        // 头段本身灌爆（无 \r\n\r\n）：按头段上限拒
                        return Err(UpnpError::RespTooLarge { limit: HTTP_MAX_HEADER });
                    }
                }
                if let Some(he) = hdr_end {
                    // 体上限 = Go `io.LimitReader(resp.Body, max)` 同义（只盖正文）
                    if resp.len().saturating_sub(he) > max_resp {
                        return Err(UpnpError::RespTooLarge { limit: max_resp });
                    }
                    // M5：声明长度已收满即收工（Go 的 `io.ReadAll(resp.Body)` 在
                    // Content-Length 边界返 EOF）——忽略 `Connection: close` 的
                    // keep-alive 路由器不能把每次 SOAP 拖到预算耗尽
                    if let Some(d) = declared {
                        if resp.len().saturating_sub(he) >= d {
                            break;
                        }
                    }
                }
            }
            Err(e) => return Err(UpnpError::Io(e)),
        }
    }
    let text = String::from_utf8_lossy(&resp).into_owned();
    if let (Some(he), Some(d)) = (hdr_end, declared) {
        let got = resp.len().saturating_sub(he);
        if got != d {
            return Err(UpnpError::ContentLengthMismatch { declared: d, got });
        }
    }
    // 状态行校验（2xx 才算成）；非 2xx 的错误面带 body——SOAP Fault 里的 713（表尾
    // 标记）等业务码要由调用方从 body 判（Go soap 同形：body 随 error 返回）。
    if !text.starts_with("HTTP/1.1 2") && !text.starts_with("HTTP/1.0 2") {
        let body = match text.find("\r\n\r\n") {
            Some(i) => text[i + 4..].to_owned(),
            None => text.clone(),
        };
        let first = text.lines().next().unwrap_or("").to_owned();
        return Err(UpnpError::Io(std::io::Error::other(format!("HTTP 非 2xx：{first} body={body}"))));
    }
    // 去头
    Ok(match text.find("\r\n\r\n") {
        Some(i) => text[i + 4..].to_owned(),
        None => text,
    })
}

/// 带期限拨号（F7/N8）：`to_socket_addrs` + 逐地址 `connect_timeout(剩余)`；剩余为零
/// 即报预算耗尽（修前 `TcpStream::connect` 无超时 = OS 默认，黑洞网关可挂很久）。
fn connect_within(host: &str, port: u16, deadline: std::time::Instant) -> Result<TcpStream, UpnpError> {
    // 字面 IP 快路径（IGD 的 LOCATION 主机基本是字面 IP）：跳过 `to_socket_addrs`
    // 的系统解析——**域名解析本身没有可取消面**（残余登记：挂死的 LAN 主机名解析
    // 不受 deadline 约束；Go 的 ctx 下解析可取消）。
    if let Ok(ip) = host.parse::<std::net::IpAddr>() {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            return Err(UpnpError::BudgetExhausted);
        }
        return TcpStream::connect_timeout(&std::net::SocketAddr::new(ip, port), remain).map_err(UpnpError::Io);
    }
    let addrs = (host, port).to_socket_addrs().map_err(UpnpError::Io)?;
    let mut last: Option<std::io::Error> = None;
    for a in addrs {
        let remain = deadline.saturating_duration_since(std::time::Instant::now());
        if remain.is_zero() {
            return Err(UpnpError::BudgetExhausted);
        }
        match TcpStream::connect_timeout(&a, remain) {
            Ok(c) => return Ok(c),
            Err(e) => last = Some(e),
        }
    }
    Err(UpnpError::Io(last.unwrap_or_else(|| std::io::Error::new(std::io::ErrorKind::AddrNotAvailable, "无可用地址"))))
}

/// `http://host[:port]/path` 解析（pub = fuzz 可达面——LOCATION 头的自由文本面）。
pub fn parse_http_url(url: &str) -> Result<(String, u16, String), UpnpError> {
    let rest = url
        .strip_prefix("http://")
        .ok_or_else(|| UpnpError::BadUrl(url.to_owned()))?;
    let (hostport, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (h.to_owned(), p.parse().unwrap_or(80)),
        None => (hostport.to_owned(), 80),
    };
    Ok((host, port, path.to_owned()))
}

/// 解析 IGD 描述：找 WANIPConnection/WANPPPConnection 服务的 controlURL（device 树
/// 嵌套——按 `<service>` 块扫描：块内 serviceType 含 WAN 连接字样即命中）。
pub fn find_wan_service(desc: &str, base_url: &str, deadline: std::time::Instant) -> Option<Igd> {
    let mut off = 0usize;
    while let Some(start) = desc[start_off(off)..].find("<service>") {
        let abs_start = start_off(off) + start;
        let end = desc[abs_start..].find("</service>")? + abs_start;
        let block = &desc[abs_start..end];
        if let (Some(st), Some(cu)) = (xml_tag(block, "serviceType"), xml_tag(block, "controlURL")) {
            if st.contains("WANIPConnection") || st.contains("WANPPPConnection") {
                let control_url = join_url(base_url, &cu);
                return Some(Igd { deadline, control_url, service_type: st });
            }
        }
        off = end;
    }
    None
}

fn start_off(off: usize) -> usize {
    off
}

fn join_url(base: &str, path: &str) -> String {
    if path.starts_with("http://") || path.starts_with("https://") {
        return path.to_owned();
    }
    let (host, port, _) = match parse_http_url(base) {
        Ok(v) => v,
        Err(_) => return path.to_owned(),
    };
    if path.starts_with('/') {
        format!("http://{host}:{port}{path}")
    } else {
        format!("http://{host}:{port}/{path}")
    }
}

/// XML 最小取值（`<tag>…</tag>`；pub = fuzz 可达面——描述文件/SOAP body 的自由文本面）。
pub fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let i = body.find(&open)?;
    let j = body[i + open.len()..].find(&close)? + i + open.len();
    Some(body[i + open.len()..j].trim().to_owned())
}

/// SSDP 找到网关的 UPnP 描述并解析出 WAN 连接服务的控制地址（discoverIGD）。
/// `deadline` = **全局**预算（F10：穿透到 SSDP 腿——内部取 `min(deadline, 5s)`）。
pub fn discover_igd(local_ip: Ipv4Addr, deadline: std::time::Instant) -> Result<Igd, UpnpError> {
    let loc = ssdp_location(Some(local_ip), deadline)?;
    igd_from_location_deadline(&loc, deadline)
}

/// 已知 LOCATION 直取（mock 测试与 discover 共用）。
pub fn igd_from_location(loc: &str, budget: Duration) -> Result<Igd, UpnpError> {
    igd_from_location_deadline(loc, std::time::Instant::now() + budget)
}

fn igd_from_location_deadline(loc: &str, deadline: std::time::Instant) -> Result<Igd, UpnpError> {
    let body = http_call(loc, "GET", None, None, None, Some(deadline), HTTP_MAX_DESC)
        .map_err(|e| UpnpError::DescFetch(e.to_string()))?;
    find_wan_service(&body, loc, deadline).ok_or(UpnpError::NoWanService)
}

impl Igd {
    pub fn control_url(&self) -> &str {
        &self.control_url
    }

    /// SOAP 调用（Envelope 形态对齐 Go soap()）。
    pub fn soap(&self, action: &str, args: &[(&str, String)]) -> Result<String, UpnpError> {
        let mut b = String::new();
        b.push_str(r#"<?xml version="1.0"?><s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/""#);
        b.push_str(r#" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/"><s:Body><u:"#);
        b.push_str(action);
        b.push_str(r#" xmlns:u=""#);
        b.push_str(&self.service_type);
        b.push_str(r#"">"#);
        for (k, v) in args {
            b.push_str(&format!("<{k}>{v}</{k}>"));
        }
        b.push_str(&format!("</u:{action}></s:Body></s:Envelope>"));
        let sa = format!("{}#{}", self.service_type, action);
        http_call(
            &self.control_url,
            "POST",
            Some(r#"text/xml; charset="utf-8""#),
            Some(&sa),
            Some(&b),
            Some(self.deadline),
            HTTP_MAX_SOAP, // F7：Go `io.LimitReader` 同值（SOAP 64KiB）
        )
        .map_err(|e| {
            // 保留 body 上下文（list_mappings 的「到表尾」判定要读 713 形态——Display
            // 链 "{action}: {source}" 与原 format! 同串）
            UpnpError::Soap { action: action.to_owned(), msg: e.to_string() }
        })
    }

    /// 加映射（**先加后删**，F9）：先按 1 小时租期直接加；冲突（路由器报 718）时
    /// ——`allow_evict = true`（枚举完整且该端口在快照里明确属于我们）才 `delete` +
    /// 重试；否则**不动既有映射**、直接 Err（让位下一候选）。
    /// 租期失败退 0 的既有语义与日志保持（只接受 0 的机型退永久并**出声**）。
    pub fn add_port_mapping(
        &self,
        external_port: u16,
        internal_ip: Ipv4Addr,
        internal_port: u16,
        allow_evict: bool,
        logf: &dyn Fn(&str),
    ) -> Result<(), UpnpError> {
        match self.add_with_lease(external_port, internal_ip, internal_port, 3600) {
            Ok(()) => Ok(()),
            Err(e) if is_conflict(&e) => {
                if !allow_evict {
                    // 枚举残缺/归属不明，或该端口不属于我们：不删既有映射（Go 会强删 ⇒ 登记差异）
                    return Err(e);
                }
                // 快照明确属于我们 ⇒ 先删后加即幂等重建（Go 形态）
                let _ = self.delete_mapping(external_port, "UDP");
                match self.add_with_lease(external_port, internal_ip, internal_port, 3600) {
                    Ok(()) => Ok(()),
                    Err(e2) => self.lease_zero_fallback(external_port, internal_ip, internal_port, e2, logf),
                }
            }
            Err(e) => self.lease_zero_fallback(external_port, internal_ip, internal_port, e, logf),
        }
    }

    /// 租期回退（既有语义：部分路由器不接受非 0 租期 ⇒ 退永久并**出声**）。
    fn lease_zero_fallback(
        &self,
        external_port: u16,
        internal_ip: Ipv4Addr,
        internal_port: u16,
        first_err: UpnpError,
        logf: &dyn Fn(&str),
    ) -> Result<(), UpnpError> {
        if self.add_with_lease(external_port, internal_ip, internal_port, 0).is_ok() {
            logf(&format!(
                "UPnP：路由器不接受 1 小时租期（{first_err}），已按**永久（0）**写入映射 外部 {external_port}——该映射在本机异常退出后不会自动过期；正常退出会缩到 5 分钟"
            ));
            return Ok(());
        }
        Err(first_err)
    }

    fn add_with_lease(&self, external_port: u16, internal_ip: Ipv4Addr, internal_port: u16, lease: u32) -> Result<(), UpnpError> {
        self.soap(
            "AddPortMapping",
            &[
                ("NewRemoteHost", String::new()),
                ("NewExternalPort", external_port.to_string()),
                ("NewProtocol", "UDP".to_owned()),
                ("NewInternalPort", internal_port.to_string()),
                ("NewInternalClient", internal_ip.to_string()),
                ("NewEnabled", "1".to_owned()),
                ("NewPortMappingDescription", UPNP_MAP_DESC.to_owned()),
                ("NewLeaseDuration", lease.to_string()),
            ],
        )?;
        Ok(())
    }

    /// 删一条（不存在时路由器报 714——忽略语义由调用方按 body 判定）。
    pub fn delete_mapping(&self, external_port: u16, proto: &str) -> Result<(), UpnpError> {
        self.soap(
            "DeletePortMapping",
            &[
                ("NewRemoteHost", String::new()),
                ("NewExternalPort", external_port.to_string()),
                ("NewProtocol", proto.to_owned()),
            ],
        )?;
        Ok(())
    }

    /// 枚举路由器上的端口映射（F9a：一轮**只枚举一次**——调用方把结果封成
    /// `MappingTable` 快照，find/clean/select 全吃快照；此前一轮 3×(N+1) 次 SOAP 往返）。
    /// `complete=false` = 没见到表尾标记就到 max 条/读完了（清单残缺——不能拿
    /// 「清单里没有」当「路由器上没有」用）。
    pub fn list_mappings(&self) -> MappingTable {
        let mut out = Vec::new();
        for i in 0..LIST_MAX {
            match self.soap("GetGenericPortMappingEntry", &[("NewPortMappingIndex", i.to_string())]) {
                Err(e) => {
                    // 表尾标记只认**结构化形态**（评审 M9：错误串含整个 body——
                    // 裸 contains("713") 会把端口号 1713/序列号误判成表尾，破坏
                    // 「清单残缺 ⇒ fail-open」的保守防线）
                    let msg = e.to_string();
                    if msg.contains(">713<") || msg.contains("SpecifiedArrayIndexInvalid") {
                        return MappingTable { list: out, complete: true }; // 表尾标记
                    }
                    return MappingTable { list: out, complete: false };
                }
                Ok(body) => {
                    if body.contains("SpecifiedArrayIndexInvalid") || body.contains(">713<") {
                        return MappingTable { list: out, complete: true };
                    }
                    out.push(UpnpMapping {
                        external_port: xml_tag(&body, "NewExternalPort").and_then(|v| v.parse().ok()).unwrap_or(0),
                        protocol: xml_tag(&body, "NewProtocol").unwrap_or_default(),
                        internal_port: xml_tag(&body, "NewInternalPort").and_then(|v| v.parse().ok()).unwrap_or(0),
                        internal_client: xml_tag(&body, "NewInternalClient").unwrap_or_default(),
                        enabled: xml_tag(&body, "NewEnabled").as_deref() == Some("1"),
                        description: xml_tag(&body, "NewPortMappingDescription").unwrap_or_default(),
                        lease_duration: xml_tag(&body, "NewLeaseDuration").and_then(|v| v.parse().ok()).unwrap_or(0),
                    });
                }
            }
        }
        MappingTable { list: out, complete: false } // 走满 max 条仍没见表尾：按截断处理（保守）
    }

    /// GetExternalIPAddress（部分路由器返回空——调用方自己兜底）。
    pub fn external_ip(&self) -> Result<Ipv4Addr, UpnpError> {
        let body = self.soap("GetExternalIPAddress", &[])?;
        let raw = xml_tag(&body, "NewExternalIPAddress").unwrap_or_default();
        if raw.is_empty() {
            return Err(UpnpError::EmptyExternalIp);
        }
        raw.parse().map_err(|_| UpnpError::BadExternalIp(raw))
    }

    /// 表里找「我们自己的」映射（同描述前缀 + 同内网客户端；内网端口指着别的活实例的
    /// 不算）。返回顺序偏好：外部端口 == 监听端口 → 表里第一条我们的。吃**枚举快照**
    /// （F9a：一轮只枚举一次）。
    pub fn find_our_mapping(&self, table: &MappingTable, desc_prefix: &str, client: Ipv4Addr, listen_port: u16) -> Option<(u16, u16)> {
        let mut fallback = None;
        for m in &table.list {
            if m.protocol != "UDP" || classify_mapping(m, desc_prefix, client, listen_port) != MappingOwner::Ours {
                continue;
            }
            if m.external_port == listen_port {
                return Some((m.external_port, m.internal_port));
            }
            if fallback.is_none() {
                fallback = Some((m.external_port, m.internal_port));
            }
        }
        fallback
    }

    /// 删掉**我们自己的**映射（同前缀 + 同内网客户端；内网端口指着别的活实例的不删）。
    /// 返回删除条数。`skip_external_port` = 本轮 `prefer`（F9：候选申请成功前**不删**
    /// ——这才是真正的轮级「先加后删」；修前 clean 会连刚选为 prefer 的那条一起删掉）。
    /// 删成功的条目从快照剔除（后续 select 看到的是「路由器现状」）。
    pub fn clean_mappings(
        &self,
        table: &mut MappingTable,
        desc_prefix: &str,
        client: Ipv4Addr,
        keep_internal_port: u16,
        skip_external_port: Option<u16>,
        logf: &dyn Fn(&str),
    ) -> usize {
        let mut n = 0;
        let mut kept: Vec<UpnpMapping> = Vec::with_capacity(table.list.len());
        for m in table.list.drain(..) {
            if Some(m.external_port) == skip_external_port {
                kept.push(m); // prefer：候选申请成功前不删（轮级先加后删）
                continue;
            }
            match classify_mapping(&m, desc_prefix, client, keep_internal_port) {
                MappingOwner::Ours => {
                    if self.delete_mapping(m.external_port, &m.protocol).is_ok() {
                        n += 1;
                    } else {
                        kept.push(m);
                    }
                }
                MappingOwner::LiveSibling => {
                    logf(&format!(
                        "UPnP：外部 {} 的映射指向本机另一活实例（内网端口 {} 在监听），不清",
                        m.external_port, m.internal_port
                    ));
                    kept.push(m);
                }
                _ => kept.push(m),
            }
        }
        table.list = kept;
        n
    }

    /// 用很短的租期重建同一条映射（退出时缩租——快速重启能沿用同一个公网端口，
    /// 出口真退休了映射自动过期）。F9：**先 add(lease) → 718 才 delete + add(lease)**
    /// ——调用方已核验该映射属于我们（`allow_evict = true`）；加不回去时原映射仍在。
    pub fn re_add_short_lease(&self, ext_port: u16, internal_ip: Ipv4Addr, internal_port: u16, lease: u32) -> Result<(), UpnpError> {
        match self.add_with_lease(ext_port, internal_ip, internal_port, lease) {
            Ok(()) => Ok(()),
            Err(e) if is_conflict(&e) => {
                let _ = self.delete_mapping(ext_port, "UDP");
                self.add_with_lease(ext_port, internal_ip, internal_port, lease)
            }
            Err(e) => Err(e),
        }
    }
}

/// 冲突（路由器报 718 `ConflictInMappingEntry`）判定——只认结构化形态（同 713 表尾
/// 的判定纪律：裸 `contains("718")` 会被端口号/序列号误判）。
fn is_conflict(e: &UpnpError) -> bool {
    let msg = e.to_string();
    msg.contains(">718<") || msg.contains("ConflictInMappingEntry")
}

/// 一次枚举的**快照**（F9a：一轮只枚举一次，find/clean/select 全吃它）。
pub struct MappingTable {
    list: Vec<UpnpMapping>,
    complete: bool,
}

impl MappingTable {
    /// 本轮是否可做所有权核验（枚举完整 + 无归属不明条目）；不满足时打归因行并
    /// fail-open（把它当 foreign 会让自己的映射每轮 +1 漂移直到候选让尽）。
    fn verifiable(&self, desc_prefix: &str, local_ip: Ipv4Addr, internal_port: u16, logf: &dyn Fn(&str)) -> bool {
        if !self.complete {
            logf(&format!(
                "UPnP：映射表枚举不完整（{} 条未见表尾标记，表超上限或路由器漏报），本轮跳过所有权核验——申请时不删既有映射（让位优先）",
                self.list.len()
            ));
            return false;
        }
        for m in self.list.iter().filter(|m| m.protocol == "UDP") {
            if classify_mapping(m, desc_prefix, local_ip, internal_port) == MappingOwner::Unknown {
                logf(&format!(
                    "UPnP：映射表中存在归属不明的条目（外部 {} 客户端={:?} desc={}，路由器客户端字段形态异常），本轮跳过所有权核验——申请时不删既有映射（让位优先）",
                    m.external_port, m.internal_client, m.description
                ));
                return false;
            }
        }
        true
    }
}

/// 候选循环（F10：**全局期限**——`discover(cand, deadline)` 由调用方注入（可测），
/// 每轮按剩余收窄，到点即止）。修前每候选各起一份预算（缩租 N×(5s+8s)、公网端点
/// N×45s）⇒ 停机路径被慢网关拖死（launchd ExitTimeOut 20s 内会 SIGKILL）。
pub(crate) fn pick_igd_before<F>(cands: &[Ipv4Addr], deadline: std::time::Instant, discover: F) -> Result<(Igd, Ipv4Addr), UpnpError>
where
    F: Fn(Ipv4Addr, std::time::Instant) -> Result<Igd, UpnpError>,
{
    let mut last_err = String::new();
    for cand in cands {
        if std::time::Instant::now() >= deadline {
            last_err = "总预算耗尽".to_owned();
            break;
        }
        match discover(*cand, deadline) {
            Ok(g) => return Ok((g, *cand)),
            Err(e) => last_err = e.to_string(),
        }
    }
    Err(UpnpError::NoIgd { candidates: format!("{cands:?}"), last: last_err })
}

/// 退出缩租的结果（F10：显式返回 ⇒ engine 侧按结果打 additive 日志，静默跳过变可见）。
#[derive(Debug)]
pub enum ShrinkOutcome {
    /// 候选里没找到可用 IGD。
    NoIgd,
    /// 找到 IGD 但映射表枚举残缺（清单不可信——不当作"没有映射"）。
    TableIncomplete,
    /// 找到 IGD 且表完整，但没有我们的映射。
    NoMapping,
    /// 缩租成功（外部端口）。
    Shrunk { ext: u16 },
    /// 缩租失败（原映射保持原租期；附类型化归因）。
    Failed { ext: u16, err: UpnpError },
}

/// 缩租（shrinkUPnPClease 的纯逻辑面）：**全局 8s 预算**贯穿候选与 SSDP/描述腿。
pub fn shrink_lease_before(cands: &[Ipv4Addr], listen_port: u16, deadline: std::time::Instant) -> ShrinkOutcome {
    let Ok((g, ip)) = pick_igd_before(cands, deadline, discover_igd) else {
        return ShrinkOutcome::NoIgd;
    };
    let table = g.list_mappings();
    let Some((ext, internal)) = g.find_our_mapping(&table, UPNP_MAP_DESC, ip, listen_port) else {
        return if table.complete { ShrinkOutcome::NoMapping } else { ShrinkOutcome::TableIncomplete };
    };
    match g.re_add_short_lease(ext, ip, internal, 300) {
        Ok(()) => ShrinkOutcome::Shrunk { ext },
        Err(e) => ShrinkOutcome::Failed { ext, err: e },
    }
}

/// 为 internal_port 申请一个外部端口（ensurePortMapping + selectExternalPort）：
/// 端口选择顺序 = 上次成功的外部端口（路由器表认领——**权威记忆**，不需要本地文件）
/// → 与监听端口同号 → +1…+9；每个候选先核验所有权（别人的/活实例的让位）。
/// **全局 40s 预算**（F10，对齐 Go 单 ctx）。
pub fn ensure_port_mapping(
    candidates: &[Ipv4Addr],
    internal_port: u16,
    logf: &dyn Fn(&str),
    dlogf: &dyn Fn(&str),
    deadline: std::time::Instant,
) -> Result<(u16, Ipv4Addr), UpnpError> {
    let (g, local_ip) = pick_igd_before(candidates, deadline, discover_igd)?;
    let ext = mapping_round(&g, internal_port, local_ip, logf, dlogf)?;
    Ok((ext, local_ip))
}

/// 一轮的完整动作（F9a：**单一枚举**——快照 → 找我们的（prefer）→ 清旧（跳过 prefer）
/// → 选端口申请）。`ensure_port_mapping` 与单测共用（「枚举一次」的断言能真正打到
/// 生产轮次，而不是只测拼装出的等价代码）。
fn mapping_round(
    g: &Igd,
    internal_port: u16,
    local_ip: Ipv4Addr,
    logf: &dyn Fn(&str),
    dlogf: &dyn Fn(&str),
) -> Result<u16, UpnpError> {
    // F9a：一轮只枚举一次（快照）；verify 计算一次并传给 select（fail-open 归因行只打一次）
    let mut table = g.list_mappings();
    let verify = table.verifiable(UPNP_MAP_DESC, local_ip, internal_port, logf);
    let (prefer, prev_internal) = match g.find_our_mapping(&table, UPNP_MAP_DESC, local_ip, internal_port) {
        Some((ext, int)) => {
            if ext != internal_port {
                logf(&format!("UPnP：路由器上已有我们的映射（外部 {ext} → 内网 {int}），优先沿用外部端口 {ext}"));
            } else {
                logf(&format!("UPnP：路由器上已有我们的映射 外部 {ext} → 内网 {int}，直接续用"));
            }
            (ext, int)
        }
        None => (0, 0),
    };
    let _ = prev_internal;
    // 轮级先加后删（F9/T1）：clean 跳过 prefer —— 候选申请成功前不删我们当前的映射
    let n = g.clean_mappings(&mut table, UPNP_MAP_DESC, local_ip, internal_port, Some(prefer), dlogf);
    if n > 0 {
        dlogf(&format!("UPnP：清掉 {n} 条本机同前缀的旧映射（换端口或上次退出的遗留）"));
    }
    select_external_port(g, &table, internal_port, prefer, local_ip, verify, logf)
}

/// 候选端口逐个申请（**先加后删** + 所有权门 `allow_evict`；映射表枚举失败/残缺/含
/// 归属不明条目时 fail-open 直接申请——把它当 foreign 会让自己的映射每轮 +1 漂移直到
/// 候选让尽）。`allow_evict = verify && 快照中该 ext 条目存在且 classify == Ours`。
pub fn select_external_port(
    g: &Igd,
    table: &MappingTable,
    internal_port: u16,
    prefer: u16,
    local_ip: Ipv4Addr,
    verify: bool,
    logf: &dyn Fn(&str),
) -> Result<u16, UpnpError> {
    let taken: std::collections::HashMap<u16, &UpnpMapping> = table
        .list
        .iter()
        .filter(|m| m.protocol == "UDP")
        .map(|m| (m.external_port, m))
        .collect();
    // 候选去重（F9c，Go `tried` 同义）：prefer → internal_port → +1…+9，同值只试一次
    let mut cands: Vec<u16> = Vec::new();
    let push = |c: u16, cands: &mut Vec<u16>| {
        if c != 0 && !cands.contains(&c) {
            cands.push(c);
        }
    };
    push(prefer, &mut cands);
    for i in 0..=9u32 {
        let c = internal_port as u32 + i; // u32 计算：u16 直接加会在 >65526 时回绕到特权端口
        if c > 0xFFFF {
            break;
        }
        push(c as u16, &mut cands);
    }
    let mut occupied = 0usize;
    let mut refused = 0usize;
    let mut first_occupant = String::new();
    for ext in cands {
        let mut allow_evict = false;
        if verify {
            if let Some(m) = taken.get(&ext) {
                match classify_mapping(m, UPNP_MAP_DESC, local_ip, internal_port) {
                    MappingOwner::Foreign => {
                        occupied += 1;
                        if first_occupant.is_empty() {
                            first_occupant = format!("{}（desc={}）", m.internal_client, m.description);
                        }
                        logf(&format!(
                            "UPnP：外部端口 {} 已被 {} 的映射占用（desc={}），让位",
                            ext, m.internal_client, m.description
                        ));
                        continue;
                    }
                    MappingOwner::LiveSibling => {
                        occupied += 1;
                        if first_occupant.is_empty() {
                            first_occupant = format!("本机另一活实例（内网端口 {} 在监听）", m.internal_port);
                        }
                        logf(&format!("UPnP：外部端口 {} 的映射指向本机另一活实例（内网端口 {} 在监听），让位", ext, m.internal_port));
                        continue;
                    }
                    MappingOwner::Ours => allow_evict = true, // 快照明确属于我们：718 时才删
                    MappingOwner::Unknown => {}               // verify=true 时不可能（上面已核验）
                }
            }
        }
        match g.add_port_mapping(ext, local_ip, internal_port, allow_evict, logf) {
            Ok(()) => {
                if ext == prefer && prefer != internal_port {
                    logf(&format!("UPnP：沿用上次成功的外部端口 {ext} → 内网 {internal_port}"));
                } else if occupied > 0 {
                    logf(&format!(
                        "UPnP：让位 {occupied} 个候选（首个占用者 {first_occupant}），最终取得外部端口 {ext} → 内网 {internal_port}"
                    ));
                }
                return Ok(ext);
            }
            Err(e) => {
                refused += 1;
                if !allow_evict && is_conflict(&e) {
                    logf(&format!("UPnP：外部端口 {ext} 已被占用且归属未经核验（{e}），不删既有映射，换下一个候选"));
                } else {
                    logf(&format!("UPnP：外部端口 {ext} 申请失败（{e}），换下一个候选"));
                }
            }
        }
    }
    Err(UpnpError::NoPortAvailable { occupied, refused })
}

/// 本机可能用于 UPnP 的内网 IPv4 候选（过滤回环/虚拟网卡与公网地址——真正判据仍是
/// 「谁能联系上路由器」（SSDP 自校验））。
pub fn local_ipv4_candidates() -> Vec<Ipv4Addr> {
    let mut out = Vec::new();
    for ifi in interfaces() {
        if !ifi.up || ifi.loopback || is_virtual_iface(&ifi.name) {
            continue;
        }
        for a in ifi.addrs {
            if !a.is_private() {
                continue;
            }
            out.push(a);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// M-SEARCH 报文形状（SSDP 真网关判据不可本地测——形状钉死 + mock HTTP 生命周期）。
    #[test]
    fn msearch_shape() {
        let m = msearch_message();
        assert!(m.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(m.contains("HOST: 239.255.255.250:1900\r\n"));
        assert!(m.contains("MAN: \"ssdp:discover\"\r\n"));
        assert!(m.contains("MX: 2\r\n"));
        assert!(m.contains(&format!("ST: {SSDP_ST}\r\n")));
        assert!(m.ends_with("\r\n\r\n"));
    }

    /// M8 三件套：组播 socket option 在真实网卡 IP 上调用成功（只证 setsockopt
    /// 返回值——组播收发面不可本地测，真机判据见 INTEROP-CRITERIA 登记条目）。
    /// 机器无物理 IPv4 卡（纯离线 CI 形态）时跳过。
    #[test]
    fn multicast_pin_options_apply() {
        let Some(ifi) = crate::server::egress::physical_candidates().into_iter().find(|i| !i.addrs.is_empty()) else {
            eprintln!("（无物理 IPv4 网卡——M8 单测跳过 setsockopt 断言）");
            return;
        };
        let ip = ifi.addrs[0];
        let conn = UdpSocket::bind(SocketAddrV4::new(ip, 0)).expect("绑源地址");
        pin_multicast(&conn, ip).expect("三件套 setsockopt 应全部成功（真实网卡 IP）");
    }

    /// headerValue（大小写不敏感 + 冒号后取值）。
    #[test]
    fn header_value_parse() {
        let resp = "HTTP/1.1 200 OK\r\nCACHE-CONTROL: max-age=5\r\nLOCATION: http://192.168.3.1:49152/root.xml\r\n\r\n";
        assert_eq!(header_value(resp, "LOCATION").as_deref(), Some("http://192.168.3.1:49152/root.xml"));
        assert_eq!(header_value(resp, "location").as_deref(), Some("http://192.168.3.1:49152/root.xml"));
        assert_eq!(header_value(resp, "ST"), None);
    }

    /// 描述文件解析：WANIPConnection 命中 + controlURL 拼接。
    #[test]
    fn description_parse() {
        let desc = r#"<?xml version="1.0"?><root><device><serviceList><service>
<serviceType>urn:schemas-upnp-org:service:WANCommonInterfaceConfig:1</serviceType>
<controlURL>/ctrlu/common</controlURL>
</service><service>
<serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType>
<controlURL>/ctrlu/wanip</controlURL>
</service></serviceList></device></root>"#;
        let g = find_wan_service(desc, "http://192.168.3.1:49152/root.xml", std::time::Instant::now() + UPNP_TOTAL_BUDGET).expect("应命中");
        assert_eq!(g.control_url(), "http://192.168.3.1:49152/ctrlu/wanip");
        // 绝对 URL 原样
        let g2 = find_wan_service(desc.replace("/ctrlu/wanip", "http://10.0.0.2:80/ctrlu").as_str(), "http://192.168.3.1/root.xml", std::time::Instant::now() + UPNP_TOTAL_BUDGET).unwrap();
        assert_eq!(g2.control_url(), "http://10.0.0.2:80/ctrlu");
        // 无 WAN 服务
        assert!(find_wan_service("<root/>", "http://1.2.3.4/", std::time::Instant::now() + UPNP_TOTAL_BUDGET).is_none());
    }

    /// mock IGD：AddPortMapping/DeletePortMapping/GetGenericPortMappingEntry 生命周期 +
    /// find_our_mapping / clean_mappings 所有权语义（真实网关判据不可本地测——mock 面）。
    #[test]
    fn soap_lifecycle_against_mock() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let entries: Arc<std::sync::Mutex<Vec<UpnpMapping>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
        let calls = Arc::new(AtomicUsize::new(0));
        let e2 = Arc::clone(&entries);
        let c2 = Arc::clone(&calls);
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                c2.fetch_add(1, Ordering::SeqCst);
                let mut c = conn;
                let mut buf = Vec::new();
                // 按头读齐（Content-Length 体跟读——read_to_end 会与客户端等响应互等死锁）
                let mut byte = [0u8; 1];
                loop {
                    match c.read(&mut byte) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            buf.push(byte[0]);
                            if buf.ends_with(b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf).into_owned();
                                let cl = header_value(&head, "Content-Length")
                                    .and_then(|v| v.parse::<usize>().ok())
                                    .unwrap_or(0);
                                let mut body = vec![0u8; cl];
                                if cl > 0 {
                                    let _ = c.read_exact(&mut body);
                                }
                                buf.extend_from_slice(&body);
                                break;
                            }
                        }
                    }
                }
                let req = String::from_utf8_lossy(&buf).into_owned();
                if req.starts_with("GET ") {
                    let body = r#"<root><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><controlURL>/ctrlu</controlURL></service></serviceList></device></root>"#;
                    let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).as_bytes());
                    continue;
                }
                // SOAP 分派
                let e = e2.lock().unwrap();
                if req.contains("AddPortMapping") {
                    let ext = xml_tag(&req, "NewExternalPort").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let int_p = xml_tag(&req, "NewInternalPort").and_then(|v| v.parse().ok()).unwrap_or(0);
                    let client = xml_tag(&req, "NewInternalClient").unwrap_or_default();
                    let lease = xml_tag(&req, "NewLeaseDuration").and_then(|v| v.parse().ok()).unwrap_or(0);
                    drop(e);
                    let mut e = e2.lock().unwrap();
                    e.retain(|m| !(m.external_port == ext && m.protocol == "UDP"));
                    e.push(UpnpMapping {
                        external_port: ext,
                        protocol: "UDP".to_owned(),
                        internal_port: int_p,
                        internal_client: client,
                        enabled: true,
                        description: UPNP_MAP_DESC.to_owned(),
                        lease_duration: lease,
                    });
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
                } else if req.contains("DeletePortMapping") {
                    let ext = xml_tag(&req, "NewExternalPort").and_then(|v| v.parse().ok()).unwrap_or(0);
                    drop(e);
                    e2.lock().unwrap().retain(|m| m.external_port != ext || m.protocol != "UDP");
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
                } else if req.contains("GetGenericPortMappingEntry") {
                    let idx: usize = xml_tag(&req, "NewPortMappingIndex").and_then(|v| v.parse().ok()).unwrap_or(0);
                    if idx >= e.len() {
                        let fault = r#"<s:Fault><detail><UPnPError><errorCode>713</errorCode></UPnPError></detail></s:Fault>"#;
                        let _ = c.write_all(format!("HTTP/1.1 500 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", fault.len(), fault).as_bytes());
                    } else {
                        let m = &e[idx];
                        let body = format!(
                            "<m><NewExternalPort>{}</NewExternalPort><NewProtocol>{}</NewProtocol><NewInternalPort>{}</NewInternalPort><NewInternalClient>{}</NewInternalClient><NewEnabled>1</NewEnabled><NewPortMappingDescription>{}</NewPortMappingDescription><NewLeaseDuration>{}</NewLeaseDuration></m>",
                            m.external_port, m.protocol, m.internal_port, m.internal_client, m.description, m.lease_duration
                        );
                        let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).as_bytes());
                    }
                } else {
                    let _ = c.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n");
                }
            }
        });
        let base = format!("http://127.0.0.1:{port}/root.xml");
        let g = igd_from_location(&base, UPNP_TOTAL_BUDGET).expect("mock IGD 应可发现");
        let client: Ipv4Addr = "192.168.3.12".parse().unwrap();
        let logf = |_: &str| {};

        // 加映射 → 表里可见（allow_evict=true：快照已核验属于我们）
        g.add_port_mapping(42641, client, 42641, true, &logf).unwrap();
        let table = g.list_mappings();
        assert!(table.complete, "到表尾");
        assert_eq!(table.list.len(), 1);
        assert_eq!(table.list[0].external_port, 42641);
        assert_eq!(table.list[0].lease_duration, 3600, "1 小时租期优先");

        // find_our_mapping：同号命中
        assert_eq!(g.find_our_mapping(&table, UPNP_MAP_DESC, client, 42641), Some((42641, 42641)));
        // 换内网客户端 = 别人的
        let other: Ipv4Addr = "192.168.3.99".parse().unwrap();
        assert_eq!(g.find_our_mapping(&table, UPNP_MAP_DESC, other, 42641), None);

        // select_external_port：prefer 沿用（verify=true，条目 Ours ⇒ allow_evict）
        let ext = select_external_port(&g, &table, 42641, 42641, client, true, &logf).unwrap();
        assert_eq!(ext, 42641);

        // clean：自己的清掉（skip=None）
        let mut table2 = table;
        let n = g.clean_mappings(&mut table2, UPNP_MAP_DESC, client, 42641, None, &logf);
        assert_eq!(n, 1);
        assert!(table2.list.is_empty(), "删成功的条目从快照剔除");

        // re_add_short_lease（退出缩租：先 add → 本 mock 无 718 语义 ⇒ 一次 add 即成）
        g.add_port_mapping(42650, client, 42641, true, &logf).unwrap();
        g.re_add_short_lease(42650, client, 42641, 300).unwrap();
        let list3 = g.list_mappings();
        assert_eq!(list3.list[0].lease_duration, 300);
        assert!(calls.load(Ordering::SeqCst) > 0);
    }

    /// 所有权分类（纯函数面）：前缀/客户端/活监听三元组。
    #[test]
    fn classify_ownership() {
        let m = |desc: &str, client: &str, int_port: u16| UpnpMapping {
            external_port: 1,
            protocol: "UDP".into(),
            internal_port: int_port,
            internal_client: client.into(),
            enabled: true,
            description: desc.into(),
            lease_duration: 0,
        };
        let me: Ipv4Addr = "192.168.3.12".parse().unwrap();
        assert_eq!(classify_mapping(&m(UPNP_MAP_DESC, "192.168.3.12", 42641), UPNP_MAP_DESC, me, 42641), MappingOwner::Ours);
        // 前缀不同 = 别人的
        assert_eq!(classify_mapping(&m("other-tool", "192.168.3.12", 42641), UPNP_MAP_DESC, me, 42641), MappingOwner::Foreign);
        // 内网客户端不同 = 别人的
        assert_eq!(classify_mapping(&m(UPNP_MAP_DESC, "192.168.3.99", 42641), UPNP_MAP_DESC, me, 42641), MappingOwner::Foreign);
        // 内网端口形态异常 = 归属不明
        assert_eq!(classify_mapping(&m(UPNP_MAP_DESC, "hostname-lan", 42641), UPNP_MAP_DESC, me, 42641), MappingOwner::Unknown);
    }

    // ---------- F7 http_call：分调用上限 / 长度一致 / 绝对期限 / 拨号期限 ----------

    /// 脚本化 HTTP mock（每个连接一份响应脚本；`delay` 逐块注入实现滴流）。
    fn http_mock(script: Vec<(Vec<u8>, Duration)>) -> u16 {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut c = conn;
                // 读掉请求（头 + Content-Length 体）
                let mut buf = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    match c.read(&mut byte) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            buf.push(byte[0]);
                            if buf.ends_with(b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf).into_owned();
                                let cl = header_value(&head, "Content-Length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                                let mut body = vec![0u8; cl];
                                if cl > 0 {
                                    let _ = c.read_exact(&mut body);
                                }
                                break;
                            }
                        }
                    }
                }
                for (chunk, delay) in &script {
                    if !delay.is_zero() {
                        std::thread::sleep(*delay);
                    }
                    if c.write_all(chunk).is_err() {
                        break;
                    }
                    let _ = c.flush();
                }
            }
        });
        port
    }

    #[test]
    fn http_call_cap_length_and_deadline() {
        // ① 正常（200 + 长度相符）⇒ Ok（返回去头正文）
        let port = http_mock(vec![(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\n\r\nhello".to_vec(), Duration::ZERO)]);
        let url = format!("http://127.0.0.1:{port}/x");
        let body = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_secs(3)), HTTP_MAX_SOAP).unwrap();
        assert_eq!(body, "hello");
        // ② 声明超上限（8 MiB）只发头 ⇒ 立即 Err（不等体）
        let port = http_mock(vec![(b"HTTP/1.1 200 OK\r\nContent-Length: 8388608\r\n\r\n".to_vec(), Duration::ZERO)]);
        let url = format!("http://127.0.0.1:{port}/x");
        let t0 = std::time::Instant::now();
        let e = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_secs(3)), HTTP_MAX_SOAP).unwrap_err();
        assert!(matches!(e, UpnpError::RespTooLarge { .. }), "应报超限：{e}");
        assert!(t0.elapsed() < Duration::from_millis(500), "声明超限应立拒（实 {:?}）", t0.elapsed());
        // ③ 声明 100 实发 50（连接关闭）⇒ 长度不符
        let port = http_mock(vec![(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n".to_vec(), Duration::ZERO), (vec![b'x'; 50], Duration::ZERO)]);
        let url = format!("http://127.0.0.1:{port}/x");
        let e = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_secs(3)), HTTP_MAX_SOAP).unwrap_err();
        assert!(matches!(e, UpnpError::ContentLengthMismatch { declared: 100, got: 50 }), "应报长度不符：{e}");
        // ④ 滴流（每 200ms 1 字节）⇒ 绝对期限到点崩（修前可无限挂）
        let mut drip: Vec<(Vec<u8>, Duration)> = vec![(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n".to_vec(), Duration::ZERO)];
        for _ in 0..1000 {
            drip.push((vec![b'y'], Duration::from_millis(100))); // 逐字节滴流
        }
        let port = http_mock(drip);
        let url = format!("http://127.0.0.1:{port}/x");
        let t0 = std::time::Instant::now();
        let e = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_millis(300)), HTTP_MAX_SOAP).unwrap_err();
        let dt = t0.elapsed();
        assert!(matches!(e, UpnpError::BudgetExhausted | UpnpError::Io(_)), "滴流应按期限崩：{e}");
        assert!(dt < Duration::from_secs(1), "滴流须按期返回（实 {dt:?}）");
        // ⑤ 黑洞地址 + 短期限 ⇒ 拨号受界
        let t0 = std::time::Instant::now();
        let e = http_call("http://192.0.2.1:80/x", "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_millis(200)), HTTP_MAX_SOAP).unwrap_err();
        let dt = t0.elapsed();
        assert!(matches!(e, UpnpError::BudgetExhausted | UpnpError::Io(_)), "黑洞应报错：{e}");
        assert!(dt < Duration::from_millis(600), "拨号须受界（实 {dt:?}）");
        // ⑤' 无 Content-Length + 超大正文（70 KiB > SOAP 64 KiB）⇒ 超限拒
        let port = http_mock(vec![
            (b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_vec(), Duration::ZERO),
            (vec![b'q'; 70 * 1024], Duration::ZERO),
        ]);
        let url = format!("http://127.0.0.1:{port}/x");
        let e = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_secs(3)), HTTP_MAX_SOAP).unwrap_err();
        assert!(matches!(e, UpnpError::RespTooLarge { .. }), "无长度声明的超大正文应被体积闸拦下：{e}");
        // ⑤'' 头段灌爆（无 \r\n\r\n）⇒ 头段上限拒
        let port = http_mock(vec![(b"HTTP/1.1 200 OK\r\nX: ".to_vec(), Duration::ZERO), (vec![b'h'; HTTP_MAX_HEADER + 1024], Duration::ZERO)]);
        let url = format!("http://127.0.0.1:{port}/x");
        let e = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_secs(3)), HTTP_MAX_SOAP).unwrap_err();
        assert!(matches!(e, UpnpError::RespTooLarge { limit } if limit == HTTP_MAX_HEADER), "头段灌爆应拒：{e}");
        // ⑥ 边界：正文恰 64 KiB（= SOAP 上限）⇒ Ok
        let payload = vec![b'z'; HTTP_MAX_SOAP];
        let mut resp = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", payload.len()).into_bytes();
        resp.extend_from_slice(&payload);
        let port = http_mock(vec![(resp, Duration::ZERO)]);
        let url = format!("http://127.0.0.1:{port}/x");
        let body = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_secs(3)), HTTP_MAX_SOAP).unwrap();
        assert_eq!(body.len(), HTTP_MAX_SOAP, "恰在上限内应放行");
    }

    /// **M5 回归**：keep-alive mock（发满声明长度后**不关连接**、继续挂着）⇒ 立即返回
    /// （Go `io.ReadAll(resp.Body)` 在长度边界返 EOF；修前 `read_to_end` 会耗到预算耗尽）。
    #[test]
    fn http_call_returns_when_declared_length_satisfied() {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut c = conn;
                let mut buf = [0u8; 512];
                let _ = c.read(&mut buf);
                let body = "hello-keepalive";
                let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}", body.len()).as_bytes());
                let _ = c.flush();
                std::thread::sleep(Duration::from_secs(3)); // 不关连接（keep-alive 形态）
            }
        });
        let url = format!("http://127.0.0.1:{port}/x");
        let t0 = std::time::Instant::now();
        let body = http_call(&url, "GET", None, None, None, Some(std::time::Instant::now() + Duration::from_secs(3)), HTTP_MAX_SOAP)
            .expect("收满声明长度即应成功");
        let dt = t0.elapsed();
        assert_eq!(body, "hello-keepalive");
        assert!(dt < Duration::from_millis(500), "不得等到期限（实 {dt:?}）");
    }

    // ---------- F8 SSDP 来源过滤 / 非空 LOCATION ----------

    #[test]
    fn ssdp_response_ok_cases() {
        let ok = "HTTP/1.1 200 OK\r\nST: x\r\nLOCATION: http://192.168.3.1:49152/root.xml\r\n\r\n";
        // 公网源 + 合规正文 ⇒ false（不校来源 = LAN 内 SSRF）
        assert!(!ssdp_response_ok(&"203.0.113.9:1900".parse().unwrap(), ok));
        // 私网源 + HTTP/1.1 200 + 非空 LOCATION ⇒ true
        assert!(ssdp_response_ok(&"192.168.3.1:1900".parse().unwrap(), ok));
        // 环回源也放行（本机 mock 形态）
        assert!(ssdp_response_ok(&"127.0.0.1:1900".parse().unwrap(), ok));
        // 状态行非 200 ⇒ false
        assert!(!ssdp_response_ok(
            &"192.168.3.1:1900".parse().unwrap(),
            "HTTP/1.1 404 Not Found\r\nLOCATION: http://192.168.3.1/x\r\n\r\n"
        ));
        // LOCATION 空 ⇒ false（对齐 Go `loc != ""`）
        assert!(!ssdp_response_ok(&"192.168.3.1:1900".parse().unwrap(), "HTTP/1.1 200 OK\r\nLOCATION:\r\n\r\n"));
        // 无 LOCATION ⇒ false
        assert!(!ssdp_response_ok(&"192.168.3.1:1900".parse().unwrap(), "HTTP/1.1 200 OK\r\nST: x\r\n\r\n"));
        // HTTP/1.0 200 也认；组播/未指定源拒
        assert!(ssdp_response_ok(&"10.0.0.5:1900".parse().unwrap(), "HTTP/1.0 200 OK\r\nLOCATION: http://10.0.0.1/x\r\n\r\n"));
        assert!(!ssdp_response_ok(&"0.0.0.0:1900".parse().unwrap(), ok));
        assert!(!ssdp_response_ok(&"224.0.0.251:1900".parse().unwrap(), ok));
        // 谓词 = `is_private() || is_loopback()`（Rust 的 `is_private` 只认 RFC1918）：
        // link-local 169.254/16 与 CGNAT 100.64/10 会被拒——今天无实际后果（候选面
        // `local_ipv4_candidates` 本就只收 RFC1918），Q-J 放宽候选面时须复核
        assert!(!ssdp_response_ok(&"169.254.1.1:1900".parse().unwrap(), ok));
        assert!(!ssdp_response_ok(&"100.64.0.1:1900".parse().unwrap(), ok));
        assert!(!ssdp_response_ok(&"[fd00::1]:1900".parse().unwrap(), ok));
    }

    // ---------- F9 一轮一次枚举 / 所有权门 / 轮级序 / 候选去重 ----------

    /// 可编程 mock IGD：记录整轮 SOAP action 序列与计数；可注入 718 冲突语义、
    /// 表尾缺失（枚举残缺）、指定端口拒绝、Delete 无效（加不回去）形态。
    struct MockState {
        entries: Vec<UpnpMapping>,
        actions: Vec<String>,
        enum_calls: usize,
        add_calls: usize,
        del_calls: usize,
        conflict_718: bool,
        no_tail: bool,
        refuse_ext: Option<u16>,
        delete_noop: bool,
    }

    fn start_mock() -> (u16, Arc<std::sync::Mutex<MockState>>) {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let st: Arc<std::sync::Mutex<MockState>> = Arc::new(std::sync::Mutex::new(MockState {
            entries: Vec::new(),
            actions: Vec::new(),
            enum_calls: 0,
            add_calls: 0,
            del_calls: 0,
            conflict_718: false,
            no_tail: false,
            refuse_ext: None,
            delete_noop: false,
        }));
        let st2 = Arc::clone(&st);
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let mut c = conn;
                let mut buf = Vec::new();
                let mut byte = [0u8; 1];
                loop {
                    match c.read(&mut byte) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            buf.push(byte[0]);
                            if buf.ends_with(b"\r\n\r\n") {
                                let head = String::from_utf8_lossy(&buf).into_owned();
                                let cl = header_value(&head, "Content-Length").and_then(|v| v.parse::<usize>().ok()).unwrap_or(0);
                                let mut body = vec![0u8; cl];
                                if cl > 0 {
                                    let _ = c.read_exact(&mut body);
                                }
                                buf.extend_from_slice(&body);
                                break;
                            }
                        }
                    }
                }
                let req = String::from_utf8_lossy(&buf).into_owned();
                if req.starts_with("GET ") {
                    let body = r#"<root><device><serviceList><service><serviceType>urn:schemas-upnp-org:service:WANIPConnection:1</serviceType><controlURL>/ctrlu</controlURL></service></serviceList></device></root>"#;
                    let _ = c.write_all(format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body).as_bytes());
                    continue;
                }
                let ext = xml_tag(&req, "NewExternalPort").and_then(|v| v.parse::<u16>().ok()).unwrap_or(0);
                let action = if req.contains("AddPortMapping") {
                    "AddPortMapping"
                } else if req.contains("DeletePortMapping") {
                    "DeletePortMapping"
                } else if req.contains("GetGenericPortMappingEntry") {
                    "GetGenericPortMappingEntry"
                } else {
                    "other"
                };
                let mut s = st2.lock().unwrap();
                s.actions.push(format!("{action}:{ext}"));
                let resp: String = match action {
                    "AddPortMapping" => {
                        s.add_calls += 1;
                        let int_p = xml_tag(&req, "NewInternalPort").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let client = xml_tag(&req, "NewInternalClient").unwrap_or_default();
                        let lease = xml_tag(&req, "NewLeaseDuration").and_then(|v| v.parse().ok()).unwrap_or(0);
                        let exists = s.entries.iter().any(|m| m.external_port == ext && m.protocol == "UDP");
                        if s.refuse_ext == Some(ext) || (s.conflict_718 && exists) {
                            let fault = "<s:Fault><detail><UPnPError><errorCode>718</errorCode><errorDescription>ConflictInMappingEntry</errorDescription></UPnPError></detail></s:Fault>";
                            format!("HTTP/1.1 500 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{fault}", fault.len())
                        } else {
                            s.entries.retain(|m| !(m.external_port == ext && m.protocol == "UDP"));
                            s.entries.push(UpnpMapping {
                                external_port: ext,
                                protocol: "UDP".to_owned(),
                                internal_port: int_p,
                                internal_client: client,
                                enabled: true,
                                description: UPNP_MAP_DESC.to_owned(),
                                lease_duration: lease,
                            });
                            "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_owned()
                        }
                    }
                    "DeletePortMapping" => {
                        s.del_calls += 1;
                        if !s.delete_noop {
                            s.entries.retain(|m| m.external_port != ext || m.protocol != "UDP");
                        }
                        "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_owned()
                    }
                    "GetGenericPortMappingEntry" => {
                        s.enum_calls += 1;
                        let idx: usize = xml_tag(&req, "NewPortMappingIndex").and_then(|v| v.parse().ok()).unwrap_or(0);
                        if idx >= s.entries.len() {
                            let fault = if s.no_tail {
                                // 非 713 的错误 ⇒ 枚举残缺（complete=false）
                                "<s:Fault><detail><UPnPError><errorCode>501</errorCode><errorDescription>ActionFailed</errorDescription></UPnPError></detail></s:Fault>".to_owned()
                            } else {
                                "<s:Fault><detail><UPnPError><errorCode>713</errorCode><errorDescription>SpecifiedArrayIndexInvalid</errorDescription></UPnPError></detail></s:Fault>".to_owned()
                            };
                            format!("HTTP/1.1 500 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{fault}", fault.len())
                        } else {
                            let m = &s.entries[idx];
                            let body = format!(
                                "<m><NewExternalPort>{}</NewExternalPort><NewProtocol>{}</NewProtocol><NewInternalPort>{}</NewInternalPort><NewInternalClient>{}</NewInternalClient><NewEnabled>1</NewEnabled><NewPortMappingDescription>{}</NewPortMappingDescription><NewLeaseDuration>{}</NewLeaseDuration></m>",
                                m.external_port, m.protocol, m.internal_port, m.internal_client, m.description, m.lease_duration
                            );
                            format!("HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}", body.len(), body)
                        }
                    }
                    _ => "HTTP/1.1 200 OK\r\nConnection: close\r\nContent-Length: 0\r\n\r\n".to_owned(),
                };
                drop(s);
                let _ = c.write_all(resp.as_bytes());
            }
        });
        (port, st)
    }

    fn mock_igd(port: u16) -> Igd {
        igd_from_location(&format!("http://127.0.0.1:{port}/root.xml"), UPNP_TOTAL_BUDGET).expect("mock IGD")
    }

    fn ours(ext: u16, client: Ipv4Addr, int_port: u16) -> UpnpMapping {
        UpnpMapping {
            external_port: ext,
            protocol: "UDP".to_owned(),
            internal_port: int_port,
            internal_client: client.to_string(),
            enabled: true,
            description: UPNP_MAP_DESC.to_owned(),
            lease_duration: 3600,
        }
    }

    /// F9a：表长 N ⇒ 一轮 `GetGenericPortMappingEntry` 计数 == **N+1**（第 N+1 次取
    /// 713 表尾；修前 3×(N+1)）且显式读到表尾标记。
    #[test]
    fn enumerate_once_per_round() {
        let (port, st) = start_mock();
        let g = mock_igd(port);
        let client: Ipv4Addr = "192.168.3.12".parse().unwrap();
        {
            let mut s = st.lock().unwrap();
            s.entries = vec![ours(42641, client, 42641), ours(42642, client, 42641), ours(42643, client, 42641)];
        }
        let logf = |_: &str| {};
        let n = st.lock().unwrap().entries.len(); // 3（轮前表长）
        let ext = mapping_round(&g, 42641, client, &logf, &logf).expect("一轮应成功");
        assert_eq!(ext, 42641, "prefer 沿用");
        let s = st.lock().unwrap();
        assert_eq!(s.enum_calls, n + 1, "一轮只枚举一次（N+1 次含表尾标记；修前 3(N+1)）");
        assert!(!s.actions.iter().any(|a| a == "DeletePortMapping:42641"), "prefer 不得被删：{:?}", s.actions);
        assert_eq!(s.del_calls, 2, "非 prefer 的陈旧条目（42642/42643）应被清：{:?}", s.actions);
    }

    /// F9：718 门——`allow_evict=false`（枚举残缺 ⇒ fail-open）时**不得**发
    /// DeletePortMapping，流程换下一候选。
    #[test]
    fn evict_only_when_verified_ours() {
        let (port, st) = start_mock();
        let g = mock_igd(port);
        let client: Ipv4Addr = "192.168.3.12".parse().unwrap();
        {
            let mut s = st.lock().unwrap();
            s.entries = vec![ours(42641, client, 42641)]; // 表里有我们的映射
            s.conflict_718 = true; // 但路由器对已存在端口回 718
            s.no_tail = true; // 枚举残缺 ⇒ verify=false ⇒ allow_evict=false
        }
        let logf = |_: &str| {};
        let ext = mapping_round(&g, 42641, client, &logf, &logf).expect("应换到下一候选");
        let s = st.lock().unwrap();
        assert_eq!(s.del_calls, 0, "未经核验不得删既有映射（修前无条件先删）");
        assert_ne!(ext, 42641, "冲突端口让位（实 {ext}）");
        assert!(s.add_calls >= 2, "42641 被拒后应换下一候选（实 {} 次 add）", s.add_calls);
        drop(s);

        // allow_evict=true（枚举完整 + 快照明确属于我们）⇒ 718 时 delete + 重加成功
        let (port2, st2) = start_mock();
        let g2 = mock_igd(port2);
        {
            let mut s = st2.lock().unwrap();
            s.entries = vec![ours(42641, client, 42641)];
            s.conflict_718 = true;
        }
        let ext2 = mapping_round(&g2, 42641, client, &logf, &logf).expect("核验后应幂等重建");
        assert_eq!(ext2, 42641);
        let s2 = st2.lock().unwrap();
        assert_eq!(s2.del_calls, 1, "核验属于我们 ⇒ 718 时先删后加");
        assert_eq!(s2.entries.len(), 1);
        assert_eq!(s2.entries[0].lease_duration, 3600, "重建仍用 1 小时租期");
    }

    /// F9/T1：轮级序——prefer 端口上首调用是 `AddPortMapping`，整轮不得出现该端口的
    /// `DeletePortMapping`（修前 clean 会先删掉 prefer）。
    #[test]
    fn round_level_add_before_delete() {
        let (port, st) = start_mock();
        let g = mock_igd(port);
        let client: Ipv4Addr = "192.168.3.12".parse().unwrap();
        {
            let mut s = st.lock().unwrap();
            // 表里两条：prefer 一条 + 一条陈旧（非 prefer ⇒ clean 会删它）
            s.entries = vec![ours(42641, client, 42641), ours(42650, client, 42641)];
        }
        let logf = |_: &str| {};
        let ext = mapping_round(&g, 42641, client, &logf, &logf).expect("一轮成功");
        assert_eq!(ext, 42641);
        let s = st.lock().unwrap();
        let prefer_ops: Vec<&String> = s.actions.iter().filter(|a| a.ends_with(":42641")).collect();
        assert_eq!(prefer_ops.len(), 1, "prefer 端口整轮只应有一次操作：{prefer_ops:?}");
        assert!(prefer_ops[0].starts_with("AddPortMapping"), "首调用必须是 Add：{prefer_ops:?}");
        assert!(!s.actions.iter().any(|a| a == "DeletePortMapping:42641"), "prefer 不得被删：{:?}", s.actions);
        assert!(s.actions.iter().any(|a| a == "DeletePortMapping:42650"), "陈旧的非 prefer 条目应被清：{:?}", s.actions);
    }

    /// F9c：候选去重（prefer == internal_port 且被拒 ⇒ 该端口只试一次；修前两次）。
    #[test]
    fn candidates_deduped() {
        let (port, st) = start_mock();
        let g = mock_igd(port);
        let client: Ipv4Addr = "192.168.3.12".parse().unwrap();
        {
            let mut s = st.lock().unwrap();
            // prefer == internal_port == 42641（表里有我们的映射 ⇒ prefer 选它）；
            // 枚举残缺（no_tail）⇒ verify=false ⇒ allow_evict=false ⇒ 被拒后不删、换候选。
            // 去重前候选表 = [42641(prefer), 42641(internal), 42642, …] ⇒ 该端口试 **2** 次。
            s.entries = vec![ours(42641, client, 42641)];
            s.no_tail = true;
            s.refuse_ext = Some(42641);
        }
        let logf = |_: &str| {};
        let ext = mapping_round(&g, 42641, client, &logf, &logf).expect("换下一候选应成功");
        assert_eq!(ext, 42642);
        let s = st.lock().unwrap();
        let tries = s.actions.iter().filter(|a| *a == "AddPortMapping:42641").count();
        assert_eq!(tries, 1, "同值候选只试一次（修前 prefer==internal_port 会试两次）：{:?}", s.actions);
    }

    /// F9：缩租「先 add(300) → 718 才 delete + add(300)」；加不回去时原映射仍在。
    #[test]
    fn shrink_lease_add_before_delete() {
        let (port, st) = start_mock();
        let g = mock_igd(port);
        let client: Ipv4Addr = "192.168.3.12".parse().unwrap();
        {
            let mut s = st.lock().unwrap();
            s.entries = vec![ours(42641, client, 42641)];
            s.conflict_718 = true;
        }
        g.re_add_short_lease(42641, client, 42641, 300).expect("缩租应成功");
        {
            let s = st.lock().unwrap();
            let ops: Vec<&String> = s.actions.iter().filter(|a| a.ends_with(":42641")).collect();
            assert_eq!(ops.len(), 3, "add → delete → add：{ops:?}");
            assert!(ops[0].starts_with("AddPortMapping"));
            assert!(ops[1].starts_with("DeletePortMapping"));
            assert!(ops[2].starts_with("AddPortMapping"));
            assert_eq!(s.entries[0].lease_duration, 300, "缩租后租期 300");
        }
        // 「加不回去」形态：Delete 无效 + 全部 add 被拒 ⇒ Err 且原映射仍在（原租期）
        let (port2, st2) = start_mock();
        let g2 = mock_igd(port2);
        {
            let mut s = st2.lock().unwrap();
            s.entries = vec![ours(42641, client, 42641)];
            s.conflict_718 = true;
            s.refuse_ext = Some(42641);
            s.delete_noop = true;
        }
        assert!(g2.re_add_short_lease(42641, client, 42641, 300).is_err(), "加不回去应报错");
        let s2 = st2.lock().unwrap();
        assert_eq!(s2.entries.len(), 1, "原映射仍在");
        assert_eq!(s2.entries[0].lease_duration, 3600, "原租期保持");
    }

    // ---------- F10 全局期限（候选循环 + SSDP 穿透） ----------

    /// 注入「睡 100ms 后失败」的 discover + 10 候选 + deadline 250ms ⇒ 调用数 ≤3、
    /// 总耗时受界（**全局**期限生效，而非每候选各起预算）。
    #[test]
    fn pick_igd_honors_global_deadline() {
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&calls);
        let cands: Vec<Ipv4Addr> = (0..10).map(|i| Ipv4Addr::new(192, 168, 3, 10 + i)).collect();
        let t0 = std::time::Instant::now();
        let r = pick_igd_before(&cands, std::time::Instant::now() + Duration::from_millis(250), move |_ip, _dl| {
            c2.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(100));
            Err(UpnpError::NoIgdResponse("mock".to_owned()))
        });
        let dt = t0.elapsed();
        assert!(r.is_err());
        let n = calls.load(Ordering::SeqCst);
        assert!(n <= 3, "全局期限下调用数应 ≤3（实 {n}）");
        // 无全局期限形态 = 10 候选 × 100ms = 1s ⇒ 阈值 500ms 两侧 ≥2× 余量
        assert!(dt < Duration::from_millis(500), "总耗时应受界（实 {dt:?}）");
    }

    /// **真 `discover_igd` 期限用例（R2 要求）**：黑洞/不可达形态 + 短 deadline ⇒
    /// 总耗时受界——证明 SSDP 腿被全局期限穿透（不注入假 discover 时也受界）。
    #[test]
    fn discover_igd_deadline_penetrates_ssdp() {
        let t0 = std::time::Instant::now();
        let r = discover_igd(Ipv4Addr::LOCALHOST, std::time::Instant::now() + Duration::from_millis(250));
        let dt = t0.elapsed();
        assert!(r.is_err(), "本机 SSDP 被 macOS 本地网络隐私拒/无 IGD ⇒ 应失败");
        assert!(dt < Duration::from_secs(2), "SSDP 腿必须被期限穿透（修前自带 5s；实 {dt:?}）");
    }
}
