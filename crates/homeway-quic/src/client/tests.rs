//! 客户端连接面的单测（**真线程岛 + 真回环 IO + 真出口面**；不钉固定端口、时间断言只判
//! 上界——M0 设计 §9.2 flake 口径①②）。
//!
//! 本文件在异步面白名单内（`client/**`）⇒ 可以用 quinn/tokio 名字组装「测试用出口」与
//! 「引擎桩」（裁决语义真源 = `homeway-core` 的 `admit_reg3`：MAC + **本连接** exporter）。
//!
//! 覆盖的判据（设计 §10 的 S2-1/S2-2/S2-3/S2-6）：
//! - 公面成员的行为面（命令→回执/快照/行文）；
//! - **赛跑**：三候选（一活两死）⇒ 胜者/清单/`via` 正确；全死 ⇒ `NoCandidate`；
//!   错 RPK 钉定 ⇒ 握手中止（失败归 `NoCandidate`）；
//! - **登记 + 刷新**：登记成功后出口侧引擎桩收帧（真出口 `peer: +` 的等价面）；
//!   刷新按节拍（`RefreshTimer` 的 60s 虚拟时钟 + 集成面短节拍）；
//! - **迁移**：`rebind` 后 ①连接未断 ②出口 `remote_address()` 变化（E-q2 行）
//!   ③收发继续（出口发回程 ⇒ 岛侧入站证据 ⇒ N-b 行）；失败判据 = `migration_unconfirmed`；
//! - 丢弃四类计数 + 事件回调 + N-c 行（`DatagramDropped`）。

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::time::Instant as TokioInstant;

use crate::cmd::{
    Candidate, Cmd, DropReason, IslandErr, IslandEvent, IslandReply, IslandSnapshot, Logf, Via,
};
use crate::config::{IslandConfig, IslandCredential, TokenSecret};
use crate::exit::{ExitInbound, ExitQuic, ExitQuicConfig, Reg3Verdict};
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::{Island, IslandTx};

use super::register::{self, RefreshTimer};

/// 收工预算（与 `wgcore::CLIENT_CLOSE_BUDGET` 同量级 = 2s）。
const BUDGET: Duration = Duration::from_secs(2);
/// 异步等待上界（flake 口径②：只判上界）。
const WAIT: Duration = Duration::from_secs(8);

/// 测试用 token secret / 设备身份（「引擎桩」与岛共用）。
const SECRET: [u8; 32] = [0x5A; 32];
const PUBKEY: [u8; 32] = [0x11; 32];
const DEV: [u8; 8] = [0x22; 8];
/// 设备派生地址（桩给定；真实值 = `homeway-core` 的 `tunnel_addr` 派生）。
const TUNNEL_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 7, 1);
const TUN_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 7, 2);

fn sink() -> (Logf, Receiver<String>) {
    let (tx, rx) = channel();
    (
        Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }),
        rx,
    )
}

/// 岛侧测试入口（绑定钉成回环 `127.0.0.1:0` —— 迁移用例要从 `127.0.0.1` 起跑到 `127.0.0.2`）。
fn island_with(patrol: Duration, pin: RpkPublicKey) -> Island {
    let (logf, _rx) = sink();
    island_with_log(patrol, pin, logf)
}

fn island_with_log(patrol: Duration, pin: RpkPublicKey, logf: Logf) -> Island {
    let mut cfg = IslandConfig::new(IslandCredential::new(
        TokenSecret::from_bytes(SECRET),
        PUBKEY,
        DEV,
        pin,
    ));
    cfg.bind = Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0));
    cfg.patrol = patrol;
    Island::start(logf, Arc::new(|_r: &str| {}), cfg).expect("岛可起（含 QUIC 端点）")
}

/// 测试用出口（**真出口面**：`ExitQuic` 自身；引擎侧由 [`Stub`] 扮演）。
/// 绑回环：迁移用例的客户端在 `127.0.0.1` ↔ `127.0.0.2` 间换 IP（回环别名全段本地可达）。
fn exit_face(seed: u8) -> ExitQuic {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("出口 socket 可绑");
    let (logf, _rx) = sink();
    ExitQuic::start(sock, ExitQuicConfig::new(Ed25519Seed::from_bytes([seed; 32]), 32), logf)
        .expect("出口 QUIC 面可起")
}

/// 出口的可连地址（`local_addr` 是**已绑定**地址；本测试全走回环 ⇒ 取端口 + 127.0.0.1）。
fn connectable(quic: &ExitQuic) -> SocketAddrV4 {
    let port = match quic.local_addr() {
        SocketAddr::V4(v) => v.port(),
        SocketAddr::V6(_) => unreachable!(),
    };
    SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)
}

/// 内层 IPv4 包（出站投递用；形态同 `homeway-core` 的 `server/device.rs` 测试构造）。
fn inner_pkt(src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
    let mut p = vec![0u8; 28];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&28u16.to_be_bytes());
    p[12..16].copy_from_slice(&src.octets());
    p[16..20].copy_from_slice(&dst.octets());
    p
}

/// 测试侧「引擎桩」：只经**真接口**（`drain_inbound`）拿事件，裁决语义 = 核心侧
/// `admit_reg3` 的 MAC 判定（secret + 本连接 exporter）。
struct Stub {
    accepted: AtomicU64,
    rejected: AtomicU64,
    packets: Mutex<Vec<Vec<u8>>>,
}

impl Stub {
    fn new() -> Arc<Stub> {
        Arc::new(Stub {
            accepted: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            packets: Mutex::new(Vec::new()),
        })
    }

    fn pump(&self, quic: &ExitQuic) {
        quic.drain_inbound(|item| match item {
            ExitInbound::Reg(req) => {
                if req.frame.mac_matches(&SECRET, &req.exporter) {
                    self.accepted.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg3Verdict::Accepted {
                        tunnel_ip: TUNNEL_IP,
                        tun_ip: TUN_IP,
                    });
                } else {
                    self.rejected.fetch_add(1, Ordering::SeqCst);
                    req.reply(Reg3Verdict::Rejected);
                }
            }
            ExitInbound::Packet { pkt, .. } => self.packets.lock().unwrap().push(pkt),
        });
    }

    fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::SeqCst)
    }

    fn rejected(&self) -> u64 {
        self.rejected.load(Ordering::SeqCst)
    }
}

/// 有界轮询（每轮先 pump；只判上界）。
async fn wait_for(
    stub: &Stub,
    quic: &ExitQuic,
    mut cond: impl FnMut() -> bool,
    wait: Duration,
) -> bool {
    let deadline = Instant::now() + wait;
    loop {
        stub.pump(quic);
        if cond() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 投一条带 reply 的命令并**异步有界**取回（不可用阻塞 `recv_timeout`：会卡住测试 runtime，
/// 出口面就没人 pump 了——准入裁决要桩先回执）。
async fn send_wait<T>(
    island: &Island,
    stub: &Stub,
    quic: &ExitQuic,
    make: impl FnOnce(IslandReply<T>) -> Cmd,
    wait: Duration,
) -> Result<T, IslandErr> {
    let (tx, rx) = channel();
    island.tx().send(make(tx)).expect("命令投递（unbounded）");
    let deadline = Instant::now() + wait;
    loop {
        stub.pump(quic);
        match rx.try_recv() {
            Ok(v) => return v,
            Err(TryRecvError::Disconnected) => return Err(IslandErr::EngineGone),
            Err(TryRecvError::Empty) => {}
        }
        assert!(Instant::now() < deadline, "命令回执超时（岛未应答 ⇒ 挂死）");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// 收日志行（非阻塞轮询版 `drain_until`）。
async fn logs_until(rx: &Receiver<String>, needle: &str, wait: Duration) -> Vec<String> {
    let deadline = Instant::now() + wait;
    let mut lines = Vec::new();
    while Instant::now() < deadline {
        while let Ok(l) = rx.try_recv() {
            lines.push(l);
        }
        if lines.iter().any(|l| l.contains(needle)) {
            return lines;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    lines
}

fn direct(addr: SocketAddrV4) -> Candidate {
    Candidate {
        addr,
        via: Via::Direct,
    }
}

/// 本机非回环 IPv4（`connect` 只选路由、不发包；无默认路由 ⇒ None）。
fn lan_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    s.connect((Ipv4Addr::new(192, 0, 2, 1), 9)).ok()?; // TEST-NET-1：仅用于选路由
    match s.local_addr().ok()? {
        SocketAddr::V4(v) if !v.ip().is_loopback() => Some(*v.ip()),
        _ => None,
    }
}

/// 迁移目标地址（「换网卡」的本地等价物；设计 §2.3 的本地注入代理）：
/// ① `127.0.0.2`（Linux 的 lo 别名）能绑就用它；② darwin 的 `lo0` 只有 `127.0.0.1`
/// ⇒ 借本机网卡地址（设计 P7 的同款形态）；③ 都不行退回「同 IP 换端口」（仍走 rebind +
/// 路径验证，但不是「换 IP」分支——用例打印实际口径，不静默降级）。
fn alt_bind_addr() -> (SocketAddrV4, &'static str) {
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

/// 「死候选」= 已绑定但**从不回包**的 UDP socket（握手永不完成；不产生 ICMP 干扰）。
fn dead_candidate() -> (UdpSocket, Candidate) {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("死候选可绑");
    let addr = match s.local_addr().expect("可读地址") {
        SocketAddr::V4(v) => v,
        SocketAddr::V6(_) => unreachable!(),
    };
    (s, direct(addr))
}

// ---------- 1. 端点就绪 + 登记（真出口 + 引擎桩） ----------

/// **判据（S2-6 主干）**：岛连上真出口 ⇒ 首条 bidi 控制流登记 ⇒ 出口侧引擎桩收到合法
/// `hr-reg3`（= 真出口 `peer: +` 的等价面：绑定成功）⇒ N-a/C4'/C5'/C6' 行齐。
#[tokio::test]
async fn connect_registers_on_control_stream_and_logs_criteria_lines() {
    let quic = exit_face(1);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));

    let outcome = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(connectable(&quic))],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("单候选（活）必须胜出");
    assert_eq!(outcome.completed.len(), 1, "完成清单 = 胜者：{outcome:?}");
    assert!(outcome.unfinished.is_empty(), "无未完成候选：{outcome:?}");
    assert_eq!(outcome.via, Via::Direct, "直连候选 ⇒ via=Direct");
    assert!(outcome.rtt_ms < 1000, "回环 RTT 量级（上界）：{}", outcome.rtt_ms);

    assert!(
        wait_for(&stub, &quic, || stub.accepted() >= 1, WAIT).await,
        "出口侧必须收到并采纳登记（MAC + 本连接 exporter）"
    );
    assert_eq!(stub.accepted(), 1, "恰好一次登记");
    assert_eq!(quic.snapshot().regs_accepted, 1, "出口面登记计数 +1");

    let snap = island.snapshot();
    assert_eq!(snap.via, Some(Via::Direct), "快照 via 入位：{snap:?}");
    assert!(snap.ep.is_some(), "快照 ep 入位");
    assert!(snap.mtu.is_some(), "快照 mtu（max_datagram_size）入位");
    assert!(snap.current_mtu >= 1320, "current_mtu ≥ min_mtu：{}", snap.current_mtu);
    assert_eq!(snap.mirrors, 1, "投出的候选数累计");

    let lines = logs_until(&logs, "quic: 登记已发", WAIT).await;
    for needle in [
        "quic: 端点就绪（本地 ",
        "quic: 赛跑投出 1 个候选（直连 1 / 中继 0；本行每轮限 3 条）",
        "quic: 赛跑结算：胜出 直连 ",
        "quic: 路径确立：直连 ",
        "（首个完成握手）",
        "quic: 登记已发",
    ] {
        assert!(
            lines.iter().any(|l| l.contains(needle)),
            "缺判据行「{needle}」：{lines:?}"
        );
    }
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "赛跑登记后必须能预算内收工"
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 2. 赛跑：三候选（一活两死） ----------

/// **判据（S2-2）**：三候选（一活两死）⇒ 胜者正确、`via` 正确、耗时上界、清单正确。
#[tokio::test]
async fn race_picks_first_completer_among_dead_candidates() {
    let quic = exit_face(2);
    let stub = Stub::new();
    let (_dead_a, cand_a) = dead_candidate();
    let (_dead_b, cand_b) = dead_candidate();
    let alive_addr = connectable(&quic);
    assert_ne!(cand_a.addr, alive_addr);
    assert_ne!(cand_b.addr, alive_addr);

    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let budget = Duration::from_secs(3);
    let t0 = Instant::now();
    let outcome = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            // 死候选夹在活候选两侧：胜者必须是「活」的那条（不是位置效应）
            cands: vec![cand_a, direct(alive_addr), cand_b],
            budget,
            reply,
        },
        WAIT,
    )
    .await
    .expect("一活两死 ⇒ 必须有胜者");
    let elapsed = t0.elapsed();

    assert_eq!(outcome.winner, alive_addr, "胜者 = 活候选：{outcome:?}");
    assert_eq!(outcome.via, Via::Direct);
    assert_eq!(outcome.completed, vec![alive_addr], "完成清单只含胜者");
    assert_eq!(
        outcome.unfinished,
        vec![cand_a.addr, cand_b.addr],
        "未完成清单 = 两个死候选：{outcome:?}"
    );
    assert!(
        elapsed < budget + Duration::from_secs(2),
        "耗时上界（不判精确墙钟，flake 口径②）：实耗 {elapsed:?}"
    );
    assert!(
        wait_for(&stub, &quic, || stub.accepted() >= 1, WAIT).await,
        "胜者必须完成登记"
    );
    assert_eq!(island.snapshot().via, Some(Via::Direct));
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-2 的失败面）**：全候选失败 ⇒ `IslandErr::NoCandidate`（且快照无 path）。
#[tokio::test]
async fn race_all_dead_reports_no_candidate() {
    let quic = exit_face(3);
    let stub = Stub::new();
    let (_d1, c1) = dead_candidate();
    let (_d2, c2) = dead_candidate();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());

    let budget = Duration::from_millis(600);
    let t0 = Instant::now();
    let res = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![c1, c2],
            budget,
            reply,
        },
        WAIT,
    )
    .await;
    assert!(
        matches!(res, Err(IslandErr::NoCandidate)),
        "全死必须归 NoCandidate：{res:?}"
    );
    assert!(
        t0.elapsed() < budget + Duration::from_secs(2),
        "失败也必须在上界内定音：{:?}",
        t0.elapsed()
    );
    let snap = island.snapshot();
    assert_eq!(snap.via, None, "无胜者 ⇒ via=None");
    assert_eq!(snap.ep, None);
    // 空候选（且未 SetCandidates）同归 NoCandidate
    let res2 = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: Vec::new(),
            budget,
            reply,
        },
        WAIT,
    )
    .await;
    assert!(matches!(res2, Err(IslandErr::NoCandidate)), "{res2:?}");
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S1-9 的消费侧）**：错 RPK 钉定 ⇒ 握手在 TLS 内中止 ⇒ 赛跑无胜者
/// （不是「连上再拒」；出口侧握手失败计数增长）。
#[tokio::test]
async fn wrong_pin_aborts_handshake_without_winner() {
    let quic = exit_face(4);
    let stub = Stub::new();
    let wrong = RpkPublicKey::from_bytes([0xEE; 32]);
    assert_ne!(wrong, quic.rpk_public_key());
    let island = island_with(Duration::from_secs(60), wrong);
    let addr = connectable(&quic);
    let res = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: Duration::from_millis(900),
            reply,
        },
        WAIT,
    )
    .await;
    assert!(
        matches!(res, Err(IslandErr::NoCandidate)),
        "错 pin 不得连上：{res:?}"
    );
    assert_eq!(stub.accepted(), 0, "出口侧零采纳（握手期即中止）");
    assert_eq!(quic.snapshot().admitted, 0, "出口面零采纳连接");
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 赛跑在途时再来一条 `Connect` ⇒ `RaceInFlight`（赛跑结论是一次性的）；收工可在途中进行
/// （长任务随 `JoinSet` abort，不拖累收工预算）。
#[tokio::test]
async fn connect_while_racing_is_rejected_and_stop_is_prompt() {
    let quic = exit_face(5);
    let stub = Stub::new();
    let (_d, dead) = dead_candidate();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());

    let (tx1, rx1) = channel();
    island
        .tx()
        .send(Cmd::Connect {
            cands: vec![dead],
            budget: Duration::from_secs(30), // 长预算：确保「在途」窗口足够观察
            reply: tx1,
        })
        .expect("投递第一轮赛跑");
    let second = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![dead],
            budget: Duration::from_millis(100),
            reply,
        },
        Duration::from_secs(2),
    )
    .await;
    assert!(
        matches!(second, Err(IslandErr::RaceInFlight)),
        "在途赛跑必须拒第二轮：{second:?}"
    );
    drop(rx1); // 第一轮的回执在收工 abort 时随之 drop（不读）

    let t0 = Instant::now();
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "赛跑在途也必须预算内收工"
    );
    assert!(
        t0.elapsed() < Duration::from_secs(4),
        "收工不得越过上界：{:?}",
        t0.elapsed()
    );
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 3. 刷新（60s 节拍：虚拟时钟 + 集成面短节拍） ----------

/// **判据（S2-6 的 60s 节拍）**：纯定时语义用虚拟时钟（`start_paused`）——一拍一帧。
#[tokio::test(start_paused = true)]
async fn refresh_timer_ticks_one_frame_per_patrol_in_virtual_time() {
    let patrol = Duration::from_secs(60);
    let t0 = TokioInstant::now();
    let mut t = RefreshTimer::new(patrol);
    assert!(!t.due(t0), "起手不到点");
    assert!(!t.due(t0 + Duration::from_secs(59)), "59s 不到点");
    assert!(t.due(t0 + Duration::from_secs(60)), "60s 到点");
    assert!(!t.due(t0 + Duration::from_secs(119)), "下一拍前不到点");
    assert!(t.due(t0 + Duration::from_secs(120)), "120s 到点");

    // 虚拟时钟推进（自动跳）语义下再走一遍：3 拍 ⇒ 恰 3 次到点
    let mut t = RefreshTimer::new(patrol);
    let mut fired = 0;
    for _ in 0..3 {
        tokio::time::sleep(patrol).await;
        if t.due(TokioInstant::now()) {
            fired += 1;
        }
    }
    assert_eq!(fired, 3, "60s 节拍：3 拍恰 3 次");
}

/// **判据（S2-6）**：刷新帧字节级到达对端（`hr-reg3` + 本连接 exporter），行文 = C15'。
/// 岛侧节拍用短值（集成面不判精确墙钟——只判「按拍发生且逐帧合法」）。
#[tokio::test]
async fn refresh_frames_reach_peer_and_log_c15_prime() {
    let quic = exit_face(6);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(
        Duration::from_millis(300),
        quic.rpk_public_key(),
        Arc::clone(&logf),
    );
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    assert_eq!(stub.accepted(), 1, "起手一次登记");

    // 两拍刷新 ⇒ 出口侧再收两帧（每帧都是合法的连接绑定登记帧）
    assert!(
        wait_for(&stub, &quic, || stub.accepted() >= 3, WAIT).await,
        "刷新帧必须按拍到达（accepted={}）",
        stub.accepted()
    );
    assert_eq!(stub.rejected(), 0, "刷新帧不得被拒（exporter 复用正确）");
    let lines = logs_until(&logs, "quic: 注册刷新 → ", WAIT).await;
    let line = lines
        .iter()
        .find(|l| l.contains("quic: 注册刷新 → "))
        .expect("C15' 行在")
        .clone();
    assert!(line.contains(&addr.to_string()), "行含端点：{line}");
    assert!(
        line.contains("dev=22222222") && line.contains("中继=false"),
        "行含 dev 短指纹与中继位：{line}"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 4. 迁移（rebind） ----------

/// **判据（S2-3）**：`127.0.0.1:0` → `127.0.0.2:0` rebind 后
/// ①连接未断 ②出口 `remote_address()` 变化（E-q2「路径变更」行）③收发继续（出口回程 ⇒
/// 岛侧入站证据 ⇒ N-b 行）④**设备表不新增条目**（引擎桩 accepted 不增）。
///
/// 源 IP 变化（不是换端口）⇒ 走协议的真迁移分支（`paths.rs` 的换 IP 分支）。
#[tokio::test]
async fn rebind_keeps_connection_and_peer_observes_new_path() {
    let quic = exit_face(7);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(
        Duration::from_secs(3), // 迁移保持判据窗（够长：回环 PATH_RESPONSE 是亚秒级）
        quic.rpk_public_key(),
        Arc::clone(&logf),
    );
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    let path_changes0 = quic.snapshot().path_changes;

    // ---- rebind 到另一个本地地址（源地址变化 = 「换网卡」的本地等价物）----
    let (alt, how) = alt_bind_addr();
    println!("迁移目标口径：{how} ⇒ {alt}");
    let to = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Rebind { local: Some(alt), reply },
        WAIT,
    )
    .await
    .expect("换绑必须成功");
    assert_ne!(to.port(), 0, "新本地地址由内核分配：{to}");

    // ② 出口侧：remote_address() 变化 + E-q2 行（路径变更）
    assert!(
        wait_for(
            &stub,
            &quic,
            || quic.snapshot().path_changes > path_changes0,
            WAIT
        )
        .await,
        "出口必须观测到路径变更（path_changes={}）",
        quic.snapshot().path_changes
    );

    // ① 连接未断 + ③ 收发继续：出口发一包 ⇒ 只在**新路径**上才可能到达岛
    let sent = quic.send_to_pub(&PUBKEY, &inner_pkt(TUN_IP, Ipv4Addr::new(10, 0, 0, 7)));
    assert_eq!(
        sent,
        crate::ExitSend::Handled,
        "出口侧出站必须已在 QUIC 面绑定（回程发往新路径）"
    );
    assert!(
        wait_for(&stub, &quic, || island.snapshot().migrations >= 1, WAIT).await,
        "岛必须在保持窗内确认迁移（入站证据 ⇒ N-b）"
    );

    let lines = logs_until(&logs, "quic: 迁移完成（", WAIT).await;
    let line = lines
        .iter()
        .find(|l| l.contains("quic: 迁移完成（"))
        .expect("N-b 行在")
        .clone();
    assert!(
        line.contains(&to.to_string()) && line.contains(" → "),
        "N-b 行含旧 → 新（新地址 = {to}）：{line}"
    );

    // ④ 出口设备表不新增条目 + 连接仍只有一条 + 岛侧 path 未清
    assert_eq!(stub.accepted(), 1, "迁移不得产生新登记（设备表不新增条目）");
    assert_eq!(quic.snapshot().connections, 1, "出口侧连接数不变");
    let snap = island.snapshot();
    assert_eq!(snap.via, Some(Via::Direct), "连接保持：{snap:?}");
    assert!(!snap.migration_unconfirmed, "迁移已确认");
    assert_eq!(snap.migrations, 1);

    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 迁移保持的**失败判据**面：无连接时 `rebind` 只是换 socket（不起保持窗），
/// 且 `migration_unconfirmed` 保持清零——「N 拍无回包 ⇒ 未确认」的到点分支由
/// `client::migration` 的纯逻辑用例覆盖（真路径无回包不可确定性构造）。
#[tokio::test]
async fn rebind_without_connection_only_swaps_socket() {
    let quic = exit_face(8);
    let stub = Stub::new();
    let island = island_with(Duration::from_millis(200), quic.rpk_public_key());
    let to = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Rebind {
            local: Some(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)),
            reply,
        },
        WAIT,
    )
    .await
    .expect("无连接也可换绑（endpoint 粒度）");
    assert_eq!(*to.ip(), Ipv4Addr::LOCALHOST);
    let snap = island.snapshot();
    assert!(!snap.migration_unconfirmed, "无连接 ⇒ 不起保持窗");
    assert_eq!(snap.migrations, 0);
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

// ---------- 5. Probe / 丢弃计数 / SetCandidates ----------

/// **判据（S2-5 的 `Probe` 命令面）**：未建连 ⇒ `NotConnected`；已登记连接 ⇒ 预算内有结论
/// （对端回包证据 ⇒ Ok(rtt)）；连接被关 ⇒ `ConnectionLost`。
#[tokio::test]
async fn probe_reports_alive_then_connection_lost() {
    let quic = exit_face(9);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());

    let not_connected = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Probe {
            budget: Duration::from_millis(200),
            reply,
        },
        Duration::from_secs(2),
    )
    .await;
    assert!(
        matches!(not_connected, Err(IslandErr::NotConnected)),
        "未建连必须归 NotConnected：{not_connected:?}"
    );

    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");

    let rtt = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Probe {
            budget: Duration::from_secs(3),
            reply,
        },
        WAIT,
    )
    .await
    .expect("活连接必须判活（对端回包证据）");
    assert!(rtt < Duration::from_secs(1), "回环 RTT 量级上界：{rtt:?}");

    // ① 连接被**显式关闭**（出口侧摘绑定 ⇒ CONNECTION_CLOSE）⇒ 判活归 ConnectionLost
    quic.unbind_pub(&PUBKEY);
    let mut last = Ok(Duration::ZERO);
    for _ in 0..40 {
        last = send_wait(
            &island,
            &stub,
            &quic,
            |reply| Cmd::Probe {
                budget: Duration::from_millis(300),
                reply,
            },
            Duration::from_secs(2),
        )
        .await;
        if matches!(last, Err(IslandErr::ConnectionLost)) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        matches!(last, Err(IslandErr::ConnectionLost)),
        "显式关闭必须归 ConnectionLost：{last:?}"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// **判据（S2-5 的失败面，设计 §2.3）**：对端**静默消失**（无专用错误——quinn 侧要等
/// `max_idle_timeout` 才定音）⇒ 判活必须**失败**（`ConnectionLost` 或 `ProbeNoResponse`，
/// 由是否收到 CONNECTION_CLOSE 定——**不得**判活成功）。
#[tokio::test]
async fn probe_fails_when_peer_silently_gone() {
    let quic = exit_face(13);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    // 出口面收工（可能发出 CONNECTION_CLOSE，也可能来不及刷出 ⇒ 客户端侧静默）
    assert!(quic.stop_within(Instant::now() + BUDGET));
    let mut last = Ok(Duration::ZERO);
    for _ in 0..20 {
        last = send_wait(
            &island,
            &stub,
            &quic,
            |reply| Cmd::Probe {
                budget: Duration::from_millis(300),
                reply,
            },
            Duration::from_secs(2),
        )
        .await;
        if last.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        last.is_err(),
        "对端消失后判活必须失败（不误报活）：{last:?}"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
}

/// **判据（§2.4/§6.4 的「丢弃可观测、不静默」）**：四类计数入快照 + N-c 行 + 事件回调；
/// `TunPacket` 在无登记连接时归 `未登记`（登记前丢弃）。
#[tokio::test]
async fn drop_counters_events_and_nc_line() {
    let quic = exit_face(10);
    let stub = Stub::new();
    let (logf, logs) = sink();
    let island = island_with_log(Duration::from_secs(60), quic.rpk_public_key(), Arc::clone(&logf));
    let (ev_tx, ev_rx) = channel();
    island
        .tx()
        .send(Cmd::SetOnEvent {
            h: Arc::new(move |e: IslandEvent| {
                let _ = ev_tx.send(e);
            }),
        })
        .expect("装事件回调");

    // 数据面上报（TUN 读线程/回程线程的形态）
    island
        .tx()
        .send(Cmd::DatagramDropped {
            reason: DropReason::TooLarge,
            n: 2,
        })
        .expect("投递丢弃上报");
    let lines = logs_until(&logs, "quic: 丢弃 超限=2", WAIT).await;
    assert!(
        lines
            .iter()
            .any(|l| l.contains("quic: 丢弃 超限=2 发送缓冲满=0 回程队列满=0 未登记=0")),
        "N-c 行四字段序固定：{lines:?}"
    );
    assert_eq!(
        ev_rx.recv_timeout(WAIT).expect("事件必到"),
        IslandEvent::DatagramDropped {
            reason: DropReason::TooLarge,
            n: 2
        }
    );

    // 无连接时的 TunPacket ⇒ `未登记`（登记前丢弃）
    island
        .tx()
        .send(Cmd::TunPacket(vec![0u8; 64].into_boxed_slice()))
        .expect("投包");
    assert!(
        wait_for(&stub, &quic, || island.snapshot().drops.unregistered >= 1, WAIT).await,
        "无登记连接的 TunPacket 必须计 `未登记`"
    );
    let snap = island.snapshot();
    assert_eq!(snap.packets_in, 1, "投递计数照旧");
    assert_eq!(snap.drops.too_large, 2);

    // 有连接后：`TunPacket` 不再计未登记（发送面 = S2-4）
    let addr = connectable(&quic);
    let _ = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: vec![direct(addr)],
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("登记成功");
    let before = island.snapshot().drops.unregistered;
    island
        .tx()
        .send(Cmd::TunPacket(vec![0u8; 64].into_boxed_slice()))
        .expect("投包");
    assert!(
        wait_for(&stub, &quic, || island.snapshot().packets_in >= 2, WAIT).await,
        "第二包已处置"
    );
    assert_eq!(
        island.snapshot().drops.unregistered,
        before,
        "有登记连接后不再计 `未登记`"
    );
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// `SetCandidates` 更新清单；`Connect` 的空 `cands` 回落到它（S2-1 的成员语义）。
#[tokio::test]
async fn set_candidates_is_used_when_connect_has_none() {
    let quic = exit_face(11);
    let stub = Stub::new();
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    let addr = connectable(&quic);
    island
        .tx()
        .send(Cmd::SetCandidates {
            cands: vec![direct(addr)],
        })
        .expect("更新候选");
    let outcome = send_wait(
        &island,
        &stub,
        &quic,
        |reply| Cmd::Connect {
            cands: Vec::new(),
            budget: WAIT,
            reply,
        },
        WAIT,
    )
    .await
    .expect("回落 SetCandidates 的清单 ⇒ 胜出");
    assert_eq!(outcome.winner, addr);
    assert_eq!(island.snapshot().candidates, 1);
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 岛收工后投递命令 ⇒ `EngineGone`（通道已断；不得挂死）。
#[tokio::test]
async fn send_after_stop_is_engine_gone() {
    let quic = exit_face(12);
    let island = island_with(Duration::from_secs(60), quic.rpk_public_key());
    assert!(island.stop_within(Instant::now() + BUDGET));
    assert!(island.is_finished());
    let (tx, rx) = channel();
    let tx_handle: IslandTx = island.tx();
    // 收工后投递：可能仍进队列（接收端已 drop ⇒ 立刻 Err）或直接 Err——两形态都归 EngineGone
    if tx_handle
        .send(Cmd::Connect {
            cands: Vec::new(),
            budget: Duration::from_millis(1),
            reply: tx,
        })
        .is_err()
    {
        return;
    }
    let got = match rx.recv_timeout(BUDGET) {
        Ok(v) => v,
        Err(_) => return, // 回执口随岛退出被 drop ⇒ 调用侧归 EngineGone
    };
    assert!(matches!(got, Err(IslandErr::EngineGone) | Err(IslandErr::NoCandidate)));
    assert!(quic.stop_within(Instant::now() + BUDGET));
}

/// 快照 `Default` 面（S3 的 JSON 段基线）：未建连时四类丢弃零值、`via/ep/mtu` 为空。
#[test]
fn snapshot_default_is_empty_path() {
    let s = IslandSnapshot::default();
    assert_eq!(s.via, None);
    assert_eq!(s.ep, None);
    assert_eq!(s.rtt_ms, 0);
    assert_eq!(s.mtu, None);
    assert_eq!(s.current_mtu, 0);
    assert_eq!(s.mirrors, 0);
    assert_eq!(s.drops.too_large, 0);
    assert_eq!(s.drops.send_buffer_full, 0);
    assert_eq!(s.drops.return_queue_full, 0);
    assert_eq!(s.drops.unregistered, 0);
    assert_eq!(s.migrations, 0);
    assert!(!s.migration_unconfirmed);
}

/// reg3 帧的**连接绑定**在客户端侧的字节面复核（S1-3 的对偶）：同 secret 同字段但
/// exporter 不同 ⇒ 帧不同（换连接重放必败的客户端侧根据）。
#[test]
fn reg3_frame_binds_to_exporter() {
    let cred = IslandCredential::new(
        TokenSecret::from_bytes(SECRET),
        PUBKEY,
        DEV,
        RpkPublicKey::from_bytes([0x33; 32]),
    );
    let a = register::frame_now(&cred, &[0xA1; 32]);
    let b = register::frame_now(&cred, &[0xB2; 32]);
    assert_ne!(a, b, "exporter 不同 ⇒ 帧不同");
    assert_eq!(&a[..2], b"H3", "魔数 = H3");
    assert_eq!(a.len(), crate::reg3::LEN);
    assert_eq!(&a[2..34], &PUBKEY);
    assert_eq!(&a[34..42], &DEV);
}
