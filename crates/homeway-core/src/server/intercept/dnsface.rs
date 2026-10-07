//! DNS 面的隧道栈内监听（R3；语义真源 `internal/server/dns_tunnel.go` + `pkg/dns`
//! 的 serveUDP/serveTCP/ServeStream）。
//!
//! FIX-60 结构：监听面**在隧道栈内**——隧道 IP:53（UDP+TCP，手机声明的 DNS）与
//! 隧道 IP:<解析腿端口>（TCP，客户端远程解析腿）是栈内真 listener，demux 优先投它
//! （`Interceptor::on_plain` 的 served_ports 面），应答源地址即隧道 IP——没有「host
//! 端口被占 ⇒ 手机解析全断」的失败模式。
//!
//! 线程面（H3）：查询一律 `submit_*` 非阻塞投 DNS 专用 worker；应答经 `DnsReply`
//! 回投通道在驱动拍内路由（UDP→原源端点 / TCP 连接→RFC1035 分帧写回）。
//! smoltcp 的 TcpSocket 一 socket 一连接 ⇒ 每端口维持监听池，Established 后转连接表、
//! 补一个新监听（并发上限 = Go maxTCPConns 64，两端口合计；满表后新 SYN 无监听可命中
//! ⇒ 栈回 RST——Go 形态是立即 Close，差异登记 §4.2 族）。

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::socket::udp::{self, Socket as UdpSocket};
use smoltcp::wire::IpEndpoint;

use crate::server::dnsproxy::{DnsProxy, SubmitOutcome};

/// TCP 客户端面（隧道内可达）连接上限（Go maxTCPConns；两端口合计）。
pub const MAX_TCP_CONNS: usize = 64;
/// TCP 客户端单消息空闲期限（挂住不发的客户端不能无限占 socket）。
const TCP_IDLE: Duration = Duration::from_secs(30);
/// 每端口维持的空闲监听 socket 数（新连接的接续面；满 64 上限后不再补）。
const LISTEN_BACKLOG: usize = 2;
/// 每连接分帧积攒上限（2B 前缀 + u16 长度场 ⇒ 报文 ≤64KB；超限按对端异常收线）。
const CONN_BUF: usize = 128 * 1024;
/// UDP 查询面缓冲（代答栈内 socket 的收包粒度）。
const UDP_RX: usize = 64 * 1024;
/// 单连接待写字节上限（F8/H1：per-conn 待写队列的**硬上界**）。隧道 TCP 面 tx buffer
/// 仅 16KB，慢读客户端不读应答时 `deliver_tcp` 入队会无界增长——超本上限即**收线**
/// （客户端按超时重试）。取 256KB = 数个大应答（≤64KB）的余量，与拦截腿 `WATERMARK`
/// 同量级；配合读侧软背压（`CONN_TX_GATE`）时正常慢读走背压、极少触发收线。
const CONN_TX_CAP: usize = 256 * 1024;
/// 读侧软背压门（H1：`c.tx` 达本阈值即**停读该连接**——贴 Go「阻塞写」语义：写不出去
/// 就不再读新查询 ⇒ 客户端发送窗最终关闭 ⇒ 内存有界，而非靠收线）。
const CONN_TX_GATE: usize = 64 * 1024;

/// DNS-over-TCP 的 2B BE 长度前缀纯解析（无 IO；pub = fuzz 可达面）。
/// `Ok(None)` = 帧未到齐；`mlen == 0` = 空 TCP 消息（Go 判异常收线——返回
/// `Some(vec![])` 由调用方收线）。
pub fn decode_tcp_frame(buf: &[u8]) -> Option<Vec<u8>> {
    if buf.len() < 2 {
        return None;
    }
    let mlen = u16::from_be_bytes([buf[0], buf[1]]) as usize;
    if buf.len() < 2 + mlen {
        return None;
    }
    Some(buf[2..2 + mlen].to_vec())
}

/// 应答路由键（tag → 归宿；提交时登记、应答时取走）。
#[derive(Clone, Copy)]
pub enum DnsRoute {
    /// 拦截层进程内腿（非隧道 IP :53 的兜底会话）——由 Interceptor 经 flow 表回投。
    UdpFlow(u64),
    /// 拦截层 **TCP** 进程内腿（非隧道 IP :53 的 TCP 兜底会话，M3）——RFC1035
    /// 帧化后经 flow 的 tx_backlog 回投。
    TcpFlow(u64),
    /// 隧道 IP:53 的栈内 UDP listener——回投原源端点。
    Udp53(IpEndpoint),
    /// 栈内 TCP 连接——RFC1035 分帧写回。
    Tcp(SocketHandle),
}

/// 一个 TCP 监听端口的面（监听池 + 连接表）。
#[derive(Default)]
struct TcpFace {
    port: u16,
    listeners: Vec<SocketHandle>,
    conns: HashMap<SocketHandle, TcpConn>,
}

struct TcpConn {
    /// 分帧积攒缓冲（跨读保留不完整帧）。
    rx: Vec<u8>,
    /// 待写字节队列（F8：RFC1035 整帧入队——2B BE 长度 + 正文；部分写余量续传）。
    /// 帧边界天然保持（字节流前缀消费），与拦截腿 `tx_backlog` + `flush_backlog` 同构；
    /// 隧道 TCP 面 tx buffer 仅 16KB，>16KB 的应答必须分多次续写才能送达（Go 阻塞写等价）。
    tx: Vec<u8>,
    last: Instant,
}



impl TcpFace {
    /// 补监听池：Established 转走后补位（总占位 = conns + listeners ≤ MAX_TCP_CONNS）。
    fn top_up(&mut self, tunnel_ip: Ipv4Addr, sockets: &mut SocketSet) {
        while self.listeners.len() < LISTEN_BACKLOG
            && self.conns.len() + self.listeners.len() < MAX_TCP_CONNS
        {
            let mut sock = TcpSocket::new(
                tcp::SocketBuffer::new(vec![0u8; 16 * 1024]),
                tcp::SocketBuffer::new(vec![0u8; 16 * 1024]),
            );
            sock.set_nagle_enabled(false);
            if sock.listen(IpEndpoint::new(tunnel_ip.into(), self.port)).is_err() {
                return;
            }
            self.listeners.push(sockets.add(sock));
        }
    }
}

/// DNS 的全部隧道栈内监听面（驱动线程独占——挂在 Interceptor 上随拍服务）。
pub struct DnsFaces {
    tunnel_ip: Ipv4Addr,
    udp53: Option<SocketHandle>,
    tcp53: TcpFace,
    resolve: Option<TcpFace>,
    pending: HashMap<u64, DnsRoute>,
    next_tag: u64,
}

impl DnsFaces {
    /// 装配：隧道 IP:53 UDP+TCP 恒建；解析腿端口（≠53 且非 0）另建 TCP 面。
    /// `served` 登记 demux 优先面（on_plain 直投栈）。
    pub fn attach(
        tunnel_ip: Ipv4Addr,
        resolve_port: u16,
        sockets: &mut SocketSet,
        served: &mut std::collections::HashSet<u16>,
    ) -> Self {
        let mut me = Self {
            tunnel_ip,
            udp53: None,
            tcp53: TcpFace { port: 53, listeners: Vec::new(), conns: HashMap::new() },
            resolve: None,
            pending: HashMap::new(),
            next_tag: 1,
        };
        let rx_meta: Vec<udp::PacketMetadata> = (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
        let tx_meta: Vec<udp::PacketMetadata> = (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
        let mut usock = UdpSocket::new(
            udp::PacketBuffer::new(rx_meta, vec![0u8; UDP_RX]),
            udp::PacketBuffer::new(tx_meta, vec![0u8; UDP_RX]),
        );
        if usock.bind(IpEndpoint::new(tunnel_ip.into(), 53)).is_ok() {
            me.udp53 = Some(sockets.add(usock));
        }
        me.tcp53.top_up(tunnel_ip, sockets);
        served.insert(53);
        if resolve_port != 0 && resolve_port != 53 {
            let mut face = TcpFace { port: resolve_port, listeners: Vec::new(), conns: HashMap::new() };
            face.top_up(tunnel_ip, sockets);
            me.resolve = Some(face);
            served.insert(resolve_port);
        }
        me
    }

    /// 登记一条路由并返回 tag（提交查询时用）。
    pub fn route_tag(&mut self, route: DnsRoute) -> u64 {
        let tag = self.next_tag;
        self.next_tag += 1;
        self.pending.insert(tag, route);
        tag
    }

    /// 取走一条路由（应答到达时；不存在 = 已收线，丢弃）。
    pub fn take_route(&mut self, tag: u64) -> Option<DnsRoute> {
        self.pending.remove(&tag)
    }

    /// 在途待答路由表条目数（F2/[门-A4] 观测面：回执回收失灵的信号面——正常应随应答
    /// 回投归零；持续增长 = 丢弃路径未回收 tag）。
    pub(crate) fn pending_len(&self) -> usize {
        self.pending.len()
    }

    /// 服务拍：读 UDP 查询 / 推进监听池 / 读 TCP 连接帧——全部 submit（非阻塞）。
    pub fn service(&mut self, dns: &Arc<DnsProxy>, sockets: &mut SocketSet) {
        if let Some(h) = self.udp53 {
            let mut buf = [0u8; UDP_RX];
            while let Ok((n, meta)) = sockets.get_mut::<UdpSocket>(h).recv_slice(&mut buf) {
                let tag = self.route_tag(DnsRoute::Udp53(meta.endpoint));
                if dns.submit_udp(tag, buf[..n].to_vec()) == SubmitOutcome::Dropped {
                    // F2：丢弃路径回收 tag（否则 pending 项永不回收）
                    self.take_route(tag);
                }
            }
        }
        // face 从 self 摘出处理（tag 登记与 face 推进的借用分离）
        let mut tcp53 = std::mem::take(&mut self.tcp53);
        self.service_face(&mut tcp53, dns, sockets);
        self.tcp53 = tcp53;
        if let Some(mut resolve) = self.resolve.take() {
            self.service_face(&mut resolve, dns, sockets);
            self.resolve = Some(resolve);
        }
    }

    fn service_face(&mut self, face: &mut TcpFace, dns: &Arc<DnsProxy>, sockets: &mut SocketSet) {
        // ① 监听池推进：非 Listen（含 Established）转连接表，随后补位
        let promoted: Vec<SocketHandle> = face
            .listeners
            .iter()
            .copied()
            .filter(|h| sockets.get_mut::<TcpSocket>(*h).state() != tcp::State::Listen)
            .collect();
        face.listeners.retain(|h| sockets.get_mut::<TcpSocket>(*h).state() == tcp::State::Listen);
        for h in promoted {
            face.conns.insert(
                h,
                TcpConn { rx: Vec::new(), tx: Vec::new(), last: Instant::now() },
            );
        }
        face.top_up(self.tunnel_ip, sockets);
        // ② 连接读：可读字节进分帧缓冲 → 完整帧逐条 submit
        let handles: Vec<SocketHandle> = face.conns.keys().copied().collect();
        for h in handles {
            // ②' 待写续写（F8：按 send_slice 余量推进——部分写余量留住，帧边界保持）
            if let Some(c) = face.conns.get_mut(&h) {
                if !c.tx.is_empty() {
                    let w = sockets
                        .get_mut::<TcpSocket>(h)
                        .send_slice(&c.tx)
                        .unwrap_or(0);
                    if w > 0 {
                        c.tx.drain(..w);
                        c.last = Instant::now();
                    }
                }
            }
            let mut dead = false;
            // 读侧软背压（H1）：待写积压达门阈值即停读本连接——不读 ⇒ 客户端发送窗
            // 最终关闭（Go 阻塞写等价），避免无界堆积；余量由 deliver_tcp 的硬上限兜底。
            let read_gated = face
                .conns
                .get(&h)
                .map(|c| c.tx.len() >= CONN_TX_GATE)
                .unwrap_or(false);
            if !read_gated {
                loop {
                    let mut chunk = [0u8; 8192];
                    match sockets.get_mut::<TcpSocket>(h).recv_slice(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            let Some(c) = face.conns.get_mut(&h) else { break };
                            if c.rx.len() + n > CONN_BUF {
                                dead = true; // 分帧积攒超限：对端异常，收线
                                break;
                            }
                            c.rx.extend_from_slice(&chunk[..n]);
                            c.last = Instant::now();
                        }
                    }
                }
            }
            if dead {
                sockets.remove(h);
                face.conns.remove(&h);
                self.pending.retain(|_, r| !matches!(r, DnsRoute::Tcp(x) if *x == h));
                continue;
            }
            // 完整帧提交（2B BE 长度前缀；len=0 按 Go「空 TCP 消息」判异常收线）
            loop {
                let msg = {
                    let Some(c) = face.conns.get_mut(&h) else { break };
                    match decode_tcp_frame(&c.rx) {
                        None => break,           // 帧未到齐（含 <2B）
                        Some(m) if m.is_empty() => {
                            // len=0：Go「空 TCP 消息」判异常收线
                            sockets.remove(h);
                            face.conns.remove(&h);
                            self.pending.retain(|_, r| !matches!(r, DnsRoute::Tcp(x) if *x == h));
                            break;
                        }
                        Some(m) => {
                            c.rx.drain(..2 + m.len());
                            m
                        }
                    }
                };
                let tag = self.route_tag(DnsRoute::Tcp(h));
                if dns.submit_tcp(tag, msg) == SubmitOutcome::Dropped {
                    // F2：丢弃路径回收 tag
                    self.take_route(tag);
                }
            }
        }
    }

    /// 应答写回（UDP :53 面——拦截腿与 TCP 面由 Interceptor::pump 分派后各投各面）。
    /// 返回 `false` = 栈 tx 满/无 socket（写不进）——调用方计 `udp_drop`（F10：此前
    /// `let _ =` 静默丢）。
    pub fn deliver_udp53(&self, sockets: &mut SocketSet, from: IpEndpoint, resp: &[u8]) -> bool {
        let Some(h) = self.udp53 else { return false };
        sockets.get_mut::<UdpSocket>(h).send_slice(resp, from).is_ok()
    }

    /// TCP 连接的 RFC1035 分帧写回（F8：per-conn 待写队列 + 部分写续传）。**先判在册**
    /// （评审 H2：应答最长 2.5s 后才回，窗口内连接可能已被 reap 摘除——smoltcp 的
    /// SocketHandle 无版本号，remove 后槽位复用会让 get_mut panic 或写进无关连接）。
    ///
    /// 整帧（2B BE 长度 + 正文）入队，续写由 `service_face` 每拍按余量推进——隧道 TCP
    /// 面 tx buffer 仅 16KB，>16KB 的应答必须分多次续写（原「余量不足整体丢帧」在 16KB
    /// buffer 下必然失败）。长度域 u16 是协议事实：`resp.len() > u16::MAX` → **收线**
    /// （对齐 Go：`writeTCPMessage` 返错 → `ServeStream` 返回 → 连接关闭）。
    pub fn deliver_tcp(&mut self, sockets: &mut SocketSet, h: SocketHandle, resp: &[u8]) {
        if !self.tcp_conn_live(h) {
            return; // 连接已收线：应答丢弃（客户端会按超时重试）
        }
        if resp.len() > u16::MAX as usize {
            self.drop_conn(sockets, h);
            return;
        }
        // H1 硬上限：待写积压 + 本帧会超上限 → 收线（客户端按超时重试）。读侧软背压
        //（`CONN_TX_GATE`）让正常慢读走背压、极少触发本分支；本分支保证**内存有界**。
        let over_cap = self
            .conn_mut(h)
            .map(|c| c.tx.len() + 2 + resp.len() > CONN_TX_CAP)
            .unwrap_or(false);
        if over_cap {
            self.drop_conn(sockets, h);
            return;
        }
        let Some(c) = self.conn_mut(h) else { return };
        c.tx.extend_from_slice(&(resp.len() as u16).to_be_bytes());
        c.tx.extend_from_slice(resp);
        c.last = Instant::now();
        // 本拍即试写（余量由 service_face 续写）
        let w = sockets.get_mut::<TcpSocket>(h).send_slice(&c.tx).unwrap_or(0);
        if w > 0 {
            c.tx.drain(..w);
        }
    }

    /// 取某连接的可变引用（两 face 任一命中）。
    fn conn_mut(&mut self, h: SocketHandle) -> Option<&mut TcpConn> {
        if self.tcp53.conns.contains_key(&h) {
            return self.tcp53.conns.get_mut(&h);
        }
        self.resolve.as_mut().and_then(|f| f.conns.get_mut(&h))
    }

    /// 摘除某连接（长度域溢出收线——F8）：清连接表 + 栈 socket + 在途 DNS 路由。
    fn drop_conn(&mut self, sockets: &mut SocketSet, h: SocketHandle) {
        let found = self.tcp53.conns.remove(&h).is_some()
            || self.resolve.as_mut().is_some_and(|f| f.conns.remove(&h).is_some());
        if found {
            sockets.remove(h);
            self.pending.retain(|_, r| !matches!(r, DnsRoute::Tcp(x) if *x == h));
        }
    }

    /// handle 是否仍在册（两 face 的连接表任一命中即可——同号不同 face 的碰撞由
    /// reap_face 的 pending 清理兜底）。
    fn tcp_conn_live(&self, h: SocketHandle) -> bool {
        self.tcp53.conns.contains_key(&h)
            || self.resolve.as_ref().map(|f| f.conns.contains_key(&h)).unwrap_or(false)
    }

    /// TCP 连接空闲回收（单消息 30s）与已关连接清理；在途 DNS 路由同步清障（H2）。
    pub fn reap(&mut self, sockets: &mut SocketSet) {
        let mut tcp53 = std::mem::take(&mut self.tcp53);
        let v1 = Self::reap_face(&mut tcp53, sockets);
        self.tcp53 = tcp53;
        for h in v1 {
            self.pending.retain(|_, r| !matches!(r, DnsRoute::Tcp(x) if *x == h));
        }
        if let Some(mut resolve) = self.resolve.take() {
            let v2 = Self::reap_face(&mut resolve, sockets);
            for h in v2 {
                self.pending.retain(|_, r| !matches!(r, DnsRoute::Tcp(x) if *x == h));
            }
            self.resolve = Some(resolve);
        }
    }

    /// 返回被收线的连接句柄（调用方负责清在途 DNS 路由——评审 H2）。
    fn reap_face(face: &mut TcpFace, sockets: &mut SocketSet) -> Vec<SocketHandle> {
        let victims: Vec<SocketHandle> = face
            .conns
            .iter()
            .filter(|(h, c)| {
                c.last.elapsed() > TCP_IDLE || {
                    let s = sockets.get_mut::<TcpSocket>(**h);
                    s.state() == tcp::State::Closed && !s.can_send()
                }
            })
            .map(|(h, _)| *h)
            .collect();
        for h in &victims {
            face.conns.remove(h);
            sockets.remove(*h);
        }
        victims
    }

    /// 全停（收工：listeners/conns 全摘）。
    pub fn close_all(&mut self, sockets: &mut SocketSet) {
        let mut tcp53 = std::mem::take(&mut self.tcp53);
        Self::close_face(&mut tcp53, sockets);
        self.tcp53 = tcp53;
        if let Some(mut resolve) = self.resolve.take() {
            Self::close_face(&mut resolve, sockets);
        }
        if let Some(h) = self.udp53.take() {
            sockets.remove(h);
        }
        self.pending.clear();
    }

    fn close_face(face: &mut TcpFace, sockets: &mut SocketSet) {
        for h in face.listeners.drain(..) {
            sockets.remove(h);
        }
        for h in face.conns.drain() {
            sockets.remove(h.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::iface::SocketSet;
    use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer};

    /// 手工塞一条连接（真实栈 socket 槽位 + 连接表项）。
    fn conn_with(sockets: &mut SocketSet, faces: &mut DnsFaces) -> SocketHandle {
        let sock = TcpSocket::new(
            SocketBuffer::new(vec![0u8; 16 * 1024]),
            SocketBuffer::new(vec![0u8; 16 * 1024]),
        );
        let h = sockets.add(sock);
        faces
            .tcp53
            .conns
            .insert(h, TcpConn { rx: Vec::new(), tx: Vec::new(), last: Instant::now() });
        h
    }

    /// F8：长度域 u16 溢出（> 65535）→ 收线（对齐 Go writeTCPMessage 返错）。
    #[test]
    fn deliver_tcp_over_u16_max_drops_conn() {
        let mut sockets = SocketSet::new(vec![]);
        let mut served = std::collections::HashSet::new();
        let mut faces = DnsFaces::attach(
            Ipv4Addr::new(100, 64, 255, 1),
            0,
            &mut sockets,
            &mut served,
        );
        let h = conn_with(&mut sockets, &mut faces);
        // 登记一条在途路由（收线须一并清障）
        let tag = faces.route_tag(DnsRoute::Tcp(h));
        assert_eq!(faces.pending_len(), 1);
        faces.deliver_tcp(&mut sockets, h, &vec![0u8; u16::MAX as usize + 1]);
        assert!(!faces.tcp_conn_live(h), "长度域溢出 → 连接收线");
        assert_eq!(faces.pending_len(), 0, "收线清在途路由 tag");
        assert!(faces.take_route(tag).is_none());
    }

    /// F8/H1：待写队列硬上限——积压 + 新帧超 `CONN_TX_CAP` 时收线（内存有界）。
    #[test]
    fn deliver_tcp_over_cap_drops_conn() {
        let mut sockets = SocketSet::new(vec![]);
        let mut served = std::collections::HashSet::new();
        let mut faces = DnsFaces::attach(
            Ipv4Addr::new(100, 64, 255, 1),
            0,
            &mut sockets,
            &mut served,
        );
        let h = conn_with(&mut sockets, &mut faces);
        // 预置接近上限的积压（未建立连接 ⇒ send_slice 写不进，全留队）
        if let Some(c) = faces.conn_mut(h) {
            c.tx.resize(CONN_TX_CAP - 100, 0);
        }
        faces.deliver_tcp(&mut sockets, h, &vec![0u8; 5000]);
        assert!(!faces.tcp_conn_live(h), "积压超上限 → 收线（有界）");
    }

    /// F8：整帧入队——2B BE 长度前缀 + 正文（无错位）；余量留待 service_face 续写。
    #[test]
    fn deliver_tcp_enqueues_intact_frame() {
        let mut sockets = SocketSet::new(vec![]);
        let mut served = std::collections::HashSet::new();
        let mut faces = DnsFaces::attach(
            Ipv4Addr::new(100, 64, 255, 1),
            0,
            &mut sockets,
            &mut served,
        );
        let h = conn_with(&mut sockets, &mut faces);
        let resp = vec![0xABu8; 20000];
        faces.deliver_tcp(&mut sockets, h, &resp);
        let c = faces.tcp53.conns.get(&h).expect("连接在册");
        // 未建立连接 → send_slice 无效态写不进 ⇒ 全帧留在待写队列
        assert_eq!(c.tx.len(), 2 + 20000, "整帧（2B 长度 + 正文）入队");
        assert_eq!(u16::from_be_bytes([c.tx[0], c.tx[1]]) as usize, 20000, "长度前缀正确");
        assert!(c.tx[2..].iter().all(|&b| b == 0xAB));
    }
}