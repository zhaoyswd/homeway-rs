//! 质量仪表收口与真实 OS I/O 回归，不使用公网或生产出口。
use super::*;
use std::io::{Read, Write};

fn interceptor() -> Interceptor {
    Interceptor::attach(
        Config {
            tunnel_ip: Ipv4Addr::new(100, 64, 255, 1),
            dns: None,
            dns_events: None,
            dns_resolve_port: 0,
            tx_shape: None,
            logf: Arc::new(|_| {}),
        },
        Arc::new(Stats::default()),
    )
}
fn view(port: u16, protocol: Proto) -> View5 {
    View5 {
        src: Ipv4Addr::new(100, 64, 5, 6),
        src_port: 41234,
        dst: Ipv4Addr::LOCALHOST,
        dst_port: port,
        proto: if protocol == Proto::Tcp { 6 } else { 17 },
        tcp_flags: nat::TCP_SYN,
        tcp_seq: 123,
        tcp_ack: 0,
        udp_payload: (0, 0),
    }
}

#[test]
fn delayed_connect_acceptance_and_short_close_preserve_timing_and_device() {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut itc = interceptor();
    let syn = nat::build_tcp_syn(
        Ipv4Addr::new(100, 64, 5, 6),
        41234,
        Ipv4Addr::LOCALHOST,
        port,
        123,
    );
    itc.on_plain_for_device(syn, [7; 8]);
    let flow = *itc.flows.keys().next().unwrap();
    let phase = itc.drain_quality();
    assert_eq!(phase.records[0].device, Some([7; 8]));
    // TCP nonblocking connect is accepted only when reactor observes completion.
    if phase.records[0].connect_state == ConnectState::Pending {
        std::thread::sleep(Duration::from_millis(25));
        itc.on_connect_done(flow);
        assert!(itc.flows[&flow].quality.record.connect_ms.unwrap() >= 25);
    }
    itc.teardown_flow(flow);
    let closed = itc.drain_quality();
    assert_eq!(closed.records.len(), 1);
    let q = &closed.records[0];
    assert!(q.closed);
    assert_eq!(q.connect_state, ConnectState::Connected);
    assert_eq!(q.device, Some([7; 8]));
    assert!(q.connect_ms.is_some());
    assert!(itc.drain_quality().records.is_empty());
}

#[test]
fn real_tcp_io_reports_actual_bytes_and_first_reply_not_connection_delay() {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut itc = interceptor();
    let flow = itc.alloc_flow(&view(port, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
    itc.dial_upstream(flow, DialTarget::Tcp(listener.local_addr().unwrap()));
    let (mut upstream, _) = listener.accept().unwrap();
    itc.on_connect_done(flow);
    // Enqueue is not sending: the pre-flush sample must have no upstream bytes.
    itc.flows
        .get_mut(&flow)
        .unwrap()
        .io
        .as_mut()
        .unwrap()
        .out_tcp
        .push(b"request");
    assert_eq!(itc.drain_quality().records[0].up_bytes, 0);
    itc.flush_out(flow);
    let sent = itc.drain_quality();
    assert_eq!(sent.records[0].up_bytes, 7);
    std::thread::sleep(Duration::from_millis(20));
    let mut request = [0u8; 7];
    upstream.read_exact(&mut request).unwrap();
    assert_eq!(&request, b"request");
    upstream.write_all(b"reply").unwrap();
    let until = Instant::now() + Duration::from_secs(1);
    while itc.flows[&flow].quality.record.total_down_bytes < 5 {
        itc.read_upstream(flow);
        assert!(
            Instant::now() < until,
            "private upstream response must arrive"
        );
        std::thread::yield_now();
    }
    let received = itc.drain_quality();
    assert_eq!(
        (received.records[0].up_bytes, received.records[0].down_bytes),
        (0, 5)
    );
    assert!(received.records[0].first_upstream_byte_ms.unwrap() >= 20);
    assert_eq!(received.records[0].total_up_bytes, 7);
    // Short close emits only the remaining delta, not the lifetime byte count again.
    itc.teardown_flow(flow);
    let ended = itc.drain_quality();
    assert_eq!(
        (ended.records[0].up_bytes, ended.records[0].down_bytes),
        (0, 0)
    );
    assert_eq!(
        (
            ended.records[0].total_up_bytes,
            ended.records[0].total_down_bytes
        ),
        (7, 5)
    );
    assert!(ended.records[0].tcp_metrics.is_some());
}

#[test]
fn connect_timeout_and_immediate_failure_survive_remove_paths() {
    let mut itc = interceptor();
    let pending = itc.alloc_flow(&view(80, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
    // Deterministic pending fd with expired connect deadline; no network blackhole assumption.
    let fd = sysock::socket(
        sysock::pack_addr(&"127.0.0.1:80".parse().unwrap()).domain,
        sysock::SOCK_STREAM,
    )
    .unwrap();
    itc.place_io(
        pending,
        fd,
        false,
        Some(ConnState::InProgress),
        Instant::now() - Duration::from_secs(1),
    );
    itc.reap_idle();
    let timeout = itc.drain_quality();
    assert_eq!(timeout.records.len(), 1);
    assert_eq!(timeout.records[0].connect_state, ConnectState::Failed);
    assert_eq!(timeout.records[0].error_kind, Some(FlowError::Timeout));
    assert_eq!(
        timeout.records[0].close_reason,
        Some(CloseReason::DialFailed)
    );
    assert!(timeout.records[0].closed);

    let failed = itc.alloc_flow(&view(81, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
    itc.note_quality_error(failed, FlowError::Connect, Some(12345));
    itc.on_dial_failed(failed);
    let failed = itc.drain_quality();
    assert_eq!(failed.records[0].os_error, Some(12345));
    assert_eq!(failed.records[0].connect_state, ConnectState::Failed);
    assert!(itc.flows.is_empty());
}

#[test]
fn bounded_short_flow_queue_reports_overflow_once_and_reuses_capacity() {
    let mut itc = interceptor();
    for _ in 0..CLOSED_CAPACITY + 3 {
        let flow = itc.alloc_flow(&view(80, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
        itc.remove_flow(flow);
    }
    assert_eq!(itc.quality_closed.len(), CLOSED_CAPACITY);
    let batch = itc.drain_quality();
    assert_eq!(
        (batch.records.len(), batch.closed_overflow),
        (CLOSED_CAPACITY, 3)
    );
    assert_eq!(itc.drain_quality().closed_overflow, 0);
    let flow = itc.alloc_flow(&view(80, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
    itc.remove_flow(flow);
    assert_eq!(itc.drain_quality().records.len(), 1);
}

#[test]
fn udp_local_ready_with_no_reply_stays_successfully_local_not_remote_failure() {
    let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = socket.local_addr().unwrap().port();
    let mut itc = interceptor();
    let flow = itc.alloc_flow(&view(port, Proto::Udp), Kind::Transit, Proto::Udp, vec![]);
    itc.dial_upstream(flow, DialTarget::Udp(socket.local_addr().unwrap()));
    itc.finish_udp(flow);
    let q = itc.drain_quality().records.remove(0);
    assert_eq!(q.connect_state, ConnectState::UdpLocalReady);
    assert_eq!(q.error_kind, None);
    assert_eq!(q.first_upstream_byte_ms, None);
    assert_eq!(q.tcp_metrics, None);
}

#[test]
fn upstream_write_error_keeps_errno_and_does_not_count_buffered_bytes_as_sent() {
    let mut itc = interceptor();
    let flow = itc.alloc_flow(&view(80, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
    let fd = sysock::socket(
        sysock::pack_addr(&"127.0.0.1:80".parse().unwrap()).domain,
        sysock::SOCK_STREAM,
    )
    .unwrap();
    sysock::set_nonblocking(&fd, true).unwrap();
    itc.place_io(flow, fd, false, None, Instant::now());
    itc.flows
        .get_mut(&flow)
        .unwrap()
        .io
        .as_mut()
        .unwrap()
        .out_tcp
        .push(b"unsent");
    itc.flush_out(flow);
    itc.remove_flow(flow);
    let q = itc.drain_quality().records.remove(0);
    assert_eq!(q.error_kind, Some(FlowError::Write));
    assert!(q.os_error.is_some());
    assert_eq!(q.up_bytes, 0);
    assert_eq!(q.up_queue_peak_bytes, 6);
    assert_eq!(q.close_reason, Some(CloseReason::Error));
}

#[test]
fn upstream_read_reset_is_distinguished_from_clean_eof() {
    let listener = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut itc = interceptor();
    let flow = itc.alloc_flow(&view(port, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
    itc.dial_upstream(flow, DialTarget::Tcp(listener.local_addr().unwrap()));
    let (upstream, _) = listener.accept().unwrap();
    itc.on_connect_done(flow);
    sysock::rst_close_tcp(&upstream);
    drop(upstream);
    let until = Instant::now() + Duration::from_secs(1);
    loop {
        itc.read_upstream(flow);
        if itc.flows[&flow].io.is_none() {
            break;
        }
        assert!(
            Instant::now() < until,
            "RST must arrive on private loopback"
        );
        std::thread::yield_now();
    }
    itc.remove_flow(flow);
    let q = itc.drain_quality().records.remove(0);
    assert_eq!(q.error_kind, Some(FlowError::Read));
    assert!(q.os_error.is_some());
    assert_eq!(q.close_reason, Some(CloseReason::Error));
    assert!(q.tcp_metrics.is_some());
}

#[cfg(unix)]
#[test]
fn actual_would_block_records_queue_pressure_across_samples() {
    use std::io::Read;
    let (up, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    up.set_nonblocking(true).unwrap();
    peer.set_nonblocking(true).unwrap();
    let fd: sysock::SysSock = up.into();
    sysock::setsockopt_i32(
        sysock::sock_raw(&fd),
        sysock::SOL_SOCKET,
        libc::SO_SNDBUF,
        4096,
    )
    .unwrap();
    let mut itc = interceptor();
    let flow = itc.alloc_flow(&view(80, Proto::Tcp), Kind::Transit, Proto::Tcp, vec![]);
    itc.place_io(flow, fd, false, None, Instant::now());
    itc.flows
        .get_mut(&flow)
        .unwrap()
        .io
        .as_mut()
        .unwrap()
        .out_tcp
        .push(&vec![1u8; WATERMARK]);
    itc.flush_out(flow);
    assert!(
        itc.pending_out(flow) > 0,
        "small OS send buffer must block without a reader"
    );
    std::thread::sleep(Duration::from_millis(12));
    let a = itc.drain_quality().records.remove(0);
    assert!(a.up_backpressure_ms >= 10);
    assert!(a.up_queue_bytes > 0);
    assert_eq!(a.up_queue_peak_bytes, WATERMARK);
    let mut buf = [0u8; 65536];
    let until = Instant::now() + Duration::from_secs(1);
    while itc.pending_out(flow) > 0 {
        while peer.read(&mut buf).is_ok() {}
        itc.flush_out(flow);
        assert!(Instant::now() < until);
    }
    let b = itc.drain_quality().records.remove(0);
    assert_eq!(b.up_queue_bytes, 0);
    assert_eq!(b.total_up_bytes, WATERMARK as u64);
    assert_eq!(a.up_bytes + b.up_bytes, WATERMARK as u64);
    itc.remove_flow(flow);
}
