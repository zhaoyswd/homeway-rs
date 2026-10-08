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
//! （保持窗内收到对端回包 ⇒ 迁移完成）+ 出口再收一帧刷新（`quic: 连接采纳` ← **新源**）
//! 证「收发继续」。

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use homeway_core::identity::Identity;
use homeway_core::token::{self, EndpointKind};
use homeway_quic::{
    Candidate, Cmd, Island, IslandConfig, IslandCredential, IslandReply, RaceOutcome, RpkPublicKey,
    TokenSecret, Via,
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

/// 有界等待日志文件出现某行（返回命中的行）。
fn wait_log(path: &PathBuf, needle: &str, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(l) = s.lines().find(|l| l.contains(needle)) {
                return Some(l.to_owned());
            }
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

/// 有界等待日志文件里出现 `needle`（且可要求同时含 `also`）。
fn wait_log_both(path: &PathBuf, needle: &str, also: &str, wait: Duration) -> Option<String> {
    let deadline = Instant::now() + wait;
    while Instant::now() < deadline {
        if let Ok(s) = std::fs::read_to_string(path) {
            if let Some(l) = s.lines().find(|l| l.contains(needle) && l.contains(also)) {
                return Some(l.to_owned());
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
    let peer_line = wait_log(&exit_log, "peer: +", WAIT).expect("出口必须打 `peer: +`（真登记）");
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

    let change = wait_log(&exit_log, "quic: 路径变更", WAIT)
        .expect("出口必须观测到路径变更（E-q2：remote_address() 变化）");
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

    // 「收发继续」的客户端 → 出口方向：刷新帧（节拍 2s）必须从**新源**到达出口
    // ⇒ 出口打 `quic: 连接采纳 … ← <新源>`（bind 行带当前 remote_address）
    let new_ip = to.ip().to_string();
    let adopt_line = wait_log_both(&exit_log, "quic: 连接采纳", &new_ip, WAIT)
        .expect("刷新帧必须从新源到达出口（client → exit 继续）");
    println!("[e2e] exit.adopt_on_new_path={adopt_line}");

    // 连接未断（快照仍有路径 + 岛未退出）
    assert!(!island.is_finished(), "岛必须还活着");
    assert!(island.stop_within(Instant::now() + Duration::from_secs(2)), "预算内收工");
    println!("[e2e] island.stopped=true");
}
