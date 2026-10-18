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

use homeway_quic::CarrierFlag;

use super::stage::{StageMachine, TunStage};

/// 锁中毒不 panic（Q-F F6-1：单源上移到 `crate::syncutil`——facade 面 /
/// recover gate 与本处共用同一件；facade 内的既有调用点经本重导出零改动）。
pub(crate) use crate::syncutil::lock_unpoison;

/// 不健康分类的设备/传输分界（tun-death-gate）：`patrol` = 传输类（对端不可达——
/// 不写设备死位、可翻回）；`fd`/`panic`/`stop` 与**任何未知值** = 设备类（单向，
/// 仅新世代 begin 清——未知值保守归设备侧）。
const DEVICE_CLASS: fn(&str) -> bool = |why| why != "patrol";

/// 健康态的临界区载荷（reconnect-oneshot r8-N2 终稿 + 评审 r1 P0-5 整改：
/// `why`/`healthy`/`device_dead`/`last_patrol_at` 的写入与 **gen 检查**全部收进
/// `TunShared::health` 同一临界区——**gen 镜像进本结构**，`*_if_current` 持锁后比对
/// `h.gen`：旧世代的岛回调（patrol/fd/panic）/旧 worker 在锁外原子检查通过后被调度
/// 走、新世代 `begin_generation` + `begin_healthy` 完成，再拿到锁也写不进新世代
/// 〔check-then-act 窗口由此消灭——检查与写入在同一临界区内原子完成〕；锁外仍保留
/// 一枚原子 `gen` 供零锁读者〔读线程 step-0 / `island_cmd_until` 的分片检查〕）。
/// 四个写者：`mark_transport_if_current`/`mark_unhealthy`/`try_recover_transport_if_current`/
/// `begin_healthy`（+ 评审 r1 新增的 `on_adopt_if_current`）不再分次写、读侧不再读出
/// 混合快照。
struct HealthState {
    /// 世代镜像（`begin_generation` 在本临界区内更新——写路径的权威判定面）。
    gen: u64,
    /// 健康位（传输面）：`patrol` 写假、翻回写真——不承载设备死亡。
    healthy: bool,
    /// 不健康分类（patrol/fd/panic/stop；写序：先写它再翻 healthy/设备位）。
    why: String,
    /// 设备死位（fd/panic/stop + 未知分类；单向——仅新世代 [`TunShared::begin_healthy`]
    /// 清）。`running()` 只看它：传输类不健康不再把 running 拉零（2026-10-15 事故
    /// 的误杀链入口）。
    device_dead: bool,
    /// 最近一次 patrol 判定时刻（翻回最小间隔的记账面；仅 patrol 分类写入）。
    last_patrol_at: Option<Instant>,
}

/// 读侧统一快照（r8-N2：`tun_status`/`tun_wait_flip` 同锁一次读——carrier 纳入防
/// 「healthy=true 且 carrier=false」中间态可见）。
pub struct HealthSnapshot {
    pub healthy: bool,
    pub why: String,
    pub device_dead: bool,
    pub last_patrol_at: Option<Instant>,
    /// 承载在位性（同一临界区内的原子读——与上面的健康字段一致化）。
    pub carrier: bool,
}

/// tun 域的世代共享面（Arc 交给执行体）。
pub struct TunShared {
    /// 单飞锁（probeRunning）：世代在世期间持有，收尾放。
    pub probe_running: AtomicBool,
    /// 世代计数（每次 prepare 递增；阶段写与收尾都比对它防陈旧）。**零锁读面**
    /// （读线程 step-0 / `island_cmd_until` 分片检查 / `TunnelExec` 的世代守卫）；
    /// **写路径的权威 gen 在 [`HealthState`] 内**（`begin_generation` 双写——先本
    /// 原子后 health 锁，见评审 r1 P0-5 整改注记）。
    pub gen: AtomicU64,
    pub stage: StageMachine,
    /// 健康态（临界区收口，见 [`HealthState`]）。
    health: Mutex<HealthState>,
    /// **承载原子位**（reconnect-oneshot D2）：与岛装配期共握同一 `Arc`（经
    /// `IslandConfig::carrier` 注入）。写沿全部在 `health` 临界区内（r10-C13——
    /// alive 时锁内查 device_dead）；读面（读线程每包 load）走原子、零锁。
    pub carrier: Arc<CarrierFlag>,
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
    /// **finish×begin 握手门**（评审 r3 B2⑥ + r4 B2-a/B2-b 扩面）：`finish_generation`
    /// 的尾段（signal_stop/mark_unhealthy〔r4 B2-b——旧位在门外，旧 finish 停在
    /// `is_current` 判断后放行时会把新世代的停止位置真〕+ close_attach/done/
    /// probe_running）与 `begin_prepare` 的「单飞获取 + gen 递增 + 换代复位」
    /// （r4 B2-a——旧序 swap 在门外，旧 finish 尾段可把新 prepare 刚拿到的单飞锁
    /// 清成 false）同门互斥——门内**复核 gen** 后才写共享槽 ⇒ stale finish 对
    /// 已起跑的新世代零写。锁序：本门永远最外层（持门内只拿
    /// attach/done/health/stage/stop_flag）。health 路径的 `bump_flip`（r5-L6）
    /// 在 health 临界区**释放之后**才拿 `flip_gate`；两级都在门内单向，
    /// **无「持 health/flip_gate 反向拿 finish_gate」的路径**（反向序不存在）。
    finish_gate: Mutex<()>,
    /// 受控交错缝（评审 r3 B2⑤，`#[cfg(test)]`）：卡在 `finish_generation` 的
    /// `is_current` 判断之后、共享槽尾段之前——「旧 finish 停在该点 → 新世代
    /// begin → 放行」的确定性构造面（生产零开销）。
    #[cfg(test)]
    finish_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// 受控交错缝（评审 r4 B2-a，`#[cfg(test)]`）：卡在 `begin_prepare` 门内
    /// 「swap 已成功、gen 尚未递增」处（**持门**——受控交错不破临界区）——
    /// 「tun_stop -2 → 新 prepare 拿锁停在门内 → 放行旧 finish → 放行新 prepare」
    /// 生产序的确定性构造面（生产零开销）。
    #[cfg(test)]
    prepare_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// 受控交错缝（评审 r5-L2，`#[cfg(test)]`）：卡在 `finish_generation` **持门
    /// 且 gen 复核已通过之后**、尾段共享槽写之前——B2-a 对照实验的确定性屏障：
    /// 旧序（swap/钩子在门外）下旧 finish 先进入尾段；新序下它被新 prepare 持门
    /// 挡在外，永不触发（生产零开销）。
    #[cfg(test)]
    finish_tail_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
    /// 受控交错缝（评审 r5-L1，`#[cfg(test)]`）：卡在 `tun_stop` 的第二次
    /// gen/probe 复核之后、`force_release_if_current` 之前（**不持 finish_gate**
    /// ——窗口期内旧 finish 与新 prepare 可完成，复现「复核已过、放锁未落」的
    /// check-then-act 形态；生产零开销）。
    #[cfg(test)]
    stop_force_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
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
            health: Mutex::new(HealthState {
                gen: 0,
                healthy: true,
                why: String::new(),
                device_dead: false,
                last_patrol_at: None,
            }),
            carrier: CarrierFlag::new(),
            attach_tx: Mutex::new(None),
            done: Mutex::new(false),
            done_cv: Condvar::new(),
            flip_seq: AtomicU64::new(0),
            flip_gate: Mutex::new(()),
            flip_cv: Condvar::new(),
            stop_flag: Mutex::new(None),
            finish_gate: Mutex::new(()),
            #[cfg(test)]
            finish_hook: Mutex::new(None),
            #[cfg(test)]
            prepare_hook: Mutex::new(None),
            #[cfg(test)]
            finish_tail_hook: Mutex::new(None),
            #[cfg(test)]
            stop_force_hook: Mutex::new(None),
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

    /// **带世代守卫的健康临界区**（评审 r1 P0-5 整改的核心承载）：拿锁后比对
    /// `h.gen`——**检查与写入在同一临界区内原子完成**，杜绝「锁外原子检查通过 →
    /// 被调度走 → 新世代 begin 完成 → 旧调用拿到锁写进新世代」的跨代写入窗口
    /// （r8-N2 要消灭的形态）。陈旧 ⇒ `None`（不写）。
    fn with_health_if_current<T>(
        &self,
        gen: u64,
        f: impl FnOnce(&mut HealthState) -> T,
    ) -> Option<T> {
        let mut h = lock_unpoison(&self.health);
        if h.gen != gen {
            return None; // 旧世代的迟到调用：检查在锁内——新世代已接管
        }
        Some(f(&mut h))
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
    /// **世代守卫**（评审 r2 B2⑦）：旧 warmup 线程的迟到注册不得覆盖新世代的
    /// attach sender（否则新世代 `tun_attach` 恒 -1 直到 60s 死线）——陈旧且槽位
    /// 已被占用时不安装（形态同 [`Self::set_stop_flag_if_current`]）。
    pub fn attach_receiver(&self, gen: u64) -> mpsc::Receiver<i32> {
        let (tx, rx) = mpsc::sync_channel(1);
        let mut slot = lock_unpoison(&self.attach_tx);
        if self.gen.load(Ordering::Acquire) == gen || slot.is_none() {
            *slot = Some(tx);
        }
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
    /// 分类分流（tun-death-gate）：设备类写设备死位（单向）+ 承载位设备死闩；
    /// 传输类（patrol）只写传输位并记判定时刻（翻回最小间隔的基准）+ 承载位
    /// 拉假。**承载位写入与健康写入同一临界区**（reconnect-oneshot r10-C13）。
    pub fn mark_unhealthy(&self, why: &str) {
        let mut h = lock_unpoison(&self.health);
        h.why = why.to_owned();
        if DEVICE_CLASS(why) {
            h.device_dead = true;
            self.carrier.mark_device_dead();
        } else {
            h.last_patrol_at = Some(Instant::now());
            self.carrier.mark_dead();
        }
        h.healthy = false;
        drop(h);
        self.bump_flip(); // 翻沿（进入不健康——设备类或传输类）
    }

    /// 带世代守卫的不健康标记（评审 r2-M1：Go `tunRun.markUnhealthy` 内有
    /// `r.isCurrent()`——旧世代的 fd 被 destroy 后报错是必然事件，不能拿它判
    /// 新世代死没死。生产调用点〔fd 错误回调/patrol 收尾〕一律走本面）。
    /// **世代检查在 health 锁内**（评审 r1 P0-5：与写入同一临界区）。
    pub fn mark_unhealthy_if_current(&self, gen: u64, why: &str) {
        if self
            .with_health_if_current(gen, |h| {
                h.why = why.to_owned();
                if DEVICE_CLASS(why) {
                    h.device_dead = true;
                    self.carrier.mark_device_dead();
                } else {
                    h.last_patrol_at = Some(Instant::now());
                    self.carrier.mark_dead();
                }
                h.healthy = false;
            })
            .is_some()
        {
            self.bump_flip(); // 翻沿（进入不健康——设备类或传输类）
        }
    }

    /// **传输不健康的专用标记入口**（reconnect-oneshot D2-r8-N2/r13-P6）：B 门上报
    /// 沿与观测快探连败两条路径共用。检查 + 写 `why=patrol` + 置健康假 + 记判定
    /// 时刻 + **置承载位 false** 全在 `health` 同一临界区内（否则读面触发条件不
    /// 成立）；幂等——**设备死分类在场或已处传输不健康时 no-op**（不覆盖设备类
    /// 文案、不重复翻沿、carrier 也不动）。返回 `true` = 本调发生了标记。
    pub fn mark_transport_if_current(&self, gen: u64) -> bool {
        // 陈旧世代（None）与 no-op（Some(false)）同返 false——区别只在是否翻沿。
        let marked = self.with_health_if_current(gen, |h| {
            if h.device_dead {
                return false; // r13-P6：设备死在场——分类与 carrier 都不动
            }
            if h.why == "patrol" {
                return false; // 已处传输不健康——幂等 no-op（不双 bump）
            }
            h.why = "patrol".to_owned();
            h.healthy = false;
            h.last_patrol_at = Some(Instant::now());
            self.carrier.mark_dead(); // 同一临界区（读面触发条件由此成立）
            true
        }) == Some(true);
        if marked {
            self.bump_flip(); // 翻沿（进入传输不健康）
        }
        marked
    }

    /// 新世代起点：清健康位与分类（Go tunBegin：tunUnhealthyWhy.Store("") +
    /// tunHealthy.Store(true)；顺序即注释——先清分类再置健康）。设备死位与 patrol
    /// 判定时刻一并随新世代清（tun-death-gate）；承载位随新世代复位（alive=false：
    /// 岛尚未采纳——adopt 置 true）。**带世代参数**（reconnect-oneshot r8-N2：陈旧
    /// 调用 no-op——旧世代迟到的复位不得动新世代；**检查在 health 锁内**，评审 r1 P0-5）。
    pub fn begin_healthy(&self, gen: u64) {
        let reset = self.with_health_if_current(gen, |h| {
            h.why = String::new();
            h.healthy = true;
            h.device_dead = false;
            h.last_patrol_at = None;
            self.carrier.begin_generation();
        })
        .is_some();
        if reset {
            self.bump_flip(); // 翻沿（新世代起点 = 全清）
        }
    }

    /// 传输健康翻回（tun-death-gate）：仅当分类为 `patrol` 且距上次判定 ≥ `min_gap`
    /// 时生效——写序「先清 why 再置 healthy」（与 begin_healthy 同序）；**分类槽为
    /// 设备/传输共用，仅清 patrol**——设备类诊断文案不受影响。翻回成功时**承载位
    /// 同步置 true**（同一临界区内、锁内查 device_dead——r10-C13；强证据翻回的全
    /// 部路径由此承载，观测快探成功不进本入口）。返回 `Some(距上次判定时长)` =
    /// 已翻回（调用方打判据行）；`None` = 未翻（分类不符或间隔未到——抖动循环由
    /// 最小间隔拦）。
    pub fn try_recover_transport_if_current(
        &self,
        gen: u64,
        min_gap: Duration,
    ) -> Option<Duration> {
        // 陈旧世代的重赛跑回执不得清新世代的 patrol（r5 M4；检查在锁内——r1 P0-5）。
        let mut recovered: Option<Duration> = None;
        let hit = self
            .with_health_if_current(gen, |h| {
                if h.why != "patrol" {
                    return;
                }
                let Some(at) = h.last_patrol_at else { return };
                let elapsed = Instant::now().saturating_duration_since(at);
                if elapsed < min_gap {
                    return; // 间隔未到不翻（防抖动循环）
                }
                h.why = String::new();
                h.healthy = true;
                self.carrier.mark_alive(); // 同一临界区（设备死在场自拦——r5-N1）
                recovered = Some(elapsed);
            })
            .is_some();
        if hit && recovered.is_some() {
            self.bump_flip(); // 翻沿（传输健康恢复）
        }
        recovered
    }

    /// **adopt 沿的 facade 收口**（评审 r1 P1-7 + r2 B2④）：岛内阶梯/赛跑采纳成功
    /// 时——若 facade 仍处传输不健康则清 patrol，并把承载位**置 true**（driver 的
    /// adopt 不再直接写 carrier——本入口是置 true 沿的唯一岛侧路径，锁内判
    /// device_dead 后置位）；防「healthy=false ∧ carrier=true」黏滞（该形态下读面
    /// 不再触发、stats 稳态门关闭、B 门 no-op——三类恢复入口全部失效，直到下一次
    /// 外部事件）。设备类不动（carrier 由设备死闩自拦）。带世代守卫（锁内检查）。
    pub fn on_adopt_if_current(&self, gen: u64) {
        let cleared = self
            .with_health_if_current(gen, |h| {
                let mut flipped = false;
                if h.why == "patrol" && !h.device_dead {
                    h.why = String::new();
                    h.healthy = true;
                    flipped = true;
                }
                // adopt = 承载位置 true 沿（评审 r2 B2④——facade 锁内完成；设备死
                // 在场由 mark_alive 的 CAS 判负）。
                let _ = self.carrier.mark_alive();
                flipped
            })
            == Some(true);
        if cleared {
            self.bump_flip(); // 翻沿（传输健康恢复——岛内自愈路径）
        }
    }

    /// 承载位「置 true」的独立入口（reconnect-oneshot D1-a：快探/重赛跑/恢复下推
    /// 成功路径在 patrol 分类已被他人清除或从未标记时仍需置位）。**同一临界区、
    /// 锁内判 device_dead 后写**（r10-C13 + 评审 r2 B2②：carrier 写收进 health 闭包
    /// ——旧实现在锁释放后执行，与并发的 mark_transport 交错可留不一致组合）。
    /// 返回 `false` = 设备死在场（判负）或陈旧世代。
    pub fn mark_carrier_alive_if_current(&self, gen: u64) -> bool {
        self.with_health_if_current(gen, |h| !h.device_dead && self.carrier.mark_alive())
            .unwrap_or(false)
    }

    /// **strong_recover 的单临界区合并面**（评审 r2 B2①）：快探/重赛跑/恢复下推的
    /// 同步成功路径——「清 patrol + 置 healthy（含翻回计时）」与「判 device_dead 后
    /// 置承载位 true」**一次** health 临界区完成。旧实现分两步加锁
    /// （`try_recover_transport_if_current` → 解锁 → `mark_carrier_alive_if_current`），
    /// 两步之间可被 `mark_transport_if_current` 插入写出 `why=patrol ∧ healthy=false
    /// ∧ carrier=false`、随后第二步又置 carrier true——终态 `patrol ∧ carrier=true`
    /// 黏滞（读面不触发、stats 稳态门关闭、B 门 no-op）。返回
    /// `(翻回计时〔None = 分类不符/间隔未到〕, 承载位是否置真)`；陈旧世代两步全判负。
    pub fn strong_recover_if_current(&self, gen: u64) -> (Option<Duration>, bool) {
        let mut recovered: Option<Duration> = None;
        let mut alive = false;
        let hit = self
            .with_health_if_current(gen, |h| {
                if h.why == "patrol" {
                    if let Some(at) = h.last_patrol_at {
                        let elapsed = Instant::now().saturating_duration_since(at);
                        // 调用方全部走 ZERO 间隔（强证据翻回）——elapsed ≥ 0 恒真，
                        // 保留间隔判定形态与 try_recover 同源。
                        if elapsed >= Duration::ZERO {
                            h.why = String::new();
                            h.healthy = true;
                            recovered = Some(elapsed);
                        }
                    }
                }
                // B2②/B2③：锁内判 device_dead 后写（CarrierFlag 单状态字——判定与
                // 写入原子；patrol 未清/设备死在场时 mark_alive 自拦）。
                alive = self.carrier.mark_alive();
            })
            .is_some();
        if !hit {
            return (None, false); // 陈旧世代：两步全判负（不写、不翻沿）
        }
        if recovered.is_some() {
            self.bump_flip(); // 翻沿（传输健康恢复）
        }
        (recovered, alive)
    }

    /// 读侧统一快照（r8-N2：`tun_status`/`tun_wait_flip`/观测面同锁一次读——
    /// 杜绝「healthy=true 且 why=patrol 残留」「healthy=true 且 carrier=false」
    /// 的混合中间态可见）。
    pub fn health_snapshot(&self) -> HealthSnapshot {
        let h = lock_unpoison(&self.health);
        HealthSnapshot {
            healthy: h.healthy,
            why: h.why.clone(),
            device_dead: h.device_dead,
            last_patrol_at: h.last_patrol_at,
            carrier: self.carrier.alive(),
        }
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
        lock_unpoison(&self.health).why == "patrol"
    }

    /// 健康位单点读（评审 r4-4：三处调用点直读原子位、语义注释散落——正是
    /// r3-M1b 把它误读成「在飞动作已胜出」的温床）。**语义 = 「本世代/本 episode
    /// 尚无未决不健康判定」**：世代起点 [`Self::begin_healthy`] 即置真、任何
    /// mark（传输/设备）置假、恢复翻回再置真。只可用于「无未决判定」的判断
    ///（如 recover_push 吸收循环的已恢复判据、pusher 的外部恢复同步门）；
    /// **不得**当「某次在飞动作获胜」的证据——一次死亡 episode 的首个 mark
    /// 往往正是调用方自己要发的那次上报，届时本位恒真。
    pub fn is_healthy(&self) -> bool {
        lock_unpoison(&self.health).healthy
    }

    /// 设备死位单点读（快照面的派生——不再直读原子）。
    pub fn device_dead(&self) -> bool {
        lock_unpoison(&self.health).device_dead
    }

    /// `running` 的单点合成（tun-death-gate：capi 面与对账快照共用——只反映设备面
    /// 存活：单飞锁 + **非设备死** + attached。传输类不健康〔patrol〕不再把
    /// running 拉零——调用方据 `unhealthyReason` 区分「需要重建（设备死）」与
    /// 「只需恢复传输」）。三个信号各自原子、合起来不是一致快照，转换瞬间可能
    /// 瞬时 0/1；调用方已用去抖。
    pub fn running(&self) -> bool {
        self.probe_running.load(Ordering::Acquire)
            && !self.device_dead()
            && self.stage.snapshot().stage == TunStage::Attached
    }

    /// 世代收尾（**执行体世代线程的一切退出路径必须调**；幂等——done 只关一次，
    /// 终态/放锁过世代守卫）：
    /// 1. 非 failed 终态回 idle 空码（failed 保留）；
    /// 2. 当前世代：分类 "stop" + 健康位 false + 置本世代停止位；
    /// 3. 关 attach 通道（迟到投递不再被接收）；
    /// 4. 发 done 信号（tun_stop 的等待面）；
    /// 5. **最后**放单飞锁（评审 r2 B2⑤：`probe_running=false` 是新世代 prepare 的
    ///    起跑可见面——放锁前完成本世代全部共享槽写。旧序「先放锁、后关槽/发 done」
    ///    存在跨代窗口：新 prepare 见锁已放 → `begin_generation` 起跑 → 旧 finish
    ///    随后关掉新世代 attach 槽/把新 done 置真（新 `tun_attach` 恒 -1、`tun_stop`
    ///    误判已收尾）。Release 存储与此前的全部共享槽写构成 happens-before：观察
    ///    到锁已放的新世代必然已见关槽与 done）。
    ///
    /// **评审 r3 B2⑥**：尾段在 [`Self::finish_gate`] 临界区内**复核 gen** 后执行——
    /// `is_current` 是锁外一次性读，`tun_stop` -2 强放锁后新 `begin_generation`
    /// 可能已起跑；stale `is_current=true` 的旧 finish 若无门户复核，会关掉新世代
    /// attach、把新 done 置真、再清掉新 probe_running（正是 B2 要消灭的形态）。
    /// 持门 ⇒ begin 无法插入 ⇒ 复核后写共享槽原子于换代判定。
    ///
    /// **评审 r4 B2-b**：stop 收口（`signal_stop`）与 `mark_unhealthy_if_current`
    /// 一并移入门内复核之下——旧位在门外（`is_current` 判断后、门之前），旧 finish
    /// 停在该点被放行时 `signal_stop` 会把**新世代**登记的停止位置真（mark 侧有
    /// 健康锁内 gen 守卫本就安全，同门只为单一口径）。
    pub fn finish_generation(&self, gen: u64) {
        let snap = self.stage.snapshot();
        if snap.stage != TunStage::Failed {
            self.stage
                .set_if_current(gen, TunStage::Idle, "", "", false);
        }
        // 受控交错缝（评审 r3 B2⑤）：卡在旧 `is_current` 判断位（门获取之前）——
        // stale true 的旧 finish 在此放行的确定性构造面（生产零开销）。
        #[cfg(test)]
        if let Some(h) = lock_unpoison(&self.finish_hook).clone() {
            h();
        }
        // stop 收口 / 健康标记 / done / attach 通道同过世代守卫（评审 r2-H2：done 与
        // attach 是 TunShared 单例槽——旧世代迟到的收尾若无条件写，会污染新世代：
        // ① attach 通道被关 ⇒ 新世代 tun_attach 恒 -1 直到 60s 死线；② done 被置真
        // ⇒ 新世代 tun_stop 命中 is_done() 快路径直接 0、从不 request_stop ⇒ stop
        // 位永假、probe_running 无人放——之后所有 prepare 恒 -1，只有进程重启能解。
        // Go 的 done/attachCh 是每世代 tunRun 自己的通道，无此面；单例槽形态下以
        // gen 比对等价表达）。评审 r3 B2⑥ + r4 B2-b：尾段（含 signal_stop/mark）
        // 在门内复核 gen 后写——窄窗封死。
        {
            let _gate = lock_unpoison(&self.finish_gate);
            if self.gen.load(Ordering::Acquire) == gen {
                // 受控交错缝（评审 r5-L2）：持门且 gen 复核通过之后、尾段写之前
                // ——B2-a 对照实验的确定性屏障（旧序下旧 finish 先到此处；新序下
                // 新 prepare 持门 ⇒ 旧 finish 到不了）。
                #[cfg(test)]
                if let Some(h) = lock_unpoison(&self.finish_tail_hook).clone() {
                    h();
                }
                // stop 收口（评审 r2-L12：Go finish 里 close(r.stop)——收尾路径不
                // 依赖 request_stop 先行；置位让世代的一切派生线程〔patrol/pusher/
                // stats/桥/hint〕从片界观察到收尾）
                self.signal_stop();
                // mark_unhealthy_if_current("stop") 内含 bump_flip（r3-L2：无需另调
                // ——世代结束的翻沿唤醒由它承担；在飞 wait_flip 不会挂满 2min 上限）。
                // **世代检查在 health 锁内**（评审 r1 P0-5：旧世代迟到的收尾不得把
                // "stop"/设备死写进新世代）。
                self.mark_unhealthy_if_current(gen, "stop");
                self.close_attach();
                let mut done = lock_unpoison(&self.done);
                if !*done {
                    *done = true;
                    self.done_cv.notify_all();
                }
                drop(done);
                // 评审 r2 B2⑤：放锁严格最后（见方法注释第 5 条）。
                self.probe_running.store(false, Ordering::Release);
            }
        }
    }

    /// `tun_stop` -2 强放锁的门内收口（评审 r5-L1）：与 `begin_prepare` 同用
    /// [`Self::finish_gate`]——**复核 gen + 单飞锁都在门内**，复核通过才写
    /// `probe_running=false`。旧实现「门外复查 → 裸 store」的 check-then-act
    /// 窄窗（复查后旧 finish 收尾、新 prepare 完成换代，裸 store 把新世代的
    /// 单飞锁清掉）由此封死。返回 `true` = 本调放锁成功（调用方按 -2 收）；
    /// `false` = 期间世代翻新/已自然收尾（**未写任何槽**，调用方按 -1 收）。
    pub fn force_release_if_current(&self, gen: u64) -> bool {
        let _gate = lock_unpoison(&self.finish_gate);
        if self.gen.load(Ordering::Acquire) != gen
            || !self.probe_running.load(Ordering::Acquire)
        {
            return false;
        }
        self.probe_running.store(false, Ordering::Release);
        true
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
    /// 随世代翻新；attach 通道与停止位槽也一并复位）。**gen 双写**（评审 r1 P0-5）：
    /// 先写零锁原子（读线程 step-0/`island_cmd_until` 立刻可见），再在 health 锁内
    /// 更新镜像 [`HealthState::gen`]——此后旧世代的迟到写调用在锁内被判定陈旧
    /// （`with_health_if_current`），新世代健康字段由随后的 [`Self::begin_healthy`]
    /// 复位。**评审 r3 B2⑥**：换代写在 [`Self::finish_gate`] 内完成——与旧 finish
    /// 的尾段互斥（finish 持门复核 gen ⇒ begin 要么已完成（finish 尾段跳过）、
    /// 要么等 finish 尾段落地后才起跑（尾段写随后被复位）——两种交错都无跨代污染）。
    /// 生产侧的 prepare 获取路径走 [`Self::begin_prepare`]（单飞 + 递增 + 本复位
    /// 三步同门，评审 r4 B2-a）；本入口**只保留给测试与直接换代构造面**
    /// （评审 r5-L5：`#[cfg(test)]` 收窄可见性——生产代码持有它即可绕过单飞获取
    /// 直接换代，误用会让 B2-a 的窗口以另一种形式回来）。
    #[cfg(test)]
    pub(crate) fn begin_generation(&self, gen: u64) {
        let _gate = lock_unpoison(&self.finish_gate);
        self.begin_generation_locked(gen);
    }

    /// [`Self::begin_generation`] 的门内体（调用方必须已持 [`Self::finish_gate`]）。
    fn begin_generation_locked(&self, gen: u64) {
        self.gen.store(gen, Ordering::Release);
        lock_unpoison(&self.health).gen = gen;
        self.stage.begin_generation(gen);
        *lock_unpoison(&self.done) = false;
        self.close_attach();
        *lock_unpoison(&self.stop_flag) = None;
    }

    /// **prepare 的换代收口原语**（评审 r4 B2-a）：`tun_prepare` 受理路径的
    /// 「单飞获取（swap）+ gen 递增 + 换代复位」三步收进 [`Self::finish_gate`]
    /// 同一临界区——与旧 finish 的尾段（含 signal_stop/mark，r4 B2-b）互斥。
    /// 旧序（swap 在门外）的窗口：`tun_stop` -2 强放锁后，新 prepare swap 成功
    /// 而 gen 尚未递增时，停在 `is_current` 判断后的旧 finish 尾段复核仍见旧 gen
    /// ⇒ 执行清锁——把新 prepare 刚拿到的单飞锁清成 false（`begin_generation`
    /// 不复位 probe_running），新世代从起跑就失去单飞锁（running()==false、
    /// tun_stop 误判「本就没在跑」）。同门后该窗口不存在：旧 finish 持门 ⇒ 新
    /// prepare 的 swap 等门；新 prepare 持门 ⇒ 旧 finish 复核见 gen 已翻新 ⇒
    /// 尾段跳过。解析/校验留在门外（门外失败 = 无锁可放；门外期间旧 finish 只会
    /// 清旧槽——尚无可清的新锁）。返回 `Some(gen)` = 受理起跑；`None` = 单飞
    /// 在世（调用方按 -1 收——swap 已把值写回 true，无需释放）。
    pub fn begin_prepare(&self) -> Option<u64> {
        let _gate = lock_unpoison(&self.finish_gate);
        if self.probe_running.swap(true, Ordering::AcqRel) {
            return None;
        }
        // 受控交错缝（评审 r4 B2-a）：卡在「swap 已成功、gen 尚未递增」处（持门）。
        #[cfg(test)]
        if let Some(h) = lock_unpoison(&self.prepare_hook).clone() {
            h();
        }
        let gen = self.gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.begin_generation_locked(gen);
        Some(gen)
    }

    /// 受控交错缝安装（评审 r3 B2⑤，`#[cfg(test)]`）：把 finish 钩子挂上——守卫
    /// drop 时卸载（防跨用例泄漏）。
    #[cfg(test)]
    pub(crate) fn install_finish_hook(
        self: &Arc<Self>,
        h: Arc<dyn Fn() + Send + Sync>,
    ) -> FinishHookGuard {
        *lock_unpoison(&self.finish_hook) = Some(h);
        FinishHookGuard(Arc::clone(self))
    }

    /// 受控交错缝安装（评审 r4 B2-a，`#[cfg(test)]`）：把 prepare 钩子挂上——守卫
    /// drop 时卸载（防跨用例泄漏）。
    #[cfg(test)]
    pub(crate) fn install_prepare_hook(
        self: &Arc<Self>,
        h: Arc<dyn Fn() + Send + Sync>,
    ) -> PrepareHookGuard {
        *lock_unpoison(&self.prepare_hook) = Some(h);
        PrepareHookGuard(Arc::clone(self))
    }

    /// 受控交错缝安装（评审 r5-L2，`#[cfg(test)]`）：finish 尾段屏障——守卫 drop
    /// 时卸载（防跨用例泄漏）。
    #[cfg(test)]
    pub(crate) fn install_finish_tail_hook(
        self: &Arc<Self>,
        h: Arc<dyn Fn() + Send + Sync>,
    ) -> FinishTailHookGuard {
        *lock_unpoison(&self.finish_tail_hook) = Some(h);
        FinishTailHookGuard(Arc::clone(self))
    }

    /// 受控交错缝安装（评审 r5-L1，`#[cfg(test)]`）：`tun_stop` 强放前的屏障
    /// ——守卫 drop 时卸载（防跨用例泄漏）。
    #[cfg(test)]
    pub(crate) fn install_stop_force_hook(
        self: &Arc<Self>,
        h: Arc<dyn Fn() + Send + Sync>,
    ) -> StopForceHookGuard {
        *lock_unpoison(&self.stop_force_hook) = Some(h);
        StopForceHookGuard(Arc::clone(self))
    }

    /// 触发 `tun_stop` 强放前受控缝（评审 r5-L1，`#[cfg(test)]`——`mod.rs` 的
    /// `tun_stop` 调用；生产构建不存在）。
    #[cfg(test)]
    pub(crate) fn invoke_stop_force_hook(&self) {
        if let Some(h) = lock_unpoison(&self.stop_force_hook).clone() {
            h();
        }
    }
}

/// finish 钩子的卸载守卫（评审 r3 B2⑤）。
#[cfg(test)]
pub(crate) struct FinishHookGuard(Arc<TunShared>);

#[cfg(test)]
impl Drop for FinishHookGuard {
    fn drop(&mut self) {
        *lock_unpoison(&self.0.finish_hook) = None;
    }
}

/// prepare 钩子的卸载守卫（评审 r4 B2-a）。
#[cfg(test)]
pub(crate) struct PrepareHookGuard(Arc<TunShared>);

#[cfg(test)]
impl Drop for PrepareHookGuard {
    fn drop(&mut self) {
        *lock_unpoison(&self.0.prepare_hook) = None;
    }
}

/// finish 尾段屏障的卸载守卫（评审 r5-L2）。
#[cfg(test)]
pub(crate) struct FinishTailHookGuard(Arc<TunShared>);

#[cfg(test)]
impl Drop for FinishTailHookGuard {
    fn drop(&mut self) {
        *lock_unpoison(&self.0.finish_tail_hook) = None;
    }
}

/// `tun_stop` 强放前屏障的卸载守卫（评审 r5-L1）。
#[cfg(test)]
pub(crate) struct StopForceHookGuard(Arc<TunShared>);

#[cfg(test)]
impl Drop for StopForceHookGuard {
    fn drop(&mut self) {
        *lock_unpoison(&self.0.stop_force_hook) = None;
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
        sh.begin_healthy(1);
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
        assert!(!sh.is_healthy());
        assert_eq!(sh.health_snapshot().why, "stop");
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
        let _rx2 = sh.attach_receiver(2); // 新世代注册自己的 attach 接收端
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

    /// **判据（评审 r2 B2⑦）**：attach 接收端注册的世代守卫——旧 warmup 线程的
    /// 迟到注册不得覆盖新世代已注册的 sender（否则新世代 `tun_attach` 恒 -1）。
    #[test]
    fn stale_attach_receiver_registration_does_not_clobber_new_generation() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.begin_generation(2); // 新世代接管
        let rx2 = sh.attach_receiver(2); // 新世代先注册
        let _rx1 = sh.attach_receiver(1); // 旧 warmup 线程迟到注册
        assert!(
            sh.deliver_fd(33),
            "迟到注册不得顶掉新世代 sender（投递可达）"
        );
        assert_eq!(rx2.recv(), Ok(33), "fd 落进新世代的接收端");
        // 槽位空闲（尚未有人注册）时陈旧注册可安装——随后新世代注册顶替（形态同
        // set_stop_flag_if_current 的守卫）。
        sh.begin_generation(3);
        sh.close_attach(); // 模拟「槽位尚空」
        let _rx_old = sh.attach_receiver(1);
        let rx3 = sh.attach_receiver(3);
        assert!(sh.deliver_fd(44));
        assert_eq!(rx3.recv(), Ok(44), "新世代注册顶替陈旧注册");
    }

    /// **判据（评审 r2 B2⑤，r3 订正口径）**：旧 finish 与新 begin 并发——新世代
    /// 起跑的可见面是 `probe_running=false`（Release），旧 finish 的关槽/done 写
    /// **全部在其前**完成（happens-before）⇒ 观察到锁已放后立刻起跑的新世代，其
    /// attach 槽不被旧 finish 关、done 不被置真。
    ///
    /// **r3 事实订正**：本用例**不是受控交错**——`probe_running=false` 轮询观察点
    /// 在两序下都晚于关槽/done（评审 r3 实测旧序 5/5 绿，无回归力）；它验证的是
    /// 新序下的 happens-before 可见面（行为验证）。**回归力由
    /// [`Self::finish_parked_at_is_current_then_new_generation_tail_is_gated`]
    /// 承载**（钩子受控交错——旧实现必红）。r4 B2-b 补「新世代停止位不得被置真」
    /// 不变量断言（非受控交错面——受控面的回归力在下方用例）。
    #[test]
    fn finish_releases_lock_last_new_generation_slot_survives() {
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        sh.probe_running.store(true, Ordering::Release);
        let _rx1 = sh.attach_receiver(1);
        // 旧世代的 finish 在并发线程上跑；新世代起跑线程**轮询等待锁已放**（Acquire
        // 读到 Release 存储的 false ⇒ finish 此前的全部共享槽写已可见）后立即
        // begin_generation + 注册接收端。⚠ r3 订正：轮询观察点在两序下都晚于关槽/
        // done（非受控交错——回归力见下方钩子用例）。
        let fin = {
            let sh2 = Arc::clone(&sh);
            std::thread::spawn(move || sh2.finish_generation(1))
        };
        while sh.probe_running.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
        }
        // 新世代起跑（生产受理原语：门内 swap + gen 递增 + 换代复位——评审 r4 B2-a）
        let gen2 = sh.begin_prepare().expect("锁已放 ⇒ 受理");
        assert_eq!(gen2, 2);
        let _rx2 = sh.attach_receiver(2);
        let f2 = Arc::new(AtomicBool::new(false));
        sh.set_stop_flag_if_current(2, Arc::clone(&f2));
        fin.join().expect("旧 finish 收口");
        assert!(
            sh.deliver_fd(7),
            "新世代 attach 槽不被旧 finish 关（fd 可投递）"
        );
        assert_eq!(_rx2.recv(), Ok(7));
        assert!(
            !sh.is_done(),
            "新世代 done 不被旧 finish 置真（tun_stop 不会误判已收尾）"
        );
        assert!(
            sh.probe_running.load(Ordering::Acquire),
            "新世代持有自己的单飞锁"
        );
        assert!(
            !f2.load(Ordering::Acquire),
            "新世代停止位不被旧 finish 置真（r4 B2-b 不变量）"
        );
    }

    /// **判据（评审 r3 B2⑤——真受控交错；r4 B2-a/B2-b 扩面）**：旧 finish 经
    /// `#[cfg(test)]` 钩子停在旧 `is_current` 判断位（尾段之前）→ 新世代经生产
    /// 受理原语 `begin_prepare` 起跑（门内 swap + gen 递增 + 换代复位 + attach
    /// 注册 + 新停止位登记）→ 放行旧 finish ⇒ 尾段在 `finish_gate` 内**复核
    /// gen**：新世代 attach 槽不被关、done 不被置真、probe_running 不被清、
    /// **停止位不被置真**（r4 B2-b——旧实现 signal_stop 在门外，该断言必红）。
    /// 旧实现（stale `is_current` 直写尾段，无门户复核）在该交错下**必红**
    /// （对照实验在案——评审 r3 指出旧测试面对旧序 5/5 绿，本用例为其替换）。
    #[test]
    fn finish_parked_at_is_current_then_new_generation_tail_is_gated() {
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        sh.probe_running.store(true, Ordering::Release);
        let _rx1 = sh.attach_receiver(1);
        let (park_tx, park_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = Mutex::new(go_rx);
        let ptx = park_tx;
        let _un = sh.install_finish_hook(Arc::new(move || {
            let _ = ptx.send(());
            let _ = go_rx.lock().unwrap().recv();
        }));
        let fin = {
            let sh2 = Arc::clone(&sh);
            std::thread::spawn(move || sh2.finish_generation(1))
        };
        park_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("旧 finish 已停在 is_current 之后（尾段之前）");
        // 新世代起跑（生产受理原语——评审 r4 B2-a：swap/递增/复位同门；旧 finish
        // 停在门外 ⇒ begin_prepare 直接完成）。先注入 tun_stop -2 的强放锁等价
        // （旧 finish 停住 ⇒ tun_stop 烧满 STOP_WAIT ⇒ probe_running=false——
        // 否则 swap 见旧锁在世直接 -1）。随后注册 attach 接收端与新停止位。
        sh.probe_running.store(false, Ordering::Release);
        let gen2 = sh.begin_prepare().expect("-2 强放后新 prepare 受理");
        assert_eq!(gen2, 2);
        let rx2 = sh.attach_receiver(2);
        let f2 = Arc::new(AtomicBool::new(false));
        sh.set_stop_flag_if_current(2, Arc::clone(&f2));
        // 放行旧 finish：带着 stale is_current=true 恢复
        let _ = go_tx.send(());
        fin.join().expect("旧 finish 收口");
        assert!(
            sh.deliver_fd(9),
            "新世代 attach 槽不被旧 finish 关（fd 可投递）"
        );
        assert_eq!(rx2.recv(), Ok(9), "fd 落进新世代接收端");
        assert!(!sh.is_done(), "新世代 done 不被旧 finish 置真");
        assert!(
            sh.probe_running.load(Ordering::Acquire),
            "新世代单飞锁不被旧 finish 清"
        );
        assert!(
            !f2.load(Ordering::Acquire),
            "新世代停止位不被旧 finish 置真（r4 B2-b——signal_stop 门外旧序必红）"
        );
    }

    /// **判据（评审 r2 B2①/B2③；r3 口径订正）**：strong_recover 与 mark_transport
    /// 并发——终态（与一切中间快照）不得出现 `patrol ∧ carrier=true` 黏滞组合
    /// （读面不触发、stats 稳态门关闭、B 门 no-op 的形态）。合并单临界区后两类写
    /// 互斥；观察线程的 health_snapshot 是单锁读（carrier 读在 health 锁内）⇒ 无
    /// 撕裂快照。**形态 = 压力回归测试**（评审 r3 实测旧两步加锁下 2/3 红——有
    /// 回归力，但非确定性受控交错；一次绿不构成无黏滞证明）。
    #[test]
    fn strong_recover_and_mark_transport_never_leave_patrol_with_carrier_true() {
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        sh.begin_healthy(1);
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        // 干扰线程：循环 B 门/观测沿标记
        {
            let sh2 = Arc::clone(&sh);
            let stop2 = Arc::clone(&stop);
            handles.push(std::thread::spawn(move || {
                let mut i = 0u32;
                while !stop2.load(Ordering::Acquire) {
                    let _ = sh2.mark_transport_if_current(1);
                    i += 1;
                    if i > 4000 {
                        break;
                    }
                }
            }));
        }
        // 翻回线程：循环 strong_recover（合并临界区）
        {
            let sh2 = Arc::clone(&sh);
            let stop2 = Arc::clone(&stop);
            handles.push(std::thread::spawn(move || {
                let mut i = 0u32;
                while !stop2.load(Ordering::Acquire) {
                    let _ = sh2.strong_recover_if_current(1);
                    i += 1;
                    if i > 4000 {
                        break;
                    }
                }
            }));
        }
        // 观察线程：单锁快照断言不变量
        let bad = Arc::new(AtomicU64::new(0));
        {
            let sh2 = Arc::clone(&sh);
            let stop2 = Arc::clone(&stop);
            let bad2 = Arc::clone(&bad);
            handles.push(std::thread::spawn(move || {
                let mut i = 0u32;
                while !stop2.load(Ordering::Acquire) {
                    let hs = sh2.health_snapshot();
                    if hs.why == "patrol" && hs.carrier {
                        bad2.fetch_add(1, Ordering::Relaxed);
                    }
                    i += 1;
                    if i > 8000 {
                        break;
                    }
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(150));
        stop.store(true, Ordering::Release);
        for h in handles {
            h.join().expect("stress 收口");
        }
        assert_eq!(bad.load(Ordering::Relaxed), 0, "黏滞组合零观测");
        // 终态复核：无论落点在标记侧还是翻回侧，组合一致（patrol ⇒ carrier=false）。
        let hs = sh.health_snapshot();
        assert!(
            hs.why != "patrol" || !hs.carrier,
            "终态无黏滞：why={:?} carrier={}",
            hs.why,
            hs.carrier
        );
    }

    /// attach 通道：注册 → 投递 → 收口后迟到投递 false。
    #[test]
    fn attach_channel_lifecycle() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        let rx = sh.attach_receiver(1);
        assert!(sh.deliver_fd(90));
        assert_eq!(rx.recv(), Ok(90));
        sh.finish_generation(1);
        assert!(!sh.deliver_fd(91), "收尾后投递不再被接收");
        // 通道满（缓冲 1）：第二次投递 false
        sh.begin_generation(2);
        let _rx2 = sh.attach_receiver(2);
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
        sh.begin_healthy(1);
        sh.begin_generation(2); // 新世代接管
        sh.mark_unhealthy_if_current(1, "fd"); // 旧世代的 fd 错误回调
        assert!(sh.is_healthy(), "旧世代不得标记新世代");
        assert!(sh.health_snapshot().why.is_empty());
        sh.mark_unhealthy_if_current(2, "fd"); // 当前世代照常
        assert!(!sh.is_healthy());
        assert_eq!(sh.health_snapshot().why, "fd");
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
        assert!(!sh.is_healthy(), "传输健康位为假");
        sh.begin_generation(2);
        sh.begin_healthy(2);
        sh.probe_running.store(true, Ordering::Release);
        sh.stage.set_if_current(2, TunStage::Attached, "", "", true);
        sh.mark_unhealthy("fd");
        assert!(!sh.running(), "设备死把 running 拉零");
        assert!(sh.device_dead());
    }

    /// tun-death-gate：翻回仅对 patrol 生效（分类不符不动）；设备类文案不被抹。
    #[test]
    fn recover_keeps_device_class_why() {
        let sh = TunShared::new();
        sh.mark_unhealthy("fd");
        assert!(sh.try_recover_transport_if_current(1, Duration::ZERO).is_none());
        assert_eq!(
            sh.health_snapshot().why,
            "fd",
            "设备类分类文案不得被翻回抹掉"
        );
        assert!(!sh.is_healthy());
        // 未知分类保守归设备：同样不翻。
        sh.begin_healthy(1);
        sh.mark_unhealthy("fd-timeout");
        assert!(sh.try_recover_transport_if_current(1, Duration::ZERO).is_none());
        assert!(sh.device_dead(), "未知值归设备类");
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
        assert_eq!(sh.health_snapshot().why, "patrol");
        let got = sh.try_recover_transport_if_current(1, Duration::ZERO);
        assert!(got.is_some(), "间隔放宽后翻回");
        assert!(sh.is_healthy());
        assert!(
            sh.health_snapshot().why.is_empty(),
            "翻回后分类已清（写序：先清 why）"
        );
    }

    /// **不变量（评审 r4-1，r3-M1b 教训）**：健康位在「本 episode 首个 mark
    /// 之前」按构造恒真——B 门时刻读它必然得真。它只能表达「无未决不健康
    /// 判定」（M1a 的用法），不得当「某次在飞动作已胜出」的证据（M1b 的误用：
    /// 把常态 B 门上报整条吞掉，重开 ~3min 空窗）。
    #[test]
    fn healthy_bit_is_true_before_first_mark_of_episode() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        assert!(
            sh.is_healthy(),
            "世代起点即真——episode 首个 mark（通常是 B 门上报）之前无任何未决判定"
        );
        sh.mark_unhealthy("patrol");
        assert!(!sh.is_healthy(), "mark 后才为假");
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
            !sh.is_healthy(),
            "健康位仍假——不得按恢复收口（rc=0）"
        );
        assert!(sh.device_dead(), "设备死位在册");
    }

    /// **判据（评审 r1 P0-5 整改）**：gen 检查在 health 临界区内——旧 gen 的一切
    /// 写调用（mark_unhealthy / mark_transport / try_recover / mark_carrier_alive /
    /// on_adopt）在**新世代 `begin_healthy(2)` 之后**到达 ⇒ 全部 no-op（等价受控
    /// 交错：检查与写入在同一临界区原子完成，「检查通过后被换代」的窗口不存在——
    /// 本用例以「时间上晚于 begin_healthy(2) 到达的旧 gen 调用」钉该不变量）。
    #[test]
    fn stale_gen_writes_after_begin_healthy_are_noop() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.begin_healthy(1);
        // 新世代接管并完成健康复位
        sh.begin_generation(2);
        sh.begin_healthy(2);
        let flips_before = sh.flip_seq();
        // 旧 gen=1 的五类写调用（在 begin_healthy(2) 之后到达）：
        assert!(!sh.mark_transport_if_current(1), "旧 mark_transport no-op");
        sh.mark_unhealthy_if_current(1, "fd"); // 设备类迟到上报
        sh.mark_unhealthy_if_current(1, "patrol"); // B 门迟到标记
        assert!(sh.try_recover_transport_if_current(1, Duration::ZERO).is_none());
        assert!(!sh.mark_carrier_alive_if_current(1), "旧置 true 判负");
        sh.on_adopt_if_current(1); // 旧 adopt 沿通知
        let hs = sh.health_snapshot();
        assert!(hs.healthy, "新世代健康位不被旧写翻假");
        assert!(hs.why.is_empty(), "分类槽不被旧写污染");
        assert!(!hs.device_dead, "设备死位不被旧写置位");
        assert_eq!(sh.flip_seq(), flips_before, "零翻沿（全部 no-op）");
        // 对照：gen=2 照常写
        assert!(sh.mark_transport_if_current(2), "当前世代照常标记");
        assert_eq!(sh.health_snapshot().why, "patrol");
    }

    /// **判据（评审 r1 P0-5 整改，并发面）**：换代与旧写并发 stress——新世代完成
    /// `begin_healthy` 后的健康态**永不**被旧 gen 调用污染（检查在锁内 ⇒ 无窗口）。
    #[test]
    fn generation_cycle_never_polluted_by_stale_writers() {
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        sh.begin_healthy(1);
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        // 旧写者：gen=1 的 fd/patrol/翻回循环
        for _ in 0..2 {
            let sh2 = Arc::clone(&sh);
            let stop2 = Arc::clone(&stop);
            handles.push(std::thread::spawn(move || {
                let mut i = 0u32;
                while !stop2.load(Ordering::Acquire) {
                    sh2.mark_unhealthy_if_current(1, if i.is_multiple_of(2) { "fd" } else { "patrol" });
                    let _ = sh2.mark_transport_if_current(1);
                    let _ = sh2.try_recover_transport_if_current(1, Duration::ZERO);
                    i += 1;
                    if i > 4000 {
                        break;
                    }
                }
            }));
        }
        // 换代者：gen 2→4 连续推进（每代 begin_generation + begin_healthy）
        let sh3 = Arc::clone(&sh);
        handles.push(std::thread::spawn(move || {
            for g in 2..=4u64 {
                sh3.begin_generation(g);
                sh3.begin_healthy(g);
            }
        }));
        for h in handles {
            h.join().expect("stress 收口");
        }
        stop.store(true, Ordering::Release);
        sh.begin_generation(5);
        sh.begin_healthy(5);
        let hs = sh.health_snapshot();
        assert!(hs.healthy && hs.why.is_empty() && !hs.device_dead,
            "终态干净：healthy={} why={:?} device_dead={}", hs.healthy, hs.why, hs.device_dead);
    }

    /// **判据（评审 r1 P1-7 的 facade 半边 + r2 B2④）**：岛内 adopt 沿 ⇒ 清残留
    /// patrol **并置承载位 true**（置 true 沿由本入口在锁内完成——driver 不再直写）——
    /// 防「healthy=false ∧ carrier=true」黏滞（读面不再触发、stats 稳态门关闭、
    /// B 门 no-op 三类恢复入口全失效的形态）。设备类/从未标记/陈旧 gen 的 patrol
    /// 不动（carrier 置位独立成立——设备死由 mark_alive 自拦）。
    #[test]
    fn on_adopt_clears_residual_patrol() {
        let sh = TunShared::new();
        sh.begin_generation(1);
        sh.begin_healthy(1);
        // 从未标记：on_adopt 仍置 carrier true（adopt = 置 true 沿——B2④），无翻沿
        let flips0 = sh.flip_seq();
        sh.on_adopt_if_current(1);
        assert_eq!(sh.flip_seq(), flips0, "从未标记——无翻沿");
        assert!(sh.carrier.alive(), "adopt 沿置承载位 true（无 patrol 在册）");
        // patrol 在册（模拟 stats 连败标记后岛内阶梯自行恢复）：
        assert!(sh.mark_transport_if_current(1));
        assert!(!sh.carrier.alive(), "标记同步拉假承载位");
        // adopt 沿通知（唯一置 true 路径——B2④）⇒ 状态翻回 + carrier true 一步完成：
        sh.on_adopt_if_current(1);
        assert!(sh.is_healthy(), "healthy 翻真");
        assert!(!sh.transport_unhealthy(), "patrol 已清");
        assert!(sh.carrier.alive(), "承载位同临界区置 true（无中间态）");
        // 设备类：on_adopt 不得抹设备文案；carrier 置位被设备死闩自拦
        sh.mark_unhealthy("fd");
        assert!(!sh.carrier.alive());
        sh.on_adopt_if_current(1);
        assert_eq!(sh.health_snapshot().why, "fd", "设备类文案不被 adopt 沿抹掉");
        assert!(!sh.is_healthy());
        assert!(
            !sh.carrier.alive(),
            "设备死在场：置 true 判负（r5-N1——mark_alive CAS 自拦）"
        );
        // 陈旧 gen：no-op（换新世代后旧 adopt 通知——carrier/健康都不动）
        sh.begin_generation(2);
        sh.begin_healthy(2);
        sh.mark_transport_if_current(2);
        let flips = sh.flip_seq();
        sh.on_adopt_if_current(1);
        assert_eq!(sh.flip_seq(), flips, "陈旧 gen 的 adopt 通知 no-op");
        assert!(sh.transport_unhealthy(), "新世代 patrol 不被旧通知清除");
        assert!(
            !sh.carrier.alive(),
            "陈旧 gen 的 adopt 通知不得置新世代 carrier"
        );
    }
}
