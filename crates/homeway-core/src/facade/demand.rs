//! 需求信号与待发包下推判据（语义真源 `baseline:clientcore/cmd/clientcore/demand.go`
//! 纯函数面；openspec demand-driven-recovery）。
//!
//! 背景（2026-09-23 弹窗事故）：熄屏挂起期里巡检把「本地 EPERM 禁发」与「零流量需求期
//! 的对端无响应」当失败证据累积。本模块把「需求」变成核内一等信号：
//! - 计证据档（每巡检拍）：本拍 App 出站包数 > 0，或新鲜的**亮屏**位（前台位仅诊断）；
//! - 触发档（下推器）：App 出站新鲜 + 对端接收静默 → 立即下推阶梯（不等巡检拍）；
//! - 本地发送错误（EPERM 等）恒为环境噪声：不触发换源。

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// [`std::sync::Mutex`] 的锁中毒不 panic 扩展（评审 r2-L3：facade 存量 expect 收敛
/// ——c-shared 宿主里 panic = 扩展进程死；tun_shared::lock_unpoison 的 trait 化件）。
trait LockUnpoison<T> {
    fn lup(&self) -> std::sync::MutexGuard<'_, T>;
}

impl<T> LockUnpoison<T> for std::sync::Mutex<T> {
    fn lup(&self) -> std::sync::MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|e| e.into_inner())
    }
}


/// 扩展下发的亮屏/前台位的新鲜期（> 泵节拍 5s × 若干抖动，<< 巡检 60s×2）。
pub const ACTIVITY_FRESHNESS: Duration = Duration::from_secs(90);

/// App 出站包新鲜窗——「有包在等」的口径（下推器 1s 节拍检查）。
pub const OUTBOUND_FRESH: Duration = Duration::from_secs(5);
/// 接收静默阈值：已采纳路径上太久没收到对端任何包 = 隧道大概率死了
/// （与旧 handshakeheal 机制的历史取值同源；健康会话 rekey 120s）。
pub const RECV_STALE: Duration = Duration::from_secs(90);
/// 下推限频：一轮 R1（最坏 ~13s）+ 60s 冷却内不重复。
pub const PUSH_COOLDOWN: Duration = Duration::from_secs(60);

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
        let mut a = self.activity.lup();
        a.fg = fg;
        a.screen = screen;
        a.pushed_at = Some(Instant::now());
    }

    /// （值, 新鲜）。**值 = 亮屏位**（fg 不参与合成）。新鲜 = 距上次推送未超过
    /// `ACTIVITY_FRESHNESS`——挂起空窗后旧值自然过期，唤醒拍扩展先推新值、巡检后评估。
    pub fn activity_signal(&self, now: Instant) -> (bool, bool) {
        let a = self.activity.lup();
        match a.pushed_at {
            None => (false, false),
            Some(at) => (a.screen, now.duration_since(at) < ACTIVITY_FRESHNESS),
        }
    }

    /// fg 诊断位（demand 段的 fg 键）。
    pub fn fg(&self) -> bool {
        self.activity.lup().fg
    }

    /// 记录一拍的需求判定（巡检循环每拍调用；下推器不写）。
    pub fn note_demand(&self, active: bool, reason: &str, at_ms: i64) {
        *self.last.lup() = DemandState {
            active,
            reason: reason.to_owned(),
            at_ms,
        };
    }

    pub fn last(&self) -> DemandState {
        self.last.lup().clone()
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

/// 待发包下推判据（Go shouldPush 纯函数，表驱动单测钉语义）：
/// - 出站新鲜窗内没有出站 ⇒ 没有包在等；
/// - 链路最近（`RECV_STALE` 内）有回包 ⇒ 不构成「过不去」；
/// - 采纳路径本地错误新鲜（挂起禁发）⇒ 换源无效，等环境恢复；
/// - 以上都过 ⇒ 受 `PUSH_COOLDOWN` 限频；
/// - `recv_at = None` = 从未收到对端包，按静默的极端形态处理（软失败世代）。
pub fn should_push(
    out_at: Option<Instant>,
    recv_at: Option<Instant>,
    now: Instant,
    has_fresh_local_err: bool,
    since_last_push: Option<Duration>,
) -> bool {
    let Some(out) = out_at else { return false };
    if now.duration_since(out) > OUTBOUND_FRESH {
        return false; // 没有出站在等
    }
    if let Some(recv) = recv_at {
        if now.duration_since(recv) < RECV_STALE {
            return false; // 链路最近有回包：不构成「过不去」
        }
    }
    if has_fresh_local_err {
        return false; // 采纳路径本地错误（挂起禁发）：换源无效
    }
    match since_last_push {
        None => true, // 从未推过
        Some(d) => d >= PUSH_COOLDOWN,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Go shouldPush 判据链的表驱动对照（demand.go 注释口径）。
    #[test]
    fn should_push_matrix() {
        let now = Instant::now();
        let s = Duration::from_secs(1);
        // 没有出站在等（无出站 / 过期）
        assert!(!should_push(None, None, now, false, None));
        assert!(!should_push(Some(now - OUTBOUND_FRESH - s), None, now, false, None));
        // 有出站在等 + 从未收包（静默极端）+ 无本地错 + 未推过 ⇒ 推
        assert!(should_push(Some(now - s), None, now, false, None));
        // 链路最近有回包：不推
        assert!(!should_push(Some(now - s), Some(now - s), now, false, None));
        // 回包静默超阈值：推
        assert!(should_push(Some(now - s), Some(now - RECV_STALE - s), now, false, None));
        // 本地错误新鲜：不推（环境噪声）
        assert!(!should_push(Some(now - s), Some(now - RECV_STALE - s), now, true, None));
        // 限频：冷却内不推，到点推
        assert!(!should_push(Some(now - s), None, now, false, Some(PUSH_COOLDOWN - s)));
        assert!(should_push(Some(now - s), None, now, false, Some(PUSH_COOLDOWN)));
    }

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
