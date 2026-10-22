use super::*;
use crate::server::intercept::quality::{
    ConnectState, FlowProtocol, FlowQuality, InterceptorQuality,
};
use std::fs;
use std::path::PathBuf;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let mut random = [0; 8];
        getrandom::getrandom(&mut random).unwrap();
        let path = std::env::temp_dir().join(format!(
            "homeway-quality-{}-{}",
            std::process::id(),
            u64::from_le_bytes(random)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn query(from_ms: u64, to_ms: u64) -> QualityQuery {
    QualityQuery {
        from_ms,
        to_ms,
        device: None,
    }
}
fn path(at: u64, counter: u64) -> ExitPathQuality {
    ExitPathQuality {
        dev: [1; 8],
        tunnel_ip: "100.64.1.2".parse().unwrap(),
        tun_ip: "100.64.2.3".parse().unwrap(),
        connection_id: 1,
        path_epoch: 0,
        remote: "192.0.2.2:41641".parse().unwrap(),
        via: ExitPathVia::Direct,
        rtt_us: at,
        udp_tx_bytes: counter * 100,
        udp_rx_bytes: counter * 200,
        udp_tx_datagrams: counter,
        udp_rx_datagrams: counter,
        sent_packets: counter,
        lost_packets: counter / 10,
        congestion_events: counter / 100,
        cwnd: 10000,
        datagram_send_buffer_space: 10000,
        datagram_send_buffer_used: 0,
        send_buffer_drops: 0,
        finished: false,
        observed_at_ms: at,
        bound_for_ms: at,
        path_stable: true,
    }
}
fn batch(at: u64, paths: Vec<ExitPathQuality>, flows: Vec<FlowQuality>) -> Batch {
    Batch {
        at_ms: at,
        paths,
        flows: InterceptorQuality {
            records: flows,
            ..Default::default()
        },
        finished_overflow: 0,
        sampling_delay_ms: 0,
    }
}
fn flow() -> FlowQuality {
    // 以序列化输入创建真实 schema，新增可选字段不影响测试维护。
    serde_json::from_value(serde_json::json!({
        "flow_id":1,"device":[1,1,1,1,1,1,1,1],"client":"100.64.2.3:60000","target":"192.0.2.3:443",
        "protocol":"tcp","scope":"transit","started_unix_ms":1000,"elapsed_ms":1000,
        "window_started_unix_ms":1000,"ended_unix_ms":2000,"window_ms":1000,"closed":false,"close_reason":null,
        "connect_ms":20,"connect_state":"connected","error_kind":null,"os_error":null,
        "up_bytes":100,"down_bytes":200,"total_up_bytes":100,"total_down_bytes":200,
        "first_upstream_byte_ms":5,"first_upstream_byte_unix_ms":1100,
        "up_queue_peak_bytes":1,"down_queue_peak_bytes":2,"up_queue_bytes":0,"down_queue_bytes":0,
        "up_backpressure_ms":3,"down_backpressure_ms":4,"udp_queue_drops":0,"tcp_metrics":null
    })).unwrap()
}
fn write_batch(worker: &mut Worker, store: &mut storage::Store, batch: Batch) {
    for record in worker.records(batch, 0, 0) {
        store.append(&record).unwrap()
    }
}

#[test]
fn differential_reset_migration_and_short_quic_are_not_double_counted() {
    let tmp = Temp::new();
    let mut worker = Worker::new();
    let mut store = storage::Store::new(&tmp.0);
    write_batch(
        &mut worker,
        &mut store,
        batch(1000, vec![path(1000, 100)], vec![]),
    );
    write_batch(
        &mut worker,
        &mut store,
        batch(2000, vec![path(2000, 120)], vec![]),
    );
    let mut moved = path(3000, 150);
    moved.path_epoch = 1;
    write_batch(&mut worker, &mut store, batch(3000, vec![moved], vec![]));
    let mut after = path(4000, 160);
    after.path_epoch = 1;
    write_batch(&mut worker, &mut store, batch(4000, vec![after], vec![]));
    let mut reset = path(5000, 5);
    reset.path_epoch = 1;
    write_batch(&mut worker, &mut store, batch(5000, vec![reset], vec![]));
    let mut short = path(5500, 12);
    short.connection_id = 2;
    short.finished = true;
    write_batch(&mut worker, &mut store, batch(5500, vec![short], vec![]));
    let report = read_report(&tmp.0, query(0, 6000)).unwrap();
    let d = &report.devices[0];
    assert_eq!(d.observed_tx_bytes, 3000);
    assert_eq!(d.sent_packets, 30);
    assert_eq!(d.reset_samples, 2);
    assert_eq!(d.finished_samples_without_baseline, 1);
    assert_eq!(d.rtt_p50_us, Some(3000));
    assert_eq!(d.rtt_p95_us, Some(5500));
    assert_eq!(d.interval_covered_ms, 2000);
}

#[test]
fn adjacent_windows_exclude_spanning_delta_and_do_not_repeat_lifecycle_metrics() {
    let tmp = Temp::new();
    let mut worker = Worker::new();
    let mut store = storage::Store::new(&tmp.0);
    write_batch(
        &mut worker,
        &mut store,
        batch(2000, vec![path(2000, 100)], vec![flow()]),
    );
    let mut next = flow();
    next.window_started_unix_ms = 2000;
    next.ended_unix_ms = 3000;
    next.elapsed_ms = 2000;
    next.total_up_bytes = 200;
    next.total_down_bytes = 400;
    next.closed = true;
    write_batch(
        &mut worker,
        &mut store,
        batch(3000, vec![path(3000, 110)], vec![next]),
    );
    let before = read_report(&tmp.0, query(1000, 2000)).unwrap();
    let after = read_report(&tmp.0, query(2000, 4000)).unwrap();
    assert_eq!(before.targets[0].observed_up_bytes, 100);
    assert_eq!(after.targets[0].observed_up_bytes, 100);
    assert_eq!(before.targets[0].tcp_dial_observations, 1);
    assert_eq!(after.targets[0].tcp_dial_observations, 0);
    assert_eq!(before.targets[0].first_upstream_wait_observations, 1);
    assert_eq!(after.targets[0].first_upstream_wait_observations, 0);
    let span = read_report(&tmp.0, query(2500, 4000)).unwrap();
    assert_eq!(span.targets[0].observed_up_bytes, 0);
    assert_eq!(span.devices[0].observed_tx_bytes, 0);
    assert!(span.coverage.boundary_intervals_excluded >= 2);
}

#[test]
fn authenticated_device_filter_does_not_mix_unknown_or_other_device() {
    let tmp = Temp::new();
    let mut worker = Worker::new();
    let mut store = storage::Store::new(&tmp.0);
    let a = flow();
    let mut b = flow();
    b.device = Some([2; 8]);
    b.flow_id = 2;
    b.up_bytes = 999;
    let mut unknown = flow();
    unknown.device = None;
    unknown.flow_id = 3;
    unknown.up_bytes = 500;
    write_batch(
        &mut worker,
        &mut store,
        batch(2000, vec![], vec![a, b, unknown]),
    );
    let report = read_report(
        &tmp.0,
        QualityQuery {
            device: Some("0101010101010101".into()),
            ..query(0, 3000)
        },
    )
    .unwrap();
    assert_eq!(report.targets.len(), 1);
    assert_eq!(report.targets[0].observed_up_bytes, 100);
    let all = read_report(&tmp.0, query(0, 3000)).unwrap();
    assert_eq!(all.targets.len(), 3);
    assert!(all.targets.iter().any(|t| t.device.is_none()));
    assert!(read_report(
        &tmp.0,
        QualityQuery {
            device: Some("01".into()),
            ..query(0, 3000)
        }
    )
    .is_err());
}

#[test]
fn long_short_zero_duration_udp_and_restart_are_kept_separate() {
    let tmp = Temp::new();
    let mut store = storage::Store::new(&tmp.0);
    let mut first = Worker::new();
    let mut udp = flow();
    udp.protocol = FlowProtocol::Udp;
    udp.connect_state = ConnectState::UdpLocalReady;
    udp.closed = true;
    udp.window_started_unix_ms = 2000;
    udp.started_unix_ms = 2000;
    udp.window_ms = 0;
    udp.elapsed_ms = 0;
    write_batch(&mut first, &mut store, batch(2000, vec![], vec![udp]));
    let mut second = Worker::new();
    let mut f = flow();
    f.up_bytes = 50;
    write_batch(&mut second, &mut store, batch(4000, vec![], vec![f]));
    let report = read_report(&tmp.0, query(0, 5000)).unwrap();
    assert_eq!(report.coverage.runs, 2);
    assert_eq!(
        report
            .targets
            .iter()
            .find(|t| t.protocol == "udp")
            .unwrap()
            .tcp_dial_observations,
        0
    );
    assert_eq!(
        report
            .targets
            .iter()
            .map(|t| t.observed_up_bytes)
            .sum::<u64>(),
        150
    );
}

#[test]
fn damaged_and_partial_lines_are_visible_and_append_recovers() {
    let tmp = Temp::new();
    let mut store = storage::Store::new(&tmp.0);
    let mut worker = Worker::new();
    write_batch(&mut worker, &mut store, batch(1000, vec![], vec![]));
    drop(store);
    use std::io::Write;
    let current = storage::path(&tmp.0.join("cache/quality"), 0);
    let mut file = fs::OpenOptions::new().append(true).open(current).unwrap();
    file.write_all(b"broken\n\xff\n{\"schema\":").unwrap();
    drop(file);
    let report = read_report(&tmp.0, query(0, 5000)).unwrap();
    assert_eq!(report.coverage.corrupt_lines, 2);
    assert_eq!(report.coverage.partial_tail_lines, 1);
    let mut store = storage::Store::new(&tmp.0);
    write_batch(&mut worker, &mut store, batch(2000, vec![], vec![]));
    let recovered = read_report(&tmp.0, query(0, 5000)).unwrap();
    assert_eq!(recovered.coverage.heartbeat_samples, 2);
    assert_eq!(recovered.coverage.corrupt_lines, 3);
}

#[test]
fn rotation_is_bounded_and_unix_permissions_are_private() {
    let tmp = Temp::new();
    let mut store = storage::Store::new(&tmp.0);
    let mut worker = Worker::new();
    let mut records = worker.records(batch(1000, vec![path(1000, 1)], vec![]), 0, 0);
    let mut record = records.pop().unwrap();
    if let Event::Path(path) = &mut record.event {
        path.remote = "x".repeat(3 * 1024 * 1024)
    }
    for _ in 0..12 {
        store.append(&record).unwrap()
    }
    let dir = tmp.0.join("cache/quality");
    let mut total = 0;
    for index in 0..storage::FILE_COUNT {
        let m = fs::metadata(storage::path(&dir, index)).unwrap();
        assert!(m.len() <= storage::FILE_BYTES);
        total += m.len();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(m.permissions().mode() & 0o777, 0o600);
        }
    }
    assert!(total <= storage::FILE_BYTES * storage::FILE_COUNT as u64);
    assert_eq!(fs::read_dir(&dir).unwrap().count(), storage::FILE_COUNT);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
}

#[test]
fn full_queue_is_nonblocking_and_loss_counted() {
    let (tx, _rx) = mpsc::sync_channel(1);
    let dropped = Arc::new(AtomicU64::new(0));
    let (_done_tx, done) = mpsc::channel();
    let collector = Collector {
        tx,
        dropped: Arc::clone(&dropped),
        done,
    };
    collector.sample(vec![], InterceptorQuality::default(), 0, 0);
    collector.sample(vec![], InterceptorQuality::default(), 0, 0);
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
}

#[test]
fn absent_or_incomplete_history_never_reports_health() {
    let tmp = Temp::new();
    let report = read_report(&tmp.0, query(1000, 2000)).unwrap();
    assert!(report.devices.is_empty());
    assert!(report.targets.is_empty());
    assert!(report.render_text().contains("不能据此判断网络健康"));
    assert!(report.evidence.iter().any(|e| e.contains("证据不足")));
    assert!(read_report(&tmp.0, query(2000, 2000)).is_err());
    let mut worker = Worker::new();
    let mut store = storage::Store::new(&tmp.0);
    write_batch(&mut worker, &mut store, batch(5000, vec![], vec![]));
    write_batch(&mut worker, &mut store, batch(25000, vec![], vec![]));
    let report = read_report(&tmp.0, query(0, 50000)).unwrap();
    assert_eq!(report.coverage.sample_gap_count, 1);
    assert_eq!(report.coverage.sample_gap_ms, 20000);
    assert!(report.coverage.notes.iter().any(|n| n.contains("窗口起点")));
    assert!(report.coverage.notes.iter().any(|n| n.contains("窗口尾部")));
}

#[test]
fn lifecycle_error_time_is_filtered_even_when_summary_arrives_later() {
    use crate::server::intercept::quality::FlowError;
    let tmp = Temp::new();
    let mut worker = Worker::new();
    let mut store = storage::Store::new(&tmp.0);
    let mut failed = flow();
    failed.connect_state = ConnectState::Failed;
    failed.connect_ended_unix_ms = Some(1020);
    failed.error_unix_ms = Some(1020);
    failed.error_kind = Some(FlowError::Connect);
    failed.os_error = Some(111);
    failed.closed = true;
    write_batch(&mut worker, &mut store, batch(2000, vec![], vec![failed]));
    let earlier = read_report(&tmp.0, query(1000, 1500)).unwrap();
    assert_eq!(earlier.targets[0].tcp_dial_failures, 1);
    assert_eq!(earlier.targets[0].flow_error_counts["connect"], 1);
    assert_eq!(earlier.targets[0].os_error_counts[&111], 1);
    let later = read_report(&tmp.0, query(1500, 2500)).unwrap();
    assert_eq!(later.targets[0].tcp_dial_failures, 0);
    assert!(later.targets[0].flow_error_counts.is_empty());
    assert!(!earlier.render_text().contains("Some("));
}

#[test]
fn normal_finish_drains_history_but_slow_finish_has_bounded_wait() {
    let tmp = Temp::new();
    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let copy = Arc::clone(&messages);
    let log: Logf = Arc::new(move |line| copy.lock().unwrap().push(line.to_owned()));
    let collector = Collector::start(&tmp.0, Arc::clone(&log)).unwrap();
    collector.sample(
        vec![],
        InterceptorQuality {
            records: vec![flow()],
            ..Default::default()
        },
        0,
        0,
    );
    collector.finish(Duration::from_millis(200), &log);
    assert_eq!(
        read_report(&tmp.0, query(0, u64::MAX)).unwrap().targets[0].observed_up_bytes,
        100
    );
    let (tx, _rx) = mpsc::sync_channel(1);
    let (_done_tx, done) = mpsc::channel();
    let stalled = Collector {
        tx,
        dropped: Arc::new(AtomicU64::new(0)),
        done,
    };
    let at = std::time::Instant::now();
    stalled.finish(Duration::from_millis(5), &log);
    assert!(at.elapsed() < Duration::from_millis(500));
    assert!(messages
        .lock()
        .unwrap()
        .iter()
        .any(|m| m.contains("收尾超时")));
}

#[test]
fn storage_failure_is_visible_without_stopping_collector() {
    let tmp = Temp::new();
    fs::create_dir_all(tmp.0.join("cache")).unwrap();
    fs::write(tmp.0.join("cache/quality"), b"not a directory").unwrap();
    let messages = Arc::new(std::sync::Mutex::new(Vec::new()));
    let copy = Arc::clone(&messages);
    let log: Logf = Arc::new(move |line| copy.lock().unwrap().push(line.to_owned()));
    let collector = Collector::start(&tmp.0, Arc::clone(&log)).unwrap();
    collector.sample(vec![], InterceptorQuality::default(), 0, 0);
    collector.finish(Duration::from_millis(200), &log);
    assert!(messages
        .lock()
        .unwrap()
        .iter()
        .any(|m| m.contains("历史写入失败")));
}

#[test]
fn partial_tcp_metrics_preserve_unavailable_and_window_attribution() {
    use crate::sysock::{TcpQuality, TcpQualityAvailability};
    let tmp = Temp::new();
    let mut worker = Worker::new();
    let mut store = storage::Store::new(&tmp.0);
    let mut partial = flow();
    partial.tcp_metrics = Some(TcpQuality {
        availability: TcpQualityAvailability::Available,
        rtt_us: Some(1000),
        retransmitted_packets: Some(7),
        retransmitted_bytes: None,
    });
    let mut unavailable = flow();
    unavailable.flow_id = 2;
    unavailable.target.set_port(444);
    unavailable.tcp_metrics = Some(TcpQuality {
        availability: TcpQualityAvailability::Unavailable,
        rtt_us: None,
        retransmitted_packets: None,
        retransmitted_bytes: None,
    });
    let mut old = flow();
    old.flow_id = 3;
    old.target.set_port(445);
    old.started_unix_ms = 500;
    old.tcp_metrics = Some(TcpQuality {
        availability: TcpQualityAvailability::Available,
        rtt_us: Some(1200),
        retransmitted_packets: Some(9),
        retransmitted_bytes: Some(90),
    });
    let mut zero = flow();
    zero.flow_id = 4;
    zero.target.set_port(446);
    zero.tcp_metrics = Some(TcpQuality {
        availability: TcpQualityAvailability::Available,
        rtt_us: Some(900),
        retransmitted_packets: Some(0),
        retransmitted_bytes: Some(0),
    });
    write_batch(
        &mut worker,
        &mut store,
        batch(2000, vec![], vec![partial, unavailable, old.clone(), zero]),
    );
    let report = read_report(&tmp.0, query(1000, 3000)).unwrap();
    let target = |port: u16| {
        report
            .targets
            .iter()
            .find(|t| t.target.ends_with(&format!(":{port}")))
            .unwrap()
    };
    assert_eq!(target(443).tcp_rtt_us_p95, Some(1000));
    assert_eq!(target(443).tcp_retransmitted_packets, Some(7));
    assert_eq!(target(443).tcp_retransmitted_bytes, None);
    assert_eq!(target(444).tcp_retransmitted_packets, None);
    assert_eq!(target(444).tcp_retransmitted_bytes, None);
    assert_eq!(target(445).tcp_retransmitted_packets, None);
    assert_eq!(target(445).tcp_retransmitted_bytes, None);
    assert_eq!(target(446).tcp_retransmitted_packets, Some(0));
    assert_eq!(target(446).tcp_retransmitted_bytes, Some(0));
    let json = serde_json::to_value(target(443)).unwrap();
    assert!(json["tcp_retransmitted_bytes"].is_null());
    assert_eq!(json["tcp_retransmitted_packets"], 7);
    assert!(report.render_text().contains("TCP重传包/字节=7/不可用"));
    assert!(report
        .render_text()
        .contains("TCP重传包/字节=不可用/不可用"));
    // 旧流累计不可直接归窗，但后续两个窗内样本的真实差分可以归窗。
    old.window_started_unix_ms = 2000;
    old.ended_unix_ms = 2500;
    old.tcp_metrics.as_mut().unwrap().retransmitted_packets = Some(11);
    old.tcp_metrics.as_mut().unwrap().retransmitted_bytes = Some(100);
    write_batch(&mut worker, &mut store, batch(2500, vec![], vec![old]));
    let with_delta = read_report(&tmp.0, query(1900, 3000)).unwrap();
    let old = with_delta
        .targets
        .iter()
        .find(|t| t.target.ends_with(":445"))
        .unwrap();
    assert_eq!(old.tcp_retransmitted_packets, Some(2));
    assert_eq!(old.tcp_retransmitted_bytes, Some(10));
    let spanning = read_report(&tmp.0, query(2200, 3000)).unwrap();
    let old = spanning
        .targets
        .iter()
        .find(|t| t.target.ends_with(":445"))
        .unwrap();
    assert_eq!(old.tcp_retransmitted_packets, None);
    assert_eq!(old.tcp_retransmitted_bytes, None);
}
