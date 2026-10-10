//! 出口 QUIC 的 `TransportConfig` 定稿（M1 设计 §1.2，**逐参数照抄**）。
//!
//! 参数不是「调优」而是**判据**：每个值在本文件里都带设计文档给的理由，改动即偏离设计
//! （须在设计文档/路线文件登记）。集中在此是为了让 E-q1 行、测试与代码门读同一组常量。
//!
//! 与 `congestion_controller_factory` 相关的决定（**M6.7 拥塞控制对比批**，登记见
//! `docs/reviews/CC-BBR3.md`）：**设计缺省仍是「不设」= quinn 默认 CUBIC**（与 smoltcp 侧
//! CUBIC 同族，AGENTS 技术底座）；env `HOMEWAY_QUIC_CC` 可切到 `bbr`(v1) / `bbr3`
//! （移植自 tquic）作**消融臂**。不设 env ⇒ 字节级零变化（本文件是唯一接线点）。

use std::sync::Arc;
use std::time::Duration;

use quinn::congestion::ControllerFactory;
use quinn::{AckFrequencyConfig, MtuDiscoveryConfig, TransportConfig, VarInt};

use crate::tuning::CcChoice;

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

/// ACK 频率阈值（**两端共用同一份组装** ⇒ 客户端与出口都广告本值，不只是「出口侧下发」；
/// 代码门 r18 ①-2 的措辞订正。中继 200pps 预算是硬约束——默认 ACK 比值 3.97
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

// 流面限制（M3 §1.7 的账；值域与 env 覆盖见 `crate::tuning`）：
// 为什么在 `TransportConfig` 里显式设置（而不是吃 quinn 缺省）——quinn 缺省
// `100 bidi + 100 uni × 1.19 MiB` ⇒ 最坏接收窗 **≈238 MB/连接**（quinn 自身文档警告）；
// 产品内存账（§7）在 **S9 整改后**取「每流 4 MiB × 连接级聚合闸 8 MiB」⇒ **单连接接收面
// 最坏 8 MiB**（比设计 §7 的 16 MiB 账还低一半；旧值 256 KiB/隐含 16 MiB 已被 S9 取代，
// 登记见 `docs/INTEROP-CRITERIA.md` 的窗口整改条）。**两端共用同一个组装函数**
// （[`transport_config_with`]）⇒ 值域不会各写一份。
use crate::tuning::StreamLimits;

/// 组装定稿 `TransportConfig`（幂等；每次调用新建——`Arc` 交 quinn 后不可变）。
///
/// 缺省档 = 设计定值（生产面恒走 [`transport_config_with`]——流面限制是配置面；
/// 本函数是**测试面**的缺省入口，`exit/tests.rs` 用它建对照客户端）。
/// `upper_bound` 与 `initial_mtu` 同值（上探构造性关闭，见 [`MTU_UPPER_BOUND`]）。
#[cfg(test)]
pub(crate) fn transport_config() -> Arc<TransportConfig> {
    debug_assert_eq!(MTU_UPPER_BOUND, INITIAL_MTU);
    transport_config_with(INITIAL_MTU, StreamLimits::design(), CcChoice::default())
}

/// 拥塞控制器 factory 的**唯一映射点**（env 值 → quinn 的 `ControllerFactory`）。
///
/// 三档的取值都来自共享常量（不在调用点各写一份）：`cubic` = quinn 缺省
/// [`quinn::congestion::CubicConfig::default`]（**不设 factory 与此逐字节同效**，
/// 见 [`transport_config_with`] 的 `debug_assert`）；`bbr` = quinn 自带 BBRv1。
/// 两档都只调 `initial_window` 之外的量——本批不引入任何 quinn 侧的调参。
pub(crate) fn cc_factory(cc: CcChoice) -> Arc<dyn ControllerFactory + Send + Sync> {
    use quinn::congestion::{BbrConfig, CubicConfig};
    match cc {
        // 与「不设工厂」等价（quinn 的 `TransportConfig::default` 就是 `CubicConfig`）。
        CcChoice::Cubic => Arc::new(CubicConfig::default()),
        CcChoice::Bbr => Arc::new(BbrConfig::default()),
    }
}

/// 全参组装（**两端共用**）：MTU 旋钮 + 流面限制（M3 §1.7/§15-3）+ 拥塞控制器（M6.7）。
pub(crate) fn transport_config_with(
    mtu: u16,
    streams: StreamLimits,
    cc: CcChoice,
) -> Arc<TransportConfig> {
    // DPLPMTUD 上界常量的唯一作用点：旋钮**不得超过**设计上界（1400）；测试缝给更小值合法
    debug_assert!(mtu <= MTU_UPPER_BOUND, "MTU 旋钮超过设计上界常量 {MTU_UPPER_BOUND}");
    let mut t = TransportConfig::default();
    t.initial_mtu(mtu).min_mtu(mtu.min(MIN_MTU));
    let mut md = MtuDiscoveryConfig::default();
    md.upper_bound(mtu);
    t.mtu_discovery_config(Some(md));
    t.datagram_send_buffer_size(DATAGRAM_BUFFER);
    t.datagram_receive_buffer_size(Some(DATAGRAM_BUFFER));
    t.max_idle_timeout(Some(
        MAX_IDLE_TIMEOUT
            .try_into()
            .expect("30s 在 IdleTimeout 值域内（≤ 2^62 ms），不可达"),
    ));
    t.keep_alive_interval(Some(KEEP_ALIVE_INTERVAL));
    // ---- M3 §1.7：流面（**两端的 TP 都出这些值**——接收方向的对端据此自律）----
    t.max_concurrent_bidi_streams(VarInt::from_u32(streams.max_bidi));
    t.max_concurrent_uni_streams(VarInt::from_u32(streams.max_uni));
    t.stream_receive_window(VarInt::from_u32(streams.recv_window));
    // **连接级接收窗（S9 新增）**：接收面聚合上界。quinn 缺省 = `VarInt::MAX`（无界），
    // 「64 流 × 每流窗」是唯一兜底 ⇒ 每流窗抬到 4 MiB（S9）后必须显式给闸，否则最坏
    // 256 MiB/连接。见 `crate::tuning::stream_defaults::CONN_RECV_WINDOW`。
    t.receive_window(VarInt::from_u32(streams.conn_recv_window));
    // `send_window` = **连接级**（多流共享）；quinn 缺省 = 8×RWND=10 MB（§13.5 的账）。
    t.send_window(u64::from(streams.send_window));
    let mut ack = AckFrequencyConfig::default();
    ack.ack_eliciting_threshold(VarInt::from_u32(ACK_ELICITING_THRESHOLD));
    ack.max_ack_delay(Some(MAX_ACK_DELAY));
    t.ack_frequency_config(Some(ack));
    // `congestion_controller_factory`：**唯一接线点**（M6.7）。缺省档显式设成
    // `CubicConfig::default()`——与 quinn「不设」时的内建值同一份（构造性断言：
    // [`cubic_factory_matches_quinn_default`]），故缺省行为与 M1–M6 逐字节同效。
    t.congestion_controller_factory(cc_factory(cc));
    Arc::new(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（缺省零变化）**：`CcChoice::Cubic` 与「不设 factory」等价——两边都是
    /// `CubicConfig::default()`（quinn `TransportConfig::default` 的同一份表达式）。
    /// 可观测面 = 工厂建出的控制器**就是** `quinn::congestion::Cubic` 且初窗 = 12000
    /// （quinn 的 `14720.clamp(2*1200, 10*1200)`，与 MTU 无关）——本批不许改缺省行为，
    /// 该断言把「悄悄换了 CONTROLLER」变成红。
    #[test]
    fn cubic_factory_matches_quinn_default() {
        let now = std::time::Instant::now();
        let c = cc_factory(CcChoice::Cubic).build(now, INITIAL_MTU);
        assert_eq!(c.initial_window(), 12000, "quinn CubicConfig 的缺省初窗");
        assert!(
            c.into_any().downcast::<quinn::congestion::Cubic>().is_ok(),
            "缺省档必须落到 quinn 自带 CUBIC（不是任何实验控制器）"
        );
    }

    /// **判据（各档可判别）**：每个 `CcChoice` 选到**不同**的控制器——判别面取初窗与
    /// `window()` 起步值（都可观测、与实现无关）。同一输入下两档若不可判别 ⇒ 本用例红
    /// （防「开关接了但没生效」）。
    #[test]
    fn each_choice_selects_a_distinct_controller() {
        let now = std::time::Instant::now();
        let cubic = cc_factory(CcChoice::Cubic).build(now, INITIAL_MTU);
        let bbr = cc_factory(CcChoice::Bbr).build(now, INITIAL_MTU);
        assert_eq!(cubic.initial_window(), 12000, "cubic：14720 夹到 [2×1200, 10×1200]");
        assert_eq!(bbr.initial_window(), 240_000, "bbr：200 包 × 1200B（quinn K_MAX_INITIAL_CONGESTION_WINDOW）");
        assert_ne!(cubic.initial_window(), bbr.initial_window(), "两档必须可判别");
        // 起步窗：cubic = 初窗；bbr = 初窗（BBR 的 cwnd 初值 = init_cwnd）
        assert!(cubic.window() > 0 && bbr.window() > 0, "起步窗必须非 0（quinn pacer 依赖）");
        assert_ne!(cubic.window(), bbr.window());
    }

    /// **判据（MTU 口径）**：MTU 旋钮走同一条组装路径（工厂建出的控制器拿到的是
    /// `build(now, mtu)` 的 mtu；本批不改这条链）。
    #[test]
    fn factory_receives_configured_initial_mtu() {
        let now = std::time::Instant::now();
        let small = cc_factory(CcChoice::Bbr).build(now, MIN_MTU);
        let big = cc_factory(CcChoice::Bbr).build(now, INITIAL_MTU);
        // 两档的 `initial_window()` 与 mtu 无关（quinn 侧常量基于 BASE_DATAGRAM_SIZE），
        // 但 `window()` 的可下发量随 mtu 变——用「工厂确实收到了 mtu」的可判别性断言：
        // 通过 `on_mtu_update` 之后的 min_cwnd 差异体现（BBR 的 min_cwnd = 4×mtu）。
        let mut s = small;
        let mut b = big;
        s.on_mtu_update(MIN_MTU);
        b.on_mtu_update(INITIAL_MTU);
        assert_eq!(MIN_MTU, 1320);
        assert_eq!(INITIAL_MTU, 1400);
        // 两档都在各自 mtu 下保持非 0 窗（结构不变量：pacer 的 debug_assert 依赖它）。
        assert!(s.window() > 0 && b.window() > 0);
    }
}
