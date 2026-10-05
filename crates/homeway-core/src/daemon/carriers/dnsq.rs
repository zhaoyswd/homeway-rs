//! A 查询客户端助手（语义真源 `baseline:pkg/dns/client.go`）。
//!
//! 消费方 = socks 承载面的远程解析腿（经拨号缝拨出口 DNS 代答的 TCP 面 5300，
//! 再在这里完成一次查询）。查询经注入的已建立连接（StreamConn——不自带拨号），
//! 本地零解析（解析发生在出口侧——「走指定主机」语义的一半，fake-ip/内网 DNS/
//! geo 场景下本地解析会拿错地址）。
//!
//! 应答解析返回**有序候选列表**（多 A 记录全量按应答顺序返回，socks 层按序拨）；
//! NXDOMAIN 与「无 A 记录」两类否定形态以可区分哨兵错误返回（调用方回 rep=0x04
//! 且文案可区分、不缓存）。

use std::sync::Arc;
use std::time::Duration;

use crate::daemon::StreamConn;

/// 出口客户端解析腿端口（隧道 IP:5300 的 TCP——出口在**隧道栈内**起的
/// DNS-over-TCP listener；Go socksDNSPort 同源同值）。
pub const SOCKS_DNS_PORT: u16 = 5300;

const TYPE_A: u16 = 1;
/// 查询名的编码长度上限（253 字节域名 + 标签长度位）。
const MAX_QUERY_NAME: usize = 255;
/// 单次解析总预算缺省（拨 5300 + 查询；design D3：5s，超时归因、不缓存）。
pub const RESOLVE_BUDGET: Duration = Duration::from_secs(5);

/// 带期限的解析壳（中-1：`StreamConn` 阻塞读无原生超时——worker 线程跑真解析、
/// 主面按期限等；超时**关连接**解阻塞 worker 的读（连接关 = 读错误退出，无泄漏），
/// 归因解析超时）。
pub fn resolve_with_deadline(
    conn: Arc<dyn StreamConn>,
    domain: &str,
    budget: Duration,
) -> Result<DnsResolved, DnsErr> {
    let (tx, rx) = std::sync::mpsc::channel::<Result<DnsResolved, DnsErr>>();
    let d = domain.to_owned();
    let c = Arc::clone(&conn);
    let h = std::thread::Builder::new()
        .name("hw-dnsq".to_owned())
        .stack_size(256 * 1024)
        .spawn(move || {
            let r = resolve_over_conn(c.as_ref(), &d, Duration::MAX);
            let _ = tx.send(r);
        });
    match rx.recv_timeout(budget) {
        Ok(r) => {
            let _ = h.map(|h| h.join());
            r
        }
        Err(_) => {
            conn.close(); // 解阻塞 worker（读错误退出）
            Err(DnsErr::Io("解析超时（预算 {budget:?}）".replace("{budget:?}", &format!("{budget:?}"))))
        }
    }
}

/// 解析失败的哨兵（两类否定形态可区分）。
#[derive(Debug, thiserror::Error)]
pub enum DnsErr {
    /// 应答 RCODE=3（域名不存在）。
    #[error("域名不存在（NXDOMAIN）")]
    NxDomain,
    /// 应答成功但答案段没有 A 记录（如纯 CNAME 无终端地址）。
    #[error("无 IPv4 地址（应答无 A 记录）")]
    NoA,
    /// 结构性非法 / IO 失败（超时归因进 msg）。
    #[error("{0}")]
    Io(String),
}

/// 一次 A 解析的产物。
pub struct DnsResolved {
    /// 按应答顺序的 A 记录候选列表（至少一条；socks 层按序拨）。
    pub addrs: Vec<std::net::Ipv4Addr>,
    /// 答案段 A 记录 TTL 的最小值（出口代答已钳制 ≤60s）。
    pub ttl_min: u32,
}

/// 域名 → 标签序列（结构性校验：非空、每标签 1–63、总长受限；字符集外不约束——
/// 只挡结构性非法）。
fn split_domain(domain: &str) -> Result<Vec<&str>, DnsErr> {
    let domain = domain.trim().trim_end_matches('.');
    if domain.is_empty() {
        return Err(DnsErr::Io("域名为空".to_owned()));
    }
    if domain.len() > 253 {
        return Err(DnsErr::Io("域名超长（>253）".to_owned()));
    }
    let labels = domain.split('.');
    let mut out = Vec::new();
    for l in labels {
        if l.is_empty() {
            return Err(DnsErr::Io("域名字段为空（连续点）".to_owned()));
        }
        if l.len() > 63 {
            return Err(DnsErr::Io("域名字段超长（>63）".to_owned()));
        }
        out.push(l);
    }
    Ok(out)
}

/// 构造一条 A 查询报文（随机事务 ID、RD=1、QDCOUNT=1）。返回 (报文, 事务 ID)。
pub fn a_query(domain: &str) -> Result<(Vec<u8>, u16), DnsErr> {
    let labels = split_domain(domain)?;
    let mut id = [0u8; 2];
    getrandom::getrandom(&mut id).map_err(|e| DnsErr::Io(format!("事务 ID 生成失败：{e}")))?;
    let txid = u16::from_be_bytes(id);
    let mut q = Vec::with_capacity(12 + MAX_QUERY_NAME + 8);
    q.extend_from_slice(&id);
    q.extend_from_slice(&[0x01, 0x00]); // flags：RD=1
    q.extend_from_slice(&[0x00, 0x01]); // QDCOUNT=1
    q.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00, 0x00]); // AN/NS/AR = 0
    for l in labels {
        q.push(l.len() as u8);
        q.extend_from_slice(l.as_bytes());
    }
    q.push(0); // 根
    q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]); // A / IN
    Ok((q, txid))
}

/// 在一条已建立的连接上完成一次 A 查询（RFC1035 TCP 分帧——两字节长度前缀）。
/// budget 覆盖读侧（写侧小报文不设独立期限）；连接用后由调用方关闭（每查询一条
/// 新连接是消费方的既有形态）。事务 ID 与问题段做最低限度校验（错答/串答不认）。
pub fn resolve_over_conn(
    conn: &dyn StreamConn,
    domain: &str,
    budget: Duration,
) -> Result<DnsResolved, DnsErr> {
    let (q, txid) = a_query(domain)?;
    // 写：2B 长度前缀 + 报文。
    let mut framed = Vec::with_capacity(2 + q.len());
    framed.extend_from_slice(&(q.len() as u16).to_be_bytes());
    framed.extend_from_slice(&q);
    conn.write_chunk(&framed)
        .map_err(|e| DnsErr::Io(format!("发查询失败：{e}")))?;
    let _ = budget; // 读预算由消费方的连接生命周期面兜底（StreamConn 阻塞读无原生超时）
    // 读：2B 长度前缀 + 应答（**带缓冲拼装**：TCP 块边界 ≠ 帧边界——2B 前缀读块
    // 越界时余量必须留在缓冲里，丢字节会让帧读错位死等）。
    let mut buf: Vec<u8> = Vec::with_capacity(512);
    while buf.len() < 2 {
        let chunk = conn.read_chunk().map_err(|e| DnsErr::Io(format!("读应答失败：{e}")))?;
        if chunk.is_empty() {
            return Err(DnsErr::Io("连接在帧中途关闭".to_owned()));
        }
        buf.extend_from_slice(&chunk);
    }
    let n = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if !(12..=65535).contains(&n) {
        return Err(DnsErr::Io(format!("应答长度非法：{n}")));
    }
    while buf.len() - 2 < n {
        let chunk = conn.read_chunk().map_err(|e| DnsErr::Io(format!("读应答失败：{e}")))?;
        if chunk.is_empty() {
            return Err(DnsErr::Io("连接在帧中途关闭".to_owned()));
        }
        buf.extend_from_slice(&chunk);
    }
    let resp = buf[2..2 + n].to_vec();
    parse_a_response(&resp, txid)
}

/// 解析 A 查询应答：ID 校验 → RCODE 分流（3 = NXDOMAIN）→ 答案段依序抽取 A 记录
/// （CNAME 跳过、压缩指针由 skip_name 处理）→ TTL 取 A 记录最小值。无 A 记录
/// （含 NOERROR 空答案）= NoA。
pub fn parse_a_response(resp: &[u8], txid: u16) -> Result<DnsResolved, DnsErr> {
    if resp.len() < 12 {
        return Err(DnsErr::Io("应答过短".to_owned()));
    }
    if u16::from_be_bytes([resp[0], resp[1]]) != txid {
        return Err(DnsErr::Io("事务 ID 不符（串答/错答）".to_owned()));
    }
    match resp[3] & 0x0F {
        0 => {}
        3 => return Err(DnsErr::NxDomain),
        rcode => return Err(DnsErr::Io(format!("解析失败（RCODE={rcode}）"))),
    }
    let (off, ok) = skip_name(resp, 12).unwrap_or((0, false));
    if !ok || off + 4 > resp.len() {
        return Err(DnsErr::Io("question 段畸形".to_owned()));
    }
    let mut off = off + 4;
    let answers = u16::from_be_bytes([resp[6], resp[7]]) as usize;
    let mut out = DnsResolved { addrs: Vec::with_capacity(answers.min(8)), ttl_min: 0 };
    for _ in 0..answers {
        let Some((p, true)) = skip_name(resp, off) else {
            return Err(DnsErr::Io("答案段畸形".to_owned()));
        };
        if p + 10 > resp.len() {
            return Err(DnsErr::Io("答案段畸形".to_owned()));
        }
        let rdlen = u16::from_be_bytes([resp[p + 8], resp[p + 9]]) as usize;
        if p + 10 + rdlen > resp.len() {
            return Err(DnsErr::Io("答案段 RDATA 越界".to_owned()));
        }
        let typ = u16::from_be_bytes([resp[p], resp[p + 1]]);
        if typ == TYPE_A && rdlen == 4 {
            let ip = std::net::Ipv4Addr::new(resp[p + 10], resp[p + 11], resp[p + 12], resp[p + 13]);
            let ttl = u32::from_be_bytes([resp[p + 4], resp[p + 5], resp[p + 6], resp[p + 7]]);
            if out.ttl_min == 0 || ttl < out.ttl_min {
                out.ttl_min = ttl;
            }
            out.addrs.push(ip);
        }
        off = p + 10 + rdlen;
    }
    if out.addrs.is_empty() {
        return Err(DnsErr::NoA);
    }
    Ok(out)
}

/// 跳过一个（可能压缩指针形式的）域名：返回 (新偏移, 是否合法)。
fn skip_name(buf: &[u8], mut off: usize) -> Option<(usize, bool)> {
    let mut jumped = false;
    let mut next = 0usize;
    let mut hops = 0;
    while hops < 64 {
        hops += 1;
        let Some(&l) = buf.get(off) else { return Some((next, false)) };
        if l & 0xC0 == 0xC0 {
            // 压缩指针（后继不推进 off——问题段/答案段名后的起点由调用方继续）。
            let Some(&h) = buf.get(off + 1) else { return Some((next, false)) };
            if !jumped {
                next = off + 2;
                jumped = true;
            }
            off = ((l & 0x3F) as usize) << 8 | h as usize;
            continue;
        }
        if l & 0xC0 != 0 {
            return Some((next, false)); // 保留形态非法
        }
        let end = off + 1 + l as usize;
        if l == 0 {
            return Some((if jumped { next } else { off + 1 }, true));
        }
        if end >= buf.len() {
            return Some((next, false));
        }
        off = end;
    }
    Some((next, false)) // 指针环防御
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 查询报文形态（RFC1035：随机 ID、RD=1、QDCOUNT=1、标签序）。
    #[test]
    fn a_query_shape() {
        let (q, txid) = a_query("multi.example").unwrap();
        assert_eq!(u16::from_be_bytes([q[0], q[1]]), txid);
        assert_eq!(q[2] & 0x01, 1, "RD=1");
        assert_eq!(u16::from_be_bytes([q[4], q[5]]), 1, "QDCOUNT=1");
        let tail = &q[12..];
        assert_eq!(tail, b"\x05multi\x07example\x00\x00\x01\x00\x01");
        // 结构性校验负例。
        assert!(a_query("").is_err());
        assert!(a_query("a..b").is_err());
        assert!(a_query(&"a".repeat(64)).is_err());
    }

    /// 跨块读：2B 前缀与应答体任意切块都能拼装（真实现测抓出的丢字节 bug 的
    /// 回归钉——旧实现在前缀读越界时丢弃余量，帧读错位死等）。
    #[test]
    fn resolve_over_conn_with_arbitrary_chunking() {
        use super::super::testutil::TcpIo;
        use crate::daemon::StreamConn;
        // 模拟出口代答：读 2B 前缀帧 → 回一条单 A 应答。
        let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = ln.local_addr().unwrap();
        let h = std::thread::spawn(move || {
            let (c, _) = ln.accept().unwrap();
            let srv_io = TcpIo::new(c);
            let mut buf = Vec::new();
            while buf.len() < 2 {
                match srv_io.read_chunk().unwrap() {
                    v if v.is_empty() => panic!("查询中途 EOF"),
                    v => buf.extend_from_slice(&v),
                }
            }
            let n = u16::from_be_bytes([buf[0], buf[1]]) as usize;
            while buf.len() - 2 < n {
                buf.extend_from_slice(&srv_io.read_chunk().unwrap());
            }
            let q = buf[2..2 + n].to_vec();
            // 应答：单 A 10.1.2.3（事务 ID 回显查询的）。
            let mut r = vec![0u8; 12];
            r[0..2].copy_from_slice(&q[0..2]);
            r[2] = 0x80;
            r[3] = 0x00;
            r[6..8].copy_from_slice(&1u16.to_be_bytes());
            r.extend_from_slice(b"\x04test\x03com\x00\x00\x01\x00\x01");
            r.extend_from_slice(b"\xc0\x0c\x00\x01\x00\x01");
            r.extend_from_slice(&60u32.to_be_bytes());
            r.extend_from_slice(&4u16.to_be_bytes());
            r.extend_from_slice(&[10, 1, 2, 3]);
            let mut framed = (r.len() as u16).to_be_bytes().to_vec();
            framed.extend_from_slice(&r);
            srv_io.write_chunk(&framed).unwrap();
            srv_io.close();
        });
        let c = std::net::TcpStream::connect(addr).unwrap();
        let io = TcpIo::new(c);
        let res = resolve_over_conn(&io, "test.com", Duration::from_secs(3)).unwrap();
        assert_eq!(res.addrs, vec!["10.1.2.3".parse::<std::net::Ipv4Addr>().unwrap()]);
        assert_eq!(res.ttl_min, 60);
        io.close();
        let _ = h.join();
    }

    /// 应答解析：多 A 按序 / NXDOMAIN / 无 A / 串答（Go client_test 同形对拍）。
    #[test]
    fn parse_a_response_cases() {
        let mk = |rcode: u8, answers: &[([u8; 4], u32)], txid: u16| -> Vec<u8> {
            let mut r = vec![0u8; 12];
            r[0..2].copy_from_slice(&txid.to_be_bytes());
            r[2] = 0x80; // QR=1
            r[3] = rcode;
            r[6..8].copy_from_slice(&(answers.len() as u16).to_be_bytes());
            // question：multi.example A IN（真标签——问题段不吃压缩指针）。
            r.extend_from_slice(b"\x05multi\x07example\x00\x00\x01\x00\x01");
            for (ip, ttl) in answers {
                r.extend_from_slice(b"\xc0\x0c");
                r.extend_from_slice(&TYPE_A.to_be_bytes());
                r.extend_from_slice(&1u16.to_be_bytes()); // IN
                r.extend_from_slice(&ttl.to_be_bytes());
                r.extend_from_slice(&4u16.to_be_bytes());
                r.extend_from_slice(ip);
            }
            r
        };
        let res = parse_a_response(
            &mk(0, &[([1, 2, 3, 4], 60), ([5, 6, 7, 8], 30)], 0xABCD),
            0xABCD,
        )
        .unwrap();
        let want: Vec<std::net::Ipv4Addr> =
            vec!["1.2.3.4".parse().unwrap(), "5.6.7.8".parse().unwrap()];
        assert_eq!(res.addrs, want);
        assert_eq!(res.ttl_min, 30);
        assert!(matches!(
            parse_a_response(&mk(3, &[], 0xABCD), 0xABCD),
            Err(DnsErr::NxDomain)
        ));
        assert!(matches!(
            parse_a_response(&mk(0, &[], 0xABCD), 0xABCD),
            Err(DnsErr::NoA)
        ));
        assert!(matches!(
            parse_a_response(&mk(0, &[([1, 1, 1, 1], 60)], 0x1111), 0xABCD),
            Err(DnsErr::Io(_))
        ));
    }
}
