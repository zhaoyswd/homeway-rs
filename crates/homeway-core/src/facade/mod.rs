//! homeway-core facade：App 核门面（20 个 NAPI 导出面的 Rust API——R7-7c/7g）。
//!
//! 语义真源 = `baseline:clientcore/cmd/clientcore/` 的 `//export` 族（tier:AGENTS.md
//! 原生契约四处同步清单；7b 拍板 = C-ABI 复刻同名符号，本模块即符号面之下的纯 Rust
//! API——extern "C" 壳在 `homeway-capi` crate）。
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
//! - `tun_attach`：0 接管 / -1 无 ready 世代（或 fd 投不进） / -3 fd≤0 / -4 接管失败
//!   （世代已整体收工，原因见 tun_status）/ -5 轮询超时（5s 内没等到 attached——
//!   只是慢，不是失败）；
//! - `tun_stop`：0 已停 / -1 等超时 / -2 超时后强制放锁；
//! - `tun_recover`：0 某档通过 / -1 走完未恢复 / -2 无 attached 隧道 / -3·-4 本地动作 /
//!   -9 异步壳异常（壳面产生，本面不产）。
//!
//! 世代生命周期（7g 工单①）：`warmup` **启动即返**（tier 壳在 JS 线程同步直调
//! prepare——暖机 20s 窗在线程里跑会冻结事件泵）；世代线程持有 [`TunShared`] 的
//! 收尾义务（`finish_generation`：非 failed 回 idle、放单飞锁、健康位/分类、done 信号）；
//! attach 60s 死线由世代线程收割（`idle/"attach-timeout"/"就绪后无人 attach，已自行
//! 收工放锁"`——Go 生产路径同串，C-5 向量重产对齐）。
//!
//! tun 域的数据面执行体经 [`TunExecutor`] 注入；本仓实现 = `tun_exec::TunnelExec`
//! （真 wgcore hub：TUN fd → L3 直通）。

pub mod bridge_host;
pub mod demand;
pub mod events;
pub mod files_op;
pub mod portfwd;
pub mod probe_json;
pub mod service_exec;
pub mod service_op;
pub mod speedtest_op;
pub mod stage;
pub mod term_op;
pub mod tun_exec;
pub mod tun_shared;
pub mod tun_status;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use demand::DemandSignals;
use stage::TunStage;
use tun_shared::{lock_unpoison, TunShared};
use tun_status::{RunnerIn, TransportIn, TunStatusInput};

/// 暖机窗口：等注册确认为止（软失败——超时仍 ready、可 attach 自愈）。
pub const WARM_TIMEOUT: Duration = Duration::from_secs(20);
/// prepare 成功后等 attach 的上限（唯一合法长间隔 = 系统侧建接口/授权；到点世代
/// 自行收工放锁，防无人推进的世代占锁）。
pub const ATTACH_DEADLINE: Duration = Duration::from_secs(60);
/// attach 投递后等世代进入 attached 的轮询上限（Go attachTun 的 5s——到点 -5「慢」
/// 与 -4「失败」区分）。
pub const ATTACH_POLL: Duration = Duration::from_secs(5);
/// stop 收工等待预算（tunStopWait 的 3s；超时 -1，且期间无新世代 ⇒ 强制放锁 -2）。
pub const STOP_WAIT: Duration = Duration::from_secs(3);
/// attach 死线终态的判据串（Go tunmode.go:802 生产路径逐字；C-5 向量源）。
pub const ATTACH_TIMEOUT_REASON: &str = "就绪后无人 attach，已自行收工放锁";

/// tunConfig（hostsession.Config 同形；JSON 键 = Go json tag 的 camelCase——
/// 评审 r1-F05 整改：App 实发 camelCase，无 rename 会静默丢 7 字段）。
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// warmup 的类型化错误（工单⑤ r1-F27：字符串错误改枚举；code 即 tun_status 的
/// 机器可读原因码）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TunError {
    /// 会话/核类（token 解析失败、数据面装配失败等）。
    #[error("{0}")]
    Core(String),
    /// 日志面打不开（tun_prepare 的 -2 同源）。
    #[error("日志文件打不开：{0}")]
    LogOpen(String),
    /// 参数类（tun_prepare 的 -3 同源）。
    #[error("参数错误：{0}")]
    InvalidConfig(String),
}

impl TunError {
    /// tun_status 的 code 词面（stage 机的原因码族：core/config/derp/attach/…）。
    pub fn code(&self) -> &'static str {
        match self {
            TunError::Core(_) => "core",
            TunError::LogOpen(_) => "core",
            TunError::InvalidConfig(_) => "config",
        }
    }
}

/// tun 域执行体（世代生命周期里需要外部世界的动作）。本仓实现 = `tun_exec::TunnelExec`
/// （真 hub）；测试用受控实现。
///
/// 生命周期契约（工单①）：
/// - `warmup` **启动即返**：受理后 spawn 世代线程即回 `Ok(())`；同步硬失败回 `Err`
///   （facade 写 failed 终态 + 放锁后仍返回受理 0——失败原因经 `tun_status` 读，
///   Go tunBegin 的 goroutine 内失败同语义）；
/// - 世代线程**必须**在一切退出路径调 `shared.finish_generation(gen)`（终态/放锁/
///   done 信号——漏调 = 单飞锁永不释放）；
/// - 世代线程从 `shared.attach_receiver()` 拿 fd 接收端：暖机完成（stage=ready）后
///   在 `ATTACH_DEADLINE` 内等 fd，到点自行收工（`idle/"attach-timeout"` 终态）；
/// - `request_stop` 要能打断暖机（Go：关客户端是第二条打断路径）。
pub trait TunExecutor: Send + Sync {
    /// 暖机一个世代（启动即返；见模块头生命周期契约）。
    fn warmup(&self, cfg: &TunConfigJson, shared: Arc<TunShared>, gen: u64)
        -> Result<(), TunError>;
    /// 请求世代收工（幂等信号；要能打断暖机中的阻塞调用）。
    fn request_stop(&self);
    /// 恢复阶梯入口（from 起跑档位；rc 契约见 session::LadderRc::as_rc）。
    fn recover(&self, from: i64, cause: &str) -> i32;
    /// runner/transport 状态块（tunStatusJSON 的条件键源；None = 无 runner 期）。
    /// 健康位/分类在 `TunShared`（不走本 trait——世代共享面）。
    fn runner(&self) -> Option<RunnerIn>;
    fn transport(&self) -> Option<TransportIn>;
    /// portfwd 整表热替换承载（默认 -1：无承载 = 改动随下次连接的 tunConfig 生效）。
    fn request_port_forwards(&self, _rules: Vec<portfwd::PortForwardRule>) -> i32 {
        -1
    }
}

/// 无操作执行体（默认：一切本地动作失败——prepare 只到参数校验、暖机即失败）。
/// `TunnelExec`（真 hub）在 tun_exec；这保证 facade 独立可测。
#[derive(Debug, Default)]
pub struct NoopTunExecutor;

impl TunExecutor for NoopTunExecutor {
    fn warmup(
        &self,
        _cfg: &TunConfigJson,
        _shared: Arc<TunShared>,
        _gen: u64,
    ) -> Result<(), TunError> {
        // 同步硬失败（无执行体）：facade 会写 failed 终态 + 放锁
        Err(TunError::Core("无执行体（NoopTunExecutor）".into()))
    }
    fn request_stop(&self) {}
    fn recover(&self, _from: i64, _cause: &str) -> i32 {
        -2
    }
    fn runner(&self) -> Option<RunnerIn> {
        None
    }
    fn transport(&self) -> Option<TransportIn> {
        None
    }
}

/// App 核门面（20 导出面的 Rust API；线程安全）。
pub struct ClientCore {
    /// tun 域世代共享面（单飞锁/世代/阶段机/健康位/attach 通道/done）。
    tun: Arc<TunShared>,
    /// 前台位（SetForeground 的返回值语义 = 上一状态）。
    foreground: AtomicBool,
    /// 「回到前台」转变时的补探钩子（false→true 踢一次立即探测；只影响时机）。
    foreground_kick: Mutex<Option<Box<dyn Fn() + Send>>>,
    pub demand: Arc<DemandSignals>,
    pub files: files_op::FilesOps,
    executor: Mutex<Arc<dyn TunExecutor>>,
    /// 服务会话域（rc 门与状态机在 service_op；Arc = ServiceExec 的共享事实源）。
    pub service: Arc<service_op::ServiceDomain>,
    /// 服务会话真装配（Session + 桥；三导出的执行面——rc 门先行、本模块接线）。
    pub service_exec: service_exec::ServiceExec,
    /// 状态推送的最小等价面（可轮询事件队列 + 冷启动快照；真 IPC 推送在 ArkTS 侧）。
    /// Rust 独有内部面——不进 20 导出面、不参与词表对账（7g 争议②拍板：文档声明）。
    pub events: events::EventHub,
    /// 服务桥宿主（`<filesDir>/bridge/*.sock` 三座；None = 未装配——随真 Session 注入）。
    pub service_bridge: Mutex<Option<Arc<bridge_host::BridgeHost>>>,
}

impl Default for ClientCore {
    fn default() -> Self {
        Self::new(Arc::new(NoopTunExecutor))
    }
}

impl ClientCore {
    pub fn new(executor: Arc<dyn TunExecutor>) -> Self {
        Self::with_shared(executor, Arc::new(DemandSignals::new()))
    }

    /// 共享 demand 构造（capi 装配形态：TunnelExec 的巡检/pusher 与 ClientCore 的
    /// SetActivity 面必须消费**同一个** DemandSignals——共享只能经 Arc：内部全 Mutex，
    /// 值克隆会分裂成两份状态）。
    pub fn with_shared(executor: Arc<dyn TunExecutor>, demand: Arc<DemandSignals>) -> Self {
        ClientCore {
            tun: Arc::new(TunShared::new()),
            foreground: AtomicBool::new(false),
            foreground_kick: Mutex::new(None),
            demand,
            files: files_op::FilesOps::new(),
            executor: Mutex::new(executor),
            service: Arc::new(service_op::ServiceDomain::new()),
            service_exec: service_exec::ServiceExec::new(),
            events: events::EventHub::new(),
            service_bridge: Mutex::new(None),
        }
    }

    /// 装配服务桥（服务会话宿主形态——`service_start` 受理后由装配方注入；
    /// 桥状态经 `bridge_status` 并入 serviceStatusJSON 的 bridge 四键）。
    pub fn attach_service_bridge(&self, host: Option<Arc<bridge_host::BridgeHost>>) {
        *lock_unpoison(&self.service_bridge) = host;
    }

    /// 桥状态快照（bridge 四键源；未装配 = 全空——serviceStatusJSON 的缺省形态）。
    pub fn bridge_status(&self) -> bridge_host::BridgeStatus {
        lock_unpoison(&self.service_bridge)
            .as_ref()
            .map(|h| h.status())
            .unwrap_or_default()
    }

    /// 替换执行体（装配真 hub 用；测试注入受控实现用）。
    pub fn set_executor(&self, e: Arc<dyn TunExecutor>) {
        *lock_unpoison(&self.executor) = e;
    }

    fn executor(&self) -> Arc<dyn TunExecutor> {
        Arc::clone(&lock_unpoison(&self.executor))
    }

    /// tun 共享面（执行体装配/测试用）。
    pub fn tun_shared(&self) -> Arc<TunShared> {
        Arc::clone(&self.tun)
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

    /// 两阶段启动第一阶段：暖机（不碰 TUN fd）。**启动即返**（世代线程在执行体里）；
    /// 调用后轮询 `tun_status` 直到 state=ready（或 failed）；就绪后必须在
    /// `ATTACH_DEADLINE` 内 attach（到点世代自行收工）。
    pub fn tun_prepare(&self, cfg_json: &str, log_path_ok: bool) -> i32 {
        // 单飞：在世世代期间拒绝（-1 忙）
        if self.tun.probe_running.swap(true, Ordering::AcqRel) {
            return -1;
        }
        let release = |tun: &TunShared| tun.probe_running.store(false, Ordering::Release);
        let cfg: TunConfigJson = match serde_json::from_str(cfg_json) {
            Ok(c) => c,
            Err(_) => {
                release(&self.tun);
                return -3;
            }
        };
        // 参数预检（同步可判的硬失败）：空 token 是唯一能在这关同步判死的配置错
        if cfg.token.is_empty() {
            release(&self.tun);
            return -3;
        }
        // 日志重定向面（-2）：真壳接文件；本面以标志位承载（打开失败 = -2）
        if !log_path_ok {
            release(&self.tun);
            return -2;
        }
        let gen = self.tun.gen.fetch_add(1, Ordering::AcqRel) + 1;
        self.tun.begin_generation(gen); // 写入权交接 + done/attach 通道/停止位槽复位
        self.tun.begin_healthy(); // 新世代不带上一世代的分类残留（先清——顺序即注释）
        self.tun
            .stage
            .set_if_current(gen, TunStage::Preparing, "", "", false);
        match self.executor().warmup(&cfg, Arc::clone(&self.tun), gen) {
            Ok(()) => 0,
            Err(e) => {
                if matches!(e, TunError::LogOpen(_)) {
                    // 日志面打不开 = 同步 -2（Go tunStdioBegin 失败的 rc 契约；
                    // 世代未起——只放锁不写终态；此前 Preparing 已写 ⇒ 回 Idle 防
                    // App 轮询面停在 preparing 永远等不到了——评审 r2-L2）
                    self.tun
                        .stage
                        .set_if_current(gen, TunStage::Idle, "", "", false);
                    self.tun.probe_running.store(false, Ordering::Release);
                    return -2;
                }
                // 同步硬失败（如线程 spawn 失败）：世代就地收尾（failed 终态 + 放锁）。
                // 仍返回受理 0——失败原因经 tun_status 读（Go goroutine 内失败同语义）。
                self.tun.stage.set_if_current(
                    gen,
                    TunStage::Failed,
                    e.code(),
                    &e.to_string(),
                    false,
                );
                self.tun.finish_generation(gen);
                0
            }
        }
    }

    // ---- ③ ClientCoreTunAttach（attachTun）----

    /// 两阶段启动第二阶段：把 TUN fd 交给已就绪的世代。
    /// fd≤0 直接拒（fd=0 是合法的 stdin——会把核读扩展进程标准输入，极难查）。
    pub fn tun_attach(&self, fd: i32, mtu: u32) -> i32 {
        let _ = mtu; // mtu 由世代线程从 cfg 读（fd 通道只传 fd——Go attachCh 同形）
        if fd <= 0 {
            self.tun
                .stage
                .set(TunStage::Failed, "attach", "attach 收到非法 fd", false);
            return -3;
        }
        let snap = self.tun.stage.snapshot();
        if !self.tun.probe_running.load(Ordering::Acquire) || snap.stage != TunStage::Ready {
            return -1;
        }
        // 投 fd（通道缓冲 1 非阻塞；世代线程未注册接收端〔暖机未完成〕= -1——
        // 调用方稍后重试，与 Go attachCh 2s 投递窗的 -1 同归因）
        if !self.tun.deliver_fd(fd) {
            return -1;
        }
        // 轮询 stage：attached → 0；failed/idle → -4；5s → -5（慢，不是失败）
        let deadline = std::time::Instant::now() + ATTACH_POLL;
        loop {
            let st = self.tun.stage.snapshot().stage;
            match st {
                TunStage::Attached => return 0,
                TunStage::Failed | TunStage::Idle => return -4,
                _ => {}
            }
            if std::time::Instant::now() >= deadline {
                return -5;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    // ---- ④ ClientCoreTunStatus ----

    /// 状态查询（tunStatusJSON 完整键面——tun_status::tun_status_json）。
    pub fn tun_status(&self) -> String {
        let exec = self.executor();
        let snap = self.tun.stage.snapshot();
        let running = self.tun_running_inner(&exec);
        // unhealthyReason 只看分类是否非空（Go `if why != ""`——与 running 位独立）
        let why = lock_unpoison(&self.tun.unhealthy_why).clone();
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

    fn tun_running_inner(&self, _exec: &Arc<dyn TunExecutor>) -> bool {
        // 只有接上数据面且健康才算 1（单飞锁 + 健康位 + attached 的合成——三个信号
        // 各自原子、合起来不是一致快照，转换瞬间可能瞬时 0/1；调用方已用去抖）。
        self.tun.probe_running.load(Ordering::Acquire)
            && self.tun.healthy.load(Ordering::Acquire)
            && self.tun.stage.snapshot().stage == TunStage::Attached
    }

    // ---- ⑤ ClientCoreTunStop（tunStopWait）----

    /// 停止并等待收尾：0 已停（或本就没跑）/ -1 等待超时（世代仍在，锁未放）/
    /// -2 等超时后强制放锁（收工超时且期间没有新世代启动）。
    ///
    /// 终态语义（工单③）：**只放锁不写阶段**——终态由世代线程的 finish_generation
    /// 写（非 failed 回 idle；failed 保留）；tun_stop 不抢写。
    pub fn tun_stop(&self) -> i32 {
        if !self.tun.probe_running.load(Ordering::Acquire) {
            return 0; // 本就没在跑
        }
        if self.tun.is_done() {
            return 0; // 本世代早已退出（锁可能尚未被观测到放下——finish 已放）
        }
        let gen = self.tun.gen.load(Ordering::Acquire);
        self.executor().request_stop();
        if self.tun.wait_done(STOP_WAIT) {
            return 0;
        }
        // 超时：若期间没有新世代启动（gen 未变且锁未放出）⇒ 强制放锁（不写阶段——
        // 工单③：终态由世代写；旧世代迟到收尾过世代守卫无害）。健康位照 Go 同分支
        // 补写 why="stop"/healthy=false（评审 r2-L1）。
        if self.tun.gen.load(Ordering::Acquire) == gen
            && self.tun.probe_running.load(Ordering::Acquire)
        {
            self.tun.mark_unhealthy("stop");
            self.tun.probe_running.store(false, Ordering::Release);
            -2
        } else {
            -1
        }
    }

    // ---- ⑥ ClientCoreTunRecover ----

    /// 恢复阶梯下推入口（from 钳位 R1..=R3；rc 契约见 facade 头注释）。cause 文案
    /// 用**档位名**（评审 r2-L6：`扩展下推(R1 重握手)`——会进 RECOVER 判据行，
    /// Go 同串）。
    pub fn tun_recover(&self, from: i64) -> i32 {
        let lvl = crate::session::recover::Level::clamp(from);
        self.executor()
            .recover(from, &format!("扩展下推({})", lvl.name()))
    }

    // ---- ⑦ ClientCoreTunRunning ----

    pub fn tun_running(&self) -> i32 {
        i32::from(self.tun_running_inner(&self.executor()))
    }

    // ---- ⑧ ClientCoreTunSetForeground ----

    /// 下发「App 是否前台」；返回上一状态。false→true 的转变踢一次立即探测
    /// （巡检节拍固定 60s 不变；只影响探测时机）。
    pub fn tun_set_foreground(&self, fg: bool) -> i32 {
        let prev = self.foreground.swap(fg, Ordering::AcqRel);
        if !prev && fg {
            if let Some(kick) = lock_unpoison(&self.foreground_kick).as_ref() {
                kick();
            }
        }
        i32::from(prev)
    }

    /// 注册「回到前台」补探钩子。
    pub fn set_foreground_kick(&self, f: Option<Box<dyn Fn() + Send>>) {
        *lock_unpoison(&self.foreground_kick) = f;
    }

    // ---- ⑨ ClientCoreTunSetActivity ----

    /// 需求信号下发（每拍必发；唤醒拍扩展会立即补发一次）。
    pub fn tun_set_activity(&self, fg: bool, screen: bool) {
        self.demand.set_activity(fg, screen);
    }

    // ---- ⑩ ClientCoreTunSetPortForwards ----

    /// 运行中整表热替换（不重连隧道）：0 已应用（**真装表**）/ -1 无已接管世代
    /// （改动随下次连接的 tunConfig 自然生效）/ -2 JSON 非法或校验不过
    /// （Q-F-B 起含条数上限 8）。
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

    pub fn speedtest_start(
        &self,
        params_json: &str,
        run: impl FnOnce(speedtest_op::EngineParams) -> speedtest_op::SpeedOutcome,
    ) -> String {
        speedtest_op::speed_start(params_json, run)
    }

    pub fn speedtest_status(&self, s: &speedtest_op::SpeedSnapshotIn) -> String {
        speedtest_op::speed_status_json(s)
    }

    pub fn speedtest_cancel(&self) -> String {
        speedtest_op::speed_cancel_json()
    }

    // ---- ⑱⑲⑳ ClientCoreService* ----

    /// 真装配形态（工单⑤ service 三面接真 Session）：rc 门 + Session/桥接线都在
    /// ServiceExec；纯门形态（`self.service.start/stop/status_json`）保留给测试。
    pub fn service_start(&self, cfg_json: &str) -> i32 {
        self.service_exec.start(cfg_json, &self.service)
    }

    pub fn service_stop(&self) -> i32 {
        self.service_exec.stop(&self.service)
    }

    pub fn service_status(&self) -> String {
        self.service_exec.status(&self.service)
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

    /// 受控执行体：暖机在线程里推进（置 ready/failed 由测试定）；attach 成率可定。
    /// 走世代线程形态（对齐工单①语义：warmup 启动即返）。
    struct FakeExec {
        warmup_ok: bool,
        attach_ok: bool,
        stop_flag: Arc<AtomicBool>,
    }

    impl TunExecutor for FakeExec {
        fn warmup(
            &self,
            _cfg: &TunConfigJson,
            shared: Arc<TunShared>,
            gen: u64,
        ) -> Result<(), TunError> {
            let warmup_ok = self.warmup_ok;
            let attach_ok = self.attach_ok;
            let stop = Arc::clone(&self.stop_flag);
            stop.store(false, Ordering::Release);
            std::thread::spawn(move || {
                // attach 接收端**先于 Ready 发布注册**（评审 r2-M3 的生产时序契约——
                // 窗口内投 fd 必须可达；假件此前反序，µs 级竞态偶发 -1〔终轮 ci 实抓〕）
                let fd_rx = shared.attach_receiver();
                if warmup_ok {
                    shared
                        .stage
                        .set_if_current(gen, TunStage::Ready, "", "", true);
                    shared.stage.set_ready_by("wg");
                } else {
                    shared.stage.set_if_current(
                        gen,
                        TunStage::Failed,
                        "core",
                        "新栈启动失败：token 解析失败",
                        false,
                    );
                    shared.finish_generation(gen);
                    return;
                }
                // 等 fd（TunShared 的 attach 通道——facade::tun_attach 投递）/ stop
                loop {
                    match fd_rx.recv_timeout(Duration::from_millis(200)) {
                        Ok(_fd) => {
                            if attach_ok {
                                shared
                                    .stage
                                    .set_if_current(gen, TunStage::Attached, "", "", true);
                                // 挂到 stop 才收尾
                                while !stop.load(Ordering::Acquire) {
                                    std::thread::sleep(Duration::from_millis(20));
                                }
                            } else {
                                shared.stage.set_if_current(
                                    gen,
                                    TunStage::Failed,
                                    "attach",
                                    "接管失败",
                                    false,
                                );
                            }
                            break;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {
                            if stop.load(Ordering::Acquire) {
                                shared.stage.set_if_current(
                                    gen,
                                    TunStage::Idle,
                                    "stopped",
                                    "被停止请求中断",
                                    false,
                                );
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
                shared.finish_generation(gen);
            });
            Ok(())
        }
        fn request_stop(&self) {
            self.stop_flag.store(true, Ordering::Release);
        }
        fn recover(&self, _from: i64, _cause: &str) -> i32 {
            0
        }
        fn runner(&self) -> Option<RunnerIn> {
            None
        }
        fn transport(&self) -> Option<TransportIn> {
            None
        }
    }

    fn core_with(warmup_ok: bool, attach_ok: bool) -> ClientCore {
        let exec = Arc::new(FakeExec {
            warmup_ok,
            attach_ok,
            stop_flag: Arc::new(AtomicBool::new(false)),
        });
        ClientCore::new(exec)
    }

    /// 等 stage 到目标态（或超时红）。
    fn wait_stage(core: &ClientCore, want: TunStage, budget: Duration) {
        let deadline = std::time::Instant::now() + budget;
        while core.tun.stage.snapshot().stage != want {
            if std::time::Instant::now() >= deadline {
                panic!(
                    "等 stage={:?} 超时（现 {:?}）",
                    want,
                    core.tun.stage.snapshot().stage
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// 两阶段全流程：prepare（受理 → ready）→ attach（0）→ running=1 → stop（0）。
    #[test]
    fn two_phase_lifecycle() {
        let core = core_with(true, true);
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
        wait_stage(&core, TunStage::Ready, Duration::from_secs(2));
        assert!(core.tun_status().contains("\"state\":\"ready\""));
        assert!(core.tun_status().contains("\"readyBy\":\"wg\""));
        assert_eq!(core.tun_attach(90, 1280), 0);
        wait_stage(&core, TunStage::Attached, Duration::from_secs(2));
        assert!(core.tun_status().contains("\"state\":\"attached\""));
        assert_eq!(core.tun_running(), 1);
        assert!(core.tun_status().contains("\"running\":1"));
        assert_eq!(core.tun_stop(), 0);
        wait_stage(&core, TunStage::Idle, Duration::from_secs(2));
        assert_eq!(core.tun_running(), 0);
    }

    /// rc 契约：-1 忙 / -3 参数（坏 JSON、空 token）/ -2 日志。
    #[test]
    fn prepare_rc_contract() {
        let core = core_with(true, true);
        assert_eq!(core.tun_prepare("{oops", true), -3);
        assert_eq!(core.tun_prepare(r#"{"mtu":1280}"#, true), -3);
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, false), -2);
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
        // 在世世代：-1 忙
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), -1);
    }

    /// attach rc 契约：fd≤0 → -3 + failed；未 ready → -1；失败 → -4 + failed 终态保留。
    #[test]
    fn attach_rc_contract() {
        let core = core_with(true, false);
        assert_eq!(core.tun_attach(0, 1280), -3);
        assert!(core.tun_status().contains("\"code\":\"attach\""));
        // 未 prepare：-1
        let core2 = core_with(true, true);
        assert_eq!(core2.tun_attach(90, 1280), -1);
        // prepare 后 attach 失败：-4 + failed 终态保留 + 放锁（可再次 prepare）
        let core3 = core_with(true, false);
        assert_eq!(core3.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
        wait_stage(&core3, TunStage::Ready, Duration::from_secs(2));
        assert_eq!(core3.tun_attach(90, 1280), -4);
        assert!(
            core3.tun_status().contains("\"state\":\"failed\""),
            "failed 终态保留"
        );
        assert_eq!(core3.tun_running(), 0);
        // failed 后锁已放：可重新 prepare
        assert_eq!(core3.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
    }

    /// stop 契约：本就没跑 0；收工完成 0（终态由世代写——工单③）；收工卡死 →
    /// 强制放锁 -2（此后可重启）。
    #[test]
    fn stop_rc_contract() {
        let core = core_with(true, true);
        assert_eq!(core.tun_stop(), 0); // 本就没跑
        core.tun_prepare(r#"{"token":"hmw1-x"}"#, true);
        // 世代线程挂住不收 stop（stop_tx 被 drop 前挂在 recv 上）⇒ 等 3s 后 -2 放锁
        // 受控执行体收 stop 即收尾 ⇒ 正常路径 0
        wait_stage(&core, TunStage::Ready, Duration::from_secs(2));
        assert_eq!(core.tun_stop(), 0);
        // 放锁后可重启（无 -1 忙）
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
    }

    /// 同步硬失败（warmup 返回 Err）：受理 0 + failed 终态可读 + 锁已放。
    #[test]
    fn sync_hard_fail_releases_lock() {
        let core = ClientCore::new(Arc::new(NoopTunExecutor));
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
        assert!(core.tun_status().contains("\"state\":\"failed\""));
        assert!(core.tun_status().contains("无执行体"));
        // 锁已放：可再次 prepare
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), 0);
    }

    /// SetForeground 返回上一状态；false→true 踢补探钩。
    #[test]
    fn foreground_prev_and_kick() {
        let core = core_with(true, true);
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

    /// portfwd 热替换门：attached 才收（Noop/Fake 执行体恒 -1——**无承载 = 真话**；
    /// 真承载执行体（`TunnelExec`）的 `0` 在 `tun_exec` 侧覆盖）；校验失败 -2
    /// （含 Q-F-B 新增的**条数上限**）。
    #[test]
    fn port_forwards_gate() {
        let core = core_with(true, true);
        // JSON 非法 → -2
        assert_eq!(core.tun_set_port_forwards("{oops"), -2);
        // 校验不过（listen 0）→ -2
        assert_eq!(
            core.tun_set_port_forwards(
                r#"{"portForwards":[{"listen":0,"targetIp":"","targetPort":80}]}"#
            ),
            -2
        );
        // 条数超过上限（9 条 > 8）→ -2（Q-F-B F5-2）
        let nine: String = format!(
            r#"{{"portForwards":[{}]}}"#,
            (0..9)
                .map(|i| format!(r#"{{"listen":{},"targetIp":"","targetPort":80}}"#, 18080 + i))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert_eq!(
            core.tun_set_port_forwards(&nine),
            -2,
            "超条数 = -2（NAPI 门；装配期旁路另有逐条防御）"
        );
        // 未 attached → -1
        assert_eq!(
            core.tun_set_port_forwards(
                r#"{"portForwards":[{"listen":18080,"targetIp":"","targetPort":80}]}"#
            ),
            -1
        );
        core.tun_prepare(r#"{"token":"hmw1-x"}"#, true);
        wait_stage(&core, TunStage::Ready, Duration::from_secs(2));
        core.tun_attach(90, 1280);
        wait_stage(&core, TunStage::Attached, Duration::from_secs(2));
        // attached 后过了门，执行体无承载 ⇒ 仍 -1（改动随下次连接生效——语义正确）
        assert_eq!(
            core.tun_set_port_forwards(
                r#"{"portForwards":[{"listen":18080,"targetIp":"","targetPort":80}]}"#
            ),
            -1
        );
    }

    /// rename 面（评审 r1-F05/F08 整改的守卫）：真实 App 报文（camelCase）→ 全字段
    /// 非默认都能解进来；snake_case 报文不受认（不双形态容忍）。
    #[test]
    fn config_json_camelcase_rename() {
        let cfg: TunConfigJson = serde_json::from_str(
            r#"{"mtu":1280,"out":"/l","dialMs":9000,"statsSecs":30,"endpointCacheDir":"/ec","identityDir":"/id","diagFdSecs":5,"tzOffsetMinutes":480,"token":"hmw1-x","portForwards":[{"listen":18080,"targetIp":"","targetPort":80}]}"#,
        )
        .unwrap();
        assert_eq!(cfg.mtu, 1280);
        assert_eq!(cfg.out, "/l");
        assert_eq!(cfg.dial_ms, 9000);
        assert_eq!(cfg.stats_secs, 30);
        assert_eq!(cfg.endpoint_cache_dir, "/ec");
        assert_eq!(cfg.identity_dir, "/id");
        assert_eq!(cfg.diag_fd_secs, 5);
        assert_eq!(cfg.tz_offset_minutes, 480);
        assert_eq!(cfg.token, "hmw1-x");
        assert_eq!(cfg.port_forwards.len(), 1);
        assert_eq!(cfg.port_forwards[0].listen, 18080);
        // speedtest 参数同面（r1-F08）
        let sp: speedtest_op::SpeedParams = serde_json::from_str(
            r#"{"auth":"a","sock":"s","downMs":5000,"upMs":6000,"warmupMs":700,"streams":2}"#,
        )
        .unwrap();
        assert_eq!(sp.down_ms, 5000);
        assert_eq!(sp.up_ms, 6000);
        assert_eq!(sp.warmup_ms, 700);
        assert_eq!(sp.streams, 2);
    }

    /// 版本串形状（tier core 前缀 + c-shared 尾）。
    #[test]
    fn version_shape() {
        let v = ClientCore::version();
        assert!(v.starts_with("tier core "), "{v}");
        assert!(v.ends_with(", c-shared)"), "{v}");
    }

    /// attach-timeout 文案对齐（C-5 拍板：Go 生产路径同串——阶段机的窗口形态）。
    #[test]
    fn attach_timeout_reason_literal() {
        assert_eq!(ATTACH_TIMEOUT_REASON, "就绪后无人 attach，已自行收工放锁");
    }

    /// 日志打不开 = 同步 -2 且 stage 回 idle（评审 r2-L2：此前 Preparing 已写又不收
    /// 尾 ⇒ App 轮询面停在 preparing/running:0 永远等不到结果）。
    struct LogOpenExec;
    impl TunExecutor for LogOpenExec {
        fn warmup(
            &self,
            _cfg: &TunConfigJson,
            _shared: Arc<TunShared>,
            _gen: u64,
        ) -> Result<(), TunError> {
            Err(TunError::LogOpen("permission denied".into()))
        }
        fn request_stop(&self) {}
        fn recover(&self, _from: i64, _cause: &str) -> i32 {
            -2
        }
        fn runner(&self) -> Option<RunnerIn> {
            None
        }
        fn transport(&self) -> Option<TransportIn> {
            None
        }
    }

    #[test]
    fn log_open_minus2_resets_stage_to_idle() {
        let core = ClientCore::new(Arc::new(LogOpenExec));
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), -2);
        let st = core.tun_status();
        assert!(
            st.contains("\"state\":\"idle\""),
            "log -2 后不得停在 preparing：{st}"
        );
        // 锁已放：可再次受理
        assert_eq!(core.tun_prepare(r#"{"token":"hmw1-x"}"#, true), -2);
    }
}
