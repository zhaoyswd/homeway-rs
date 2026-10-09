//! 岛骨架单测（设计 §9.1 的 10 条口径；**全部走公面**）。
//!
//! flake 口径（设计 §9.2 五条，写进用例形态）：
//! 1. **不钉固定端口**——本文件零固定端口（回环端口只在用例 8 里以 `:0` 交内核分配）；
//! 2. **时间断言禁精确墙钟**——只断言**上界**（`elapsed < 预算 × 4`）与「预算内收工」形态；
//! 3. **不依赖 loadavg**——岛内无吞吐断言（性能判据全在 `tools/quic-ab.sh`）；
//! 4. **隔离复跑纪律**——本文件红了先 `--test-threads=1` 独占复跑再判是否回归；
//! 5. **登记动作**——五条口径写进路线文件「已知 flake 登记」（主会话插入，见 `docs/reviews/M0.md`）。
//!
//! 用例 4/5b 的「到点 detach」把岛线程留在卡死态（镜像 `wgcore` 的 detach 用例形态）：
//! 测试进程结束时该线程随进程消失；`Island::drop` 因 stop 位已置而不等待。

use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::net::SocketAddrV4;

use crate::cmd::{
    Candidate, Cmd, DropReason, Drops, IslandErr, IslandEvent, IslandReply, IslandSnapshot,
    RaceOutcome, Via,
};
use crate::driver::seams;
use crate::{
    Island, IslandConfig, IslandCredential, IslandTx, Logf, OnEvent, OnUnhealthy, RpkPublicKey,
    TokenSecret,
};

/// 收工预算（与 `wgcore::CLIENT_CLOSE_BUDGET` 同量级 = 2s）。
const BUDGET: Duration = Duration::from_secs(2);
/// 上界断言口径（flake 口径 2②：只判上界，不判区间）。
const CAP: Duration = Duration::from_secs(8);

/// 日志落点 + 收集端（断言记行面）。
fn sink() -> (Logf, Receiver<String>) {
    let (tx, rx) = channel();
    (
        Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }),
        rx,
    )
}

/// 不健康回调 + 收集端（断言分类面）。
fn unhealthy_sink() -> (OnUnhealthy, Receiver<String>) {
    let (tx, rx) = channel();
    (
        Arc::new(move |r: &str| {
            let _ = tx.send(r.to_owned());
        }),
        rx,
    )
}

fn noop_unhealthy() -> OnUnhealthy {
    Arc::new(|_r: &str| {})
}

/// 测试用凭据（secret/pubkey/devTag/pin 全为构造值；M0 的用例不建连，pin 对不上无妨）。
fn test_credential() -> IslandCredential {
    IslandCredential::new(
        TokenSecret::from_bytes([0x5A; 32]),
        [0x11; 32],
        [0x22; 8],
        RpkPublicKey::from_bytes([0x33; 32]),
    )
}

fn test_config() -> IslandConfig {
    IslandConfig::new(test_credential())
}

fn island_of(logf: Logf) -> Island {
    Island::start(logf, noop_unhealthy(), test_config())
        .expect("岛可起（专用线程 + 单线程 runtime + QUIC 端点）")
}

/// 有界取回回执 —— **实现设计 §3.6-2① 的映射**：通道断开（岛死/线程 panic ⇒ 栈上
/// reply sender 被 drop） ⇒ [`IslandErr::EngineGone`]，**禁 `unwrap()`**；
/// 超时在测试里是「挂死」的证据 ⇒ 判红（不是 flake 宽容面）。
fn recv_reply<T>(rx: &Receiver<Result<T, IslandErr>>, wait: Duration) -> Result<T, IslandErr> {
    match rx.recv_timeout(wait) {
        Ok(v) => v,
        Err(RecvTimeoutError::Disconnected) => Err(IslandErr::EngineGone),
        Err(RecvTimeoutError::Timeout) => panic!("回执超时（岛未应答 ⇒ 挂死）"),
    }
}

/// attach 隧道面。
///
/// ⚠️ 多数用例传 `fd = -1`（**故意无效**）：S2-4 起 attach 会真起 TUN 读/写线程，
/// 而本骨架用例只验 attach 语义 ⇒ 给一个必然 EBADF 的 fd，免得读线程去读测试进程里
/// 恰好存在的 fd（那会把别的用例的数据读走——同进程并发用例的隐性干扰）。
fn attach(island: &Island, fd: i32, mtu: u32, wait: Duration) -> Result<(), IslandErr> {
    let (rtx, rrx) = channel();
    island
        .tx()
        .send(Cmd::TunAttach { fd, mtu, reply: rtx })
        .map_err(|_| IslandErr::EngineGone)?;
    recv_reply(&rrx, wait)
}

/// 投一条带 reply 的 attach 命令但**不等回执**（注入缝用例的触发命令：岛在处置点
/// 卡死/panic ⇒ 回执永不到；receiver 留在栈上，岛侧 send 失败被忽略）。
fn put_attach_command(island: &Island) {
    let (rtx, _rrx) = channel();
    island
        .tx()
        .send(Cmd::TunAttach {
            fd: -1,
            mtu: 1280,
            reply: rtx,
        })
        .expect("命令投递（注入触发）");
}

/// 投一包（热路径无回执）。
fn put_packet(island: &Island, len: usize) {
    island
        .tx()
        .send(Cmd::TunPacket(vec![0u8; len].into_boxed_slice()))
        .expect("投包不阻塞（unbounded）");
}

/// 有界收集日志行直到出现含 `needle` 的行；返回「截至命中（含）的全部行」。
/// 先用它滚动再断言，避免多行都在时必须分次消费（无轮询 `sleep`——用 `recv_timeout` 当等待）。
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

// ---------- 1 ----------

/// 起岛 → `Stop` → `stop_within(now+2s)` = true；线程已 join（`is_finished()`）。
#[test]
fn island_starts_and_stops_within_budget() {
    let (logf, _logs) = sink();
    let island = island_of(logf);
    // 先做一次 attach 往返：证明「岛线程已进入命令循环」再起预算计时——否则预算里还含线程
    // 首次调度的时间，重载下会让「预算内收工」断言偶发红。
    assert!(matches!(attach(&island, -1, 1280, BUDGET), Ok(())));
    let t0 = Instant::now();
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "岛必须在收工预算内退出（true）"
    );
    assert!(
        t0.elapsed() < CAP,
        "收工不得越过上界（flake 口径 2②；实测 {:?}）",
        t0.elapsed()
    );
    assert!(island.is_finished(), "线程已 join 收口（exit 位可见）");
}

// ---------- 2 ----------

/// 首次 attach → `Ok(())`；二次 → `Err(TunAlreadyAttached)`。
#[test]
fn tun_attach_replies_and_rejects_second() {
    let (logf, _logs) = sink();
    let island = island_of(logf);
    assert!(
        matches!(attach(&island, -1, 1280, BUDGET), Ok(())),
        "首次 attach 必须 Ok"
    );
    assert!(
        matches!(
            attach(&island, -1, 1400, BUDGET),
            Err(IslandErr::TunAlreadyAttached)
        ),
        "二次 attach 必须 TunAlreadyAttached"
    );
    assert!(island.snapshot().attached, "快照 attached 置位");
    assert!(island.stop_within(Instant::now() + BUDGET), "正常收工");
}

// ---------- 3 ----------

/// 投 `TunPacket` 后仍可正常收工（热路径不阻塞、无回执通道需求），且计数 +1。
///
/// **有界观测**（设计门 4.2 的要求）：无 reply ⇒ 用**同通道保序**的屏障命令（再来一次
/// attach）的回执当观测点——回执到 = 其前的包必已被处置；回执取回本身有界（2s）。
/// 该形态比「轮询快照 + sleep」确定（零 flake），也满足「必须给有界等待」的口径。
#[test]
fn tun_packet_is_accepted_without_reply() {
    let (logf, _logs) = sink();
    let island = island_of(logf);
    assert!(matches!(attach(&island, -1, 1280, BUDGET), Ok(())));
    put_packet(&island, 64);
    assert!(
        matches!(
            attach(&island, -1, 1280, BUDGET),
            Err(IslandErr::TunAlreadyAttached)
        ),
        "屏障命令的回执必须在 TunPacket 之后到达（同通道保序）"
    );
    assert_eq!(island.snapshot().packets_in, 1, "投递计数 +1");
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "投包后仍可正常收工"
    );
}

// ---------- 4 ----------

/// 卡死注入（`#[cfg(test)]` 缝）⇒ 到点返回 **false** + 收割线程 `hw-quic-reap` 接手。
#[test]
fn stop_within_detaches_when_island_stuck() {
    let (logf, logs) = sink();
    let island = Island::start_with_seam(logf, noop_unhealthy(), test_config(), seams::HANG)
        .expect("岛可起（卡死注入缝）");
    // 触发注入：投一条命令（其回执永不到——岛在处置点卡死）
    put_attach_command(&island);
    // 确定性同步点：注入缝先记「注入开始」再卡死 —— 见到该行 = 岛已消费命令并进入卡死态
    // （驱动循环头判 stop 位就退出，队列里的命令会被丢弃 ⇒ 不先同步会有竞态）。
    drain_until(&logs, seams::MARK_HANG, BUDGET);
    assert!(
        !island.stop_within(Instant::now() + Duration::from_millis(200)),
        "卡死岛到点必须返回 false（detach）"
    );
    let lines = drain_until(&logs, "hw-quic-reap", Duration::from_secs(2));
    assert!(
        lines.iter().any(|l| l.contains("到点 detach")),
        "收割线程必须接手并留证：{lines:?}"
    );
}

// ---------- 5a ----------

/// panic 面专项：①不健康回调**立即**收到 `"panic"`（不等收尾）；②在途命令的
/// `reply` 归 `IslandErr::EngineGone`（**不挂死**）；③`stop_within` 返回 **true**
/// （panic 后线程已 finished——false 只属"卡死"）；④panic 记行由 `stop_within` 的
/// join 分支产生（设计 §3.6-4 分工表）。
#[test]
fn island_panic_is_classified_in_place_and_surfaces_as_engine_gone() {
    let (logf, logs) = sink();
    let (on_unhealthy, reasons) = unhealthy_sink();
    let island = Island::start_with_seam(logf, on_unhealthy, test_config(), seams::PANIC)
        .expect("岛可起（panic 注入缝）");

    // 在途命令：带 reply 的 attach 到点即 panic（回执 sender 随栈解开被 drop）。
    let (rtx, rrx) = channel();
    island
        .tx()
        .send(Cmd::TunAttach {
            fd: -1,
            mtu: 1280,
            reply: rtx,
        })
        .expect("命令投递");
    assert!(
        matches!(recv_reply(&rrx, BUDGET), Err(IslandErr::EngineGone)),
        "岛死 ⇒ 在途命令必须归 EngineGone（不得挂死）"
    );

    // ① 即时分类：不调用收工面就能收到不健康原因。
    match reasons.recv_timeout(BUDGET) {
        Ok(r) => assert_eq!(r, "panic", "不健康原因必须是 panic（判据语义取值集）"),
        Err(e) => panic!("岛内 panic 未即时分类上报：{e:?}"),
    }

    // ③ panic（unwind）后线程已 finished ⇒ 预算内收工返 true
    let t0 = Instant::now();
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "panic 后必须预算内收工（true）"
    );
    assert!(t0.elapsed() < CAP, "收工不得越过上界（实测 {:?}）", t0.elapsed());

    // ④ join 分支的 panic 记行。**非恒真断言**（代码门 M5 整改）：
    //   · 命中行必须带本测的注入文案（= 出自岛内 panic 载荷，不是别的行）；
    //   · 必须**同时**存在岛内就地分类行（`岛内 panic`）——两条分工不同（§3.6-4）；
    //   · 必须**没有**收割线程行（`hw-quic-reap`）——本测走预算内收工，行只能出自
    //     `stop_within` 的 join 分支（若谁把行改到收割线程，本断言即红）。
    let lines = drain_until(&logs, "本世代 QUIC 面已死", Duration::from_secs(2));
    let join_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("岛线程 panic") && l.contains(seams::MARK_PANIC))
        .collect();
    assert_eq!(join_lines.len(), 1, "join 侧恰一行且带注入文案：{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("岛内 panic")),
        "岛内就地分类行必须在（§3.6-1）：{lines:?}"
    );
    assert!(
        !lines.iter().any(|l| l.contains("hw-quic-reap")),
        "预算内收工不得出现收割线程行（行归属 = stop_within 的 join 分支）：{lines:?}"
    );
}

// ---------- 5b ----------

/// 卡死注入（延迟 panic）⇒ 到点 **false** + `hw-quic-reap` 接手 + **panic 行由收割线程
/// 的 join 分支产生**（与 5a 的"谁记行"口径成对）。
#[test]
fn stuck_island_panic_line_comes_from_reaper() {
    let (logf, logs) = sink();
    let island = Island::start_with_seam(logf, noop_unhealthy(), test_config(), seams::STALL_THEN_PANIC)
        .expect("岛可起（延迟 panic 缝）");
    // 触发注入：投一条命令（岛在处置点先卡死 1.2s，再 panic——回执永不到）
    put_attach_command(&island);
    // 确定性同步点：见到「注入开始」= 岛已消费命令并进入卡死窗（1.2s）
    drain_until(&logs, seams::MARK_STALL, BUDGET);
    // 到点（200ms ≪ 卡死窗 1.2s）⇒ detach（join 侧交接给收割线程）
    assert!(
        !island.stop_within(Instant::now() + Duration::from_millis(200)),
        "卡死窗内必须 detach（false）"
    );
    // 卡死窗过后岛内 panic ⇒ 只有收割线程的 join 分支能产生这行（stop_within 已 detach，
    // 本线程从未 join 过该句柄）
    let lines = drain_until(&logs, "本世代 QUIC 面已死", Duration::from_secs(5));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("hw-quic-reap") && l.contains("到点 detach")),
        "收割线程必须先留接手行：{lines:?}"
    );
    // **非恒真断言**（代码门 M5 整改）：命中行必须带**卡死**注入文案（证明是「卡死窗过后
    // 的 panic」被收割线程 join 到时记的），且恰一行；岛内就地分类行也应在。
    let join_lines: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("岛线程 panic") && l.contains(seams::MARK_STALL))
        .collect();
    assert_eq!(join_lines.len(), 1, "收割线程的 join 侧恰一行且带卡死文案：{lines:?}");
    assert!(
        lines.iter().any(|l| l.contains("岛内 panic")),
        "岛内就地分类行必须在（§3.6-1）：{lines:?}"
    );
}

// ---------- 6 ----------

/// 收工后 `snapshot()` 可读（不 panic、值冻结）。
#[test]
fn snapshot_is_pollable_after_stop() {
    let (logf, _logs) = sink();
    let island = island_of(logf);
    assert!(matches!(attach(&island, -1, 1280, BUDGET), Ok(())));
    put_packet(&island, 32);
    // **屏障**（同用例 3，代码门整改复验时实测到的竞态）：`stop_within` 会置 stop 位，而驱动
    // 循环**在循环头判位即退出** ⇒ 不等屏障就收工，队列里的 TunPacket 可能被丢（计数 0）——
    // 那不是缺陷，是停止语义；本用例要断言「冻结值 = 1」，故须先确认包已被消费（同通道保序）。
    assert!(
        matches!(
            attach(&island, -1, 1280, BUDGET),
            Err(IslandErr::TunAlreadyAttached)
        ),
        "屏障命令回执 = 其前的 TunPacket 必已被处置"
    );
    assert!(
        island.stop_within(Instant::now() + BUDGET),
        "正常收工（投包后）"
    );
    let s: IslandSnapshot = island.snapshot();
    assert!(s.attached, "收工后快照仍可读且值冻结");
    assert_eq!(s.packets_in, 1, "收工后计数冻结");
    assert_eq!(island.snapshot().packets_in, s.packets_in, "重复读取稳定");
}

// ---------- 7 ----------

/// 层 2：**可搬运性断言**（如实在此标注——一切异步类型都满足 `Send + 'static`，
/// 故本测对「公面夹带」检出率为 0，它保的是「同步面搬得动」；夹带检出靠层 3 源码门）。
#[test]
fn public_types_are_send_static() {
    fn assert_send_static<T: Send + 'static>() {}
    fn assert_send_sync_static<T: Send + Sync + 'static>() {}
    assert_send_static::<Cmd>();
    assert_send_static::<IslandTx>();
    assert_send_static::<IslandErr>();
    assert_send_static::<IslandSnapshot>();
    assert_send_static::<Island>();
    assert_send_sync_static::<IslandTx>();
    assert_send_sync_static::<Logf>();
    assert_send_sync_static::<OnUnhealthy>();
}

// ---------- 8 ----------

/// 依赖面活体证据（设计 §9.1 用例 8 的 `dep_face_alive_quinn_client_endpoint`）：
/// `current_thread` runtime 内建回环客户端端点并读回端口后丢弃（时序侧在 `driver.rs` 的
/// `#[cfg(test)]` 面——异步栈名字只允许出现在那里）。**函数名省去 crate 名**：本文件也受
/// 隔离门层 3 的 ② 条约束（代码里零异步栈名字），设计给的用例名只保留在文档里。
#[test]
fn dep_face_alive_client_endpoint() {
    let addr = crate::driver::dep_face_alive_endpoint().expect("依赖面活着：端点可建");
    assert!(addr.ip().is_loopback(), "只绑回环：{addr}");
    assert_ne!(addr.port(), 0, "回环端口由内核分配（flake 口径 1：不钉固定端口）");
}

// ---------- 10 ----------

/// 公面签名钉定（编译期）：显式 std 类型签名——一旦公面夹带异步栈类型即编译失败。
///
/// M1 S2a 起随成员同步（设计 §10 S2-1 的验收口径）：新增的 `Cmd` 成员/回执类型/配置面
/// 全部在这里逐字段写出类型；**加成员必须同步加臂**（下面没有通配臂 —— 少一条就编译红）。
#[test]
fn public_surface_signatures_are_pinned() {
    let _: fn(Logf, OnUnhealthy, IslandConfig) -> std::io::Result<Island> = Island::start;
    let _: fn(&Island) -> IslandTx = Island::tx;
    let _: fn(&Island) -> IslandSnapshot = Island::snapshot;
    let _: fn(&Island) -> bool = Island::is_finished;
    let _: fn(&Island) = Island::stop;
    let _: fn(&Island, Instant) -> bool = Island::stop_within;
    // 需求信号读面（M1 S2-4：与 `wgcore::Client` 同名同义——设计 §2.5「接口不变、来源切换」）
    let _: fn(&Island) -> i64 = Island::swap_out_pkts;
    let _: fn(&Island) -> Option<Instant> = Island::last_outbound_at;
    let _: fn(&Island) -> i64 = Island::last_outbound_unix_ms;
    let _: fn(&IslandTx, Cmd) -> Result<(), IslandErr> = IslandTx::send;
    // 线程名常量（写死供 M1 生命周期对接复用）
    let _: &str = crate::driver::ISLAND_THREAD;
    let _: &str = crate::driver::REAP_THREAD;
    // 兜底面：`Drop for Island`（镜像 `Drop for Client`）——句柄是拥有型值，可被 drop
    let _: fn(Island) = |i: Island| drop(i);
}

/// `Cmd` 成员的**字段类型**逐条钉定（编译期）：M0 三成员 + M1 S2a 的六个新成员。
#[test]
fn cmd_member_field_types_are_pinned() {
    fn pin(cmd: Cmd) {
        match cmd {
            // M0 三成员
            Cmd::TunAttach { fd, mtu, reply } => {
                let _: i32 = fd;
                let _: u32 = mtu;
                let _: IslandReply<()> = reply;
            }
            Cmd::TunPacket(pkt) => {
                let _: Box<[u8]> = pkt;
            }
            Cmd::Stop => {}
            // M1 S2a 增补（设计 §2.1 表）
            Cmd::SetOnUnhealthy { h } => {
                let _: OnUnhealthy = h;
            }
            Cmd::SetOnEvent { h } => {
                let _: OnEvent = h;
            }
            Cmd::SetCandidates { cands } => {
                let _: Vec<Candidate> = cands;
            }
            Cmd::Connect {
                cands,
                budget,
                reply,
            } => {
                let _: Vec<Candidate> = cands;
                let _: Duration = budget;
                let _: IslandReply<RaceOutcome> = reply;
            }
            Cmd::Rebind { local, reply } => {
                let _: Option<SocketAddrV4> = local;
                let _: IslandReply<SocketAddrV4> = reply;
            }
            Cmd::Probe { budget, reply } => {
                let _: Duration = budget;
                let _: IslandReply<Duration> = reply;
            }
            Cmd::DatagramDropped { reason, n } => {
                let _: DropReason = reason;
                let _: u64 = n;
            }
            // M1 S2-4 增补（镜像 `wgcore::Cmd::TunFdDead`：fd 面的 `fd` 分类落点）
            Cmd::TunFdDead { msg } => {
                let _: String = msg;
            }
            // M3 S1 增补（服务流族；设计 §1.4/§1.5——`StreamReply` 承载 typed `StreamErr`）
            Cmd::StreamOpen { tag, reply } => {
                let _: crate::StreamTag = tag;
                let _: crate::StreamReply<crate::StreamId> = reply;
            }
            Cmd::StreamWrite { id, data, reply } => {
                let _: crate::StreamId = id;
                let _: Vec<u8> = data;
                let _: crate::StreamReply<crate::StreamWriteOut> = reply;
            }
            Cmd::StreamRead { id, reply } => {
                let _: crate::StreamId = id;
                let _: crate::StreamReply<Vec<u8>> = reply;
            }
            Cmd::StreamShutdown { id, reply } => {
                let _: crate::StreamId = id;
                let _: crate::StreamReply<()> = reply;
            }
            Cmd::StreamClose { id, reply } => {
                let _: crate::StreamId = id;
                let _: crate::StreamReply<()> = reply;
            }
        }
    }
    let _: fn(Cmd) = pin;
    // 候选/承载/结算/丢弃/事件（设计 §2.1 的「类型承担不变量」）
    let _: Candidate = Candidate {
        addr: "127.0.0.1:1".parse::<SocketAddrV4>().unwrap(),
        via: Via::Relay { label: [0u8; 8] },
    };
    let _: Drops = Drops::default();
    let _: IslandEvent = IslandEvent::DatagramDropped {
        reason: DropReason::Unregistered,
        n: 1,
    };
}

/// `IslandErr` 变体逐条钉定（错误面也是公面：`source()` 不得返回异步栈错误）。
#[test]
fn island_err_variants_are_pinned() {
    fn pin(e: IslandErr) {
        match e {
            IslandErr::EngineGone => {}
            IslandErr::TunAlreadyAttached => {}
            IslandErr::NoCandidate => {}
            IslandErr::RaceInFlight => {}
            IslandErr::NotConnected => {}
            IslandErr::ConnectionLost => {}
            IslandErr::RegistrationFailed => {}
            IslandErr::ProbeNoResponse => {}
            IslandErr::Rebind(io_err) => {
                // 载荷必须是 std 的 io::Error（异步栈错误无处可藏）
                let _: std::io::Error = io_err;
            }
            // M1 S2-4 增补：TUN 面装配失败（同「std 载荷」契约）
            IslandErr::TunAttach(io_err) => {
                let _: std::io::Error = io_err;
            }
            // M3 S5 增补：准入归因（§4）——载荷仍**全是 std**（u64 / String；无异步栈夹带）
            IslandErr::AdmissionRejected { code } => {
                let _: u64 = code;
            }
            IslandErr::SessionClosed { reason } => {
                let _: String = reason;
            }
        }
    }
    let _: fn(IslandErr) = pin;
    // 构造配置面（凭据/绑定/节拍）
    let cfg = test_config();
    let _: IslandCredential = cfg.credential;
    let _: Option<SocketAddrV4> = cfg.bind;
    let _: Duration = cfg.patrol;
    let _: fn(TokenSecret) = |s: TokenSecret| drop(s);
}

/// 10 条的「第 9 条」= 源码门 `tools/check-quic-isolation.sh`（非单测，见设计 §3.4 层 3）；
/// 本用例只钉「该门在位」这一句，防将来误删调用点（门本体由 CI/ci-local 跑）。
#[test]
fn source_gate_reference_is_pinned() {
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tools/check-quic-isolation.sh");
    assert!(
        script.is_file(),
        "源码门脚本在位（设计 §3.4 层 3）：{}",
        script.display()
    );
}
