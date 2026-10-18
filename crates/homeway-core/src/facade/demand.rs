//! 需求信号面（语义真源 `baseline:clientcore/cmd/clientcore/demand.go`
//! 纯函数面；openspec demand-driven-recovery）。
//!
//! 背景（2026-09-23 弹窗事故）：熄屏挂起期里巡检把「本地 EPERM 禁发」与「零流量需求期
//! 的对端无响应」当失败证据累积。本模块把「需求」变成核内一等信号：
//! - 计证据档（每巡检拍）：本拍 App 出站包数 > 0，或新鲜的**亮屏**位（前台位仅诊断）；
//! - **reconnect-oneshot（2026-10-17）**：demand-push 决策面（`should_push` 族）已随
//!   1s 拍线程退役删除——本模块只剩**诊断记账面**（`DemandSignals`：activity 位 +
//!   最近一拍判定，stats 观测线程消费；恢复驱动 = 事件触发的一次性重连，不读本面）。
//! - 本地发送错误（EPERM 等）恒为环境噪声：不触发换源。

use std::sync::Mutex;
use std::time::{Duration, Instant};



/// 扩展下发的亮屏/前台位的新鲜期。**event-driven-heal §2.5（2026-10-15）起 = 灯塔
/// 量级（约 10 分钟 = 2× 灯塔拍）**：5s 泵退役后亮屏位由灯塔拍（约 5min）与决策点
/// 事件现查补推——90s 新鲜期会让「亮屏但隧道零出站包」（全绕过使用）期几乎恒过期
/// （原「下一拍推送即恢复」口径随泵消失）。挂起期间两侧一起冻结、值停在冻结前，
/// 空窗后按自然过期处理（唤醒后的首个灯塔拍/决策点先推新值再评估——spec 同款）。
/// 真源 = tier openspec/changes/event-driven-heal（wg-native-transport「失败证据的
/// 需求门控」+ tasks 2.5）。
pub const ACTIVITY_FRESHNESS: Duration = Duration::from_secs(600);

// reconnect-oneshot D4（评审 r1 P0-2 整改）：决策面四件已删——
// `OUTBOUND_FRESH`/`RECV_STALE`/`PUSH_COOLDOWN`/`should_push`（含 `should_push_matrix`
// 单测）。它们是 demand-push 1s 拍线程的判据（下推器已随 2026-10-17 事故批整体退役）；
// 现行需求面 = `DemandSignals` 诊断记账（`patrol_demand`/`note_demand`——stats 观测
// 线程消费）+ `activity_signal`。

/// 扩展经 NAPI（ClientCoreTunSetActivity）下发的「App 前台 / 设备亮屏」。
/// 初始为零值（两处都按 false + 陈旧处理——prepare 早期扩展还没来得及推值，保守不算需求）。
#[derive(Debug, Default)]
struct ActivityBits {
    /// 诊断用（需求合成只看 screen——2026-09-23 真机实测：熄屏不触发 App 的
    /// onBackground，「前台」名义残留 true 而用户已看不见任何 UI；fg 位保留仅作诊断）。
    fg: bool,
    screen: bool,
    pushed_at: Option<Instant>,
}

/// 最近一次巡检拍的需求判定结果（状态 JSON 的 demand 段；扩展门控同源消费）。
#[derive(Debug, Clone, Default)]
pub struct DemandState {
    pub active: bool,
    /// 依据（出站包/亮屏/熄屏/熄屏（位陈旧））；未判定过 = 空串（JSON 面兜「未判定」）。
    pub reason: String,
    /// unix 毫秒；0 = 未判定过。
    pub at_ms: i64,
}

/// 需求信号面（ActivityBits + 最近一拍判定；锁内小状态）。
#[derive(Debug, Default)]
pub struct DemandSignals {
    activity: Mutex<ActivityBits>,
    last: Mutex<DemandState>,
}

impl DemandSignals {
    pub fn new() -> Self {
        Self::default()
    }

    /// 记录扩展下发的亮屏/前台诊断位（每拍必发；唤醒拍扩展会立即补一次）。
    pub fn set_activity(&self, fg: bool, screen: bool) {
        let mut a = crate::syncutil::lock_unpoison(&self.activity);
        a.fg = fg;
        a.screen = screen;
        a.pushed_at = Some(Instant::now());
    }

    /// （值, 新鲜）。**值 = 亮屏位**（fg 不参与合成）。新鲜 = 距上次推送未超过
    /// `ACTIVITY_FRESHNESS`——挂起空窗后旧值自然过期，唤醒拍扩展先推新值、巡检后评估。
    pub fn activity_signal(&self, now: Instant) -> (bool, bool) {
        let a = crate::syncutil::lock_unpoison(&self.activity);
        match a.pushed_at {
            None => (false, false),
            Some(at) => (a.screen, now.duration_since(at) < ACTIVITY_FRESHNESS),
        }
    }

    /// fg 诊断位（demand 段的 fg 键）。
    pub fn fg(&self) -> bool {
        crate::syncutil::lock_unpoison(&self.activity).fg
    }

    /// 记录一拍的需求判定（巡检循环每拍调用；下推器不写）。
    pub fn note_demand(&self, active: bool, reason: &str, at_ms: i64) {
        *crate::syncutil::lock_unpoison(&self.last) = DemandState {
            active,
            reason: reason.to_owned(),
            at_ms,
        };
    }

    pub fn last(&self) -> DemandState {
        crate::syncutil::lock_unpoison(&self.last).clone()
    }

    /// 巡检拍的需求合成：本拍 App 出站包数 ‖ 新鲜的亮屏位。
    /// reason 进边沿日志与状态 JSON，排障时「为什么这拍算/不算需求」有据可查。
    pub fn patrol_demand(&self, out_pkts: i64, now: Instant) -> (bool, &'static str) {
        patrol_demand_with(out_pkts, || self.activity_signal(now))
    }
}

/// patrolDemand 的纯函数核（activity 经闭包注入，便于表驱动测试）。
fn patrol_demand_with(out_pkts: i64, signal: impl FnOnce() -> (bool, bool)) -> (bool, &'static str) {
    if out_pkts > 0 {
        return (true, "出站包");
    }
    let (act, fresh) = signal();
    if fresh {
        if act {
            return (true, "亮屏");
        }
        return (false, "熄屏");
    }
    (false, "熄屏（位陈旧）")
}

// （决策面 `should_push` 已随 reconnect-oneshot D4 删除——见文件头常量区的整改注记。）

#[cfg(test)]
mod tests {
    use super::*;

    /// patrolDemand 合成表（出站包优先；亮屏/熄屏/位陈旧三分支）。
    #[test]
    fn patrol_demand_matrix() {
        assert_eq!(patrol_demand_with(3, || (false, false)), (true, "出站包"));
        assert_eq!(patrol_demand_with(0, || (true, true)), (true, "亮屏"));
        assert_eq!(patrol_demand_with(0, || (false, true)), (false, "熄屏"));
        assert_eq!(patrol_demand_with(0, || (false, false)), (false, "熄屏（位陈旧）"));
        // 出站包优先于熄屏
        assert_eq!(patrol_demand_with(1, || (false, true)), (true, "出站包"));
    }

    /// activitySignal 新鲜期与首拍保守语义。
    #[test]
    fn activity_freshness_gate() {
        let d = DemandSignals::new();
        // 未推送：false + 不新鲜（prepare 早期保守不算需求）
        assert_eq!(d.activity_signal(Instant::now()), (false, false));
        d.set_activity(false, true);
        let (v, fresh) = d.activity_signal(Instant::now());
        assert!(v && fresh);
        // 超过新鲜期：不新鲜
        let later = Instant::now() + ACTIVITY_FRESHNESS + Duration::from_secs(1);
        assert_eq!(d.activity_signal(later), (true, false));
        // fg 诊断位独立
        d.set_activity(true, false);
        assert!(d.fg());
        assert_eq!(d.activity_signal(Instant::now()), (false, true));
    }
}
