//! WG 线上字节探针（`quic-ab.sh overhead` 的 WG 侧口径；M0 新增——lab 的 32B 结论原先
//! 没有留存可复现的仪器，见 tools/quic-ab/README.md 的口径节）。
//!
//! 形态：单进程两个 `Tunn`（客户端/服务端）走完握手，再对一枚 payload 字节的内层 IPv4 包
//! 做 `encapsulate`，打印**线上字节数**（= 内层包 + WG 头 16B + AEAD tag 16B ⇒ 1280 → 1312）。
//!
//! 为什么不改 WG 臂源码：口径可比优先（臂文件从 lab 原样转入），本件是**只读探针**。

use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};
use quic_ab_common::{env_num, ipv4, JsonLine};
use std::net::{IpAddr, Ipv4Addr};

fn keys(seed: u8) -> (StaticSecret, PublicKey) {
    let sk = StaticSecret::from([seed; 32]);
    (sk.clone(), PublicKey::from(&sk))
}

/// 反复 decapsulate 到 Done，收齐所有「要发出去的」数据报。
fn drain(tunn: &mut Tunn, input: &[u8], out: &mut [u8]) -> Vec<Vec<u8>> {
    let mut sent = Vec::new();
    let mut first = true;
    loop {
        let inp: &[u8] = if first { input } else { &[] };
        match tunn.decapsulate(Some(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))), inp, out) {
            TunnResult::WriteToNetwork(w) => sent.push(w.to_vec()),
            TunnResult::WriteToTunnelV4(_, _) => {}
            TunnResult::WriteToTunnelV6(_, _) => {}
            TunnResult::Done | TunnResult::Err(_) => break,
        }
        first = false;
    }
    sent
}

fn main() {
    let payload: usize = env_num("PAYLOAD", 1280usize);
    let (csk, cpk) = keys(0x11);
    let (ssk, spk) = keys(0x22);

    let mut cli = Tunn::new(csk, spk, None, None, 0x1000, None).unwrap();
    let mut srv = Tunn::new(ssk, cpk, None, None, 0x2000, None).unwrap();

    let mut obuf = vec![0u8; 65536];
    let plain = ipv4([10, 0, 0, 2], [10, 0, 0, 1], payload);

    // 1) 客户端首包：产出握手发起（数据包被排队）
    let init = match cli.encapsulate(&plain, &mut obuf) {
        TunnResult::WriteToNetwork(w) => w.to_vec(),
        other => {
            eprintln!("意外：客户端 encapsulate 未产出握手发起（{other:?}）");
            std::process::exit(2);
        }
    };
    let init_len = init.len();

    // 2) 服务端处理发起 → 产出握手响应
    let mut sbuf = vec![0u8; 65536];
    let resp = drain(&mut srv, &init, &mut sbuf);
    let resp_len = resp.first().map(|r| r.len()).unwrap_or(0);
    if resp_len == 0 {
        eprintln!("意外：服务端未产出握手响应");
        std::process::exit(3);
    }

    // 3) 客户端处理响应（会话建立；排队数据包会在此冲出，不计入本次量测）
    let _ = drain(&mut cli, &resp[0], &mut obuf);

    // 4) 会话已建立：干净地 encapsulate 一枚数据包 —— 这就是「线上每包字节」
    let data_len = match cli.encapsulate(&plain, &mut obuf) {
        TunnResult::WriteToNetwork(w) => w.len(),
        other => {
            eprintln!("意外：数据包 encapsulate 未产出线上帧（{other:?}）");
            std::process::exit(4);
        }
    };

    println!(
        "{}",
        JsonLine::new("wg-size")
            .num("payload", payload as u64)
            .num("handshake_init", init_len as u64)
            .num("handshake_resp", resp_len as u64)
            .num("data_wire", data_len as u64)
            .int("overhead", data_len as i64 - payload as i64)
            .render()
    );
}
