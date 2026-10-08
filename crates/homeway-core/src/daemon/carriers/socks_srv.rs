//! SOCKS5 子集服务端（语义真源 `baseline:pkg/socks/socks.go`——B0-2b §十「Rust 无
//! SOCKS5 服务端」缺口的实装）。
//!
//! 「桌面无 VPN 的浏览器承载面」的最小 SOCKS5 实现：no-auth + CONNECT only +
//! 域名/IPv4 目标。信任模型 = 本机用户即所有者（监听只应绑 127.0.0.1——由消费方
//! 保证）。BIND / UDP ASSOCIATE 回 rep=command not supported；IPv6 目标回
//! rep=address type not supported（隧道仅承载 IPv4）。
//!
//! 域名目标 MUST 经注入 resolver 远程解析（返回**有序候选列表**——本面按序拨，首个
//! 拨不通换下一个、全部不通才回失败）；本文件**无本地解析**（结构保证：零系统解析
//! 调用）。拨号经注入 dialer（生产 = Carriers 拨号缝）。上游/解析预算挂服务端生命
//! 周期 `closed` 位（off/Close 之后在途预算当场收）。
//!
//! 收口口径：上游拨号失败回 rep=0x01（general failure）+ 本地连接 RST（优雅 FIN 会让
//! 浏览器静默挂住）；解析否定（NXDOMAIN/无 A）回 rep=0x04（host unreachable）——
//! SOCKS5 wire 无文案字段，两类否定形态的文案在日志面区分。双向透传不设期限（长连接
//! 语义）。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::SocketAddrV4;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::{pipe_half_close, rst_close_tcp, CarrierDial, PollAccept, PollListener};

// SOCKS5 rep 码（RFC 1928）。
const REP_SUCCEEDED: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_HOST_UNREACH: u8 = 0x04;
const REP_CMD_NOT_SUP: u8 = 0x07;
const REP_ADDR_NOT_SUP: u8 = 0x08;

const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;
const CMD_CONNECT: u8 = 0x01;

/// 每 listener 并发连接上限（0 = 256）。
pub const DEFAULT_MAX_CONNS: i32 = 256;
/// 单次上游拨号总预算（N 候选共享——最坏不 N×15s 悬挂）。
pub const DIAL_BUDGET: Duration = Duration::from_secs(15);
/// 单次解析预算（套在注入 resolver 外）。
pub const RESOLVE_BUDGET: Duration = Duration::from_secs(5);
/// 方法协商 + 请求头读预算。
const HANDSHAKE_READ_BUDGET: Duration = Duration::from_secs(10);

/// 域名解析注入缝：返回按优先级排序的 IPv4 候选列表（按序拨）。消费方负责远程解析
/// （经隧道到出口代答）与缓存；本面不做任何本地解析。
pub type Resolver = Arc<dyn Fn(&str) -> Result<Vec<std::net::Ipv4Addr>, String> + Send + Sync>;

/// 服务端配置（消费方 = SocksManager）。
pub struct SocksServerConfig {
    pub resolver: Resolver,
    pub dial: CarrierDial,
    pub host: String,
    pub logf: Arc<dyn Fn(&str) + Send + Sync>,
}

/// SOCKS5 子集服务端（监听器由消费方创建并经 [`SocksServer::attach`] 挂入——
/// 服务端持有可同步释放的监听面；close 显式关全部在世连接 + 同步关监听）。
/// 按连接 id 记账（Go connreg.Registry——FIX-73：不拿连接对象当 map 键）。
pub struct SocksServer {
    cfg: SocksServerConfig,
    /// 服务端生命周期位：close 置位 → 在途拨号/解析预算收口 + 新请求拒绝。
    closed: AtomicBool,
    /// 监听失效位（serve 线程 accept 瞬态烧尽 → 中-2：状态面据此按 off 呈现 +
    /// err 如实可查——「状态说在听、实际没人受理」的反僵尸不变量）。
    dead: Mutex<Option<String>>,
    conns: Mutex<(u64, HashMap<u64, std::net::TcpStream>)>,
    /// 在役监听（close 同步取走释放——「off 之后端口立即可重用」的确定性面；
    /// serve 线程按 50ms 节拍轮询，listener 为 None = 正常收口）。
    ln: Mutex<Option<PollListener>>,
}

impl SocksServer {
    pub fn new(cfg: SocksServerConfig) -> Arc<SocksServer> {
        Arc::new(SocksServer {
            cfg,
            closed: AtomicBool::new(false),
            dead: Mutex::new(None),
            conns: Mutex::new((0, HashMap::new())),
            ln: Mutex::new(None),
        })
    }

    /// 挂入已绑定的监听器（消费方读 local_addr 后交给服务端——端口由消费方记忆）。
    pub fn attach(&self, ln: std::net::TcpListener) {
        *self.ln.lock().unwrap_or_else(|e| e.into_inner()) = Some(PollListener::new(ln));
    }

    /// 当前在世连接数（status 面可观察）。
    pub fn conns(&self) -> i32 {
        self.conns.lock().unwrap_or_else(|e| e.into_inner()).1.len() as i32
    }

    /// 受理循环（每连接一线程）；返回 Err = 监听失效（accept 瞬态错误烧尽）——
    /// 消费方按「off + err 如实呈现」收口。
    /// 监听失效原因（None = 在役健康；Some = serve 线程已退——status 面按 off 呈现）。
    pub fn dead_reason(&self) -> Option<String> {
        self.dead.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 落账「监听失效」（Q-H F4）：accept 瞬态错误烧尽 ⇒ 状态面按 off 呈现 +
    /// err 如实可查（`socks on` 因此不再被幂等短路，可重建）。`close()` **不清**
    /// 本姿态——off 后 `EntryRt` 换新对象自然归零（`socksmgr` 注释同义）。
    fn mark_dead(&self, reason: &str) {
        *self.dead.lock().unwrap_or_else(|e| e.into_inner()) = Some(reason.to_owned());
    }

    /// 【测试注入缝】下一次 `accept()` 直返 `Failed(msg)`（生产触发面 = `PollListener`
    /// 的瞬态错误烧尽分支；本缝只覆盖 serve→mark_dead→消费面接线，不硬关真 fd——
    /// 硬关会与 `PollListener::close()`/Drop 构成二次 close 同号风险，设计门 B7）。
    #[cfg(test)]
    pub(crate) fn inject_accept_failure(&self, msg: &str) {
        let mut g = self.ln.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(pl) = g.as_mut() {
            pl.inject_accept_failure(msg);
        }
    }

    pub(super) fn serve(self: &Arc<Self>) -> Result<(), String> {
        loop {
            if self.closed.load(Ordering::Acquire) {
                return Ok(());
            }
            let step = {
                let mut g = self.ln.lock().unwrap_or_else(|e| e.into_inner());
                match g.as_mut() {
                    Some(pl) => pl.accept(),
                    None => return Ok(()),
                }
            };
            match step {
                PollAccept::Conn(conn) => {
                    let id = {
                        let mut cs = self.conns.lock().unwrap_or_else(|e| e.into_inner());
                        if cs.1.len() as i32 >= DEFAULT_MAX_CONNS {
                            drop(cs);
                            (self.cfg.logf)(&format!(
                                "socks: 连接拒绝（并发上限 {DEFAULT_MAX_CONNS}）"
                            ));
                            drop(conn);
                            continue;
                        }
                        cs.0 += 1;
                        let id = cs.0;
                        cs.1.insert(id, conn);
                        id
                    };
                    let srv = Arc::clone(self);
                    // 低-1（D-2 收敛）：per-conn 线程创建失败（EMFILE/ENOMEM 级资源
                    // 耗尽）= 摘连接继续服务，不带走进程（原 expect 形态的全仓既有
                    // 面逐批收敛——本处是外部可触发面）。
                    if std::thread::Builder::new()
                        .name("hw-socks-conn".to_owned())
                        .stack_size(512 * 1024)
                        .spawn(move || {
                            srv.serve_conn_by(id);
                        })
                        .is_err()
                    {
                        self.drop_conn_by(id);
                    }
                }
                PollAccept::Idle => {} // 节拍在 PollListener::accept 的 WouldBlock 分支内（高-1）
                PollAccept::Closed => return Ok(()),
                PollAccept::Failed(err) => {
                    // Q-H F4：先落账（dead）再上抛——状态面按 off + err 如实呈现；
                    // 此前 `dead` 恒 None ⇒ accept 烧尽后 status 谎报 on、socks on 幂等短路。
                    self.mark_dead(&err);
                    return Err(err);
                }
            }
        }
    }

    /// 显式关：同步取走监听（端口立即可重用）+ RST 收口全部在世连接 + 断生命
    /// 周期位（在途拨号/解析预算随之收口）。
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        if let Some(mut l) = self.ln.lock().unwrap_or_else(|e| e.into_inner()).take() {
            l.close();
        }
        let drained: Vec<std::net::TcpStream> =
            self.conns.lock().unwrap_or_else(|e| e.into_inner()).1.drain().map(|(_, s)| s).collect();
        for s in drained {
            rst_close_tcp(&s);
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    /// 摘连接（spawn 失败路径共用 serve_conn_by 的收尾语义）。
    fn drop_conn_by(self: &Arc<Self>, id: u64) {
        (self.cfg.logf)("socks: 连接线程创建失败（资源耗尽？）——关闭该连接继续服务");
        let _ = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .remove(&id)
            .map(|s| s.shutdown(std::net::Shutdown::Both));
    }

    fn serve_conn_by(self: Arc<Self>, id: u64) {
        let conn = {
            let cs = self.conns.lock().unwrap_or_else(|e| e.into_inner());
            cs.1.get(&id).and_then(|s| s.try_clone().ok())
        };
        let Some(conn) = conn else { return };
        self.serve_conn(conn);
        // 摘表（在世计数随之收敛；socket 关闭幂等——close() 批量面已含不在场的）。
        let _ = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .1
            .remove(&id)
            .map(|s| s.shutdown(std::net::Shutdown::Both));
    }

    /// 单连接：协商 → 请求 → 解析/拨号 → 回执 → 双向透传。
    fn serve_conn(&self, mut conn: std::net::TcpStream) {
        let _ = conn.set_read_timeout(Some(HANDSHAKE_READ_BUDGET));
        let _ = conn.set_write_timeout(Some(HANDSHAKE_READ_BUDGET));
        if let Err(e) = self.negotiate(&mut conn) {
            (self.cfg.logf)(&format!("socks: 方法协商失败：{e}"));
            return;
        }
        let req = match read_request(&mut conn) {
            Ok(r) => r,
            Err(e) => {
                (self.cfg.logf)(&format!("socks: 读请求失败：{e}"));
                return;
            }
        };
        // 数据期不设期限（长连接语义）。
        let _ = conn.set_read_timeout(None);
        let _ = conn.set_write_timeout(None);

        let addrs: Vec<std::net::Ipv4Addr> = match req.atyp {
            ATYP_IPV4 | ATYP_DOMAIN => match self.resolve_targets(&req) {
                Ok(a) => a,
                Err(e) => {
                    // 解析失败：NXDOMAIN / 无 A / 超时（日志按 error 区分文案）。
                    (self.cfg.logf)(&format!(
                        "socks: 解析 {} 失败（rep=0x04）：{e}",
                        req.host
                    ));
                    let _ = reply_rep(&mut conn, REP_HOST_UNREACH);
                    return;
                }
            },
            _ => {
                let _ = reply_rep(&mut conn, REP_ADDR_NOT_SUP);
                return;
            }
        };
        let upstream = match self.dial_any(&addrs, req.port) {
            Ok(u) => u,
            Err(e) => {
                (self.cfg.logf)(&format!(
                    "socks: 拨 {}:{} 失败（rep=0x01）：{e}",
                    req.host, req.port
                ));
                let _ = reply_rep(&mut conn, REP_GENERAL_FAILURE);
                rst_close_tcp(&conn); // 上游失败本地 RST 收口（FIN 会让浏览器静默挂住）
                return;
            }
        };
        if reply_rep(&mut conn, REP_SUCCEEDED).is_err() {
            upstream.close();
            return;
        }
        pipe_half_close(&self.cfg.logf, conn, upstream);
    }

    /// 方法协商：只接受 no-auth（客户端须提供 method 0x00，否则回 0xFF 关）。
    fn negotiate(&self, conn: &mut std::net::TcpStream) -> Result<(), String> {
        let mut hdr = [0u8; 2];
        read_full(conn, &mut hdr).map_err(|e| format!("读协商头：{e}"))?;
        if hdr[0] != 0x05 {
            return Err(format!("版本 {} 非 5", hdr[0]));
        }
        let n = hdr[1] as usize;
        if n == 0 {
            return Err("方法数非法".to_owned());
        }
        let mut methods = vec![0u8; n];
        read_full(conn, &mut methods).map_err(|e| format!("读方法列表：{e}"))?;
        if methods.contains(&0x00) {
            conn.write_all(&[0x05, 0x00]).map_err(|e| format!("回方法选择：{e}"))?;
            return Ok(());
        }
        let _ = conn.write_all(&[0x05, 0xFF]);
        Err("客户端不提供 no-auth 方法".to_owned())
    }

    /// 目标 → 按序候选列表：IPv4 照拨（解析权不在本面）；域名经注入 resolver
    /// （预算在 resolver 内嵌——每查询一条新连接 + 5s 总预算；off/Close 之后的
    /// 在途解析由生命周期位在拨号/透传面收口）。
    fn resolve_targets(&self, req: &SocksRequest) -> Result<Vec<std::net::Ipv4Addr>, String> {
        if self.closed.load(Ordering::Acquire) {
            return Err("服务已关（解析收口）".to_owned());
        }
        if req.atyp == ATYP_IPV4 {
            let ip: std::net::Ipv4Addr = req
                .host
                .parse()
                .map_err(|e| format!("IPv4 目标解析失败：{e}"))?;
            return Ok(vec![ip]);
        }
        match (self.cfg.resolver)(&req.host) {
            Ok(addrs) if !addrs.is_empty() => Ok(addrs),
            Ok(_) => Err("解析返回空候选".to_owned()),
            Err(e) => Err(e),
        }
    }

    /// 按序拨候选（多 A 回退）：首个拨不通换下一个，全部不通才回错误。总预算 =
    /// 一份 DIAL_BUDGET（N 候选共享）；每候选子预算 = min(DIAL_BUDGET, 剩余按剩余
    /// 候选数均分)——单候选超时只烧掉自己的份额；服务已关/总预算耗尽不再试下一候选。
    /// （子预算 share 由拨号缝内嵌预算承载；本面在候选间复核生命周期与总预算。）
    fn dial_any(
        &self,
        addrs: &[std::net::Ipv4Addr],
        port: u16,
    ) -> Result<Arc<dyn super::StreamConn>, String> {
        let deadline = Instant::now() + DIAL_BUDGET;
        let mut last_err: Option<String> = None;
        for (i, ip) in addrs.iter().enumerate() {
            if self.closed.load(Ordering::Acquire) {
                break;
            }
            let remain = deadline.saturating_duration_since(Instant::now());
            if remain.is_zero() {
                break; // 总预算耗尽 / 服务已关——不再试下一候选
            }
            // 每候选子预算 = min(DIAL_BUDGET, 剩余按剩余候选数均分)（Go dialAny 的
            // N2 同义——单候选黑洞只烧自己的份额，总上界不破；中-8 整改：真传给
            // 拨号缝，不再各吃 15s）。
            let share = (remain / (addrs.len() - i) as u32).min(DIAL_BUDGET);
            match (self.cfg.dial.dial)(&self.cfg.host, SocketAddrV4::new(*ip, port), share).map(|c| c.io) {
                Ok(c) => return Ok(c),
                Err(e) => {
                    last_err = Some(e.to_string());
                    (self.cfg.logf)(&format!("socks: 候选 {ip} 拨不通，换下一个：{e:?}"));
                }
            }
        }
        Err(last_err.unwrap_or_else(|| "无候选".to_owned()))
    }
}

/// CONNECT 请求头产物。
struct SocksRequest {
    /// 目标主机（IPv4 串或域名）。
    host: String,
    port: u16,
    atyp: u8,
}

/// 读 CONNECT 请求头。非 CONNECT 命令回 rep=0x07 后报错收口；IPv6 ATYP 原样返回
/// （调用方回 0x08）。
fn read_request(conn: &mut std::net::TcpStream) -> Result<SocksRequest, String> {
    let mut hdr = [0u8; 4]; // VER CMD RSV ATYP
    read_full(conn, &mut hdr).map_err(|e| format!("读请求头：{e}"))?;
    if hdr[0] != 0x05 {
        return Err("请求版本非 5".to_owned());
    }
    if hdr[1] != CMD_CONNECT {
        let _ = conn.write_all(&[0x05, REP_CMD_NOT_SUP, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        return Err(format!("命令 {} 不支持（仅 CONNECT）", hdr[1]));
    }
    let addr: Vec<u8> = match hdr[3] {
        ATYP_IPV4 => read_exact_n(conn, 4).map_err(|e| format!("读目标地址：{e}"))?,
        ATYP_DOMAIN => {
            let mut l = [0u8; 1];
            read_full(conn, &mut l).map_err(|e| format!("读域名长度：{e}"))?;
            if l[0] == 0 {
                return Err("空域名".to_owned());
            }
            read_exact_n(conn, l[0] as usize).map_err(|e| format!("读目标地址：{e}"))?
        }
        ATYP_IPV6 => read_exact_n(conn, 16).map_err(|e| format!("读目标地址：{e}"))?,
        other => return Err(format!("ATYP {other} 非法")),
    };
    let mut pb = [0u8; 2];
    read_full(conn, &mut pb).map_err(|e| format!("读端口：{e}"))?;
    let port = u16::from_be_bytes(pb);
    let host = match hdr[3] {
        ATYP_IPV4 => std::net::Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]).to_string(),
        ATYP_DOMAIN => String::from_utf8_lossy(&addr).to_string(),
        _ => String::new(),
    };
    Ok(SocksRequest { host, port, atyp: hdr[3] })
}

/// 回 CONNECT 结果（BND 置零地址——代理不暴露本机绑定）。
fn reply_rep(conn: &mut std::net::TcpStream, rep: u8) -> std::io::Result<()> {
    conn.write_all(&[0x05, rep, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
}

fn read_full(conn: &mut std::net::TcpStream, buf: &mut [u8]) -> std::io::Result<()> {
    conn.read_exact(buf)
}

fn read_exact_n(conn: &mut std::net::TcpStream, n: usize) -> std::io::Result<Vec<u8>> {
    let mut v = vec![0u8; n];
    conn.read_exact(&mut v)?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{echo_listener, eventually, FakeDial};
    use super::*;
    use std::io::{Read, Write};
    use std::time::Duration;

    fn nop_log() -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(|_| {})
    }

    /// 起 SocksServer（本地随机端口）+ 假拨号缝（目标 IP → echo）。
    fn start(
        resolver_addrs: Vec<std::net::Ipv4Addr>,
        resolver_err: bool,
    ) -> (Arc<SocksServer>, u16, Arc<FakeDial>) {
        let dial = FakeDial::new();
        let (echo_addr, _h) = echo_listener();
        dial.addr_map
            .lock()
            .unwrap()
            .insert("10.9.9.9:80".parse().unwrap(), echo_addr);
        let resolver: Resolver = Arc::new(move |_name| {
            if resolver_err {
                Err("域名不存在（NXDOMAIN）".to_owned())
            } else {
                Ok(resolver_addrs.clone())
            }
        });
        let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = ln.local_addr().unwrap().port();
        let srv = SocksServer::new(SocksServerConfig {
            resolver,
            dial: dial.carrier_dial(),
            host: "a".repeat(64),
            logf: nop_log(),
        });
        srv.attach(ln);
        let s2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = s2.serve();
        });
        (srv, port, dial)
    }

    /// 最小 SOCKS5 客户端：协商 + CONNECT(IPv4/域名) → 返回已就绪流。
    fn socks_connect(port: u16, atyp: u8, host_bytes: &[u8], port_bytes: [u8; 2]) -> std::net::TcpStream {
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[0x05, 1, 0x00]).unwrap();
        let mut m = [0u8; 2];
        c.read_exact(&mut m).unwrap();
        assert_eq!(&m, &[0x05, 0x00], "no-auth 协商");
        let mut req = vec![0x05, 0x01, 0x00, atyp];
        req.extend_from_slice(host_bytes);
        req.extend_from_slice(&port_bytes);
        c.write_all(&req).unwrap();
        let mut rep = [0u8; 10];
        c.read_exact(&mut rep).unwrap();
        assert_eq!(rep[1], 0x00, "CONNECT 应成功（rep=0），实收 {rep:?}");
        c
    }

    /// IPv4 目标 CONNECT → 经拨号缝到 echo → 回读同载荷（B0-2b §十 socks 判据的
    /// 单测形态）。
    #[test]
    fn ipv4_connect_roundtrip_via_dial_seam() {
        let (_srv, port, dial) = start(vec![], false);
        let mut c = socks_connect(port, ATYP_IPV4, &[10, 9, 9, 9], [0x00, 0x50]);
        c.write_all(b"hello-socks").unwrap();
        let mut buf = [0u8; 32];
        let mut got = Vec::new();
        eventually(Duration::from_secs(5), "socks echo 回读", || {
            match c.read(&mut buf) {
                Ok(0) => false,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    got == b"hello-socks"
                }
                Err(_) => false,
            }
        });
        assert_eq!(got, b"hello-socks");
        assert!(
            dial.calls.lock().unwrap().iter().any(|c| c.contains("10.9.9.9:80")),
            "拨号缝收到任意目标形态：{:?}",
            dial.calls.lock().unwrap()
        );
    }

    /// 域名目标：经注入 resolver（多 A 按序回退——首选拨不通换下一个）。
    #[test]
    fn domain_resolve_falls_back_to_second_candidate() {
        let (echo_addr, _h) = echo_listener();
        let dial = FakeDial::new();
        // 候选 1 = 黑洞（绑定后立即 drop 的端口——拨不通）；候选 2 = echo。
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);
        dial.addr_map
            .lock()
            .unwrap()
            .insert(SocketAddrV4::new("10.9.9.1".parse().unwrap(), 80), format!("127.0.0.1:{dead_port}").parse().unwrap());
        dial.addr_map
            .lock()
            .unwrap()
            .insert(SocketAddrV4::new("10.9.9.2".parse().unwrap(), 80), echo_addr);
        let resolver: Resolver = Arc::new(move |_| {
            Ok(vec!["10.9.9.1".parse().unwrap(), "10.9.9.2".parse().unwrap()])
        });
        let ln = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = ln.local_addr().unwrap().port();
        let srv = SocksServer::new(SocksServerConfig {
            resolver,
            dial: dial.carrier_dial(),
            host: "b".repeat(64),
            logf: Arc::new(|_| {}),
        });
        srv.attach(ln);
        let s2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = s2.serve();
        });
        let mut c = socks_connect(port, ATYP_DOMAIN, b"\x0bexample.com", [0x00, 0x50]);
        c.write_all(b"fallback-ok").unwrap();
        let mut buf = [0u8; 32];
        let mut got = Vec::new();
        eventually(Duration::from_secs(8), "回退候选 echo 回读", || {
            match c.read(&mut buf) {
                Ok(0) => false,
                Ok(n) => {
                    got.extend_from_slice(&buf[..n]);
                    got == b"fallback-ok"
                }
                Err(_) => false,
            }
        });
        assert_eq!(got, b"fallback-ok");
    }

    /// 解析否定（NXDOMAIN）→ rep=0x04；上游拨号失败 → rep=0x01 + 本地 RST。
    #[test]
    fn negative_forms_rep04_rep01() {
        // ① NXDOMAIN → rep=0x04（连接保持——非 RST 形态可继续读）。
        let (_srv, port, _dial) = start(vec![], true);
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[0x05, 1, 0x00]).unwrap();
        let mut m = [0u8; 2];
        c.read_exact(&mut m).unwrap();
        c.write_all(&[0x05, 0x01, 0x00, 0x03, 0x03, b'a', b'b', b'c', 0x00, 0x50]).unwrap();
        let mut rep = [0u8; 10];
        c.read_exact(&mut rep).unwrap();
        assert_eq!(rep[1], 0x04, "NXDOMAIN 应 rep=0x04");

        // ② 拨号失败（无映射目标）→ rep=0x01。
        let (_srv2, port2, _dial2) = start(vec![], false);
        let mut c2 = std::net::TcpStream::connect(("127.0.0.1", port2)).unwrap();
        c2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c2.write_all(&[0x05, 1, 0x00]).unwrap();
        let mut m2 = [0u8; 2];
        c2.read_exact(&mut m2).unwrap();
        c2.write_all(&[0x05, 0x01, 0x00, 0x01, 10, 8, 8, 8, 0x00, 0x50]).unwrap();
        let mut rep2 = [0u8; 10];
        c2.read_exact(&mut rep2).unwrap();
        assert_eq!(rep2[1], 0x01, "拨号失败应 rep=0x01");
    }

    /// 非 CONNECT 命令（BIND）→ rep=0x07；no-auth 缺失 → 0xFF 关。
    #[test]
    fn bind_command_and_no_noauth_rejected() {
        let (_srv, port, _dial) = start(vec![], false);
        let mut c = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c.write_all(&[0x05, 1, 0x00]).unwrap();
        let mut m = [0u8; 2];
        c.read_exact(&mut m).unwrap();
        // BIND（cmd=0x02）。
        c.write_all(&[0x05, 0x02, 0x00, 0x01, 10, 9, 9, 9, 0x00, 0x50]).unwrap();
        let mut rep = [0u8; 10];
        c.read_exact(&mut rep).unwrap();
        assert_eq!(rep[1], 0x07, "BIND 应 rep=0x07");

        // 不提供 no-auth → 0xFF 后连接关。
        let mut c2 = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
        c2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        c2.write_all(&[0x05, 1, 0x02]).unwrap();
        let mut m2 = [0u8; 2];
        c2.read_exact(&mut m2).unwrap();
        assert_eq!(&m2, &[0x05, 0xFF], "无 no-auth 应回 0xFF");
    }

    /// close()：在世连接被显式关（RST——读到错误而非优雅 EOF 的语义由 OS 形态
    /// 决定，判据 = close 后 conns() 归零且连接不可再读写）。
    #[test]
    fn close_kills_inflight_conns() {
        let (srv, port, _dial) = start(vec![], false);
        let mut c = socks_connect(port, ATYP_IPV4, &[10, 9, 9, 9], [0x00, 0x50]);
        eventually(Duration::from_secs(5), "conns 计 1", || srv.conns() == 1);
        srv.close();
        eventually(Duration::from_secs(5), "conns 归零", || srv.conns() == 0);
        // close 后连接已断（读到 EOF/错误）。
        let mut buf = [0u8; 8];
        let r = c.read(&mut buf);
        assert!(matches!(r, Ok(0) | Err(_)), "close 后读应终止：{r:?}");
    }
}
