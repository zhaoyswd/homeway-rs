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
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;

use crate::client::{self, Face, Live, MigrationEvent, Watch};
use crate::cmd::{
    Candidate, Cmd, DropReason, IslandErr, IslandEvent, IslandSnapshot, Logf, OnEvent, OnUnhealthy,
    RaceOutcome,
};
use crate::config::IslandConfig;
use crate::sync_util::{lock_unpoison, log_spawn_failed, ExitSignal};

/// 驱动线程名（镜像 `homeway-wg`）。
pub(crate) const ISLAND_THREAD: &str = "homeway-quic";
/// 到点收割线程名（镜像 `hw-engine-reap`）。
pub(crate) const REAP_THREAD: &str = "hw-quic-reap";
/// 命令等待节拍上限（镜像 `wgcore` 的 `POLL_CAP = 250ms`）：无命令时周期性回看 stop 位
/// （拍内务——刷新/迁移保持检测/快照同步——挂在这一拍上）。
const TICK: Duration = Duration::from_millis(250);
/// 不健康原因（判据语义取值集 `{patrol, fd, panic, stop}` 的一员）。
const REASON_PANIC: &str = "panic";
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
    /// 事件回调（`SetOnEvent` 安装；未装 = 只入快照 + 行）。
    on_event: Mutex<Option<OnEvent>>,
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
        let patrol = cfg.patrol;

        let ctx = IslandCtx {
            snapshot: Arc::clone(&snapshot),
            stop: Arc::clone(&stop),
            exit: Arc::clone(&exit),
            logf: Arc::clone(&logf),
            on_unhealthy: Mutex::new(Arc::clone(&on_unhealthy)),
            on_event: Mutex::new(None),
        };
        let handle = thread::Builder::new()
            .name(ISLAND_THREAD.into())
            .spawn(move || thread_body(rt, rx, ctx, cfg, patrol, ready_tx, seam))?;

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
            logf,
            on_unhealthy,
        })
    }

    /// 命令投递口（clone 给 TUN 读线程/世代线程；投递不阻塞）。
    pub fn tx(&self) -> IslandTx {
        self.tx.clone()
    }

    /// 状态快照（轮询；无阻塞）。
    pub fn snapshot(&self) -> IslandSnapshot {
        lock_unpoison(&self.snapshot).clone()
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
fn thread_body(
    rt: Runtime,
    rx: UnboundedReceiver<Cmd>,
    ctx: IslandCtx,
    cfg: IslandConfig,
    patrol: Duration,
    ready_tx: mpsc::Sender<io::Result<()>>,
    seam: u8,
) {
    let res = catch_unwind(AssertUnwindSafe(|| {
        run_driver(&rt, rx, &ctx, cfg, patrol, ready_tx, seam)
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
}

/// 驱动循环：命令 / 长任务回口 / 巡检拍**三源** `select!`（M1 设计 §2.1 的驱动循环形态）。
///
/// 与 `wgcore` 的 `poll(2)` 形态同义：命令到点即处置（无需回执的热路径直落），
/// 无命令则按 [`TICK] 回看 stop 位并做拍内务。
fn run_driver(
    rt: &Runtime,
    mut rx: UnboundedReceiver<Cmd>,
    ctx: &IslandCtx,
    cfg: IslandConfig,
    patrol: Duration,
    ready_tx: mpsc::Sender<io::Result<()>>,
    seam: u8,
) {
    rt.block_on(async move {
        let mut face = match Face::open(cfg) {
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
        };
        let mut jobs: JoinSet<Job> = JoinSet::new();
        loop {
            if ctx.stop.load(Ordering::SeqCst) {
                break;
            }
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Stop) => break,
                    Some(c) => handle_cmd(c, &mut st, &mut face, ctx, &mut jobs, seam).await,
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
                                    adopt(&mut st, ctx, face.local(), est);
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
                        // 任务被 abort（收工路径）：其回执口随之 drop ⇒ 调用侧归 EngineGone
                        Err(_aborted) => {}
                    }
                }
                () = tokio::time::sleep(TICK) => {}
            }
            housekeeping(&mut st, &face, ctx).await;
        }
        // 收工：长任务全 abort（JoinSet drop）→ 连接面/端点随 face drop 关闭
        drop(jobs);
        (*ctx.logf)("quic: 岛收工（连接面随端点关闭）");
    });
}

/// 采纳胜者（§2.2 的现任裁决：**旧的已登记连接显式关闭**；N-a 行只在首个连接时打一次）。
fn adopt(st: &mut DriverState, ctx: &IslandCtx, local: SocketAddrV4, est: client::Established) {
    let patrol = st.patrol;
    let adopted = Live::new(est, patrol);
    if let Some(old) = st.live.replace(adopted) {
        old.conn.close(quinn::VarInt::from_u32(0), b"replaced by newer race");
        (*ctx.logf)("quic: 替换旧连接（旧连接已 CONNECTION_CLOSE）");
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
}

/// 拍内务（无命令时每 [`TICK`] 一次）：连接死 → 刷新 → 迁移保持检测 → 快照同步。
async fn housekeeping(st: &mut DriverState, face: &Face, ctx: &IslandCtx) {
    let patrol = st.patrol;
    // ① 连接死（对端关闭/空闲回收）：清面 + 记行。不健康分类与恢复阶梯 = S2b 接线
    //    （设计 §2.5：QUIC 连接死归既有 `patrol` 分类；此处只留可观测面）。
    if st.live.as_ref().is_some_and(|l| !l.alive()) {
        (*ctx.logf)("quic: 连接已断 —— 等上层重连/重赛跑（阶梯接线 = S2b）");
        st.live = None;
        st.watch = None;
    }
    // ② 刷新到点（C15'；写失败 = 连接已断）
    if let Some(live) = st.live.as_mut() {
        if !live.refresh_if_due(face.credential(), &ctx.logf).await {
            (*ctx.logf)("quic: 注册刷新写失败 —— 判连接已断");
            st.live = None;
            st.watch = None;
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
        }
        None => {}
    }
    // ④ 快照同步（via/ep/rtt/mtu/candidates 一律由驱动态派生）
    {
        let mut s = lock_unpoison(&ctx.snapshot);
        s.candidates = st.cands.len();
        s.mirrors = st.mirrors;
        match st.live.as_ref() {
            Some(l) => {
                s.via = Some(l.via);
                s.ep = Some(l.ep);
                s.rtt_ms = l.rtt_ms();
                s.mtu = l.mds();
                s.current_mtu = l.current_mtu();
            }
            None => {
                s.via = None;
                s.ep = None;
                s.rtt_ms = 0;
                s.mtu = None;
                s.current_mtu = 0;
            }
        }
    }
}

/// 单条命令处置（岛线程内；带 await 的只有赛跑/探活的任务投出与换绑）。
async fn handle_cmd(
    cmd: Cmd,
    st: &mut DriverState,
    face: &mut Face,
    ctx: &IslandCtx,
    jobs: &mut JoinSet<Job>,
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
            } else {
                // M1 S2a：只登记（读 fd/mtu 入行；TUN 读线程/写线程 = S2-4）
                (*ctx.logf)(&format!(
                    "quic: 隧道面已附加（fd={fd}, mtu={mtu}；fd 所有权在扩展，岛从不 close）"
                ));
                let _ = reply.send(Ok(()));
            }
        }
        Cmd::TunPacket(pkt) => {
            // 本切片只计数；**发送路径 = S2-4**。无已登记连接时按 §6.4 归 `未登记`
            // （「登记前丢弃」）——有连接但尚未接线发送的窗口由 S2-4 落地，不留静默面。
            let n = pkt.len();
            lock_unpoison(&ctx.snapshot).packets_in += 1;
            if st.live.is_none() {
                note_drop(ctx, DropReason::Unregistered, 1, "无已登记连接（发送面未接线）", Some(n as u64));
            }
        }
        // 循环侧处置（幂等；重入无害）
        Cmd::Stop => {}
        Cmd::SetOnUnhealthy { h } => {
            *lock_unpoison(&ctx.on_unhealthy) = h;
        }
        Cmd::SetOnEvent { h } => {
            *lock_unpoison(&ctx.on_event) = Some(h);
        }
        Cmd::SetCandidates { cands } => {
            let n = cands.len();
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
    let (total, line) = {
        let mut s = lock_unpoison(&ctx.snapshot);
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
        (*ctx.logf)(&format!(
            "quic: 丢弃 {line}（本次：{} ×{n} {detail}{ext}；计数行首 3 + 每 100）",
            reason.text()
        ));
    }
    let ev = lock_unpoison(&ctx.on_event).clone();
    if let Some(h) = ev {
        h(IslandEvent::DatagramDropped { reason, n });
    }
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
