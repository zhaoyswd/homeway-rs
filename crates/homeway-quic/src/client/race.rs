//! 赛跑（设计 §2.2）：并行 `connect` 到候选（LAN / 公网 / 中继），**首个完成握手者胜**、
//! 余者 drop；`via/ep/rtt` 交宿主入快照；行文 = C4'/C5'/C6'。
//!
//! 三处语义（逐条对着设计写）：
//!
//! 1. **谁赢**：首个从 `Connecting` 转为已建连（1-RTT 可用）者。QUIC 握手完成即双向可用
//!    （无 WG 的「响应过/首包」两段语义）。
//! 2. **何时算输**：其余候选 `drop`。⚠️ quinn 的 drop **会**触发 `implicit_close()`
//!    （最高可用密钥空间发 `APPLICATION_ERROR` CONNECTION_CLOSE）——所以口径是「输家被
//!    主动关闭、岛不等待关闭完成」；对**已建立但未胜出**的连接另行显式 `close()`
//!    （§2.2 的现任裁决）。
//! 3. **预算**：`budget` = 整轮预算（不是每候选）；到点未完成者按 drop 收；全候选失败
//!    ⇒ [`IslandErr::NoCandidate`]。
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
    // 余者：在途握手 → drop（quinn 侧 abort/关连接）；已完成的**显式关闭**（§2.2 现任裁决）
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

    // ---- 登记（§2.6）：本连接的 exporter + 首条 bidi 控制流 + 准入窗 ----
    let exporter = register::exporter_of(&conn)?;
    let (send, recv) =
        register::register_on_control_stream(&conn, &cred, &exporter, logf, ep, via.is_relay())
            .await?;
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
