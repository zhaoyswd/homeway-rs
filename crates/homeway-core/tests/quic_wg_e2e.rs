//! **`_wg` 档全链端到端**（M1 S3-1 的判据④：A/B 开关的全部价值所在）+ **`serve.quic=false`
//! 的 token 逐字节回退实证**（S3-4）。
//!
//! **`#[ignore]` 的形态说明**：两条用例都要求外部先起好一个 **`--quic=false`** 的本地
//! Rust 出口（端口/凭据都在它的 state 里），故不进 `cargo test --workspace` 的常跑面；
//! 驱动脚本 = `tools/quic-wg-e2e.sh`（起出口 → 取 token → 设环境 → 跑本用例 → 留证到 /tmp）。
//!
//! 环境契约：
//! - `HOMEWAY_WG_E2E_TOKEN`：出口 token（`serve token` 输出里抽 `hmw1…`）
//! - `HOMEWAY_WG_E2E_EXIT_LOG`：出口 stdout 日志（查 `peer: +` / `intercept: tcp …`）

use std::io::Read as _;
use std::net::{Ipv4Addr, UdpSocket};
use std::os::fd::AsRawFd;
use std::os::unix::net::{UnixDatagram, UnixStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_core::facade::bridge_host::bridge_client_auth;
use homeway_core::facade::demand::DemandSignals;
use homeway_core::facade::tun_exec::TunnelExec;
use homeway_core::facade::ClientCore;

/// 轮询上界（只判上界——flake 口径②）。
const WAIT: Duration = Duration::from_secs(25);

/// 等待判据（有界轮询；返回最后一次读数）。
fn wait_until(mut f: impl FnMut() -> bool, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if f() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn log_lines(path: &PathBuf) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// 有界等待日志文件**第 `skip` 行之后**出现 `needle`（可选同时含 `also`；返回命中行）。
fn wait_log_from(
    path: &PathBuf,
    skip: usize,
    needle: &str,
    also: Option<&str>,
    wait: Duration,
) -> Option<String> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(l) = s
                .lines()
                .skip(skip)
                .find(|l| l.contains(needle) && also.is_none_or(|a| l.contains(a)))
            {
                return Some(l.to_owned());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// 内层 IPv4 + UDP 包（IPv4 头校验和 + UDP 校验和**真算**——留 0 会被出口 smoltcp 静默丢）。
fn inet_checksum(buf: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < buf.len() {
        sum += u16::from_be_bytes([buf[i], buf[i + 1]]) as u32;
        i += 2;
    }
    if i < buf.len() {
        sum += (buf[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn inner_udp(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 20 + 8 + payload.len()];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
    p[8] = 64;
    p[9] = 17;
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    let ip_sum = inet_checksum(&p[..20]);
    p[10..12].copy_from_slice(&ip_sum.to_be_bytes());
    p[20..22].copy_from_slice(&sport.to_be_bytes());
    p[22..24].copy_from_slice(&dport.to_be_bytes());
    p[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    p[28..].copy_from_slice(payload);
    let mut pseudo = Vec::with_capacity(12 + 8 + payload.len());
    pseudo.extend_from_slice(&src.octets());
    pseudo.extend_from_slice(&dst.octets());
    pseudo.push(0);
    pseudo.push(17);
    pseudo.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    pseudo.extend_from_slice(&p[20..]);
    let mut udp_sum = inet_checksum(&pseudo);
    if udp_sum == 0 {
        udp_sum = 0xffff;
    }
    p[26..28].copy_from_slice(&udp_sum.to_be_bytes());
    p
}

/// 世代日志里找一行（`None` = 上界内没出现）。
fn find_line(log: &PathBuf, needle: &str) -> Option<String> {
    std::fs::read_to_string(log)
        .ok()?
        .lines()
        .find(|l| l.contains(needle))
        .map(str::to_owned)
}

/// 起一个 `transport=<bearer>` 的世代并等 `state=ready`；返回 (core, 世代日志路径, tun socketpair)。
#[allow(clippy::type_complexity)]
fn prepare_generation(
    bearer: &str,
    tag: &str,
) -> (Arc<ClientCore>, PathBuf, UnixDatagram, UnixDatagram) {
    let token = std::env::var("HOMEWAY_WG_E2E_TOKEN").expect("须给 HOMEWAY_WG_E2E_TOKEN");
    let dir = std::env::temp_dir().join(format!("hw-m1s3-e2e-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{token}","out":"{}","identityDir":"{}","transport":"{bearer}"}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(Arc::clone(&demand));
    let core = Arc::new(ClientCore::with_shared(exec, demand));
    assert_eq!(core.tun_prepare(&cfg, true), 0, "prepare 受理");
    assert!(
        wait_until(
            || core.tun_status().contains("\"state\":\"ready\""),
            WAIT
        ),
        "世代须在预算内 ready：{}",
        core.tun_status()
    );
    let (tun, peer) = UnixDatagram::pair().expect("socketpair(DGRAM)");
    assert_eq!(core.tun_attach(tun.as_raw_fd(), 1280), 0, "attach 受理");
    assert!(
        wait_until(
            || core.tun_status().contains("\"state\":\"attached\""),
            WAIT
        ),
        "世代须在预算内 attached：{}",
        core.tun_status()
    );
    (core, out, tun, peer)
}

/// **判据（S3-1 ④）**：`transport=wg` 全链——L3 走 WG（真内层流量经出口 intercept
/// transit 回环）+ 服务面走 WG（隧道的 term 桥经 WG 会话拨到出口 `tunnel_ip:7724`，
/// 出口打 `intercept: tcp exempt …（dialok）`）；且 C2/C4/C5/C6/C10/C15 **打原串**、
/// `quic:` 族行**零输出**。
#[test]
#[ignore = "端到端：需 `--quic=false` 的本地 Rust 出口在跑（tools/quic-wg-e2e.sh 驱动）"]
fn wg_bearer_full_chain_keeps_original_criterion_lines() {
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_WG_E2E_EXIT_LOG").expect("须给 HOMEWAY_WG_E2E_EXIT_LOG"),
    );
    let log0 = log_lines(&exit_log);

    // 出口本地 UDP echo（transit 目标：同机回环，不依赖外网）
    let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("echo 可绑");
    let echo_addr = match echo.local_addr().expect("echo 地址") {
        std::net::SocketAddr::V4(v) => v,
        std::net::SocketAddr::V6(_) => unreachable!(),
    };
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let echo_thread = {
        let stop = Arc::clone(&stop);
        let sock = echo.try_clone().expect("echo 克隆");
        std::thread::spawn(move || {
            sock.set_read_timeout(Some(Duration::from_millis(50))).ok();
            let mut buf = [0u8; 2048];
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    let _ = sock.send_to(&buf[..n], from);
                }
            }
        })
    };

    let (core, gen_log, _tun, peer) = prepare_generation("wg", "wg");
    // 世代日志判据行（原串——A/B 开关的价值面）
    assert!(
        find_line(&gen_log, "wgcore: 隧道侧就绪（L3 直通；").is_some(),
        "C2 原串须在场：{:?}",
        std::fs::read_to_string(&gen_log).unwrap_or_default()
    );
    let c2 = find_line(&gen_log, "wgcore: 隧道侧就绪（L3 直通；").unwrap();
    println!("[wg] C2={c2}");
    let c4 = wait_log_from(&gen_log, 0, "MIRROR 镜像包#", None, WAIT).expect("C4 原串须在场");
    println!("[wg] C4={c4}");
    let c5 = wait_log_from(&gen_log, 0, "赛跑结算：胜出 ", Some("镜像 "), WAIT).expect("C5 原串须在场");
    println!("[wg] C5={c5}");
    let c6 = wait_log_from(&gen_log, 0, "路径确立：", Some("首个回包来源"), WAIT).expect("C6 原串须在场");
    println!("[wg] C6={c6}");
    let c15 =
        wait_log_from(&gen_log, 0, "RREG 注册刷新 → ", None, WAIT).expect("C15 原串须在场（首拍补注册）");
    println!("[wg] C15={c15}");

    // ---- L3（WG）：内层 UDP → 出口 intercept transit → 回环 echo → 回程 ----
    // src = `tunIp`（状态面公布的**本设备派生地址**——出口 `src_allowed` 判据面；
    // 不能拿 token 的 peerId 自己推：那是后端公钥，派生结果不是本设备地址）
    let status0: serde_json::Value =
        serde_json::from_str(&core.tun_status()).expect("tun_status 是 JSON");
    let tun_ip: Ipv4Addr = status0["tunIp"]
        .as_str()
        .expect("状态面须公布 tunIp")
        .parse()
        .expect("tunIp 是 IPv4");
    println!("[wg] tun_ip={tun_ip}");
    let payload = b"hw-m1-s3-wg";
    let inner = inner_udp(tun_ip, Ipv4Addr::LOCALHOST, 40001, echo_addr.port(), payload);
    peer.send(&inner).expect("写 TUN 一包");
    let deadline = Instant::now() + WAIT;
    let mut back: Option<Vec<u8>> = None;
    while Instant::now() < deadline {
        peer.set_read_timeout(Some(Duration::from_millis(200))).ok();
        let mut buf = vec![0u8; 4096];
        if let Ok(n) = peer.recv(&mut buf) {
            if buf[..n].len() >= 28 + payload.len() && buf[28..n].starts_with(payload) {
                back = Some(buf[..n].to_vec());
                break;
            }
        }
    }
    let back = back.expect("L3 回程必须经 WG 回到 TUN fd（载荷逐字节）");
    println!(
        "[wg] tun.uplink={}B tun.downlink={}B payload={:?}",
        inner.len(),
        back.len(),
        String::from_utf8_lossy(&back[28..])
    );

    // ---- C10 快照行（巡检首拍立即探；`link: via=… ep=… rtt=…ms（新栈状态快照）`）----
    let c10 = wait_log_from(&gen_log, 0, "link: via=", Some("（新栈状态快照）"), WAIT)
        .expect("C10 快照行须在场");
    println!("[wg] C10={c10}");

    // ---- 服务面（WG 会话）：term 桥经 WG 拨出口 tunnel_ip:7724 ----
    let status = core.tun_status();
    let v: serde_json::Value = serde_json::from_str(&status).expect("tun_status 是 JSON");
    assert!(v.get("quic").is_none(), "wg 档状态 JSON 不得含 quic 段：{status}");
    let auth = v["bridgeAuth"].as_str().unwrap_or_default().to_owned();
    let term_sock = v["bridgeTermSock"].as_str().unwrap_or_default().to_owned();
    assert!(!auth.is_empty() && !term_sock.is_empty(), "桥须在场：{status}");
    assert!(
        PathBuf::from(&term_sock).exists(),
        "term 桥 socket 须落盘：{term_sock}"
    );
    let mut c = UnixStream::connect(&term_sock).expect("连 term 桥");
    bridge_client_auth(&mut c, &auth).expect("桥鉴权首包可写");
    let dial_line = wait_log_from(
        &exit_log,
        log0,
        "intercept: tcp exempt ",
        Some("（dialok）"),
        WAIT,
    );
    let dial_line = dial_line.expect("服务面必须经 WG 会话拨到出口 term 端口（E10 exempt 形态）");
    println!("[wg] exit.service_dial={dial_line}");
    let mut probe = [0u8; 1];
    let _ = c.read(&mut probe); // 桥泵已把连接接上出口服务（读到 EOF 也算接通）
    drop(c);

    // ---- 零 `quic:` 族行（回退档的观测面契约）----
    let all = std::fs::read_to_string(&gen_log).unwrap_or_default();
    let quic_lines: Vec<&str> = all.lines().filter(|l| l.contains("quic:")).collect();
    assert!(
        quic_lines.is_empty(),
        "回退档下 `quic:` 族行必须零输出，实得：{quic_lines:?}"
    );
    println!("[wg] quic_lines=0");

    assert!(core.tun_stop() >= -1, "停世代");
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = echo_thread.join();
    println!("[wg] done");
}

/// **判据（S3-4）**：`serve.quic=false` 的 token ①无 QUIC 类端点 ②无 `rpk` 尾字段
/// ③**再编码逐字节等于原串**（= M1 前形态；与 `tests/token_vectors.rs` 的 Go 冻结向量
/// 同一判据面）。
#[test]
#[ignore = "端到端：需 `--quic=false` 的本地 Rust 出口在跑（tools/quic-wg-e2e.sh 驱动）"]
fn serve_quic_false_token_is_byte_identical_to_pre_m1() {
    let token = std::env::var("HOMEWAY_WG_E2E_TOKEN").expect("须给 HOMEWAY_WG_E2E_TOKEN");
    let tok = homeway_core::token::decode(&token).expect("token 可解");
    assert!(tok.rpk.is_none(), "serve.quic=false ⇒ token 不带 rpk 尾字段");
    assert!(
        !tok.endpoints
            .iter()
            .any(|e| e.kind == homeway_core::token::EndpointKind::Quic),
        "serve.quic=false ⇒ token 不带 QUIC 类端点：{:?}",
        tok.endpoints
    );
    // 再编码逐字节一致（encoder 恒产规范形 ⇒ 与 M1 前的同载荷串逐字节相同；
    // 格式面的 Go 冻结向量对账在 `tests/token_vectors.rs`）
    let eps: Vec<homeway_core::token::EndpointRef<'_>> = tok
        .endpoints
        .iter()
        .map(|e| homeway_core::token::EndpointRef::new(&e.addr, e.kind))
        .collect();
    let re = homeway_core::token::encode(&homeway_core::token::TokenSpec {
        peer_id: &tok.peer_id,
        secret: &tok.secret,
        endpoints: &eps,
        rpk: None,
    })
    .expect("再编码可行");
    assert_eq!(re, token, "再编码必须逐字节等于出口铸出的串");
    println!(
        "[quic=false] token.len={} endpoints={} kinds={:?} rpk=None byte_identical=true",
        token.len(),
        tok.endpoints.len(),
        tok.endpoints.iter().map(|e| e.kind).collect::<Vec<_>>()
    );
}
