//! 岛侧**端到端**（M1 S2a 的收口判据）：真本地出口（`tools/local-rust-exit.sh`）+
//! 真引擎 + 岛客户端 ⇒ 「连上 → 登记 → 出口 `peer: +` → rebind 迁移后仍通」。
//!
//! **`#[ignore]` 的形态说明**：本用例要求外部先起好一个出口实例（端口/凭据都在它的
//! state 里），故不进 `cargo test --workspace` 的常跑面；驱动脚本 =
//! `tools/quic-island-e2e.sh`（起出口 → 取 token → 设环境 → 跑本用例 → 留证到 /tmp）。
//!
//! 环境契约：
//! - `HOMEWAY_ISLAND_E2E_TOKEN`：出口 token（`serve token` 的输出里抽 `hmw1…`）
//! - `HOMEWAY_ISLAND_E2E_EXIT_LOG`：出口 stdout 日志（查 `peer: +` / `quic: 路径变更`）
//! - `HOMEWAY_ISLAND_E2E_ALT_BIND`（选填）：rebind 目标 `ip:port`（缺省自动挑一个本地地址）
//!
//! 断言面（每条一行 `key=value` 读数，供 `docs/reviews/M1.md` 摘抄）：
//! ①`Connect` Ok（赛跑胜出 + 登记）；②出口日志 `peer: +`（真设备表登记）；
//! ③`Probe` Ok（判活）；④`Rebind` Ok ⇒ 出口 `quic: 路径变更`（E-q2）+ 岛 `migrations=1`
//! （保持窗内收到对端回包 ⇒ 迁移完成）+ 出口再收一帧刷新（表内 E8 行 ← **新源**）
//! 证「收发继续」。

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use homeway_core::identity::Identity;
use homeway_core::token::{self, EndpointKind};
use homeway_quic::{
    Candidate, Cmd, Island, IslandConfig, IslandCredential, IslandReply, RaceOutcome, RpkPublicKey,
    StreamErr, StreamId, StreamReply, StreamTag, TokenSecret, Via,
};

/// 日志/状态轮询上界（只判上界——flake 口径②）。
const WAIT: Duration = Duration::from_secs(20);

/// 岛侧日志（stdout：`--nocapture` 时进读数包）。
fn logf() -> homeway_quic::Logf {
    std::sync::Arc::new(|s: &str| println!("[island] {s}"))
}

fn noop_unhealthy() -> homeway_quic::OnUnhealthy {
    std::sync::Arc::new(|r: &str| println!("[island][unhealthy] {r}"))
}

/// 发一条带 reply 的命令并**有界**取回（岛死 ⇒ 归 `EngineGone`，不挂死）。
fn cmd<T>(island: &Island, make: impl FnOnce(IslandReply<T>) -> Cmd, wait: Duration) -> Result<T, String> {
    let (tx, rx) = mpsc::channel();
    island.tx().send(make(tx)).map_err(|e| format!("投递失败：{e}"))?;
    match rx.recv_timeout(wait) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!("岛侧错误：{e}")),
        Err(mpsc::RecvTimeoutError::Timeout) => Err("回执超时（岛挂死？）".into()),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("回执口断开（岛已退出）".into()),
    }
}

/// 当前日志行数（本轮基线的起点——断言只认同一次运行里**新增**的行，
/// 否则上一轮的 `peer: +` 会让本轮断言假绿）。
fn log_lines(path: &PathBuf) -> usize {
    std::fs::read_to_string(path)
        .map(|s| s.lines().count())
        .unwrap_or(0)
}

/// 有界等待日志文件**第 `skip` 行之后**出现 `needle`（可选同时含 `also`；返回命中的行）。
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

/// 有界等待：日志里出现 `needle`，且它**位于**含 `after` 的那一行之后（行序 = 时间序；
/// 用「同一次运行内的新行」判时序，不引入墙钟断言——flake 口径②）。
fn wait_log_after(path: &PathBuf, skip: usize, needle: &str, after: &str, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path) {
            let lines: Vec<&str> = s.lines().skip(skip).collect();
            if let Some(pos) = lines.iter().position(|l| l.contains(after)) {
                if let Some(l) = lines[pos + 1..].iter().find(|l| l.contains(needle)) {
                    return Some((*l).to_owned());
                }
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// 本机非回环 IPv4（`connect` 只选路由、不发包）。
fn lan_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?;
    match s.local_addr().ok()? {
        std::net::SocketAddr::V4(v) if !v.ip().is_loopback() => Some(*v.ip()),
        _ => None,
    }
}

/// 迁移目标（同 S2a 单测的三档口径：127.0.0.2 → 本机网卡 → 同 IP 换端口）。
fn alt_bind() -> (SocketAddrV4, &'static str) {
    if let Ok(v) = std::env::var("HOMEWAY_ISLAND_E2E_ALT_BIND") {
        let a: SocketAddrV4 = v.parse().expect("HOMEWAY_ISLAND_E2E_ALT_BIND 形如 ip:port");
        return (a, "环境指定");
    }
    let alt = Ipv4Addr::new(127, 0, 0, 2);
    if UdpSocket::bind((alt, 0)).is_ok() {
        return (SocketAddrV4::new(alt, 0), "换 IP（127.0.0.2 别名）");
    }
    if let Some(ip) = lan_ipv4() {
        return (SocketAddrV4::new(ip, 0), "换 IP（本机网卡地址）");
    }
    (
        SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        "同 IP 换端口（本机无第二地址）",
    )
}

#[test]
#[ignore = "端到端：需本地出口在跑（tools/quic-island-e2e.sh 驱动）"]
fn island_connects_registers_and_survives_rebind_against_local_exit() {
    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let log0 = log_lines(&exit_log); // 本轮基线：断言只认本次运行新增的行
    let tok = token::decode(&token_str).expect("token 可解（serve token 的输出里抽 hmw1…）");
    let rpk = tok.rpk.expect("M1 的 token 必带出口 RPK（S1-9 的 additive 字段）");

    // 候选 = token 的 QUIC 类端点（M1 §2.7 的收窄：quic 档只吃 QUIC 类）。
    //
    // **同机回环隔离缝**（照 `homeway-cli token --loopback-only` 的既有先例）：token 里的
    // QUIC 内网端点带的是出口网卡地址；本用例的出口与岛在**同一台机器**上，走网卡地址
    // 会撞上 macOS 应用防火墙的入站拦（与「现役出口不碰」精神一致 —— 本机私有实例之间
    // 一律走回环）。端口不变 ⇒ 仍是「出口真实 QUIC 监听口」。
    let cands: Vec<Candidate> = tok
        .endpoints
        .iter()
        .filter(|e| e.kind == EndpointKind::Quic)
        .map(|e| {
            let raw: SocketAddrV4 = e.addr.parse().expect("QUIC 端点地址可解（ip:port）");
            println!("[e2e] token.quic_endpoint={raw}（本用例改用回环同端口）");
            Candidate {
                addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, raw.port()),
                via: Via::Direct,
            }
        })
        .collect();
    assert!(!cands.is_empty(), "token 必须带 QUIC 类端点：{:?}", tok.endpoints);

    let id = Identity::ephemeral().expect("临时身份");
    let cred = IslandCredential::new(
        TokenSecret::from_bytes(*tok.secret.as_bytes()),
        id.public_key(),
        *id.dev_tag().as_bytes(),
        RpkPublicKey::from_bytes(*rpk.as_bytes()),
    );
    let mut cfg = IslandConfig::new(cred);
    cfg.bind = Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    cfg.patrol = Duration::from_secs(2); // 刷新节拍：迁移后靠刷新帧证「新路径收发继续」
    let island = Island::start(logf(), noop_unhealthy(), cfg).expect("岛可起（含 QUIC 端点）");

    // ---- ① 连上 + 登记 ----
    let outcome: RaceOutcome = cmd(
        &island,
        |reply| Cmd::Connect {
            cands: cands.clone(),
            budget: Duration::from_secs(5),
            reply,
        },
        Duration::from_secs(10),
    )
    .expect("赛跑必须胜出（本地出口的 QUIC 端点）");
    println!("[e2e] race.winner={}", outcome.winner);
    println!("[e2e] race.via={:?}", outcome.via);
    println!("[e2e] race.rtt_ms={}", outcome.rtt_ms);
    println!("[e2e] race.elapsed_ms={}", outcome.elapsed_ms);
    println!("[e2e] race.completed={:?}", outcome.completed);

    // ---- ② 出口侧 `peer: +`（真设备表登记；engine 的判据行）----
    let peer_line = wait_log_from(&exit_log, log0, "peer: +", None, WAIT)
        .expect("出口必须打 `peer: +`（真登记；本轮新增行）");
    println!("[e2e] exit.peer_line={peer_line}");

    // ---- ③ 判活 ----
    let rtt = cmd(
        &island,
        |reply| Cmd::Probe {
            budget: Duration::from_secs(3),
            reply,
        },
        Duration::from_secs(6),
    )
    .expect("已登记连接必须判活");
    println!("[e2e] probe.rtt_ms={}", rtt.as_millis());

    // ---- ④ 迁移：rebind → 出口路径变更 + 岛确认 + 新路径收发继续 ----
    let (alt, how) = alt_bind();
    println!("[e2e] rebind.how={how}");
    println!("[e2e] rebind.target={alt}");
    let to = cmd(
        &island,
        |reply| Cmd::Rebind { local: Some(alt), reply },
        Duration::from_secs(6),
    )
    .expect("rebind 必须成功");
    println!("[e2e] rebind.to={to}");

    let change = wait_log_from(&exit_log, log0, "quic: 路径变更", None, WAIT)
        .expect("出口必须观测到路径变更（E-q2：remote_address() 变化；本轮新增行）");
    println!("[e2e] exit.path_change_line={change}");

    // 迁移完成（保持窗内收到对端回包 ⇒ N-b）；轮询快照
    let deadline = Instant::now() + WAIT;
    while island.snapshot().migrations < 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let snap = island.snapshot();
    println!("[e2e] island.migrations={}", snap.migrations);
    println!("[e2e] island.migration_unconfirmed={}", snap.migration_unconfirmed);
    println!("[e2e] island.via={:?}", snap.via);
    println!("[e2e] island.mtu={:?}", snap.mtu);
    println!("[e2e] island.current_mtu={}", snap.current_mtu);
    assert!(snap.migrations >= 1, "迁移必须确认（收到新路径回包）");
    assert!(!snap.migration_unconfirmed, "不得判未确认");
    assert_eq!(snap.via, Some(Via::Direct), "连接保持（via 未清）");

    // 「收发继续」的客户端 → 出口方向：刷新帧（节拍 2s）必须从**新源**到达出口并被采纳。
    //
    // ⚠️ M2 §1.8 / r14 F11：刷新成功**不重绑、不打 E-q2**（`quic: 连接采纳` 只在首次准入打）
    // ⇒ 服务端侧的到达证据 = 表内 E8 行 `peer: ~ dev=… refresh (idle=…)`（**原串不变**，
    // M1 已登记），且必须落在**路径变更行之后**（本轮新行序 = 时间序）。
    let refresh_line = wait_log_after(&exit_log, log0, "refresh (idle=", "quic: 路径变更", WAIT)
        .expect("刷新帧必须从新源到达出口（client → exit 继续；E8 原串）");
    println!("[e2e] exit.refresh_on_new_path={refresh_line}");
    // 反向断言（r14 F11 的判据）：整轮**只有一条**采纳行（= 首次准入），刷新不再产生
    let run_log = std::fs::read_to_string(&exit_log).unwrap_or_default();
    let adopt_lines: Vec<&str> =
        run_log.lines().skip(log0).filter(|l| l.contains("quic: 连接采纳")).collect();
    assert_eq!(
        adopt_lines.len(),
        1,
        "刷新成功不得再打采纳行（M2 §1.8）：{adopt_lines:?}"
    );
    let _ = to; // 迁移后的本地地址（读数已打印；判据靠路径变更行 + E8 行序）

    // 连接未断（快照仍有路径 + 岛未退出）
    assert!(!island.is_finished(), "岛必须还活着");
    assert!(island.stop_within(Instant::now() + Duration::from_secs(2)), "预算内收工");
    println!("[e2e] island.stopped=true");
}

// ---------------------------------------------------------------------------
// M1 S2b：经中继全链 + 数据面双向
// ---------------------------------------------------------------------------

/// 内层 IPv4 + UDP 包的校验和（S1c 的活教训：留 0 会被出口的 smoltcp 栈**静默丢**）。
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

/// 内层 IPv4 + UDP 包（IPv4 头校验和 + UDP 校验和**真算**）。
fn inner_udp(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 20 + 8 + payload.len()];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
    p[8] = 64; // TTL
    p[9] = 17; // UDP
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
        udp_sum = 0xffff; // RFC 768
    }
    p[26..28].copy_from_slice(&udp_sum.to_be_bytes());
    p
}

/// 写一包进「应用侧」（= 岛的 TUN 读线程会读到）。
fn write_tun(peer: &std::os::unix::net::UnixDatagram, pkt: &[u8]) {
    peer.send(pkt).expect("写 TUN 一包");
}

/// 从「应用侧」读一包（有界；`None` = 上界内没读到）。
fn read_tun(peer: &std::os::unix::net::UnixDatagram, wait: Duration) -> Option<Vec<u8>> {
    peer.set_read_timeout(Some(wait)).ok()?;
    let mut buf = vec![0u8; 4096];
    match peer.recv(&mut buf) {
        Ok(n) => Some(buf[..n].to_vec()),
        Err(_) => None,
    }
}

/// **判据（M1 S2b 的端到端）**：岛客户端经 **Rust 中继全链**（`local-rust-relay.sh`）连上
/// 本地 Rust 出口，并把**真内层流量**双向推过 TUN fd：
///
/// ① `via=Relay{label}`（label = `sha256(peerId)[:8]`）胜出 + 出口 `peer: +`（真设备表）；
/// ② 数据面双向：TUN fd 投内层 UDP（IPv4/UDP 校验和真算）→ 岛 → **中继** → 出口 intercept
///    → transit 到本机 UDP echo → 回程 → 中继 → 岛 → TUN fd（载荷逐字节）；
/// ③ 忽略面：中继推的 hint 控制帧（非 kind=5）被忽略并计数（`rx_ignored`）。
///
/// 环境契约（`tools/quic-island-e2e.sh` 装配）：
/// `HOMEWAY_ISLAND_E2E_TOKEN`（含 relay 类端点的出口 token）/ `HOMEWAY_ISLAND_E2E_EXIT_LOG` /
/// `HOMEWAY_ISLAND_E2E_RELAY`（选填 `ip:port`；缺省取 token 的 Relay 类端点）。
#[test]
#[ignore = "端到端（经中继 + 数据面）：需本地中继与出口在跑（tools/quic-island-e2e.sh 驱动）"]
fn island_uses_relay_and_pushes_traffic_through_tun() {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixDatagram;

    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let log0 = log_lines(&exit_log);
    let tok = token::decode(&token_str).expect("token 可解");
    let rpk = tok.rpk.expect("M1 的 token 必带出口 RPK");

    // 中继候选：`label = sha256(peerId)[:8]`（真源 `legframe::relay_id`）
    let label = homeway_core::legframe::relay_id(tok.peer_id.as_bytes());
    let relay_addr: SocketAddrV4 = match std::env::var("HOMEWAY_ISLAND_E2E_RELAY") {
        Ok(v) => v.parse().expect("HOMEWAY_ISLAND_E2E_RELAY 形如 ip:port"),
        Err(_) => tok
            .endpoints
            .iter()
            .find(|e| e.kind == EndpointKind::Relay)
            .expect("token 必须带中继类端点（出口 `serve --relay <rl1…>` 注册过腿）")
            .addr
            .parse()
            .expect("中继端点地址可解"),
    };

    let id = Identity::ephemeral().expect("临时身份");
    let pubkey = id.public_key();
    let secret = tok.secret.clone();
    // 设备派生地址（真源 `homeway_core::tunnel_addr`）：内层包 src 必须 ∈ {tunnel_ip, tun_ip}
    let tun_ip = homeway_core::tunnel_addr::derive_tun_ip(&secret, &pubkey);
    let mut cfg = IslandConfig::new(IslandCredential::new(
        TokenSecret::from_bytes(*secret.as_bytes()),
        pubkey,
        *id.dev_tag().as_bytes(),
        RpkPublicKey::from_bytes(*rpk.as_bytes()),
    ));
    cfg.bind = Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    cfg.patrol = Duration::from_secs(2);

    // 本机 UDP echo（出口 intercept 的 **transit** 目标：同一台机器 ⇒ 回环可达）
    let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("echo 可绑");
    let echo_addr = match echo.local_addr().expect("echo 地址") {
        std::net::SocketAddr::V4(v) => v,
        std::net::SocketAddr::V6(_) => unreachable!(),
    };
    let echo_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let echo_thread = {
        let stop = std::sync::Arc::clone(&echo_stop);
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

    let island = Island::start(logf(), noop_unhealthy(), cfg).expect("岛可起（含 QUIC 端点）");
    // TUN 面 = 数据报 socketpair（一端当 fd 交给岛，另一端当「应用」）
    let (tun, peer) = UnixDatagram::pair().expect("socketpair(DGRAM)");
    let attached = cmd(
        &island,
        |reply| Cmd::TunAttach {
            fd: tun.as_raw_fd(),
            mtu: 1280,
            reply,
        },
        Duration::from_secs(5),
    );
    assert!(attached.is_ok(), "隧道面必须附加：{attached:?}");

    // ---- ① 赛跑（**只给中继候选** ⇒ 证明信封路径本身可用）----
    let outcome: RaceOutcome = cmd(
        &island,
        |reply| Cmd::Connect {
            cands: vec![Candidate {
                addr: relay_addr,
                via: Via::Relay { label },
            }],
            budget: Duration::from_secs(5),
            reply,
        },
        Duration::from_secs(15),
    )
    .expect("中继候选必须经信封握手成功");
    println!("[e2e2] race.winner={}", outcome.winner);
    println!("[e2e2] race.via={:?}", outcome.via);
    println!("[e2e2] relay.label={}", label.iter().map(|b| format!("{b:02x}")).collect::<String>());
    assert_eq!(outcome.winner, relay_addr, "胜者 = 中继候选");
    assert_eq!(outcome.via, Via::Relay { label }, "via 必须如实带回中继类别");
    let peer_line = wait_log_from(&exit_log, log0, "peer: +", None, WAIT)
        .expect("出口必须打 `peer: +`（真登记；本轮新增行）");
    println!("[e2e2] exit.peer_line={peer_line}");

    // ---- ② 数据面双向（经中继；出口 intercept transit → 回环 echo）----
    let payload = b"hw-m1-s2b-echo";
    let inner = inner_udp(
        tun_ip,
        Ipv4Addr::LOCALHOST,
        40000,
        echo_addr.port(),
        payload,
    );
    write_tun(&peer, &inner);
    let deadline = Instant::now() + WAIT;
    let mut back: Option<Vec<u8>> = None;
    while Instant::now() < deadline {
        if let Some(p) = read_tun(&peer, Duration::from_millis(200)) {
            // 回程 = 出口 intercept 回投的内层包（IPv4+UDP+载荷；载荷 = 同一 payload）
            if p.len() >= 28 + payload.len() && p[28..].starts_with(payload) {
                back = Some(p);
                break;
            }
        }
    }
    let back = back.expect("回程必须经中继回到 TUN fd（内层载荷逐字节）");
    println!(
        "[e2e2] tun.uplink={}B tun.downlink={}B payload={:?}",
        inner.len(),
        back.len(),
        String::from_utf8_lossy(&back[28..])
    );
    println!(
        "[e2e2] exit.transit_line={}",
        wait_log_from(&exit_log, log0, "transit", None, Duration::from_secs(3))
            .unwrap_or_else(|| "（无 transit 行；以回程载荷为准）".into())
    );

    // ---- ③ 读数（快照面）----
    let snap = island.snapshot();
    println!("[e2e2] island.via={:?}", snap.via);
    println!("[e2e2] island.packets_in={}", snap.packets_in);
    println!("[e2e2] island.packets_out={}", snap.packets_out);
    println!("[e2e2] island.relay_tx={}", snap.relay_tx);
    println!("[e2e2] island.rx_ignored={}", snap.rx_ignored);
    println!("[e2e2] island.drops={:?}", snap.drops);
    println!("[e2e2] island.mtu={:?} current_mtu={}", snap.mtu, snap.current_mtu);
    assert!(snap.packets_out >= 1, "回程包计数（packets_out）必须真计");
    assert!(snap.relay_tx >= 1, "中继上行包封计数必须 > 0");
    assert!(
        snap.rx_ignored >= 1,
        "中继推的 hint 控制帧（非 kind=5）必须被忽略并计数"
    );
    assert_eq!(snap.drops.too_large, 0, "本用例不注入超限");

    assert!(island.stop_within(Instant::now() + Duration::from_secs(2)), "预算内收工");
    echo_stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = echo_thread.join();
    println!("[e2e2] island.stopped=true");
}

// ---------------------------------------------------------------------------
// M1 S3-1：**世代级**接线（真产品路径）——TUN 流量经 QUIC DATAGRAM
// ---------------------------------------------------------------------------

/// **判据（S3-1 的核心：M1 的「TUN 流量经 QUIC DATAGRAM」在真产品路径上成立）**：
/// 走 `ClientCore::tun_prepare(transport=quic)` + `tun_attach(fd)` 的**世代装配**，
/// 断言 ①N-d 行声明 quic 档 ②C2'（`quic: 隧道侧就绪…`）在场 ③状态 JSON 的 `quic` 段
/// 在场（via/mtu/丢弃四类）④TUN fd 投真内层 UDP ⇒ 经 EXIT 的 intercept transit 回环
/// 回程（载荷逐字节）⑤无「回落 WG」行（岛真在用）。
#[test]
#[ignore = "端到端（世代级 L3 over QUIC）：需本地 QUIC 出口在跑（tools/quic-island-e2e.sh 驱动）"]
fn generation_l3_rides_quic_datagram_against_local_exit() {
    use homeway_core::facade::demand::DemandSignals;
    use homeway_core::facade::tun_exec::TunnelExec;
    use homeway_core::facade::ClientCore;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::net::UnixDatagram;

    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let log0 = log_lines(&exit_log);
    let _tok = token::decode(&token_str).expect("token 可解（世代层自解析，这里只证可解）");
    // 状态面读一次 JSON（`tunIp` 的源；见下方 L3 段的说明）
    let snap_json = |core: &std::sync::Arc<homeway_core::facade::ClientCore>| {
        serde_json::from_str::<serde_json::Value>(&core.tun_status()).expect("tun_status 是 JSON")
    };

    let dir = std::env::temp_dir().join(format!("hw-m1s3-gen-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{token_str}","out":"{}","identityDir":"{}","transport":"quic"}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = std::sync::Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(std::sync::Arc::clone(&demand));
    let core = std::sync::Arc::new(ClientCore::with_shared(exec, demand));
    assert_eq!(core.tun_prepare(&cfg, true), 0, "prepare 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"ready\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let st = core.tun_status();
    println!("[e2e3] tun.status.ready={}", st.contains("\"state\":\"ready\""));
    assert!(st.contains("\"state\":\"ready\""), "世代须 ready：{st}");

    // ① N-d（承载开关行）**已删**（M5 C3 单承载）+ ② C2'（L3 承载面就绪）
    let log = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(
        !log.contains("transport: 本世代 L3 承载 ="),
        "A/B 开关行（N-d）必须已删除：{log}"
    );
    assert!(
        !log.contains("回落 WG") && !log.contains("尝试 WG 兜底"),
        "单承载后不得有回落/兜底话术：{log}"
    );
    let c2 = log
        .lines()
        .find(|l| l.contains("quic: 隧道侧就绪（L3 直通；"))
        .unwrap_or("（缺）")
        .to_owned();
    println!("[e2e3] C2'={c2}");
    assert!(
        !c2.contains("经 WG"),
        "C2' 末半句（核心自连经 WG）随 WG 面删除：{c2}"
    );
    assert!(c2.contains("后端隧道 IP"), "C2' 字段面保留：{c2}");
    assert!(
        log.contains("warmup pong: 就绪（判据=quic）"),
        "暖机判据位须为 quic（C8 值域扩展）"
    );
    assert!(!log.contains("回落 WG"), "本用例不得回落（单承载）：{log}");
    assert!(!log.contains("quic: 岛未就用"), "岛必须起来：{log}");

    // ③ 状态 JSON 的 quic 段
    let v: serde_json::Value = serde_json::from_str(&st).expect("tun_status 是 JSON");
    let q = v.get("quic").expect("quic 段须在场");
    println!("[e2e3] quic.via={} mtu={} current_mtu={} candidates={}",
        q["via"], q["mtu"], q["current_mtu"], q["candidates"]);
    assert_eq!(q["drops"]["too_large"], 0, "本用例不注入超限");
    assert!(q["mtu"].as_u64().unwrap_or(0) >= 1280, "mds 须 ≥ 内层 MTU：{q}");
    assert!(q["connections"].as_u64().unwrap_or(0) >= 1, "岛须持有连接：{q}");
    assert!(q["via"] == "direct" || q["via"] == "relay", "via 词表：{q}");

    // ④ L3（QUIC DATAGRAM）：TUN fd 投内层 UDP → 出口 transit → 回环 echo → 回程
    let echo = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("echo 可绑");
    let echo_addr = match echo.local_addr().expect("echo 地址") {
        std::net::SocketAddr::V4(v) => v,
        std::net::SocketAddr::V6(_) => unreachable!(),
    };
    let echo_stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let echo_thread = {
        let stop = std::sync::Arc::clone(&echo_stop);
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
    let (tun, peer) = UnixDatagram::pair().expect("socketpair(DGRAM)");
    assert_eq!(
        core.tun_attach(tun.as_raw_fd(), 1280),
        0,
        "attach 受理（fd 交岛）"
    );
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"attached\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    let payload = b"hw-m1-s3-quic";
    // src = 状态面公布的**本设备派生地址**（出口 `src_allowed` 判据面；不能拿 token 的
    // peerId 自己推——那是后端公钥）
    let tun_ip: Ipv4Addr = snap_json(&core)["tunIp"]
        .as_str()
        .expect("状态面须公布 tunIp")
        .parse()
        .expect("tunIp 是 IPv4");
    println!("[e2e3] tun_ip={tun_ip}");
    let inner = inner_udp(tun_ip, Ipv4Addr::LOCALHOST, 40002, echo_addr.port(), payload);
    write_tun(&peer, &inner);
    let deadline = Instant::now() + WAIT;
    let mut back: Option<Vec<u8>> = None;
    while Instant::now() < deadline {
        if let Some(p) = read_tun(&peer, Duration::from_millis(200)) {
            if p.len() >= 28 + payload.len() && p[28..].starts_with(payload) {
                back = Some(p);
                break;
            }
        }
    }
    let back = back.expect("L3 回程必须经 QUIC DATAGRAM 回到 TUN fd（载荷逐字节）");
    println!(
        "[e2e3] tun.uplink={}B tun.downlink={}B payload={:?}",
        inner.len(),
        back.len(),
        String::from_utf8_lossy(&back[28..])
    );
    let transit = wait_log_from(&exit_log, log0, "udp intercept:", Some("transit 建立"), WAIT)
        .unwrap_or_else(|| "（无 transit 行）".into());
    println!("[e2e3] exit.transit_line={transit}");
    assert!(transit.contains("transit 建立"), "出口须建 transit 会话：{transit}");

    // 快照读数（留证）
    let snap2: serde_json::Value =
        serde_json::from_str(&core.tun_status()).expect("tun_status 是 JSON");
    println!(
        "[e2e3] quic.packets_in={} packets_out={} local={} relay_tx={} rx_ignored={}",
        snap2["quic"]["packets_in"],
        snap2["quic"]["packets_out"],
        snap2["quic"]["local"],
        snap2["quic"]["relay_tx"],
        snap2["quic"]["rx_ignored"]
    );
    println!("[e2e3] link={}", snap2["link"]);
    let _ = core.tun_stop();
    echo_stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = echo_thread.join();
    println!("[e2e3] done");
}


// ---------------------------------------------------------------------------
// M3 S2：出口服务流受理（真出口实例上的 tag 分发 → intake → 泵 → 真服务）
// ---------------------------------------------------------------------------

/// 建一枚已连上本地出口的岛（S2a 的连接段的抽取版；只有本用例用它）。
fn connect_local_exit() -> (Island, Vec<u8>) {
    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let tok = token::decode(&token_str).expect("token 可解");
    let rpk = tok.rpk.expect("M1 的 token 必带出口 RPK");
    let cands: Vec<Candidate> = tok
        .endpoints
        .iter()
        .filter(|e| e.kind == EndpointKind::Quic)
        .map(|e| {
            let raw: SocketAddrV4 = e.addr.parse().expect("QUIC 端点地址可解");
            Candidate {
                addr: SocketAddrV4::new(Ipv4Addr::LOCALHOST, raw.port()),
                via: Via::Direct,
            }
        })
        .collect();
    assert!(!cands.is_empty(), "token 必须带 QUIC 类端点");
    let id = Identity::ephemeral().expect("临时身份");
    let cred = IslandCredential::new(
        TokenSecret::from_bytes(*tok.secret.as_bytes()),
        id.public_key(),
        *id.dev_tag().as_bytes(),
        RpkPublicKey::from_bytes(*rpk.as_bytes()),
    );
    let mut cfg = IslandConfig::new(cred);
    cfg.bind = Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    let island = Island::start(logf(), noop_unhealthy(), cfg).expect("岛可起");
    let outcome: RaceOutcome = cmd(
        &island,
        |reply| Cmd::Connect {
            cands,
            budget: Duration::from_secs(5),
            reply,
        },
        Duration::from_secs(10),
    )
    .expect("赛跑必须胜出（本地出口的 QUIC 端点）");
    println!("[e2e4] race.winner={}", outcome.winner);
    let dev = *id.dev_tag().as_bytes();
    (island, dev.to_vec())
}

/// 发一条**流族**命令并**有界**取回（流族回执口是 `StreamReply<T>` = `Result<T, StreamErr>`，
/// 与普通 `IslandReply` 不同型 ⇒ 独立小件；超时/岛死归 `StreamErr::Timeout`/`ConnectionLost`）。
fn cmd_stream<T>(
    island: &Island,
    make: impl FnOnce(StreamReply<T>) -> Cmd,
    wait: Duration,
) -> Result<T, StreamErr> {
    let (tx, rx) = mpsc::channel();
    island
        .tx()
        .send(make(tx))
        .map_err(|_| StreamErr::ConnectionLost)?;
    match rx.recv_timeout(wait) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(e),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(StreamErr::Timeout),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(StreamErr::ConnectionLost),
    }
}

/// 读服务流直到累积出完整一行（`\n` 结尾）；返回行（含 `\n`）。
fn read_stream_line(island: &Island, id: StreamId, wait: Duration) -> Vec<u8> {
    let deadline = Instant::now() + wait;
    let mut acc: Vec<u8> = Vec::new();
    while Instant::now() < deadline {
        let left = deadline.saturating_duration_since(Instant::now());
        match cmd_stream(
            island,
            |reply| Cmd::StreamRead { id, reply },
            left.max(Duration::from_millis(50)),
        ) {
            Ok(chunk) => {
                acc.extend_from_slice(&chunk);
                if acc.contains(&b'\n') {
                    return acc;
                }
            }
            Err(e) => panic!("服务流读失败/超时：{e}（已收 {}B）", acc.len()),
        }
    }
    panic!("服务流未在 {wait:?} 内给出完整行（已收 {}B）", acc.len());
}

/// **M3 S2 判据（真实出口实例上的服务流全链）**：岛经 QUIC 开 `STREAM[tag=files]` ⇒
/// 出口分发（每流 task）→ socketpair 泵 → **真 FilesServer** ⇒ 问候行/应答行逐字节回到
/// 客户端；客户端半关（`StreamShutdown`）⇒ 服务侧收线 ⇒ 客户端读得 EOF
/// （`StreamErr::Closed`，= 今天的 EOF 语义）；出口日志新增 E-q5 行（受理/结束）。
///
/// 这条用例是 S3 换轨前的**出口侧真实入口**验收（客户端侧的服务流缝 S3 才接；本用例直接
/// 驱动岛的命令面 `Cmd::StreamOpen/Write/Read/Shutdown`——协议面与 S3 将走的完全同一条）。
#[test]
#[ignore = "端到端：需本地出口在跑（tools/quic-island-e2e.sh 驱动）"]
fn service_stream_files_over_quic_against_local_exit() {
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let log0 = log_lines(&exit_log);
    let (island, dev) = connect_local_exit();

    // ① 开 STREAM[tag=files]（tag 由岛写——协议面单源）
    let id = cmd_stream(
        &island,
        |reply| Cmd::StreamOpen {
            tag: StreamTag::Files,
            reply,
        },
        WAIT,
    )
    .expect("开 tag=files 流");
    println!("[e2e4] stream.id={id}");
    // ② 问候行（服务端先发；逐字节）
    let greet = read_stream_line(&island, id, WAIT);
    let g: serde_json::Value = serde_json::from_slice(&greet).expect("问候行是 JSON");
    assert_eq!(g["ok"], serde_json::json!(true), "问候：{g}");
    assert_eq!(g["ver"], serde_json::json!(1), "files 协议版本：{g}");
    assert!(
        g["root"].as_str().is_some_and(|r| !r.is_empty()),
        "问候须带 root：{g}"
    );
    println!("[e2e4] files.greeting={}", String::from_utf8_lossy(&greet).trim_end());

    // ③ 一条 list 请求 ⇒ 应答行（同一流上；真服务在泵的另一端）
    let req = br#"{"op":"list","path":""}"#;
    let mut req_line = req.to_vec();
    req_line.push(b'\n');
    let mut sent = 0usize;
    while sent < req_line.len() {
        let out = cmd_stream(
            &island,
            |reply| Cmd::StreamWrite {
                id,
                data: req_line[sent..].to_vec(),
                reply,
            },
            WAIT,
        )
        .expect("写请求行");
        assert!(out.n > 0, "背压回执 n=0 不该在空闲流上出现");
        sent += out.n;
    }
    let resp = read_stream_line(&island, id, WAIT);
    let r: serde_json::Value = serde_json::from_slice(&resp).expect("应答行是 JSON");
    assert_eq!(r["ok"], serde_json::json!(true), "list 应答：{r}");
    assert!(r["entries"].is_array(), "list 应答须带 entries：{r}");
    println!(
        "[e2e4] files.list_entries={}",
        r["entries"].as_array().map(|a| a.len()).unwrap_or(0)
    );

    // ④ 半关（客户端 FIN）⇒ 服务侧 read_line 收线 ⇒ 我方读到 EOF
    cmd_stream(&island, |reply| Cmd::StreamShutdown { id, reply }, WAIT).expect("半关写半边");
    let eof = cmd_stream(
        &island,
        |reply| Cmd::StreamRead { id, reply },
        Duration::from_secs(10),
    );
    assert!(
        matches!(eof, Err(StreamErr::Closed)),
        "半关后应收 EOF（StreamErr::Closed），实得 {eof:?}"
    );
    cmd_stream(&island, |reply| Cmd::StreamClose { id, reply }, WAIT).expect("关流");

    // ⑤ 出口侧 E-q5 行（受理 + 结束）与本设备短指纹
    //
    // **节流口径（M3 S4 订正，与 `[e2e5]` 同款）**：出口的 E-q5 受理行是节流面
    // （**首 3 + 每 100**，且计数跨 tag 共享——probe 也占位）。S4 起岛在 TUN 在位时
    // 跑快探阶梯（§3.1/§3.2），每连接多开 1 条 probe 流 ⇒ 本用例的 files 流受理行
    // 常落在节流窗之外。**主判据 = 字节路径（上面的问候/应答）+ 岛侧在册计数（⑥）**，
    // 日志面取「受理**或**结束**至少一条**在场」。
    let dev4 = hex4(&dev);
    let accepted = wait_log_from(&exit_log, log0, "服务流已受理（tag=files", Some(&dev4), WAIT);
    let closed = wait_log_from(&exit_log, log0, "服务流结束（tag=files", None, WAIT);
    assert!(
        accepted.is_some() || closed.is_some(),
        "出口须有本流的受理/结束行（节流窗内两条都被压时也应至少一条在场）"
    );
    println!(
        "[e2e4] exit.accepted_line={}",
        accepted.clone().unwrap_or_else(|| "（节流窗内被压）".into())
    );
    if let Some(c) = &closed {
        assert!(c.contains("↑"), "结束行须带逐向字节：{c}");
    }
    println!(
        "[e2e4] exit.closed_line={}",
        closed.unwrap_or_else(|| "（节流窗内被压）".into())
    );

    // ⑥ 关流后的岛快照（服务流计数面；`Island::snapshot` 是同步轮询口）
    let snap: homeway_quic::IslandSnapshot = island.snapshot();
    println!(
        "[e2e4] island.streams_open={} streams_refused={} bytes_in={} bytes_out={}",
        snap.streams_open, snap.streams_refused, snap.stream_bytes_in, snap.stream_bytes_out
    );
    assert!(snap.streams_open >= 1, "岛侧在册服务流计数：{snap:?}");
    println!("[e2e4] done");
}

/// 8B devTag 的前 4B hex（与出口行 `dev=%s` 同款——`bridge::dev_short` 同口径）。
fn hex4(dev: &[u8]) -> String {
    dev.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// M3 S3：**客户端换轨**——App 核的服务流经 STREAM（隧道桥 DialFn 换实现）
// ---------------------------------------------------------------------------

/// 桥 UDS 上读一行（有界：到上界未出 `\n` ⇒ 空串——用例据此判失败而不挂死）。
fn read_bridge_line(c: &mut std::os::unix::net::UnixStream, wait: Duration) -> Vec<u8> {
    use std::io::Read as _;
    c.set_read_timeout(Some(wait)).ok();
    let mut acc: Vec<u8> = Vec::new();
    let mut buf = [0u8; 4096];
    while !acc.contains(&b'\n') {
        match c.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => acc.extend_from_slice(&buf[..n]),
            Err(_) => break, // 读超时/连接断：交调用方按「已收 N B」判
        }
    }
    acc
}

/// **M3 S3 判据（服务流经 STREAM 的 App 核端到端）**：`transport=quic` 的**真世代**
/// （`TunnelExec` prepare → `tun_attach` → 隧道桥起）里，App 形态的 files 请求
/// （`<identityDir>/bridge/files.sock` + 鉴权首包 + `{"op":"list"}` 行）**经 QUIC STREAM
/// 端到端**走通：
/// ①桥问候行/应答行逐字节（files 协议零改动）；②出口新增 `quic: 服务流已受理（tag=files`
/// （E-q5）；③核日志有岛侧 `quic: 服务流已开（tag=files`；④**负判据**——本轮出口日志
/// **不再出现** WG 服务腿的 `intercept: tcp exempt …:7802`（换轨的事实面：服务流没走栈 B）；
/// ⑤世代未回落 WG（`quic: 隧道侧就绪（L3 直通；` 在场、`本世代回落 WG 承载` 不在）。
#[test]
#[ignore = "端到端（App 核服务流 over QUIC）：需本地 QUIC 出口在跑（tools/quic-island-e2e.sh 驱动）"]
fn service_stream_rides_quic_through_app_core_bridge_against_local_exit() {
    use homeway_core::facade::bridge_host::bridge_client_auth;
    use homeway_core::facade::demand::DemandSignals;
    use homeway_core::facade::tun_exec::TunnelExec;
    use homeway_core::facade::ClientCore;
    use std::io::Write as _;
    use std::os::fd::AsRawFd as _;
    use std::os::unix::net::{UnixDatagram, UnixStream};

    let token_str = std::env::var("HOMEWAY_ISLAND_E2E_TOKEN").expect("须给 HOMEWAY_ISLAND_E2E_TOKEN");
    let exit_log = PathBuf::from(
        std::env::var("HOMEWAY_ISLAND_E2E_EXIT_LOG").expect("须给 HOMEWAY_ISLAND_E2E_EXIT_LOG"),
    );
    let log0 = log_lines(&exit_log);

    let dir = std::env::temp_dir().join(format!("hw-m3s3-bridge-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("临时目录可建");
    let out = dir.join("gen.log");
    let ident_dir = dir.join("identity");
    let _ = std::fs::remove_file(&out);
    let cfg = format!(
        r#"{{"token":"{token_str}","out":"{}","identityDir":"{}","transport":"quic"}}"#,
        out.display(),
        ident_dir.display()
    );
    let demand = std::sync::Arc::new(DemandSignals::new());
    let exec = TunnelExec::new(std::sync::Arc::clone(&demand));
    let core = std::sync::Arc::new(ClientCore::with_shared(exec, demand));
    assert_eq!(core.tun_prepare(&cfg, true), 0, "prepare 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"ready\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.tun_status().contains("\"state\":\"ready\""), "世代须 ready");

    let (tun, _peer) = UnixDatagram::pair().expect("socketpair(DGRAM)");
    assert_eq!(core.tun_attach(tun.as_raw_fd(), 1280), 0, "attach 受理");
    let deadline = Instant::now() + WAIT;
    while !core.tun_status().contains("\"state\":\"attached\"") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(core.tun_status().contains("\"state\":\"attached\""), "世代须 attached（桥随之起）");

    // 世代日志：QUIC 档在场（判据 ⑤ 的前半）
    let log = std::fs::read_to_string(&out).unwrap_or_default();
    assert!(log.contains("quic: 隧道侧就绪（L3 直通；"), "QUIC 档须在场：{log}");
    assert!(!log.contains("回落 WG"), "单承载后不得有回落话术：{log}");

    // ① 桥面（状态 JSON 的 bridgeAuth/bridgeFilesSock）
    let v: serde_json::Value = serde_json::from_str(&core.tun_status()).expect("tun_status 是 JSON");
    let auth = v["bridgeAuth"].as_str().unwrap_or_default().to_owned();
    let files_sock = v["bridgeFilesSock"].as_str().unwrap_or_default().to_owned();
    assert!(!auth.is_empty() && !files_sock.is_empty(), "桥须在场：{v}");
    // M3 S5 的正向面：本世代准入成功 ⇒ 快照归因两字段保持空（0/空串）
    let q = v.get("quic").expect("quic 段须在场（本世代 quic 档）");
    assert_eq!(q["admit_reject_code"], 0, "准入成功不得留准入码：{q}");
    assert_eq!(q["admit_reject_text"], "", "准入成功不得留短语：{q}");
    println!("[e2e5] bridge.files_sock={files_sock}");

    let mut c = UnixStream::connect(&files_sock).expect("连 files 桥");
    bridge_client_auth(&mut c, &auth).expect("桥鉴权首包可写");
    let greet = read_bridge_line(&mut c, WAIT);
    assert!(greet.ends_with(b"\n"), "问候行须完整收到（App 核服务流经 STREAM）");
    let g: serde_json::Value = serde_json::from_slice(&greet).expect("问候行是 JSON");
    assert_eq!(g["ok"], serde_json::json!(true), "问候：{g}");
    assert_eq!(g["ver"], serde_json::json!(1), "files 协议版本：{g}");
    println!("[e2e5] bridge.greeting={}", String::from_utf8_lossy(&greet).trim_end());

    let mut req = br#"{"op":"list","path":""}"#.to_vec();
    req.push(b'\n');
    c.write_all(&req).expect("写 list 请求行");
    let resp = read_bridge_line(&mut c, WAIT);
    let r: serde_json::Value = serde_json::from_slice(&resp).expect("应答行是 JSON");
    assert_eq!(r["ok"], serde_json::json!(true), "list 应答：{r}");
    assert!(r["entries"].is_array(), "list 应答须带 entries：{r}");
    println!(
        "[e2e5] bridge.list_entries={}",
        r["entries"].as_array().map(|a| a.len()).unwrap_or(0)
    );
    drop(c);

    // ② 出口 E-q5 行（真出口上的 tag 分发）。**节流面**（首 3 + 每 100，同一出口实例
    // 被本脚本的多个用例共享）⇒ 受理/结束**至少一条**在场即可（两条都被压时打印说明，
    // 真正的判据是上面的字节路径 + 下面的负判据——它们不依赖日志节流）。
    let accepted = wait_log_from(&exit_log, log0, "服务流已受理（tag=files", None, WAIT);
    let closed = wait_log_from(&exit_log, log0, "服务流结束（tag=files", Some("↑"), WAIT);
    assert!(
        accepted.is_some() || closed.is_some(),
        "出口须有本流的受理/结束行（节流窗内两条都被压时也应至少一条在场）"
    );
    println!(
        "[e2e5] exit.accepted_line={}",
        accepted.clone().unwrap_or_else(|| "（节流窗内被压）".into())
    );
    println!(
        "[e2e5] exit.closed_line={}",
        closed.clone().unwrap_or_else(|| "（节流窗内被压）".into())
    );

    // ③ 核日志的岛侧开流行
    let opened = std::fs::read_to_string(&out)
        .unwrap_or_default()
        .lines()
        .find(|l| l.contains("服务流已开（tag=files"))
        .unwrap_or("（无开流行）")
        .to_owned();
    assert!(opened.contains("tag=files"), "核须记岛侧开流行：{opened}");
    println!("[e2e5] core.opened_line={opened}");

    // ④ 负判据：本轮出口日志**不得**出现 WG 服务腿的 exempt 行（换轨的事实面）
    let new_lines: Vec<String> = std::fs::read_to_string(&exit_log)
        .unwrap_or_default()
        .lines()
        .skip(log0)
        .map(str::to_owned)
        .collect();
    let exempt: Vec<&String> = new_lines
        .iter()
        .filter(|l| l.contains("intercept: tcp exempt") && l.contains(":7802"))
        .collect();
    assert!(
        exempt.is_empty(),
        "QUIC 档服务流不得再经 WG 服务腿（exempt 行零命中），实得：{exempt:?}"
    );
    println!("[e2e5] exit.exempt_lines=0（换轨负判据）");

    let _ = core.tun_stop();
    println!("[e2e5] done");
}
