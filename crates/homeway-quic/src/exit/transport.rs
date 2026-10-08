//! 出口 QUIC 的 `TransportConfig` 定稿（M1 设计 §1.2，**逐参数照抄**）。
//!
//! 参数不是「调优」而是**判据**：每个值在本文件里都带设计文档给的理由，改动即偏离设计
//! （须在设计文档/路线文件登记）。集中在此是为了让 E-q1 行、测试与代码门读同一组常量。
//!
//! 与 `congestion_controller_factory` 相关的唯一决定是「**不设**」：留 quinn 默认
//! （CUBIC），与 smoltcp 侧 CUBIC 同族（AGENTS 技术底座），不做实验性替换。

use std::sync::Arc;
use std::time::Duration;

use quinn::{AckFrequencyConfig, MtuDiscoveryConfig, TransportConfig, VarInt};

/// 初始 MTU：内层 1280 + QUIC 头；实测 `initial_mtu=1400 ⇒ max_datagram_size()=1362`
/// （M1 设计 §0.3 P2）——够装 1280 内层包。
pub(crate) const INITIAL_MTU: u16 = 1400;

/// MTU 下限（**不是**协议地板 1200）：黑障检测命中时 `current_mtu` 一跳落到此值
/// （quinn 无逐级下探），要装下 1280 内层包需 MTU ≥ 1318（1RTT 开销实测 38B）⇒ 取 1320
/// （mds 1282，余 2B）。取 1200 = 黑障后 mds 1162 ⇒ 1280 内层包全丢（隧道功能性死亡）。
pub(crate) const MIN_MTU: u16 = 1320;

/// DPLPMTUD 上界 = [`INITIAL_MTU`]（**构造性关闭上探**）：上探空间被信封头寸锁住
/// （中继上行 +11B；`1400+11+48 = 1459 ≤ 1500`，余 41B），且 `upper_bound == current`
/// ⇒ 二分区间退化 ⇒ 一个探测包都不发。**黑障检测仍活**（`MtuDiscovery` 恒建
/// `BlackHoleDetector`，与搜索态无关）⇒ 窄路径保护不丢。代价登记：黑障后停在 1320
/// 且不会自行回升（≈4.3% 线开销）；`mtu_discovery_config` **不能设 `None`**（那会连
/// 黑障检测一起关掉）。
pub(crate) const MTU_UPPER_BOUND: u16 = 1400;

/// 每连接 datagram 发送/接收缓冲（各 1 MiB）。
///
/// quinn 默认量级；**不是** 4MB——4MB 是今日 WG **单 socket 全设备共享**的量级，逐连接
/// 直译 = 32×8MiB 不可接受（M1 设计 §6.3 裁决；`docs/QUIC-BASELINE.md` 内存门槛同源）。
pub(crate) const DATAGRAM_BUFFER: usize = 1 << 20;

/// ACK 频率阈值（出口侧下发给对端；中继 200pps 预算是硬约束——默认 ACK 比值 3.97
/// ⇒ 下行上界 ≈9 Mbps，须显式开启；M1 设计 §1.2/§7.2 B3）。
pub(crate) const ACK_ELICITING_THRESHOLD: u32 = 16;

/// 一并下发的 `max_ack_delay`：不设 = 沿用对端 TP 的 25ms ⇒ 给内层 TCP 的 RTT 观测与
/// 慢启动节奏加 25ms 噪声。
pub(crate) const MAX_ACK_DELAY: Duration = Duration::from_millis(5);

/// 空闲回收（30s，暂不改）：与今日 WG 的「无流量即有界回收」量级匹配。
pub(crate) const MAX_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// 保活（10s）：中继注册腿/分配腿回收窗 90s（扫描粒度 5s）+ NAT 老化 ⇒ 需兜底；
/// 10s PING = 0.1pps，占上行预算 0.05%。
pub(crate) const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);

/// 连接迁移开关（**M1 退出口「WiFi→蜂窝连接不断」的协议前提**；`false` 时服务端把新源
/// 当陌生包丢弃、上行全死，M1 设计 §0.3 P8 的反例实测）。
pub(crate) const MIGRATION: bool = true;

/// 组装定稿 `TransportConfig`（幂等；每次调用新建——`Arc` 交 quinn 后不可变）。
pub(crate) fn transport_config() -> Arc<TransportConfig> {
    let mut t = TransportConfig::default();
    t.initial_mtu(INITIAL_MTU).min_mtu(MIN_MTU);
    let mut md = MtuDiscoveryConfig::default();
    md.upper_bound(MTU_UPPER_BOUND);
    t.mtu_discovery_config(Some(md));
    t.datagram_send_buffer_size(DATAGRAM_BUFFER);
    t.datagram_receive_buffer_size(Some(DATAGRAM_BUFFER));
    t.max_idle_timeout(Some(
        MAX_IDLE_TIMEOUT
            .try_into()
            .expect("30s 在 IdleTimeout 值域内（≤ 2^62 ms），不可达"),
    ));
    t.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    let mut ack = AckFrequencyConfig::default();
    ack.ack_eliciting_threshold(VarInt::from_u32(ACK_ELICITING_THRESHOLD));
    ack.max_ack_delay(Some(MAX_ACK_DELAY));
    t.ack_frequency_config(Some(ack));
    // `congestion_controller_factory`：**不设** = 默认 CUBIC（见模块头）
    Arc::new(t)
}
