//! 赛跑（设计 §2.2）：并行 `connect` 到候选（LAN / 公网 / 中继），**首个完成握手者胜**、
//! 余者 drop；`via/ep/rtt` 交宿主入快照；行文 = C4'/C5'/C6'。
//!
//! 三处语义（逐条对着设计写）：
//!
//! 1. **谁赢**：首个从 `Connecting` 转为已建连（1-RTT 可用）者。QUIC 握手完成即双向可用
//!    （无 WG 的「响应过/首包」两段语义）。
//! 2. **何时算输**：其余候选 `drop`。⚠️ **口径订正（M2 S4 实测，主会话 §14-1④ 承接）**：
//!    `Connecting`（**未完成**的候选）**没有** quinn 的 close API —— `drop` 只是停止等待，
//!    连接驱动仍在跑（回环实测：3 候选 abort 后出口侧仍见到 2 条完成 ⇒ 销账）；因此**不能**
//!    把「未完成输家」说成「被主动关闭」。对**已完成但未胜出**的连接另行显式 `close()`
//!    （§2.2 的现任裁决，见下面的 drain 循环）；未完成输家的收口靠岛世代收摊（端点关）。
//!    出口侧对此的归因面 = `ExitQuicSnapshot::handshake_peer_closed`（主动关）vs
//!    `handshake_timeouts`/Failed-Other（静默/期限）——两档**都在每源闸的「未完成」输入集内**
//!    （§14-1① 不放宽计数集）。
//! 3. **预算**：`budget` = 整轮预算（不是每候选）；到点未完成者按 drop 收；全候选失败
//!    ⇒ [`IslandErr::NoCandidate`]。**同一枚 `budget` 也划出准入段的可用量**（M2 §1.7）：
//!    胜者产出后按 `max(剩余, ADMIT_MIN)` 给准入自带期限，失败/到点一律**显式关连接**
//!    （见 [`admit_budget`] / [`close_on_failed_admission`]）。
//!
//! 节流（C4'）照 `wtransport/bind.rs` 的 MIRROR 行常数：每轮 ≤3 行、两行间隔 ≥1s
//! （本切片每轮只打一行，闸由宿主持有——形态保留给 S2b/S3 的重复赛跑）。

use std::sync::Arc;
use std::time::{Duration, Instant};

use quinn::{ClientConfig, Connection, Endpoint};
use tokio::task::JoinSet;
use tokio::time::Instant as TokioInstant;

use crate::cmd::{Candidate, IslandErr, Logf, RaceOutcome};
use crate::config::IslandCredential;

use super::register;
use super::{Established, fmt_dur, server_name};

/// C4' 行节流常数（照 `wtransport/bind.rs` 的 MIRROR 行：每轮 ≤3 行、间隔 ≥1s）。
const RACE_LOG_MAX: u32 = 3;
const RACE_LOG_GAP: Duration = Duration::from_secs(1);

/// 准入段的**最小预算下界**（M2 设计 §1.7 / 设计门 r14 F8）：晚胜（赛跑吃光整轮预算）
/// 时仍给准入一个下界，避免「必然失败但不说清」。
pub(crate) const ADMIT_MIN: Duration = Duration::from_secs(2);

/// 准入可用预算 = `max(剩余, ADMIT_MIN)`（设计 §1.7 的规则，写死在这里——纯值函数，
/// 好让 `start_paused` 的虚拟时钟用例直接断言）。
///
/// **为什么准入段要自带期限**（r14 F8 的现状订正）：`QUIC_CONNECT_BUDGET` 今天只作赛跑
/// 循环的 deadline；登记段自身没有任何超时，唯一外层界是宿主的 RPC 超时——那时归因会落到
/// 笼统的 RPC 超时，且岛内任务仍持连接。本期限 + [`close_on_failed_admission`] 一起把
/// 「准入失败」变成**有界且显式收口**的结论。
pub(crate) fn admit_budget(total: Duration, elapsed: Duration) -> Duration {
    total.saturating_sub(elapsed).max(ADMIT_MIN)
}

/// 准入段的自带期限包装（抽名 = 虚拟时钟可断言；`None` = 到点）。
pub(super) async fn within<F: std::future::Future>(budget: Duration, fut: F) -> Option<F::Output> {
    tokio::time::timeout(budget, fut).await.ok()
}

/// 准入失败/超时的**显式收口**（设计 §1.7-③ / 设计门 r14 F8）：岛内任务在失败后必须显式
/// 关闭连接，**不留悬挂连接**。
///
/// 不关的代价：客户端侧要等 `max_idle_timeout=30s` 才有结论，出口侧的对端槽位被同一段
/// 时间占着（M2 的「未认证状态有界」在客户端半边同样成立才是真闭环）。归因行是排障入口
/// （行文含实际预算 ⇒ 能分辨「走了 `ADMIT_MIN` 下界」与「用了剩余量」）。
fn close_on_failed_admission(conn: &Connection, why: &str, budget: Duration, logf: &Logf) {
    conn.close(quinn::VarInt::from_u32(0), b"admission failed");
    (*logf)(&format!(
        "quic: 准入失败（{why}；预算 {}）—— 连接已显式关闭（不留悬挂）",
        fmt_dur(budget)
    ));
}

/// C4' 行节流器（双条件；宿主持有、跨轮复用）。
pub(crate) struct LogGate {
    n: u32,
    at: Option<Instant>,
}

impl LogGate {
    pub(crate) fn new() -> Self {
        Self { n: 0, at: None }
    }

    /// 本轮重置（每轮赛跑起跑时调）。
    pub(crate) fn reset(&mut self) {
        self.n = 0;
        self.at = None;
    }

    /// 本行是否可打（可打则就地记账）。
    pub(crate) fn due(&mut self, now: Instant) -> bool {
        let ok = self.n < RACE_LOG_MAX
            && self
                .at
                .is_none_or(|t| now.duration_since(t) >= RACE_LOG_GAP);
        if ok {
            self.n += 1;
            self.at = Some(now);
        }
        ok
    }
}

/// 赛跑 + 登记（设计 §2.2 + §2.6）：胜者完成握手后立刻走首条 bidi 控制流登记。
pub(crate) async fn run(
    endpoint: Arc<Endpoint>,
    cfg: ClientConfig,
    cred: Arc<IslandCredential>,
    cands: &[Candidate],
    budget: Duration,
    log_c4: bool,
    logf: &Logf,
) -> Result<(Established, RaceOutcome), IslandErr> {
    if cands.is_empty() {
        return Err(IslandErr::NoCandidate);
    }
    let t0 = Instant::now();
    if log_c4 {
        let relay_n = cands.iter().filter(|c| c.via.is_relay()).count();
        (*logf)(&format!(
            "quic: 赛跑投出 {} 个候选（直连 {} / 中继 {relay_n}；本行每轮限 3 条）",
            cands.len(),
            cands.len() - relay_n
        ));
    }

    // 并行发起（`Connecting` 自带端点引用 ⇒ 可搬进任务；`Endpoint` 本身不可 Clone）。
    let mut set: JoinSet<(usize, Result<Connection, quinn::ConnectionError>)> = JoinSet::new();
    let mut refused: Vec<usize> = Vec::new();
    for (i, c) in cands.iter().enumerate() {
        match endpoint.connect_with(cfg.clone(), c.addr.into(), server_name()) {
            Ok(connecting) => {
                set.spawn(async move { (i, connecting.await) });
            }
            // 调用面拒绝（端点停止/地址族不符）：计入「未完成」，不作整轮失败
            Err(_e) => refused.push(i),
        }
    }

    let mut completed: Vec<usize> = Vec::new();
    let mut unfinished: Vec<usize> = (0..cands.len())
        .filter(|i| !refused.contains(i))
        .collect();
    let mut winner: Option<(usize, Connection)> = None;
    let deadline = TokioInstant::now() + budget;
    loop {
        let joined = tokio::select! {
            j = set.join_next() => match j { Some(v) => v, None => break },
            () = tokio::time::sleep_until(deadline) => break,
        };
        match joined {
            Ok((i, Ok(conn))) => {
                completed.push(i);
                unfinished.retain(|x| *x != i);
                winner = Some((i, conn));
                break;
            }
            // 握手失败（对端 RST/协议错/超时前定音）：留「未完成」清单
            Ok((i, Err(_e))) => {
                unfinished.retain(|x| *x != i);
            }
            Err(_aborted) => {}
        }
    }
    // 余者：**已完成的**候选在 drain 里**显式关闭**（§2.2 现任裁决）；**未完成的**（在途
    // 握手）只能 abort——`Connecting` 无 close API（见模块头第 2 条，M2 S4 实测订正）。
    set.abort_all();
    while let Some(joined) = set.join_next().await {
        if let Ok((i, Ok(conn))) = joined {
            if !completed.contains(&i) {
                completed.push(i);
                unfinished.retain(|x| *x != i);
            }
            conn.close(quinn::VarInt::from_u32(0), b"lost race");
        }
    }

    let Some((wi, conn)) = winner else {
        // 全候选失败：失败清单进行（排障要看得见"哪个候选没起来"，r12 专2-3）
        let miss: Vec<String> = unfinished.iter().map(|i| cands[*i].addr.to_string()).collect();
        (*logf)(&format!(
            "quic: 赛跑小结：无胜者（候选 {} 个，耗时 {}）；未完成={}",
            cands.len(),
            fmt_dur(t0.elapsed()),
            miss.join("、")
        ));
        return Err(IslandErr::NoCandidate);
    };
    let elapsed = t0.elapsed();
    let via = cands[wi].via;
    let ep = cands[wi].addr;
    let rtt_ms = conn.rtt().as_millis() as u64;
    let done_list: Vec<String> = completed.iter().map(|i| cands[*i].addr.to_string()).collect();
    let miss_list: Vec<String> = unfinished.iter().map(|i| cands[*i].addr.to_string()).collect();
    (*logf)(&format!(
        "quic: 赛跑结算：胜出 {} {}（候选 {} 个，耗时 {}）；完成={}；未完成={}",
        via.text(),
        ep,
        cands.len(),
        fmt_dur(elapsed),
        done_list.join("、"),
        miss_list.join("、"),
    ));
    (*logf)(&format!(
        "quic: 路径确立：{} {}（首个完成握手）",
        via.text(),
        ep
    ));

    // ---- 准入（§1.7）：本连接的 exporter + 首条 bidi 控制流上的 `hr-reg4` 四帧 ----
    // **自带期限 + 失败显式收口**（设计 §1.7 / r14 F8）：预算 = `max(剩余, ADMIT_MIN)`，
    // 到点/失败 ⇒ `RegistrationFailed` + 显式关连接（不留悬挂）。
    let abudget = admit_budget(budget, t0.elapsed());
    let exporter = match register::exporter_of(&conn) {
        Ok(e) => e,
        Err(e) => {
            close_on_failed_admission(&conn, "取连接绑定值（TLS exporter）失败", abudget, logf);
            return Err(e);
        }
    };
    let (send, recv) = match within(
        abudget,
        register::register_on_control_stream(&conn, &cred, &exporter, logf),
    )
    .await
    {
        Some(Ok(pair)) => pair,
        Some(Err(e)) => {
            close_on_failed_admission(&conn, &format!("登记失败（{e}）"), abudget, logf);
            return Err(e);
        }
        // 到点：出口面未回挑战/回执（或无响应）——客户端侧与「出口拒」在传输层不可区分
        None => {
            close_on_failed_admission(&conn, "超时（准入预算内未收到回执）", abudget, logf);
            return Err(IslandErr::RegistrationFailed);
        }
    };
    let outcome = RaceOutcome {
        winner: ep,
        via,
        rtt_ms,
        completed: completed.iter().map(|i| cands[*i].addr).collect(),
        unfinished: unfinished.iter().map(|i| cands[*i].addr).collect(),
        elapsed_ms: elapsed.as_millis() as u64,
    };
    Ok((
        Established {
            conn,
            send,
            recv,
            exporter,
            via,
            ep,
        },
        outcome,
    ))
}
