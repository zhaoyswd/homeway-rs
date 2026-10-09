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

use super::stage::{StageMachine, TunStage};

/// 锁中毒不 panic（Q-F F6-1：单源上移到 `crate::syncutil`——facade 面 /
/// recover gate 与本处共用同一件；facade 内的既有调用点经本重导出零改动）。
pub(crate) use crate::syncutil::lock_unpoison;

/// tun 域的世代共享面（Arc 交给执行体）。
pub struct TunShared {
    /// 单飞锁（probeRunning）：世代在世期间持有，收尾放。
    pub probe_running: AtomicBool,
    /// 世代计数（每次 prepare 递增；阶段写与收尾都比对它防陈旧）。
    pub gen: AtomicU64,
    pub stage: StageMachine,
    /// 健康位（Go tunHealthy：fd 循环异常/巡检耗尽/收工都会翻 false）。
    pub healthy: AtomicBool,
    /// 不健康分类（patrol/fd/panic/stop；Go tunUnhealthyWhy——写序：先写它再翻 healthy）。
    pub unhealthy_why: Mutex<String>,
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
            unhealthy_why: Mutex::new(String::new()),
            attach_tx: Mutex::new(None),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
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

    /// 标记不健康（写序契约：先 why 后 healthy——见模块头）。只有当前世代有权
    /// （世代守卫在调用方过——执行体知道自己是否当前世代时才调）。
    pub fn mark_unhealthy(&self, why: &str) {
        *lock_unpoison(&self.unhealthy_why) = why.to_owned();
        self.healthy.store(false, Ordering::Release);
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
    /// tunHealthy.Store(true)；顺序即注释——先清分类再置健康）。
    pub fn begin_healthy(&self) {
        *lock_unpoison(&self.unhealthy_why) = String::new();
        self.healthy.store(true, Ordering::Release);
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
}
