//! QUIC 档**恢复阶梯**（M3 §3.1/§3.2 的唯一实现面）：**快探 → 复探 → M/R → B**。
//!
//! 与 WG 档的关系（§3.4）：**替代而非并存**——WG 档保留 `session/recover` 的 R1/R2/R3
//! （C11 族行逐字不变），QUIC 档**不再产生 C11 族行**，其恢复时间线由本模块的 C18 族承担。
//!
//! 三动作（§3.1 的表；本模块只出**判定**，「做什么」由宿主执行）：
//!
//! | 动作 | 触发 | 宿主落点 |
//! |---|---|---|
//! | **M 迁移** | 复探失败 **且本机发送面新鲜报错** | `Face::rebind`（换本地 socket，保连接） |
//! | **R 重连** | 复探失败 且本机发送面**无**错 | 新 QUIC 连接（同端点/同本地 socket）+ 四帧准入 |
//! | **B 世代重建** | **连续 2 次 R 失败 且 累计失败窗 ≥10s** | `Step::Rebuild` ⇒ 宿主上报 `patrol`（交世代层） |
//!
//! 触发链（§3.1 的动作顺序，逐条）：
//! 1. 快探失败 ⇒ **不动连接**，本拍内用**加倍预算**复探一次（700ms → 1.4s）；
//! 2. 复探**成功** ⇒ 判「路径抖动」，记行 + 计数，不进任何动作（**连续 3 次抖动 ⇒ 升格为失败**，
//!    防 fail-silent）；复探**失败** ⇒ 按上表选 M 或 R（**复探两次之后必有动作**——修掉原稿
//!    「无限不动」的活性洞）；
//! 3. 动作后**快探确认**：成 ⇒ 归零 + 行；败 ⇒ **另一动作**（M↔R）；两动作都败 ⇒ 计入 B 的门。
//!
//! `migration_unconfirmed`（§3.1 末段）**升为动作前置条件**：M 之后**一个快探预算内**无回显
//! ⇒ 置位（窗口从 60s 巡检拍收窄到 ≤1 个快探预算）⇒ **允许走 R**。字段与 N-b 行文不变，
//! 语义登记（S7）。
//!
//! 为什么在本文件（岛内）而不是世代层：§3.2-1 点名「拍间 ≤300ms **由 runtime 拍内务驱动**」，
//! 且 M/R 判别的信号源（`SockStats` 的发送面错误）与动作（`Face::rebind`/`Client::connect`）
//! 全在岛内；世代层只接收 **B** 的上报（`patron` 分类，既有面）。
//!
//! **本文件属异步面**（`tools/check-quic-isolation.sh` 第 ② 条的 `ASYNC_FILES`：快探预算的
//! 超时语义走 `tokio::time`，`start_paused` 用例要它）。

use std::time::Duration;

use tokio::time::Instant as TokioInstant;

use crate::cmd::{IslandErr, Logf};
use crate::tuning::ProbeTuning;

/// 三动作里的两个「就地」动作（B 由 [`Step::Rebuild`] 交世代层；§3.1 的表）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Action {
    /// M：换本地 socket（保连接）。
    Migrate,
    /// R：新 QUIC 连接（同端点、同本地 socket）。
    Reconnect,
}

impl Action {
    /// 快照/行文用短词。
    pub(crate) const fn text(self) -> &'static str {
        match self {
            Action::Migrate => "migrate",
            Action::Reconnect => "reconnect",
        }
    }

    /// 动作选词（中文；判据行用）。
    const fn zh(self) -> &'static str {
        match self {
            Action::Migrate => "M（换本地 socket）",
            Action::Reconnect => "R（新 QUIC 连接）",
        }
    }

    /// M↔R 翻转（§3.1-3「败 ⇒ 另一动作」）。
    const fn flip(self) -> Self {
        match self {
            Action::Migrate => Action::Reconnect,
            Action::Reconnect => Action::Migrate,
        }
    }
}

/// 一轮探活的用途（预算与计分口径都不同）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Round {
    /// 首探（在用档拍间 = [`ProbeTuning::fast_gap`]；待机档 = `idle_interval`）。
    First,
    /// 复探（预算 = 首探 × `reprobe_factor`）。
    Reprobe,
    /// 动作确认（§3.1-3；预算 = 首探）。
    Confirm,
}

/// 发送面读数（M/R 判别的输入；由宿主从 `Face::send_err_view(tuning.send_err_fresh)` 取）。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SendFace {
    /// 白名单命中且落在新鲜度窗内 ⇒ M 判据成立（§3.1-N5）。
    pub fresh: bool,
    /// 末次白名单错误的 errno（落纸；`None` = 无/已随 rebind 清零）。
    pub errno: Option<i32>,
}

/// 阶梯要宿主执行的一步。
///
/// **每一步都是「可独立执行 + 结论回灌」的**：宿主执行完必须回灌结论
/// （探活 ⇒ [`Ladder::on_round`]；M ⇒ [`Ladder::on_migrate_result`]；
/// R ⇒ [`Ladder::on_reconnect_result`]），否则阶梯会停在单飞态。
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum Step {
    /// 无动作（等下一拍）。
    Idle,
    /// 起一次探活（`budget` 已按轮次算好）。
    Probe {
        /// 轮次（首探/复探/动作确认）。
        round: Round,
        /// 本次预算。
        budget: Duration,
    },
    /// M：换本地 socket（宿主执行 `Face::rebind(None)` 后回灌结论）。
    Migrate {
        /// 触发原因（探活失败面）。
        why: String,
    },
    /// R：起新连接（宿主起连接任务后回灌结论）。
    Reconnect {
        /// 触发原因。
        why: String,
        /// 本步是否承「迁移未确认」而来（宿主据此记 N-b 行 + 置 `migration_unconfirmed`）。
        after_migration_unconfirmed: bool,
    },
    /// B：世代重建（宿主记行 + 上报 `patrol` 交世代层）。
    Rebuild {
        /// 触发原因。
        why: String,
        /// 连续 R 失败次数。
        r_fails: u32,
    },
}

/// 一次在途动作的记账（确认探活要认领它）。
#[derive(Clone, Debug)]
struct Pending {
    action: Action,
    why: String,
    at: TokioInstant,
}

/// 快探阶梯（§3.1/§3.2；岛线程独占——无锁）。
pub(crate) struct Ladder {
    tun: ProbeTuning,
    /// 阶梯起始时刻（**待机档首探的节拍基准**；§3.2-2：待机档 `PATROL_INTERVAL` 一拍）。
    ///
    /// 代码门 r18 ②-3 的修法：原实现把首拍判据写成 `in_use`（`last_round_at == None` 时
    /// 只有在用档才探）⇒ **从未有出站流量的岛永不探首拍**，「待机档 60s 巡检」实质不存在。
    started_at: TokioInstant,
    /// 上一拍（挂起空窗检测；§3.2-3）。
    last_tick: TokioInstant,
    /// 上一轮探活的开始时刻（节拍基准）。
    last_round_at: Option<TokioInstant>,
    /// 单飞位（探活/动作在途 ⇒ 不再起新轮；§3.2-4）。
    inflight: bool,
    /// 首探失败的原因（等复探结论判「抖动 vs 故障」）。
    pending_fail: Option<String>,
    /// 连续抖动次数（§3.1-2：≥ `jitter_streak` ⇒ 升格为失败）。
    jitter_streak: u32,
    /// 连续失败次数（本失败轮；成功即清零）。
    fail_streak: u32,
    /// 下一次要做的动作（失败轮开始时按发送面信号选；失败后 M↔R 翻转）。
    next_action: Action,
    /// 在途动作（确认探活认领它）。
    pending: Option<Pending>,
    /// 连续 R 失败次数（B 门的两个输入之一）。
    r_fail_streak: u32,
    /// 本轮 R 失败链的**首次**失败时刻（B 门的窗基准）。
    r_fail_since: Option<TokioInstant>,
    /// 本轮 R 尝试次数（判据行的「第 %d 次」）。
    r_attempts: u32,
    /// B 已上报（阶梯休眠：同一世代不重复上报；新连接采纳时 `rearm`）。
    dormant: bool,
    /// 累计成功快探次数（快照读数；e2e 的 T_recv 观测位）。
    pub(crate) probe_ok: u64,
    /// 最近一次动作（`""` = 未发生；快照读数）。
    pub(crate) last_action: &'static str,
    /// 失败行节流计数（首 3 + 每 100）。
    fail_gate: u64,
    /// 抖动行节流计数（同上）。
    jitter_gate: u64,
}

impl Ladder {
    /// 起阶梯（参数 = 已施 env 覆盖的生效值）。
    pub(crate) fn new(tun: ProbeTuning) -> Self {
        Self {
            tun,
            started_at: TokioInstant::now(),
            last_tick: TokioInstant::now(),
            last_round_at: None,
            inflight: false,
            pending_fail: None,
            jitter_streak: 0,
            fail_streak: 0,
            next_action: Action::Reconnect,
            pending: None,
            r_fail_streak: 0,
            r_fail_since: None,
            r_attempts: 0,
            dormant: false,
            probe_ok: 0,
            last_action: "",
            fail_gate: 0,
            jitter_gate: 0,
        }
    }

    /// 生效参数（宿主算复探/重连预算与在用窗时读同一份）。
    pub(crate) fn tuning(&self) -> &ProbeTuning {
        &self.tun
    }

    /// 连续失败次数（快照读数）。
    pub(crate) fn fail_streak(&self) -> u32 {
        self.fail_streak
    }

    /// 连续抖动次数（快照读数）。
    pub(crate) fn jitter_streak(&self) -> u32 {
        self.jitter_streak
    }

    /// 复探预算（§3.1-1：首探 × `reprobe_factor`）。
    pub(crate) fn reprobe_budget(&self) -> Duration {
        self.tun.fast_budget.saturating_mul(self.tun.reprobe_factor)
    }

    /// R 动作的**连接预算**：两倍复探预算（= 4× 首探预算，缺省 2.8s）。
    ///
    /// 为什么不是「首探预算」量级：R 要完成握手 + 四帧准入（§3.3 实测 268ms），预算是**上界**
    /// 而非期望值；对端在停机窗口里时（`kill -9` 相位）由 quinn 的 Initial 重传跨越窗口
    /// ——预算过短会让「出口刚回来」的那次 R 在握手完成前被掐断（T_recv 相位最坏值）。
    pub(crate) fn reconnect_budget(&self) -> Duration {
        self.reprobe_budget().saturating_mul(2)
    }

    /// B 已上报后**重新武装**（新连接采纳 ⇒ 新失败链重新计数；§3.1 的「归零」）。
    pub(crate) fn rearm(&mut self) {
        self.dormant = false;
        self.pending = None;
        self.inflight = false;
        self.pending_fail = None;
        self.fail_streak = 0;
        self.jitter_streak = 0;
        self.r_fail_streak = 0;
        self.r_fail_since = None;
        self.r_attempts = 0;
    }

    /// 到点判据（§3.2 的三段节拍）。`in_use` = 在用档（TUN 在位 + 出站新鲜）。
    ///
    /// **挂起空窗**（拍间 > 2×待机节拍）= 进程被冻结过 ⇒ 立即探（§3.2-3 的「主动恢复」；
    /// 其动作由失败链给 M→R，与世代层的「挂起唤醒」行同拍）。
    ///
    /// **首拍**：在用档**立即**探（用户在用时不该空等一个节拍）；待机档自阶梯起算满一个
    /// 60s 节拍即探（§3.2-2 的 `PATROL_INTERVAL` 口径）。**代码门 r18 ②-3 的修法**：
    /// 旧实现写成 `None => in_use`（首拍只在用档才探）⇒「挂上 TUN 但无流量」的岛**永不探首拍**，
    /// 待机档 60s 巡检实质不存在（与该注释声称的行为相反，且是真机待机长尾的成因之一）。
    pub(crate) fn due(&mut self, now: TokioInstant, in_use: bool) -> Option<(Round, Duration)> {
        let gap = now.saturating_duration_since(self.last_tick);
        self.last_tick = now;
        if self.inflight || self.dormant {
            return None;
        }
        if self.last_round_at.is_some() && gap > 2 * self.tun.idle_interval {
            return Some((Round::First, self.tun.fast_budget));
        }
        let wait = if in_use {
            self.tun.fast_gap
        } else {
            self.tun.idle_interval
        };
        let due = match self.last_round_at {
            None => in_use || now.saturating_duration_since(self.started_at) >= wait,
            Some(at) => now.saturating_duration_since(at) >= wait,
        };
        due.then_some((Round::First, self.tun.fast_budget))
    }

    /// 记「本轮探活已下发」（单飞 + 节拍基准）。
    pub(crate) fn note_round(&mut self, round: Round, now: TokioInstant) {
        let _ = round;
        self.inflight = true;
        self.last_round_at = Some(now);
    }

    /// 探活结论（唯一的计分口）。按 §3.1 的动作顺序推进状态机。
    pub(crate) fn on_round(
        &mut self,
        round: Round,
        res: Result<Duration, IslandErr>,
        now: TokioInstant,
        send: SendFace,
        logf: &Logf,
    ) -> Step {
        self.inflight = false;
        match (round, res) {
            // ---- 成功面 ----
            (Round::Reprobe, Ok(_)) => {
                // §3.1-2：复探成功 ⇒ 抖动（记行 + 计数），不进任何动作
                let why = self
                    .pending_fail
                    .take()
                    .unwrap_or_else(|| "（前因已记）".to_owned());
                self.probe_ok += 1;
                self.fail_streak = 0;
                self.jitter_streak = self.jitter_streak.saturating_add(1);
                self.jitter_gate += 1;
                if crate::client::log_due(self.jitter_gate) {
                    (*logf)(&format!("quic: 链路探活抖动（{why}，已复探）"));
                }
                if self.jitter_streak >= self.tun.jitter_streak {
                    // §3.1-③：连续抖动升格为失败（防 fail-silent）
                    let why = format!("连续 {} 次抖动升格", self.jitter_streak);
                    (*logf)(&format!("quic: {why} —— 计入失败链（防 fail-silent）"));
                    return self.fail_step(why, now, send, logf);
                }
                Step::Idle
            }
            (Round::First, Ok(_)) | (Round::Confirm, Ok(_)) => {
                self.probe_ok += 1;
                self.fail_streak = 0;
                self.jitter_streak = 0;
                self.pending_fail = None;
                let done = self.pending.take();
                match done {
                    Some(Pending {
                        action: Action::Reconnect,
                        why,
                        at,
                    }) => {
                        // §3.1-3：动作后快探确认成 ⇒ 归零 + 行
                        self.r_fail_streak = 0;
                        self.r_fail_since = None;
                        self.r_attempts = 0;
                        (*logf)(&format!(
                            "quic: 链路重连完成（原因={why}，耗时 {}）",
                            crate::client::fmt_dur(now.saturating_duration_since(at))
                        ));
                    }
                    // M 的确认由宿主记 N-b 行（它持 from → to）；此处只清记账
                    Some(Pending {
                        action: Action::Migrate,
                        ..
                    })
                    | None => {}
                }
                Step::Idle
            }
            // ---- 失败面 ----
            (Round::First, Err(e)) => {
                let why = err_text(&e);
                self.pending_fail = Some(why);
                Step::Probe {
                    round: Round::Reprobe,
                    budget: self.reprobe_budget(),
                }
            }
            (Round::Reprobe, Err(e)) => {
                let why = self.pending_fail.take().unwrap_or_else(|| err_text(&e));
                self.fail_step(why, now, send, logf)
            }
            (Round::Confirm, Err(e)) => {
                let why = err_text(&e);
                match self.pending.take() {
                    Some(Pending {
                        action: Action::Reconnect,
                        why: cause,
                        ..
                    }) => self.r_failed(format!("{cause}（确认探活：{why}）"), now, logf),
                    Some(Pending {
                        action: Action::Migrate,
                        ..
                    }) => {
                        // §3.1 末段：M 之后一个预算内无回显 ⇒ 置位（宿主记 N-b 行）⇒ 允许走 R
                        self.probe_ok = 0; // 无回显：清零仅为读数一致（不计成功）
                        self.next_action = Action::Reconnect;
                        self.go_action(
                            Action::Reconnect,
                            "迁移未确认".to_owned(),
                            true,
                            now,
                            logf,
                        )
                    }
                    // 无在途动作的 Confirm（理论不可达）：按普通失败收
                    None => self.fail_step(why, now, send, logf),
                }
            }
        }
    }

    /// **链路确定性死亡**（`close_reason` 非空 / 注册刷新写失败）⇒ 直接进动作判别（M/R）。
    ///
    /// 为什么不消耗一个探活预算：§3.1-1 的复探是**假阳性抑制**（路径抖动），而链路确定性
    /// 死亡（协议层已定音：对端关闭/空闲回收）不是抖动——无回显是结构性的，两次瞬时失败
    /// 等价。**活性不放松**：动作仍必出（`fail_step` 只回 M/R/B，绝不回 `Probe`）。
    pub(crate) fn on_link_dead(&mut self, now: TokioInstant, send: SendFace, logf: &Logf) -> Step {
        if self.dormant {
            return Step::Idle; // B 已上报：本世代不再动作（等世代重建/采纳新连接时 rearm）
        }
        if self.inflight {
            return Step::Idle; // 单飞：动作/探活在途，让它自己收线（不并发动作）
        }
        self.pending = None;
        self.pending_fail = Some("连接已断".to_owned());
        self.fail_step("连接已断".to_owned(), now, send, logf)
    }

    /// **确认探活不可执行**（动作已做完，但岛上已无连接可探）⇒ 按「该动作失败」收线。
    ///
    /// **为什么必须单列（代码门 r18 ②-1 的修法）**：[`Self::on_link_dead`] 的 `inflight`
    /// 早退是给「真在途的探活/动作」用的（让它自己收线，不并发动作）；但**动作已完成、
    /// 只是确认轮发不出去**时，早退会让阶梯停在 `inflight=true` 且 `pending` 不清 ⇒
    /// `due()` 恒 `None`、housekeeping 又因 `live == None` 不再命中 ⇒ **永久卡死**
    /// （动作链再无出口，B 门不可达）。可达链：连接确定性死亡 ⇒ 首动作 R 失败 ⇒ 翻转 M
    /// ⇒ `rebind` 成功（换本地 socket 恒成功）⇒ 确认探活无连接可发。
    ///
    /// 语义 = 「**确认不可执行 = 确认失败**」：M 在途 ⇒ 迁移未确认 ⇒ 走 R；
    /// R 在途 ⇒ `r_failed`（进 B 门）。两者都不回 `Probe`（活性：动作仍必出）。
    pub(crate) fn on_confirm_unavailable(
        &mut self,
        now: TokioInstant,
        send: SendFace,
        logf: &Logf,
    ) -> Step {
        match self.pending.take() {
            Some(Pending {
                action: Action::Migrate,
                ..
            }) => {
                self.probe_ok = 0; // 无回显：清零仅为读数一致（不计成功）
                self.next_action = Action::Reconnect;
                self.go_action(
                    Action::Reconnect,
                    "迁移未确认（无连接可确认）".to_owned(),
                    true,
                    now,
                    logf,
                )
            }
            Some(Pending {
                action: Action::Reconnect,
                why,
                ..
            }) => self.r_failed(format!("{why}（无连接可确认）"), now, logf),
            // 无在途动作（动作间隙的连接死）：按普通失败收（不新增面）
            None => self.fail_step("无连接可确认".to_owned(), now, send, logf),
        }
    }

    /// **动作链过长**的收线（代码门 r18 ②-1 的活性安全网）：一次**同步**执行链步数超限 ⇒
    /// 判结构性问题（可构造：候选清单为空 × M/R 轮转，两者都在同步路径上，若不收线会变成
    /// 岛线程自旋）⇒ 按 B 收（置休眠 + 保 `inflight=false`），宿主走既有 `Step::Rebuild`
    /// 分支（上报不健康交世代层）。
    pub(crate) fn force_rebuild(&mut self, steps: u32) -> Step {
        self.dormant = true;
        self.inflight = false;
        self.pending = None;
        self.last_action = "rebuild";
        Step::Rebuild {
            why: format!("动作链过长（同步 {steps} 步，连接/候选面结构性异常）"),
            r_fails: self.r_fail_streak,
        }
    }

    /// M（Rebind）的结论回灌（宿主执行完 `Face::rebind` 后调）。
    ///
    /// ⚠️ 成败两分支的记账差别：**成功 ⇒ 保留 `pending`**（确认探活要认领它，从而分辨
    /// 「迁移已确认 / 未确认」）；失败 ⇒ 转 R（`pending` 换手）。
    pub(crate) fn on_migrate_result(&mut self, ok: bool, at: TokioInstant, logf: &Logf) -> Step {
        if ok {
            self.inflight = true; // 确认探活在途（禁止并发起新轮）
            return Step::Probe {
                round: Round::Confirm,
                budget: self.tun.fast_budget,
            };
        }
        let why = self
            .pending
            .take()
            .map(|p| p.why)
            .unwrap_or_else(|| "（无在途动作）".to_owned());
        (*logf)("quic: 换本地 socket 失败 —— 转 R（新 QUIC 连接）");
        self.next_action = Action::Reconnect;
        self.go_action(Action::Reconnect, format!("{why}（换绑失败）"), false, at, logf)
    }

    /// R（新连接）的结论回灌（宿主起完连接任务后调）。
    ///
    /// **耗时口径（代码门 r18 ②-6 的修法）**：成功时**保留** `go_action` 记下的 `pending.at`
    /// （= R 的**发起**时刻），不重设为完成时刻——C18 的 `链路重连完成（原因=…，耗时 %v）`
    /// 要报的是「R 全周期（握手 + 四帧准入 + 确认探活）」，旧实现只报确认探活那一段
    /// （落纸成 `耗时 0s`/`1ms`，排障会读成「重连只要 1ms」）。
    pub(crate) fn on_reconnect_result(&mut self, ok: bool, why: String, now: TokioInstant, logf: &Logf) -> Step {
        if ok {
            self.inflight = true; // 确认探活在途
            match self.pending.as_mut() {
                Some(p) if p.action == Action::Reconnect => p.why = why,
                _ => self._set_pending(Action::Reconnect, why, now),
            }
            Step::Probe {
                round: Round::Confirm,
                budget: self.tun.fast_budget,
            }
        } else {
            self.r_failed(why, now, logf)
        }
    }

    /// 失败步（§3.1-3：复探失败 ⇒ 选 M 或 R）。
    fn fail_step(&mut self, why: String, now: TokioInstant, send: SendFace, logf: &Logf) -> Step {
        self.fail_streak = self.fail_streak.saturating_add(1);
        self.fail_gate += 1;
        if crate::client::log_due(self.fail_gate) {
            (*logf)(&format!(
                "quic: 链路快探失败（连续 {}，原因={why}）",
                self.fail_streak
            ));
        }
        // M/R 判别（§3.1 的表）：**本机发送面新鲜报错 ⇒ M；否则 R**。只在失败轮起点选
        // （`fail_streak == 1`）——其后按「败 ⇒ 另一动作」翻转，不受信号反复影响。
        if self.fail_streak == 1 {
            let chosen = if send.fresh {
                Action::Migrate
            } else {
                Action::Reconnect
            };
            self.next_action = chosen;
            let detail = match (chosen, send.errno) {
                (Action::Migrate, Some(e)) => format!("本机发送面报错（errno={e}）"),
                (Action::Migrate, None) => "本机发送面报错".to_owned(),
                (Action::Reconnect, _) => "本机发送面无错".to_owned(),
            };
            (*logf)(&format!(
                "quic: 链路动作选 {}（原因={why}；{detail}）",
                chosen.zh()
            ));
        }
        let act = self.next_action;
        self.go_action(act, why, false, now, logf)
    }

    /// 下发一个动作（含 R 的行/记账；M 的记账由确认探活认领）。
    fn go_action(
        &mut self,
        act: Action,
        why: String,
        after_migration_unconfirmed: bool,
        now: TokioInstant,
        logf: &Logf,
    ) -> Step {
        self.last_action = act.text();
        self.inflight = true;
        match act {
            Action::Migrate => {
                self._set_pending(Action::Migrate, why.clone(), now);
                Step::Migrate { why }
            }
            Action::Reconnect => {
                self.r_attempts = self.r_attempts.saturating_add(1);
                if self.r_fail_since.is_none() {
                    self.r_fail_since = Some(now);
                }
                (*logf)(&format!(
                    "quic: 链路重连中（原因={why}，第 {} 次）",
                    self.r_attempts
                ));
                self._set_pending(Action::Reconnect, why.clone(), now);
                Step::Reconnect {
                    why,
                    after_migration_unconfirmed,
                }
            }
        }
    }

    /// R 失败（§3.1：败 ⇒ 另一动作；连续 2 次 + 窗 ≥10s ⇒ B）。
    fn r_failed(&mut self, why: String, now: TokioInstant, logf: &Logf) -> Step {
        self.r_fail_streak = self.r_fail_streak.saturating_add(1);
        let since = *self.r_fail_since.get_or_insert(now);
        (*logf)(&format!(
            "quic: 链路重连失败（原因={why}，第 {} 次）—— 交世代重建",
            self.r_fail_streak
        ));
        let window = now.saturating_duration_since(since);
        if self.r_fail_streak >= self.tun.reconnect_streak && window >= self.tun.rebuild_window {
            self.dormant = true;
            self.inflight = false;
            self.pending = None;
            self.last_action = "rebuild";
            (*logf)(&format!(
                "quic: 世代重建（原因={why}；连续重连失败 {}）",
                self.r_fail_streak
            ));
            return Step::Rebuild {
                why,
                r_fails: self.r_fail_streak,
            };
        }
        // 另一动作（M↔R）：R 失败 ⇒ M
        (*logf)(&format!(
            "quic: 交另一动作（M↔R；窗 {} / 门 {} 次未到）",
            crate::client::fmt_dur(window),
            self.tun.reconnect_streak
        ));
        self.next_action = Action::Reconnect.flip();
        let act = self.next_action;
        self.go_action(act, why, false, now, logf)
    }

    fn _set_pending(&mut self, action: Action, why: String, at: TokioInstant) {
        self.pending = Some(Pending { action, why, at });
    }
}

/// 探活失败的原因文案（判据行的 `%s`；typed 面不许字符串错误，这里只做**渲染**）。
pub(crate) fn err_text(e: &IslandErr) -> String {
    match e {
        IslandErr::ProbeNoResponse => "探活无回显".to_owned(),
        IslandErr::ConnectionLost => "连接已断".to_owned(),
        IslandErr::NotConnected => "无连接".to_owned(),
        other => other.to_string(),
    }
}

/// **一轮探活的预算实现**（唯一：`tokio::time::timeout(budget, f)`；到点 ⇒
/// [`IslandErr::ProbeNoResponse`]）。
///
/// 为什么在这里而不是调用点：§3.2 的「预算」是判据面的量（`T_detect` 由它算出），
/// 必须**单源**且可被 `start_paused` 虚拟时钟确定性断言（`client::probe` 内部还有一层
/// 流面期限——两层都到点即失败，口径一致）。
pub(crate) async fn run_round<F, Fut>(budget: Duration, f: F) -> Result<Duration, IslandErr>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<Duration, IslandErr>>,
{
    match tokio::time::timeout(budget, f()).await {
        Ok(r) => r,
        Err(_) => Err(IslandErr::ProbeNoResponse),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn tuning() -> ProbeTuning {
        ProbeTuning::design()
    }

    /// 记行口（用例收集行；节流口径与生产同源）。
    fn lines() -> (Logf, Arc<Mutex<Vec<String>>>) {
        let v = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&v);
        let f: Logf = Arc::new(move |l: &str| sink.lock().unwrap().push(l.to_owned()));
        (f, v)
    }

    fn ok() -> Result<Duration, IslandErr> {
        Ok(Duration::from_millis(1))
    }

    fn failed() -> Result<Duration, IslandErr> {
        Err(IslandErr::ProbeNoResponse)
    }

    /// **判据（节拍三段，§3.2）**：在用档 = 背靠背（拍间 `fast_gap`，≤300ms）；待机档 = 60s；
    /// 挂起空窗（> 2×待机节拍）⇒ 立即探。
    #[test]
    fn cadence_is_tiered_and_suspension_wakes_it() {
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        // 首拍：立即（无历史）
        assert!(l.due(t0, true).is_some(), "首拍立即探");
        l.note_round(Round::First, t0);
        l.on_round(Round::First, ok(), t0, SendFace::default(), &lines().0);

        // 在用档：拍间 < fast_gap ⇒ 未到点；= fast_gap ⇒ 到点
        assert!(l.due(t0 + Duration::from_millis(100), true).is_none());
        let due = l.due(t0 + Duration::from_millis(250), true).expect("到点");
        assert_eq!(due.0, Round::First);
        assert_eq!(due.1, Duration::from_millis(700), "首探预算 = §13-T2 的承重值");
        l.note_round(due.0, t0 + Duration::from_millis(250));
        l.on_round(Round::First, ok(), t0 + Duration::from_millis(250), SendFace::default(), &lines().0);

        // 待机档：250ms 不到点、59s 不到点、60s 到点
        let base = t0 + Duration::from_millis(250);
        assert!(l.due(base + Duration::from_millis(250), false).is_none(), "待机档不背靠背");
        assert!(l.due(base + Duration::from_secs(59), false).is_none(), "待机档 60s 节拍");
        assert!(l.due(base + Duration::from_secs(60), false).is_some(), "待机档到点");

        // 挂起空窗：拍间 > 2×待机节拍 ⇒ 立即探（不等节拍）
        let mut l2 = Ladder::new(tuning());
        let t1 = TokioInstant::now();
        let _ = l2.due(t1, true);
        l2.note_round(Round::First, t1);
        l2.on_round(Round::First, ok(), t1, SendFace::default(), &lines().0);
        assert!(
            l2.due(t1 + Duration::from_secs(121), false).is_some(),
            "挂起空窗（>2×60s）⇒ 主动恢复探"
        );
    }

    /// **判据（复探，§3.1-1/2）**：首探失败 ⇒ 同一失败链里用**加倍预算**复探；
    /// 复探成功 ⇒ 抖动行 + 不进动作；连 3 次抖动 ⇒ 升格为失败。
    #[test]
    fn reprobe_uses_doubled_budget_and_jitter_escalates() {
        let (logf, log) = lines();
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        l.note_round(Round::First, t0);
        let step = l.on_round(Round::First, failed(), t0, SendFace::default(), &logf);
        match step {
            Step::Probe { round, budget } => {
                assert_eq!(round, Round::Reprobe);
                assert_eq!(budget, Duration::from_millis(1400), "加倍预算（700 × 2）");
            }
            other => panic!("首探失败应复探，实得 {other:?}"),
        }
        // 复探成功 ⇒ 抖动行 + 无动作
        l.note_round(Round::Reprobe, t0);
        let step = l.on_round(Round::Reprobe, ok(), t0, SendFace::default(), &logf);
        assert_eq!(step, Step::Idle, "抖动不进动作");
        assert!(log.lock().unwrap().iter().any(|l| l.contains("链路探活抖动")), "{:?}", log.lock().unwrap());
        assert_eq!(l.jitter_streak(), 1);

        // 连 3 次抖动 ⇒ 升格为失败（走 M/R 动作链）
        for i in 2..=3 {
            let t = t0 + Duration::from_secs(i);
            l.note_round(Round::First, t);
            let s = l.on_round(Round::First, failed(), t, SendFace::default(), &logf);
            assert!(matches!(s, Step::Probe { round: Round::Reprobe, .. }), "第 {i} 轮复探");
            l.note_round(Round::Reprobe, t);
            let s = l.on_round(Round::Reprobe, ok(), t, SendFace::default(), &logf);
            if i < 3 {
                assert_eq!(s, Step::Idle, "第 {i} 次抖动仍不动");
            } else {
                assert!(
                    matches!(s, Step::Reconnect { .. } | Step::Migrate { .. }),
                    "第 3 次抖动升格为失败（实得 {s:?}）"
                );
            }
        }
        assert!(log.lock().unwrap().iter().any(|l| l.contains("抖动升格")), "升格须记行");
    }

    /// **判据（M/R 判别，§3.1-N5）**：发送面新鲜报错 ⇒ **M**；无错 ⇒ **R**；
    /// 动作失败后按「另一动作」翻转（M↔R）。
    #[test]
    fn action_selection_follows_send_face_and_flips_on_failure() {
        let (logf, log) = lines();
        // ① 无发送面错误 ⇒ R
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        l.note_round(Round::First, t0);
        let _ = l.on_round(Round::First, failed(), t0, SendFace::default(), &logf);
        l.note_round(Round::Reprobe, t0);
        let s = l.on_round(Round::Reprobe, failed(), t0, SendFace::default(), &logf);
        assert!(matches!(s, Step::Reconnect { .. }), "无发送面错 ⇒ R（实得 {s:?}）");
        assert!(log.lock().unwrap().iter().any(|l| l.contains("动作选 R")), "{:?}", log.lock().unwrap());

        // ② 发送面新鲜报错（白名单 errno）⇒ M
        let (logf2, log2) = lines();
        let mut l2 = Ladder::new(tuning());
        l2.note_round(Round::First, t0);
        let _ = l2.on_round(Round::First, failed(), t0, SendFace::default(), &logf2);
        l2.note_round(Round::Reprobe, t0);
        let s = l2.on_round(
            Round::Reprobe,
            failed(),
            t0,
            SendFace { fresh: true, errno: Some(51) }, // ENETUNREACH
            &logf2,
        );
        assert!(matches!(s, Step::Migrate { .. }), "发送面报错 ⇒ M（实得 {s:?}）");
        assert!(
            log2.lock().unwrap().iter().any(|l| l.contains("errno=51")),
            "errno 落纸：{:?}",
            log2.lock().unwrap()
        );

        // ③ M 的确认探活失败 ⇒ 置位「迁移未确认」⇒ 立即走 R（§3.1 末段）
        l2.note_round(Round::Confirm, t0);
        let s = l2.on_round(
            Round::Confirm,
            failed(),
            t0 + Duration::from_millis(700),
            SendFace::default(),
            &logf2,
        );
        match s {
            Step::Reconnect {
                after_migration_unconfirmed,
                ..
            } => assert!(after_migration_unconfirmed, "该 R 承「迁移未确认」而来"),
            other => panic!("M 未确认 ⇒ R，实得 {other:?}"),
        }

        // ④ R 失败 ⇒ 另一动作 M（翻转）
        let s = l2.on_reconnect_result(false, "对端不可达".to_owned(), t0, &logf2);
        assert!(matches!(s, Step::Migrate { .. }), "R 失败 ⇒ M（实得 {s:?}）");
    }

    /// **判据（待机档首探，§3.2-2；代码门 r18 ②-3 的修法）**：从未有出站流量（`in_use=false`）
    /// 的岛，自阶梯起算满一个待机节拍（60s）**必须**探首拍；未到点不探。
    /// 旧实现（`None => in_use`）下这一支恒 `None` ⇒ 待机档 60s 巡检不存在。
    #[test]
    fn standby_first_probe_fires_after_one_idle_interval() {
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        assert!(l.due(t0 + Duration::from_secs(59), false).is_none(), "未满一拍不探");
        let due = l
            .due(t0 + Duration::from_secs(60), false)
            .expect("待机档满 60s 首探（§3.2-2）");
        assert_eq!(due.0, Round::First);
        assert_eq!(due.1, Duration::from_millis(700));
        // 在用档仍**立即**探（首拍不空等）
        let mut l2 = Ladder::new(tuning());
        let t1 = TokioInstant::now();
        assert!(l2.due(t1, true).is_some(), "在用档首拍立即探");
        // 待机档按 60s 续拍（首探完成后进入节拍面）
        l.note_round(Round::First, t0 + Duration::from_secs(60));
        let _ = l.on_round(
            Round::First,
            ok(),
            t0 + Duration::from_secs(60),
            SendFace::default(),
            &lines().0,
        );
        assert!(
            l.due(t0 + Duration::from_secs(119), false).is_none(),
            "待机档续拍仍按 60s"
        );
        assert!(l.due(t0 + Duration::from_secs(120), false).is_some(), "续拍到点");
    }

    /// **判据（确认探活不可执行 ⇒ 按动作失败收线；代码门 r18 ②-1）**：
    /// ① 在途 M（`rebind` 成功但已无连接可确认）⇒ **迁移未确认** ⇒ 走 R（且该 R 承
    ///    「迁移未确认」而来）；
    /// ② 在途 R ⇒ 计 R 失败（进 B 门）；
    /// ③ 无在途动作 ⇒ 按普通失败收（不得静默停在单飞态）。
    #[test]
    fn confirm_unavailable_is_accounted_as_action_failure() {
        let (logf, _log) = lines();
        let t0 = TokioInstant::now();
        // ① M 在途（构造：直接置 pending + inflight，等价于 `on_migrate_result(true)` 之后）
        let mut l = Ladder::new(tuning());
        l.inflight = true;
        l._set_pending(Action::Migrate, "连接已断".to_owned(), t0);
        let s = l.on_confirm_unavailable(
            t0 + Duration::from_millis(700),
            SendFace::default(),
            &logf,
        );
        match s {
            Step::Reconnect {
                after_migration_unconfirmed,
                ..
            } => assert!(after_migration_unconfirmed, "该 R 承「迁移未确认」而来"),
            other => panic!("M 不可确认 ⇒ R，实得 {other:?}"),
        }
        assert!(l.inflight, "R 在途：单飞位须在位");
        assert!(matches!(l.pending.as_ref().map(|p| p.action), Some(Action::Reconnect)));
        // ② R 在途 ⇒ 计 R 失败（此处窗未满 ⇒ 走另一动作，但 r_fail_streak 必须增）
        let s = l.on_confirm_unavailable(t0 + Duration::from_millis(800), SendFace::default(), &logf);
        assert!(
            matches!(s, Step::Migrate { .. } | Step::Rebuild { .. }),
            "R 不可确认 ⇒ 计入 R 失败链（实得 {s:?}）"
        );
        assert_eq!(l.r_fail_streak, 1, "R 失败计数");
        // ③ 无在途动作 ⇒ 普通失败（产物 = 动作，绝不是 Idle）
        let mut l3 = Ladder::new(tuning());
        assert!(!l3.inflight && l3.pending.is_none());
        let s = l3.on_confirm_unavailable(t0, SendFace::default(), &logf);
        assert!(
            matches!(s, Step::Reconnect { .. } | Step::Migrate { .. } | Step::Rebuild { .. }),
            "无在途动作也必须出动作（不得 Idle 静默），实得 {s:?}"
        );
    }

    /// **判据（同步链安全网，代码门 r18 ②-1 的配套）**：`force_rebuild` ⇒ `Step::Rebuild`
    /// + 休眠态（`due` 不再起轮、`on_link_dead` 不再动作）。
    #[test]
    fn chain_guard_forces_rebuild_and_stands_down() {
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        match l.force_rebuild(9) {
            Step::Rebuild { why, .. } => assert!(why.contains("动作链过长"), "{why}"),
            other => panic!("实得 {other:?}"),
        }
        assert!(!l.inflight, "不得留在单飞态");
        assert!(l.due(t0 + Duration::from_secs(1), true).is_none(), "休眠：不再起轮");
        assert_eq!(
            l.on_link_dead(t0, SendFace::default(), &lines().0),
            Step::Idle,
            "休眠期连接死也不再动作（等世代重建/rearm）"
        );
    }

    /// **判据（C18「耗时」口径，代码门 r18 ②-6）**：R 的耗时 = **发起 → 确认成功**
    /// （不是确认探活那一段）；旧实现重设 `pending.at` 为完成时刻 ⇒ 落纸恒 `0s`/`1ms`。
    #[test]
    fn reconnect_elapsed_covers_dispatch_to_confirm() {
        let (logf, log) = lines();
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        // go_action(R) 在 t0 发起（`pending.at = t0`）
        let s = l.go_action(Action::Reconnect, "探活无回显".to_owned(), false, t0, &logf);
        assert!(matches!(s, Step::Reconnect { .. }));
        let at = l.pending.as_ref().expect("R 在途").at;
        // 300ms 后连接完成 ⇒ 确认探活通过（用同一时间线）
        let s = l.on_reconnect_result(true, "探活无回显".to_owned(), at, &logf);
        assert!(matches!(s, Step::Probe { round: Round::Confirm, .. }));
        assert_eq!(l.pending.as_ref().expect("确认在途").at, at, "发起时刻不得被覆盖");
        l.note_round(Round::Confirm, at);
        let _ = l.on_round(Round::Confirm, ok(), at + Duration::from_millis(300), SendFace::default(), &logf);
        assert!(
            log.lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("quic: 链路重连完成（原因=探活无回显，耗时 300ms）")),
            "耗时须覆盖「发起 → 确认成功」：{:?}",
            log.lock().unwrap()
        );
    }

    /// **判据（B 门，§3.1）**：**连续 2 次 R 失败 且 累计失败窗 ≥10s** ⇒ 世代重建；
    /// 窗未到 ⇒ 继续另一动作（不提前重建）。
    #[test]
    fn rebuild_gate_needs_two_reconnect_failures_and_ten_seconds() {
        let (logf, log) = lines();
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        // 第 1 次 R 失败（t0，窗起点）
        let s = l.on_reconnect_result(false, "对端不可达".to_owned(), t0, &logf);
        assert!(matches!(s, Step::Migrate { .. }), "第 1 次失败 ⇒ 另一动作（实得 {s:?}）");
        // M 的确认失败 ⇒ R（第 2 次）；窗仅 ~0s ⇒ 不重建
        l.note_round(Round::Confirm, t0);
        let s = l.on_round(
            Round::Confirm,
            failed(),
            t0 + Duration::from_millis(5),
            SendFace::default(),
            &logf,
        );
        assert!(matches!(s, Step::Reconnect { .. }), "M 未确认 ⇒ R");
        let s = l.on_reconnect_result(
            false,
            "对端不可达".to_owned(),
            t0 + Duration::from_millis(10),
            &logf,
        );
        assert!(
            matches!(s, Step::Migrate { .. }),
            "连续 2 次但窗 10ms < 10s ⇒ 不重建（实得 {s:?}）"
        );
        // 窗满：第 3 次 R 失败发生在 t0+11s ⇒ 开门
        l.note_round(Round::Confirm, t0);
        let t = t0 + Duration::from_secs(11);
        let s = l.on_round(Round::Confirm, failed(), t, SendFace::default(), &logf);
        assert!(matches!(s, Step::Reconnect { .. }), "M 未确认 ⇒ R");
        let s = l.on_reconnect_result(false, "对端不可达".to_owned(), t, &logf);
        match s {
            Step::Rebuild { r_fails, .. } => assert_eq!(r_fails, 3, "连续失败次数入行",),
            other => panic!("窗满 + 连续 ≥2 ⇒ B（实得 {other:?}）"),
        }
        let logged = log.lock().unwrap();
        assert!(logged.iter().any(|l| l.contains("世代重建（原因=")), "{logged:?}");
        assert!(
            logged.iter().any(|l| l.contains("链路重连失败（原因=对端不可达，第 3 次）—— 交世代重建")),
            "C18 行文逐字：{logged:?}"
        );
    }

    /// **判据（单飞，§3.2-4）**：探活/动作在途 ⇒ `due` 不再起新轮（不自打架）。
    #[test]
    fn single_flight_blocks_new_rounds() {
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        assert!(l.due(t0, true).is_some());
        l.note_round(Round::First, t0);
        assert!(l.due(t0 + Duration::from_secs(1), true).is_none(), "在途 ⇒ 不起新轮");
        l.on_round(Round::First, ok(), t0, SendFace::default(), &lines().0);
        assert!(l.due(t0 + Duration::from_secs(2), true).is_some(), "收线后可再起");
    }

    /// **判据（B 之后休眠 + 重新武装，§3.1）**：B 上报后本世代不再重复上报；新连接采纳
    /// （`rearm`）才恢复节拍。
    #[test]
    fn rebuild_is_reported_once_per_generation_until_rearm() {
        let (logf, _l) = lines();
        let t0 = TokioInstant::now();
        // 直接构造「窗满 + 连续 2 失败」：先失败一次，再把窗基准前推（同 crate 用例可见私有字段）
        let mut l = Ladder::new(tuning());
        let s = l.on_reconnect_result(false, "x".to_owned(), t0, &logf);
        assert!(matches!(s, Step::Migrate { .. }));
        l.r_fail_since = Some(t0 - Duration::from_secs(11));
        let s = l.on_reconnect_result(false, "x".to_owned(), t0, &logf);
        assert!(matches!(s, Step::Rebuild { .. }), "实得 {s:?}");
        assert!(l.due(t0 + Duration::from_secs(2), true).is_none(), "休眠：不再起轮");
        l.rearm();
        assert!(l.due(t0 + Duration::from_secs(3), true).is_some(), "重装后恢复节拍");
        assert_eq!(l.fail_streak(), 0);
    }

    /// **判据（C18 行族逐字，§8.2-10；S7 登记的行文以本用例为准）**：六条行文一条不少；
    /// 且**替代关系**成立（§3.4：QUIC 档不产生 C11 族行——本模块零 `RECOVER`）。
    #[test]
    fn c18_line_family_texts_are_pinned_and_replaces_c11() {
        let (logf, log) = lines();
        let mut l = Ladder::new(tuning());
        let t0 = TokioInstant::now();
        // ① 失败（复探失败定音）
        l.note_round(Round::First, t0);
        let _ = l.on_round(Round::First, failed(), t0, SendFace::default(), &logf);
        l.note_round(Round::Reprobe, t0);
        let s = l.on_round(Round::Reprobe, failed(), t0, SendFace::default(), &logf);
        assert!(matches!(s, Step::Reconnect { .. }));
        // ② 抖动（另一条链：首探失败 + 复探成功）
        let mut l2 = Ladder::new(tuning());
        l2.note_round(Round::First, t0);
        let _ = l2.on_round(Round::First, failed(), t0, SendFace::default(), &logf);
        l2.note_round(Round::Reprobe, t0);
        let _ = l2.on_round(Round::Reprobe, ok(), t0, SendFace::default(), &logf);
        // ③ 重连完成（R 起跑 → 确认探活通过）
        l2.note_round(Round::Confirm, t0);
        let _ = l2.on_reconnect_result(true, "探活无回显".to_owned(), t0, &logf);
        l2.note_round(Round::Confirm, t0);
        let _ = l2.on_round(Round::Confirm, ok(), t0, SendFace::default(), &logf);
        // ④ 重连失败（第 1 次）+ ⑤ 世代重建（把窗基准前推以开门）
        let _ = l2.on_reconnect_result(false, "对端不可达".to_owned(), t0, &logf);
        l2.r_fail_since = Some(t0 - Duration::from_secs(11));
        let _ = l2.on_reconnect_result(false, "对端不可达".to_owned(), t0, &logf);

        let logged = log.lock().unwrap();
        let want = [
            "quic: 链路快探失败（连续 1，原因=探活无回显）",
            "quic: 链路探活抖动（探活无回显，已复探）",
            "quic: 链路重连中（原因=探活无回显，第 1 次）",
            "quic: 链路重连完成（原因=探活无回显，",
            "quic: 链路重连失败（原因=对端不可达，第 1 次）—— 交世代重建",
            "quic: 世代重建（原因=对端不可达；连续重连失败 ",
        ];
        for w in want {
            assert!(
                logged.iter().any(|l| l.starts_with(w)),
                "C18 行文缺：{w}\n实得：{logged:?}"
            );
        }
        assert!(
            logged.iter().all(|l| !l.contains("RECOVER")),
            "QUIC 档不得产生 C11 族行（§3.4 的替代关系）：{logged:?}"
        );
    }

    /// **判据（`start_paused` 虚拟时钟，S4 完成判据点名的「预算/节拍/复探/抖动」四分支）**：
    /// 用一段**迷你驱动环**（拍 = 25ms 虚拟时间）跑完整链路——
    /// ①预算到点即失败（桩挂住 ⇒ 恰在预算处收）；②在用档背靠背节拍；③复探预算 = 2×；
    /// ④复探成功 ⇒ 抖动不动作；⑤失败链 ⇒ M/R 动作。
    ///
    /// 虚拟时钟 ⇒ 断言确定（无墙钟抖动；flake 口径照 M0 §9.2②：只判上界与形态）。
    #[tokio::test(start_paused = true)]
    async fn paused_clock_covers_budget_cadence_reprobe_and_jitter() {
        let (logf, log) = lines();
        let t0 = TokioInstant::now();
        // 桩：挂住的探活（永不返回）⇒ 被预算掐断
        let r = run_round(Duration::from_millis(700), || async {
            std::future::pending::<()>().await;
            Ok(Duration::ZERO)
        })
        .await;
        assert!(matches!(r, Err(IslandErr::ProbeNoResponse)), "预算到点即失败（实得 {r:?}）");
        let spent = t0.elapsed();
        assert!(
            spent >= Duration::from_millis(700),
            "不得提前返回（实耗 {spent:?}）"
        );
        assert!(spent < Duration::from_millis(1000), "超时不得越界（实耗 {spent:?}）");

        // 迷你驱动环：探活桩按轮次给结论；记录每轮「虚拟时刻 + 轮次 + 预算」
        let mut l = Ladder::new(tuning());
        let mut seen: Vec<(Duration, Round, Duration)> = Vec::new();
        let mut stub: Vec<Result<Duration, IslandErr>> = vec![
            Ok(Duration::from_millis(1)),  // 首拍成功
            Err(IslandErr::ProbeNoResponse), // 首探失败 ⇒ 复探
            Ok(Duration::from_millis(2)),  // 复探成功 ⇒ 抖动
            Err(IslandErr::ProbeNoResponse), // 首探失败 ⇒ 复探
            Err(IslandErr::ProbeNoResponse), // 复探失败 ⇒ 动作（R）
        ];
        let start = TokioInstant::now();
        let mut steps: Vec<Step> = Vec::new();
        let mut pending: Option<(Round, Duration)> = None;
        while start.elapsed() < Duration::from_secs(5) {
            let now = TokioInstant::now();
            let (round, budget) = match pending.take() {
                Some(p) => p,
                None => match l.due(now, true) {
                    Some(p) => p,
                    None => {
                        tokio::time::sleep(Duration::from_millis(25)).await;
                        continue;
                    }
                },
            };
            l.note_round(round, TokioInstant::now());
            seen.push((start.elapsed(), round, budget));
            let res = if stub.is_empty() {
                Ok(Duration::from_millis(1))
            } else {
                stub.remove(0)
            };
            let step = l.on_round(round, res, TokioInstant::now(), SendFace::default(), &logf);
            match step {
                Step::Probe { round, budget } => pending = Some((round, budget)),
                other => {
                    if !matches!(other, Step::Idle) {
                        steps.push(other);
                    }
                }
            }
            // 让虚拟时钟前进一点（模拟宿主拍；也避免同刻自旋）
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        assert!(seen.len() >= 5, "探活轮次：{seen:?}");
        // ① 节拍：在用档背靠背（相邻首探间隔 ≤ fast_gap + 一拍）
        let firsts: Vec<Duration> = seen
            .iter()
            .filter(|(_, r, _)| *r == Round::First)
            .map(|(at, _, _)| *at)
            .collect();
        assert!(firsts.len() >= 2, "至少两拍首探：{firsts:?}");
        for w in firsts.windows(2) {
            let gap = w[1] - w[0];
            assert!(
                gap <= Duration::from_millis(300),
                "在用档背靠背（拍间 ≤300ms），实得 {gap:?}"
            );
        }
        // ② 预算：首探 = 700ms；复探 = 1400ms（§3.1-1 的加倍）
        for (_, round, budget) in &seen {
            match round {
                Round::First => assert_eq!(*budget, Duration::from_millis(700)),
                Round::Reprobe => assert_eq!(*budget, Duration::from_millis(1400), "复探加倍"),
                Round::Confirm => assert_eq!(*budget, Duration::from_millis(700)),
            }
        }
        // ③ 抖动 + 动作：有抖动行、有重连动作
        let logged = log.lock().unwrap();
        assert!(logged.iter().any(|l| l.contains("链路探活抖动")), "{logged:?}");
        assert!(
            steps.iter().any(|s| matches!(s, Step::Reconnect { .. })),
            "复探两次后必有动作：{steps:?}"
        );
        assert!(
            logged.iter().any(|l| l.contains("链路重连中（原因=")),
            "动作行须落纸：{logged:?}"
        );
    }
}
