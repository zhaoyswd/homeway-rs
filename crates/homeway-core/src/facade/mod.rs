//! homeway-core facade：App 核门面（20 个 NAPI 导出面的 Rust API——R7-7c）。
//!
//! 语义真源 = `baseline:clientcore/cmd/clientcore/` 的 `//export` 族（tier:AGENTS.md
//! 原生契约四处同步清单；7b 拍板 = C-ABI 复刻同名符号，本模块即符号面之下的纯 Rust
//! API——**不接 NAPI 绑定**，extern "C" 壳是 R7 第 2 棒的活）。
//!
//! 20 个面（tier:AGENTS.md 清单）：
//! - tun 生命周期 8：`tun_prepare/tun_attach/tun_status/tun_stop/tun_recover/
//!   tun_running/tun_set_foreground/tun_set_activity`；
//! - portfwd 1：`tun_set_port_forwards`；
//! - files/term/probe/service 面 11：`files_call/term_call/probe_addr/probe_reach/
//!   speedtest_start/speedtest_status/speedtest_cancel/service_start/service_stop/
//!   service_status/version`。
//!
//! 返回码契约（probe_lib.go 头注释逐字）：
//! - `tun_prepare`：0 开始 / -1 忙 / -2 日志打不开 / -3 参数错（含 token 空）；
//! - `tun_attach`：0 接管 / -1 无 ready 世代 / -3 fd≤0 / -4 接管失败 / -5 轮询超时；
//! - `tun_stop`：0 已停 / -1 等超时 / -2 超时后强制放锁；
//! - `tun_recover`：0 某档通过 / -1 走完未恢复 / -2 无 attached 隧道 / -3·-4 本地动作 /
//!   -9 异步壳异常（壳面产生，本面不产）。
//!
//! tun 域的数据面执行体经 [`TunExecutor`] 注入——第 2 棒接真 wgcore hub（TUN fd →
//! L3 直通）；本棒以 trait 契约 + 状态机全路径单测钉语义（两阶段/世代/单飞/停等）。

pub mod bridge_host;
pub mod demand;
pub mod events;
pub mod files_op;
pub mod portfwd;
pub mod probe_json;
pub mod service_op;
pub mod speedtest_op;
pub mod stage;
pub mod term_op;
pub mod tun_status;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;

use demand::DemandSignals;
use stage::{StageMachine, TunStage};
use tun_status::{RunnerIn, TransportIn, TunStatusInput};

/// 暖机窗口：等注册确认为止（软失败——超时仍 ready、可 attach 自愈）。
pub const WARM_TIMEOUT: Duration = Duration::from_secs(20);
/// prepare 成功后等 attach 的上限（唯一合法长间隔 = 系统侧建接口/授权；到点世代
/// 自行收工放锁，防无人推进的世代占锁）。
pub const ATTACH_DEADLINE: Duration = Duration::from_secs(60);
/// stop 收工等待预算（tunStopWait 的 3s；超时 -1，且期间无新世代 ⇒ 强制放锁 -2）。
pub const STOP_WAIT: Duration = Duration::from_secs(3);

/// tunConfig（hostsession.Config 同形；JSON 键 = Go json tag）。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct TunConfigJson {
    #[serde(default)]
    pub mtu: i64,
    #[serde(default)]
    pub out: String,
    #[serde(default)]
    pub dial_ms: i64,
    #[serde(default)]
    pub stats_secs: i64,
    #[serde(default)]
    pub endpoint_cache_dir: String,
    #[serde(default)]
    pub identity_dir: String,
    #[serde(default)]
    pub diag_fd_secs: i64,
    #[serde(default)]
    pub tz_offset_minutes: i64,
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub port_forwards: Vec<portfwd::PortForwardRule>,
}

/// tun 域执行体（世代生命周期里需要外部世界的动作）。第 2 棒接 wgcore hub；
/// 本棒测试用受控实现。
pub trait TunExecutor: Send + Sync {
    /// 暖机一个世代（不碰 TUN fd；完成时把阶段机置 ready〔meowed〕/failed）。
    /// 返回 Err = 同步硬失败（世代立刻 failed）。
    fn warmup(&self, cfg: &TunConfigJson, stage: &StageMachine, gen: u64) -> Result<(), String>;
    /// attach：把 TUN fd 交给已就绪的世代接管数据面。
    fn attach(&self, fd: i32, mtu: u32) -> Result<(), String>;
    /// 请求世代收工（幂等信号）。
    fn request_stop(&self);
    /// 等世代真正退出（≤ budget）；返回是否已退。
    fn wait_stopped(&self, budget: Duration) -> bool;
    /// 恢复阶梯入口（from 起跑档位；rc 契约见 session::LadderRc::as_rc）。
    fn recover(&self, from: i64, cause: &str) -> i32;
    /// 世代健康位快照（fd 循环异常会翻 false——「界面显示已连接、隧道其实已死」的漂谎防线）。
    fn healthy(&self) -> bool;
    /// 不健康原因分类（patrol=传输类/fd/panic/stop=设备层；空 = 健康）。
    fn unhealthy_why(&self) -> String;
    /// runner/transport 状态块（tunStatusJSON 的条件键源；None = 无 runner 期）。
    fn runner(&self) -> Option<RunnerIn>;
    fn transport(&self) -> Option<TransportIn>;
    /// portfwd 整表热替换承载（默认 -1：无承载 = 改动随下次连接的 tunConfig 生效）。
    fn request_port_forwards(&self, _rules: Vec<portfwd::PortForwardRule>) -> i32 {
        -1
    }
}

/// 无操作执行体（默认：一切本地动作失败——prepare 只到参数校验、attach 恒败）。
/// 真壳在第 2 棒注入；这保证 facade 独立可测。
#[derive(Debug, Default)]
pub struct NoopTunExecutor {
    pub healthy: AtomicBool,
}

impl TunExecutor for NoopTunExecutor {
    fn warmup(&self, _cfg: &TunConfigJson, stage: &StageMachine, _gen: u64) -> Result<(), String> {
        stage.set(TunStage::Failed, "core", "无执行体（NoopTunExecutor）", false);
        Err("无执行体".to_owned())
    }
    fn attach(&self, _fd: i32, _mtu: u32) -> Result<(), String> {
        Err("无执行体".to_owned())
    }
    fn request_stop(&self) {}
    fn wait_stopped(&self, _budget: Duration) -> bool {
        true
    }
    fn recover(&self, _from: i64, _cause: &str) -> i32 {
        -2
    }
    fn healthy(&self) -> bool {
        self.healthy.load(Ordering::Relaxed)
    }
    fn unhealthy_why(&self) -> String {
        String::new()
    }
    fn runner(&self) -> Option<RunnerIn> {
        None
    }
    fn transport(&self) -> Option<TransportIn> {
        None
    }
}

/// tun 域状态（单飞锁 + 世代 + 阶段机 + 前台位）。
struct TunDomain {
    /// 单飞锁（probeRunning）：世代在世期间持有，收工放。
    probe_running: AtomicBool,
    /// 世代计数（每次 prepare 递增；阶段机/attach 都比对它防陈旧）。
    gen: std::sync::atomic::AtomicU64,
    stage: StageMachine,
    /// 当前世代的 attach 截止（ready 时起表；过线世代自收工放锁）。
    attach_deadline: Mutex<Option<Instant>>,
    /// 前台位（SetForeground 的返回值语义 = 上一状态）。
    foreground: AtomicBool,
    /// 「回到前台」转变时的补探钩子（false→true 踢一次立即探测；只影响时机）。
    foreground_kick: Mutex<Option<Box<dyn Fn() + Send>>>,
    /// 收工强制放锁后的「孤儿世代」标记（旧世代 goroutine 可能还在收尾——它写阶段
    /// 前必须过世代比对，见 StageMachine::set_if_current）。
    orphan_alive: AtomicBool,
}

impl TunDomain {
    fn new() -> Self {
        TunDomain {
            probe_running: AtomicBool::new(false),
            gen: std::sync::atomic::AtomicU64::new(0),
            stage: StageMachine::new(),
            attach_deadline: Mutex::new(None),
            foreground: AtomicBool::new(false),
            foreground_kick: Mutex::new(None),
            orphan_alive: AtomicBool::new(false),
        }
    }
}

/// App 核门面（20 导出面的 Rust API；线程安全）。
pub struct ClientCore {
    tun: TunDomain,
    pub demand: DemandSignals,
    pub files: files_op::FilesOps,
    executor: Mutex<Arc<dyn TunExecutor>>,
    /// 服务会话域（rc 门与状态面在 service_op；真 Session/桥挂接在 7d 装配位）。
    pub service: service_op::ServiceDomain,
    /// 状态推送的最小等价面（可轮询事件队列 + 冷启动快照；真 IPC 推送在 ArkTS 侧）。
    pub events: events::EventHub,
    /// 服务桥宿主（`<filesDir>/bridge/*.sock` 三座；None = 未装配——第 2 棒随真
    /// Session 注入，本棒接口 + 单测已齐）。
    pub service_bridge: Mutex<Option<Arc<bridge_host::BridgeHost>>>,
}

impl Default for ClientCore {
    fn default() -> Self {
        Self::new(Arc::new(NoopTunExecutor::default()))
    }
}

impl ClientCore {
    pub fn new(executor: Arc<dyn TunExecutor>) -> Self {
        ClientCore {
            tun: TunDomain::new(),
            demand: DemandSignals::new(),
            files: files_op::FilesOps::new(),
            executor: Mutex::new(executor),
            service: service_op::ServiceDomain::new(),
            events: events::EventHub::new(),
            service_bridge: Mutex::new(None),
        }
    }

    /// 装配服务桥（7d：服务会话宿主形态——`service_start` 受理后由装配方注入；
    /// 桥状态经 `bridge_status` 并入 serviceStatusJSON 的 bridge 四键）。
    pub fn attach_service_bridge(&self, host: Option<Arc<bridge_host::BridgeHost>>) {
        *self.service_bridge.lock().expect("服务桥锁中毒") = host;
    }

    /// 桥状态快照（bridge 四键源；未装配 = 全空——serviceStatusJSON 的缺省形态）。
    pub fn bridge_status(&self) -> bridge_host::BridgeStatus {
        self.service_bridge
            .lock()
            .expect("服务桥锁中毒")
            .as_ref()
            .map(|h| h.status())
            .unwrap_or_default()
    }

    /// 替换执行体（第 2 棒装配真 hub 用；测试注入受控实现用）。
    pub fn set_executor(&self, e: Arc<dyn TunExecutor>) {
        *self.executor.lock().expect("executor 锁中毒") = e;
    }

    fn executor(&self) -> Arc<dyn TunExecutor> {
        Arc::clone(&self.executor.lock().expect("executor 锁中毒"))
    }

    // ---- ① ClientCoreVersion ----

    /// 版本串（Go `tier core %s (%s, c-shared)` 同形；构建方注入版本，回落 devel 形态。
    /// runtime 段 = rustc 版本——Go 面是 runtime.Version()，Rust 面等价信息位）。
    pub fn version() -> String {
        let v = option_env!("HOMEWAY_CORE_VERSION").unwrap_or("");
        let v = if v.is_empty() { "(devel)" } else { v };
        format!("tier core {v} (rust {}, c-shared)", rustc_semver())
    }

    // ---- ② ClientCoreTunPrepare（tunBegin）----

    /// 两阶段启动第一阶段：暖机（不碰 TUN fd）。调用后轮询 `tun_status` 直到
    /// state=ready（或 failed）；就绪后必须在 `ATTACH_DEADLINE` 内 attach。
    pub fn tun_prepare(&self, cfg_json: &str, log_path_ok: bool) -> i32 {
        // 单飞：在世世代期间拒绝（-1 忙）
        if self.tun.probe_running.swap(true, Ordering::AcqRel) {
            return -1;
        }
        let cfg: TunConfigJson = match serde_json::from_str(cfg_json) {
            Ok(c) => c,
            Err(_) => {
                self.tun.probe_running.store(false, Ordering::Release);
                return -3;
            }
        };
        // 参数预检（同步可判的硬失败）：空 token 是唯一能在这关同步判死的配置错
        if cfg.token.is_empty() {
            self.tun.probe_running.store(false, Ordering::Release);
            return -3;
        }
        // 日志重定向面（-2）：真壳接文件；本面以标志位承载（打开失败 = -2）
        if !log_path_ok {
            self.tun.probe_running.store(false, Ordering::Release);
            return -2;
        }
        let gen = self.tun.gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.tun.stage.begin_generation(gen);
        // 新世代不带上一世代的分类残留（先清——顺序即注释）
        self.tun.orphan_alive.store(false, Ordering::Release);
        self.tun.stage.set_if_current(gen, TunStage::Preparing, "", "", false);
        let exec = self.executor();
        let stage = &self.tun.stage;
        let result = exec.warmup(&cfg, stage, gen);
        if let Err(e) = result {
            // 同步硬失败：世代收尾（放锁 + failed 终态）
            self.tun.stage.set_if_current(gen, TunStage::Failed, "core", &e, false);
            self.tun.probe_running.store(false, Ordering::Release);
            return 0; // 受理 0；failed 原因经 tun_status 读
        }
        0
    }

    // ---- ③ ClientCoreTunAttach ----

    /// 两阶段启动第二阶段：把 TUN fd 交给已就绪的世代。
    /// fd≤0 直接拒（fd=0 是合法的 stdin——会把核读扩展进程标准输入，极难查）。
    pub fn tun_attach(&self, fd: i32, mtu: u32) -> i32 {
        if fd <= 0 {
            self.tun.stage.set(TunStage::Failed, "attach", "attach 收到非法 fd", false);
            return -3;
        }
        let gen = self.tun.gen.load(Ordering::Acquire);
        let snap = self.tun.stage.snapshot();
        if !self.tun.probe_running.load(Ordering::Acquire) || snap.stage != TunStage::Ready {
            return -1;
        }
        // attach 截止检查（无人推进的世代已收工——tun_status 可读 attach-timeout）
        if let Some(dl) = *self.tun.attach_deadline.lock().expect("attach 截止锁中毒") {
            if Instant::now() > dl {
                self.tun.stage.set_if_current(gen, TunStage::Idle, "attach-timeout", "等待接入超时，世代已收工", false);
                self.tun.probe_running.store(false, Ordering::Release);
                return -1;
            }
        }
        match self.executor().attach(fd, mtu) {
            Ok(()) => {
                self.tun
                    .stage
                    .set_if_current(gen, TunStage::Attached, "", "", true);
                *self.tun.attach_deadline.lock().expect("attach 截止锁中毒") = None;
                0
            }
            Err(e) => {
                // 接管失败：世代整体收工（状态可查原因）
                self.tun.stage.set_if_current(gen, TunStage::Failed, "attach", &e, false);
                self.tun.probe_running.store(false, Ordering::Release);
                -4
            }
        }
    }

    // ---- ④ ClientCoreTunStatus ----

    /// 状态查询（tunStatusJSON 完整键面——tun_status::tun_status_json）。
    pub fn tun_status(&self) -> String {
        let exec = self.executor();
        let snap = self.tun.stage.snapshot();
        let running = self.tun_running_inner(&exec);
        // unhealthyReason 只看分类是否非空（Go `if why != ""`——与 running 位独立）
        let why = exec.unhealthy_why();
        let input = TunStatusInput {
            stage: snap,
            running,
            demand: self.demand.last(),
            demand_fg: self.demand.fg(),
            unhealthy_reason: (!why.is_empty()).then_some(why),
            runner: exec.runner(),
            transport: exec.transport(),
        };
        tun_status::tun_status_json(&input)
    }

    fn tun_running_inner(&self, exec: &Arc<dyn TunExecutor>) -> bool {
        // 只有接上数据面且健康才算 1（单飞锁 + 健康位 + attached 的合成——三个信号
        // 各自原子、合起来不是一致快照，转换瞬间可能瞬时 0/1；调用方已用去抖）。
        self.tun.probe_running.load(Ordering::Acquire)
            && exec.healthy()
            && self.tun.stage.snapshot().stage == TunStage::Attached
    }

    // ---- ⑤ ClientCoreTunStop ----

    /// 停止并等待收尾：0 已停（或本就没跑）/ -1 等待超时（世代仍在，锁未放）/
    /// -2 等超时后强制放锁（收工超时且期间没有新世代启动）。
    pub fn tun_stop(&self) -> i32 {
        if !self.tun.probe_running.load(Ordering::Acquire) {
            return 0; // 本就没在跑
        }
        let gen = self.tun.gen.load(Ordering::Acquire);
        self.executor().request_stop();
        if self.executor().wait_stopped(STOP_WAIT) {
            self.tun.stage.set_if_current(gen, TunStage::Idle, "stopped", "", false);
            self.tun.probe_running.store(false, Ordering::Release);
            return 0;
        }
        // 超时：若期间没有新世代启动（gen 未变且锁未放出）⇒ 强制放锁
        if self.tun.gen.load(Ordering::Acquire) == gen && self.tun.probe_running.load(Ordering::Acquire) {
            self.tun.orphan_alive.store(true, Ordering::Release);
            self.tun.probe_running.store(false, Ordering::Release);
            -2
        } else {
            -1
        }
    }

    // ---- ⑥ ClientCoreTunRecover ----

    /// 恢复阶梯下推入口（from 钳位 R1..=R3；rc 契约见 facade 头注释）。
    pub fn tun_recover(&self, from: i64) -> i32 {
        self.executor().recover(from, &format!("扩展下推({from})"))
    }

    // ---- ⑦ ClientCoreTunRunning ----

    pub fn tun_running(&self) -> i32 {
        i32::from(self.tun_running_inner(&self.executor()))
    }

    // ---- ⑧ ClientCoreTunSetForeground ----

    /// 下发「App 是否前台」；返回上一状态。false→true 的转变踢一次立即探测
    /// （巡检节拍固定 60s 不变；只影响探测时机）。
    pub fn tun_set_foreground(&self, fg: bool) -> i32 {
        let prev = self.tun.foreground.swap(fg, Ordering::AcqRel);
        if !prev && fg {
            if let Some(kick) = self.tun.foreground_kick.lock().expect("kick 锁中毒").as_ref() {
                kick();
            }
        }
        i32::from(prev)
    }

    /// 注册「回到前台」补探钩子。
    pub fn set_foreground_kick(&self, f: Option<Box<dyn Fn() + Send>>) {
        *self.tun.foreground_kick.lock().expect("kick 锁中毒") = f;
    }

    // ---- ⑨ ClientCoreTunSetActivity ----

    /// 需求信号下发（每拍必发；唤醒拍扩展会立即补发一次）。
    pub fn tun_set_activity(&self, fg: bool, screen: bool) {
        self.demand.set_activity(fg, screen);
    }

    // ---- ⑩ ClientCoreTunSetPortForwards ----

    /// 运行中整表热替换（不重连隧道）：0 已应用 / -1 无已接管世代（改动随下次连接
    /// 的 tunConfig 自然生效）/ -2 JSON 非法或校验不过。
    pub fn tun_set_port_forwards(&self, cfg: &str) -> i32 {
        let rules = match portfwd::parse_rules_json(cfg) {
            Ok(r) => r,
            Err(_) => return -2,
        };
        if portfwd::validate_table(&rules).is_err() {
            return -2;
        }
        // 判据 = 已 attached 且世代活着（probeRunning==0 / stage != attached / 收工中
        // 都 -1——热更新不得装进已跑过收工的死世代）。
        if !self.tun.probe_running.load(Ordering::Acquire) {
            return -1;
        }
        if self.tun.stage.snapshot().stage != TunStage::Attached {
            return -1;
        }
        self.executor().request_port_forwards(rules)
    }

    // ---- ⑪⑫ ClientCoreProbeAddr / ProbeReach ----

    pub fn probe_addr(&self, token: &str) -> String {
        probe_json::probe_addr_json(token)
    }

    pub fn probe_reach(&self, report: &probe_json::ReachReport) -> String {
        probe_json::probe_reach_json(report)
    }

    // ---- ⑬ ClientCoreFilesCall ----

    pub fn files_call(&self, op_json: &str) -> String {
        self.files.files_call(op_json)
    }

    // ---- ⑭ ClientCoreTermCall ----

    pub fn term_call(&self, op_json: &str) -> String {
        term_op::term_call(op_json)
    }

    // ---- ⑮⑯⑰ ClientCoreSpeedTest* ----

    pub fn speedtest_start(&self, params_json: &str, run: impl FnOnce(speedtest_op::EngineParams) -> speedtest_op::SpeedOutcome) -> String {
        speedtest_op::speed_start(params_json, run)
    }

    pub fn speedtest_status(&self, s: &speedtest_op::SpeedSnapshotIn) -> String {
        speedtest_op::speed_status_json(s)
    }

    pub fn speedtest_cancel(&self) -> String {
        speedtest_op::speed_cancel_json()
    }

    // ---- ⑱⑲⑳ ClientCoreService* ----

    pub fn service_start(&self, cfg_json: &str) -> i32 {
        self.service.start(cfg_json)
    }

    pub fn service_stop(&self) -> i32 {
        self.service.stop()
    }

    pub fn service_status(&self) -> String {
        self.service.status_json()
    }
}

/// rustc 版本（编译期常量；等价 Go runtime.Version() 的信息位）。
fn rustc_semver() -> &'static str {
    // 编译期取（无运行时 API）：经 env 不可得时回落 stable 形态
    option_env!("HOMEWAY_RUSTC_VERSION").unwrap_or("stable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 受控执行体：warmup 置 ready/failed 由测试定；attach 成率可定。
    struct FakeExec {
        warmup_ok: bool,
        attach_ok: bool,
        healthy: AtomicBool,
        stop_tx: mpsc::Sender<()>,
        stopped: Arc<AtomicBool>,
    }

    impl TunExecutor for FakeExec {
        fn warmup(&self, _cfg: &TunConfigJson, stage: &StageMachine, _gen: u64) -> Result<(), String> {
            if self.warmup_ok {
                stage.set(TunStage::Ready, "", "", true);
                stage.set_ready_by("wg");
                Ok(())
            } else {
                Err("新栈启动失败：token 解析失败".into())
            }
        }
        fn attach(&self, _fd: i32, _mtu: u32) -> Result<(), String> {
            if self.attach_ok {
                Ok(())
            } else {
                Err("接管失败".into())
            }
        }
        fn request_stop(&self) {
            let _ = self.stop_tx.send(());
        }
        fn wait_stopped(&self, _budget: Duration) -> bool {
            self.stopped.load(Ordering::Relaxed)
        }
        fn recover(&self, _from: i64, _cause: &str) -> i32 {
            0
        }
        fn healthy(&self) -> bool {
            self.healthy.load(Ordering::Relaxed)
        }
        fn unhealthy_why(&self) -> String {
            String::new()
        }
        fn runner(&self) -> Option<RunnerIn> {
            None
        }
        fn transport(&self) -> Option<TransportIn> {
            None
        }
    }

    fn core_with(warmup_ok: bool, attach_ok: bool) -> (ClientCore, mpsc::Receiver<()>, Arc<AtomicBool>) {
        let (tx, rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let exec = Arc::new(FakeExec {
            warmup_ok,
            attach_ok,
            healthy: AtomicBool::new(true),
            stop_tx: tx,
            stopped: Arc::clone(&stopped),
        });
        (ClientCore::new(exec), rx, stopped)
    }

    /// 两阶段全流程：prepare（受理 → ready）→ attach（0）→ running=1 → stop（0）。
    #[test]
    fn two_phase_lifecycle() {
        let (core, _rx, stopped) = core_with(true, true);
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
        assert!(core.tun_status().contains("\"state\":\"ready\""));
        assert!(core.tun_status().contains("\"readyBy\":\"wg\""));
        assert_eq!(core.tun_attach(90, 1280), 0);
        assert!(core.tun_status().contains("\"state\":\"attached\""));
        assert_eq!(core.tun_running(), 1);
        assert!(core.tun_status().contains("\"running\":1"));
        stopped.store(true, Ordering::Relaxed);
        assert_eq!(core.tun_stop(), 0);
        assert_eq!(core.tun_running(), 0);
    }

    /// rc 契约：-1 忙 / -3 参数（坏 JSON、空 token）/ -2 日志。
    #[test]
    fn prepare_rc_contract() {
        let (core, _rx, _s) = core_with(true, true);
        assert_eq!(core.tun_prepare("{oops", true), -3);
        assert_eq!(core.tun_prepare(r#"{"mtu":1280}"#, true), -3);
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, false), -2);
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
        // 在世世代：-1 忙（服务会话与隧道会话不得并发的前提在 App 侧；核内只有单飞）
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), -1);
    }

    /// attach 契约：fd≤0 → -3 + failed；未 ready → -1；失败 → -4 + failed。
    #[test]
    fn attach_rc_contract() {
        let (core, _rx, _s) = core_with(true, false);
        assert_eq!(core.tun_attach(0, 1280), -3);
        assert!(core.tun_status().contains("\"code\":\"attach\""));
        // 未 prepare：-1
        let (core2, _rx2, _s2) = core_with(true, true);
        assert_eq!(core2.tun_attach(90, 1280), -1);
        // prepare 后 attach 失败：-4 + failed（世代收工放锁——可再次 prepare）
        assert_eq!(core2.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
        core2.set_executor({
            let (tx, _rx) = mpsc::channel();
            Arc::new(FakeExec {
                warmup_ok: true,
                attach_ok: false,
                healthy: AtomicBool::new(true),
                stop_tx: tx,
                stopped: Arc::new(AtomicBool::new(true)),
            })
        });
        assert_eq!(core2.tun_attach(90, 1280), -4);
        assert!(core2.tun_status().contains("\"state\":\"failed\""));
    }

    /// stop 契约：本就没跑 0；收工完成 0；超时且无新世代 ⇒ -2 强制放锁（此后可重启）。
    #[test]
    fn stop_rc_contract() {
        let (core, _rx, stopped) = core_with(true, true);
        assert_eq!(core.tun_stop(), 0); // 本就没跑
        core.tun_prepare(r#"{"token":"hmw1-x"}"#, true);
        // 未收工（stopped=false）⇒ 超时后 -2 放锁
        stopped.store(false, Ordering::Relaxed);
        assert_eq!(core.tun_stop(), -2);
        // 放锁后可重启（无 -1 忙）
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
    }

    /// SetForeground 返回上一状态；false→true 踢补探钩。
    #[test]
    fn foreground_prev_and_kick() {
        let (core, _rx, _s) = core_with(true, true);
        let kicked = Arc::new(AtomicBool::new(false));
        let k2 = Arc::clone(&kicked);
        core.set_foreground_kick(Some(Box::new(move || k2.store(true, Ordering::Relaxed))));
        assert_eq!(core.tun_set_foreground(true), 0); // 初始 false
        assert!(kicked.load(Ordering::Relaxed), "false→true 应踢补探");
        kicked.store(false, Ordering::Relaxed);
        assert_eq!(core.tun_set_foreground(true), 1); // 上一状态 true
        assert!(!kicked.load(Ordering::Relaxed), "true→true 不踢");
        assert_eq!(core.tun_set_foreground(false), 1);
        assert_eq!(core.tun_set_foreground(true), 0); // 再度转变踢
    }

    /// portfwd 热替换门：attached 才收（Noop 执行体恒 -1）；校验失败 -2。
    #[test]
    fn port_forwards_gate() {
        let (core, _rx, stopped) = core_with(true, true);
        // JSON 非法 → -2
        assert_eq!(core.tun_set_port_forwards("{oops"), -2);
        // 校验不过（listen 0）→ -2
        assert_eq!(
            core.tun_set_port_forwards(r#"{"portForwards":[{"listen":0,"targetIp":"","targetPort":80}]}"#),
            -2
        );
        // 未 attached → -1（Noop 执行体恒 -1；门先红在 stage）
        assert_eq!(
            core.tun_set_port_forwards(r#"{"portForwards":[{"listen":18080,"targetIp":"","targetPort":80}]}"#),
            -1
        );
        core.tun_prepare(r#"{"token":"hmw1-x"}"#, true);
        core.tun_attach(90, 1280);
        stopped.store(true, Ordering::Relaxed);
        // attached 后过了门，执行体无承载 ⇒ 仍 -1（改动随下次连接生效——语义正确）
        assert_eq!(
            core.tun_set_port_forwards(r#"{"portForwards":[{"listen":18080,"targetIp":"","targetPort":80}]}"#),
            -1
        );
    }

    /// 版本串形状（tier core 前缀 + c-shared 尾）。
    #[test]
    fn version_shape() {
        let v = ClientCore::version();
        assert!(v.starts_with("tier core "), "{v}");
        assert!(v.ends_with(", c-shared)"), "{v}");
    }
}
