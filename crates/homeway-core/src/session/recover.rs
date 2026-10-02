//! 统一恢复阶梯（语义真源 `baseline:clientcore/hostsession/recover.go`）。
//!
//! R1 重握手（保采纳）→ R2 换源（保采纳）→ R3 重赛跑（清采纳，学习缓存候选兜底）；
//! 每档**探测先行**（活着则零动作）、失败自动升级、成功即止。只能向上补齐（直入高档
//! 先把低档动作补上）。单飞 + 执行中触发合并（后来者等当前轮、共享 rc，不抬档）。
//!
//! 预算（Go var 同义，测试可缩短）：先行探测 3s（会话活着时一个 RTT 内必答）、
//! 动作 2s（本地动作 = 引擎 RPC，超时按 -3 收轮——把「本地栈卡住」与「包发出去没
//! 回音」分开归因）、验证 10s（巡检同宽，容纳 WG 首发丢包后的 5s 重发）。
//!
//! ⚠️ 探测是契约的一半：Rebind/Rearm 只动本地状态，少了验证探测等于把「本地换了
//! socket」谎报成「对端可达」（Go 旧 tunRebindWait 的教训）。
//!
//! Rust 形态（反直译，评审中-17）：`LadderRc` enum 替代裸 int 返回码、动作面收敛为
//! `apply(Action)` + `refresh_reg()`（bool 子形态显式化）、进度用 `BTreeSet<Level>`；
//! i32 只在 CLI/NAPI 边界转换。

use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::go_fmt::fmt_duration_go_ms;

/// 三段预算（Go recover.go:75-79 var 同值；测试经 LadderDeps 注入缩短）。
pub const PRE_PROBE: Duration = Duration::from_secs(3);
pub const VERIFY: Duration = Duration::from_secs(10);
/// 动作预算（EngineTransport 的 RPC recv_timeout；动作快操作，仅防引擎卡死）。
pub const ACTION: Duration = Duration::from_secs(2);

/// 阶梯档位（数字越大动作越重；跨 CLI/NAPI 面传 int，显式判别值）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// 重握手（补注册 + 丢会话，保采纳）。
    R1 = 1,
    /// 换源（R1 动作 + 换本地 socket，保采纳）。
    R2 = 2,
    /// 重赛跑（R2 动作 + 清采纳，学习缓存候选兜底）。
    R3 = 3,
}

impl Level {
    /// 把外部输入（NAPI/CLI int）钳到合法档位（Go clampRecoverLevel 同义）。
    pub fn clamp(from: i64) -> Level {
        match from {
            ..=1 => Level::R1,
            2 => Level::R2,
            _ => Level::R3,
        }
    }

    /// 档位名（判据行用，Go recoverLevelName 同串）。
    pub fn name(self) -> &'static str {
        match self {
            Level::R1 => "R1 重握手",
            Level::R2 => "R2 换源",
            Level::R3 => "R3 重赛跑",
        }
    }
}

/// 一轮阶梯的终态（Go 裸 int 返回码的 enum 承载；边界转换见 `as_rc`）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LadderRc {
    /// 某档探测通过（Go 0；携带命中档）。
    Recovered(Level),
    /// 入口防陈旧命中（世代已换代，本轮没跑——Go 的「直接 return 0」形态；
    /// 不做记账、不打判据行。评审低-4）。
    Stale,
    /// 走完 R3 仍有界探测失败——交上层整会话重建（Go -1）。
    Exhausted,
    /// 本地动作超时（挂起期/低功耗/路由表空；Go -3）。
    ActionTimeout,
    /// 本地动作立即失败（Go -4）。
    ActionFailed(String),
}

impl LadderRc {
    /// CLI/NAPI 边界的 int 返回码（Go 返回码契约）。
    pub fn as_rc(&self) -> i32 {
        match self {
            LadderRc::Recovered(_) | LadderRc::Stale => 0,
            LadderRc::Exhausted => -1,
            LadderRc::ActionTimeout => -3,
            LadderRc::ActionFailed(_) => -4,
        }
    }
}

/// 档位动作（Go recoverTransport 的方法集收敛；测试假实现只需实现 trait）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Action {
    /// 丢弃本地 WG 会话（保采纳）。
    ResetPeerSession,
    /// 换本地 socket（保采纳）。
    Rebind,
    /// 清采纳、重赛跑（学习缓存候选兜底）。
    Rearm,
}

/// 动作失败的两类（Go errActionTimeout 哨兵 + 立即失败的归因分离）。
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ActionError {
    #[error("本地动作超时")]
    Timeout,
    #[error("{0}")]
    Failed(String),
}

/// 补注册结果（Go RefreshReg bool 的显式形态）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshRegOutcome {
    Sent,
    /// 未发出（bind 已收工或无采纳地址）——现场证据，不是失败；阶梯照常走验证。
    Skipped,
}

/// 阶梯动作面（生产 = Session 的引擎 RPC 包装；单测注入假实现）。
pub trait LadderTransport {
    /// 有界执行一个档位动作（预算由实现侧持有——引擎 RPC 的 recv_timeout）。
    fn apply(&mut self, a: Action) -> Result<(), ActionError>;
    /// 补注册（R1 档动作的第一步）。
    fn refresh_reg(&mut self) -> Result<RefreshRegOutcome, ActionError>;
    /// 该路径完成一次真实往返（探测先行通过时调；中继采纳路径实现侧自行跳过）。
    fn note_path_alive(&mut self);
}

/// 阶梯依赖面（probe = 有界存活探测，true = 对端可达）。
pub struct LadderDeps<'a> {
    pub probe: &'a mut dyn FnMut(Duration) -> bool,
    pub tr: &'a mut dyn LadderTransport,
    pub logf: &'a dyn Fn(&str),
    /// 先行/验证两段预算（测试缩短用；动作预算在动作面实现侧）。
    pub pre_probe: Duration,
    pub verify: Duration,
}

/// 阶梯本体（Go runRecoverLadder 逐行对齐）。
pub fn run_ladder(deps: &mut LadderDeps, from: Level, cause: &str) -> LadderRc {
    let started = Instant::now();
    // 本轮已执行过的档（档位推进只做增量；直入高档时把低档动作补齐）。
    let mut did: BTreeSet<Level> = BTreeSet::new();
    for lvl in [Level::R1, Level::R2, Level::R3] {
        if lvl < from {
            continue;
        }
        // 探测先行：已恢复（或上一档动作的应答迟到了一拍）则不再加码。复探通过时
        // 归因到上一档（Go P2-1：否则把治愈记到起查头上）。
        if (deps.probe)(deps.pre_probe) {
            deps.tr.note_path_alive();
            let elapsed = fmt_duration_go_ms(started.elapsed());
            if lvl > from {
                (deps.logf)(&format!(
                    "RECOVER {} 动作生效（{} 复探通过——上一发验证探测只是丢包，原因={cause}，起跑={}，耗时 {elapsed}）",
                    prev_level(lvl).name(),
                    lvl.name(),
                    from.name(),
                ));
                return LadderRc::Recovered(prev_level(lvl));
            }
            (deps.logf)(&format!(
                "RECOVER 已恢复（{} 起查，原因={cause}，零档位动作，耗时 {elapsed}）",
                from.name(),
            ));
            return LadderRc::Recovered(from);
        }
        // R1 动作（补注册 + 丢会话；所有 ≥R1 的档都先补齐它）
        if !did.contains(&Level::R1) {
            did.insert(Level::R1);
            if let Err(rc) = run_r1_actions(deps, cause) {
                return rc;
            }
        }
        // R2 动作（换本地 socket）
        if lvl >= Level::R2 && !did.contains(&Level::R2) {
            did.insert(Level::R2);
            if let Err(e) = deps.tr.apply(Action::Rebind) {
                return local_fail(deps, Level::R2, cause, &e);
            }
            (deps.logf)(&format!("RECOVER R2 换源（原因={cause}）：换本地 socket（保采纳）"));
        }
        // R3 动作（清采纳重赛跑）
        if lvl >= Level::R3 && !did.contains(&Level::R3) {
            did.insert(Level::R3);
            if let Err(e) = deps.tr.apply(Action::Rearm) {
                return local_fail(deps, Level::R3, cause, &e);
            }
            (deps.logf)(&format!("RECOVER R3 重赛跑（原因={cause}）：清采纳，学习缓存候选兜底"));
        }
        // 动作后验证探测
        if (deps.probe)(deps.verify) {
            (deps.logf)(&format!(
                "RECOVER 恢复于 {}（原因={cause}，起跑={}，耗时 {}）",
                lvl.name(),
                from.name(),
                fmt_duration_go_ms(started.elapsed()),
            ));
            return LadderRc::Recovered(lvl);
        }
    }
    (deps.logf)(&format!(
        "RECOVER 走完 R1→R3 仍未恢复（起跑={}，原因={cause}，耗时 {}）—— 交上层升级",
        from.name(),
        fmt_duration_go_ms(started.elapsed()),
    ));
    LadderRc::Exhausted
}

/// R1 档动作：RefreshReg + ResetPeerSession（Go :163-182；两步都在动作预算内，
/// 任何一步的**超时/立即失败**收轮——Rust 单体重建无「移除失败/回写失败」子态，
/// 「丢会话失败」行不可达〔形态豁免〕）。
fn run_r1_actions(deps: &mut LadderDeps, cause: &str) -> Result<(), LadderRc> {
    match deps.tr.refresh_reg() {
        Ok(RefreshRegOutcome::Sent) => {}
        Ok(RefreshRegOutcome::Skipped) => {
            (deps.logf)(&format!(
                "RECOVER R1 重握手（原因={cause}）：补注册未发出（bind 已收工或无采纳地址）"
            ));
        }
        Err(e) => return Err(local_fail(deps, Level::R1, cause, &e)),
    }
    if let Err(e) = deps.tr.apply(Action::ResetPeerSession) {
        return Err(local_fail(deps, Level::R1, cause, &e));
    }
    (deps.logf)(&format!(
        "RECOVER R1 重握手（原因={cause}）：补注册 + 丢会话（保采纳）"
    ));
    Ok(())
}

fn local_fail(deps: &LadderDeps, lvl: Level, cause: &str, e: &ActionError) -> LadderRc {
    let rc = match e {
        ActionError::Timeout => LadderRc::ActionTimeout,
        ActionError::Failed(m) => LadderRc::ActionFailed(m.clone()),
    };
    (deps.logf)(&format!(
        "RECOVER {} 本地动作失败（原因={cause}，rc={}）：{e}",
        lvl.name(),
        rc.as_rc(),
    ));
    rc
}

fn prev_level(l: Level) -> Level {
    match l {
        Level::R1 | Level::R2 => Level::R1,
        Level::R3 => Level::R2,
    }
}

// ---------- 单飞与触发合并（Go recoverGate；per-round rc + panic 兜底，评审中-9/10） ----------

struct Round {
    /// None = 执行中；Some(rc) = 已完成（结果随轮次对象走——等待方读到的必然是
    /// 自己等的那一轮的 rc，不会被紧接的下一轮覆写）。
    state: Mutex<Option<LadderRc>>,
    cv: Condvar,
}

impl Round {
    fn wait(&self) -> LadderRc {
        let mut st = self.state.lock().expect("gate 锁中毒");
        loop {
            if let Some(rc) = st.clone() {
                return rc;
            }
            st = self.cv.wait(st).expect("gate 锁中毒");
        }
    }

    fn publish(&self, rc: LadderRc) {
        let mut st = self.state.lock().expect("gate 锁中毒");
        if st.is_none() {
            *st = Some(rc);
        }
        self.cv.notify_all();
    }
}

/// 恢复闸：同一时刻只跑一轮阶梯；执行中的再次触发**合并**（等当前轮、共享结果、
/// 不抬档——阶梯本就从起跑档自动升到 R3，先到的轮次失败自然覆盖后来者要的重档）。
/// 按 Session 实例隔离（服务域单闸）。
#[derive(Default)]
pub struct RecoverGate {
    cur: Mutex<Option<Arc<Round>>>,
}

enum GateSlot {
    /// 有轮在执行——等它。
    Wait(Arc<Round>),
    /// 本调用成为执行者（已置位）。
    Execute(Arc<Round>),
}

impl RecoverGate {
    pub fn new() -> Self {
        Self::default()
    }

    fn enter(&self) -> GateSlot {
        let mut cur = self.cur.lock().expect("gate 锁中毒");
        if let Some(round) = cur.clone() {
            return GateSlot::Wait(round);
        }
        let round = Arc::new(Round {
            state: Mutex::new(None),
            cv: Condvar::new(),
        });
        *cur = Some(Arc::clone(&round));
        GateSlot::Execute(round)
    }

    fn leave(&self, round: &Arc<Round>) {
        let mut cur = self.cur.lock().expect("gate 锁中毒");
        if cur.as_ref().is_some_and(|r| Arc::ptr_eq(r, round)) {
            *cur = None;
        }
    }

    /// 单飞入口：执行中触发 → 等待共享结果；否则起新一轮。
    /// 执行者**不持闸锁跑阶梯**（锁内只做置位/发布——绝不持锁跨 45s 阶梯，R1 纪律 3）；
    /// 执行侧 panic 由 RunGuard 兜底发布失败态（不让后来者死等）。
    pub fn merge<F>(&self, from: Level, run: F) -> LadderRc
    where
        F: FnOnce(Level) -> LadderRc,
    {
        match self.enter() {
            GateSlot::Wait(round) => round.wait(),
            GateSlot::Execute(round) => {
                let mut guard = RunGuard {
                    round: Arc::clone(&round),
                    gate: self,
                    completed: false,
                };
                let rc = run(from);
                guard.complete(rc.clone());
                rc
            }
        }
    }
}

/// 执行侧 RAII：正常路径 complete() 发布；panic 路径 Drop 发布失败态并清闸
/// （Rust 线程 panic 只死一条线程——不兜底会让后来者死等）。
struct RunGuard<'a> {
    round: Arc<Round>,
    gate: &'a RecoverGate,
    completed: bool,
}

impl RunGuard<'_> {
    fn complete(&mut self, rc: LadderRc) {
        // 先清闸再发布（Go 同一临界区语义：夹缝里进来的新触发起新轮，不会吞——评审低-3）
        self.gate.leave(&self.round);
        self.round.publish(rc);
        self.completed = true;
    }
}

impl Drop for RunGuard<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.round.publish(LadderRc::ActionFailed(
                "阶梯执行线程异常终止（panic 兜底）".into(),
            ));
            self.gate.leave(&self.round);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 收集型 logf。
    fn log_sink() -> (crate::Logf, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(move |s: &str| {
                let _ = tx.send(s.to_owned());
            }),
            rx,
        )
    }

    /// 假动作面：按脚本应答 + 记录动作序列。
    #[derive(Default)]
    struct FakeTransport {
        actions: Vec<Action>,
        refreshed: bool,
        refresh_skipped: bool,
        fail_rebind: Option<ActionError>,
        alive_marked: u32,
    }

    impl LadderTransport for FakeTransport {
        fn apply(&mut self, a: Action) -> Result<(), ActionError> {
            self.actions.push(a);
            match a {
                Action::Rebind => self.fail_rebind.clone().map_or(Ok(()), Err),
                _ => Ok(()),
            }
        }
        fn refresh_reg(&mut self) -> Result<RefreshRegOutcome, ActionError> {
            self.refreshed = true;
            Ok(if self.refresh_skipped {
                RefreshRegOutcome::Skipped
            } else {
                RefreshRegOutcome::Sent
            })
        }
        fn note_path_alive(&mut self) {
            self.alive_marked += 1;
        }
    }

    fn deps<'a>(
        probe: &'a mut dyn FnMut(Duration) -> bool,
        tr: &'a mut FakeTransport,
        logf: &'a dyn Fn(&str),
    ) -> LadderDeps<'a> {
        LadderDeps {
            probe,
            tr,
            logf,
            pre_probe: Duration::from_millis(1),
            verify: Duration::from_millis(1),
        }
    }

    /// 零档位恢复：起查档探测即通——无动作、行同串、落 NotePathAlive。
    #[test]
    fn recovered_at_start_with_zero_action() {
        let (logf, logs) = log_sink();
        let mut tr = FakeTransport::default();
        let mut probe = |_: Duration| true;
        let rc = run_ladder(&mut deps(&mut probe, &mut tr, logf.as_ref()), Level::R2, "拨号失败");
        assert_eq!(rc, LadderRc::Recovered(Level::R2));
        assert!(!tr.refreshed && tr.actions.is_empty(), "探测先行 ⇒ 零动作");
        assert_eq!(tr.alive_marked, 1, "探测通过落 NotePathAlive");
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER 已恢复（R2 换源 起查，原因=拨号失败，零档位动作，耗时 "
            )),
            "{all:?}"
        );
    }

    /// R2 起跑 → 先补齐 R1 动作 → 验证通过：行序列 + 归因。
    #[test]
    fn r2_start_pads_r1_actions_then_verifies() {
        let (logf, logs) = log_sink();
        let mut tr = FakeTransport::default();
        // 先行探测败、R1+R2 动作后验证通
        let mut n = 0u32;
        let mut probe = move |_: Duration| {
            n += 1;
            n > 1
        };
        let rc = run_ladder(
            &mut deps(&mut probe, &mut tr, logf.as_ref()),
            Level::R2,
            "巡检连续失败",
        );
        assert_eq!(rc, LadderRc::Recovered(Level::R2));
        assert!(tr.refreshed, "R1 补注册");
        assert_eq!(tr.actions, vec![Action::ResetPeerSession, Action::Rebind]);
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER R1 重握手（原因=巡检连续失败）：补注册 + 丢会话（保采纳）"
            )),
            "{all:?}"
        );
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER R2 换源（原因=巡检连续失败）：换本地 socket（保采纳）"
            )),
            "{all:?}"
        );
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER 恢复于 R2 换源（原因=巡检连续失败，起跑=R2 换源，耗时 "
            )),
            "{all:?}"
        );
    }

    /// R1 命中：起查 R1、动作后验证通过。
    #[test]
    fn r1_hit_after_action() {
        let (logf, logs) = log_sink();
        let mut tr = FakeTransport::default();
        let mut n = 0u32;
        let mut probe = move |_: Duration| {
            n += 1;
            n > 1
        };
        let rc = run_ladder(&mut deps(&mut probe, &mut tr, logf.as_ref()), Level::R1, "巡检失败");
        assert_eq!(rc, LadderRc::Recovered(Level::R1));
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER 恢复于 R1 重握手（原因=巡检失败，起跑=R1 重握手，耗时 "
            )),
            "{all:?}"
        );
    }

    /// 走完 R3：全档动作 + 耗尽行 + Exhausted。
    #[test]
    fn exhausted_runs_all_levels() {
        let (logf, logs) = log_sink();
        let mut tr = FakeTransport::default();
        let mut probe = |_: Duration| false;
        let rc = run_ladder(&mut deps(&mut probe, &mut tr, logf.as_ref()), Level::R1, "挂起唤醒");
        assert_eq!(rc, LadderRc::Exhausted);
        assert_eq!(
            tr.actions,
            vec![Action::ResetPeerSession, Action::Rebind, Action::Rearm]
        );
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER 走完 R1→R3 仍未恢复（起跑=R1 重握手，原因=挂起唤醒，耗时 "
            ) && l.ends_with("）—— 交上层升级")),
            "{all:?}"
        );
    }

    /// 复探通过归因到上一档（动作生效行）。
    #[test]
    fn reprobe_attributes_to_previous_level() {
        let (logf, logs) = log_sink();
        let mut tr = FakeTransport::default();
        // 先行（R1）败 → R1 动作 → 验证败 → R2 先行通 ⇒ R1 动作生效
        let mut n = 0u32;
        let mut probe = move |_: Duration| {
            n += 1;
            n >= 3
        };
        let rc = run_ladder(&mut deps(&mut probe, &mut tr, logf.as_ref()), Level::R1, "拨号失败");
        assert_eq!(rc, LadderRc::Recovered(Level::R1), "归因到上一档（R1）");
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER R1 重握手 动作生效（R2 换源 复探通过——上一发验证探测只是丢包，原因=拨号失败，起跑=R1 重握手，耗时 "
            )),
            "{all:?}"
        );
    }

    /// R1 补注册未发出（bind 无采纳）：一行现场证据，动作照常完成。
    #[test]
    fn refresh_reg_skipped_logs_evidence() {
        let (logf, logs) = log_sink();
        let mut tr = FakeTransport {
            refresh_skipped: true,
            ..Default::default()
        };
        let mut n = 0u32;
        let mut probe = move |_: Duration| {
            n += 1;
            n > 1
        };
        let rc = run_ladder(&mut deps(&mut probe, &mut tr, logf.as_ref()), Level::R1, "巡检失败");
        assert_eq!(rc, LadderRc::Recovered(Level::R1));
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER R1 重握手（原因=巡检失败）：补注册未发出（bind 已收工或无采纳地址）"
            )),
            "{all:?}"
        );
    }

    /// 动作超时/立即失败 → rc=-3/-4 + 行。
    #[test]
    fn action_timeout_and_fail_codes() {
        let (logf, logs) = log_sink();
        let mut tr = FakeTransport {
            fail_rebind: Some(ActionError::Timeout),
            ..Default::default()
        };
        let mut probe = |_: Duration| false;
        let rc = run_ladder(&mut deps(&mut probe, &mut tr, logf.as_ref()), Level::R2, "拨号失败");
        assert_eq!(rc, LadderRc::ActionTimeout);
        assert_eq!(rc.as_rc(), -3);
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER R2 换源 本地动作失败（原因=拨号失败，rc=-3）：本地动作超时"
            )),
            "{all:?}"
        );

        let (logf, logs) = log_sink();
        let mut tr = FakeTransport {
            fail_rebind: Some(ActionError::Failed("socket 起不来".into())),
            ..Default::default()
        };
        let rc = run_ladder(&mut deps(&mut probe, &mut tr, logf.as_ref()), Level::R2, "拨号失败");
        assert_eq!(rc, LadderRc::ActionFailed("socket 起不来".into()));
        assert_eq!(rc.as_rc(), -4);
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with(
                "RECOVER R2 换源 本地动作失败（原因=拨号失败，rc=-4）：socket 起不来"
            )),
            "{all:?}"
        );
    }

    /// gate 单飞合并：并发两触发共享一轮 rc；不抬档；闸随后清空。
    #[test]
    fn gate_merges_concurrent_triggers() {
        let gate = Arc::new(RecoverGate::new());
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (done_tx, done_rx) = mpsc::channel::<LadderRc>();
        std::thread::scope(|s| {
            // 第一轮：挂住等 release
            let g1 = Arc::clone(&gate);
            s.spawn(move || {
                g1.merge(Level::R2, |_| {
                    let _ = release_rx.recv();
                    LadderRc::Recovered(Level::R2)
                })
            });
            std::thread::sleep(Duration::from_millis(50)); // 等第一轮置位
            let g2 = Arc::clone(&gate);
            let dt = done_tx;
            let waiter = s.spawn(move || {
                let rc = g2.merge(Level::R3, |_| panic!("合并方不得自跑"));
                let _ = dt.send(rc);
            });
            std::thread::sleep(Duration::from_millis(50));
            let _ = release_tx.send(());
            // waiter 返回的是第一轮的 rc
            assert_eq!(
                done_rx.recv_timeout(Duration::from_secs(2)).unwrap(),
                LadderRc::Recovered(Level::R2)
            );
            waiter.join().unwrap();
        });
        // 闸已清空：下一轮自己跑
        let rc = gate.merge(Level::R1, |_| LadderRc::Exhausted);
        assert_eq!(rc, LadderRc::Exhausted);
    }

    /// 执行者 panic：RunGuard 兜底发布失败态，后来者不死等。
    #[test]
    fn gate_panic_publishes_failure() {
        let gate = RecoverGate::new();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            gate.merge(Level::R1, |_| panic!("阶梯炸了"));
        }));
        assert!(result.is_err());
        // 闸已清 + 失败态可读
        let rc = gate.merge(Level::R2, LadderRc::Recovered);
        assert_eq!(rc, LadderRc::Recovered(Level::R2), "panic 后闸必须可用");
    }
}
