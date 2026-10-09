//! 栈 B：smoltcp 栈装配 + TunDevice（IP 队列型 `phy::Device`）+ hub 判据。
//!
//! 对齐 Go 侧 `pkg/wgnet`（gVisor 栈 B）+ `wgcore/hub` 的 R1 子集（设计文档 §4）：
//! - Device 层自建（smoltcp 无 inject API）：RX 队列收 WG 解密出的明文包，TX 队列出
//!   poll 产出的明文包（驱动线程逐个 encapsulate）；
//! - **`DeviceCapabilities` 显式**：`medium = Medium::Ip`（Default 是 Ethernet ⇒ 走 ARP，
//!   隧道静默不通——评审 ③-1）、`max_transmission_unit = 1280`（坑 4/23）、
//!   `max_burst_size = None`（Some(N) 会把通告窗口硬截 N×MSS——评审 ③-2）；
//! - 入站分流照 Go hub 真口径：`dst == 本机派生隧道IP` 投栈，**其余静默丢**（无 TUN 源；
//!   计数器是新增诊断位，不打日志不 assert）；
//! - 本地地址 = `derive_tunnel_ip(secret, identity.pub)`，/32 + 默认路由（网关 = 出口
//!   隧道 IP；Medium::Ip 不做邻居解析，网关仅为路由锚点）；
//! - 每连接 Nagle 关（Go wgnet 口径：小 JSON 步进交互延迟）；
//! - **loopback 决策**（评审 ③-6）：127/8 出站在拨号入口拒绝——Go 的 gVisor 栈带
//!   lo + HandleLocal（拨 127.0.0.1 不出隧道），smoltcp 只有默认路由会把 127/8 发进
//!   隧道；显式拒绝保证两侧「环回不出隧道」语义一致。

use std::collections::VecDeque;
use std::net::{Ipv4Addr, SocketAddrV4};
use std::sync::{Arc, Mutex};

use smoltcp::iface::{Config as IfaceConfig, Interface, SocketSet};
use smoltcp::iface::SocketHandle;
use smoltcp::phy::{self, DeviceCapabilities, Medium};
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::time::Instant;
use smoltcp::wire::{HardwareAddress, IpCidr};

/// 隧道 MTU（两端契约常量，坑 4/23；v0.2.2 简洁化批起恒 1280——P2 升档机器
/// 已删除，无其它档）。
pub const MTU: usize = 1280;
/// 每方向每连接 TCP 缓冲（Go netstack 同量级：files 帧 256KB 不触发零窗）。
const TCP_BUF: usize = 1024 * 1024;
/// Device RX/TX 队列深度（包数；满则丢 + 计数——burst 吞吐位）。
pub(crate) const QUEUE_CAP: usize = 1024;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DialError {
    #[error("环回地址不进隧道（本地栈内无此路由）")]
    LoopbackRejected,
    // M3 S6/A12（设计 §5.3-A12）：**删除死变体** `TooManyConns`（`#[error("连接表已满")]`）
    // ——声明以来全仓零构造点（`StackB::connect` 不做并发上限），而 M3 起「并发上限从无到有」
    // 的失败态在 **QUIC 档**（`max_concurrent_bidi_streams=64`）由 `StreamErr::Busy`
    // 快速失败承担（§1.6），栈 B 侧接线 = 改 WG 档行为且该文件 M5 即删 ⇒ 取删除。
    // 登记：`docs/INTEROP-CRITERIA.md`「判据变更记录」M3 S6/S7 批（非判据行）。
    #[error("smoltcp: {0}")]
    Stack(String),
}

/// 出站明文包：TxToken 写入 → 驱动线程 drain 取走（缓冲按需分配、取走即弃；
/// 中-10② 的加锁面保留一次（poll 与 drain 同线程本可消锁，但 phy::Device 的
/// receive/transmit 借用约束下池锁是最简零拷贝形态——锁竞争者只有驱动线程自身）。
type SharedQueue = Arc<Mutex<VecDeque<Vec<u8>>>>;

/// IP 队列型虚拟设备：WG 侧入/出，smoltcp 侧收/发。
pub struct TunDevice {
    rx_queue: VecDeque<Vec<u8>>,
    tx_out: SharedQueue,
    rx_dropped: u64,
    /// 内层 MTU（caps 面——恒 MTU；Interface::new 构造期快照消费）。
    mtu: usize,
}

impl Default for TunDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl TunDevice {
    pub fn new() -> Self {
        Self {
            rx_queue: VecDeque::with_capacity(QUEUE_CAP),
            tx_out: Arc::new(Mutex::new(VecDeque::with_capacity(QUEUE_CAP))),
            rx_dropped: 0,
            mtu: MTU,
        }
    }

    /// 生效内层 MTU。
    pub fn mtu(&self) -> usize {
        self.mtu
    }

    /// WG 解密出的明文包入队（hub 判据后的投递面）。
    pub fn rx_push(&mut self, pkt: &[u8]) {
        if self.rx_queue.len() >= QUEUE_CAP {
            self.rx_dropped += 1;
            return;
        }
        self.rx_queue.push_back(pkt.to_vec());
    }

    /// 测试面：入站队列长度。
    #[cfg(test)]
    pub fn rx_queue_len(&self) -> usize {
        self.rx_queue.len()
    }

    /// 收队列溢出丢弃的累计计数（R8-2 归因插桩——驱动线程节流记行消费）。
    pub(crate) fn rx_dropped(&self) -> u64 {
        self.rx_dropped
    }

    /// 测试面：TX 出站队列长度（拦截层排障用）。
    #[cfg(test)]
    pub fn tx_out_len_for_test(&self) -> usize {
        self.tx_out.lock().map(|q| q.len()).unwrap_or(0)
    }

    /// 取 poll 产出的全部出站明文包（驱动线程逐个 encapsulate）。
    pub fn drain_tx(&mut self, out: &mut Vec<Vec<u8>>) {
        let mut q = self.tx_out.lock().expect("TX 队列锁中毒");
        out.extend(q.drain(..));
    }

    fn tx_handle(&self) -> SharedQueue {
        Arc::clone(&self.tx_out)
    }
}

pub struct DevRxToken {
    buf: Vec<u8>,
}

impl phy::RxToken for DevRxToken {
    // 0.14 签名：闭包收共享切片（R8-8a 迁移——此前 &mut [u8]）。
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.buf)
    }
}

pub struct DevTxToken {
    out: SharedQueue,
}

impl phy::TxToken for DevTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let ret = f(&mut buf);
        self.out.lock().expect("TX 队列锁中毒").push_back(buf);
        ret
    }
}

impl phy::Device for TunDevice {
    type RxToken<'a> = DevRxToken;
    type TxToken<'a> = DevTxToken;

    fn receive(&mut self, _t: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        self.rx_queue.pop_front().map(|buf| {
            (
                DevRxToken { buf },
                DevTxToken {
                    out: self.tx_handle(),
                },
            )
        })
    }

    fn transmit(&mut self, _t: Instant) -> Option<Self::TxToken<'_>> {
        Some(DevTxToken {
            out: self.tx_handle(),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ip; // 显式：Default=Ethernet 会走 ARP（评审 ③-1）
        caps.max_transmission_unit = self.mtu;
        // max_burst_size 保持 None：Some(N) 会把通告窗口硬截到 N×MSS（评审 ③-2）
        caps
    }
}

/// 栈 B：Interface + SocketSet + 连接表的会话态包装（驱动线程内独占）。
pub struct StackB {
    pub iface: Interface,
    pub sockets: SocketSet<'static>,
    pub device: TunDevice,
    pub tunnel_ip: Ipv4Addr,
    /// 入站包 dst ≠ 隧道 IP 的静默丢计数（hub 判据；R1 无 TUN 源）。
    pub dropped_not_for_b: u64,
    next_local_port: u16,
}

impl StackB {
    /// 装配：本地 /32 = 派生隧道 IP；默认路由网关 = 出口隧道 IP（Medium::Ip 不做邻居
    /// 解析）。`random_seed` 用时基抖动（smoltcp 建议：避免端口/序号跨启动碰撞）。
    pub fn new(tunnel_ip: Ipv4Addr, server_tunnel_ip: Ipv4Addr, now: Instant) -> Self {
        let mut device = TunDevice::new();
        let mut iface = Interface::new(
            IfaceConfig::new(HardwareAddress::Ip),
            &mut device,
            now,
        );
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(tunnel_ip.into(), 32))
                .expect("唯一地址必入表");
        });
        iface
            .routes_mut()
            .add_default_ipv4_route(server_tunnel_ip)
            .expect("路由表默认空，必成功");
        // ephemeral 端口基址**每次装配随机**（对齐内核随机端口语义）：确定性起点会让
        // 同身份跨进程重连复用同四元组，撞上出口 gVisor 拦截层尚在 idle 等待的半开
        // 连接（它用旧 seq 回 SYN-ACK，新 socket 判非法丢弃 ⇒ 建连超时；2026-10-02
        // 实测定位——同身份第二次连接必现、换身份即愈）
        let mut rnd = [0u8; 2];
        getrandom::getrandom(&mut rnd).expect("系统随机源不可用");
        Self {
            iface,
            sockets: SocketSet::new(vec![]),
            device,
            tunnel_ip,
            dropped_not_for_b: 0,
            next_local_port: 32768 + u16::from_le_bytes(rnd) % 16384,
        }
    }

    /// hub 判据（Go `hub.isForB` 同义）：dst == 本机隧道 IP 投栈；其余**静默丢** + 计数。
    pub fn inject(&mut self, pkt: &[u8]) {
        if pkt.len() >= 20 && pkt[0] >> 4 == 4 && pkt[16..20] == self.tunnel_ip.octets() {
            self.device.rx_push(pkt);
        } else {
            self.dropped_not_for_b += 1;
        }
    }

    /// 建一条主动连接（ephemeral 本地端口自管分配 + 在用查重；Nagle 关）。
    pub fn connect(&mut self, dst: SocketAddrV4) -> Result<SocketHandle, DialError> {
        if dst.ip().is_loopback() {
            return Err(DialError::LoopbackRejected); // 评审 ③-6 决策
        }
        let rx = tcp::SocketBuffer::new(vec![0u8; TCP_BUF]);
        let tx = tcp::SocketBuffer::new(vec![0u8; TCP_BUF]);
        let mut sock = TcpSocket::new(rx, tx);
        sock.set_nagle_enabled(false); // Go wgnet「Nagle 关」口径
        // R8-8a：上游 CUBIC（smoltcp 0.14 RFC 合规实现）——核心自连的 bulk 发送方
        // （测速上行/files 上传）此前是 0.11 的「整窗突发 + RTO go-back-N」形态，真机
        // 有损 WiFi 下吞吐塌陷（E2E §6 A5' 上行 bulk 退化的机制归因，同 R6.6 下行）。
        // 线上行为（wire 字节）不可见差异——性能选择，无对齐义务（R6.6 垫片同口径）。
        sock.set_congestion_control(tcp::CongestionControl::Cubic);
        let handle = self.sockets.add(sock);
        let local = self.alloc_local_port();
        let cx = self.iface.context();
        let s = self.sockets.get_mut::<TcpSocket>(handle);
        if let Err(e) = s.connect(cx, dst, local) {
            // 失败回收槽位与 256KB 缓冲（评审中-8：Unaddressable 是用户输入可触发面）
            self.sockets.remove(handle);
            return Err(DialError::Stack(format!("{e:?}")));
        }
        Ok(handle)
    }

    /// UDP 面的 ephemeral 端口（与 TCP 同一分配器——共用「在用查重」）。
    pub fn alloc_udp_port(&mut self) -> u16 {
        self.alloc_local_port()
    }

    fn alloc_local_port(&mut self) -> u16 {
        loop {
            let p = self.next_local_port;
            self.next_local_port = if self.next_local_port >= 61000 {
                32768
            } else {
                self.next_local_port + 1
            };
            // 简单递增 + 在用查重（判据面并发连接数小）
            // 在用查重：遍历代价小（判据面并发连接数小）
            let in_use = self.sockets.iter().any(|(_, s)| {
                use smoltcp::socket::AnySocket;
                TcpSocket::downcast(s)
                    .map(|tcp| tcp.local_endpoint().map(|e| e.port) == Some(p))
                    .unwrap_or(false)
            });
            if !in_use {
                return p;
            }
        }
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::iface::{Config as IfaceConfig, Interface, SocketSet};
    use smoltcp::phy::Device as _;
    use smoltcp::socket::tcp::{self, Socket as TcpSocket};
    use smoltcp::time::Instant as SmolInstant;
    use smoltcp::wire::{HardwareAddress, IpCidr};
    use std::net::{Ipv4Addr, SocketAddrV4};

    /// 两台 TunDevice 背靠背（A.tx → B.rx / B.tx → A.rx），驱动两栈 poll 直到稳定。
    fn pump(a: &mut Stack, b: &mut Stack, rounds: usize) {
        for i in 0..rounds {
            let t = SmolInstant::from_millis(i as i64);
            a.iface.poll(t, &mut a.dev, &mut a.socks);
            b.iface.poll(t, &mut b.dev, &mut b.socks);
            let mut out = Vec::new();
            a.dev.drain_tx(&mut out);
            for p in out {
                b.inject(&p);
            }
            let mut out = Vec::new();
            b.dev.drain_tx(&mut out);
            for p in out {
                a.inject(&p);
            }
        }
    }

    struct Stack {
        iface: Interface,
        socks: SocketSet<'static>,
        dev: TunDevice,
        #[allow(dead_code)]
        addr: Ipv4Addr,
    }

    impl Stack {
        fn inject(&mut self, pkt: &[u8]) {
            // 测试面直投 device 队列（不做 hub 判据——判据单测另测）
            self.dev.rx_push(pkt);
        }
    }

    fn stack(ip: Ipv4Addr) -> Stack {
        let mut dev = TunDevice::new();
        let mut iface = Interface::new(IfaceConfig::new(HardwareAddress::Ip), &mut dev, SmolInstant::from_millis(0));
        iface.update_ip_addrs(|a| a.push(IpCidr::new(ip.into(), 32)).unwrap());
        iface.routes_mut().add_default_ipv4_route(Ipv4Addr::new(100, 64, 255, 1)).unwrap();
        Stack { iface, socks: SocketSet::new(vec![]), dev, addr: ip }
    }

    /// caps 三项钉死（评审 ③-1/③-2 验收）：Medium::Ip / MTU 1280 / burst 不截窗口。
    #[test]
    fn device_caps_are_explicit() {
        let dev = TunDevice::new();
        let caps = dev.capabilities();
        assert_eq!(caps.medium, Medium::Ip, "Default=Ethernet 会走 ARP（静默不通）");
        assert_eq!(caps.max_transmission_unit, MTU);
        assert!(caps.max_burst_size.is_none(), "Some(N) 会把通告窗口硬截到 N×MSS");
    }

    /// 栈对栈 TCP：建立 → 传输 4MB → 通告窗口不被 burst 截断（单流吞吐量级验收）。
    #[test]
    fn stack_to_stack_tcp_transfer_fills_window() {
        let a_ip = Ipv4Addr::new(100, 64, 10, 1);
        let b_ip = Ipv4Addr::new(100, 64, 20, 2);
        let mut b = stack(b_ip);

        // B 侧监听
        let mut listen_sock = TcpSocket::new(
            tcp::SocketBuffer::new(vec![0u8; 128 * 1024]),
            tcp::SocketBuffer::new(vec![0u8; 128 * 1024]),
        );
        listen_sock.listen(7803).unwrap();
        let listen_h = b.socks.add(listen_sock);

        // A 侧主动连（StackB::connect 全路径：端口分配 + Nagle 关 + loopback 拒绝）
        let mut stackb_like = StackB::new(a_ip, Ipv4Addr::new(100, 64, 255, 1), SmolInstant::from_millis(0));
        let client_h = stackb_like
            .connect(SocketAddrV4::new(b_ip, 7803))
            .expect("建连");
        // 挪到 a 栈驱动（StackB 自带 iface；这里直接用其 sockets/iface 跑）
        let mut a = stackb_like_to_stack(stackb_like);

        pump(&mut a, &mut b, 50);
        assert_eq!(a.socks.get::<TcpSocket>(client_h).state(), tcp::State::Established, "A 侧应建立");

        // B accept（监听 socket 转已建立——smoltcp 单 socket listen→establish 直接复用）
        assert_eq!(b.socks.get::<TcpSocket>(listen_h).state(), tcp::State::Established, "B 侧应建立");

        // 传输 4MB（分块写 + pump 循环），测墙上时间估算吞吐量级
        let block = vec![0xABu8; 16384];
        let total = 4 * 1024 * 1024usize;
        let mut sent = 0usize;
        let mut received = 0usize;
        let t0 = std::time::Instant::now();
        let mut rounds = 0usize;
        while (sent < total || received < total) && rounds < 2000 {
            if sent < total {
                let n = a.socks.get_mut::<TcpSocket>(client_h).send_slice(&block).unwrap_or(0);
                sent += n;
            }
            pump(&mut a, &mut b, 2);
            rounds += 1;
            let mut buf = [0u8; 16384];
            loop {
                let n = b.socks.get_mut::<TcpSocket>(listen_h).recv_slice(&mut buf).unwrap_or(0);
                if n == 0 {
                    break;
                }
                received += n;
            }
        }
        let dt = t0.elapsed();
        assert_eq!(received, total, "应收满 4MB（实收 {received}）");
        let mbps = (total as f64 * 8.0) / dt.as_secs_f64() / 1e6;
        // 窗口若被截到 1×MSS（~1.2KB），4MB 需 ~3400 个 RTT 级别的往返——2000 轮内收不满；
        // 正常窗口下 2000 轮 × 2 pump 远绰绰有余。此断言即「跑满窗口」的量级验收。
        assert!(mbps > 100.0, "栈对栈吞吐 {mbps:.0}Mbps 过低（疑似窗口被截）");
    }

    /// 8s 机制钉子（D-3；评审 r1-5.4）：接收端 smoltcp 的 ACK 密度由「ingress 分批 ×
    /// poll 交错」决定——Interface::poll 先排空全部 ingress 再 egress、每 poll 每
    /// socket 至多一个累积 ACK ⇒ **一次收光一次 poll = 一团一 ACK**（真机 18 段/ACK
    /// 的机制根源）；**每 2 满段一 poll = 每 ≤2 段一 ACK**（immediate_ack_to_transmit
    /// 阈值 = 未确认 > 1×MSS）。8s（有界 drain）的整个前提是这两条 smoltcp 行为——
    /// 栈升级若改行为，本测试红（8s 时钟失效面）。
    #[test]
    fn ack_density_batched_vs_interleaved_poll() {
        let a_ip = Ipv4Addr::new(100, 64, 10, 1);
        let b_ip = Ipv4Addr::new(100, 64, 20, 2);
        let mut b = stack(b_ip);
        let mut listen_sock = TcpSocket::new(
            tcp::SocketBuffer::new(vec![0u8; 128 * 1024]),
            tcp::SocketBuffer::new(vec![0u8; 128 * 1024]),
        );
        listen_sock.listen(7804).unwrap();
        let listen_h = b.socks.add(listen_sock);
        let mut stackb_like = StackB::new(a_ip, Ipv4Addr::new(100, 64, 255, 1), SmolInstant::from_millis(0));
        let client_h = stackb_like
            .connect(SocketAddrV4::new(b_ip, 7804))
            .expect("建连");
        let mut a = stackb_like_to_stack(stackb_like);
        pump(&mut a, &mut b, 50);
        assert_eq!(a.socks.get::<TcpSocket>(client_h).state(), tcp::State::Established);
        assert_eq!(b.socks.get::<TcpSocket>(listen_h).state(), tcp::State::Established);

        // 热身：过 64KB（多轮 pump 让 cwnd 离开 2×MSS 初始窗——单轮 A-poll 才产得出
        // ≥4 段的批，两种形态才可判别）
        let block = vec![0x5au8; 4096];
        let mut warm = 0usize;
        let mut rounds = 0;
        while warm < 64 * 1024 && rounds < 400 {
            a.socks.get_mut::<TcpSocket>(client_h).send_slice(&block).unwrap_or(0);
            pump(&mut a, &mut b, 2);
            let mut buf = [0u8; 16384];
            loop {
                let n = b.socks.get_mut::<TcpSocket>(listen_h).recv_slice(&mut buf).unwrap_or(0);
                if n == 0 { break; }
                warm += n;
            }
            rounds += 1;
        }
        assert!(warm >= 64 * 1024, "热身未完成（{warm}B）");

        // 产一批 ≥4 段的下行数据（单次 A-poll 后收 A 的 tx 产物，不投 B）
        let a_poll_collect = |a: &mut Stack, t_ms: i64| -> Vec<Vec<u8>> {
            a.iface.poll(SmolInstant::from_millis(t_ms), &mut a.dev, &mut a.socks);
            let mut out = Vec::new();
            a.dev.drain_tx(&mut out);
            out
        };
        let t_ms = 10_000i64;
        // —— 形态甲（旧行为）：一次收光一次 poll
        a.socks.get_mut::<TcpSocket>(client_h).send_slice(&vec![0x5b; 8 * 1220]).unwrap_or(0);
        let mut pkts = a_poll_collect(&mut a, t_ms);
        // A 可能先发已 ACK 部分的补发/新数据——把数据包按序投 B（全部一次投）
        let n1 = pkts.len();
        for p in pkts.drain(..) {
            b.inject(&p);
        }
        assert!(n1 >= 4, "单次 A-poll 应产 ≥4 段（实测 {n1}——热身不足则 cwnd 未长）");
        b.iface.poll(SmolInstant::from_millis(t_ms), &mut b.dev, &mut b.socks);
        let mut acks = Vec::new();
        b.dev.drain_tx(&mut acks);
        assert_eq!(acks.len(), 1, "一次收光+一次 poll = 恰一个累积 ACK（实测 {}）", acks.len());
        // A 消化该 ACK（窗口推进，形态乙的批才产得出）
        for p in acks {
            a.inject(&p);
        }
        a.iface.poll(SmolInstant::from_millis(t_ms + 1), &mut a.dev, &mut a.socks);
        let mut sink = Vec::new();
        a.dev.drain_tx(&mut sink);

        // —— 形态乙（8s）：每 2 段投一次、各配一次 poll
        a.socks.get_mut::<TcpSocket>(client_h).send_slice(&vec![0x5c; 8 * 1220]).unwrap_or(0);
        let pkts = a_poll_collect(&mut a, t_ms + 2);
        let n2 = pkts.len();
        assert!(n2 >= 4, "形态乙同款批产 ≥4 段（实测 {n2}）");
        let mut ack_count = 0usize;
        for chunk in pkts.chunks(2) {
            for p in chunk {
                b.inject(p);
            }
            b.iface.poll(SmolInstant::from_millis(t_ms + 3), &mut b.dev, &mut b.socks);
            let mut acks = Vec::new();
            b.dev.drain_tx(&mut acks);
            ack_count += acks.len();
            for p in acks {
                a.inject(&p);
            }
        }
        println!("ACK 密度形态对照：批收光（{n1} 段）=1 ACK；每 2 段一 poll（{n2} 段）={ack_count} ACK");
        assert!(
            ack_count >= n2 / 2 - 1,
            "每 2 段一 poll 的 ACK 数（{ack_count}）应接近段数/2（{n2}/2）——8s 时钟形态"
        );
        let _ = MTU;
    }

    fn stackb_like_to_stack(s: StackB) -> Stack {
        Stack {
            iface: s.iface,
            socks: s.sockets,
            dev: s.device,
            addr: s.tunnel_ip,
        }
    }

    /// loopback 拒绝（评审 ③-6 决策）：127/8 出站在拨号入口被拒。
    #[test]
    fn loopback_dial_rejected() {
        let mut s = StackB::new(Ipv4Addr::new(100, 64, 1, 2), Ipv4Addr::new(100, 64, 255, 1), SmolInstant::from_millis(0));
        let r = s.connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 9999));
        assert!(matches!(r, Err(DialError::LoopbackRejected)));
    }

    /// hub 判据：dst ≠ 本机隧道 IP 的包静默丢 + 计数；dst 相符投栈。
    #[test]
    fn hub_dispatch_by_dst() {
        let my_ip = Ipv4Addr::new(100, 64, 1, 2);
        let mut s = StackB::new(my_ip, Ipv4Addr::new(100, 64, 255, 1), SmolInstant::from_millis(0));
        let mk = |dst: Ipv4Addr| {
            let mut p = vec![0u8; 40];
            p[0] = 0x45;
            let total = 40u16;
            p[2..4].copy_from_slice(&total.to_be_bytes());
            p[16..20].copy_from_slice(&dst.octets());
            p
        };
        s.inject(&mk(my_ip));
        assert_eq!(s.dropped_not_for_b, 0);
        assert_eq!(s.device.rx_queue_len(), 1);
        s.inject(&mk(Ipv4Addr::new(100, 64, 9, 9)));
        assert_eq!(s.dropped_not_for_b, 1, "非 B 包应静默丢 + 计数");
        assert_eq!(s.device.rx_queue_len(), 1, "不应投栈");
    }
}
