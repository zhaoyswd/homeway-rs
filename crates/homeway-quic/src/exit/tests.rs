//! 出口 QUIC 面单测（**真线程 + 真回环 IO**；不钉固定端口、时间断言只判上界——
//! M0 设计 §9.2 flake 口径①②）。
//!
//! 断言面：E-q1 就绪行（实际端口/MTU/缓冲）、收工预算与幂等、端口释放；
//! 「对 pin/错 pin 握手」与资源上限用例随 S1a 的后续工作单元进本文件。

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cmd::Logf;
use crate::rpk::Ed25519Seed;
use crate::{ExitQuic, ExitQuicConfig};

/// 收工预算（与 `wgcore::CLIENT_CLOSE_BUDGET` 同量级 = 2s）。
const BUDGET: Duration = Duration::from_secs(2);
/// 日志等待上界（flake 口径②：只判上界）。
const WAIT: Duration = Duration::from_secs(8);

/// 日志落点 + 收集端。
fn sink() -> (Logf, Receiver<String>) {
    let (tx, rx) = channel();
    (
        Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }),
        rx,
    )
}

/// 有界收集日志行直到出现含 `needle` 的行（`recv_timeout` 当等待——不用阻塞 sleep）。
fn drain_until(rx: &Receiver<String>, needle: &str, wait: Duration) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left.max(Duration::from_millis(1))) {
            Ok(line) => {
                let hit = line.contains(needle);
                lines.push(line);
                if hit {
                    return lines;
                }
            }
            Err(RecvTimeoutError::Timeout) | Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    panic!("日志未在 {wait:?} 内出现「{needle}」（已收 {} 行：{lines:?}）", lines.len());
}

/// 回环 socket（端口由内核分配后读回——flake 口径①）。
fn loopback_socket() -> UdpSocket {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("回环 :0 必可绑")
}

fn seed(byte: u8) -> Ed25519Seed {
    Ed25519Seed::from_bytes([byte; 32])
}

/// E-q1：端点就绪行含**实际端口 / migration / initial_mtu / datagram 缓冲**。
#[test]
fn endpoint_logs_e_q1_ready_line_with_actual_port() {
    let sock = loopback_socket();
    let want_port = sock.local_addr().unwrap().port();
    let (logf, rx) = sink();
    let quic = ExitQuic::start(
        sock,
        ExitQuicConfig { rpk_seed: seed(1) },
        Arc::clone(&logf),
    )
    .expect("端点可起");

    let lines = drain_until(&rx, "quic: 端点就绪", WAIT);
    let line = lines.last().expect("就绪行在").clone();
    assert!(line.contains(&want_port.to_string()), "行含实际端口：{line}");
    assert!(line.contains("migration=true"), "行含 migration：{line}");
    assert!(line.contains("initial_mtu=1400"), "行含 initial_mtu：{line}");
    assert!(
        line.contains("datagram 缓冲 1048576B"),
        "行含 datagram 缓冲（1 MiB）：{line}"
    );
    assert_eq!(quic.local_addr().port(), want_port, "local_addr = 真实绑定端口");
    assert_eq!(
        quic.rpk_public_key().as_bytes().len(),
        32,
        "出口公钥 = 32B 裸 Ed25519"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET), "预算内收工");
    assert!(quic.is_finished(), "线程已退出");
}

/// 收工后端口释放（socket 随端点 drop 归还内核——退让语义依赖这条）。
#[test]
fn stop_releases_the_udp_port() {
    let sock = loopback_socket();
    let addr: SocketAddr = sock.local_addr().unwrap();
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(sock, ExitQuicConfig { rpk_seed: seed(2) }, logf).expect("端点可起");
    assert!(quic.stop_within(Instant::now() + BUDGET));
    drop(quic);
    // 同端口可再绑 = socket 真已释放（UDP 无 TIME_WAIT）
    UdpSocket::bind(addr).expect("收工后同端口应可再绑");
}

/// 收工幂等（重复 `stop_within` 不挂死、不重复 join）。
#[test]
fn stop_within_is_idempotent() {
    let (logf, _rx) = sink();
    let quic = ExitQuic::start(loopback_socket(), ExitQuicConfig { rpk_seed: seed(3) }, logf)
        .expect("端点可起");
    assert!(quic.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET), "重入即 true");
    assert!(quic.is_finished());
}
