//! 参照点探测协议（pkg/probe 客户端半边；语义真源 `baseline:pkg/probe/probe.go`）。
//!
//! 明文一问一答（不进 WG、不登记 peer——只回答「这个地址的 UDP 能不能来回」与
//! 「出口当前可公布的公网端点」）：
//!
//! ```text
//! 请求:  "HWQ" ‖ ver(1) ‖ type(1) ‖ nonce(8) ‖ 填充…        （≥16B）
//! 响应:  "HWR" ‖ ver(1) ‖ type(1) ‖ nonce(8) ‖ payload
//!   type=1 ping：buildLen(1) ‖ build(≤32) ‖ flags(1，可省)
//!                ‖ epCount(1，可省) ‖ epCount × [16B 4in6 ‖ 2B BE 端口]
//! ```
//!
//! 防放大不变量（服务端义务）：带列表的应答总长 ≤ 请求长度——**请求方以 pad 声明
//! 可收上限**（旁路探测 pad 200；pad 16 的旧形态自然拿不到列表）。nonce 随机
//! （防盲注入/串答）。未知 type/短包忽略（前向兼容）。

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// 协议版本。
pub const VERSION: u8 = 1;
/// ping（可达性 + 构建标记 + 能力位 + 端点列表）。
pub const TYPE_PING: u8 = 1;
/// 端点列表段条数上限（出口与消费侧同约束）。
pub const MAX_ENDPOINTS: usize = 8;
/// 探针 flags：**中继命名空间**的「控制面降级」位（F10）。bit0–4 是 serve 的 udpcap
/// （`engine.rs` UDPCAP_*），故中继位从 bit5 起——按位掩码消费方（客户端只按位取用）
/// 追加位安全。
pub const FLAG_RELAY_CTL_DEGRADED: u8 = 1 << 5;

const REQ_MAGIC: [u8; 3] = *b"HWQ";
const RESP_MAGIC: [u8; 3] = *b"HWR";
/// 旁路探测请求填充（要拿端点列表就得 pad 到期望最大应答长度；Go probePad=200 同源）。
pub const PROBE_PAD: usize = 200;

/// PingEx 的完整应答。
#[derive(Debug, Clone, Default)]
pub struct PingResult {
    pub rtt: Duration,
    pub build: String,
    /// 出口能力位（bit0 = 默认路径可承载 UDP:53；bit1 = 通用 UDP）。
    pub flags: u8,
    /// 出口端点列表段（请求 pad 够长才有；老出口无此段 ⇒ 空）。
    pub endpoints: Vec<SocketAddr>,
}

/// 组请求（13B 头 + pad 填充）。
pub fn encode_request(typ: u8, nonce: &[u8; 8], pad: usize) -> Vec<u8> {
    let mut out = vec![0u8; 13 + pad];
    out[0..3].copy_from_slice(&REQ_MAGIC);
    out[3] = VERSION;
    out[4] = typ;
    out[5..13].copy_from_slice(nonce);
    out
}

/// 是不是探测请求（测试桩判别用；客户端只做请求方）。
pub fn is_probe_request(b: &[u8]) -> bool {
    b.len() >= 3 && b[0..3] == REQ_MAGIC
}

// ---- 服务端应答面（R3；语义真源 pkg/probe/probe.go 的 RespondEx）----

/// 解析探测请求（服务端侧）：非探测包返回 None（调用方放行给数据面）；
/// 是探测但短/版本不符返回 Some(Err)（应忽略不回包——前向兼容）。
pub fn decode_request(b: &[u8]) -> Option<io::Result<(u8, [u8; 8])>> {
    if b.len() < 3 || b[0..3] != REQ_MAGIC {
        return None;
    }
    if b.len() < 16 {
        return Some(Err(io::Error::new(io::ErrorKind::InvalidData, "probe: 包太短")));
    }
    if b[3] != VERSION {
        return Some(Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("probe: 版本 {} 不支持", b[3]),
        )));
    }
    let mut nonce = [0u8; 8];
    nonce.copy_from_slice(&b[5..13]);
    Some(Ok((b[4], nonce)))
}

/// 出口对探测请求的应答（RespondEx 同义）。None = 不回包（非探测/未知 type/版本不符）。
///
/// 防放大约束（MUST）：只有「带列表的应答总长 ≤ 请求长度」才附列表——请求方以 pad
/// 后的长度声明可收上限（老请求方 pad 16 自然拿不到列表段）。endpoints 超上限截断
/// 到 MAX_ENDPOINTS；非法条目（零端口）跳过。
pub fn respond_ex(req: &[u8], build: &str, flags: u8, endpoints: &[SocketAddr]) -> Option<Vec<u8>> {
    let (typ, nonce) = decode_request(req)?.ok()?;
    let mut base = Vec::with_capacity(48);
    base.extend_from_slice(&RESP_MAGIC);
    base.push(VERSION);
    base.push(typ);
    base.extend_from_slice(&nonce);
    match typ {
        TYPE_PING => {
            // **字节截断**（与 Go `probe.go:173-174` 的 `build[:maxBuild]` 逐字同形、且
            // **不 panic**）。不用 `floor_char_boundary`：多字节字符跨界时会少发 1–3 字节、
            // 长度字节也跟着变 ⇒ 引入未登记的 wire 差异。
            let b = build.as_bytes();
            let n = b.len().min(32);
            base.push(n as u8);
            base.extend_from_slice(&b[..n]);
            base.push(flags);
        }
        _ => return None, // 未知类型：忽略（前向兼容）
    }
    if endpoints.is_empty() {
        return Some(base);
    }
    let mut list: Vec<&SocketAddr> = endpoints
        .iter()
        .filter(|ep| ep.port() != 0)
        .take(MAX_ENDPOINTS)
        .collect();
    if list.is_empty() {
        return Some(base);
    }
    let mut with_list = base.clone();
    with_list.push(list.len() as u8);
    for ep in list.drain(..) {
        let a16 = match ep.ip() {
            std::net::IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(), // 4in6：与 hint 同一 18B 范式
            std::net::IpAddr::V6(v6) => v6.octets(),
        };
        with_list.extend_from_slice(&a16);
        with_list.extend_from_slice(&ep.port().to_be_bytes());
    }
    if with_list.len() > req.len() {
        return Some(base); // 请求没 pad 够：不带列表（老客户端形态），防放大约束优先
    }
    Some(with_list)
}

/// 解析响应（magic/ver/type/nonce 全验；不符返回错误，调用方丢弃）。
/// 参照点探测应答的纯解析（无 IO；pub = fuzz/向量可达面——unconnected recv_from
/// 的任意源可喂，flags 字节是自由面）。
pub fn decode_response(b: &[u8], nonce: &[u8; 8]) -> io::Result<PingResult> {
    if b.len() < 13 || b[0..3] != RESP_MAGIC {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "probe: 不是探测响应"));
    }
    if b[3] != VERSION || b[4] != TYPE_PING {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("probe: 响应版本/类型不符（{}/{}）", b[3], b[4]),
        ));
    }
    if &b[5..13] != nonce {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "probe: nonce 不匹配"));
    }
    let mut resp = PingResult::default();
    let payload = &b[13..];
    if payload.is_empty() {
        return Ok(resp);
    }
    let n = payload[0] as usize;
    if n > payload.len() - 1 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "probe: 包太短"));
    }
    resp.build = String::from_utf8_lossy(&payload[1..1 + n]).into_owned();
    let mut rest = &payload[1 + n..];
    if rest.is_empty() {
        return Ok(resp);
    }
    resp.flags = rest[0];
    rest = &rest[1..];
    // 端点列表段（追加在 flags 之后；老出口无此段）
    if rest.is_empty() {
        return Ok(resp);
    }
    let cnt = rest[0] as usize;
    if cnt > MAX_ENDPOINTS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("probe: 端点列表条数超上限（{cnt} > {MAX_ENDPOINTS}）"),
        ));
    }
    if rest.len() < 1 + cnt * 18 {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "probe: 包太短"));
    }
    for i in 0..cnt {
        let e = &rest[1 + i * 18..1 + i * 18 + 18];
        let addr = decode_4in6(&e[..16]);
        let port = u16::from_be_bytes([e[16], e[17]]);
        if port == 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "probe: 端点端口为零"));
        }
        resp.endpoints.push(SocketAddr::new(addr, port));
    }
    Ok(resp)
}

/// 16B 4in6 编码 → IpAddr（v4 映射形态归一为 v4）。
fn decode_4in6(b: &[u8]) -> std::net::IpAddr {
    let mut a16 = [0u8; 16];
    a16.copy_from_slice(&b[..16]);
    if a16[..10] == [0u8; 10] && a16[10] == 0xff && a16[11] == 0xff {
        std::net::IpAddr::V4(std::net::Ipv4Addr::new(a16[12], a16[13], a16[14], a16[15]))
    } else {
        std::net::IpAddr::V6(a16.into())
    }
}

/// 一问一答（独立 socket；3s 缺省预算）。**旁路观测纪律**：死候选失败是预期，
/// 调用方不得把结果计入健康判定。
pub fn ping_ex(target: SocketAddr, pad: usize, budget: Duration) -> io::Result<PingResult> {
    let local = if target.is_ipv4() { "0.0.0.0:0" } else { "[::]:0" };
    let sock = UdpSocket::bind(local)?;
    sock.set_read_timeout(Some(budget))?;
    let mut nonce = [0u8; 8];
    getrandom::getrandom(&mut nonce).expect("系统随机源不可用");
    let req = encode_request(TYPE_PING, &nonce, pad);
    let start = Instant::now();
    sock.send_to(&req, target)?;
    let mut buf = [0u8; 1500];
    loop {
        let (n, _) = sock.recv_from(&mut buf)?;
        if let Ok(r) = decode_response(&buf[..n], &nonce) {
            return Ok(PingResult { rtt: start.elapsed(), ..r });
        }
        // 非法/无关包：继续等到预算耗尽
    }
}

/// 探测线索的消费卫兵（与出口侧对称，transport.go probeAddrAcceptable 同义）：
/// 只要全球单播且不落 fake-IP 段（198.18/15，代理）与 CGNAT（100.64/10）——
/// 应答无认证，不能让投喂污染候选表。
///
/// **v4-mapped 归一（M2 代码门 G3，同类 D1）**：Go 同函数首行 `ip = ip.Unmap()`。
/// 双栈 socket 上 IPv4 对端/hint 以 `::ffff:a.b.c.d` 出现；不先归一则私网/回环/
/// 链路本地/fake-IP/CGNAT 的 mapped 形态全部走 v6 分支放行（可达链 =
/// `wtransport/bind.rs` 的未认证 hint → 候选集/打洞），安全卫兵被绕过。
/// 归一后 mapped 与纯 v4 **同判定**；真 IPv6（含 `::1`/ULA）语义不变。
pub fn probe_addr_acceptable(addr: &SocketAddr) -> bool {
    let ip = crate::udpbatch::unmap_v4_in6(*addr).ip();
    match ip {
        std::net::IpAddr::V4(v4) => {
            let o = v4.octets();
            let is_private = o[0] == 10 || o[0] == 172 && (16..=31).contains(&o[1]) || o[0] == 192 && o[1] == 168;
            let is_loopback = v4.is_loopback();
            let is_link_local = o[0] == 169 && o[1] == 254;
            let is_fake_ip = o[0] == 198 && (18..=19).contains(&o[1]); // 198.18.0.0/15
            let is_cgnat = o[0] == 100 && (64..=127).contains(&o[1]); // 100.64.0.0/10
            let is_multicast = v4.is_multicast();
            !(is_private || is_loopback || is_link_local || is_fake_ip || is_cgnat || is_multicast)
        }
        std::net::IpAddr::V6(v6) => {
            !(v6.is_loopback() || v6.is_multicast() || (v6.segments()[0] & 0xfe00) == 0xfc00 /* ULA */
                || (v6.segments()[0] & 0xffc0) == 0xfe80 /* link-local */)
        }
    }
}

/// 便捷：对候选并发旁路探测，返回应答端点列表（消费卫兵过滤后）。
/// 结果只进学习缓存与日志（旁路纪律：不计入 failStreak/健康判定）。
pub fn probe_candidates(
    targets: &[SocketAddr],
    budget: Duration,
    logf: &dyn Fn(&str),
    on_endpoint: &mut dyn FnMut(SocketAddr),
) {
    let mut learned = 0usize;
    std::thread::scope(|s| {
        let mut handles = Vec::with_capacity(targets.len());
        for t in targets {
            handles.push(s.spawn(move || {
                ping_ex(*t, PROBE_PAD, budget).ok().map(|r| r.endpoints)
            }));
        }
        for h in handles {
            if let Ok(Some(eps)) = h.join() {
                for ep in eps {
                    if probe_addr_acceptable(&ep) {
                        on_endpoint(ep);
                        learned += 1;
                    }
                }
            }
        }
    });
    if learned > 0 {
        logf(&format!(
            "旁路探测：{} 个直连候选，应答端点列表 {learned} 条已入学习缓存（来源=探测线索）",
            targets.len()
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 编解码往返（对拍 Go 字节形状：13B 头 + pad；响应解析含 build/flags/端点列表）。
    #[test]
    fn request_shape_and_response_roundtrip() {
        let nonce = [7u8; 8];
        let req = encode_request(TYPE_PING, &nonce, 16);
        assert_eq!(req.len(), 13 + 16);
        assert_eq!(&req[..3], b"HWQ");
        assert_eq!(req[3], VERSION);
        assert_eq!(req[4], TYPE_PING);
        assert_eq!(&req[5..13], &nonce);
        assert!(is_probe_request(&req));

        // 响应：build(5B) + flags + 2 条端点（4in6 编码）
        let mut resp = Vec::new();
        resp.extend_from_slice(b"HWR");
        resp.push(VERSION);
        resp.push(TYPE_PING);
        resp.extend_from_slice(&nonce);
        resp.push(5);
        resp.extend_from_slice(b"v0.15");
        resp.push(0x03); // flags
        resp.push(2); // epCount
        let ep1: SocketAddr = "203.0.113.9:41641".parse().unwrap();
        let ep2: SocketAddr = "[2001:db8::1]:41641".parse().unwrap();
        for ep in [ep1, ep2] {
            let a16 = match ep.ip() {
                std::net::IpAddr::V4(v4) => v4.to_ipv6_mapped().octets(),
                std::net::IpAddr::V6(v6) => v6.octets(),
            };
            resp.extend_from_slice(&a16);
            resp.extend_from_slice(&ep.port().to_be_bytes());
        }
        let r = decode_response(&resp, &nonce).unwrap();
        assert_eq!(r.build, "v0.15");
        assert_eq!(r.flags, 0x03);
        assert_eq!(r.endpoints, vec![ep1, ep2]);

        // nonce 不符丢弃
        assert!(decode_response(&resp, &[8u8; 8]).is_err());
        // 魔数不符
        let mut bad = resp.clone();
        bad[0] = b'X';
        assert!(decode_response(&bad, &nonce).is_err());
    }

    /// F9：build 串按**字节**截断（与 Go `probe.go:173-174` 同形），非 ASCII 跨界不 panic。
    #[test]
    fn respond_build_byte_truncation_no_panic() {
        let nonce = [1u8; 8];
        let req = encode_request(TYPE_PING, &nonce, 16);
        // 31 个 ASCII + 一个 3 字节汉字 = 34 字节，汉字恰跨 32 字节边界
        let build = format!("{}汉", "a".repeat(31));
        assert!(build.len() > 32);
        let resp = respond_ex(&req, &build, 0x20, &[]).unwrap();
        let payload = &resp[13..];
        assert_eq!(payload[0] as usize, 32, "长度字节 = 32（字节截断）");
        assert_eq!(&payload[1..33], &build.as_bytes()[..32], "字节前缀与 Go 同形");
        assert_eq!(payload[33], 0x20, "flags 紧随 build");
        // 解码侧 lossy 读回不报错
        assert!(decode_response(&resp, &nonce).is_ok());
        // 纯 ASCII > 32 同样截断
        let long = "b".repeat(40);
        let r2 = respond_ex(&req, &long, 0, &[]).unwrap();
        assert_eq!(r2[13] as usize, 32);
    }

    /// 消费卫兵：私网/回环/fake-IP/CGNAT 拒；全球单播 v4/v6 收。
    #[test]
    fn addr_acceptance_guard() {
        let bad = ["10.0.0.1:1", "192.168.1.1:1", "127.0.0.1:1", "169.254.1.1:1", "198.18.0.1:1", "100.64.0.1:1"];
        for b in bad {
            assert!(!probe_addr_acceptable(&b.parse().unwrap()), "{b} 应拒");
        }
        assert!(probe_addr_acceptable(&"203.0.113.9:41641".parse().unwrap()));
        assert!(probe_addr_acceptable(&"[2001:db8::1]:41641".parse().unwrap()));
        assert!(!probe_addr_acceptable(&"[fe80::1]:1".parse().unwrap()));
        assert!(!probe_addr_acceptable(&"[fc00::1]:1".parse().unwrap()));
    }

    /// M2 代码门 G3（D1 同类）：v4-mapped 形态先归一 ⇒ 与纯 v4 **同判定**；
    /// mapped 的私网/回环/链路本地/fake-IP/CGNAT 全拒（改前全放行——安全卫兵绕过）。
    #[test]
    fn addr_acceptance_guard_normalizes_v4_mapped() {
        // 等价性质：对每个 v4，mapped 形态与纯 v4 判定相同。
        for v4 in [
            "10.0.0.1",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "198.18.0.1",
            "100.64.0.1",
            "224.0.0.1",
            "203.0.113.9",
            "8.8.8.8",
        ] {
            let pure: SocketAddr = format!("{v4}:41641").parse().unwrap();
            let mapped: SocketAddr = format!("[::ffff:{v4}]:41641").parse().unwrap();
            assert_eq!(
                probe_addr_acceptable(&mapped),
                probe_addr_acceptable(&pure),
                "mapped {v4} 与纯 v4 必须同判定"
            );
        }
        // 负例（mapped 形态的拒绝面——改前全部被放行）
        for bad in [
            "[::ffff:10.0.0.1]:1",
            "[::ffff:172.16.0.1]:1",
            "[::ffff:192.168.1.1]:1",
            "[::ffff:127.0.0.1]:1",
            "[::ffff:169.254.1.1]:1",
            "[::ffff:198.18.0.1]:1",
            "[::ffff:100.64.0.1]:1",
            "[::ffff:224.0.0.1]:1",
        ] {
            assert!(!probe_addr_acceptable(&bad.parse().unwrap()), "{bad} 应拒（同 v4 判定）");
        }
        // 正例：mapped 的全球单播收（与纯 v4 同判定）
        assert!(probe_addr_acceptable(&"[::ffff:203.0.113.9]:41641".parse().unwrap()));
        // 真 IPv6 不受归一影响（负例：回环/ULA/链路本地；正例：GUA）
        assert!(!probe_addr_acceptable(&"[::1]:1".parse().unwrap()));
        assert!(!probe_addr_acceptable(&"[fc00::1]:1".parse().unwrap()));
        assert!(!probe_addr_acceptable(&"[fe80::1]:1".parse().unwrap()));
        assert!(!probe_addr_acceptable(&"[ff02::1]:1".parse().unwrap()));
        assert!(probe_addr_acceptable(&"[2001:db8::1]:41641".parse().unwrap()));
    }
}
