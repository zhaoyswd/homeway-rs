//! 出口流质量窗：纯内存、单线程计数；不解析或保留应用正文。
use std::net::SocketAddrV4;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::sysock::TcpQuality;

/// 已结束流最多保留一拍；满时丢新，显式报 overflow。
pub(super) const CLOSED_CAPACITY: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowProtocol {
    Tcp,
    Udp,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowScope {
    Transit,
    Dns,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectState {
    Pending,
    Connected,
    UdpLocalReady,
    Failed,
    DnsInternal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FlowError {
    Connect,
    Timeout,
    Read,
    Write,
    InvalidFd,
    Internal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CloseReason {
    Closed,
    Idle,
    Shutdown,
    DialFailed,
    UpstreamEof,
    Error,
}

/// 字节/背压/峰值为窗增量；连接耗时、首字节等待与 total 字段为生命周期值。
/// TCP OS 重传是该 socket 的累计值，按 flow_id 求差；不把接收字节当 HTTP 请求。
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FlowQuality {
    pub flow_id: u64,
    pub device: Option<[u8; 8]>,
    pub client: SocketAddrV4,
    pub target: SocketAddrV4,
    pub protocol: FlowProtocol,
    pub scope: FlowScope,
    pub started_unix_ms: u64,
    pub elapsed_ms: u64,
    pub window_started_unix_ms: u64,
    pub ended_unix_ms: u64,
    pub window_ms: u64,
    pub closed: bool,
    pub close_reason: Option<CloseReason>,
    pub connect_ms: Option<u64>,
    pub connect_ended_unix_ms: Option<u64>,
    pub connect_state: ConnectState,
    pub error_kind: Option<FlowError>,
    pub error_unix_ms: Option<u64>,
    pub os_error: Option<i32>,
    pub up_bytes: u64,
    pub down_bytes: u64,
    pub total_up_bytes: u64,
    pub total_down_bytes: u64,
    /// 首次成功 send 有效载荷 → 首次后续 recv 有效载荷；不是 HTTP TTFB。
    /// 服务端先发数据的协议无可比较的请求起点，保持 None。
    pub first_upstream_byte_ms: Option<u64>,
    pub first_upstream_byte_unix_ms: Option<u64>,
    pub up_queue_bytes: usize,
    pub down_queue_bytes: usize,
    pub up_queue_peak_bytes: usize,
    pub down_queue_peak_bytes: usize,
    pub up_backpressure_ms: u64,
    pub down_backpressure_ms: u64,
    pub udp_queue_drops: u64,
    pub tcp_metrics: Option<TcpQuality>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InterceptorQuality {
    pub records: Vec<FlowQuality>,
    pub closed_overflow: u64,
    pub active_flows: usize,
    pub shape_backlog_bytes: usize,
    pub shape_backlog_peak_bytes: usize,
    pub reactor_pumps: u64,
    pub reactor_peak_wall_us: u64,
}

pub(super) fn millis(d: Duration) -> u64 {
    d.as_millis().min(u64::MAX as u128) as u64
}
pub(super) fn unix_ms() -> u64 {
    millis(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default(),
    )
}

/// 队列仍非空代表 drain/send 无法立即消化，按单调时钟积分；跨窗不丢不重算。
#[derive(Default)]
struct QueuePressure {
    current: usize,
    peak: usize,
    blocked_since: Option<Instant>,
    duration: Duration,
}
impl QueuePressure {
    fn observe(&mut self, bytes: usize, blocked: bool, now: Instant) {
        self.current = bytes;
        self.peak = self.peak.max(bytes);
        match (self.blocked_since, blocked) {
            (None, true) => self.blocked_since = Some(now),
            (Some(start), false) => {
                self.duration += now.saturating_duration_since(start);
                self.blocked_since = None;
            }
            _ => {}
        }
    }
    fn take(&mut self, now: Instant) -> (usize, u64) {
        if let Some(start) = self.blocked_since.as_mut() {
            self.duration += now.saturating_duration_since(*start);
            *start = now;
        }
        let peak = self.peak.max(self.current);
        self.peak = self.current;
        (peak, millis(std::mem::take(&mut self.duration)))
    }
}

pub(super) struct FlowMeter {
    pub record: FlowQuality,
    started: Instant,
    window: Instant,
    first_sent: Option<Instant>,
    first_received: bool,
    up: QueuePressure,
    down: QueuePressure,
}
impl FlowMeter {
    pub fn new(
        id: u64,
        device: Option<[u8; 8]>,
        client: SocketAddrV4,
        target: SocketAddrV4,
        protocol: FlowProtocol,
        scope: FlowScope,
    ) -> Self {
        let now = Instant::now();
        let stamp = unix_ms();
        Self {
            record: FlowQuality {
                flow_id: id,
                device,
                client,
                target,
                protocol,
                scope,
                started_unix_ms: stamp,
                elapsed_ms: 0,
                window_started_unix_ms: stamp,
                ended_unix_ms: stamp,
                window_ms: 0,
                closed: false,
                close_reason: None,
                connect_ms: None,
                connect_ended_unix_ms: None,
                connect_state: if scope == FlowScope::Dns {
                    ConnectState::DnsInternal
                } else {
                    ConnectState::Pending
                },
                error_kind: None,
                error_unix_ms: None,
                os_error: None,
                up_bytes: 0,
                down_bytes: 0,
                total_up_bytes: 0,
                total_down_bytes: 0,
                first_upstream_byte_ms: None,
                first_upstream_byte_unix_ms: None,
                up_queue_bytes: 0,
                down_queue_bytes: 0,
                up_queue_peak_bytes: 0,
                down_queue_peak_bytes: 0,
                up_backpressure_ms: 0,
                down_backpressure_ms: 0,
                udp_queue_drops: 0,
                tcp_metrics: None,
            },
            started: now,
            window: now,
            first_sent: None,
            first_received: false,
            up: QueuePressure::default(),
            down: QueuePressure::default(),
        }
    }
    pub fn elapsed_ms(&self) -> u64 {
        millis(self.started.elapsed())
    }
    pub fn connected(&mut self) {
        let elapsed = self.elapsed_ms();
        self.record.connect_ms = Some(elapsed);
        self.record.connect_ended_unix_ms =
            Some(self.record.started_unix_ms.saturating_add(elapsed));
        self.record.connect_state = match self.record.protocol {
            FlowProtocol::Tcp => ConnectState::Connected,
            FlowProtocol::Udp => ConnectState::UdpLocalReady,
        };
    }
    pub fn error(&mut self, kind: FlowError, error: Option<i32>) {
        if self.record.error_kind.is_none() {
            self.record.error_kind = Some(kind);
            self.record.error_unix_ms = Some(
                self.record
                    .started_unix_ms
                    .saturating_add(self.elapsed_ms()),
            );
            self.record.os_error = error;
        }
        self.record.close_reason = Some(CloseReason::Error);
    }
    pub fn sent(&mut self, bytes: usize, now: Instant) {
        self.record.up_bytes += bytes as u64;
        self.record.total_up_bytes += bytes as u64;
        if bytes > 0 && !self.first_received {
            self.first_sent.get_or_insert(now);
        }
    }
    pub fn received(&mut self, bytes: usize, now: Instant) {
        self.record.down_bytes += bytes as u64;
        self.record.total_down_bytes += bytes as u64;
        if bytes > 0 && !self.first_received {
            self.first_received = true;
            self.record.first_upstream_byte_ms = self
                .first_sent
                .map(|start| millis(now.saturating_duration_since(start)));
            if self.first_sent.is_some() {
                self.record.first_upstream_byte_unix_ms = Some(
                    self.record
                        .started_unix_ms
                        .saturating_add(millis(now.saturating_duration_since(self.started))),
                );
            }
        }
    }
    pub fn queue_sizes(&mut self, up: usize, down: usize) {
        self.up.current = up;
        self.up.peak = self.up.peak.max(up);
        self.down.current = down;
        self.down.peak = self.down.peak.max(down);
    }
    pub fn up_queue(&mut self, bytes: usize, blocked: bool, now: Instant) {
        self.up.observe(bytes, blocked, now);
    }
    pub fn down_queue(&mut self, bytes: usize, blocked: bool, now: Instant) {
        self.down.observe(bytes, blocked, now);
    }
    pub fn take(&mut self, closed: bool, now: Instant) -> FlowQuality {
        self.record.closed = closed;
        self.record.up_queue_bytes = self.up.current;
        self.record.down_queue_bytes = self.down.current;
        self.record.elapsed_ms = millis(now.saturating_duration_since(self.started));
        self.record.window_ms = millis(now.saturating_duration_since(self.window));
        // Monotonic duration gives stable boundaries if wall clock adjusts while running.
        self.record.ended_unix_ms = self
            .record
            .started_unix_ms
            .saturating_add(self.record.elapsed_ms);
        (
            self.record.up_queue_peak_bytes,
            self.record.up_backpressure_ms,
        ) = self.up.take(now);
        (
            self.record.down_queue_peak_bytes,
            self.record.down_backpressure_ms,
        ) = self.down.take(now);
        if closed && self.record.close_reason.is_none() {
            self.record.close_reason = Some(CloseReason::Closed);
        }
        let record = self.record.clone();
        self.record.up_bytes = 0;
        self.record.down_bytes = 0;
        self.record.udp_queue_drops = 0;
        self.record.window_started_unix_ms = self.record.ended_unix_ms;
        self.window = now;
        record
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn meter() -> FlowMeter {
        FlowMeter::new(
            1,
            None,
            "100.64.1.2:1234".parse().unwrap(),
            "1.2.3.4:443".parse().unwrap(),
            FlowProtocol::Tcp,
            FlowScope::Transit,
        )
    }
    #[test]
    fn first_byte_starts_at_actual_send_and_windows_do_not_repeat_bytes() {
        let mut m = meter();
        let base = m.started;
        m.sent(12, base + Duration::from_secs(2));
        m.received(40, base + Duration::from_millis(2250));
        let a = m.take(false, base + Duration::from_secs(5));
        assert_eq!(
            (a.up_bytes, a.down_bytes, a.first_upstream_byte_ms),
            (12, 40, Some(250))
        );
        let b = m.take(true, base + Duration::from_secs(6));
        assert_eq!(
            (
                b.up_bytes,
                b.down_bytes,
                b.total_up_bytes,
                b.total_down_bytes
            ),
            (0, 0, 12, 40)
        );
        assert_eq!((b.window_ms, b.closed), (1000, true));
        assert_eq!(b.window_started_unix_ms, a.ended_unix_ms);
    }
    #[test]
    fn server_first_data_and_udp_ready_are_not_http_or_reachability_measurements() {
        let mut m = meter();
        let t = m.started;
        m.received(30, t);
        m.sent(20, t + Duration::from_secs(1));
        m.received(40, t + Duration::from_secs(2));
        assert_eq!(
            m.take(true, t + Duration::from_secs(3))
                .first_upstream_byte_ms,
            None
        );
        m.record.protocol = FlowProtocol::Udp;
        m.connected();
        assert_eq!(m.record.connect_state, ConnectState::UdpLocalReady);
    }
    #[test]
    fn backpressure_crosses_windows_without_loss_or_double_counting() {
        let mut m = meter();
        let t = m.started;
        m.up_queue(123, true, t + Duration::from_secs(1));
        m.down_queue(456, true, t + Duration::from_secs(2));
        let a = m.take(false, t + Duration::from_secs(5));
        assert_eq!((a.up_backpressure_ms, a.down_backpressure_ms), (4000, 3000));
        assert_eq!((a.up_queue_peak_bytes, a.down_queue_peak_bytes), (123, 456));
        m.up_queue(0, false, t + Duration::from_secs(6));
        m.down_queue(0, false, t + Duration::from_secs(7));
        let b = m.take(true, t + Duration::from_secs(8));
        assert_eq!((b.up_backpressure_ms, b.down_backpressure_ms), (1000, 2000));
    }
}
