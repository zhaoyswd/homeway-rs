//! tun 域世代共享面（语义真源 `baseline:clientcore/cmd/clientcore/probe_lib.go` 的
//! probeRunning/tunHealthy/tunUnhealthyWhy + `tunmode.go` 的 tunRun 通道族）。
//!
//! 为什么从 mod.rs 拆出（工单①世代生命周期）：warmup 改「启动即返」后，世代线程
//! 由执行体（`TunExecutor` 实现）持有，但收尾动作（终态 + 放锁 + 健康位）必须过
//! 世代守卫且与 facade 面共享——把这一小块状态打成一个 `Arc`，warmup 时交给
//! 执行体；世代号（Go 的 tunRun 指针身份）等价表达守卫。
//!
//! 契约（Go finish/runTun2Tailcat defer 的合成，**执行体的世代线程必须在一切
//! 退出路径上调 `finish_generation`**，否则单飞锁永不释放）：
//! - 非 failed 终态回 idle（空码空因——Go defer 同义：stopped/attach-timeout 码
//!   只在收尾完成前的窗口可读，收尾后统一归零）；
//! - **failed 终态保留**（调用方要读原因）；
//! - 健康位收尾置 false + 分类 "stop"（Go finish 同义；写序契约：先 why 后
//!   healthy——读方先见 healthy=false 时 reason 必已就位）；
//! - 只有当前世代才有权清共享状态（旧世代晚到的收尾不得动新世代）。

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::stage::{StageMachine, TunStage};

/// 锁中毒不 panic（Q-F F6-1：单源上移到 `crate::syncutil`——facade 面 /
/// recover gate 与本处共用同一件；facade 内的既有调用点经本重导出零改动）。
pub(crate) use crate::syncutil::lock_unpoison;

/// 不健康分类的设备/传输分界（tun-death-gate）：`patrol` = 传输类（对端不可达——
/// 不写设备死位、可翻回）；`fd`/`panic`/`stop` 与**任何未知值** = 设备类（单向，
/// 仅新世代 begin 清——未知值保守归设备侧）。
const DEVICE_CLASS: fn(&str) -> bool = |why| why != "patrol";

/// tun 域的世代共享面（Arc 交给执行体）。
pub struct TunShared {
    /// 单飞锁（probeRunning）：世代在世期间持有，收尾放。
    pub probe_running: AtomicBool,
    /// 世代计数（每次 prepare 递增；阶段写与收尾都比对它防陈旧）。
    pub gen: AtomicU64,
    pub stage: StageMachine,
    /// 健康位（传输面，tun-death-gate 拆分后）：`patrol` 写假、翻回写真——不再
    /// 承载设备死亡（设备死看 [`Self::device_dead`]）。
    pub healthy: AtomicBool,
    /// 设备死位（fd/panic/stop + 未知分类；单向——仅新世代 [`Self::begin_healthy`]
    /// 清）。`running()` 只看它：传输类不健康不再把 running 拉零（2026-10-15 事故
    /// 的误杀链入口）。
    pub device_dead: AtomicBool,
    /// 不健康分类（patrol/fd/panic/stop；写序：先写它再翻 healthy/设备位）。
    pub unhealthy_why: Mutex<String>,
    /// 最近一次 patrol 判定时刻（翻回最小间隔的记账面；仅 patrol 分类写入）。
    last_patrol_at: Mutex<Option<Instant>>,
    /// attach fd 通道（缓冲 1；世代线程接收端经 [`Self::attach_receiver`] 注册——
    /// Go attachCh。投递端非阻塞：满/无接收者即 false（调用方按 -1 收）。
    attach_tx: Mutex<Option<mpsc::SyncSender<i32>>>,
    /// 世代收尾信号（Go run.done：`tun_stop` 有界等待的观测面；每世代一次性）。
    done: Mutex<bool>,
    done_cv: Condvar,
    /// 执行体世代的停止位（Go run.stop 的收口面：finish 里 close(r.stop) 同义——
    /// 评审 r2-L12。世代线程入口经 [`Self::set_stop_flag_if_current`] 登记；
    /// `finish_generation` 对当前世代置位——保证收尾路径不依赖 request_stop 先行）。
    stop_flag: Mutex<Option<Arc<AtomicBool>>>,
    /// event-driven-heal §2.4：翻沿序号（宿主挂起式等待的唤醒面——分类/自救态的
    /// 任何翻沿都 `bump_flip`；宿主 `wait_flip` 比对上次见到的序号即知有翻沿）。
    flip_seq: AtomicU64,
    /// 翻沿等待原语（与 `flip_seq` 配对；锁本身无载荷——只做唤醒/序号可见性）。
    flip_gate: Mutex<()>,
    flip_cv: Condvar,
}

impl Default for TunShared {
    fn default() -> Self {
        Self::new()
    }
}

impl TunShared {
    pub fn new() -> Self {
        TunShared {
            probe_running: AtomicBool::new(false),
            gen: AtomicU64::new(0),
            stage: StageMachine::new(),
            healthy: AtomicBool::new(true),
            device_dead: AtomicBool::new(false),
            unhealthy_why: Mutex::new(String::new()),
            last_patrol_at: Mutex::new(None),
            attach_tx: Mutex::new(None),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
            flip_seq: AtomicU64::new(0),
            flip_gate: Mutex::new(()),
            flip_cv: Condvar::new(),
            stop_flag: Mutex::new(None),
        }
    }

    /// 执行体登记本世代的停止位（世代线程构建 GenRun 后调；世代起点换新）。
    /// 带世代守卫（复核 r3-F11：-2 强放锁后被挂起 ≥3s 的旧线程恢复时不得把槽
    /// 改回旧世代的旗标——冻结恢复是手机真实场景）。
    pub fn set_stop_flag_if_current(&self, gen: u64, f: Arc<AtomicBool>) {
        let mut slot = lock_unpoison(&self.stop_flag);
        if self.gen.load(Ordering::Acquire) == gen || slot.is_none() {
            *slot = Some(f);
        }
    }

    /// 置当前世代的停止位（request_stop 之外的收口面：finish_generation 兜底；
    /// 世代句柄未就的窗口期由 request_stop 中继调用——评审 r2-M2）。
    pub fn signal_stop(&self) {
        if let Some(f) = lock_unpoison(&self.stop_flag).as_ref() {
            f.store(true, Ordering::Release);
        }
    }

    /// 世代线程注册 attach 接收端（世代起点调；返回 Receiver——暖机完成后等 fd 用）。
    /// 返回的通道在发送端全 drop（世代被强制放锁后的迟到投递）时 recv 返回 Err，
    /// 世代线程按 stop 信号收工。
    pub fn attach_receiver(&self) -> mpsc::Receiver<i32> {
        let (tx, rx) = mpsc::sync_channel(1);
        *lock_unpoison(&self.attach_tx) = Some(tx);
        rx
    }

    /// 投 fd 给 ready 世代（Go `attachCh <- fd` 的非阻塞面；false = 通道满/无接收者）。
    pub fn deliver_fd(&self, fd: i32) -> bool {
        lock_unpoison(&self.attach_tx)
            .as_ref()
            .is_some_and(|tx| tx.try_send(fd).is_ok())
    }

    /// attach 投递面收口（世代收尾后迟到投递不再被接收；Go 的通道随世代 drop 同义）。
    fn close_attach(&self) {
        *lock_unpoison(&self.attach_tx) = None;
    }

    /// 标记不健康（写序契约：先 why 后健康位——见模块头）。只有当前世代有权
    /// （世代守卫在调用方过——执行体知道自己是否当前世代时才调）。
    /// 分类分流（tun-death-gate）：设备类写 [`Self::device_dead`]（单向）；
    /// 传输类（patrol）只写传输位并记判定时刻（翻回最小间隔的基准）。
    pub fn mark_unhealthy(&self, why: &str) {
        *lock_unpoison(&self.unhealthy_why) = why.to_owned();
        if DEVICE_CLASS(why) {
            self.device_dead.store(true, Ordering::Release);
        } else {
            *lock_unpoison(&self.last_patrol_at) = Some(Instant::now());
        }
        self.healthy.store(false, Ordering::Release);
        self.bump_flip(); // 翻沿（进入不健康——设备类或传输类）
    }

    /// 带世代守卫的不健康标记（评审 r2-M1：Go `tunRun.markUnhealthy` 内有
    /// `r.isCurrent()`——旧世代的 fd 被 destroy 后报错是必然事件，不能拿它判
    /// 新世代死没死。生产调用点〔fd 错误回调/patrol 收尾〕一律走本面）。
    pub fn mark_unhealthy_if_current(&self, gen: u64, why: &str) {
        if self.gen.load(Ordering::Acquire) == gen {
            self.mark_unhealthy(why);
        }
    }

    /// 新世代起点：清健康位与分类（Go tunBegin：tunUnhealthyWhy.Store("") +
    /// tunHealthy.Store(true)；顺序即注释——先清分类再置健康）。设备死位与 patrol
    /// 判定时刻一并随新世代清（tun-death-gate）。
    pub fn begin_healthy(&self) {
        *lock_unpoison(&self.unhealthy_why) = String::new();
        self.healthy.store(true, Ordering::Release);
        self.device_dead.store(false, Ordering::Release);
        *lock_unpoison(&self.last_patrol_at) = None;
        self.bump_flip(); // 翻沿（新世代起点 = 全清）
    }

    /// 传输健康翻回（tun-death-gate）：仅当分类为 `patrol` 且距上次判定 ≥ `min_gap`
    /// 时生效——写序「先清 why 再置 healthy」（与 begin_healthy 同序）；**分类槽为
    /// 设备/传输共用，仅清 patrol**——设备类诊断文案不受影响。返回
    /// `Some(距上次判定时长)` = 已翻回（调用方打判据行）；`None` = 未翻（分类不符
    /// 或间隔未到——抖动循环由最小间隔拦）。
    pub fn try_recover_transport_if_current(
        &self,
        gen: u64,
        min_gap: Duration,
    ) -> Option<Duration> {
        if self.gen.load(Ordering::Acquire) != gen {
            return None; // 陈旧世代的重赛跑回执不得清新世代的 patrol（r5 M4）
        }
        let mut why = lock_unpoison(&self.unhealthy_why);
        if *why != "patrol" {
            return None;
        }
        let at = (*lock_unpoison(&self.last_patrol_at))?;
        let elapsed = Instant::now().saturating_duration_since(at);
        if elapsed < min_gap {
            return None;
        }
        *why = String::new();
        drop(why);
        self.healthy.store(true, Ordering::Release);
        self.bump_flip(); // 翻沿（传输健康恢复）
        Some(elapsed)
    }

    /// 翻沿信号（event-driven-heal §2.4）：分类/自救态翻沿时调用——唤醒所有
    /// 挂起中的 `wait_flip`。返回新序号。
    pub fn bump_flip(&self) -> u64 {
        let seq = self.flip_seq.fetch_add(1, Ordering::AcqRel) + 1;
        drop(lock_unpoison(&self.flip_gate));
        self.flip_cv.notify_all();
        seq
    }

    /// 翻沿序号读面（宿主首次调用前取基线）。
    pub fn flip_seq(&self) -> u64 {
        self.flip_seq.load(Ordering::Acquire)
    }

    /// 挂起式等待翻沿（event-driven-heal §2.4 选型 (b)：单飞 ≤1 由宿主保证；
    /// 超时返回**当前**序号——超时本身构成一次轻对账，不丢翻沿）。序号已大于
    /// `last` 时立即返回。
    pub fn wait_flip(&self, last: u64, timeout: Duration) -> u64 {
        let mut g = lock_unpoison(&self.flip_gate);
        let deadline = Instant::now() + timeout;
        while self.flip_seq.load(Ordering::Acquire) <= last {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            let slice = deadline.saturating_duration_since(now);
            let (g2, res) = match self.flip_cv.wait_timeout(g, slice) {
                Ok(v) => v,
                // 毒锁：取回守卫与超时结果按常态收口（序号在锁内重读）
                Err(e) => e.into_inner(),
            };
            g = g2;
            if res.timed_out() {
                break;
            }
        }
        self.flip_seq.load(Ordering::Acquire)
    }

    /// 传输类不健康的单点判别（r5 L1：`why=="patrol"` 的派生集中在此——状态组装
    /// 面两处共用，别在调用点散写字符串比对）。
    pub fn transport_unhealthy(&self) -> bool {
        *lock_unpoison(&self.unhealthy_why) == "patrol"
    }

    /// `running` 的单点合成（tun-death-gate：capi 面与对账快照共用——只反映设备面
    /// 存活：单飞锁 + **非设备死** + attached。传输类不健康〔patrol〕不再把
    /// running 拉零——调用方据 `unhealthyReason` 区分「需要重建（设备死）」与
    /// 「只需恢复传输」）。三个信号各自原子、合起来不是一致快照，转换瞬间可能
    /// 瞬时 0/1；调用方已用去抖。
    pub fn running(&self) -> bool {
        self.probe_running.load(Ordering::Acquire)
            && !self.device_dead.load(Ordering::Acquire)
            && self.stage.snapshot().stage == TunStage::Attached
    }

    /// 世代收尾（**执行体世代线程的一切退出路径必须调**；幂等——done 只关一次，
    /// 终态/放锁过世代守卫）：
    /// 1. 非 failed 终态回 idle 空码（failed 保留）；
    /// 2. 当前世代：分类 "stop" + 健康位 false + 放单飞锁；
    /// 3. 关 attach 通道（迟到投递不再被接收）；
    /// 4. 发 done 信号（tun_stop 的等待面）。
    pub fn finish_generation(&self, gen: u64) {
        let snap = self.stage.snapshot();
        if snap.stage != TunStage::Failed {
            self.stage
                .set_if_current(gen, TunStage::Idle, "", "", false);
        }
        let is_current = self.gen.load(Ordering::Acquire) == gen;
        if is_current {
            // stop 收口（评审 r2-L12：Go finish 里 close(r.stop)——收尾路径不依赖
            // request_stop 先行；置位让世代的一切派生线程〔patrol/pusher/stats/
            // 桥/hint〕从片界观察到收尾）
            self.signal_stop();
            // mark_unhealthy("stop") 内含 bump_flip（r3-L2：无需另调——世代结束
            // 的翻沿唤醒由它承担；在飞 wait_flip 不会挂满 2min 上限）。
            self.mark_unhealthy("stop");
            self.probe_running.store(false, Ordering::Release);
        }
        // done/attach 通道同过世代守卫（评审 r2-H2：它们是 TunShared 单例槽——
        // 旧世代迟到的收尾若无条件写，会污染新世代：① attach 通道被关 ⇒ 新世代
        // tun_attach 恒 -1 直到 60s 死线；② done 被置真 ⇒ 新世代 tun_stop 命中
        // is_done() 快路径直接 0、从不 request_stop ⇒ stop 位永假、probe_running
        // 无人放——之后所有 prepare 恒 -1，只有进程重启能解。Go 的 done/attachCh
        // 是每世代 tunRun 自己的通道，无此面；单例槽形态下以 gen 比对等价表达）。
        if is_current {
            self.close_attach();
            let mut done = lock_unpoison(&self.done);
            if !*done {
                *done = true;
                self.done_cv.notify_all();
            }
        }
    }

    /// 本世代是否已收尾（tun_stop 的快速路径）。
    pub fn is_done(&self) -> bool {
        *lock_unpoison(&self.done)
    }

    /// 等世代收尾（有界；返回是否已收尾——Go `select { case <-r.done … time.After }`）。
    pub fn wait_done(&self, budget: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + budget;
        let mut done = lock_unpoison(&self.done);
        while !*done {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                return false;
            }
            let (g, _to) = self
                .done_cv
                .wait_timeout(done, left)
                .unwrap_or_else(|e| e.into_inner());
            done = g;
        }
        true
    }

    /// 世代起点的 done 复位（新 prepare 受理时——TunShared 是全核单例，done 位
    /// 随世代翻新；attach 通道与停止位槽也一并复位）。
    pub fn begin_generation(&self, gen: u64) {
        self.gen.store(gen, Ordering::Release);
        self.stage.begin_generation(gen);
        *lock_unpoison(&self.done) = false;
        self.close_attach();
        *lock_unpoison(&self.stop_flag) = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（event-driven-heal §2.4）**：翻沿信号唤醒挂起中的等待——bump 后
    /// wait_flip 即时返回（不等超时），返回新序号。
    #[test]
    fn flip_wakes_pending_wait_with_new_seq() {
        let sh = std::sync::Arc::new(TunShared::new());
        let sh2 = std::sync::Arc::clone(&sh);
        let h = std::thread::spawn(move || sh2.wait_flip(0, Duration::from_secs(5)));
        std::thread::sleep(Duration::from_millis(50));
        let t0 = Instant::now();
        assert_eq!(sh.bump_flip(), 1, "bump 返回新序号");
        let got = h.join().expect("等待线程收口");
        assert_eq!(got, 1);
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "唤醒即时（实测 {:?}）",
            t0.elapsed()
        );
    }

    /// **判据**：超时返回当前序号（不丢翻沿语义——超时即一次轻对账）；已过序号
    /// 立即返回不等待。
    #[test]
    fn flip_wait_times_out_and_immediate_path() {
        let sh = TunShared::new();
        let t0 = Instant::now();
        assert_eq!(sh.wait_flip(0, Duration::from_millis(80)), 0, "无翻沿：超时返当前序号");
        assert!(t0.elapsed() >= Duration::from_millis(70), "真等了超时窗");
        assert_eq!(sh.bump_flip(), 1);
        let t1 = Instant::now();
        assert_eq!(sh.wait_flip(0, Duration::from_secs(5)), 1, "已过序号：立即返回");
        assert!(t1.elapsed() < Duration::from_secs(1));
        assert_eq!(sh.wait_flip(1, Duration::from_millis(0)), 1, "timeout=0 = 立即取快照");
    }

    /// **判据**：三个翻沿点都 bump——mark_unhealthy（进入不健康，两类都算）、
    /// begin_healthy（新世代清零）、try_recover_transport（传输翻回）。
    #[test]
    fn classification_flips_signal_the_channel() {
        let sh = TunShared::new();
        let base = sh.flip_seq();
        sh.mark_unhealthy("patrol");
        assert!(sh.flip_seq() > base, "进入传输类不健康 = 翻沿");
        let mid = sh.flip_seq();
        sh.begin_generation(1);
        sh.begin_healthy();
        assert!(sh.flip_seq() > mid, "新世代清零 = 翻沿");
        // 传输翻回：需 patrol 分类 + 过最小间隔（0 = 恒过）
        sh.mark_unhealthy("patrol");
        let before = sh.flip_seq();
        assert!(sh.try_recover_transport_if_current(1, Duration::ZERO).is_some());
        assert!(sh.flip_seq() > before, "传输健康翻回 = 翻沿");
        // 陈旧世代不翻沿（守卫语义不因翻沿通道改变）
        let guard_before = sh.flip_seq();
        assert!(sh.try_recover_transport_if_current(999, Duration::ZERO).is_none());
        assert_eq!(sh.flip_seq(), guard_before, "陈旧世代不产生翻沿");
    }

    #[test]
    fn finish_releases_lock_and_resets_stage() {
        let sh = TunShared::new();
        sh.probe_running.store(true, Ordering::Release);
        sh.begin_generation(1);
        sh.stage.set_if_current(1, TunStage::Attached, "", "", true);
        sh.finish_generation(1);
        assert!(!sh.probe_running.load(Ordering::Acquire), "收尾放锁");
        assert_eq!(
            sh.stage.snapshot().stage,
            TunStage::Idle,
            "非 failed 回 idle"
        );
        assert_eq!(sh.stage.snapshot().code, "", "空码（stopped 码是窗口语义）");
        assert!(!sh.healthy.load(Ordering::Acquire));
        assert_eq!(*lock_unpoison(&sh.unhealthy_why), "stop");
        assert!(sh.is_done());
    }

    /// failed 终态保留（调用方要读原因）。
    #[test]
    fn finish_keeps_failed() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.stage
            .set_if_current(1, TunStage::Failed, "core", "启动失败", false);
        sh.finish_generation(1);
        assert_eq!(sh.stage.snapshot().stage, TunStage::Failed);
        assert_eq!(sh.stage.snapshot().code, "core");
        assert_eq!(sh.stage.snapshot().reason, "启动失败");
    }

    /// 旧世代晚到的收尾：不得动新世代（世代守卫）——锁已由新世代持有。
    #[test]
    fn stale_generation_finish_is_noop_on_shared() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.begin_generation(2); // 新世代接管
        sh.probe_running.store(true, Ordering::Release);
        sh.stage
            .set_if_current(2, TunStage::Preparing, "", "", false);
        let _rx2 = sh.attach_receiver(); // 新世代注册自己的 attach 接收端
        sh.finish_generation(1); // 旧世代迟到收尾
        assert!(
            sh.probe_running.load(Ordering::Acquire),
            "旧世代收尾不得放新世代的锁"
        );
        assert_eq!(sh.stage.snapshot().stage, TunStage::Preparing);
        // 评审 r2-H2：done/attach 也是世代共享面——旧世代迟到收尾**不得**发 done
        // （否则新世代 tun_stop 命中 is_done() 快路径、request_stop 永不执行）
        // 也不得关 attach 通道（否则新世代 tun_attach 恒 -1）
        assert!(!sh.is_done(), "旧世代收尾不得发新世代的 done");
        assert!(sh.deliver_fd(1), "旧世代收尾不得关新世代的 attach 通道");
    }

    /// attach 通道：注册 → 投递 → 收口后迟到投递 false。
    #[test]
    fn attach_channel_lifecycle() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        let rx = sh.attach_receiver();
        assert!(sh.deliver_fd(90));
        assert_eq!(rx.recv(), Ok(90));
        sh.finish_generation(1);
        assert!(!sh.deliver_fd(91), "收尾后投递不再被接收");
        // 通道满（缓冲 1）：第二次投递 false
        sh.begin_generation(2);
        let _rx2 = sh.attach_receiver();
        assert!(sh.deliver_fd(1));
        assert!(!sh.deliver_fd(2), "通道满 = false（调用方按 -1 收）");
    }

    /// wait_done 有界等待：未收尾超时 false；当前世代收尾后 Condvar 唤醒 true。
    #[test]
    fn wait_done_bounded() {
        use std::sync::Arc;
        let sh = Arc::new(TunShared::new());
        assert!(
            !sh.wait_done(std::time::Duration::from_millis(50)),
            "未收尾 = 超时 false"
        );
        sh.begin_generation(1); // done 属世代面——当前世代收尾才发
        let sh2 = Arc::clone(&sh);
        let t = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(30));
            sh2.finish_generation(1);
        });
        assert!(
            sh.wait_done(std::time::Duration::from_secs(2)),
            "收尾后被唤醒"
        );
        t.join().unwrap();
    }

    /// mark_unhealthy_if_current（评审 r2-M1）：旧世代（fd 被 destroy 后的迟到报错
    /// 是必然事件）不得把新世代标成不健康。
    #[test]
    fn mark_unhealthy_generation_guard() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.begin_healthy();
        sh.begin_generation(2); // 新世代接管
        sh.mark_unhealthy_if_current(1, "fd"); // 旧世代的 fd 错误回调
        assert!(sh.healthy.load(Ordering::Acquire), "旧世代不得标记新世代");
        assert!(lock_unpoison(&sh.unhealthy_why).is_empty());
        sh.mark_unhealthy_if_current(2, "fd"); // 当前世代照常
        assert!(!sh.healthy.load(Ordering::Acquire));
        assert_eq!(*lock_unpoison(&sh.unhealthy_why), "fd");
    }

    /// stop 位收口（评审 r2-L12）：finish 对当前世代置位；旧世代迟到收尾不动
    /// 新世代的停止位；begin_generation 换新时清槽。
    #[test]
    fn finish_stops_current_generation_flag() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        let f1 = Arc::new(AtomicBool::new(false));
        sh.set_stop_flag_if_current(1, Arc::clone(&f1));
        sh.begin_generation(2); // 新起点：槽已清
        let f2 = Arc::new(AtomicBool::new(false));
        sh.set_stop_flag_if_current(2, Arc::clone(&f2));
        sh.finish_generation(1); // 旧世代迟到收尾
        assert!(
            !f1.load(Ordering::Acquire),
            "旧世代收尾不动自己的旧位（已出槽）"
        );
        assert!(!f2.load(Ordering::Acquire), "更不得动新世代的停止位");
        sh.finish_generation(2); // 当前世代收尾
        assert!(
            f2.load(Ordering::Acquire),
            "收尾置当前世代停止位（Go close(r.stop) 同义）"
        );
    }

    /// tun-death-gate：传输类不健康（patrol）不把 running 拉零；设备类才拉。
    #[test]
    fn patrol_not_pull_running_zero() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.probe_running.store(true, Ordering::Release);
        sh.stage.set_if_current(1, TunStage::Attached, "", "", true);
        assert!(sh.running(), "健康基线");
        sh.mark_unhealthy("patrol");
        assert!(
            sh.running(),
            "patrol 只写传输位——running（设备面）仍为真（2026-10-15 事故的误杀链入口）"
        );
        assert!(!sh.healthy.load(Ordering::Acquire), "传输健康位为假");
        sh.begin_generation(2);
        sh.begin_healthy();
        sh.probe_running.store(true, Ordering::Release);
        sh.stage.set_if_current(2, TunStage::Attached, "", "", true);
        sh.mark_unhealthy("fd");
        assert!(!sh.running(), "设备死把 running 拉零");
        assert!(sh.device_dead.load(Ordering::Acquire));
    }

    /// tun-death-gate：翻回仅对 patrol 生效（分类不符不动）；设备类文案不被抹。
    #[test]
    fn recover_keeps_device_class_why() {
        let sh = TunShared::new();
        sh.mark_unhealthy("fd");
        assert!(sh.try_recover_transport_if_current(1, Duration::ZERO).is_none());
        assert_eq!(
            *lock_unpoison(&sh.unhealthy_why),
            "fd",
            "设备类分类文案不得被翻回抹掉"
        );
        assert!(!sh.healthy.load(Ordering::Acquire));
        // 未知分类保守归设备：同样不翻。
        sh.begin_healthy();
        sh.mark_unhealthy("fd-timeout");
        assert!(sh.try_recover_transport_if_current(1, Duration::ZERO).is_none());
        assert!(sh.device_dead.load(Ordering::Acquire), "未知值归设备类");
    }

    /// r5 M4：翻回带世代守卫——陈旧世代的重赛跑回执不得清新世代的 patrol。
    #[test]
    fn recover_respects_generation_guard() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.mark_unhealthy("patrol");
        sh.begin_generation(2); // 新世代接管（begin_healthy 未调——新世代仍在 patrol 外）
        sh.mark_unhealthy("patrol");
        assert!(
            sh.try_recover_transport_if_current(1, Duration::ZERO).is_none(),
            "旧世代回执不得翻新世代"
        );
        assert!(
            sh.try_recover_transport_if_current(2, Duration::ZERO).is_some(),
            "当前世代照常翻回"
        );
    }

    /// tun-death-gate：patrol 翻回的最小间隔与写序（先清 why 再置 healthy）。
    #[test]
    fn recover_patrol_with_min_gap() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.mark_unhealthy("patrol");
        assert!(
            sh.try_recover_transport_if_current(1, Duration::from_secs(30)).is_none(),
            "间隔未到不翻（防抖动循环）"
        );
        assert_eq!(*lock_unpoison(&sh.unhealthy_why), "patrol");
        let got = sh.try_recover_transport_if_current(1, Duration::ZERO);
        assert!(got.is_some(), "间隔放宽后翻回");
        assert!(sh.healthy.load(Ordering::Acquire));
        assert!(
            lock_unpoison(&sh.unhealthy_why).is_empty(),
            "翻回后分类已清（写序：先清 why）"
        );
    }

    /// **判据（评审 r3-M1a）**：设备类 mark 改写分类槽后 `transport_unhealthy()`
    /// 变假——「恢复」判据不得用它（recover_push 吸收循环曾据此把设备死判成
    /// rc=0，吞掉唯一合法的重建升级）。健康位是唯一精确判据：真 ⟺ 翻回/新世代。
    #[test]
    fn device_class_mark_disguised_as_recovery_rejected() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.mark_unhealthy("patrol");
        assert!(sh.transport_unhealthy(), "前置：传输类在册");
        // 吸收窗内设备判死（fd 类——含僵死 fd 双阈值判死路径）改写分类槽。
        sh.mark_unhealthy("fd");
        assert!(
            !sh.transport_unhealthy(),
            "分类槽已被设备类改写（旧判据在此误判「已恢复」）"
        );
        assert!(
            !sh.healthy.load(Ordering::Acquire),
            "健康位仍假——不得按恢复收口（rc=0）"
        );
        assert!(sh.device_dead.load(Ordering::Acquire), "设备死位在册");
    }
}
