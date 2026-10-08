//! WG 臂（wg-ring / wg-shim **共用本文件**——两臂唯一差别 = ring 来源，见各自 Cargo.toml）：
//! boringtun 双向隧道，1280B 内层包往返。
//!
//! 形态对齐本仓产品口径：
//! - 内层 1280B（= stackb::MTU）、keepalive=None、rate_limiter=None（= make_tunn）
//! - 满载往返（bulk 形态）；客户端计入 encapsulate+decapsulate 两侧 CPU
//! - 密钥确定性（固定种子）——免握手协调，可复现
//!
//! 度量：客户端进程 user+sys CPU / 成功往返数 = **每包 CPU 时间**（主指标）。
//! 墙钟吞吐只作同刻相对参考（本机负载会让它漂移，见 PERF-AB §9.15.1）。
//!
//! 转入自 `/tmp/quic-lab/wg-ring/src/bin/wg.rs`（仅 `use` 面改走 `quic-ab-common`
//! ——ippkt/CPU/JSON 三件在 arms 布局里由 common 提供）。

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use quic_ab_common::{cpu_now_us, env_num, ipv4, JsonLine};

const PKT: usize = 1280;
const CAP: usize = 4096;
const WARMUP: usize = 5_000;
const RECV_TIMEOUT: Duration = Duration::from_millis(1500);

/// 确定性密钥：客户端/服务端各一个，免去握手期交换公钥的协调。
fn client_keys() -> (StaticSecret, PublicKey) {
    let sk = StaticSecret::from([0x11u8; 32]);
    (sk.clone(), PublicKey::from(&sk))
}
fn server_keys() -> (StaticSecret, PublicKey) {
    let sk = StaticSecret::from([0x22u8; 32]);
    (sk.clone(), PublicKey::from(&sk))
}

/// 收一个数据报 + decap 到 Done（WriteToNetwork 全部回发）。返回解出的明文（可空）。
fn inbound(
    tunn: &mut Tunn,
    datagram: Option<&[u8]>,
    sock: &UdpSocket,
    peer: SocketAddr,
    out: &mut [u8],
) -> Vec<u8> {
    let mut plain: Vec<u8> = Vec::new();
    let mut first = true;
    loop {
        let src = Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1)));
        let input: &[u8] = if first { datagram.unwrap_or(&[]) } else { &[] };
        match tunn.decapsulate(src, input, out) {
            TunnResult::WriteToNetwork(w) => {
                let _ = sock.send_to(w, peer);
            }
            TunnResult::WriteToTunnelV4(p, _) => plain = p.to_vec(),
            TunnResult::WriteToTunnelV6(_, _) => {}
            TunnResult::Done => break,
            TunnResult::Err(_) => break,
        }
        first = false;
    }
    plain
}

fn server() -> io::Result<()> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    println!("PORT {}", sock.local_addr()?.port());
    use std::io::Write as _;
    io::stdout().flush()?;

    let (sk, _pk) = server_keys();
    let (_csk, cpk) = client_keys();
    // index 任意（两端独立选）；rate_limiter=None 对齐产品（make_tunn）
    let mut tunn = Tunn::new(sk, cpk, None, None, 0x2000, None).unwrap();

    let mut buf = vec![0u8; 65535];
    let mut out = vec![0u8; CAP];
    loop {
        let (n, peer) = match sock.recv_from(&mut buf) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let plain = inbound(&mut tunn, Some(&buf[..n]), &sock, peer, &mut out);
        if !plain.is_empty() {
            // 内层包原样回：encapsulate → 发出（形态同出口 device.encapsulate）
            if let TunnResult::WriteToNetwork(w) = tunn.encapsulate(&plain, &mut out) {
                let _ = sock.send_to(w, peer);
            }
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arm = std::env::var("ARM").unwrap_or_else(|_| "wg".into());

    if args.get(1).map(|s| s.as_str()) == Some("server") {
        server().unwrap();
        return;
    }

    let port: u16 = args[1].parse().unwrap();
    let n: usize = args[2].parse().unwrap();
    let peer: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();

    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(RECV_TIMEOUT)).unwrap();
    let (sk, _pk) = client_keys();
    let (_ssk, spk) = server_keys();
    let mut tunn = Tunn::new(sk, spk, None, None, 0x1000, None).unwrap();

    // 合法 IPv4 包（decap 侧会按 IP 头校验；形态同真实栈 B/TUN 投递）
    let payload: usize = env_num("PAYLOAD", PKT);
    let plain = ipv4([10, 0, 0, 2], [10, 0, 0, 1], payload);
    let mut obuf = vec![0u8; CAP];
    let mut rbuf = vec![0u8; 65535];
    let mut ibuf = vec![0u8; CAP];

    let step = |tunn: &mut Tunn, obuf: &mut [u8], rbuf: &mut [u8], ibuf: &mut [u8]| -> bool {
        match tunn.encapsulate(&plain, obuf) {
            TunnResult::WriteToNetwork(w) => {
                let _ = sock.send_to(w, peer);
            }
            TunnResult::Err(_) => return false,
            _ => {}
        }
        match sock.recv_from(rbuf) {
            Ok((n, from)) => {
                let got = inbound(tunn, Some(&rbuf[..n]), &sock, from, ibuf);
                !got.is_empty()
            }
            Err(_) => false,
        }
    };

    let mut fails = 0usize;
    for _ in 0..WARMUP {
        if !step(&mut tunn, &mut obuf, &mut rbuf, &mut ibuf) {
            fails += 1;
        }
    }
    eprintln!("warmup 完成：{WARMUP} 步，失败 {fails}");
    let idle_secs: u64 = env_num("IDLE_SECS", 0u64);
    if idle_secs > 0 {
        eprintln!("IDLE 模式：hold {idle_secs}s（无流量）");
        std::thread::sleep(std::time::Duration::from_secs(idle_secs));
        println!("{{\"arm\":\"wg-idle\",\"idle_secs\":{idle_secs}}}");
        return;
    }
    let (u0, s0) = cpu_now_us();
    let t0 = Instant::now();
    let mut done = 0usize;
    fails = 0;
    for i in 0..n {
        if step(&mut tunn, &mut obuf, &mut rbuf, &mut ibuf) {
            done += 1;
        } else {
            fails += 1;
        }
        if i % 1000 == 999 {
            eprintln!(
                "  进度 {}/{n}：成功 {done} 失败 {fails}，已用 {:.1}s",
                i + 1,
                t0.elapsed().as_secs_f64()
            );
        }
    }
    let wall = t0.elapsed();
    let (u1, s1) = cpu_now_us();

    let cpu_us = ((u1 - u0) + (s1 - s0)) as f64;
    println!(
        "{}",
        JsonLine::new(&arm)
            .num("pkts", done as u64)
            .num("payload", payload as u64)
            .f("wall_ms", wall.as_secs_f64() * 1000.0, 1)
            .f("cpu_ms", cpu_us / 1000.0, 1)
            .f("cpu_us_per_pkt", cpu_us / done.max(1) as f64, 3)
            .f(
                "mbps",
                (done * payload * 2) as f64 * 8.0 / wall.as_secs_f64() / 1e6,
                1
            )
            .render()
    );
}
