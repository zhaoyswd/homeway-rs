//! 岛宿主：专用线程 + `current_thread` runtime + 命令循环 + 收工/收割。
//!
//! **本文件是岛内两处异步面之一**（另一处 = `client/**`；隔离门层 3 断言）。
//!
//! 与今日 `wgcore` 的同构性（设计 §3.2）：1 枚专用线程（`homeway-quic` ⟷ `homeway-wg`）、
//! 命令通道投递不阻塞、每命令一条回执、状态快照轮询、`Stop` + 有界 join + 到点收割线程。
//! **唯一机制偏离**：唤醒原语从「self-pipe + `poll(2)`」换成「运行时通道自带的 waker」——
//! 净减的只有唤醒 fd 一族不变量（不是「零收割期资源」：到点 detach 后岛线程照样持
//! runtime + 连接端点，残余见设计 §8.1）。
//!
//! 驱动循环（M1 S2a 起）= **三源 `select!`**（设计 §2.1 的驱动循环形态）：
//! ① 命令通道；② 长任务（赛跑 / 探活）的 `JoinSet` 回口；③ 巡检拍（TICK）。
//! 长任务一律进 `JoinSet`（M0 §3.5 的「禁裸 task」），收工 `drop(jobs)` = abort 全部；
//! 拍内务（刷新/迁移保持检测/快照同步）在原地做——与长任务无共享状态。
//!
//! 纪律：
//! - 岛内**只做非阻塞 IO**；阻塞面（TUN 读）留在调用侧的专用 std 线程（M1 起）；
//! - 岛内**零阻塞 `sleep`**（层 3 门禁）：收工的有界等待走 [`ExitSignal`] 的条件变量，
//!   到点即返回，不轮询、不 `sleep`；
//! - panic 面（设计 §3.6）：线程体套 `catch_unwind` ⇒ 就地记行 + 不健康回调 `"panic"`
//!   ⇒ 置退出位 + `resume_unwind`（让 join 侧能观测到 panic，承 §3.6-3 的兜底面）；
//!   **预算内收工**由 `stop_within` 的 join 分支记 panic 行、**到点 detach** 由收割线程记。

use std::any::Any;
use std::io;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;

use crate::client::dataplane::{self, DropNote};
use crate::client::{self, Face, Live, MigrationEvent, Watch};
use crate::cmd::{
    Candidate, Cmd, DropReason, IslandErr, IslandEvent, IslandSnapshot, Logf, OnEvent, OnUnhealthy,
    RaceOutcome, Via,
};
use crate::config::IslandConfig;
use crate::sync_util::{lock_unpoison, log_spawn_failed, ExitSignal};
use crate::tun::{self, ReturnPath, TunCounters};

/// 驱动线程名（镜像 `homeway-wg`）。
pub(crate) const ISLAND_THREAD: &str = "homeway-quic";
/// 到点收割线程名（镜像 `hw-engine-reap`）。
pub(crate) const REAP_THREAD: &str = "hw-quic-reap";
/// 命令等待节拍上限（镜像 `wgcore` 的 `POLL_CAP = 250ms`）：无命令时周期性回看 stop 位
/// （拍内务——刷新/迁移保持检测/快照同步——挂在这一拍上）。
const TICK: Duration = Duration::from_millis(250);
/// 不健康原因（判据语义取值集 `{patrol, fd, panic, stop}` 的一员）。
const REASON_PANIC: &str = "panic";
/// QUIC 承载的巡检判死分类（设计 §2.5：**归既有取值 `patrol`**，不新增枚举值）。
const REASON_PATROL: &str = "patrol";
/// TUN fd 面判死分类（镜像 `wgcore::TunFdDead → markUnhealthy("fd")`；既有取值集的一员）。
const REASON_FD: &str = "fd";
/// 无注入（**生产路径恒取此值**；注入缝只在 `#[cfg(test)]` 的 `seams` 面里活）。
const NO_SEAM: u8 = 0;

/// 记行节流（仓内既有口径「首 3 + 每 100」——`relay/mod.rs` 的 `reject_log_due` 同款）。
fn log_due(n: u64) -> bool {
    n <= 3 || n.is_multiple_of(100)
}

/// 命令投递口（[`Island::tx`] 的产出）。
///
/// **三条封印**（写进 `lib.rs` 契约，防后续提交把边界打开）：不实现 `Deref`、
/// 不提供取内件的 accessor、`Clone` 返回本类型。
#[derive(Clone)]
pub struct IslandTx(UnboundedSender<Cmd>);

impl IslandTx {
    /// 投递命令（**不阻塞**——unbounded）。返回 `Err(EngineGone)` = 岛已收工/线程已退出。
    ///
    /// M1 起 TUN 读线程的存活纪律（设计 §3.6-2④，镜像 `wgcore/mod.rs` 的
    /// `if cmd_tx.send(…).is_err() { return; }`）：投递失败即自退，不重试、不挂死。
    pub fn send(&self, cmd: Cmd) -> Result<(), IslandErr> {
        self.0.send(cmd).map_err(|_| IslandErr::EngineGone)
    }
}

/// 岛线程的共享面（起线程时一次装配——避免超参函数）。
struct IslandCtx {
    snapshot: Arc<Mutex<IslandSnapshot>>,
    stop: Arc<AtomicBool>,
    exit: Arc<ExitSignal>,
    logf: Logf,
    /// 不健康回调（可被 `SetOnUnhealthy` 运行时替换 ⇒ 锁内只放 `Arc<dyn Fn>` 的搬运）。
    on_unhealthy: Mutex<OnUnhealthy>,
    /// 事件回调（`SetOnEvent` 安装；未装 = 只入快照 + 行）。`Arc` 形态：数据面任务
    /// （回程泵）也要经同一条计数面上报。
    on_event: Arc<Mutex<Option<OnEvent>>>,
    /// TUN 面的共享计数（读线程/写线程两写、同步面读——`wgcore::TunCounters` 语义）。
    counters: Arc<TunCounters>,
}

impl IslandCtx {
    fn unhealthy(&self, reason: &str) {
        let h = lock_unpoison(&self.on_unhealthy).clone();
        h(reason);
    }
}

/// 岛句柄（同步面）：命令投递口 + 状态轮询 + 收工（幂等，`&self`——`Arc<Island>` 共享时
/// 也能收口，形态同 `wgcore::Client`）。
pub struct Island {
    tx: IslandTx,
    /// stop 位（幂等闸 + 驱动循环回看点；与 `wgcore::Client` 同形）。
    stop: Arc<AtomicBool>,
    /// JoinHandle（`Option::take` 天然防 double-join：到点 detach 时交收割线程）。
    handle: Mutex<Option<JoinHandle<()>>>,
    /// 岛线程退出信号（有界等待面）。
    exit: Arc<ExitSignal>,
    /// 状态快照（同步面轮询）。
    snapshot: Arc<Mutex<IslandSnapshot>>,
    /// TUN 面计数（需求信号读面：`swap_out_pkts`/`last_outbound_at` 与 `wgcore` 同接口）。
    counters: Arc<TunCounters>,
    logf: Logf,
    on_unhealthy: OnUnhealthy,
}

impl Island {
    /// 起岛：装配 + 起专用线程（线程名 `homeway-quic`；`current_thread` runtime）。
    ///
    /// `on_unhealthy` = 岛侧不健康回调（**岛线程内**执行；只允许内存操作/通道投递）；
    /// `cfg` = 构造配置（凭据/绑定/巡检节拍——见 [`crate::IslandConfig`]）。
    ///
    /// **返回失败即无岛**：QUIC 端点/身份在此定音（`client::Face::open`），失败不让
    /// 「起了但死的岛」出到调用侧（形态同 `ExitQuic::start` 的 ready 握手）。
    pub fn start(
        logf: Logf,
        on_unhealthy: OnUnhealthy,
        cfg: IslandConfig,
    ) -> std::io::Result<Island> {
        Self::start_inner(logf, on_unhealthy, cfg, NO_SEAM)
    }

    fn start_inner(
        logf: Logf,
        on_unhealthy: OnUnhealthy,
        cfg: IslandConfig,
        seam: u8,
    ) -> std::io::Result<Island> {
        // runtime 在**起线程前**建：建不出来就别起（失败即报错，不留"起了但死的岛"）。
        // `current_thread` = 单线程结构不变量（`rt-multi-thread` feature 不启用）；
        // `enable_all()` 开时间驱动，IO 驱动由异步栈的 runtime 后端 feature 统一带入。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (tx, rx) = tmpsc::unbounded_channel::<Cmd>();
        let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();
        let snapshot = Arc::new(Mutex::new(IslandSnapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let exit = Arc::new(ExitSignal::new());
        let counters = TunCounters::new();
        let patrol = cfg.patrol;

        let ctx = IslandCtx {
            snapshot: Arc::clone(&snapshot),
            stop: Arc::clone(&stop),
            exit: Arc::clone(&exit),
            logf: Arc::clone(&logf),
            on_unhealthy: Mutex::new(Arc::clone(&on_unhealthy)),
            on_event: Arc::new(Mutex::new(None)),
            counters: Arc::clone(&counters),
        };
        let tier_tx = IslandTx(tx.clone()); // 岛线程侧的命令口（TUN 线程也各持一份）
        let handle = thread::Builder::new()
            .name(ISLAND_THREAD.into())
            .spawn(move || {
                thread_body(rt, rx, tier_tx, ctx, cfg, patrol, ready_tx, seam)
            })?;

        // 端点/身份的就绪握手（失败 → join 取回证据 → 报错，不留半起态）
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = handle.join();
                return Err(e);
            }
            Err(_) => {
                let _ = handle.join();
                return Err(io::Error::other("岛线程在就绪前退出（见其 panic 记行）"));
            }
        }

        Ok(Island {
            tx: IslandTx(tx),
            stop,
            handle: Mutex::new(Some(handle)),
            exit,
            snapshot,
            counters,
            logf,
            on_unhealthy,
        })
    }

    /// 命令投递口（clone 给 TUN 读线程/世代线程；投递不阻塞）。
    pub fn tx(&self) -> IslandTx {
        self.tx.clone()
    }

    /// 状态快照（轮询；无阻塞）。
    ///
    /// 计数面（`packets_out`）**原子直读**（与出口面的 `ExitStats::snapshot` 同款：反应式、
    /// 不必等巡检拍）——每连接/每包的计数不该有 250ms 的快照延迟。
    pub fn snapshot(&self) -> IslandSnapshot {
        let mut s = lock_unpoison(&self.snapshot).clone();
        s.packets_out = self.counters.write_pkts();
        s
    }

    /// 取走并清零 App 出站包计数（**需求信号的生产者面**；设计 §2.5 的「接口不变、来源
    /// 切换」——名字/语义与 `wgcore::Client::swap_out_pkts` 同形，quic 档的消费点
    /// （`session/recover` 的 rearm/heal）改读本方法）。
    pub fn swap_out_pkts(&self) -> i64 {
        self.counters.swap_out_pkts()
    }

    /// 最近出站包时刻（**单调面**换算；语义与 `wgcore::Client::last_outbound_at` 同形：
    /// 0 = 本世代从未有过应用出站）。
    pub fn last_outbound_at(&self) -> Option<Instant> {
        self.counters.last_outbound_at()
    }

    /// 最近出站包时刻（unix 毫秒；0 = 从未）——`wgcore::Client::last_outbound_unix_ms` 同形。
    pub fn last_outbound_unix_ms(&self) -> i64 {
        self.counters
            .last_outbound_ns
            .load(std::sync::atomic::Ordering::Relaxed)
            / 1_000_000
    }

    /// TUN fd 字节对表（读=应用出站方向累计、写=应用入站方向累计）——
    /// **接口与语义镜像** `wgcore::Client::tun_stats`（世代层 runner 块换源用：
    /// quic 档的 `stats{fdReadBytes,fdWriteBytes}` 取自岛）。
    pub fn tun_stats(&self) -> (u64, u64) {
        (
            self.counters
                .read_bytes
                .load(std::sync::atomic::Ordering::Relaxed),
            self.counters
                .write_bytes
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// 岛线程是否已退出（`ExitSignal` 置位面；收工后判定与 M1 排障用）。
    pub fn is_finished(&self) -> bool {
        self.exit.is_exited()
    }

    /// 收工（幂等；**无界 join**——跟随岛线程自然退出）。
    ///
    /// 量级参照：收工预算面一律走 [`Self::stop_within`]（M1 起 = 世代收尾链三处）；
    /// 本无界版只留 `Drop for Island` 兜底（镜像 `Drop for Client`）。锁一律 `lock_unpoison`。
    pub fn stop(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return; // 已收工（幂等）
        }
        let _ = self.tx.send(Cmd::Stop);
        if let Some(h) = lock_unpoison(&self.handle).take() {
            join_and_classify(h, &self.logf, &self.on_unhealthy);
        }
    }

    /// 有界收工：到点放弃 join，把 `JoinHandle` 交**收割线程** `hw-quic-reap` 收口。
    ///
    /// **返回值语义（写清，防 M1 收尾链误读）**：
    /// - `true` = 本次调用确认「收工已完成」——含两种情形：①本次等到岛线程退出并在本线程
    ///   join 收口（panic 行由本分支记）；②**重入**：此前已有人收过工（stop 位已置，本调用
    ///   不做任何等待即返 `true`）；无句柄（理论窗口）同此。
    /// - `false` = 本次**到点 detach**（岛线程可能存活到自行退出，持 runtime + 连接端点——
    ///   残余登记见设计 §8.1：老世代可能在收尾后继续发包、`CONNECTION_CLOSE` 晚到）。
    ///   ⚠️ 形态提醒（镜像 `wgcore::Client::stop_within`）：detach 后再调用会因重入而返
    ///   `true`——「`true`」是「收工流程已了结」，不是「线程必已退出」；判线程是否真退出
    ///   用 [`Self::is_finished`]。
    pub fn stop_within(&self, deadline: Instant) -> bool {
        if self.stop.swap(true, Ordering::SeqCst) {
            return true; // 重入：已有人收过工（不重复等待）
        }
        let _ = self.tx.send(Cmd::Stop);
        let h = match lock_unpoison(&self.handle).take() {
            None => return true, // 无句柄（理论窗口）：收工请求已发，视同已收
            Some(h) => h,
        };
        if self.exit.wait_exit(deadline) {
            join_and_classify(h, &self.logf, &self.on_unhealthy);
            return true;
        }
        // 到点 detach（设计 §3.6-4：到点 detach ⇒ panic 行/卡死记录由收割线程的 join 分支产生）
        //
        // M0 §8.1 残余项的**可观测面**（设计 §10 S2-5 判据）：detach 后老世代仍持 runtime
        // + quinn `Endpoint`（UDP fd + 缓冲），可能继续对出口发包 ⇒ 就地打一行**计数行**
        // （UDP 源端口 + 在用连接数），快照同源可轮询（`IslandSnapshot::{local,connections}`）。
        let (local, conns) = {
            let s = lock_unpoison(&self.snapshot);
            (s.local, s.connections)
        };
        (*self.logf)(&format!(
            "quic: 到点 detach —— 老世代仍持 UDP 源端口 {}、连接 {} 条（可能继续对出口发包；M0 §8.1 残余）",
            local
                .map(|a| a.to_string())
                .unwrap_or_else(|| "（未就绪）".to_owned()),
            conns
        ));
        let logf = Arc::clone(&self.logf);
        let on_unhealthy = Arc::clone(&self.on_unhealthy);
        let spawned = thread::Builder::new()
            .name(REAP_THREAD.into())
            .spawn(move || {
                (logf)(&format!(
                    "{REAP_THREAD}: 到点 detach—— 岛线程未在收工预算内退出，由本线程等待其自行退出"
                ));
                join_and_classify(h, &logf, &on_unhealthy);
            });
        if let Err(e) = spawned {
            // 极端形态（线程资源耗尽）：收割线程起不来 ⇒ 放弃 join 侧记录
            // （JoinHandle 随 drop 分离，岛线程自行退出；不静默——记行留证）。
            log_spawn_failed(
                &self.logf,
                REAP_THREAD,
                &e,
                "岛线程的 join 侧记录缺席（岛线程自行退出）",
            );
        }
        false
    }
}

impl Drop for Island {
    /// 兜底收工（**无界 join**——镜像 `Drop for Client`）。
    ///
    /// ⚠️ 前提：仅当**从未**走到「到点 detach」时才会在此真等；一旦 `stop_within` 已置 stop 位
    /// （含 detach 形态），本兜底立即返回、不会挂死调用方。若有人不调 `stop_within` 直接 drop
    /// 而岛又卡死，则本 Drop 会一直等（与既有的 `Drop for Client` 同形，M1 收尾链一律先走
    /// 有界版）。
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------- 线程体 ----------

/// 岛线程体：`catch_unwind` 就地分类 ⇒ 置退出位 ⇒ `resume_unwind`。
///
/// 为什么 `resume_unwind`：设计 §3.6-3 要求 join 侧能识别「岛线程 panic」（记行 +
/// 再走一次不健康回调），而 §3.6-1 要求分类**即时**（不等收尾）。二者同时成立只有
/// 「先就地处置、再把 panic 交还 join」一条路——`resume_unwind` 不会重放 panic hook。
#[allow(clippy::too_many_arguments)] // 线程起点一次性移交全部状态（同 `exit::thread_body` 口径）
fn thread_body(
    rt: Runtime,
    rx: UnboundedReceiver<Cmd>,
    tx: IslandTx,
    ctx: IslandCtx,
    cfg: IslandConfig,
    patrol: Duration,
    ready_tx: mpsc::Sender<io::Result<()>>,
    seam: u8,
) {
    let res = catch_unwind(AssertUnwindSafe(|| {
        run_driver(&rt, rx, tx, &ctx, cfg, patrol, ready_tx, seam)
    }));
    if let Err(payload) = res {
        let msg = panic_msg(payload.as_ref());
        (ctx.logf)(&format!(
            "quic: 岛内 panic（{msg}）—— 判不健康（panic），岛线程退出（不复用该线程）"
        ));
        ctx.unhealthy(REASON_PANIC);
        ctx.exit.mark_exited();
        std::panic::resume_unwind(payload);
    }
    ctx.exit.mark_exited();
}

/// 长任务的一次性结论（进 `JoinSet`；收工时随 `drop(jobs)` 一起 abort）。
enum Job {
    /// 赛跑 + 登记（回执口随任务搬走；胜者原料在 `Ok` 里）。
    Race {
        reply: crate::cmd::IslandReply<RaceOutcome>,
        result: Result<(client::Established, RaceOutcome), IslandErr>,
    },
    /// 探活（回执口同上）。
    Probe {
        reply: crate::cmd::IslandReply<Duration>,
        result: Result<Duration, IslandErr>,
    },
    /// 回程泵（常驻任务：DATAGRAM → 有界队列 → TUN 写线程；`Ok(())` = 连接结束/收工）。
    /// 参数 = 该泵所属连接的 `stable_id()`（复位防重位时按身份比对，见 `pump_conn`）。
    Return(usize),
}

/// TUN 面（数据面的 fd 侧；`TunAttach` 时装配，随岛收工/连接结束而弃）。
///
/// 只留「数据面需要读的两件」：内层 MTU（窄路径判据）与回程队列（回程泵的落点）。
/// fd 本身由两枚线程持有（读/写各一），岛线程**不再碰 fd**（所有权在扩展）。
struct TunPlane {
    /// 内层 MTU（N-a 行与「窄路径不可用」判据的输入；1280）。
    mtu: u32,
    /// 回程队列（岛 → 写线程；有界 2048 条）。
    ret: Arc<ReturnPath>,
}

/// 岛线程独占的驱动状态（不上锁——只有本线程碰它）。
struct DriverState {
    /// 已登记的在用连接（`None` = 未建连/连接已断）。
    live: Option<Live>,
    /// `SetCandidates`/`Connect` 登记的候选（`Connect` 的空 `cands` 回落到它）。
    cands: Vec<Candidate>,
    /// 赛跑在途（同刻只允许一轮：赛跑结论是一次性的）。
    racing: bool,
    /// C4' 行节流器（跨轮复用）。
    race_gate: client::LogGate,
    /// 累计投出的候选数（快照 `mirrors`）。
    mirrors: u64,
    /// 迁移保持检测窗（重绑后起）。
    watch: Option<Watch>,
    /// N-a 行只打一次（端点就绪 + 首个连接的实测 MTU）。
    na_logged: bool,
    /// 巡检节拍（刷新 + 迁移保持判据；构造期定案，运行期不改）。
    patrol: Duration,
    /// TUN 面（数据面；未附加 = `None`）。
    tun: Option<TunPlane>,
    /// 回程泵防重位：记的是**在跑泵所属连接的 `stable_id()`**（`None` = 无泵在跑）。
    /// 用连接身份而非裸 bool 的理由（代码门 r13 的 M1）：`adopt` 换连接时旧泵必然随旧
    /// 连接（已 `close`）退出，裸 bool 会残留 `true` ⇒ 现任连接**永不起泵**（回程黑洞）；
    /// 改成身份比对后，「被替换连接的老泵退出」不会复位现任泵的位（只有身份相同才复位）。
    pump_conn: Option<usize>,
    /// 「窄路径不可用」行的去重位（记的是当时观测到的 `mds`）。
    narrow_logged: Option<u32>,
}

/// 驱动循环：命令 / 长任务回口 / 巡检拍**三源** `select!`（M1 设计 §2.1 的驱动循环形态）。
///
/// 与 `wgcore` 的 `poll(2)` 形态同义：命令到点即处置（无需回执的热路径直落），
/// 无命令则按 [`TICK] 回看 stop 位并做拍内务。
#[allow(clippy::too_many_arguments)] // 同 `thread_body`：线程起点单参化无收益
fn run_driver(
    rt: &Runtime,
    mut rx: UnboundedReceiver<Cmd>,
    tx: IslandTx,
    ctx: &IslandCtx,
    cfg: IslandConfig,
    patrol: Duration,
    ready_tx: mpsc::Sender<io::Result<()>>,
    seam: u8,
) {
    rt.block_on(async move {
        let mut face = match Face::open(cfg, Arc::clone(&ctx.logf)) {
            Ok(f) => f,
            Err(e) => {
                let _ = ready_tx.send(Err(e));
                return;
            }
        };
        if ready_tx.send(Ok(())).is_err() {
            return; // 调用侧已放弃（start 失败路径）——直接收摊
        }
        let mut st = DriverState {
            live: None,
            cands: Vec::new(),
            racing: false,
            race_gate: client::LogGate::new(),
            mirrors: 0,
            watch: None,
            na_logged: false,
            patrol,
            tun: None,
            pump_conn: None,
            narrow_logged: None,
        };
        let mut jobs: JoinSet<Job> = JoinSet::new();
        loop {
            if ctx.stop.load(Ordering::SeqCst) {
                break;
            }
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Stop) => break,
                    Some(c) => handle_cmd(c, &mut st, &mut face, ctx, &mut jobs, &tx, seam).await,
                    // 所有投递口掉光（实践里非主退出路径——同步面的岛句柄持 sender；
                    // 主路径是 `Cmd::Stop`／stop 位，见设计 §3.2）
                    None => break,
                },
                Some(joined) = jobs.join_next(), if !jobs.is_empty() => {
                    match joined {
                        Ok(Job::Race { reply, result }) => {
                            st.racing = false;
                            match result {
                                Ok((est, outcome)) => {
                                    adopt(&mut st, ctx, &face, est, &mut jobs);
                                    let _ = reply.send(Ok(outcome));
                                }
                                Err(e) => {
                                    (*ctx.logf)(&format!("quic: 赛跑未成（{e}）"));
                                    let _ = reply.send(Err(e));
                                }
                            }
                        }
                        Ok(Job::Probe { reply, result }) => {
                            let _ = reply.send(result);
                        }
                        // 回程泵结束（连接死/隧道面收工）：连接死面由 housekeeping 统一处置。
                        // **只有"现任连接"的那枚泵**才复位防重位——被替换连接的老泵退出时
                        // 若一并复位，会误判"无泵"（代码门 r13 的 M1）。
                        Ok(Job::Return(conn)) => {
                            if st.pump_conn == Some(conn) {
                                st.pump_conn = None;
                            }
                        }
                        // 任务被 abort（收工路径）：其回执口随之 drop ⇒ 调用侧归 EngineGone
                        Err(_aborted) => {}
                    }
                }
                () = tokio::time::sleep(TICK) => {}
            }
            housekeeping(&mut st, &face, ctx, seam).await;
        }
        // 收工：长任务全 abort（JoinSet drop）→ 连接面/端点随 face drop 关闭
        drop(jobs);
        (*ctx.logf)("quic: 岛收工（连接面随端点关闭）");
    });
}

/// 起回程泵（每连接一枚常驻任务；`pump_conn` 按**连接身份**防重、连接死时复位）：
/// `read_datagram` → 有界队列 → TUN 写线程（满 ⇒ 丢 + 计 `回程队列满`）。
///
/// 形态取「任务」而不是驱动循环的 select 分支：与出口面的 `conn::datagrams` 同构，
/// 且不依赖 `read_datagram` 的取消安全面（任务是独占的、只在连接死或被 abort 时结束）。
fn maybe_start_pump(st: &mut DriverState, ctx: &IslandCtx, jobs: &mut JoinSet<Job>) {
    let (Some(live), Some(tun)) = (st.live.as_ref(), st.tun.as_ref()) else {
        return;
    };
    let conn = live.conn.clone();
    let conn_id = conn.stable_id();
    if st.pump_conn == Some(conn_id) {
        return; // 现任连接的回程泵已在跑
    }
    let note = drop_note(ctx);
    let ret = Arc::clone(&tun.ret);
    jobs.spawn(async move {
        dataplane::pump_return(conn, ret, note).await;
        Job::Return(conn_id)
    });
    st.pump_conn = Some(conn_id);
}

/// 采纳胜者（§2.2 的现任裁决：**旧的已登记连接显式关闭**；N-a 行只在首个连接时打一次）。
fn adopt(
    st: &mut DriverState,
    ctx: &IslandCtx,
    face: &Face,
    est: client::Established,
    jobs: &mut JoinSet<Job>,
) {
    let patrol = st.patrol;
    let local = face.local();
    // S2-7：中继候选的（地址 → label）在采纳时**钉住**（后续 `SetCandidates` 不该把
    // 现任连接的条目抹掉——否则在用的中继连接上行会突然变裸包）。
    if let Via::Relay { label } = est.via {
        face.relays().pin(est.ep, label);
    }
    let adopted = Live::new(est, patrol);
    if let Some(old) = st.live.replace(adopted) {
        old.conn.close(quinn::VarInt::from_u32(0), b"replaced by newer race");
        if !st.na_logged {
            (*ctx.logf)("quic: 替换旧连接（旧连接已 CONNECTION_CLOSE）");
        }
    }
    if !st.na_logged {
        st.na_logged = true;
        let live = st.live.as_ref().expect("刚放入");
        (*ctx.logf)(&format!(
            "quic: 端点就绪（本地 {local}，MTU {}，max_datagram_size={}）",
            live.current_mtu(),
            live.mds().unwrap_or(0)
        ));
    }
    // 「窄路径不可用」行（设计 §12-① ⑤：`mds < 内层MTU` 时除计数外再打一条）：1280
    // 内层包在窄路径上**每一个都会被丢**（端到端 TCP 反复重传同尺寸段 ⇒ 用户感知是断），
    // 故必须**显式**告知，而不是靠计数行让人猜。
    check_narrow_path(st, ctx);
    // 数据面：隧道面已附加 ⇒ 起回程泵（S2-4）
    maybe_start_pump(st, ctx, jobs);
}

/// 「窄路径不可用」判据（`mds < 内层 MTU`；同一 `mds` 只打一次——连接换了再判）。
fn check_narrow_path(st: &mut DriverState, ctx: &IslandCtx) {
    let (Some(live), Some(tun)) = (st.live.as_ref(), st.tun.as_ref()) else {
        return;
    };
    let Some(mds) = live.mds() else { return };
    if mds >= tun.mtu || st.narrow_logged == Some(mds) {
        return;
    }
    st.narrow_logged = Some(mds);
    (*ctx.logf)(&format!(
        "quic: 窄路径不可用 —— max_datagram_size={mds}B < 内层 MTU={}B，1280 内层包将全部被丢（丢 + 计数不静默；MTU 降级旋钮 = HOMEWAY_QUIC_MTU）",
        tun.mtu
    ));
}

/// 拍内务（无命令时每 [`TICK`] 一次）：连接死 → 刷新 → 迁移保持检测 → 快照同步。
///
/// **巡检接线（设计 §2.5，S2-6 的判据面）**：
/// - QUIC 连接死（`Connection::closed()` 的可观测等价面 = `close_reason()` 非空）⇒
///   **即时**归既有分类 `patrol`（`mark_unhealthy_if_current(gen,"patrol")` 的岛侧落点）；
///   「即时」而非「等下一拍探活 3 连败」的理由：岛内没有恢复阶梯（阶梯在世代层，
///   M1 不动），连接死是**确定性**判据、不是抖动 ⇒ 让上层立刻有信号（M1 的「断线恢复
///   ≤3.5s」判据靠它，等 3×60s 的巡检拍毫无意义）。
/// - **反向腿失败不拆世代**：岛只看得见 QUIC 这条腿 ⇒ 只有 QUIC 连接的死驱动分类；
///   WG 面的巡检失败（世代层的事）不会经岛触发任何动作。
/// - **不误伤**：探活失败/超时（连接仍在）**不**触发分类——那是世代层阶梯的输入
///   （`Cmd::Probe` 的结论由世代层消费；岛不抢跑）。
async fn housekeeping(st: &mut DriverState, face: &Face, ctx: &IslandCtx, seam: u8) {
    let patrol = st.patrol;
    // ① 连接死（对端关闭/空闲回收）：清面 + 记行 + **归 `patrol` 分类**（S2-6 判据）
    if st.live.as_ref().is_some_and(|l| !l.alive()) {
        (*ctx.logf)("quic: 连接已断 —— 等上层重连/重赛跑（阶梯接线 = 世代层）");
        st.live = None;
        st.watch = None;
        // 连接死 ⇒ 其回程泵随 `read_datagram` 出错自退（`Job::Return` 按身份复位）；
        // 这里同步清掉防重位只为「新连接可在同一拍内起泵」。
        st.pump_conn = None;
        ctx.unhealthy(REASON_PATROL);
    }
    // ② 刷新到点（C15'；写失败 = 连接已断）
    if let Some(live) = st.live.as_mut() {
        if !live.refresh_if_due(face.credential(), &ctx.logf).await {
            (*ctx.logf)("quic: 注册刷新写失败 —— 判连接已断");
            st.live = None;
            st.watch = None;
            st.pump_conn = None;
            ctx.unhealthy(REASON_PATROL);
        }
    }
    // ③ 迁移保持检测（N-b / 未确认；判据本体在 `client::migration`）
    //    先取结论再动 `st.watch`（借用序：结论是值，窗是状态）。
    let ev = match (st.watch.as_ref(), st.live.as_ref()) {
        (Some(w), Some(live)) => w
            .tick(live.udp_rx(), patrol, Instant::now())
            .map(|e| (e, w.from(), w.to())),
        _ => None,
    };
    match ev {
        Some((MigrationEvent::Confirmed { elapsed }, from, to)) => {
            (*ctx.logf)(&format!(
                "quic: 迁移完成（{from} → {to}，耗时 {}）",
                client::fmt_dur(elapsed)
            ));
            lock_unpoison(&ctx.snapshot).migrations += 1;
            st.watch = None;
        }
        Some((MigrationEvent::Unconfirmed, from, to)) => {
            (*ctx.logf)(&format!(
                "quic: 迁移未确认（{from} → {to}，{} 内无对端回包 ⇒ 回落重连/重赛跑）",
                client::fmt_dur(patrol)
            ));
            lock_unpoison(&ctx.snapshot).migration_unconfirmed = true;
            st.watch = None;
            // 未确认 = 新路径不可用（协议侧要等 `max_idle_timeout` 30s 才定音）⇒ 与
            // 连接死同面：**立刻**给世代层 `patrol` 分类（回落动作在世代层；设计 §2.3）。
            ctx.unhealthy(REASON_PATROL);
        }
        None => {}
    }
    // ④ 快照同步（via/ep/rtt/mtu/candidates 一律由驱动态派生；local 取端点现值）
    {
        let mut s = lock_unpoison(&ctx.snapshot);
        s.candidates = st.cands.len();
        s.mirrors = st.mirrors;
        s.local = Some(face.local());
        s.connections = u64::from(st.live.is_some());
        s.packets_out = ctx.counters.write_pkts();
        match st.live.as_ref() {
            Some(l) => {
                s.via = Some(l.via);
                s.ep = Some(l.ep);
                s.rtt_ms = l.rtt_ms();
                s.mtu = l.mds();
                s.current_mtu = l.current_mtu();
                let (lost, cong) = l.path_stats();
                s.lost_packets = lost;
                s.congestion_events = cong;
                // N8①/S2-5：瞬时量（无连接必为 0——它不属于「累计到本世代」的那一族）
                s.send_buffer_used = l.send_buffer_used();
            }
            None => {
                s.via = None;
                s.ep = None;
                s.rtt_ms = 0;
                s.mtu = None;
                s.current_mtu = 0;
                s.send_buffer_used = 0;
            }
        }
        let (relay_tx, rx_ignored, _rx_dgrams, _tx_dgrams) = face.sock_stats();
        s.relay_tx = relay_tx;
        s.rx_ignored = rx_ignored;
    }
    // ⑤ 「窄路径不可用」判据（mds 变小/连接换过都要重判；S2-4）
    check_narrow_path(st, ctx);
    // 测试注入缝（仅 `#[cfg(test)]`；生产构建里是空函数）：连接在位后卡死——测
    // 「detach 后老世代 UDP 源端口/连接数可观测」（M0 §8.1 残余项）。
    #[cfg(test)]
    seams::apply_if_live(seam, st.live.is_some(), &ctx.logf);
    #[cfg(not(test))]
    let _ = seam;
}

/// 单条命令处置（岛线程内；带 await 的只有赛跑/探活的任务投出与换绑）。
#[allow(clippy::too_many_arguments)] // 宿主单入口（参数即句柄集；同 `exit::run_exit`）
async fn handle_cmd(
    cmd: Cmd,
    st: &mut DriverState,
    face: &mut Face,
    ctx: &IslandCtx,
    jobs: &mut JoinSet<Job>,
    tx: &IslandTx,
    seam: u8,
) {
    #[cfg(test)]
    seams::apply(seam, &ctx.logf);
    #[cfg(not(test))]
    let _ = seam;

    match cmd {
        Cmd::TunAttach { fd, mtu, reply } => {
            let already = {
                let mut s = lock_unpoison(&ctx.snapshot);
                if s.attached {
                    true
                } else {
                    s.attached = true;
                    false
                }
            };
            if already {
                let _ = reply.send(Err(IslandErr::TunAlreadyAttached));
                return;
            }
            // 数据面（S2-4）：①读线程（`Cmd::TunPacket` 生产者，投递失败自退）；
            // ②有界回程队列 + 写线程（`ReturnPath`）。fd 所有权在扩展：岛从不 close。
            let ret = match ReturnPath::new(
                fd,
                tx.clone(),
                Arc::clone(&ctx.counters),
                Arc::clone(&ctx.logf),
            ) {
                Ok(r) => r,
                Err(e) => {
                    lock_unpoison(&ctx.snapshot).attached = false; // 半起态不留
                    (*ctx.logf)(&format!("homeway-tun-write 启动失败（{e}）—— 数据面不可用"));
                    let _ = reply.send(Err(IslandErr::TunAttach(e)));
                    return;
                }
            };
            if let Err(e) = tun::spawn_reader(
                fd,
                tx.clone(),
                Arc::clone(&ctx.counters),
                Arc::clone(&ctx.logf),
            ) {
                lock_unpoison(&ctx.snapshot).attached = false;
                log_spawn_failed(&ctx.logf, tun::READ_THREAD, &e, "应用出站方向不可用（回程面照常）");
                let _ = reply.send(Err(IslandErr::TunAttach(e)));
                return;
            }
            (*ctx.logf)(&format!(
                "quic: 隧道面已附加（fd={fd}, mtu={mtu}；数据面已接线：读线程 + 回程队列 {} 条 + 写线程；fd 所有权在扩展，岛从不 close）",
                tun::RETURN_QUEUE_MAX
            ));
            st.tun = Some(TunPlane {
                mtu,
                ret,
            });
            check_narrow_path(st, ctx);
            maybe_start_pump(st, ctx, jobs);
            let _ = reply.send(Ok(()));
        }
        Cmd::TunPacket(pkt) => {
            // 在途名额交还（§6.4 的有界通道等价实现：读线程占名额、岛消费即还）
            ctx.counters.done_send();
            // 发送路径（S2-4）：①准入窗由结构保证（`Live` 只在四帧准入完成之后存在）；
            // ②/③ 检包 + 缓冲预检 + 分类计数 = `client::dataplane::send_datagram_checked`
            // （**唯一**的 `send_datagram` 调用点）；无连接 ⇒ 归 `未登记`（登记前丢弃）。
            lock_unpoison(&ctx.snapshot).packets_in += 1;
            match st.live.as_ref() {
                Some(live) => {
                    let note = drop_note(ctx);
                    dataplane::send_datagram_checked(&live.conn, pkt, &note);
                    // N8①/S2-5：缓冲占用**按包**刷新——`housekeeping` 的 TICK 分支在连续流量
                    // 下不会到点（`select!` 的 `sleep(TICK)` 每轮重建 ⇒ 只在无事件的 250ms
                    // 之后才跑），黑洞期的读数不能等到那时才可见。
                    lock_unpoison(&ctx.snapshot).send_buffer_used = live.send_buffer_used();
                }
                None => {
                    let n = pkt.len();
                    note_drop(ctx, DropReason::Unregistered, 1, "无已登记连接", Some(n as u64));
                }
            }
        }
        // 循环侧处置（幂等；重入无害）
        Cmd::Stop => {}
        Cmd::TunFdDead { msg } => {
            // 镜像 `wgcore` 的 `TunFdDead`：记行 + 既有取值 `fd` 的分类 + 卸面
            // （数据面已不可信；连接面留给世代层按阶梯处置）。
            (*ctx.logf)(&format!("quic: {msg}"));
            ctx.unhealthy(REASON_FD);
            st.tun = None;
            // 旧泵的消费者（写线程）已退：它在下一包 `try_push` 归 `Gone` 时自退（S6 的 A3）；
            // 清防重位让"同连接再附加"能起新泵（老泵已不可写，不会与新泵争抢同一队列）。
            st.pump_conn = None;
        }
        Cmd::SetOnUnhealthy { h } => {
            *lock_unpoison(&ctx.on_unhealthy) = h;
        }
        Cmd::SetOnEvent { h } => {
            *lock_unpoison(&ctx.on_event) = Some(h);
        }
        Cmd::SetCandidates { cands } => {
            let n = cands.len();
            // S2-7：候选面同时喂 socket 的中继表（发送侧包封的路由键——必须在任何
            // `connect` 之前装配，否则中继候选的握手包会以裸包发出）
            face.relays().set(&cands);
            st.cands = cands;
            lock_unpoison(&ctx.snapshot).candidates = n;
            (*ctx.logf)(&format!("quic: 候选清单已更新（{n} 条）"));
        }
        Cmd::DatagramDropped { reason, n } => {
            note_drop(ctx, reason, n, "数据面上报", None);
        }
        Cmd::Connect {
            cands,
            budget,
            reply,
        } => {
            if st.racing {
                let _ = reply.send(Err(IslandErr::RaceInFlight));
                return;
            }
            let list = if cands.is_empty() { st.cands.clone() } else { cands };
            if list.is_empty() {
                let _ = reply.send(Err(IslandErr::NoCandidate));
                return;
            }
            st.racing = true;
            st.race_gate.reset();
            let log_c4 = st.race_gate.due(Instant::now());
            st.mirrors += list.len() as u64;
            // S2-7：本轮候选（含中继类）先入 socket 的中继表，再发赛跑任务
            face.relays().set(&list);
            let endpoint = face.endpoint();
            let ccfg = face.client_config();
            let cred = Arc::clone(face.credential());
            let logf = Arc::clone(&ctx.logf);
            jobs.spawn(async move {
                let result = client::connect(endpoint, ccfg, cred, &list, budget, log_c4, &logf).await;
                Job::Race { reply, result }
            });
        }
        Cmd::Rebind { local, reply } => {
            let from = face.local();
            match face.rebind(local) {
                Ok(to) => {
                    // 保持检测窗只在有连接时起（无连接 = 只是换 socket，没有「保持」可言）
                    st.watch = st
                        .live
                        .as_ref()
                        .map(|l| Watch::start(from, to, l.udp_rx(), Instant::now()));
                    lock_unpoison(&ctx.snapshot).migration_unconfirmed = false;
                    (*ctx.logf)(&format!("quic: 本地 socket 已换绑（{from} → {to}）"));
                    let _ = reply.send(Ok(to));
                }
                Err(e) => {
                    (*ctx.logf)(&format!("quic: 换绑失败（{from} → …；{e}）"));
                    let _ = reply.send(Err(e));
                }
            }
        }
        Cmd::Probe { budget, reply } => match st.live.as_ref() {
            None => {
                let _ = reply.send(Err(IslandErr::NotConnected));
            }
            Some(live) => {
                let conn = live.conn.clone();
                jobs.spawn(async move {
                    let result = client::probe(&conn, budget).await;
                    Job::Probe { reply, result }
                });
            }
        },
    }
}

/// 记一次丢弃（四类计数 + N-c 行 + 事件回调；`ext` = 本次明细里的包长/条数补充）。
fn note_drop(ctx: &IslandCtx, reason: DropReason, n: u64, detail: &str, ext: Option<u64>) {
    note_drop_shared(&ctx.snapshot, &ctx.logf, &ctx.on_event, reason, n, detail, ext);
}

/// 计数上报的**共享件**（岛线程与数据面任务走同一份：一处字段序、一处节流口径）。
fn note_drop_shared(
    snapshot: &Mutex<IslandSnapshot>,
    logf: &Logf,
    on_event: &Mutex<Option<OnEvent>>,
    reason: DropReason,
    n: u64,
    detail: &str,
    ext: Option<u64>,
) {
    let (total, line) = {
        let mut s = lock_unpoison(snapshot);
        let total = s.drops.bump(reason, n);
        let d = s.drops;
        (
            total,
            format!(
                "超限={} 发送缓冲满={} 回程队列满={} 未登记={}",
                d.too_large, d.send_buffer_full, d.return_queue_full, d.unregistered
            ),
        )
    };
    if log_due(total) {
        let ext = ext.map(|v| format!("，字节/条数={v}")).unwrap_or_default();
        (*logf)(&format!(
            "quic: 丢弃 {line}（本次：{} ×{n} {detail}{ext}；计数行首 3 + 每 100）",
            reason.text()
        ));
    }
    let ev = lock_unpoison(on_event).clone();
    if let Some(h) = ev {
        h(IslandEvent::DatagramDropped { reason, n });
    }
}

/// 数据面任务的丢弃上报口（`Arc<dyn Fn>`：常驻任务需要 `'static`——故三件共享件都克隆）。
fn drop_note(ctx: &IslandCtx) -> DropNote {
    let snapshot = Arc::clone(&ctx.snapshot);
    let logf = Arc::clone(&ctx.logf);
    let on_event = Arc::clone(&ctx.on_event);
    Arc::new(move |reason, detail| {
        note_drop_shared(&snapshot, &logf, &on_event, reason, 1, detail, None)
    })
}

// ---------- 收工/分类小件 ----------

/// join 收口 + panic 分类（**谁记 panic 行**：预算内 = `stop_within` 的 join 分支；
/// 到点 detach = 收割线程 `hw-quic-reap` 的 join 分支——设计 §3.6-4 的分工表）。
fn join_and_classify(h: JoinHandle<()>, logf: &Logf, on_unhealthy: &OnUnhealthy) {
    if let Err(payload) = h.join() {
        let msg = panic_msg(payload.as_ref());
        (*logf)(&format!("quic: 岛线程 panic（{msg}）—— 本世代 QUIC 面已死"));
        (on_unhealthy)(REASON_PANIC);
    }
}

/// panic 载荷取文案（`&str`/`String` 两形态；其余载荷不臆测内容）。
/// `pub(crate)`：出口 QUIC 面（`exit/`）的线程体共用同一条 panic 记行口径。
pub(crate) fn panic_msg(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "非字符串 panic 载荷".to_string()
    }
}

// ---------- 测试缝（`#[cfg(test)]` 门控：release/cdylib 构建里完全消失 ⇒ 零 dead_code） ----------

/// 测试缝起岛入口（`#[cfg(test)]` ⇒ 生产构建里不存在；模式见 `seams`）。
#[cfg(test)]
impl Island {
    pub(crate) fn start_with_seam(
        logf: Logf,
        on_unhealthy: OnUnhealthy,
        cfg: IslandConfig,
        seam: u8,
    ) -> std::io::Result<Island> {
        Self::start_inner(logf, on_unhealthy, cfg, seam)
    }
}

/// 依赖面**活体证据**的运行时侧（设计 §3.7 用例 8）：在 `current_thread` runtime 上下文里
/// 建一枚回环客户端端点、读回内核分配的端口后丢弃——**不建连接、不做身份**。
///
/// 放在本文件而不是 `tests.rs`：异步栈名字只允许出现在异步面（隔离门层 3）。
#[cfg(test)]
impl IslandTx {
    /// 测试缝：一枚**无接收者**的投递口（`send` 恒 `EngineGone`——与「岛已收工」同面）。
    /// 生产路径的命令口**恒**由 [`Island::tx`] 给出（构造面不开在这个缝上）。
    pub(crate) fn dead_for_test() -> IslandTx {
        let (tx, rx) = tmpsc::unbounded_channel::<Cmd>();
        drop(rx);
        IslandTx(tx)
    }
}

#[cfg(test)]
pub(crate) fn dep_face_alive_endpoint() -> std::io::Result<std::net::SocketAddr> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let _guard = rt.enter(); // 端点构造需要 runtime 上下文
    let endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap())?;
    let addr = endpoint.local_addr()?;
    drop(endpoint);
    Ok(addr)
}

/// M0 挂起/panic 注入缝（设计 §3.7：**不用** cargo feature——那会经 CLI 透传成生产可注入面）。
///
/// 模式按**岛实例**传入（不是全局静态）⇒ 并发用例互不污染。`tests.rs` 经 `super::` 触达。
/// 每个模式在动作前先记一行「注入开始」（`MARK_*`）——用例据此**确定性地**知道岛已消费
/// 该命令并进入注入态（否则 `stop_within` 可能与「stop 位已置但命令尚在队列里」竞态：
/// 驱动循环头判 stop 位就退出，队列里的命令会被丢弃——与 `wgcore` 的 stop 位同义）。
#[cfg(test)]
pub(crate) mod seams {
    use std::time::Duration;

    use crate::cmd::Logf;

    /// 命令处置点就地 panic（测即时分类 + 在途回执归 `EngineGone`）。
    pub(crate) const PANIC: u8 = 1;
    /// 命令处置点永久卡死（测到点 detach + 收割线程接手）。
    pub(crate) const HANG: u8 = 2;
    /// 连接在位后的拍内务里**永久卡死**（测「detach 后老世代 UDP 源端口/连接数可观测」；
    /// 与 `HANG` 的区别：本模式**先让岛真的连上**再卡，故 detach 时确有在用连接）。
    pub(crate) const HANG_WITH_LIVE: u8 = 4;
    /// 连接在位卡死的「注入开始」记行（用例的确定性同步点）。
    pub(crate) const MARK_HANG_LIVE: &str = "测试注入：连接后在拍内务卡死";

    /// 命令处置点卡死约 3s 后再 panic（测「到点 detach 后，panic 行由收割线程记」）。
    /// **窗长 3s（不是 1.2s）**：用例在「见到 MARK_STALL」后才起 200ms 的 `stop_within` 预算，
    /// 若测试线程在两步之间被调度延迟 > 窗长，岛已 panic ⇒ `wait_exit` 立即为真 ⇒「必须 detach」
    /// 的断言会偶发假红（本棒实测 1/30）。窗给足即消除该不确定性。
    pub(crate) const STALL_THEN_PANIC: u8 = 3;

    /// 注入开始的记行标记（用例的确定性同步点；与上面的模式码一一对应）。
    pub(crate) const MARK_PANIC: &str = "测试注入：岛内 panic";
    pub(crate) const MARK_HANG: &str = "测试注入：永久卡死";
    pub(crate) const MARK_STALL: &str = "测试注入：卡死 3s 后 panic";

    /// 命令到点时的注入动作（岛线程内、异步上下文里执行；先记行再动作）。
    pub(crate) fn apply(mode: u8, logf: &Logf) {
        match mode {
            PANIC => {
                (*logf)(MARK_PANIC);
                panic!("{MARK_PANIC}");
            }
            HANG => {
                (*logf)(MARK_HANG);
                block_forever();
            }
            STALL_THEN_PANIC => {
                (*logf)(MARK_STALL);
                block_briefly(Duration::from_millis(3000));
                panic!("{MARK_STALL}");
            }
            _ => {}
        }
    }

    /// 连接在位 ⇒ 记行 + 永久卡死（`HANG_WITH_LIVE` 的拍内务落点）。
    pub(crate) fn apply_if_live(mode: u8, live: bool, logf: &Logf) {
        if mode == HANG_WITH_LIVE && live {
            (*logf)(MARK_HANG_LIVE);
            block_forever();
        }
    }

    /// 永久卡死：阻塞在**永不投递**的同步通道上（发送端绑到具名变量 ⇒ 活到作用域末，
    /// 通道不关闭 ⇒ `recv` 不返回）。不用 `std::thread::sleep`——隔离门层 3 禁阻塞 sleep。
    fn block_forever() {
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let _ = rx.recv();
    }

    /// 卡死一段（有界阻塞等待；同上，不用 `sleep`）。
    fn block_briefly(d: Duration) {
        let (_tx, rx) = std::sync::mpsc::channel::<()>();
        let _ = rx.recv_timeout(d);
    }
}
