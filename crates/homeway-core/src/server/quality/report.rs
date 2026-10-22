use super::{hex_device, now_ms, storage, Event, PathRecord, QualityError, Record, SAMPLE_MS};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;

/// 查询区间 [from_ms,to_ms)。设备使用完整 8 字节 devTag 的 16 位 hex。
#[derive(Debug, Clone)]
pub struct QualityQuery {
    pub from_ms: u64,
    pub to_ms: u64,
    pub device: Option<String>,
}
impl Default for QualityQuery {
    fn default() -> Self {
        let to_ms = now_ms();
        Self {
            from_ms: to_ms.saturating_sub(600_000),
            to_ms,
            device: None,
        }
    }
}

#[derive(Debug, Serialize, Default)]
pub struct Coverage {
    pub oldest_ms: Option<u64>,
    pub newest_ms: Option<u64>,
    pub heartbeat_samples: u64,
    pub runs: usize,
    pub restart_markers: u64,
    pub corrupt_lines: u64,
    pub partial_tail_lines: u64,
    pub sample_gap_count: u64,
    pub sample_gap_ms: u64,
    pub boundary_intervals_excluded: u64,
    /// 窗口内所见最大累计值（每进程累计，不声称为窗口增量）。
    pub queue_dropped_observed: u64,
    pub storage_errors_observed: u64,
    pub finished_overflow_observed: u64,
    pub closed_overflow: u64,
    pub sampling_delay_peak_ms: u64,
    pub reactor_pumps: u64,
    pub reactor_peak_wall_us: u64,
    pub shape_backlog_peak_bytes: usize,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize, Default)]
pub struct DeviceReport {
    pub device: String,
    pub path_samples: u64,
    pub relay_samples: u64,
    pub rtt_p50_us: Option<u64>,
    pub rtt_p95_us: Option<u64>,
    pub observed_tx_bytes: u64,
    pub observed_rx_bytes: u64,
    pub sent_packets: u64,
    pub sender_declared_lost_packets: u64,
    pub sender_loss_estimate: Option<f64>,
    pub congestion_events: u64,
    pub send_buffer_drops: u64,
    pub send_buffer_peak_bytes: u64,
    pub baseline_samples: u64,
    pub finished_samples_without_baseline: u64,
    pub reset_samples: u64,
    pub interval_covered_ms: u64,
    pub observed_tx_bytes_per_second: Option<f64>,
    pub observed_rx_bytes_per_second: Option<f64>,
}

#[derive(Debug, Serialize, Default)]
pub struct TargetReport {
    pub device: Option<String>,
    pub target: String,
    pub protocol: String,
    pub scope: String,
    pub observed_flows: u64,
    pub closed_flows: u64,
    pub tcp_dial_observations: u64,
    pub tcp_dial_failures: u64,
    pub tcp_dial_ms_p95: Option<u64>,
    /// 仅是首次上游写入到首次接收的等待，绝非 HTTP TTFB。
    pub first_upstream_wait_ms_p95: Option<u64>,
    pub first_upstream_wait_observations: u64,
    pub observed_up_bytes: u64,
    pub observed_down_bytes: u64,
    pub up_backpressure_ms: u64,
    pub down_backpressure_ms: u64,
    pub up_queue_peak_bytes: usize,
    pub down_queue_peak_bytes: usize,
    pub tcp_rtt_us_p95: Option<u64>,
    pub tcp_metric_samples: u64,
    pub tcp_retransmitted_packets: Option<u64>,
    pub tcp_retransmitted_bytes: Option<u64>,
    pub tcp_metrics_unavailable_samples: u64,
    pub flow_error_counts: BTreeMap<String, u64>,
    pub os_error_counts: BTreeMap<i32, u64>,
    pub udp_queue_drops: u64,
    pub interval_covered_ms: u64,
    pub observed_up_bytes_per_second: Option<f64>,
    pub observed_down_bytes_per_second: Option<f64>,
}

#[derive(Debug, Serialize)]
pub struct QualityReport {
    pub schema: u8,
    pub from_ms: u64,
    pub to_ms: u64,
    pub device: Option<String>,
    pub coverage: Coverage,
    pub devices: Vec<DeviceReport>,
    pub targets: Vec<TargetReport>,
    pub evidence: Vec<String>,
    pub limitations: Vec<String>,
}

#[derive(Default)]
struct DeviceAgg {
    report: DeviceReport,
    rtt: Vec<u64>,
    intervals: Vec<(u64, u64)>,
}
#[derive(Default)]
struct TargetAgg {
    report: TargetReport,
    dial: Vec<u64>,
    first: Vec<u64>,
    rtt: Vec<u64>,
    intervals: Vec<(u64, u64)>,
}
struct TcpPrevious {
    at: u64,
    packets: Option<u64>,
    bytes: Option<u64>,
}

fn percentile(values: &mut [u64], percentile: usize) -> Option<u64> {
    if values.is_empty() {
        return None;
    }
    values.sort_unstable();
    Some(values[(values.len() * percentile).div_ceil(100).saturating_sub(1)])
}
fn in_window(at: u64, q: &QualityQuery) -> bool {
    at >= q.from_ms && at < q.to_ms
}
fn interval_inside(from: u64, to: u64, q: &QualityQuery) -> bool {
    from >= q.from_ms && to <= q.to_ms && (to > from || (to == from && in_window(to, q)))
}
fn interval_overlaps(from: u64, to: u64, q: &QualityQuery) -> bool {
    from < q.to_ms && to > q.from_ms && to > from
}
fn matches_device(device: Option<&str>, q: &QualityQuery) -> bool {
    q.device
        .as_deref()
        .is_none_or(|expected| device.is_some_and(|d| d.eq_ignore_ascii_case(expected)))
}
fn interval_union_ms(mut intervals: Vec<(u64, u64)>) -> u64 {
    intervals.sort_unstable();
    let mut end = 0;
    let mut total = 0u64;
    for (from, to) in intervals {
        total = total.saturating_add(to.saturating_sub(from.max(end)));
        end = end.max(to);
    }
    total
}

/// 仅读本机有界历史，不联系出口、不修改文件。缺历史返回「证据不足」。
pub fn read_report(
    state_dir: &Path,
    mut query: QualityQuery,
) -> Result<QualityReport, QualityError> {
    if query.from_ms >= query.to_ms
        || query
            .device
            .as_ref()
            .is_some_and(|d| d.len() != 16 || !d.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(QualityError::InvalidQuery);
    }
    query.device = query.device.map(|d| d.to_ascii_lowercase());
    let dir = state_dir.join("cache/quality");
    let before_scan = history_stamps(&dir);
    let mut records = Vec::new();
    let mut coverage = Coverage::default();
    for index in (0..storage::FILE_COUNT).rev() {
        let file = match File::open(storage::path(&dir, index)) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        // 本地文件即使遭破坏也不能越过容量上限消耗无界内存。
        let mut reader = BufReader::new(file.take(storage::FILE_BYTES));
        let mut line = Vec::new();
        loop {
            line.clear();
            if reader.read_until(b'\n', &mut line)? == 0 {
                break;
            }
            if !line.ends_with(b"\n") {
                coverage.partial_tail_lines += 1;
                continue;
            }
            match serde_json::from_slice::<Record>(&line) {
                Ok(record) if record.schema == 1 => records.push(record),
                _ => coverage.corrupt_lines += 1,
            }
        }
    }
    let after_scan = history_stamps(&dir);
    if history_rotated(&before_scan, &after_scan) {
        coverage.notes.push(
            "查询期间历史发生轮转或文件替换，可能漏读或重复；本报告覆盖不完整，请重查。".into(),
        );
    } else if before_scan.first() != after_scan.first() {
        coverage
            .notes
            .push("查询期间当前文件仍在追加，报告为读取时的近似快照。".into());
    }
    records.sort_by_key(|r| r.at_ms);
    let mut devices: BTreeMap<String, DeviceAgg> = BTreeMap::new();
    let mut targets: BTreeMap<(Option<String>, String, String, String), TargetAgg> =
        BTreeMap::new();
    let mut runs = HashSet::new();
    let mut last_heartbeat: Option<u64> = None;
    let mut observed_flows = HashSet::new();
    let mut dial_seen = HashSet::new();
    let mut first_seen = HashSet::new();
    let mut closed_seen = HashSet::new();
    let mut tcp_prev: BTreeMap<(u64, u64), TcpPrevious> = BTreeMap::new();
    let mut error_seen = HashSet::new();
    for record in records {
        let at = record.at_ms;
        match record.event {
            Event::Heartbeat {
                from_ms,
                queue_dropped,
                storage_errors,
                finished_overflow,
                closed_overflow,
                reactor_peak_wall_us,
                reactor_pumps,
                shape_backlog_bytes,
                shape_backlog_peak_bytes,
                sampling_delay_ms,
                start,
                ..
            } => {
                coverage.oldest_ms = Some(coverage.oldest_ms.map_or(at, |v| v.min(at)));
                coverage.newest_ms = Some(coverage.newest_ms.map_or(at, |v| v.max(at)));
                if let Some(previous) = last_heartbeat.replace(at) {
                    if at.saturating_sub(previous) > SAMPLE_MS + SAMPLE_MS / 2
                        && interval_overlaps(previous, at, &query)
                    {
                        coverage.sample_gap_count += 1;
                        coverage.sample_gap_ms += at
                            .min(query.to_ms)
                            .saturating_sub(previous.max(query.from_ms));
                    }
                }
                if in_window(at, &query) {
                    runs.insert(record.run);
                    coverage.heartbeat_samples += 1;
                    coverage.restart_markers += u64::from(start);
                    coverage.queue_dropped_observed =
                        coverage.queue_dropped_observed.max(queue_dropped);
                    coverage.storage_errors_observed =
                        coverage.storage_errors_observed.max(storage_errors);
                    coverage.finished_overflow_observed =
                        coverage.finished_overflow_observed.max(finished_overflow);
                    if interval_inside(from_ms, at, &query) {
                        coverage.closed_overflow += closed_overflow
                    }
                    coverage.reactor_peak_wall_us =
                        coverage.reactor_peak_wall_us.max(reactor_peak_wall_us);
                    coverage.sampling_delay_peak_ms =
                        coverage.sampling_delay_peak_ms.max(sampling_delay_ms);
                    coverage.shape_backlog_peak_bytes = coverage
                        .shape_backlog_peak_bytes
                        .max(shape_backlog_bytes)
                        .max(shape_backlog_peak_bytes);
                    if !start && interval_inside(from_ms, at, &query) {
                        coverage.reactor_pumps += reactor_pumps;
                    }
                }
            }
            Event::Path(path) => {
                if !matches_device(Some(&path.device), &query) {
                    continue;
                }
                let agg = devices.entry(path.device.clone()).or_default();
                agg.report.device = path.device.clone();
                if in_window(at, &query) {
                    agg.report.path_samples = agg.report.path_samples.saturating_add(1);
                    agg.report.relay_samples = agg
                        .report
                        .relay_samples
                        .saturating_add(u64::from(path.via == "relay"));
                    if path.rtt_us > 0 && path.path_stable {
                        agg.rtt.push(path.rtt_us)
                    }
                    agg.report.baseline_samples = agg
                        .report
                        .baseline_samples
                        .saturating_add(u64::from(path.baseline));
                    agg.report.finished_samples_without_baseline = agg
                        .report
                        .finished_samples_without_baseline
                        .saturating_add(u64::from(path.baseline && path.finished));
                    agg.report.reset_samples = agg
                        .report
                        .reset_samples
                        .saturating_add(u64::from(path.reset));
                    agg.report.send_buffer_peak_bytes =
                        agg.report.send_buffer_peak_bytes.max(path.send_buffer_used);
                }
                if interval_inside(path.from_ms, at, &query) {
                    add_path(agg, &path, at)
                } else if !path.baseline && interval_overlaps(path.from_ms, at, &query) {
                    coverage.boundary_intervals_excluded += 1
                }
            }
            Event::Flow(flow) => {
                let device = flow.device.map(hex_device);
                if !matches_device(device.as_deref(), &query) {
                    continue;
                }
                let is_inside =
                    interval_inside(flow.window_started_unix_ms, flow.ended_unix_ms, &query);
                let overlaps =
                    interval_overlaps(flow.window_started_unix_ms, flow.ended_unix_ms, &query);
                let connect_at = flow.connect_ended_unix_ms;
                let first_at = flow.first_upstream_byte_unix_ms;
                if !is_inside
                    && !overlaps
                    && !in_window(at, &query)
                    && !connect_at.is_some_and(|stamp| in_window(stamp, &query))
                    && !first_at.is_some_and(|stamp| in_window(stamp, &query))
                    && !flow
                        .error_unix_ms
                        .is_some_and(|stamp| in_window(stamp, &query))
                {
                    continue;
                }
                let protocol = match flow.protocol {
                    super::super::intercept::quality::FlowProtocol::Tcp => "tcp",
                    super::super::intercept::quality::FlowProtocol::Udp => "udp",
                }
                .to_string();
                let scope = match flow.scope {
                    super::super::intercept::quality::FlowScope::Transit => "transit",
                    super::super::intercept::quality::FlowScope::Dns => "dns",
                }
                .to_string();
                let key = (
                    device.clone(),
                    flow.target.to_string(),
                    protocol.clone(),
                    scope.clone(),
                );
                let agg = targets.entry(key).or_default();
                agg.report.device = device;
                agg.report.target = flow.target.to_string();
                agg.report.protocol = protocol;
                agg.report.scope = scope;
                let id = (record.run, flow.flow_id);
                if observed_flows.insert(id) {
                    agg.report.observed_flows = agg.report.observed_flows.saturating_add(1)
                }
                if flow.closed && in_window(at, &query) && closed_seen.insert(id) {
                    agg.report.closed_flows = agg.report.closed_flows.saturating_add(1)
                }
                if let Some(connect) = flow.connect_ms {
                    let connect_at = flow
                        .connect_ended_unix_ms
                        .unwrap_or_else(|| flow.started_unix_ms.saturating_add(connect));
                    if agg.report.protocol == "tcp"
                        && in_window(connect_at, &query)
                        && dial_seen.insert(id)
                    {
                        agg.report.tcp_dial_observations =
                            agg.report.tcp_dial_observations.saturating_add(1);
                        agg.report.tcp_dial_failures =
                            agg.report
                                .tcp_dial_failures
                                .saturating_add(u64::from(matches!(
                                    flow.connect_state,
                                    super::super::intercept::quality::ConnectState::Failed
                                )));
                        agg.dial.push(connect);
                    }
                }
                if flow
                    .first_upstream_byte_unix_ms
                    .is_some_and(|stamp| in_window(stamp, &query))
                    && first_seen.insert(id)
                {
                    if let Some(first) = flow.first_upstream_byte_ms {
                        agg.first.push(first)
                    }
                }
                if is_inside {
                    agg.intervals
                        .push((flow.window_started_unix_ms, flow.ended_unix_ms));
                    agg.report.observed_up_bytes =
                        agg.report.observed_up_bytes.saturating_add(flow.up_bytes);
                    agg.report.observed_down_bytes = agg
                        .report
                        .observed_down_bytes
                        .saturating_add(flow.down_bytes);
                    agg.report.udp_queue_drops = agg
                        .report
                        .udp_queue_drops
                        .saturating_add(flow.udp_queue_drops);
                    agg.report.up_backpressure_ms = agg
                        .report
                        .up_backpressure_ms
                        .saturating_add(flow.up_backpressure_ms);
                    agg.report.down_backpressure_ms = agg
                        .report
                        .down_backpressure_ms
                        .saturating_add(flow.down_backpressure_ms);
                    agg.report.up_queue_peak_bytes =
                        agg.report.up_queue_peak_bytes.max(flow.up_queue_peak_bytes);
                    agg.report.down_queue_peak_bytes = agg
                        .report
                        .down_queue_peak_bytes
                        .max(flow.down_queue_peak_bytes);
                } else if overlaps {
                    coverage.boundary_intervals_excluded += 1
                }
                if let Some(error) = flow.error_kind {
                    if flow
                        .error_unix_ms
                        .is_some_and(|stamp| in_window(stamp, &query))
                        && error_seen.insert(id)
                    {
                        *agg.report
                            .flow_error_counts
                            .entry(format!("{error:?}").to_ascii_lowercase())
                            .or_default() += 1;
                        if let Some(os) = flow.os_error {
                            *agg.report.os_error_counts.entry(os).or_default() += 1
                        }
                    }
                }
                if let Some(tcp) = flow.tcp_metrics {
                    if in_window(at, &query) {
                        if let Some(rtt) = tcp.rtt_us {
                            agg.rtt.push(rtt);
                            agg.report.tcp_metric_samples =
                                agg.report.tcp_metric_samples.saturating_add(1)
                        } else {
                            agg.report.tcp_metrics_unavailable_samples =
                                agg.report.tcp_metrics_unavailable_samples.saturating_add(1)
                        }
                    }
                    if let Some(previous) = tcp_prev.get(&id) {
                        if interval_inside(previous.at, at, &query) {
                            if let (Some(before), Some(after)) =
                                (previous.packets, tcp.retransmitted_packets)
                            {
                                if let Some(delta) = after.checked_sub(before) {
                                    add_available_count(
                                        &mut agg.report.tcp_retransmitted_packets,
                                        delta,
                                    );
                                }
                            }
                            if let (Some(before), Some(after)) =
                                (previous.bytes, tcp.retransmitted_bytes)
                            {
                                if let Some(delta) = after.checked_sub(before) {
                                    add_available_count(
                                        &mut agg.report.tcp_retransmitted_bytes,
                                        delta,
                                    );
                                }
                            }
                        }
                    } else if in_window(flow.started_unix_ms, &query) && in_window(at, &query) {
                        if let Some(packets) = tcp.retransmitted_packets {
                            add_available_count(&mut agg.report.tcp_retransmitted_packets, packets);
                        }
                        if let Some(bytes) = tcp.retransmitted_bytes {
                            add_available_count(&mut agg.report.tcp_retransmitted_bytes, bytes);
                        }
                    }
                    tcp_prev.insert(
                        id,
                        TcpPrevious {
                            at,
                            packets: tcp.retransmitted_packets,
                            bytes: tcp.retransmitted_bytes,
                        },
                    );
                }
            }
        }
    }
    coverage.runs = runs.len();
    if coverage.heartbeat_samples == 0 {
        coverage
            .notes
            .push("所选窗口无采样心跳；不能据此判断网络健康。".into())
    }
    if coverage.oldest_ms.is_some_and(|at| query.from_ms < at) {
        coverage
            .notes
            .push("窗口起点早于现存历史；更早数据可能已轮转或尚未启用采集。".into())
    }
    if coverage
        .newest_ms
        .is_some_and(|at| query.to_ms.saturating_sub(at) > SAMPLE_MS * 2)
    {
        coverage
            .notes
            .push("窗口尾部超出现存采样，可能出口已停或采集失败。".into())
    }
    if coverage.runs > 1 || coverage.restart_markers > 0 {
        coverage
            .notes
            .push("窗口内存在启动标记或多个进程批次；累计计数按进程分开，不跨重启求差。".into())
    }
    if query.device.is_some() {
        coverage
            .notes
            .push("设备过滤仅作用于路径和目标；心跳、采集丢失及出口内部指标为出口全局。".into());
    }
    let mut evidence = Vec::new();
    let devices:Vec<_>=devices.into_values().filter_map(|mut agg| {
        if agg.report.path_samples==0 && agg.report.sent_packets==0 && agg.report.observed_rx_bytes==0 && agg.report.observed_tx_bytes==0 { return None }
        agg.report.rtt_p50_us=percentile(&mut agg.rtt,50); agg.report.rtt_p95_us=percentile(&mut agg.rtt,95);
        agg.report.sender_loss_estimate=(agg.report.sent_packets>0).then(||agg.report.sender_declared_lost_packets as f64/agg.report.sent_packets as f64);
        agg.report.interval_covered_ms=interval_union_ms(agg.intervals);
        if agg.report.interval_covered_ms>0 {
            agg.report.observed_tx_bytes_per_second=Some(agg.report.observed_tx_bytes as f64*1000.0/agg.report.interval_covered_ms as f64);
            agg.report.observed_rx_bytes_per_second=Some(agg.report.observed_rx_bytes as f64*1000.0/agg.report.interval_covered_ms as f64);
        }
        if agg.report.relay_samples>0 { evidence.push(format!("设备 {} 有 {} 次中继路径观测；需结合直连失败原因排查。",agg.report.device,agg.report.relay_samples)) }
        if agg.report.sender_declared_lost_packets>0 { evidence.push(format!("隧道发送方向：设备 {} 出口发送 {} 包，同期判丢 {} 包；判丢会滞后发送，需结合RTT和业务时段核对。",agg.report.device,agg.report.sent_packets,agg.report.sender_declared_lost_packets)) }
        if agg.report.send_buffer_drops>0 { evidence.push(format!("设备 {} 出口→手机发送缓冲丢弃 {} 个包，隧道发送方向有明确积压证据。",agg.report.device,agg.report.send_buffer_drops)) }
        Some(agg.report)
    }).collect();
    let mut targets: Vec<_> = targets
        .into_values()
        .map(|mut agg| {
            agg.report.tcp_dial_ms_p95 = percentile(&mut agg.dial, 95);
            agg.report.first_upstream_wait_observations = agg.first.len() as u64;
            agg.report.first_upstream_wait_ms_p95 = percentile(&mut agg.first, 95);
            agg.report.tcp_rtt_us_p95 = percentile(&mut agg.rtt, 95);
            agg.report.interval_covered_ms = interval_union_ms(agg.intervals);
            if agg.report.interval_covered_ms > 0 {
                agg.report.observed_up_bytes_per_second = Some(
                    agg.report.observed_up_bytes as f64 * 1000.0
                        / agg.report.interval_covered_ms as f64,
                );
                agg.report.observed_down_bytes_per_second = Some(
                    agg.report.observed_down_bytes as f64 * 1000.0
                        / agg.report.interval_covered_ms as f64,
                );
            }
            agg.report
        })
        .collect();
    targets.sort_by_key(|t| {
        std::cmp::Reverse((
            t.tcp_dial_failures,
            t.flow_error_counts.values().sum::<u64>(),
            t.tcp_dial_ms_p95.unwrap_or(0),
            t.first_upstream_wait_ms_p95.unwrap_or(0),
            t.up_backpressure_ms.saturating_add(t.down_backpressure_ms),
            t.observed_up_bytes.saturating_add(t.observed_down_bytes),
        ))
    });
    for t in targets.iter().take(10) {
        if t.tcp_dial_failures > 0 {
            evidence.push(format!(
                "目标连接方向：设备 {} → {} 有 {}/{} 次 TCP 拨号失败；错误 {:?}，OS错误 {:?}。",
                t.device.as_deref().unwrap_or("未知"),
                t.target,
                t.tcp_dial_failures,
                t.tcp_dial_observations,
                t.flow_error_counts,
                t.os_error_counts
            ))
        }
        if t.down_backpressure_ms > 0 || t.up_backpressure_ms > 0 {
            evidence.push(format!("转发队列：目标 {} 向目标/回手机背压累计 {}/{} ms；结合隧道缓冲及上游TCP指标判断接收端是否消化不足。",t.target,t.up_backpressure_ms,t.down_backpressure_ms))
        }
    }
    let has_clues = !evidence.is_empty();
    if coverage.reactor_peak_wall_us > 0 {
        evidence.push(format!("窗口内出口 reactor 单拍最大墙钟耗时 {} us（包含处理，不等于调度延迟或 CPU 使用率）；采样超期峰值 {} ms，整形队列峰值 {} B。",coverage.reactor_peak_wall_us,coverage.sampling_delay_peak_ms,coverage.shape_backlog_peak_bytes))
    }
    evidence.push(if has_clues { "瓶颈线索：已观测上述阶段的丢包、失败或积压迹象；请结合相同时段业务卡顿核对，不能据此证明单一瓶颈。" } else { "瓶颈归因：当前证据不足以定位单一瓶颈；没有显式异常也不能判定健康。" }.into());
    Ok(QualityReport {
        schema:1,from_ms:query.from_ms,to_ms:query.to_ms,device:query.device,coverage,devices,targets,evidence,
        limitations:vec![
            "仅观测出口单侧：QUIC 丢包为出口发送端判丢估计，不能代替手机上行丢包；判丢可能滞后发送窗口。".into(),
            "实际字节速率不代表链路可用带宽；没有错误、低流量或没有数据均不能判定健康。".into(),
            "HTTPS / HTTP2 / HTTP3 按连接或 UDP 会话观测，不是业务请求；UDP 本机就绪不代表远端应答，UDP 错误不称请求失败。".into(),
            "首次上游等待从首次实际写入计时，不是 HTTP TTFB，不能拆分服务端处理与网络时延。".into(),
            "增量只计完全落入窗口的区间；边界区间不按比例推算。长流拨号和首字节按实际发生时点筛选并按流去重。".into(),
            "RTT 分位数从时点原始观测计算，不平均分位数；TCP 内核指标依平台可用性提供。历史最多 4×8MiB，保留时长随流量变化。".into(),
            "CPU 使用率、线程调度延迟和主动带宽测试本版未采集；reactor 耗时和队列不能单独证明出口 CPU 瓶颈。".into(),
            "异步落盘不阻塞VPN；正常stop最多等待200ms排空，超时会记日志，进程崩溃或超时可能缺最后一拍和关闭摘要。".into(),
            "坏行和不完整尾行统计覆盖全部现存文件，无法准确归属查询时段或设备。".into(),
            "TCP重传包数与字节数分别统计：仅累加字段可用且能归属窗口的读数/差分，部分可用时数值代表已观测子集；无可归窗读数为不可用，不填0。".into(),
            "QUIC 初样本、迁移及计数重置只作基线，缺基线的短会话尾样本不计历史累计字节；DNS 内部代答的 OS 字节为 0，另参照现有 DNS 服务计数。".into(),
        ],
    })
}

fn add_path(agg: &mut DeviceAgg, path: &PathRecord, at: u64) {
    if path.baseline {
        return;
    }
    agg.intervals.push((path.from_ms, at));
    agg.report.observed_tx_bytes = agg
        .report
        .observed_tx_bytes
        .saturating_add(path.udp_tx_bytes);
    agg.report.observed_rx_bytes = agg
        .report
        .observed_rx_bytes
        .saturating_add(path.udp_rx_bytes);
    agg.report.sent_packets = agg.report.sent_packets.saturating_add(path.sent_packets);
    agg.report.sender_declared_lost_packets = agg
        .report
        .sender_declared_lost_packets
        .saturating_add(path.lost_packets);
    agg.report.congestion_events = agg
        .report
        .congestion_events
        .saturating_add(path.congestion_events);
    agg.report.send_buffer_drops = agg
        .report
        .send_buffer_drops
        .saturating_add(path.send_buffer_drops);
}

impl QualityReport {
    pub fn render_text(&self) -> String {
        use std::fmt::Write;
        let mut s = format!("出口质量报告 [{} , {}) ms\n", self.from_ms, self.to_ms);
        let c = &self.coverage;
        let _=writeln!(s,"覆盖：{} 次心跳，{} 个进程批次；缺采样 {} 段 / {} ms；坏行 {}，不完整尾行 {}，排除边界区间 {}。",c.heartbeat_samples,c.runs,c.sample_gap_count,c.sample_gap_ms,c.corrupt_lines,c.partial_tail_lines,c.boundary_intervals_excluded);
        let _=writeln!(s,"观测损失（窗口所见进程累计上限）：队列={}，存储错误={}，QUIC关闭摘要溢出={}；流关闭摘要窗溢出={}。",c.queue_dropped_observed,c.storage_errors_observed,c.finished_overflow_observed,c.closed_overflow);
        let _ = writeln!(
            s,
            "出口内部：reactor={} 拍，单拍最大={} us，采样超期峰值={} ms，整形队列峰值={} B。",
            c.reactor_pumps,
            c.reactor_peak_wall_us,
            c.sampling_delay_peak_ms,
            c.shape_backlog_peak_bytes
        );
        for note in &c.notes {
            let _ = writeln!(s, "覆盖说明：{note}");
        }
        for d in &self.devices {
            let _=writeln!(s,"设备 {}：RTT p50/p95={}/{}，出口发/收={}/{} B，发送包={}，发送端判丢={}，中继样本={}，缓冲丢弃={}。",d.device,display_us(d.rtt_p50_us),display_us(d.rtt_p95_us),d.observed_tx_bytes,d.observed_rx_bytes,d.sent_packets,d.sender_declared_lost_packets,d.relay_samples,d.send_buffer_drops);
        }
        let _ = writeln!(
            s,
            "目标：共 {} 组，按失败、耗时、背压及流量排序，文本显示前 20 组（JSON 保留全部）。",
            self.targets.len()
        );
        for t in self.targets.iter().take(20) {
            let _=writeln!(s,"设备 {} → {} {}/{}：流={}，TCP拨号失败/观测={}/{}，拨号p95={}，上/下行={}/{} B，背压={}/{} ms，TCP RTT p95={}。",t.device.as_deref().unwrap_or("未知"),t.target,t.protocol,t.scope,t.observed_flows,t.tcp_dial_failures,t.tcp_dial_observations,display_ms(t.tcp_dial_ms_p95),t.observed_up_bytes,t.observed_down_bytes,t.up_backpressure_ms,t.down_backpressure_ms,display_us(t.tcp_rtt_us_p95));
            let _=writeln!(s,"  首次上游等待p95={}，TCP重传包/字节={}/{}；流错误={:?}，OS错误={:?}，UDP队列丢弃={}。",display_ms(t.first_upstream_wait_ms_p95),display_count(t.tcp_retransmitted_packets),display_count(t.tcp_retransmitted_bytes),t.flow_error_counts,t.os_error_counts,t.udp_queue_drops);
        }
        for e in &self.evidence {
            let _ = writeln!(s, "证据：{e}");
        }
        for l in &self.limitations {
            let _ = writeln!(s, "口径：{l}");
        }
        s
    }
}

fn display_us(value: Option<u64>) -> String {
    value.map_or_else(
        || "不可用".into(),
        |us| format!("{:.2} ms", us as f64 / 1000.0),
    )
}
fn display_ms(value: Option<u64>) -> String {
    value.map_or_else(|| "不可用".into(), |ms| format!("{ms} ms"))
}

#[derive(PartialEq)]
struct HistoryStamp {
    len: u64,
    created: Option<std::time::SystemTime>,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64),
}
fn history_stamps(dir: &Path) -> Vec<Option<HistoryStamp>> {
    (0..storage::FILE_COUNT)
        .map(|index| {
            std::fs::metadata(storage::path(dir, index))
                .ok()
                .map(|m| HistoryStamp {
                    len: m.len(),
                    created: m.created().ok(),
                    modified: m.modified().ok(),
                    #[cfg(unix)]
                    identity: {
                        use std::os::unix::fs::MetadataExt;
                        (m.dev(), m.ino())
                    },
                })
        })
        .collect()
}
fn history_rotated(before: &[Option<HistoryStamp>], after: &[Option<HistoryStamp>]) -> bool {
    if before.iter().skip(1).ne(after.iter().skip(1)) {
        return true;
    }
    match (
        before.first().and_then(Option::as_ref),
        after.first().and_then(Option::as_ref),
    ) {
        (None, None) => false,
        (Some(a), Some(b)) => {
            #[cfg(unix)]
            if a.identity != b.identity {
                return true;
            }
            a.created != b.created || b.len < a.len
        }
        _ => true,
    }
}

fn add_available_count(total: &mut Option<u64>, value: u64) {
    *total = Some(total.map_or(value, |before| before.saturating_add(value)));
}
fn display_count(value: Option<u64>) -> String {
    value.map_or_else(|| "不可用".into(), |count| count.to_string())
}
