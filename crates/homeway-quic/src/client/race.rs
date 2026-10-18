//! 赛跑（设计 §2.2 + 2026-10-13 直连优先改裁）：并行 `connect` 到候选（LAN / 公网 / 中继），
//! **首个完成握手者胜**、余者 drop；**直连优先**（用户指令 2026-10-13，回归老 `DirectFirst`
//! 与 `tier:openspec/project.md`「直连优先、中继纯后备」口径——M5 前「取最快者」的裁决①被本
//! 条取代）：窗口内**只投直连**，窗口到点或直连已无在途才解锁中继（同一轮）。`via/ep/rtt`
//! 交宿主入快照；行文 = C4'/C5'/C6'。
//!
//! 四处语义（逐条对着设计写）：
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
//! 4. **直连优先窗口**（[`direct_window`]；`IslandConfig::direct_first`，缺省 2s、`0` = 关）：
//!    起跑只投**直连**候选；解锁中继的两个条件 = ①窗口到点仍无胜者 ②直连已无在途（全失败/
//!    调用面拒绝，此时不必烧满窗口）。无直连候选 / 无中继候选 / 窗口 = 0 ⇒ 不设窗口（全候选
//!    同刻投出，= 改裁前形态）。解锁与结果都**不静默**：解锁记行（两种原因分档），
//!    未投出的中继候选**不入** `completed`/`unfinished` 清单（与「调用面拒绝」同档——
//!    两清单的并集 = 本轮**实际投出**的候选，「赛跑投出」行是名册、不再是投出集）。
//!
//! 节流（C4'）照 `wtransport/bind.rs` 的 MIRROR 行常数：每轮 ≤3 行、两行间隔 ≥1s
//! （本切片每轮只打一行，闸由宿主持有——形态保留给 S2b/S3 的重复赛跑）。解锁行**不占**该闸：
//! 每轮至多一条（一次性事件，不是可重复的镜像拍）。

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

/// 直连优先窗口的**有效值** = `min(配置窗口, 整轮预算 / 2)`（纯值函数，虚拟时钟可断言）。
///
/// 为什么夹到半轮：配置窗口 ≥ 预算的形态会让中继腿**永远拿不到余量**（窗口到点时预算
/// 也用光了）——「中继纯后备」要求后备有一个**可达的上限时间点**，夹到半轮把「中继至少
/// 拿到半轮预算」变成结构性保证。生产缺省（预算 5s、窗口 2s）不受影响 = 设计值原样。
pub(crate) fn direct_window(budget: Duration, direct_first: Duration) -> Duration {
    direct_first.min(budget / 2)
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
        "准入失败（{why}；预算 {}）—— 连接已显式关闭（不留悬挂）",
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
///
/// `direct_first` = 直连优先窗口（[`direct_window`] 取有效值；`0` = 关）。
#[allow(clippy::too_many_arguments)] // 赛跑单入口：句柄三件 + 候选 + 两枚时间值（整轮预算/岛级窗口）+ 行闸/记行口
pub(crate) async fn run(
    endpoint: Arc<Endpoint>,
    cfg: ClientConfig,
    cred: Arc<IslandCredential>,
    cands: &[Candidate],
    budget: Duration,
    direct_first: Duration,
    log_c4: bool,
    logf: &Logf,
) -> Result<(Established, RaceOutcome), IslandErr> {
    if cands.is_empty() {
        return Err(IslandErr::NoCandidate);
    }
    let t0 = Instant::now();
    if log_c4 {
        // 行文零改（判据行）：语义 = 本轮**名册**（直连优先下中继的实际投出点 = 解锁行）
        let relay_n = cands.iter().filter(|c| c.via.is_relay()).count();
        (*logf)(&format!(
            "赛跑投出 {} 个候选（直连 {} / 中继 {relay_n}；本行每轮限 3 条）",
            cands.len(),
            cands.len() - relay_n
        ));
    }

    // ---- 直连优先（模块头第 4 条）：起跑只投直连；窗口到点/直连无在途才解锁中继 ----
    // 不设窗口的边界三档（与老 Go 版 `relayOK` 同档）：无直连候选 / 无中继候选 / 窗口 = 0。
    let window = direct_window(budget, direct_first);
    let (direct, relay): (Vec<usize>, Vec<usize>) =
        (0..cands.len()).partition(|i| !cands[*i].via.is_relay());
    let staged = !direct.is_empty() && !relay.is_empty() && !window.is_zero();
    let start: Vec<usize> = if staged {
        direct
    } else {
        (0..cands.len()).collect()
    };
    let mut staged_relay: Vec<usize> = if staged { relay } else { Vec::new() };

    // 并行发起（`Connecting` 自带端点引用 ⇒ 可搬进任务；`Endpoint` 本身不可 Clone）。
    let mut set: JoinSet<(usize, Result<Connection, quinn::ConnectionError>)> = JoinSet::new();
    let mut refused: Vec<usize> = Vec::new();
    let mut pending_direct: Vec<usize> = Vec::new();
    for &i in &start {
        match endpoint.connect_with(cfg.clone(), cands[i].addr, server_name()) {
            Ok(connecting) => {
                if !cands[i].via.is_relay() {
                    pending_direct.push(i);
                }
                set.spawn(async move { (i, connecting.await) });
            }
            // 调用面拒绝（端点停止/地址族不符）：不入两清单（见下），不作整轮失败
            Err(_e) => refused.push(i),
        }
    }

    let mut completed: Vec<usize> = Vec::new();
    // 未投出（窗口未解锁的中继候选 / 调用面拒绝）**不入**两清单——与「调用面拒绝」同档
    // （两清单的并集 = 本轮实际投出的候选，见模块头第 4 条）
    let mut unfinished: Vec<usize> = (0..cands.len())
        .filter(|i| !refused.contains(i) && !staged_relay.contains(i))
        .collect();
    let mut winner: Option<(usize, Connection)> = None;
    let deadline = TokioInstant::now() + budget;
    let mut unlock_at = staged.then(|| TokioInstant::now() + window);
    loop {
        // 解锁判定（每轮顶部重估）：窗口到点，或直连已无在途（全失败/调用面拒绝）
        if !staged_relay.is_empty() {
            let due = unlock_at.is_some_and(|t| TokioInstant::now() >= t);
            if due || pending_direct.is_empty() {
                let n = staged_relay.len();
                for &i in &staged_relay {
                    match endpoint.connect_with(cfg.clone(), cands[i].addr, server_name()) {
                        Ok(connecting) => {
                            set.spawn(async move { (i, connecting.await) });
                            // 投出即入「未完成」清单（完成/失败时再摘除）——与起跑侧同记法，
                            // 否则「投出但没完成」的中继会在结算行的未完成清单里消失
                            unfinished.push(i);
                        }
                        Err(_e) => refused.push(i),
                    }
                }
                (*logf)(&format!(
                    "赛跑：{} → 解锁中继候选 {} 个",
                    if due {
                        format!("直连窗口 {} 到点仍无胜者", fmt_dur(window))
                    } else {
                        "直连候选全部失败".to_owned()
                    },
                    n
                ));
                staged_relay.clear();
                unlock_at = None;
            }
        }
        let joined = tokio::select! {
            j = set.join_next() => match j { Some(v) => v, None => break },
            // 唤醒点：窗口在途 ⇒ 到点回顶部走解锁判定；否则 ⇒ 整轮预算到点（收工）
            () = tokio::time::sleep_until(unlock_at.unwrap_or(deadline)) => {
                if unlock_at.is_some() { continue } else { break }
            }
        };
        match joined {
            Ok((i, Ok(conn))) => {
                completed.push(i);
                unfinished.retain(|x| *x != i);
                pending_direct.retain(|x| *x != i);
                winner = Some((i, conn));
                break;
            }
            // 握手失败（对端 RST/协议错/超时前定音）：留「未完成」清单
            Ok((i, Err(_e))) => {
                unfinished.retain(|x| *x != i);
                pending_direct.retain(|x| *x != i);
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
            "赛跑小结：无胜者（候选 {} 个，耗时 {}）；未完成={}",
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
        "赛跑结算：胜出 {} {}（候选 {} 个，耗时 {}）；完成={}；未完成={}",
        via.text(),
        ep,
        cands.len(),
        fmt_dur(elapsed),
        done_list.join("、"),
        miss_list.join("、"),
    ));
    (*logf)(&format!(
        "路径确立：{} {}（首个完成握手）",
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
