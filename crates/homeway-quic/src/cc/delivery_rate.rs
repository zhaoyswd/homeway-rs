//! 投递速率估计器（**移植**；上游 = tquic 的 `delivery_rate.rs`）。
//!
//! # 出处与许可（Apache-2.0 归属，**必留**）
//!
//! - 上游仓库：<https://github.com/Tencent/tquic>，文件
//!   `src/congestion_control/delivery_rate.rs`，commit
//!   `938e90adb460b5ff08b2bc6d11a3e1ba52c27a8d`。
//! - 许可：Apache License 2.0（原文见 `THIRD-PARTY.md` 与本仓库根 `NOTICE` 式登记）。
//! - 参考论文：<https://datatracker.ietf.org/doc/html/draft-cheng-iccrg-delivery-rate-estimation-02>。
//!
//! # 与上游的差异（**接口适配，算法逐行保真**）
//!
//! 上游把「每包发送时的估计器快照」写进框架的 `SentPacket.rate_sample_state`
//! （同一 crate 内可改），并在 `on_ack` 时读回。本仓的框架（= 本文件不得点名的异步栈）
//! 只给控制器 `on_sent(now, bytes, last_pkt_num)` / `on_ack(now, sent_time, bytes, …)`，
//! **不给包号、不给可变包对象** ⇒ 快照改存本模块的 [`SentRecord`]，由控制器侧按
//! (发送时刻, 字节数) FIFO 配对（见 `bbr3.rs` 的 `SentTable`）。逐项差异：
//!
//! | 上游 | 本仓 | 语义 |
//! |---|---|---|
//! | `SentPacket.rate_sample_state.*` | [`SentRecord`] 的字段 | 等价（同一个「发送时快照」语义） |
//! | `packet.time_acked` | 调用侧传入的 `now` | 等价（上游 = 框架在 ack 时写的时间戳） |
//! | `packet.pkt_num` | [`SentRecord::pkt_num`]（= 发送批次的最后包号） | **近似**：上游逐包号，本仓只有「数据报级」包号（同 `on_sent` 给的值）|
//! | `RateSample`/`DeliveryRateEstimator` 的**算法** | 逐行保真 | 零改动 |
//!
//! 本文件是**纯 std**（零异步栈名字、零 IO）⇒ 不入异步面清单。

use std::time::Duration;
use std::time::Instant;

/// 单包（本仓 = 单数据报）发送时的估计器快照（= 上游 `RateSamplePacketState`）。
///
/// 上游把它挂在 `SentPacket` 上；本仓由控制器侧持有的定长 FIFO 承载（见 `bbr3.rs`）。
#[derive(Debug, Clone)]
pub(super) struct SentRecord {
    /// 发送批次的最后包号（近似：上游是逐包号，见文件头差异表）。
    pub(super) pkt_num: u64,

    /// 发送时刻。
    pub(super) time_sent: Instant,

    /// 发送字节数（= 上游 `SentPacket.sent_size`）。
    pub(super) sent_size: u64,

    /// `P.first_sent_time`：发送时 C.first_sent_time 的快照。
    pub(super) first_sent_time: Option<Instant>,

    /// `P.delivered_time`：发送时 C.delivered_time 的快照（**被消费后置 `None`**，
    /// 上游同款「标记已用，避免被累积 ACK 二次取样」）。
    pub(super) delivered_time: Option<Instant>,

    /// `P.delivered`：发送时 C.delivered 的快照。
    pub(super) delivered: u64,

    /// `P.is_app_limited`：发送时的应用受限位快照。
    pub(super) is_app_limited: bool,

    /// `P.tx_in_flight`：发送时在途字节（速率采样用）。
    pub(super) tx_in_flight: u64,

    /// `P.lost`：发送时累计丢字节（用于「发送后丢了多少」）。
    pub(super) lost: u64,
}

impl SentRecord {
    /// 新建一条发送快照（快照字段由 [`DeliveryRateEstimator::on_packet_sent`] 填）。
    pub(super) fn new(pkt_num: u64, time_sent: Instant, sent_size: u64) -> Self {
        Self {
            pkt_num,
            time_sent,
            sent_size,
            first_sent_time: None,
            delivered_time: None,
            delivered: 0,
            is_app_limited: false,
            tx_in_flight: 0,
            lost: 0,
        }
    }
}

/// Rate sample output.
///
/// See
/// <https://datatracker.ietf.org/doc/html/draft-cheng-iccrg-delivery-rate-estimation-02#section-3.1.3>.
#[derive(Debug, Default, Clone)]
struct RateSample {
    /// rs.delivery_rate: The delivery rate sample (in most cases rs.delivered / rs.interval).
    delivery_rate: u64,

    /// rs.is_app_limited: The P.is_app_limited from the most recent packet delivered;
    /// indicates whether the rate sample is application-limited.
    is_app_limited: bool,

    /// rs.interval: The length of the sampling interval.
    interval: Duration,

    /// rs.delivered: The amount of data marked as delivered over the sampling interval.
    delivered: u64,

    /// rs.prior_delivered: The P.delivered count from the most recent packet delivered.
    prior_delivered: u64,

    /// rs.prior_time: The P.delivered_time from the most recent packet delivered.
    prior_time: Option<Instant>,

    /// rs.send_elapsed: Send time interval calculated from the most recent packet delivered.
    send_elapsed: Duration,

    /// rs.ack_elapsed: ACK time interval calculated from the most recent packet delivered.
    ack_elapsed: Duration,

    /// sample rtt.
    rtt: Duration,
}

/// Delivery rate estimator.
///
/// <https://datatracker.ietf.org/doc/html/draft-cheng-iccrg-delivery-rate-estimation-02#section-3.1.1>.
#[derive(Debug, Clone)]
pub(super) struct DeliveryRateEstimator {
    /// C.delivered: The total amount of data (measured in octets or in packets) delivered
    /// so far over the lifetime of the transport connection. This does not include pure ACK packets.
    delivered: u64,

    /// C.delivered_time: The wall clock time when C.delivered was last updated.
    delivered_time: Instant,

    /// C.first_sent_time: If packets are in flight, then this holds the send time of the packet that
    /// was most recently marked as delivered. Else, if the connection was recently idle, then this
    /// holds the send time of most recently sent packet.
    first_sent_time: Instant,

    /// C.app_limited: The index of the last transmitted packet marked as application-limited,
    /// or 0 if the connection is not currently application-limited.
    last_app_limited_pkt_num: u64,

    /// Record largest acked packet number to determine if app-limited state exits.
    largest_acked_pkt_num: u64,

    /// The last sent packet number.
    /// If application-limited occurs, it will be the end of last_app_limited_pkt_num.
    last_sent_pkt_num: u64,

    /// Rate sample.
    rate_sample: RateSample,
}

impl DeliveryRateEstimator {
    /// 以给定时刻构造（上游用 `Instant::now()`；本仓把时刻**显式传入**——控制器拿到的
    /// 时刻是框架给的那一个，构造期不许各取一份时钟）。
    pub(super) fn new(now: Instant) -> Self {
        Self {
            delivered: 0,
            delivered_time: now,
            first_sent_time: now,
            last_app_limited_pkt_num: 0,
            largest_acked_pkt_num: 0,
            last_sent_pkt_num: 0,
            rate_sample: RateSample::default(),
        }
    }

    /// Upon each packet transmission.
    /// See <https://datatracker.ietf.org/doc/html/draft-cheng-iccrg-delivery-rate-estimation-02#section-3.2>.
    pub(super) fn on_packet_sent(
        &mut self,
        rec: &mut SentRecord,
        bytes_in_flight: u64,
        bytes_lost: u64,
    ) {
        // no packets in flight yet?
        if bytes_in_flight == 0 {
            self.first_sent_time = rec.time_sent;
            self.delivered_time = rec.time_sent;
        }

        rec.first_sent_time = Some(self.first_sent_time);
        rec.delivered_time = Some(self.delivered_time);
        rec.delivered = self.delivered;
        rec.is_app_limited = self.is_app_limited();
        rec.tx_in_flight = bytes_in_flight;
        rec.lost = bytes_lost;

        self.last_sent_pkt_num = rec.pkt_num;
    }

    /// Update rate sampler (rs) when a packet is SACKed or ACKed.
    ///
    /// `now` = 框架给出的 ack 时刻（上游读 `packet.time_acked`，语义相同）。
    /// See <https://datatracker.ietf.org/doc/html/draft-cheng-iccrg-delivery-rate-estimation-02#section-3.3>.
    pub(super) fn update_rate_sample(&mut self, rec: &mut SentRecord, now: Instant) {
        if rec.delivered_time.is_none() {
            // Packet already SACKed or packet not acked
            return;
        }

        self.delivered = self.delivered.saturating_add(rec.sent_size);
        // note: Update rate sample after P.time_acked got update. The default Instant::now() is
        // not accurate for estimating ack_elapsed.
        self.delivered_time = now;

        // Update info using the newest packet:
        if self.rate_sample.prior_time.is_none()
            || rec.delivered > self.rate_sample.prior_delivered
        {
            self.rate_sample.prior_delivered = rec.delivered;
            self.rate_sample.prior_time = rec.delivered_time;
            self.rate_sample.is_app_limited = rec.is_app_limited;

            self.first_sent_time = rec.time_sent;
        }

        // Use each ACK to update delivery rate.
        self.rate_sample.send_elapsed = rec
            .time_sent
            .saturating_duration_since(rec.first_sent_time.unwrap_or(rec.time_sent));
        self.rate_sample.ack_elapsed = self
            .delivered_time
            .saturating_duration_since(rec.delivered_time.unwrap_or(rec.time_sent));
        self.rate_sample.rtt = self.delivered_time.saturating_duration_since(rec.time_sent);

        self.rate_sample.delivered = self
            .delivered
            .saturating_sub(self.rate_sample.prior_delivered);

        // Mark the packet as delivered once it's SACKed to
        // avoid being used again when it's cumulatively acked.
        rec.delivered_time = None;

        self.largest_acked_pkt_num = rec.pkt_num.max(self.largest_acked_pkt_num);
    }

    /// 记录框架给出的「本批最大已确认包号」（`on_end_acks` 的 `largest_acked`；上游没有
    /// 这一路输入——本仓用它兜住「一个包被 ack 但没有走 [`Self::update_rate_sample`]」的
    /// 情形，语义与上游的逐包更新同向）。
    pub(super) fn on_largest_acked(&mut self, pkt_num: u64) {
        self.largest_acked_pkt_num = pkt_num.max(self.largest_acked_pkt_num);
    }

    /// Upon receiving ACK, fill in delivery rate sample rs.
    /// See <https://datatracker.ietf.org/doc/html/draft-cheng-iccrg-delivery-rate-estimation-02#section-3.3>.
    pub(super) fn generate_rate_sample(&mut self) {
        // For each newly SACKed or ACKed packet P,
        //     `UpdateRateSample(P, rs)`
        // It's done before generate_rate_sample is called.

        // Clear app-limited field if bubble is ACKed and gone.
        if self.is_app_limited() && self.largest_acked_pkt_num > self.last_app_limited_pkt_num {
            self.set_app_limited(false);
        }

        // Nothing delivered on this ACK.
        if self.rate_sample.prior_time.is_none() {
            return;
        }

        // Use the longer of the send_elapsed and ack_elapsed.
        self.rate_sample.interval = self.rate_sample.send_elapsed.max(self.rate_sample.ack_elapsed);

        self.rate_sample.delivered = self
            .delivered
            .saturating_sub(self.rate_sample.prior_delivered);

        if self.rate_sample.interval.is_zero() {
            return;
        }

        self.rate_sample.delivery_rate = self.rate_sample.delivered * 1_000_000_u64
            / self.rate_sample.interval.as_micros() as u64;
    }

    /// Set app limited status and record the latest packet num as end of app limited mode.
    pub(super) fn set_app_limited(&mut self, is_app_limited: bool) {
        self.last_app_limited_pkt_num = if is_app_limited {
            self.last_sent_pkt_num.max(1)
        } else {
            0
        };
    }

    /// Check if application limited.
    /// See <https://datatracker.ietf.org/doc/html/draft-cheng-iccrg-delivery-rate-estimation-02#section-3.4>.
    pub(super) fn is_app_limited(&self) -> bool {
        self.last_app_limited_pkt_num != 0
    }

    /// C.delivered.
    pub(super) fn delivered(&self) -> u64 {
        self.delivered
    }

    /// rs.delivered.
    pub(super) fn sample_delivered(&self) -> u64 {
        self.rate_sample.delivered
    }

    /// rs.prior_delivered.
    pub(super) fn sample_prior_delivered(&self) -> u64 {
        self.rate_sample.prior_delivered
    }

    /// Delivery rate.
    pub(super) fn delivery_rate(&self) -> u64 {
        self.rate_sample.delivery_rate
    }

    /// Get rate sample rtt.
    pub(super) fn sample_rtt(&self) -> Duration {
        self.rate_sample.rtt
    }

    /// Check whether the current rate sample is application limited.
    pub(super) fn is_sample_app_limited(&self) -> bool {
        self.rate_sample.is_app_limited
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(pkt_num: u64, time_sent: Instant, size: u64) -> SentRecord {
        SentRecord::new(pkt_num, time_sent, size)
    }

    /// 上游 `delivery_rate_on_packet_sent` 的等价用例（快照逐字段）。
    #[test]
    fn delivery_rate_on_packet_sent() {
        let now = Instant::now();
        let mut est = DeliveryRateEstimator::new(now);
        let mut bytes_in_flight: u64 = 0;
        let bytes_lost: u64 = 0;

        let mut p1 = rec(1, now, 240);
        est.on_packet_sent(&mut p1, bytes_in_flight, bytes_lost);
        assert_eq!(est.first_sent_time, now);
        assert_eq!(est.delivered_time, now);
        assert_eq!(est.last_sent_pkt_num, 1);
        assert_eq!(p1.first_sent_time, Some(now));
        assert_eq!(p1.delivered_time, Some(now));
        assert_eq!(p1.tx_in_flight, 0);
        assert_eq!(p1.lost, 0);
        assert!(!p1.is_app_limited);

        let mut p2 = rec(2, now + Duration::from_millis(10), 240);
        bytes_in_flight += 240;
        est.on_packet_sent(&mut p2, bytes_in_flight, bytes_lost);
        assert_eq!(p2.first_sent_time, Some(now), "在途非 0 ⇒ first_sent_time 不变");
        assert_eq!(p2.tx_in_flight, 240);
    }

    /// 上游 `delivery_rate_generate_rate_sample` 的等价用例。
    #[test]
    fn delivery_rate_generate_rate_sample() {
        let now = Instant::now();
        let mut est = DeliveryRateEstimator::new(now);
        // 人为构造「有采样」的内部态（与上游用例同法）
        est.last_app_limited_pkt_num = 10;
        est.largest_acked_pkt_num = 12;
        assert!(est.is_app_limited());
        est.generate_rate_sample();
        assert!(!est.is_app_limited(), "bubble 被 ack ⇒ 退出 app-limited");
        assert_eq!(est.delivery_rate(), 0, "无采样 ⇒ 速率 0");

        est.rate_sample.send_elapsed = Duration::from_millis(20);
        est.rate_sample.ack_elapsed = Duration::from_millis(25);
        est.rate_sample.prior_delivered = 1200;
        est.rate_sample.prior_time = Some(now);
        est.delivered = 7200;
        est.generate_rate_sample();
        assert_eq!(est.rate_sample.interval, Duration::from_millis(25));
        assert_eq!(est.delivered(), 7200);
        assert_eq!(est.sample_delivered(), 6000);
        assert_eq!(est.sample_prior_delivered(), 1200);
        assert_eq!(est.delivery_rate(), 6_000_000 / 25);
    }

    /// 上游 `delivery_rate_update_rate_sample` 的等价用例（两段采样 + RTT/区间）。
    #[test]
    fn delivery_rate_update_rate_sample() {
        let now = Instant::now();
        let mut est = DeliveryRateEstimator::new(now);
        let pkt_size: u64 = 240;
        let n_pkts = 5u64;
        let mut bytes_in_flight = 0u64;

        let mut part1: Vec<SentRecord> = (0..n_pkts)
            .map(|n| rec(n, now, pkt_size))
            .collect();
        let mut part2: Vec<SentRecord> = (n_pkts..2 * n_pkts)
            .map(|n| rec(n, now + Duration::from_millis(5), pkt_size))
            .collect();

        for p in &mut part1 {
            est.on_packet_sent(p, bytes_in_flight, 0);
            bytes_in_flight += p.sent_size;
        }
        let ack1 = now + Duration::from_millis(20);
        for p in &mut part1 {
            est.update_rate_sample(p, ack1);
        }
        est.generate_rate_sample();
        assert_eq!(est.delivered(), pkt_size * n_pkts);
        assert_eq!(est.sample_delivered(), pkt_size * n_pkts);
        assert_eq!(est.sample_prior_delivered(), 0);
        assert_eq!(est.delivery_rate(), pkt_size * n_pkts * 1000 / 20);

        // 已消费的快照不得二次取样（`delivered_time = None`）
        let mut consumed = rec(99, now, pkt_size);
        consumed.delivered_time = None;
        let before = est.delivered();
        est.update_rate_sample(&mut consumed, ack1);
        assert_eq!(est.delivered(), before, "已消费的快照不再计入 delivered");

        est.set_app_limited(true);
        assert_eq!(
            est.last_sent_pkt_num,
            n_pkts - 1,
            "app-limited 记的是**当时**最后一次发送的包号（part1 的最后一包）"
        );
        est.set_app_limited(false);

        for p in &mut part2 {
            est.on_packet_sent(p, bytes_in_flight, 0);
            bytes_in_flight += p.sent_size;
        }
        let ack2 = now + Duration::from_millis(30);
        for p in &mut part2 {
            est.update_rate_sample(p, ack2);
        }
        est.generate_rate_sample();
        assert_eq!(est.delivered(), pkt_size * n_pkts * 2);
        assert_eq!(est.sample_delivered(), pkt_size * n_pkts);
        assert_eq!(est.sample_prior_delivered(), pkt_size * n_pkts);
        let send_elapsed = part2[0]
            .time_sent
            .saturating_duration_since(part1[0].time_sent);
        assert_eq!(est.rate_sample.ack_elapsed, ack2 - ack1);
        assert_eq!(est.rate_sample.send_elapsed, send_elapsed);
        assert_eq!(est.sample_rtt(), ack2 - part2[0].time_sent);
        assert!(!est.is_sample_app_limited());
    }
}
