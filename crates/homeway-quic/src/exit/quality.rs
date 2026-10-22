//! 出口观测 DTO；同步消费侧不接触承载实现类型。

use std::net::{Ipv4Addr, SocketAddr};

/// 出口实际发送路由；relay 包含摘腿保护窗（该窗内发送丢弃，不回落直连）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitPathVia {
    Direct,
    Relay,
}

/// 一条已认证绑定的被动采样。
///
/// UDP 与 sent/lost/congestion 计数为**本连接累计**（含握手和服务流）。RTT/cwnd 是当前
/// 路径瞬时值；path_epoch 只在观测到地址/承载变化时递增，快速往返迁移可能不可见。
/// lost_packets 仅为**出口发送端判丢**，不能称双向丢包。UDP 计数不含 IP/UDP/中继信封头。
///
/// 不同 connection_id 不得直接差分；跨 epoch 的差值不能归给单一路径。connection_id
/// 仅在本 ExitQuic 实例内唯一，持久化侧还须用进程/面实例身份隔离重启。
#[derive(Clone, Debug)]
pub struct ExitPathQuality {
    pub dev: [u8; 8],
    pub tunnel_ip: Ipv4Addr,
    pub tun_ip: Ipv4Addr,
    pub connection_id: u64,
    pub path_epoch: u64,
    /// 读取统计前后路径一致；false 时仅使用连接累计数，不对当前路径归因。
    pub path_stable: bool,
    pub remote: SocketAddr,
    pub via: ExitPathVia,
    pub observed_at_ms: u64,
    /// 从认证绑定到采样的时长；计数还包含此前握手字节。
    pub bound_for_ms: u64,
    pub rtt_us: u64,
    pub udp_tx_bytes: u64,
    pub udp_rx_bytes: u64,
    pub udp_tx_datagrams: u64,
    pub udp_rx_datagrams: u64,
    pub sent_packets: u64,
    pub lost_packets: u64,
    pub congestion_events: u64,
    pub cwnd: u64,
    pub datagram_send_buffer_space: u64,
    pub datagram_send_buffer_used: u64,
    /// 本连接 DATAGRAM 发送预检满 + 桥出站队列满；不含共享 socket/注入队列丢弃。
    pub send_buffer_drops: u64,
    /// 已摘绑定的最后样本；替换/撤销时并不等待关闭握手，故并非全部线上收尾包。
    pub finished: bool,
}
