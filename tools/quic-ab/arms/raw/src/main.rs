//! raw UDP 地板臂：1280B 包 sendto/recv_from 往返，测「传输+内核」成本。
//!
//! 口径：客户端发 N 包、服务端原样回——与 WG/QUIC 臂同形的「发包数」成本，不含任何加密。
//! 转入自 `/tmp/quic-lab/raw/src/bin/raw.rs`（唯二调整：CPU/JSON 走 `quic-ab-common`；
//! 载荷跟 `PAYLOAD` 环境变量走，与另两臂同口径）。

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

use quic_ab_common::{cpu_now_us, env_num, JsonLine};

const WARMUP: usize = 20_000;

fn server() -> io::Result<()> {
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    println!("PORT {}", sock.local_addr()?.port());
    use std::io::Write;
    io::stdout().flush()?;
    let mut buf = vec![0u8; 65535];
    loop {
        match sock.recv_from(&mut buf) {
            Ok((n, peer)) => {
                let _ = sock.send_to(&buf[..n], peer);
            }
            Err(_) => continue,
        }
    }
}

fn client(port: u16, n: usize) -> io::Result<()> {
    let server: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let sock = UdpSocket::bind("127.0.0.1:0")?;
    sock.connect(server)?;
    let pkt_len: usize = env_num("PAYLOAD", 1280usize);
    let pkt = vec![0x41u8; pkt_len];
    let mut buf = vec![0u8; 65535];

    // 暖机（页错误/分支预测/时钟稳定）
    for _ in 0..WARMUP {
        sock.send(&pkt)?;
        let _ = sock.recv(&mut buf);
    }

    let idle_secs: u64 = env_num("IDLE_SECS", 0u64);
    if idle_secs > 0 {
        eprintln!("IDLE 模式：hold {idle_secs}s（无流量）");
        std::thread::sleep(Duration::from_secs(idle_secs));
        println!("{{\"arm\":\"raw-idle\",\"idle_secs\":{idle_secs}}}");
        return Ok(());
    }

    let (u0, s0) = cpu_now_us();
    let t0 = Instant::now();
    for _ in 0..n {
        sock.send(&pkt)?;
        let _ = sock.recv(&mut buf);
    }
    let wall = t0.elapsed();
    let (u1, s1) = cpu_now_us();

    let cpu_us = (u1 - u0) + (s1 - s0);
    println!(
        "{}",
        JsonLine::new("raw")
            .num("pkts", n as u64)
            .num("payload", pkt_len as u64)
            .f("wall_ms", wall.as_secs_f64() * 1000.0, 1)
            .f("cpu_ms", cpu_us as f64 / 1000.0, 1)
            .f("cpu_us_per_pkt", cpu_us as f64 / n as f64, 3)
            .f(
                "mbps",
                (n * pkt_len * 2) as f64 * 8.0 / wall.as_secs_f64() / 1e6,
                1
            )
            .render()
    );
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("server") {
        server().unwrap();
    } else {
        let port: u16 = args[1].parse().unwrap();
        let n: usize = args[2].parse().unwrap();
        client(port, n).unwrap();
    }
}
