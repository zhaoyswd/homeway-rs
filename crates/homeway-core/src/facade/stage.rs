//! 阶段机（语义真源 `baseline:clientcore/cmd/clientcore/tunmode.go` 的 stage 族）。
//!
//! tun 生命周期的五阶段 + 机器可读原因码 + 就绪判据 + 世代语义：
//! - `set_if_current`：只有**当前世代**能改写阶段（旧世代晚到的收尾不得把新世代
//!   改写成 idle/failed——Go setStageIfCurrent 同语义）；
//! - 原因码词面：`core`（会话/核类）/ `attach`（数据面类）/ `stopped` /
//!   `attach-timeout`（中止类）/ 空串（正常）；调用方据此映射界面归因，不嗅探 reason 文本。

use std::sync::Mutex;
use std::time::Instant;

/// 隧道生命周期阶段（`tunStageName` 同串）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TunStage {
    #[default]
    Idle,
    Preparing,
    /// 暖机完成，等待 attach。
    Ready,
    /// 已接管数据面。
    Attached,
    Failed,
}

impl TunStage {
    pub fn as_str(self) -> &'static str {
        match self {
            TunStage::Idle => "idle",
            TunStage::Preparing => "preparing",
            TunStage::Ready => "ready",
            TunStage::Attached => "attached",
            TunStage::Failed => "failed",
        }
    }
}

/// 阶段快照（tunStatusJSON 的 state/code/reason/meowed/readyBy/elapsedMs 源）。
/// `since` 默认 = now（Go stageSince 初始化 = time.Now() 同义）。
#[derive(Debug, Clone)]
pub struct StageSnapshot {
    pub stage: TunStage,
    pub code: String,
    pub reason: String,
    pub meowed: bool,
    pub ready_by: String,
    pub since: Instant,
}

impl Default for StageSnapshot {
    fn default() -> Self {
        StageSnapshot {
            stage: TunStage::Idle,
            code: String::new(),
            reason: String::new(),
            meowed: false,
            ready_by: String::new(),
            since: Instant::now(),
        }
    }
}

/// 机内完整状态 = 快照 + 写入世代（世代号不进快照面——它是机内守卫量）。
#[derive(Debug, Default)]
struct Inner {
    snap: StageSnapshot,
    writer_gen: u64,
}

/// 阶段机（锁内小状态；世代号由持有方在世代起点递增后调 `begin_generation` 登记）。
#[derive(Debug)]
pub struct StageMachine {
    inner: Mutex<Inner>,
}

impl Default for StageMachine {
    fn default() -> Self {
        StageMachine { inner: Mutex::new(Inner::default()) }
    }
}

impl StageMachine {
    pub fn new() -> Self {
        Self::default()
    }

    /// 无条件写阶段（Go setStage：随写打 since 时间戳）。
    pub fn set(&self, stage: TunStage, code: &str, reason: &str, meowed: bool) {
        let mut g = self.inner.lock().expect("阶段锁中毒");
        g.snap = StageSnapshot {
            stage,
            code: code.to_owned(),
            reason: reason.to_owned(),
            meowed,
            ready_by: std::mem::take(&mut g.snap.ready_by),
            since: Instant::now(),
        };
    }

    /// 世代守卫写：`gen` 是调用方（世代）自报的世代号，与机内登记的**写入世代**一致才落笔。
    /// Go 的 setStageIfCurrent 经 tunRun 指针身份判「当前」；Rust 面以世代计数等价表达
    /// （世代 = begin 时递增的 u64，同号即同世代）。
    pub fn set_if_current(&self, gen: u64, stage: TunStage, code: &str, reason: &str, meowed: bool) {
        let mut g = self.inner.lock().expect("阶段锁中毒");
        if g.writer_gen != gen {
            return;
        }
        g.snap = StageSnapshot {
            stage,
            code: code.to_owned(),
            reason: reason.to_owned(),
            meowed,
            ready_by: std::mem::take(&mut g.snap.ready_by),
            since: Instant::now(),
        };
    }

    /// 记录最近一次就绪的判据（现仅 "wg"——隧道内暖机探测；随 tunStatusJSON 下发）。
    /// 新世代起点清空（Go runTun2Tailcat 世代起点 setReadyBy("")）。
    pub fn set_ready_by(&self, by: &str) {
        self.inner.lock().expect("阶段锁中毒").snap.ready_by = by.to_owned();
    }

    pub fn snapshot(&self) -> StageSnapshot {
        self.inner.lock().expect("阶段锁中毒").snap.clone()
    }

    /// 世代起点的写入权交接（begin 时调）：旧世代此后的 set_if_current 全部失效。
    pub fn begin_generation(&self, gen: u64) {
        self.inner.lock().expect("阶段锁中毒").writer_gen = gen;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_names() {
        assert_eq!(TunStage::Idle.as_str(), "idle");
        assert_eq!(TunStage::Preparing.as_str(), "preparing");
        assert_eq!(TunStage::Ready.as_str(), "ready");
        assert_eq!(TunStage::Attached.as_str(), "attached");
        assert_eq!(TunStage::Failed.as_str(), "failed");
    }

    /// 世代守卫：旧世代晚到的收尾不得改写新世代（Go「旧世代退出不得再改 stage」）。
    #[test]
    fn generation_guard() {
        let m = StageMachine::new();
        m.begin_generation(1);
        m.set_if_current(1, TunStage::Preparing, "", "", false);
        // 世代 2 begin（新世代接管写入权）
        m.begin_generation(2);
        m.set_if_current(2, TunStage::Ready, "", "", true);
        // 旧世代 1 的迟到收尾：不得把新世代改回 idle
        m.set_if_current(1, TunStage::Idle, "", "", false);
        assert_eq!(m.snapshot().stage, TunStage::Ready);
        // 当前世代照常可写
        m.set_if_current(2, TunStage::Attached, "", "", true);
        assert_eq!(m.snapshot().stage, TunStage::Attached);
    }

    /// set（无条件面）不受世代守卫限制——收工路径的终态写（failed 保留）走这里。
    #[test]
    fn unconditional_set_ignores_generation() {
        let m = StageMachine::new();
        m.begin_generation(7);
        m.set(TunStage::Failed, "core", "硬失败", false);
        assert_eq!(m.snapshot().stage, TunStage::Failed);
        assert_eq!(m.snapshot().code, "core");
    }

    /// readyBy 不随阶段写清空（只随世代起点显式清）——Go 侧 setStage 不动 stageReadyBy。
    #[test]
    fn ready_by_survives_stage_writes() {
        let m = StageMachine::new();
        m.set_ready_by("wg");
        m.set(TunStage::Ready, "", "", true);
        assert_eq!(m.snapshot().ready_by, "wg");
        m.set_ready_by("");
        assert_eq!(m.snapshot().ready_by, "");
    }
}
