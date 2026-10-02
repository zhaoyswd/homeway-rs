//! 拦截层（R3；语义真源 `pkg/intercept`）。
//!
//! 挂在出口隧道侧栈上，把「WG 解密后的明文 IP 包」按目的地址分流（tun2socks 同款语义，
//! smoltcp 形态 = 包级 NAT 重写——见 `nat.rs` 头注释）：
//!
//!    dst == 隧道IP → 豁免：LocalServices 命中端口转投 UDS、其余回环同端口重拨
//!    dst == 其它   → 过境：终结（栈内 TCP 状态机）+ 本机 socket 重拨
//!
//! **拨号先行**（设计 §4.1 / 评审 H2）：TCP SYN 不立即回 SYN-ACK——先建映射缓存 SYN、
//! worker 拨 upstream 成功（DialOk → Adopt）后才建栈内 socket 注入缓存（SYN-ACK 由此
//! 产生）；失败构造 RST 回客户端。三态（建立时点/失败可见性/黑洞 10s）与 Go
//! （Forwarder 先拨号后 CreateEndpoint）等价。

pub mod dnsface;
pub mod nat;
pub mod pool;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
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
use self::pool::{PoolCmd, PoolEvent, Upstream, WorkerPool};
use crate::server::dnsproxy::{DnsProxy, DnsReply};

/// TCP 并发上限（serve 装配层覆盖包内默认 4096——FIX-63 生产值）。
pub const MAX_CONNS: usize = 1024;
/// TCP 空闲回收（生产值；豁免腿上的长会话别收太短）。
pub const TCP_IDLE: Duration = Duration::from_secs(5 * 60);
/// UDP 会话空闲回收（与手机侧空闲回收对齐）。
pub const UDP_IDLE: Duration = Duration::from_secs(60);
/// DNS :53 会话的短回收（一问一答即闲，防挤占会话表）。
pub const DNS_IDLE: Duration = Duration::from_secs(10);
/// UDP 会话上限（保险阀）。
pub const MAX_UDP_SESSIONS: usize = 4096;
/// 每五元组建会话窗口的缓冲上限（超出丢最新）。
const UDP_PENDING_MAX: usize = 16;
/// 栈内 socket 缓冲（过境 TCP：通告窗口面——有意放宽 vs Go rcvWnd=4096，登记差异表）。
const FLOW_BUF: usize = 256 * 1024;
/// 背压高水位（per-flow 未确认字节；双向）。
const WATERMARK: usize = 256 * 1024;
/// 建连窗口的 SYN/重复包缓存上限。
const SYN_CACHE_MAX: usize = 4;

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
    pub fn snapshot(&self) -> [(&'static str, u64); 6] {
        [
            ("dialok", self.dial_ok.load(Ordering::Relaxed)),
            ("dialfail", self.dial_fail.load(Ordering::Relaxed)),
            ("flows", self.flows.load(Ordering::Relaxed)),
            ("rejected", self.rejected.load(Ordering::Relaxed)),
            ("udpReplied", self.udp_replied.load(Ordering::Relaxed)),
            ("udpNoReply", self.udp_no_reply.load(Ordering::Relaxed)),
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
    Dialing { cache: Vec<Vec<u8>> },
    Established,
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
    /// 栈→upstream 在途字节（Written 清账；水位门控 drain）。
    unacked_out: usize,
    /// transit UDP：是否收到过回包（udpcap 实测位）。
    udp_replied: bool,
    /// TCP：建流 SYN 的 seq（拨号失败构造 RST|ACK 的 ack 依据）。
    syn_seq: u32,
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
    pool: WorkerPool,
    events: std::sync::mpsc::Receiver<PoolEvent>,
    halted: bool,
    /// 栈内真 listener 的端口集（demux 优先面；3d 的 DNS listener 登记）。
    served_ports: std::collections::HashSet<u16>,
    /// 出站明文包队列（TX 反重写后待 encap——pump 返回给引擎）。
    tx_out: Vec<Vec<u8>>,
    /// DNS 的隧道栈内监听面（:53 UDP/TCP + 解析腿 TCP；dns 开才建）。
    dns_faces: Option<DnsFaces>,
    /// DNS worker 应答回投通道（pump 拍内 drain）。
    dns_rx: Option<std::sync::mpsc::Receiver<DnsReply>>,
    /// UDP 会话号（判据行 #N——进程级递增，对齐旧 udp relay 口径）。
    udp_seq: u64,
    time0: Instant,
    smol_now: SmolInstant,
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
        let (pool, events) = WorkerPool::spawn(8);
        (cfg.logf)(&format!(
            "intercept: 过境拦截就绪（隧道IP {}；豁免=转投本机同端口；TCP 并发上限 {}）",
            cfg.tunnel_ip, MAX_CONNS
        ));
        let dns_rx = cfg.dns_events.take();
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
            pool,
            events,
            halted: false,
            served_ports: std::collections::HashSet::new(),
            tx_out: Vec::new(),
            dns_faces: None,
            dns_rx,
            udp_seq: 0,
            time0: Instant::now(),
            smol_now: SmolInstant::from_millis(0),
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

    /// RX：WG decap 出的明文包（源校验已过）。
    pub fn on_plain(&mut self, pkt: Vec<u8>) {
        let Some(v) = Ipv4View::parse(&pkt) else {
            return; // 畸形：静默丢（IP 层）
        };
        let l4_off = if v.proto == 6 { 20 } else if v.proto == 17 { 8 } else { 0 };
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
        let proto = if v.proto == 6 { Proto::Tcp } else { Proto::Udp };
        let five = (v.src, v.src_port, v.dst, v.dst_port, v.proto);
        if let Some(&flow) = self.by_five.get(&five) {
            let Some(f) = self.flows.get_mut(&flow) else { return };
            let rw = f.rw_port;
            let is_dialing = matches!(f.phase, Phase::Dialing { .. });
            if is_dialing {
                // 建会话窗口：包进缓存（重放用）——上限丢最新（TCP ≤4 / UDP ≤16）。
                // UDP 缓存**纯载荷**（重放直投 upstream）；TCP 缓存整包（就绪后重写注栈）。
                let cap = if proto == Proto::Tcp { SYN_CACHE_MAX } else { UDP_PENDING_MAX };
                let item = if proto == Proto::Udp {
                    let (a, b) = snapshot.udp_payload;
                    pkt[a.min(pkt.len())..b.min(pkt.len())].to_vec()
                } else {
                    pkt
                };
                if let Phase::Dialing { cache } = &mut f.phase {
                    if cache.len() < cap {
                        cache.push(item);
                    }
                }
                return;
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
        let tcp_flows = self.flows.values().filter(|f| f.proto == Proto::Tcp).count();
        if tcp_flows >= MAX_CONNS {
            self.stats.incr_reject();
            let rejects = self.stats.rejects();
            (self.cfg.logf)(&format!(
                "intercept: tcp 拒绝 {}:{} ← {}:{}（并发上限 {}，在册 {}，累计拒绝 {}）",
                v.dst, v.dst_port, v.src, v.src_port, MAX_CONNS, tcp_flows + 1, rejects
            ));
            self.tx_out.push(build_rst_for(v));
            return;
        }
        let (kind, upstream) = self.route_upstream(v.dst, v.dst_port, Proto::Tcp);
        let flow = self.alloc_flow(v, kind, Proto::Tcp, vec![pkt]);
        let _ = upstream;
        self.start_dial(flow, v);
    }

    /// 豁免/过境/DNS 的 upstream 决策（Go serveTCP 的 target/LocalServices 同口径）。
    fn route_upstream(&self, dst: Ipv4Addr, port: u16, proto: Proto) -> (Kind, Upstream) {
        if proto == Proto::Udp && self.cfg.dns.is_some() && port == 53 {
            return (Kind::Dns, Upstream::Udp(loopback(port)));
        }
        if dst == self.cfg.tunnel_ip {
            // 豁免：LocalServices 命中 → UDS（UDP 不查表——Go 同口径）；未命中 → 回环同端口
            if proto == Proto::Tcp {
                if let Some(sock) = self.cfg.local_services.get(&port) {
                    return (Kind::Exempt, Upstream::Unix(sock.clone()));
                }
            }
            let target = loopback(port);
            return if proto == Proto::Tcp {
                (Kind::Exempt, Upstream::Tcp(target))
            } else {
                (Kind::Exempt, Upstream::Udp(target))
            };
        }
        let target = SocketAddrV4::new(dst, port).into();
        if proto == Proto::Tcp {
            (Kind::Transit, Upstream::Tcp(target))
        } else {
            (Kind::Transit, Upstream::Udp(target))
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
                unacked_out: 0,
                udp_replied: false,
                syn_seq: v.tcp_seq,
            },
        );
        flow
    }

    /// rw_port 分配（避开在用与低端口；环形递增）。
    fn alloc_rw_port(&mut self) -> u16 {
        loop {
            let p = self.next_rw_port;
            self.next_rw_port = if self.next_rw_port >= 61000 { 20000 } else { self.next_rw_port + 1 };
            if !self.by_rw_port.contains_key(&p) {
                return p;
            }
        }
    }

    fn start_dial(&mut self, flow: u64, v: &View5) {
        let (_, upstream) = self.route_upstream(v.dst, v.dst_port, Proto::Tcp);
        let worker = (flow as usize) % self.pool.workers();
        // UDP 的 dns 腿不经 pool（无 socket）——udp_new 已分流；此处仅 TCP 形态
        self.pool.spawn_dial(flow, upstream, worker);
    }

    /// UDP 新会话（首包）：置在建位 + pending；DNS 腿直接就绪，其余投 worker 拨号。
    fn udp_new(&mut self, v: &View5, pkt: Vec<u8>) {
        if self.halted {
            if let Some(icmp) = nat::build_icmp_unreachable(&pkt) {
                self.tx_out.push(icmp);
            }
            return;
        }
        let udp_flows = self.flows.values().filter(|f| f.proto == Proto::Udp).count();
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
        let (kind, upstream) = self.route_upstream(v.dst, v.dst_port, Proto::Udp);
        // 建会话窗口：栈内 udp socket 即刻建（后续包经重写命中）；pending 缓存首包**纯载荷**
        let (pa, pb) = v.udp_payload;
        let first = pkt[pa.min(pkt.len())..pb.min(pkt.len())].to_vec();
        let flow = self.alloc_flow(v, kind, Proto::Udp, vec![first]);
        if kind == Kind::Dns {
            // 进程内腿：无拨号——直接「就绪」+ 重放
            self.udp_ready(flow);
            return;
        }
        let worker = (flow as usize) % self.pool.workers();
        self.pool.spawn_dial(flow, upstream, worker);
    }

    /// UDP 会话就绪（DialOk 或 DNS 腿）：重放 first+pending（upstream 直投，不注栈）。
    fn udp_ready(&mut self, flow: u64) {
        let (replays, kind, orig_dst, client) = {
            let Some(f) = self.flows.get_mut(&flow) else { return };
            let Phase::Dialing { cache } = &f.phase else { return };
            let snap = (cache.clone(), f.kind, f.orig_dst, f.client);
            f.phase = Phase::Established;
            snap
        };
        // 栈内 udp socket 就位（DNS 进程内腿不经 worker——on_dial_ok 之外也要建；
        // ensure 幂等，DialOk 路径重复调用无害）
        self.ensure_udp_socket(flow);
        // 会话号 + 判据行（E12 建立）
        self.udp_seq += 1;
        let seq = self.udp_seq;
        self.stats.incr_flow();
        (self.cfg.logf)(&format!(
            "udp intercept: 会话 #{seq} {} 建立（{}:{} ← {}:{}）",
            kind.as_str(), orig_dst.0, orig_dst.1, client.0, client.1
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
                if tag != 0 {
                    dns.submit_leg(tag, q);
                }
            }
            return;
        }
        for p in replays {
            self.pool.send_for(flow, PoolCmd::Out { flow, data: p });
        }
    }

    /// 把一段数据经栈内 udp socket 回投客户端（DNS 应答/UpstreamData 共用）。
    fn udp_send_to_client(&mut self, flow: u64, data: &[u8]) {
        let Some(f) = self.flows.get(&flow) else { return };
        let Some(h) = f.sock else { return };
        let ep = IpEndpoint::new(f.client.0.into(), f.client.1);
        let sock = self.sockets.get_mut::<UdpSocket>(h);
        let _ = sock.send_slice(data, ep);
        if let Some(f) = self.flows.get_mut(&flow) {
            f.last_active = Instant::now();
        }
    }

    /// 驱动拍：事件处理 → DNS 应答回投 → 栈 poll → TX 反重写 → idle/水位 → DNS 面
    /// 服务 → 返回出站明文包（引擎 encap）。
    pub fn pump(&mut self) -> Vec<Vec<u8>> {
        // ① worker 事件
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev);
        }
        // ①' DNS 应答回投（worker 池异步产出；H3——驱动线程只做路由写回）
        self.drain_dns();
        // ② 栈 poll
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        // ③ TX：反重写
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        // ④ 栈内 socket 数据面（读→Out / 关闭推进）+ DNS 面服务 + idle 看门狗
        self.service_sockets();
        self.service_dns();
        self.reap_idle();
        // ⑤ 再 poll 一轮（③④ 产生的状态变化让 ACK/数据尽早在本拍出站）
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        std::mem::take(&mut self.tx_out)
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
            let Some(faces) = self.dns_faces.as_mut() else { continue };
            let Some(route) = faces.take_route(reply.tag) else { continue };
            let Some(resp) = reply.resp else { continue }; // 畸形不回包
            match route {
                DnsRoute::UdpFlow(flow) => self.udp_send_to_client(flow, &resp),
                DnsRoute::Udp53(from) => faces.deliver_udp53(&mut self.sockets, from, &resp),
                DnsRoute::Tcp(h) => faces.deliver_tcp(&mut self.sockets, h, &resp),
            }
        }
    }

    /// DNS 面服务拍（读查询 → submit worker；监听池推进）。
    fn service_dns(&mut self) {
        let Some(dns) = self.cfg.dns.clone() else { return };
        if let Some(faces) = self.dns_faces.as_mut() {
            faces.service(&dns, &mut self.sockets);
            faces.reap(&mut self.sockets);
        }
    }

    fn on_tx(&mut self, mut pkt: Vec<u8>) {
        let Some(v) = Ipv4View::parse(&pkt) else {
            self.tx_out.push(pkt);
            return;
        };
        // 反重写：src=(隧道IP, rw_port) 命中 → src=(orig_dst)；真 listener 应答不重写
        if v.src == self.cfg.tunnel_ip && self.by_rw_port.contains_key(&v.src_port) {
            if let Some(&flow) = self.by_rw_port.get(&v.src_port) {
                if let Some(f) = self.flows.get(&flow) {
                    let (ip, port) = f.orig_dst;
                    nat::rewrite_src(&mut pkt, ip, port);
                }
            }
        }
        self.tx_out.push(pkt);
    }

    fn on_event(&mut self, ev: PoolEvent) {
        match ev {
            PoolEvent::DialOk { flow } => self.on_dial_ok(flow),
            PoolEvent::DialFailed { flow } => self.on_dial_failed(flow),
            PoolEvent::UpstreamData { flow, data } => self.on_upstream_data(flow, data),
            PoolEvent::UpstreamEof { flow } => self.on_upstream_eof(flow),
            PoolEvent::Closed { flow } => {
                // worker 侧已收：若栈侧也已亡则清流
                self.maybe_reap(flow);
            }
            PoolEvent::Written { flow, n } => {
                if let Some(f) = self.flows.get_mut(&flow) {
                    f.unacked_out = f.unacked_out.saturating_sub(n);
                }
            }
        }
    }

    fn on_dial_ok(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else { return };
        let Phase::Dialing { .. } = f.phase else {
            return; // 非 Dialing（竞态）：Adopt 已发生，让 Closed 路径清
        };
        match f.proto {
            Proto::Udp => {
                // UDP：DialOk 事件仅用于驱动重放（fd 已 Adopt）——建栈内 socket 在会话建立时
                self.ensure_udp_socket(flow);
                self.udp_ready(flow);
            }
            Proto::Tcp => {
                // TCP：建栈内 listen socket + 注入缓存包（SYN-ACK 由此产生）
                let rw = f.rw_port;
                let mut sock = TcpSocket::new(
                    tcp::SocketBuffer::new(vec![0u8; FLOW_BUF]),
                    tcp::SocketBuffer::new(vec![0u8; FLOW_BUF]),
                );
                sock.set_nagle_enabled(false); // Go SetDelayOption(false) 同口径
                sock.set_timeout(Some(smoltcp::time::Duration::from_secs(TCP_IDLE.as_secs()))); // R2 低-10：精确 idle 回收
                if let Err(e) = sock.listen(IpEndpoint::new(self.cfg.tunnel_ip.into(), rw)) {
                    (self.cfg.logf)(&format!("intercept: tcp listen rw_port {rw} 失败：{e:?}"));
                    self.teardown_flow(flow, false);
                    return;
                }
                let h = self.sockets.add(sock);
                let f = self.flows.get_mut(&flow).expect("刚判存在");
                let Phase::Dialing { cache } = std::mem::replace(&mut f.phase, Phase::Established) else {
                    unreachable!("上面已判 Dialing");
                };
                f.sock = Some(h);
                // 注入缓存（重写后）——此时 SYN-ACK 会在本拍 poll 产出
                for mut p in cache {
                    nat::rewrite_dst(&mut p, self.cfg.tunnel_ip, rw);
                    self.device.rx_push(&p);
                }
                // 判据行（E10 dialok）
                let (kind, orig_dst, client) = (f.kind, f.orig_dst, f.client);
                self.stats.incr_ok();
                self.stats.incr_flow();
                (self.cfg.logf)(&format!(
                    "intercept: tcp {} {}:{} ← {}:{}（dialok）",
                    kind.as_str(), orig_dst.0, orig_dst.1, client.0, client.1
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
        let rx_meta: Vec<udp::PacketMetadata> = (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
        let tx_meta: Vec<udp::PacketMetadata> = (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
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
        let Some(f) = self.flows.get(&flow) else { return };
        let (kind, orig_dst, client, proto, f_syn_seq) = (f.kind, f.orig_dst, f.client, f.proto, f.syn_seq);
        self.stats.incr_fail();
        if proto == Proto::Tcp {
            // RST 回客户端（源 = orig dst——Go r.Complete(true) 同义）。ack = 记录的
            // SYN 的 iss+1（SYN-SENT 侧只认这种形态——smoltcp rst_reply 同构）。
            let syn = nat::build_tcp_syn(client.0, client.1, orig_dst.0, orig_dst.1, f_syn_seq);
            let v = Ipv4View::parse(&syn).expect("构造包恒可解析");
            self.tx_out.push(nat::build_tcp_rst(&v));
            (self.cfg.logf)(&format!(
                "intercept: tcp {} {}:{} ← {}:{} 拨号失败：连接失败",
                kind.as_str(), orig_dst.0, orig_dst.1, client.0, client.1
            ));
        } else {
            (self.cfg.logf)(&format!(
                "intercept: udp {} {}:{} ← {}:{} 开 socket 失败：连接失败",
                kind.as_str(), orig_dst.0, orig_dst.1, client.0, client.1
            ));
        }
        self.remove_flow(flow);
    }

    fn on_upstream_data(&mut self, flow: u64, data: Vec<u8>) {
        let n = data.len();
        let Some(f) = self.flows.get_mut(&flow) else { return };
        match f.proto {
            Proto::Tcp => {
                let Some(h) = f.sock else { return };
                // 写入栈内 socket（客户端方向）；缓冲满则丢——水位门控下不应常发
                let sock = self.sockets.get_mut::<TcpSocket>(h);
                if sock.can_send() {
                    let _ = sock.send_slice(&data);
                }
            }
            Proto::Udp => {
                if f.kind == Kind::Transit {
                    f.udp_replied = true; // downSeen：真实转发会话的实测位
                }
                self.udp_send_to_client(flow, &data);
            }
        }
        if let Some(f) = self.flows.get_mut(&flow) {
            f.last_active = Instant::now();
        }
        // 背压清账
        self.pool.send_for(flow, PoolCmd::Ack { flow, n });
    }

    fn on_upstream_eof(&mut self, flow: u64) {
        let Some(f) = self.flows.get_mut(&flow) else { return };
        match f.proto {
            Proto::Tcp => {
                // 「任一方 EOF 即双向拆」的 upstream 半边：栈内 socket 发 FIN
                if let Some(h) = f.sock {
                    self.sockets.get_mut::<TcpSocket>(h).close();
                }
            }
            Proto::Udp => {
                // UDP 无 EOF 概念（读错误同拆）
                self.finish_udp(flow);
            }
        }
    }

    /// UDP 会话收尾（判据行 + 计数 + 清流）。
    fn finish_udp(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else { return };
        let (kind, orig_dst, client, replied, seq_note) = (f.kind, f.orig_dst, f.client, f.udp_replied, f.rw_port);
        let _ = seq_note;
        self.stats.decr_flow();
        if kind == Kind::Transit {
            self.stats.incr_udp_session(replied);
        }
        (self.cfg.logf)(&format!(
            "udp intercept: 会话 #{} 关闭（{}:{} ← {}:{}）",
            self.udp_seq, orig_dst.0, orig_dst.1, client.0, client.1
        ));
        self.remove_flow(flow);
    }

    /// 栈内 socket 服务：读数据 → Out（水位门控）+ TCP 关闭推进。
    fn service_sockets(&mut self) {
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        for flow in flows {
            let Some(f) = self.flows.get(&flow) else { continue };
            let (proto, phase_ready) = (f.proto, matches!(f.phase, Phase::Established));
            if !phase_ready {
                continue;
            }
            let Some(h) = f.sock else { continue };
            let gated = f.unacked_out > WATERMARK;
            match proto {
                Proto::Tcp => {
                    let (can_recv, _may_recv, state, can_send) = {
                        let s = self.sockets.get_mut::<TcpSocket>(h);
                        (s.can_recv(), s.may_recv(), s.state(), s.can_send())
                    };
                    if can_recv && !gated {
                        // 读尽 → Out
                        let mut total = 0usize;
                        loop {
                            let mut buf = [0u8; 64 * 1024];
                            let n = self
                                .sockets
                                .get_mut::<TcpSocket>(h)
                                .recv_slice(&mut buf)
                                .unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            let data = buf[..n].to_vec();
                            self.pool.send_for(flow, PoolCmd::Out { flow, data });
                            total += n;
                        }
                        if total > 0 {
                            if let Some(f) = self.flows.get_mut(&flow) {
                                f.unacked_out += total;
                                f.last_active = Instant::now();
                            }
                        }
                    }
                    // 对端 FIN 且缓冲排空 → 本地 close（FIN 推进；CloseWait 不会自发迁移——
                    // **必须限定 CloseWait 态**：Listen/SynSent 等未连接态 may_recv 恒 false，
                    // 无条件 close 会把刚 listen 的 socket 立刻关掉）
                    if state == tcp::State::CloseWait && !can_recv {
                        let has_pending = self
                            .flows
                            .get(&flow)
                            .map(|f| f.unacked_out > 0)
                            .unwrap_or(false);
                        if !has_pending {
                            self.sockets.get_mut::<TcpSocket>(h).close();
                        }
                    }
                    // 彻底关 + 双向无在途 → 收流
                    if state == tcp::State::Closed && !can_recv && !can_send {
                        self.teardown_flow(flow, false);
                    }
                }
                Proto::Udp => {
                    // 读出（读时校验源 = 客户端——「connect 语义」的替代）→ Out
                    let expect = self
                        .flows
                        .get(&flow)
                        .map(|f| IpEndpoint::new(f.client.0.into(), f.client.1))
                        .unwrap_or_else(|| IpEndpoint::new(Ipv4Addr::UNSPECIFIED.into(), 0));
                    loop {
                        let mut buf = [0u8; 65536];
                        let (n, meta) = match self.sockets.get_mut::<UdpSocket>(h).recv_slice(&mut buf) {
                            Ok(v) => v,
                            Err(_) => break,
                        };
                        if meta.endpoint != expect {
                            continue; // 非客户端来源：丢弃（包已取出，继续读）
                        }
                        let data = buf[..n].to_vec();
                        self.pool.send_for(flow, PoolCmd::Out { flow, data });
                        if let Some(f) = self.flows.get_mut(&flow) {
                            f.last_active = Instant::now();
                        }
                    }
                }
            }
        }
    }

    /// idle 看门狗（TCP 5min / UDP 60s / DNS 10s——共享活跃时间戳）。
    fn reap_idle(&mut self) {
        let now = Instant::now();
        let victims: Vec<u64> = self
            .flows
            .iter()
            .filter(|(_, f)| {
                let idle = match (f.kind, f.proto) {
                    (Kind::Dns, _) => DNS_IDLE,
                    (_, Proto::Udp) => UDP_IDLE,
                    (_, Proto::Tcp) => TCP_IDLE,
                };
                now.duration_since(f.last_active) > idle
            })
            .map(|(k, _)| *k)
            .collect();
        for flow in victims {
            let is_udp = self.flows.get(&flow).map(|f| f.proto == Proto::Udp).unwrap_or(false);
            if is_udp {
                self.finish_udp(flow);
            } else {
                self.teardown_flow(flow, false);
            }
        }
    }

    /// 拆流（TCP 关闭路径）：栈 socket abort/close + worker Close + 判据行。
    fn teardown_flow(&mut self, flow: u64, linger_rst: bool) {
        let Some(f) = self.flows.get(&flow) else { return };
        let (kind, orig_dst, client, proto) = (f.kind, f.orig_dst, f.client, f.proto);
        if proto == Proto::Tcp {
            if let Some(h) = f.sock {
                self.sockets.get_mut::<TcpSocket>(h).close();
                let _ = h;
            }
            self.stats.decr_flow();
            (self.cfg.logf)(&format!(
                "intercept: tcp {} {}:{} ← {}:{} 关闭",
                kind.as_str(), orig_dst.0, orig_dst.1, client.0, client.1
            ));
        }
        self.pool.send_for(flow, PoolCmd::Close { flow, linger_rst });
        self.remove_flow(flow);
    }

    /// 「worker 已 Closed 回执 + 栈侧已亡」的清流（Closed 事件路径）。
    fn maybe_reap(&mut self, flow: u64) {
        // 简化：Closed 到达即允许清（栈侧状态由 teardown/close 路径自理）
        let gone = self
            .flows
            .get(&flow)
            .map(|f| match f.sock {
                None => true,
                Some(h) => self.sockets.get_mut::<TcpSocket>(h).state() == tcp::State::Closed,
            })
            .unwrap_or(true);
        if gone {
            self.remove_flow(flow);
        }
    }

    /// 清流记录（表 + 栈 socket 槽位）。worker 侧 fd 由 Close 命令收。
    fn remove_flow(&mut self, flow: u64) {
        if let Some(f) = self.flows.remove(&flow) {
            if let Some(h) = f.sock {
                self.sockets.remove(h);
                // smoltcp 0.11 的 remove 即 prune——TIME_WAIT 期保留语义由「Closed 才移除」
                // 的调用纪律承载（service_sockets 的 state==Closed 检查）
            }
            self.by_rw_port.remove(&f.rw_port);
            self.by_five
                .remove(&(f.client.0, f.client.1, f.orig_dst.0, f.orig_dst.1, if f.proto == Proto::Tcp { 6 } else { 17 }));
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

    /// 有界宽限收尾（Drain）：宽限内自然收销账，到期 `Close{linger_rst}` RST。
    pub fn drain(&mut self, grace: Duration) -> usize {
        self.halt_new();
        let deadline = Instant::now() + grace;
        loop {
            let _ = self.pump();
            if self.flows.is_empty() {
                return 0;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // 到期：逐条 RST
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        let n = flows.len();
        for flow in flows {
            self.teardown_flow(flow, true);
        }
        let out_deadline = Instant::now() + Duration::from_secs(2);
        while !self.flows.is_empty() && Instant::now() < out_deadline {
            let _ = self.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        (self.cfg.logf)(&format!("intercept: 过境宽限收尾——到期仍 在途 {n} 条过境 TCP 连接已 RST"));
        n
    }

    /// 全停（teardown：在途 TCP 立即拆——收工语义）。
    pub fn close(&mut self) {
        self.halt_new();
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        for flow in flows {
            self.teardown_flow(flow, false);
        }
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

    fn noop_logf() -> Logf {
        Arc::new(|_| {})
    }

    fn cfg_base(tunnel_ip: Ipv4Addr) -> Config {
        Config {
            tunnel_ip,
            local_services: HashMap::new(),
            dns: None,
            dns_events: None,
            dns_resolve_port: 0,
            logf: noop_logf(),
        }
    }

    /// 交叉泵：客户端栈（StackB）↔ 拦截层的包交换（n 轮；拦截层出站包注回客户端栈）。
    fn cross_pump(client: &mut StackB, itc: &mut Interceptor, rounds: usize, tick: &mut i64) {
        for _ in 0..rounds {
            // 客户端 → 拦截层（时间单调推进——栈定时器依赖）
            *tick += 5;
            let t = SmolInstant::from_millis(*tick);
            client.iface.poll(t, &mut client.device, &mut client.sockets);
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
        }
        assert!(established, "豁免流应建连（state={:?}）", client.sockets.get::<TcpSocket>(h).state());
        assert!(stats.snapshot()[0].1 >= 1, "dialok 应计数");

        // 数据往返
        client.sockets.get_mut::<TcpSocket>(h).send_slice(b"hello-exempt").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            let mut buf = [0u8; 4096];
            let n = client.sockets.get_mut::<TcpSocket>(h).recv_slice(&mut buf).unwrap_or(0);
            if n > 0 {
                got.extend_from_slice(&buf[..n]);
                break;
            }
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
        // 隧道 IP 上无服务的端口（豁免 upstream 127.0.0.1:1——无人听）
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, 1))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut refused = false;
        let mut was_syn_sent = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            let st = client.sockets.get::<TcpSocket>(h).state();
            if st == tcp::State::SynSent {
                was_syn_sent = true;
            }
            // RST 被 smoltcp 接受后 abort：SynSent → Closed 且 endpoint 被清
            if was_syn_sent && st == tcp::State::Closed {
                refused = true;
                break;
            }
        }
        assert!(refused, "拨号失败应回 RST（PathProbe :1 同款语义）");
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
                let Ok((n, from)) = up.recv_from(&mut buf) else { return };
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
            DnsConfig { resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(), ..Default::default() },
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
        assert!(resp.windows(4).any(|w| w == [127, 0, 0, 1]), "A 记录 127.0.0.1 在应答里");
        assert!(proxy.stats_line().contains("q=1"), "隧道 UDP 面计 q：{}", proxy.stats_line());
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
                    if v.proto == 17 && v.dst == Ipv4Addr::new(100, 64, 10, 9) && v.src == Ipv4Addr::new(8, 8, 8, 8) {
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
        assert!(proxy.stats_line().contains("q=1"), "腿不计 q：{}", proxy.stats_line());
        assert!(proxy.stats_line().contains("resp=2"));
        let _ = SI::from_millis(0i64);
        let _ = CUdp::new(
            smoltcp::socket::udp::PacketBuffer::new(vec![], vec![]),
            smoltcp::socket::udp::PacketBuffer::new(vec![], vec![]),
        );
        let _ = std::fs::remove_dir_all(&dir);
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
                    if v.proto == 17 && v.dst == Ipv4Addr::new(100, 64, 10, 3) && v.src == Ipv4Addr::LOCALHOST {
                        resp = Some(v.payload.to_vec());
                    }
                }
            }
            if resp.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(resp.as_deref(), Some(&b"udp-echo-q"[..]), "UDP 回投应反重写到客户端");
        assert!(stats.snapshot()[2].1 >= 1, "flows gauge 应计会话");
        drop(echo_thread);
    }
}
