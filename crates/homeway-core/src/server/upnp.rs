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
use std::net::{Ipv4Addr, SocketAddrV4, TcpStream, UdpSocket};
use std::time::Duration;

use super::egress::{interfaces, is_virtual_iface};

const SSDP_ADDR: &str = "239.255.255.250:1900";
const SSDP_ST: &str = "urn:schemas-upnp-org:device:InternetGatewayDevice:1";
/// 映射描述前缀（路由器表里「我们的映射」的认领判据）。
pub const UPNP_MAP_DESC: &str = "homeway-exit";
const UPNP_TIMEOUT: Duration = Duration::from_secs(5);
/// 映射表枚举上限（listMappings 的 max）。
const LIST_MAX: usize = 200;

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
}

/// M-SEARCH 报文（形状真源——ssdpLocation；`MX: 2` + IGD ST）。
pub fn msearch_message() -> String {
    format!("M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\nMAN: \"ssdp:discover\"\r\nMX: 2\r\nST: {SSDP_ST}\r\n\r\n")
}

/// 一个可用的 WAN 连接服务（WANIPConnection / WANPPPConnection）。
#[derive(Clone)]
pub struct Igd {
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

/// 发一次 SSDP M-SEARCH，取第一个 IGD 的 LOCATION（重试 3 次：家用路由器/交换机的
/// IGMP 收敛有几秒抖动）。local_ip 用于绑源地址（多网卡机器只有与路由器同网段的那张能用）。
pub fn ssdp_location(local_ip: Option<Ipv4Addr>) -> Result<String, UpnpError> {
    let bind: std::net::SocketAddr = match local_ip {
        Some(ip) => SocketAddrV4::new(ip, 0).into(),
        None => "0.0.0.0:0".parse().expect("合法字面量"),
    };
    let conn = UdpSocket::bind(bind)?;
    conn.set_read_timeout(Some(Duration::from_millis(1200)))?;
    let msg = msearch_message();
    let deadline = std::time::Instant::now() + UPNP_TIMEOUT;
    let mut last_err = String::new();
    let mut buf = [0u8; 4096];
    let mut attempt = 0;
    while attempt < 3 && std::time::Instant::now() < deadline {
        attempt += 1;
        if let Err(e) = conn.send_to(msg.as_bytes(), SSDP_ADDR) {
            // 发送失败重试（Go upnp.go:150-154——IGMP 收敛抖动下 sendto 偶发
            // no route to host，重发一次通常就通）
            last_err = format!("发送 M-SEARCH: {e}");
            std::thread::sleep(Duration::from_millis(500));
            continue;
        }
        loop {
            if std::time::Instant::now() >= deadline {
                break;
            }
            match conn.recv_from(&mut buf) {
                Ok((n, _)) => {
                    if let Some(loc) = header_value(&String::from_utf8_lossy(&buf[..n]), "LOCATION") {
                        return Ok(loc);
                    }
                }
                Err(e) => {
                    last_err = e.to_string();
                    break; // 等下一次重试
                }
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(UpnpError::NoIgdResponse(last_err))
}

fn header_value(resp: &str, key: &str) -> Option<String> {
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

fn http_call(url: &str, method: &str, content_type: Option<&str>, soap_action: Option<&str>, body: Option<&str>) -> Result<String, UpnpError> {
    let (host, port, path) = parse_http_url(url)?;
    let mut stream = TcpStream::connect((host.as_str(), port))?;
    stream.set_read_timeout(Some(UPNP_TIMEOUT))?;
    stream.set_write_timeout(Some(UPNP_TIMEOUT))?;
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
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp)?;
    let text = String::from_utf8_lossy(&resp).into_owned();
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

fn parse_http_url(url: &str) -> Result<(String, u16, String), UpnpError> {
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
pub fn find_wan_service(desc: &str, base_url: &str) -> Option<Igd> {
    let mut off = 0usize;
    while let Some(start) = desc[start_off(off)..].find("<service>") {
        let abs_start = start_off(off) + start;
        let end = desc[abs_start..].find("</service>")? + abs_start;
        let block = &desc[abs_start..end];
        if let (Some(st), Some(cu)) = (xml_tag(block, "serviceType"), xml_tag(block, "controlURL")) {
            if st.contains("WANIPConnection") || st.contains("WANPPPConnection") {
                let control_url = join_url(base_url, &cu);
                return Some(Igd { control_url, service_type: st });
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

fn xml_tag(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let i = body.find(&open)?;
    let j = body[i + open.len()..].find(&close)? + i + open.len();
    Some(body[i + open.len()..j].trim().to_owned())
}

/// SSDP 找到网关的 UPnP 描述并解析出 WAN 连接服务的控制地址（discoverIGD）。
pub fn discover_igd(local_ip: Ipv4Addr) -> Result<Igd, UpnpError> {
    let loc = ssdp_location(Some(local_ip))?;
    igd_from_location(&loc)
}

/// 已知 LOCATION 直取（mock 测试与 discover 共用）。
pub fn igd_from_location(loc: &str) -> Result<Igd, UpnpError> {
    let body = http_call(loc, "GET", None, None, None).map_err(|e| UpnpError::DescFetch(e.to_string()))?;
    find_wan_service(&body, loc).ok_or(UpnpError::NoWanService)
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
        http_call(&self.control_url, "POST", Some(r#"text/xml; charset="utf-8""#), Some(&sa), Some(&b)).map_err(|e| {
            // 保留 body 上下文（list_mappings 的「到表尾」判定要读 713 形态——Display
            // 链 "{action}: {source}" 与原 format! 同串）
            UpnpError::Soap { action: action.to_owned(), msg: e.to_string() }
        })
    }

    /// 加映射（先删同名保证幂等——多数路由器重复添加回 718；租期优先 1 小时，
    /// 只接受 0 的机型退永久并**出声**）。
    pub fn add_port_mapping(&self, external_port: u16, internal_ip: Ipv4Addr, internal_port: u16, logf: &dyn Fn(&str)) -> Result<(), UpnpError> {
        let _ = self.delete_mapping(external_port, "UDP");
        match self.add_with_lease(external_port, internal_ip, internal_port, 3600) {
            Ok(()) => Ok(()),
            Err(e) => {
                if self.add_with_lease(external_port, internal_ip, internal_port, 0).is_ok() {
                    logf(&format!(
                        "UPnP：路由器不接受 1 小时租期（{e}），已按**永久（0）**写入映射 外部 {external_port}——该映射在本机异常退出后不会自动过期；正常退出会缩到 5 分钟"
                    ));
                    return Ok(());
                }
                Err(e)
            }
        }
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

    /// 枚举路由器上的端口映射。`complete=false` = 没见到表尾标记就到 max 条/读完了
    /// （清单残缺——不能拿「清单里没有」当「路由器上没有」用）。
    pub fn list_mappings(&self) -> (Vec<UpnpMapping>, bool) {
        let mut out = Vec::new();
        for i in 0..LIST_MAX {
            match self.soap("GetGenericPortMappingEntry", &[("NewPortMappingIndex", i.to_string())]) {
                Err(e) => {
                    // 表尾标记只认**结构化形态**（评审 M9：错误串含整个 body——
                    // 裸 contains("713") 会把端口号 1713/序列号误判成表尾，破坏
                    // 「清单残缺 ⇒ fail-open」的保守防线）
                    let msg = e.to_string();
                    if msg.contains(">713<") || msg.contains("SpecifiedArrayIndexInvalid") {
                        return (out, true); // 表尾标记
                    }
                    return (out, false);
                }
                Ok(body) => {
                    if body.contains("SpecifiedArrayIndexInvalid") || body.contains(">713<") {
                        return (out, true);
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
        (out, false) // 走满 max 条仍没见表尾：按截断处理（保守）
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
    /// 不算）。返回顺序偏好：外部端口 == 监听端口 → 表里第一条我们的。
    pub fn find_our_mapping(&self, desc_prefix: &str, client: Ipv4Addr, listen_port: u16) -> Option<(u16, u16)> {
        let (list, _) = self.list_mappings();
        let mut fallback = None;
        for m in &list {
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
    /// 返回删除条数。
    pub fn clean_mappings(&self, desc_prefix: &str, client: Ipv4Addr, keep_internal_port: u16, logf: &dyn Fn(&str)) -> usize {
        let (list, _) = self.list_mappings();
        let mut n = 0;
        for m in &list {
            match classify_mapping(m, desc_prefix, client, keep_internal_port) {
                MappingOwner::Ours => {
                    if self.delete_mapping(m.external_port, &m.protocol).is_ok() {
                        n += 1;
                    }
                }
                MappingOwner::LiveSibling => {
                    logf(&format!(
                        "UPnP：外部 {} 的映射指向本机另一活实例（内网端口 {} 在监听），不清",
                        m.external_port, m.internal_port
                    ));
                }
                _ => {}
            }
        }
        n
    }

    /// 用很短的租期重建同一条映射（退出时缩租——快速重启能沿用同一个公网端口，
    /// 出口真退休了映射自动过期）。
    pub fn re_add_short_lease(&self, ext_port: u16, internal_ip: Ipv4Addr, internal_port: u16, lease: u32) -> Result<(), UpnpError> {
        let _ = self.delete_mapping(ext_port, "UDP");
        self.add_with_lease(ext_port, internal_ip, internal_port, lease)?;
        Ok(())
    }
}

/// 为 internal_port 申请一个外部端口（ensurePortMapping + selectExternalPort）：
/// 端口选择顺序 = 上次成功的外部端口（路由器表认领——**权威记忆**，不需要本地文件）
/// → 与监听端口同号 → +1…+9；每个候选先核验所有权（别人的/活实例的让位）。
pub fn ensure_port_mapping(
    candidates: &[Ipv4Addr],
    internal_port: u16,
    logf: &dyn Fn(&str),
    dlogf: &dyn Fn(&str),
) -> Result<(u16, Ipv4Addr), UpnpError> {
    let mut igd = None;
    let mut local_ip = Ipv4Addr::UNSPECIFIED;
    let mut last_err = String::new();
    for cand in candidates {
        match discover_igd(*cand) {
            Ok(g) => {
                igd = Some(g);
                local_ip = *cand;
                break;
            }
            Err(e) => last_err = e.to_string(),
        }
    }
    let Some(g) = igd else {
        return Err(UpnpError::NoIgd { candidates: format!("{candidates:?}"), last: last_err });
    };
    let (prefer, prev_internal) = match g.find_our_mapping(UPNP_MAP_DESC, local_ip, internal_port) {
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
    let n = g.clean_mappings(UPNP_MAP_DESC, local_ip, internal_port, dlogf);
    if n > 0 {
        dlogf(&format!("UPnP：清掉 {n} 条本机同前缀的旧映射（换端口或上次退出的遗留）"));
    }
    let ext = select_external_port(&g, internal_port, prefer, local_ip, logf)?;
    Ok((ext, local_ip))
}

/// 候选端口逐个申请（所有权核验 + 让位语义；映射表枚举失败/残缺/含归属不明条目时
/// fail-open 直接申请——把它当 foreign 会让自己的映射每轮 +1 漂移直到候选让尽）。
pub fn select_external_port(g: &Igd, internal_port: u16, prefer: u16, local_ip: Ipv4Addr, logf: &dyn Fn(&str)) -> Result<u16, UpnpError> {
    let (list, complete) = g.list_mappings();
    let mut verify = complete;
    if !verify {
        logf(&format!(
            "UPnP：映射表枚举不完整（{} 条未见表尾标记，表超上限或路由器漏报），本轮跳过所有权核验，按旧逻辑直接申请",
            list.len()
        ));
    }
    let taken: std::collections::HashMap<u16, &UpnpMapping> =
        list.iter().filter(|m| m.protocol == "UDP").map(|m| (m.external_port, m)).collect();
    if verify {
        for m in taken.values() {
            if classify_mapping(m, UPNP_MAP_DESC, local_ip, internal_port) == MappingOwner::Unknown {
                logf(&format!(
                    "UPnP：映射表中存在归属不明的条目（外部 {} 客户端={:?} desc={}，路由器客户端字段形态异常），本轮跳过所有权核验，按旧逻辑直接申请",
                    m.external_port, m.internal_client, m.description
                ));
                verify = false;
                break;
            }
        }
    }
    let mut cands: Vec<u16> = Vec::new();
    if prefer != 0 {
        cands.push(prefer);
    }
    for i in 0..=9u32 {
        let c = internal_port as u32 + i; // u32 计算：u16 直接加会在 >65526 时回绕到特权端口
        if c > 0xFFFF {
            break;
        }
        cands.push(c as u16);
    }
    let mut occupied = 0usize;
    let mut refused = 0usize;
    let mut first_occupant = String::new();
    for ext in cands {
        if ext == 0 {
            continue;
        }
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
                    _ => {} // Ours：先删后加即幂等重建
                }
            }
        }
        match g.add_port_mapping(ext, local_ip, internal_port, logf) {
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
                logf(&format!("UPnP：外部端口 {ext} 申请失败（{e}），换下一个候选"));
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
        let g = find_wan_service(desc, "http://192.168.3.1:49152/root.xml").expect("应命中");
        assert_eq!(g.control_url(), "http://192.168.3.1:49152/ctrlu/wanip");
        // 绝对 URL 原样
        let g2 = find_wan_service(desc.replace("/ctrlu/wanip", "http://10.0.0.2:80/ctrlu").as_str(), "http://192.168.3.1/root.xml").unwrap();
        assert_eq!(g2.control_url(), "http://10.0.0.2:80/ctrlu");
        // 无 WAN 服务
        assert!(find_wan_service("<root/>", "http://1.2.3.4/").is_none());
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
        let g = igd_from_location(&base).expect("mock IGD 应可发现");
        let client: Ipv4Addr = "192.168.3.12".parse().unwrap();
        let logf = |_: &str| {};

        // 加映射 → 表里可见
        g.add_port_mapping(42641, client, 42641, &logf).unwrap();
        let (list, complete) = g.list_mappings();
        assert!(complete, "到表尾");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].external_port, 42641);
        assert_eq!(list[0].lease_duration, 3600, "1 小时租期优先");

        // find_our_mapping：同号命中
        assert_eq!(g.find_our_mapping(UPNP_MAP_DESC, client, 42641), Some((42641, 42641)));
        // 换内网客户端 = 别人的
        let other: Ipv4Addr = "192.168.3.99".parse().unwrap();
        assert_eq!(g.find_our_mapping(UPNP_MAP_DESC, other, 42641), None);

        // select_external_port：prefer 沿用
        let ext = select_external_port(&g, 42641, 42641, client, &logf).unwrap();
        assert_eq!(ext, 42641);

        // clean：自己的清掉
        let n = g.clean_mappings(UPNP_MAP_DESC, client, 42641, &logf);
        assert_eq!(n, 1);
        let (list2, _) = g.list_mappings();
        assert!(list2.is_empty());

        // re_add_short_lease（退出缩租）
        g.add_port_mapping(42650, client, 42641, &logf).unwrap();
        g.re_add_short_lease(42650, client, 42641, 300).unwrap();
        let (list3, _) = g.list_mappings();
        assert_eq!(list3[0].lease_duration, 300);
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
}
