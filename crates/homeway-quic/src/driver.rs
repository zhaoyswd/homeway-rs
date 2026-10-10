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
use std::net::SocketAddrV4;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;
use tokio::time::Instant as TokioInstant;

use crate::client::dataplane::{self, DropNote, DropNoteN};
use crate::client::ladder::{self, Ladder, Round, SendFace, Step as LadderStep};
use crate::client::streams::{self, StreamStats, Streams};
use crate::client::{self, Face, Live, MigrationEvent, Watch};
use crate::cmd::{
    Candidate, Cmd, DropReason, IslandErr, IslandEvent, IslandSnapshot, Logf, OnEvent, OnUnhealthy,
    RaceOutcome, Via,
};
use crate::config::IslandConfig;
use crate::stream::{StreamErr, OPEN_BUDGET};
use crate::sync_util::{lock_unpoison, log_spawn_failed, ExitSignal};
use crate::tun::{self, ReturnPath, TunCounters};
use crate::tuning::{apply_probe_env, apply_stream_env, ProbeTuning, StreamLimits};

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
    /// **服务流面计数**（M3 S1：写者/读任务在岛线程外加，快照装配读；同一枚 `Arc`
    /// 同时交给 `Streams` 与 `Island`）。
    stream_stats: Arc<StreamStats>,
    /// **快探成功计数**（M3 S4；原子直读——判据/e2e 的「首个回显成功」观测位不该等
    /// 拍内务的快照同步，口径同 `packets_out`）。
    ladder_probe_ok: Arc<AtomicU64>,
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
    /// **已解决的流面限制**（env 覆盖后的生效值；判据/标定读数面——S8 门槛与 S7 登记条
    /// 都读它，避免「配置里写的」与「运行时用的」两套值）。
    streams: StreamLimits,
    /// **已解决的快探参数**（同上；S4 消费）。
    probe: ProbeTuning,
    /// 快探成功计数（原子直读口；岛线程写、同步面读）。
    ladder_probe_ok: Arc<AtomicU64>,
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
        mut cfg: IslandConfig,
        seam: u8,
    ) -> std::io::Result<Island> {
        // ---- M3 §15-2/§15-3：流面与快探参数的 env 消融臂（照 `HOMEWAY_QUIC_MTU` 先例：
        // env 优先 → 显式配置 → 设计缺省；非法/越界 ⇒ 不改该项 + 记行）----
        let n_streams = apply_stream_env(&mut cfg.streams, &logf);
        let n_probe = apply_probe_env(&mut cfg.probe, &logf);
        let lim = cfg.streams;
        (*logf)(&format!(
            "流面参数（bidi={} uni={} recv_window={}B conn_recv_window={}B send_window={}B 待发={}B；有效服务流 {}；env 覆盖 {n_streams} 项）",
            lim.max_bidi,
            lim.max_uni,
            lim.recv_window,
            lim.conn_recv_window,
            lim.send_window,
            lim.pending_bytes,
            lim.service_capacity()
        ));
        let pt = cfg.probe;
        (*logf)(&format!(
            "快探参数（首探 {:?}，拍间 {:?}，复探 ×{}，待机 {:?}，抖动阈值 {}，B 门 连续 {}/窗 {:?}，发送面新鲜度窗 {:?}，在用窗 {:?}；env 覆盖 {n_probe} 项）",
            pt.fast_budget,
            pt.fast_gap,
            pt.reprobe_factor,
            pt.idle_interval,
            pt.jitter_streak,
            pt.reconnect_streak,
            pt.rebuild_window,
            pt.send_err_fresh,
            pt.in_use_fresh
        ));
        let resolved_streams = cfg.streams;
        let resolved_probe = cfg.probe;
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
        let stream_stats = Arc::new(StreamStats::default());
        let ladder_probe_ok = Arc::new(AtomicU64::new(0));
        let patrol = cfg.patrol;

        let ctx = IslandCtx {
            snapshot: Arc::clone(&snapshot),
            stop: Arc::clone(&stop),
            exit: Arc::clone(&exit),
            logf: Arc::clone(&logf),
            on_unhealthy: Mutex::new(Arc::clone(&on_unhealthy)),
            on_event: Arc::new(Mutex::new(None)),
            counters: Arc::clone(&counters),
            stream_stats: Arc::clone(&stream_stats),
            ladder_probe_ok: Arc::clone(&ladder_probe_ok),
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
            streams: resolved_streams,
            probe: resolved_probe,
            ladder_probe_ok,
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
        // M6.5：`packets_in` / `send_buffer_used` 也改**原子直读**（热路径不再每包上锁；
        // 口径与 `packets_out` 同款：反应式读数不等拍内务同步）
        s.packets_in = self.counters.pkts_in.load(Ordering::Relaxed);
        s.send_buffer_used = self.counters.send_buf_used.load(Ordering::Relaxed);
        // M3 S4：快探成功计数原子直读（判据面「首个回显成功」不等拍内务同步）
        s.ladder_probe_ok = self.ladder_probe_ok.load(Ordering::Relaxed);
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

    /// **生效的流面限制**（env 覆盖后；M3 §15-3 的标定读数面）。
    pub fn stream_limits(&self) -> StreamLimits {
        self.streams
    }

    /// **生效的快探参数**（env 覆盖后；M3 §15-2，S4 消费）。
    pub fn probe_tuning(&self) -> ProbeTuning {
        self.probe
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
            "到点 detach —— 老世代仍持 UDP 源端口 {}、连接 {} 条（可能继续对出口发包；M0 §8.1 残余）",
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
            "岛内 panic（{msg}）—— 判不健康（panic），岛线程退出（不复用该线程）"
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
    /// **快探阶梯**的一轮探活（M3 §3.1/§3.2；同刻只许一轮在途——单飞在阶梯里）。
    FastProbe {
        round: Round,
        result: Result<Duration, IslandErr>,
    },
    /// **快探阶梯的 R 动作**（§3.1：新 QUIC 连接 + 四帧准入；`Ok` = 已采纳的连接）。
    LadderConnect {
        why: String,
        result: Result<client::Established, IslandErr>,
    },
    /// 回程泵（常驻任务：DATAGRAM → 有界队列 → TUN 写线程；`Ok(())` = 连接结束/收工）。
    /// 参数 = 该泵所属连接的 `stable_id()`（复位防重位时按身份比对，见 `pump_conn`）。
    Return(usize),
    /// 服务流任务结束（写者任务 / 读任务；**记账已在任务内落**——见
    /// `client::streams::{writer_task,read_task}`，本变体只让 `JoinSet` 的收割口有名字）。
    StreamDone,
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
    /// **服务流注册表**（M3 S1；`Arc` 因为写者/读任务各持一份）。
    streams: Arc<Streams>,
    /// 已解决的快探参数（读快照/发送面新鲜度窗用；S4 的阶梯消费同一份）。
    probe: ProbeTuning,
    /// **快探阶梯**（M3 S4 §3.1/§3.2）：快探 → 复探 → M/R → B 的状态机（唯一动作判定面）。
    ladder: Ladder,
    /// M 动作的记账（确认探活的结论要落 N-b 行/置 `migration_unconfirmed`）。
    rebind: Option<RebindNote>,
    /// **阶梯 R 动作的候选**（§3.1：R = 新连接「**同端点**、同本地 socket」）——
    /// 采纳连接时按胜者的 `ep`/`via` 钉住（`Cmd::Connect` 的候选列表**不**进本槽：
    /// 赛跑候选面与「现任端点」是两件事，R 只保现任）。
    ladder_cand: Option<Candidate>,
    /// 「连接已断」行去重位（连接死是持续态；阶梯在动作，日志不该每拍一行）。
    dead_logged: bool,
    /// **本世代曾完成准入**（M5 §2.4-A-2 ①：准入是 QUIC 档唯一的「可用」事实）。
    ///
    /// 为什么不是 `live.is_some()` / `tun.is_some()`（两处旧判据都不成立）：
    /// ① `live` 在「连接死 → 交阶梯」的两处**先被置 `None`**（本文件 `housekeeping` 的
    /// ①/② 支）⇒ 用 `live` 判 = **恒假**（阶梯永不启动）；② `tun`（TUN 面已附加）是
    /// **数据面**事实，而 M5 起宿主会话（`facade/host_session.rs`，无 TUN）也必须让阶梯
    /// 工作（否则「无 TUN 岛的阶梯被结构性关闭」= 假「已连接」）。
    /// 准入是两档共同的可用性事实（`Live` 只在 `hr-reg4` 四帧通过后构造）⇒ 只要本世代
    /// 曾经准入过（`adopt` 置位），连接死的恢复动作就该跑。
    admitted: bool,
    /// 丢弃上报口（单条形态；**采纳连接时构造一次**——M6.5 去每包 `Arc` 分配）。
    drop_note: Option<DropNote>,
    /// 丢弃上报口（带条数；回程泵批投用）。
    drop_note_n: Option<DropNoteN>,
    /// **服务流出站需求信号**（M5 §2.4-A-2 ⑤：宿主会话里「在用档」判据的 STREAM 面来源）。
    ///
    /// 旧 `in_use_now` 只认 `TunCounters::last_outbound_at`（只由 TUN 数据面写）⇒ 无 TUN 的
    /// 宿主会话恒「待机档」（60s 探活节拍）。这里给 STREAM 面记一笔**最近成功写出的时刻**
    /// （`Cmd::StreamWrite` 回执 `n > 0`），两档合成「有出站即新鲜」——与 `tun` 档同
    /// `ProbeTuning::in_use_fresh` 窗。
    last_stream_out_at: Option<std::time::Instant>,
}

/// M（换本地 socket）动作的记账（§3.1 的 `from → to` 与确认探活的耗时）。
struct RebindNote {
    from: SocketAddrV4,
    to: SocketAddrV4,
    at: TokioInstant,
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
    // 流面限制/快探参数要在 `cfg` 被 `Face::open` 吃掉之前取出（`Streams` 与快照面共用）
    let stream_limits = cfg.streams;
    let probe_tuning = cfg.probe;
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
            streams: Arc::new(Streams::new(
                stream_limits,
                Arc::clone(&ctx.stream_stats),
                Arc::clone(&ctx.logf),
                Arc::clone(&ctx.on_event),
            )),
            probe: probe_tuning,
            ladder: Ladder::new(probe_tuning),
            rebind: None,
            ladder_cand: None,
            dead_logged: false,
            admitted: false,
            last_stream_out_at: None,
            drop_note: None,
            drop_note_n: None,
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
                                    // M3 §4：准入被拒 ⇒ 归因行（**行名前缀刻意与出口的
                                    // `quic: 准入被拒（dev=…` 区分**——设计门 P3）+ 快照两字段
                                    // （App 状态面据此回答「为什么走了 WG」）。
                                    if let IslandErr::AdmissionRejected { code } = &e {
                                        let line = crate::admit_close::client_line(*code);
                                        (*ctx.logf)(&line);
                                        let mut s = lock_unpoison(&ctx.snapshot);
                                        s.admit_reject_code = Some(*code);
                                        s.admit_reject_text =
                                            crate::admit_close::text(*code).map(str::to_owned);
                                    }
                                    (*ctx.logf)(&format!("赛跑未成（{e}）"));
                                    let _ = reply.send(Err(e));
                                }
                            }
                        }
                        Ok(Job::Probe { reply, result }) => {
                            let _ = reply.send(result);
                        }
                        // ---- M3 S4：快探阶梯的回口（§3.1 的动作顺序在此闭环）----
                        Ok(Job::FastProbe { round, result }) => {
                            let now = TokioInstant::now();
                            let ok = result.is_ok();
                            let send = send_face(&face, st.ladder.tuning().send_err_fresh);
                            let step = st.ladder.on_round(round, result, now, send, &ctx.logf);
                            // 探活成功计数**就地**发布（回显时刻即观测时刻；快照面原子直读）
                            ctx.ladder_probe_ok
                                .store(st.ladder.probe_ok, Ordering::Relaxed);
                            // M 的确认探活（N-b 行族：行文不变，窗口收窄到 ≤1 快探预算）
                            if round == Round::Confirm && ok {
                                if let Some(note) = st.rebind.take() {
                                    (*ctx.logf)(&format!(
                                        "迁移完成（{} → {}，耗时 {}）",
                                        note.from,
                                        note.to,
                                        client::fmt_dur(now.saturating_duration_since(note.at))
                                    ));
                                    lock_unpoison(&ctx.snapshot).migrations += 1;
                                    lock_unpoison(&ctx.snapshot).migration_unconfirmed = false;
                                }
                            }
                            ladder_step(&mut st, ctx, &mut face, &mut jobs, step);
                        }
                        Ok(Job::LadderConnect { why, result }) => {
                            let now = TokioInstant::now();
                            let step = match result {
                                Ok(est) => {
                                    adopt(&mut st, ctx, &face, est, &mut jobs);
                                    st.ladder.on_reconnect_result(true, why, now, &ctx.logf)
                                }
                                Err(e) => st.ladder.on_reconnect_result(
                                    false,
                                    format!("{why}（{e}）"),
                                    now,
                                    &ctx.logf,
                                ),
                            };
                            ladder_step(&mut st, ctx, &mut face, &mut jobs, step);
                        }
                        // 回程泵结束（连接死/隧道面收工）：连接死面由 housekeeping 统一处置。
                        // **只有"现任连接"的那枚泵**才复位防重位——被替换连接的老泵退出时
                        // 若一并复位，会误判"无泵"（代码门 r13 的 M1）。
                        Ok(Job::Return(conn)) => {
                            if st.pump_conn == Some(conn) {
                                st.pump_conn = None;
                            }
                        }
                        // 服务流任务结束：槽侧记账已在任务内落（`writer_finished` /
                        // 读任务回执），这里无需动作——变体存在的意义是让 abort/panic
                        // 与正常结束在同一处可见（排障时 `JoinSet` 的收割口有名字）。
                        Ok(Job::StreamDone) => {}
                        // 任务被 abort（收工路径）：其回执口随之 drop ⇒ 调用侧归 EngineGone
                        Err(_aborted) => {}
                    }
                }
                () = tokio::time::sleep(TICK) => {}
            }
            housekeeping(&mut st, &mut face, ctx, &mut jobs, seam).await;
        }
        // 收工：长任务全 abort（JoinSet drop）→ 连接面/端点随 face drop 关闭
        drop(jobs);
        (*ctx.logf)("岛收工（连接面随端点关闭）");
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
    let note = st
        .drop_note_n
        .clone()
        .unwrap_or_else(|| drop_note_with_count(ctx));
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
    // M3 S4：现任端点钉住（§3.1 的 R = 同端点重连）
    st.ladder_cand = Some(Candidate {
        addr: est.ep,
        via: est.via,
    });
    let adopted = Live::new(est, patrol);
    // M5 §2.4-A-2 ①：本世代「曾完成准入」位（`Live` 只在四帧准入之后构造）——连接死的
    // 恢复动作判据（旧 `tun.is_some()` 在宿主会话恒假 ⇒ 阶梯被结构性关闭）。
    st.admitted = true;
    // M6.5：丢弃上报口只在此构造一次（热路径零新分配）
    if st.drop_note.is_none() {
        st.drop_note = Some(drop_note_one(ctx));
        st.drop_note_n = Some(drop_note_with_count(ctx));
    }
    // M3 S4：新连接 = 新失败链 ⇒ 阶梯重新武装（B 之后休眠的阶梯在此复活）
    st.ladder.rearm();
    st.rebind = None;
    st.dead_logged = false;
    if let Some(old) = st.live.replace(adopted) {
        // 换连接 = 旧连接上的**服务流全部作废**（读回 EOF/写快速失败，§1.6 的 EOF 同形性）；
        // 不清就会留下「挂在死连接上的在册流」——`files` 那一侧会一直等不到回显。
        let n = st.streams.clear_on_connection_loss();
        if n > 0 {
            (*ctx.logf)(&format!(
                "换连接 —— 旧连接上的 {n} 条服务流按 EOF 收（调用方按需重开）"
            ));
        }
        old.conn.close(quinn::VarInt::from_u32(0), b"replaced by newer race");
        if !st.na_logged {
            (*ctx.logf)("替换旧连接（旧连接已 CONNECTION_CLOSE）");
        }
    }
    if !st.na_logged {
        st.na_logged = true;
        let live = st.live.as_ref().expect("刚放入");
        (*ctx.logf)(&format!(
            "端点就绪（本地 {local}，MTU {}，max_datagram_size={}）",
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
        "窄路径不可用 —— max_datagram_size={mds}B < 内层 MTU={}B，1280 内层包将全部被丢（丢 + 计数不静默；MTU 降级旋钮 = HOMEWAY_QUIC_MTU）",
        tun.mtu
    ));
}

/// 拍内务（无命令时每 [`TICK`] 一次）：连接死 → 阶梯 → 刷新 → 迁移保持检测 → 快照同步。
///
/// **巡检接线（M3 S4 重写，设计 §3）**：
/// - QUIC 连接死（`close_reason()` 非空）⇒ **不再**就地判不健康：岛内已有恢复阶梯
///   （快探 → 复探 → M/R → B）⇒ 连接死只是「快探必然失败的确定性形态」，交给阶梯的
///   动作链（**R 重连**保世代；连续失败才由 **B** 上报不健康 ⇒ 世代重建）。
///   M1 的「即时分类 `patrol`」（当时岛内无阶梯）由本重写取代——**语义登记**见 S7。
/// - **反向腿失败不拆世代**：岛只看得见 QUIC 这条腿 ⇒ 只有 QUIC 面的信号驱动动作；
///   WG 面的巡检失败（世代层的事）不会经岛触发任何动作。
/// - **不误伤**：探活失败/超时（连接仍在）由快探阶梯自己消费（`Cmd::Probe` 的结论仍
///   只回调用方；岛不抢跑）。
async fn housekeeping(
    st: &mut DriverState,
    face: &mut Face,
    ctx: &IslandCtx,
    jobs: &mut JoinSet<Job>,
    seam: u8,
) {
    let patrol = st.patrol;
    // ① 连接死（对端关闭/空闲回收）：清面 + 记行 + **交快探阶梯**（§3.1；不再就地判不健康）
    if st.live.as_ref().is_some_and(|l| !l.alive()) {
        // M3 §4（设计门 F4）：**准入后**的会话级关闭（被替换/设备被摘除）单独归因——
        // 与准入窗内的 `AdmissionRejected` 严格分开，不误报成「准入被拒」。
        if let Some(l) = st.live.as_ref() {
            if let IslandErr::SessionClosed { reason } = client::session_closed(&l.conn) {
                (*ctx.logf)(&format!(
                    "会话被对端关闭（{reason}）——会话级归因（非准入面）"
                ));
            }
        }
        if !st.dead_logged {
            st.dead_logged = true;
            (*ctx.logf)("连接已断 —— 交快探阶梯（M/R/B；§3.1）保世代重连，不再就地拆世代");
        }
        // M3：在册服务流一并作废（读回 EOF / 写快速失败；§1.6 的 EOF 同形性——
        // 与今天 `stackb` 在连接死时的表现一致，调用方按需重开）
        let n = st.streams.clear_on_connection_loss();
        if n > 0 {
            (*ctx.logf)(&format!("连接断 —— {n} 条在册服务流按 EOF 收"));
        }
        st.live = None;
        st.watch = None;
        // 连接死 ⇒ 其回程泵随 `read_datagram` 出错自退（`Job::Return` 按身份复位）；
        // 这里同步清掉防重位只为「新连接可在同一拍内起泵」。
        st.pump_conn = None;
        // §3.1：链路**确定性**死亡（`close_reason` 已置）⇒ 无回显是结构性的，不消耗探活
        // 预算，直接进「复探失败」面的动作判别（M/R）——这是 ≤3.5s 判据在重启相位上的落点。
        //
        // **门槛 = 本世代曾完成准入**（M5 §2.4-A-2 ①：准入是 QUIC 档唯一的「可用」事实；
        // 旧 `tun.is_some()` 是数据面事实 ⇒ 宿主会话（无 TUN）阶梯被结构性关闭）。
        if st.admitted {
            let send = send_face(face, st.ladder.tuning().send_err_fresh);
            let step = st.ladder.on_link_dead(TokioInstant::now(), send, &ctx.logf);
            ladder_step(st, ctx, face, jobs, step);
        }
    } else {
        st.dead_logged = false;
    }
    // ② 刷新到点（C15'；写失败 = 连接已断 ⇒ 交阶梯，判据同 ①）
    if let Some(live) = st.live.as_mut() {
        if !live.refresh_if_due(face.credential(), &ctx.logf).await {
            (*ctx.logf)("注册刷新写失败 —— 判连接已断（交快探阶梯）");
            st.live = None;
            st.watch = None;
            st.pump_conn = None;
            // M5 §2.4-A-2 ①：同 ① 支——判据 = 「曾完成准入」（`live` 刚被置 None ⇒ 用它恒假）。
            if st.admitted {
                let send = send_face(face, st.ladder.tuning().send_err_fresh);
                let step = st.ladder.on_link_dead(TokioInstant::now(), send, &ctx.logf);
                ladder_step(st, ctx, face, jobs, step);
            }
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
                "迁移完成（{from} → {to}，耗时 {}）",
                client::fmt_dur(elapsed)
            ));
            lock_unpoison(&ctx.snapshot).migrations += 1;
            st.watch = None;
        }
        Some((MigrationEvent::Unconfirmed, from, to)) => {
            (*ctx.logf)(&format!(
                "迁移未确认（{from} → {to}，{} 内无对端回包 ⇒ 回落重连/重赛跑）",
                client::fmt_dur(patrol)
            ));
            lock_unpoison(&ctx.snapshot).migration_unconfirmed = true;
            st.watch = None;
            // 未确认 = 新路径不可用（协议侧要等 `max_idle_timeout` 30s 才定音）⇒
            // **交快探阶梯**（§3.1：位升为动作前置条件 ⇒ 允许走 R）——M1 的「即时判
            // 不健康（世代重建）」由本重写取代（梯级动作 = M/R/B，B 才上报不健康）。
            // **判据 = 连接在位**（M5 §2.4-A-2 ③：迁移确认的语义面是「有没有连接在保」——
            // 此处 `live` 仍 `Some`（与 ①/② 支的「刚置 None」不同）⇒ 直接用它）。
            if st.live.is_some() {
                let send = send_face(face, st.ladder.tuning().send_err_fresh);
                let step = st.ladder.on_link_dead(TokioInstant::now(), send, &ctx.logf);
                ladder_step(st, ctx, face, jobs, step);
            }
        }
        None => {}
    }
    // ③b **快探节拍**（§3.2：在用档背靠背 / 待机档 60s / 挂起空窗立即探——
    //     由 runtime 拍内务驱动；单飞与动作链在阶梯里）。
    //     **判据 = 连接在位**（M5 §2.4-A-2 ④：快探节拍是阶梯的唯一周期触发源——
    //     旧 `live && tun` 把无 TUN 的宿主会话整条关掉；`tun` 与「有没有连接要保」
    //     无关，故去掉）。
    if st.live.is_some() {
        let in_use = in_use_now(st, ctx, &st.probe);
        if let Some((round, budget)) = st.ladder.due(TokioInstant::now(), in_use) {
            ladder_step(st, ctx, face, jobs, LadderStep::Probe { round, budget });
        }
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
        // M3 S1：服务流面计数（`quic` 段的岛侧子集，§8.2-12）
        let sc = st.streams.counters();
        s.streams_open = sc.open;
        s.streams_active = st.streams.active() as u64;
        s.streams_refused = sc.refused;
        s.stream_bytes_out = sc.bytes_out;
        s.stream_bytes_in = sc.bytes_in;
        s.stream_backpressure_events = sc.backpressure;
        // M3 S1 / §3.1-N5：本机发送面信号（M/R 判别的输入；新鲜度按已解决的快探窗现算）
        let ev = face.send_err_view(st.probe.send_err_fresh);
        s.sock_send_errs = ev.total;
        s.sock_send_errs_local = ev.local;
        s.sock_send_err_age_ms = ev.age.map(|d| d.as_millis() as u64);
        s.sock_send_err_last_errno = ev.last_errno;
        s.sock_send_err_local_fresh = ev.fresh;
        // M3 S4：快探阶梯读数（§3.1/§3.2 的观测面；e2e 的 T_recv 观测位 = `ladder_probe_ok`）
        s.ladder_probe_ok = st.ladder.probe_ok;
        s.ladder_fail_streak = st.ladder.fail_streak();
        s.ladder_jitter_streak = st.ladder.jitter_streak();
        s.ladder_action = st.ladder.last_action.to_owned();
    }
    // M6.7 临时诊断（收口删除）：逐秒落岛侧 quinn 读数 + TUN 面计数 + 四类丢弃
    if let Some(seq) = crate::exit::m67_due() {
        if let Some(live) = st.live.as_ref() {
            let (drops, counters) = {
                let s = lock_unpoison(&ctx.snapshot);
                (
                    format!(
                        "{}/{}/{}/{}",
                        s.drops.too_large,
                        s.drops.send_buffer_full,
                        s.drops.return_queue_full,
                        s.drops.unregistered
                    ),
                    format!(
                        " pkts_in={} pkts_out={} sbuf={}",
                        s.packets_in, s.packets_out, s.send_buffer_used
                    ),
                )
            };
            let extra = format!(" drops={drops}{counters}");
            (*ctx.logf)(&crate::exit::m67_line(
                "island",
                seq,
                0,
                &live.conn,
                &extra,
            ));
        }
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
                "隧道面已附加（fd={fd}, mtu={mtu}；数据面已接线：读线程 + 回程队列 {} 条 + 写线程；fd 所有权在扩展，岛从不 close）",
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
            ctx.counters.pkts_in.fetch_add(1, Ordering::Relaxed);
            match st.live.as_ref() {
                Some(live) => {
                    let note = st
                        .drop_note
                        .clone()
                        .unwrap_or_else(|| drop_note_one(ctx));
                    dataplane::send_datagram_checked(&live.conn, pkt, &note);
                    // N8①/S2-5：缓冲占用**按包**刷新——`housekeeping` 的 TICK 分支在连续流量
                    // 下不会到点（`select!` 的 `sleep(TICK)` 每轮重建 ⇒ 只在无事件的 250ms
                    // 之后才跑），黑洞期的读数不能等到那时才可见。
                    ctx.counters
                        .send_buf_used
                        .store(live.send_buffer_used(), Ordering::Relaxed);
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
            (*ctx.logf)(&msg);
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
            (*ctx.logf)(&format!("候选清单已更新（{n} 条）"));
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
                    (*ctx.logf)(&format!("本地 socket 已换绑（{from} → {to}）"));
                    let _ = reply.send(Ok(to));
                }
                Err(e) => {
                    (*ctx.logf)(&format!("换绑失败（{from} → …；{e}）"));
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
                let probe = Arc::clone(live.probe_slot());
                jobs.spawn(async move {
                    let result = client::probe(&conn, &probe, budget).await;
                    Job::Probe { reply, result }
                });
            }
        },
        // ---------- M3 S1：服务流族（设计 §1.4/§1.5/§1.6/§1.7） ----------
        //
        // **命令循环零写等待**（本切片的可判定事实，隔离门第 ⑩ 条同款断言）：
        // 下面五条臂里唯一的 await 是 `StreamOpen` 的 `open_bi`（受 OPEN_BUDGET 有界、
        // 且自记账通过后不阻塞）；`StreamWrite` 走同步的 `Streams::write`（非阻塞接纳），
        // 真正的 `write_all().await` 全在写者任务里。
        Cmd::StreamOpen { tag, reply } => {
            let Some(live) = st.live.as_ref() else {
                st.streams.note_refused(tag, StreamErr::ConnectionLost);
                let _ = reply.send(Err(StreamErr::ConnectionLost));
                return;
            };
            // 自记账配额（§1.6：额度耗尽 = 快速失败，不等 open_bi 阻塞/超时）
            if !st.streams.has_capacity() {
                st.streams.note_refused(tag, StreamErr::Busy);
                let _ = reply.send(Err(StreamErr::Busy));
                return;
            }
            match tokio::time::timeout(OPEN_BUDGET, live.conn.open_bi()).await {
                Ok(Ok((send, recv))) => {
                    let slot = st.streams.register(tag, recv);
                    let id = slot.id();
                    let streams = Arc::clone(&st.streams);
                    jobs.spawn(async move {
                        streams::writer_task(send, slot, streams).await;
                        Job::StreamDone
                    });
                    // **回执投不出去 ⇒ 就地关流**（M4 §2.2 的 H1 残余窄窗）：调用方已放弃
                    // （客户端 seam 拿不到 id ⇒ 它的 RAII 守卫也无从构造）时，这个流在岛内
                    // **没有句柄可关**；不关就永久占一个槽（`has_capacity()` = `map.len() < 62`）
                    // ——62 条之后同一连接上的全部服务流都会 `Busy`。
                    if reply.send(Ok(id)).is_err() {
                        let _ = st.streams.close(id);
                    }
                }
                Ok(Err(_)) => {
                    // 连接在开流途中死掉（对端 CONNECTION_CLOSE / 本地关闭）
                    st.streams.note_refused(tag, StreamErr::ConnectionLost);
                    let _ = reply.send(Err(StreamErr::ConnectionLost));
                }
                Err(_) => {
                    // 对端 TP 与自记账不一致（错配）⇒ 快速失败而非挂死命令循环
                    st.streams.note_refused(tag, StreamErr::Busy);
                    let _ = reply.send(Err(StreamErr::Busy));
                }
            }
        }
        Cmd::StreamWrite { id, data, reply } => {
            // **同步**：非阻塞接纳（`n` 由待发队列余量给出）⇒ 绝不 await 到写满
            let r = st.streams.write(id, data);
            // M5 §2.4-A-2 ⑤：STREAM 面需求信号（「在用档」判据的宿主会话来源）——
            // 只在**真有字节出站**时刷新（`n=0` 背压不算需求）。
            if matches!(&r, Ok(w) if w.n > 0) {
                st.last_stream_out_at = Some(std::time::Instant::now());
            }
            let _ = reply.send(r);
        }
        Cmd::StreamRead { id, reply } => match st.streams.slot_of(id) {
            Some(slot) => {
                let streams = Arc::clone(&st.streams);
                jobs.spawn(async move {
                    streams::read_task(slot, reply, streams).await;
                    Job::StreamDone
                });
            }
            None => {
                let _ = reply.send(Err(StreamErr::Closed));
            }
        },
        Cmd::StreamShutdown { id, reply } => {
            let r = st.streams.shutdown(id).map(|_tag| ());
            let _ = reply.send(r);
        }
        Cmd::StreamClose { id, reply } => {
            let r = st.streams.close(id).map(|_slot| ());
            let _ = reply.send(r);
        }
    }
}

// ---------- M3 S4：快探阶梯的执行面（§3.1 的动作顺序） ----------

/// 发送面读数（M/R 判别的输入；`SockStats` 的白名单 + 新鲜度窗）。
fn send_face(face: &Face, window: Duration) -> SendFace {
    let v = face.send_err_view(window);
    SendFace {
        fresh: v.fresh,
        errno: v.last_errno,
    }
}

/// 「在用档」判据（§3.2-1）：**有出站且新鲜**（岛侧等价面 = ①TUN 数据面 `TunCounters`
/// ②服务流出站（M5 §2.4-A-2 ⑤：宿主会话无 TUN ⇒ 用 STREAM 面补需求信号））。
/// 亮屏位在岛内不可达——偏离登记见 `tuning::probe_defaults::IN_USE_FRESH`。
fn in_use_now(st: &DriverState, ctx: &IslandCtx, tuning: &ProbeTuning) -> bool {
    let tun_fresh = st.tun.is_some()
        && ctx
            .counters
            .last_outbound_at()
            .is_some_and(|t| t.elapsed() <= tuning.in_use_fresh);
    let stream_fresh = st
        .last_stream_out_at
        .is_some_and(|t| t.elapsed() <= tuning.in_use_fresh);
    tun_fresh || stream_fresh
}

/// 执行阶梯的一步（**同步臂就地闭环、spawn 臂投任务并返回**）。
///
/// 闭环性：M（换本地 socket）是同步原语 ⇒ 其结论就地回灌（`on_migrate_result`）并
/// 继续推进（失败 ⇒ 立刻 R；成功 ⇒ 起确认探活）。R 与探活经 `JoinSet` 回口
/// （`Job::LadderConnect` / `Job::FastProbe`）——回口处再调本函数。
fn ladder_step(
    st: &mut DriverState,
    ctx: &IslandCtx,
    face: &mut Face,
    jobs: &mut JoinSet<Job>,
    mut step: LadderStep,
) {
    // **同步链步数安全网**（代码门 r18 ②-1 的配套）：正常链 ≤2 步（M/R ⇒ 确认），但
    // 「候选为空 × M/R 轮转」这类结构性异常会在同步路径上环回 ⇒ 超限即按 B 收（上报
    // 不健康交世代层），**不得**让岛线程自旋或停在单飞态。
    let mut steps: u32 = 0;
    loop {
        steps += 1;
        if steps > 8 {
            step = st.ladder.force_rebuild(steps);
        }
        match step {
            LadderStep::Idle => return,
            LadderStep::Probe { round, budget } => {
                let Some(live) = st.live.as_ref() else {
                    // 无连接（链路已死/动作间隙）：探活**不可执行** ⇒ 按该动作失败收线。
                    // **不得**回灌 `on_link_dead`（其 `inflight` 早退会把阶梯永久停在
                    // `inflight=true` 且 `pending` 不清 ⇒ `due()` 恒 None、housekeeping 又
                    // 无 live 可命中 ⇒ 岛内动作链死绝——代码门 r18 ②-1 的正是此链）。
                    let send = send_face(face, st.ladder.tuning().send_err_fresh);
                    step = st.ladder.on_confirm_unavailable(TokioInstant::now(), send, &ctx.logf);
                    continue;
                };
                let conn = live.conn.clone();
                let slot = Arc::clone(live.probe_slot());
                st.ladder.note_round(round, TokioInstant::now());
                jobs.spawn(async move {
                    let result =
                        ladder::run_round(budget, || client::probe(&conn, &slot, budget)).await;
                    Job::FastProbe { round, result }
                });
                return;
            }
            LadderStep::Migrate { why } => {
                let from = face.local();
                match face.rebind(None) {
                    Ok(to) => {
                        (*ctx.logf)(&format!(
                            "本地 socket 已换绑（{from} → {to}）—— M 动作（原因={why}）"
                        ));
                        st.rebind = Some(RebindNote {
                            from,
                            to,
                            at: TokioInstant::now(),
                        });
                        step = st
                            .ladder
                            .on_migrate_result(true, TokioInstant::now(), &ctx.logf);
                    }
                    Err(e) => {
                        (*ctx.logf)(&format!("换绑失败（{from} → …；{e}）"));
                        step = st
                            .ladder
                            .on_migrate_result(false, TokioInstant::now(), &ctx.logf);
                    }
                }
            }
            LadderStep::Reconnect {
                why,
                after_migration_unconfirmed,
            } => {
                if after_migration_unconfirmed {
                    // §3.1 末段：M 之后**一个快探预算内**无回显 ⇒ 置位（N-b 行文不变，
                    // 窗口 60s → ≤1 快探预算 = 语义登记项 S7）
                    match st.rebind.take() {
                        Some(note) => (*ctx.logf)(&format!(
                            "迁移未确认（{} → {}，{} 内无对端回包 ⇒ 回落重连/重赛跑）",
                            note.from,
                            note.to,
                            client::fmt_dur(st.ladder.tuning().fast_budget)
                        )),
                        None => (*ctx.logf)(
                            "迁移未确认（一个快探预算内无对端回包 ⇒ 回落重连/重赛跑）",
                        ),
                    }
                    lock_unpoison(&ctx.snapshot).migration_unconfirmed = true;
                    st.watch = None;
                }
                // R 的候选 = **现任端点**（§3.1「同端点、同本地 socket」）；无现任
                // （理论窗口：未采纳过连接）⇒ 回落「最近一次 SetCandidates」清单。
                let list: Vec<Candidate> = match st.ladder_cand {
                    Some(c) => vec![c],
                    None => st.cands.clone(),
                };
                if list.is_empty() {
                    // 无候选可拨（未采纳过连接且未 SetCandidates）：按 R 失败回灌 ⇒ B 门照走
                    step = st.ladder.on_reconnect_result(
                        false,
                        "无候选可拨".to_owned(),
                        TokioInstant::now(),
                        &ctx.logf,
                    );
                    continue;
                }
                face.relays().set(&list);
                let endpoint = face.endpoint();
                let ccfg = face.client_config();
                let cred = Arc::clone(face.credential());
                let budget = st.ladder.reconnect_budget();
                let logf = Arc::clone(&ctx.logf);
                jobs.spawn(async move {
                    let result = client::connect(endpoint, ccfg, cred, &list, budget, false, &logf)
                        .await
                        .map(|(est, _outcome)| est);
                    Job::LadderConnect { why, result }
                });
                return;
            }
            LadderStep::Rebuild { why, r_fails } => {
                (*ctx.logf)(&format!(
                    "快探阶梯走完 M/R（连续重连失败 {r_fails}，原因={why}）—— 上报不健康（交世代重建）"
                ));
                ctx.unhealthy(REASON_PATROL);
                return;
            }
        }
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
    let (first_of_note, line) = {
        let mut s = lock_unpoison(snapshot);
        let total = s.drops.bump(reason, n);
        // 节流口径（M6.5 批化后）：按**本条上报的首条计数**判（= 逐包上报时的口径，
        // 「首 3 + 每 100」不因一次报 N 条而漏掉首条）
        let first_of_note = total.saturating_sub(n).saturating_add(1);
        let d = s.drops;
        (
            first_of_note,
            format!(
                "超限={} 发送缓冲满={} 回程队列满={} 未登记={}",
                d.too_large, d.send_buffer_full, d.return_queue_full, d.unregistered
            ),
        )
    };
    if log_due(first_of_note) {
        let ext = ext.map(|v| format!("，字节/条数={v}")).unwrap_or_default();
        (*logf)(&format!(
            "丢弃 {line}（本次：{} ×{n} {detail}{ext}；计数行首 3 + 每 100）",
            reason.text()
        ));
    }
    let ev = lock_unpoison(on_event).clone();
    if let Some(h) = ev {
        h(IslandEvent::DatagramDropped { reason, n });
    }
}

/// 数据面丢弃上报口（**单条**形态；M6.5：在**采纳连接时构造一次**存进 `DriverState`，
/// 热路径只 `Arc::clone`，不再每包新建闭包）。
fn drop_note_one(ctx: &IslandCtx) -> DropNote {
    let snapshot = Arc::clone(&ctx.snapshot);
    let logf = Arc::clone(&ctx.logf);
    let on_event = Arc::clone(&ctx.on_event);
    Arc::new(move |reason, detail| {
        note_drop_shared(&snapshot, &logf, &on_event, reason, 1, detail, None)
    })
}

/// 数据面任务的丢弃上报口（`Arc<dyn Fn>`：常驻任务需要 `'static`——故三件共享件都克隆）。
///
/// M6.5：改成**带条数**的形态（`n`）——回程泵按批投递，丢一批要一次记 N 条（计数仍精确）。
fn drop_note_with_count(ctx: &IslandCtx) -> crate::client::dataplane::DropNoteN {
    let snapshot = Arc::clone(&ctx.snapshot);
    let logf = Arc::clone(&ctx.logf);
    let on_event = Arc::clone(&ctx.on_event);
    Arc::new(move |reason, n, detail| {
        note_drop_shared(&snapshot, &logf, &on_event, reason, n, detail, None)
    })
}

// ---------- 收工/分类小件 ----------

/// join 收口 + panic 分类（**谁记 panic 行**：预算内 = `stop_within` 的 join 分支；
/// 到点 detach = 收割线程 `hw-quic-reap` 的 join 分支——设计 §3.6-4 的分工表）。
fn join_and_classify(h: JoinHandle<()>, logf: &Logf, on_unhealthy: &OnUnhealthy) {
    if let Err(payload) = h.join() {
        let msg = panic_msg(payload.as_ref());
        (*logf)(&format!("岛线程 panic（{msg}）—— 本世代 QUIC 面已死"));
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
