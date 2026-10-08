//! 岛宿主：专用线程 + `current_thread` runtime + 命令循环 + 收工/收割。
//!
//! **本文件是岛内唯一允许出现异步栈名字的文件**（隔离门层 3 断言；公面文件零命中）。
//!
//! 与今日 `wgcore` 的同构性（设计 §3.2）：1 枚专用线程（`homeway-quic` ⟷ `homeway-wg`）、
//! 命令通道投递不阻塞、每命令一条回执、状态快照轮询、`Stop` + 有界 join + 到点收割线程。
//! **唯一机制偏离**：唤醒原语从「self-pipe + `poll(2)`」换成「运行时通道自带的 waker」——
//! 净减的只有唤醒 fd 一族不变量（不是「零收割期资源」：到点 detach 后岛线程照样持
//! runtime + 连接端点，残余见设计 §8.1）。
//!
//! 纪律：
//! - 岛内**只做非阻塞 IO**；阻塞面（TUN 读）留在调用侧的专用 std 线程（M1 起）；
//! - 岛内**零阻塞 `sleep`**（层 3 门禁）：收工的有界等待走 [`ExitSignal`] 的条件变量，
//!   到点即返回，不轮询、不 `sleep`；
//! - panic 面（设计 §3.6）：线程体套 `catch_unwind` ⇒ 就地记行 + 不健康回调 `"panic"`
//!   ⇒ 置退出位 + `resume_unwind`（让 join 侧能观测到 panic，承 §3.6-3 的兜底面）；
//!   **预算内收工**由 `stop_within` 的 join 分支记 panic 行、**到点 detach** 由收割线程记。

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Runtime;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

use crate::cmd::{Cmd, IslandErr, IslandSnapshot, Logf, OnUnhealthy};
use crate::sync_util::{lock_unpoison, log_spawn_failed};

/// 驱动线程名（镜像 `homeway-wg`）。
pub(crate) const ISLAND_THREAD: &str = "homeway-quic";
/// 到点收割线程名（镜像 `hw-engine-reap`）。
pub(crate) const REAP_THREAD: &str = "hw-quic-reap";
/// 命令等待节拍上限（镜像 `wgcore` 的 `POLL_CAP = 250ms`）：无命令时周期性回看 stop 位。
const TICK: Duration = Duration::from_millis(250);
/// 不健康原因（判据语义取值集 `{patrol, fd, panic, stop}` 的一员）。
const REASON_PANIC: &str = "panic";
/// 无注入（**生产路径恒取此值**；注入缝只在 `#[cfg(test)]` 的 `seams` 面里活）。
const NO_SEAM: u8 = 0;

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
    on_unhealthy: OnUnhealthy,
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
    /// `on_unhealthy` = 岛侧不健康回调（**岛线程内**执行；只允许内存操作/通道投递）。
    pub fn start(logf: Logf, on_unhealthy: OnUnhealthy) -> std::io::Result<Island> {
        Self::start_inner(logf, on_unhealthy, NO_SEAM)
    }

    fn start_inner(logf: Logf, on_unhealthy: OnUnhealthy, seam: u8) -> std::io::Result<Island> {
        // runtime 在**起线程前**建：建不出来就别起（失败即报错，不留"起了但死的岛"）。
        // `current_thread` = 单线程结构不变量（`rt-multi-thread` feature 不启用）；
        // `enable_all()` 开时间驱动，IO 驱动由异步栈的 runtime 后端 feature 统一带入。
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (tx, rx) = mpsc::unbounded_channel::<Cmd>();
        let snapshot = Arc::new(Mutex::new(IslandSnapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let exit = Arc::new(ExitSignal::new());

        let ctx = IslandCtx {
            snapshot: Arc::clone(&snapshot),
            stop: Arc::clone(&stop),
            exit: Arc::clone(&exit),
            logf: Arc::clone(&logf),
            on_unhealthy: Arc::clone(&on_unhealthy),
        };
        let handle = thread::Builder::new()
            .name(ISLAND_THREAD.into())
            .spawn(move || thread_body(rt, rx, ctx, seam))?;

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
fn thread_body(rt: Runtime, rx: UnboundedReceiver<Cmd>, ctx: IslandCtx, seam: u8) {
    let res = catch_unwind(AssertUnwindSafe(|| run_driver(&rt, rx, &ctx, seam)));
    if let Err(payload) = res {
        let msg = panic_msg(payload.as_ref());
        (ctx.logf)(&format!(
            "quic: 岛内 panic（{msg}）—— 判不健康（panic），岛线程退出（不复用该线程）"
        ));
        (ctx.on_unhealthy)(REASON_PANIC);
        ctx.exit.mark_exited();
        std::panic::resume_unwind(payload);
    }
    ctx.exit.mark_exited();
}

/// 驱动循环：命令 / 定时两源 `select!`（M1 起加第三源 = 连接 IO 的可等待面）。
///
/// 与 `wgcore` 的 `poll(2)` 形态同义：命令到点即处置（无需回执的热路径直落），
/// 无命令则按 [`TICK`] 回看 stop 位。
fn run_driver(rt: &Runtime, mut rx: UnboundedReceiver<Cmd>, ctx: &IslandCtx, seam: u8) {
    rt.block_on(async move {
        loop {
            if ctx.stop.load(Ordering::SeqCst) {
                break;
            }
            tokio::select! {
                cmd = rx.recv() => match cmd {
                    Some(Cmd::Stop) => break,
                    Some(c) => handle_cmd(c, &ctx.snapshot, &ctx.logf, seam),
                    // 所有投递口掉光（实践里非主退出路径——同步面的岛句柄持 sender；
                    // 主路径是 `Cmd::Stop`／stop 位，见设计 §3.2）
                    None => break,
                },
                () = tokio::time::sleep(TICK) => {}
            }
        }
    });
}

/// 单条命令处置（岛线程内、**非阻塞**：只做内存操作与通道投递）。
fn handle_cmd(cmd: Cmd, snapshot: &Arc<Mutex<IslandSnapshot>>, logf: &Logf, seam: u8) {
    let _ = (&seam, &logf); // 非 test 构建下注入缝无用途（`#[cfg(test)]` 面）
    #[cfg(test)]
    seams::apply(seam, logf);
    match cmd {
        Cmd::TunAttach { fd, mtu, reply } => {
            let already = {
                let mut s = lock_unpoison(snapshot);
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
                // M0：只登记（读 fd/mtu 入行——M1 起此处才是真 attach：注册 fd + 起读线程）
                (*logf)(&format!(
                    "quic: 隧道面已附加（fd={fd}, mtu={mtu}；fd 所有权在扩展，岛从不 close）"
                ));
                let _ = reply.send(Ok(()));
            }
        }
        Cmd::TunPacket(_pkt) => {
            // M0：包只计数（无引擎面）。M1 起 = DATAGRAM 出站（`Box<[u8]>` 交运行时零拷贝）。
            let mut s = lock_unpoison(snapshot);
            s.packets_in += 1;
        }
        // 循环侧处置（幂等；重入无害）
        Cmd::Stop => {}
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
fn panic_msg(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "非字符串 panic 载荷".to_string()
    }
}

/// 岛线程退出信号：幂等置位 + 有界等待（**不轮询、不 `sleep`**——层 3 门禁）。
struct ExitSignal {
    gate: Mutex<bool>,
    cv: Condvar,
}

impl ExitSignal {
    fn new() -> Self {
        Self {
            gate: Mutex::new(false),
            cv: Condvar::new(),
        }
    }

    /// 岛线程退出时置位并唤醒等待者（正常退出与 panic 路径都走——panic 路径在
    /// `resume_unwind` **之前**调用，故 join 侧不会等满预算）。
    fn mark_exited(&self) {
        *lock_unpoison(&self.gate) = true;
        self.cv.notify_all();
    }

    fn is_exited(&self) -> bool {
        *lock_unpoison(&self.gate)
    }

    /// 有界等待：期限内退出返回 `true`；到点返回 `false`（调用侧转 detach + 交收割线程）。
    fn wait_exit(&self, deadline: Instant) -> bool {
        let mut g = lock_unpoison(&self.gate);
        while !*g {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (g2, _to) = self
                .cv
                .wait_timeout(g, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            g = g2;
        }
        true
    }
}

// ---------- 测试缝（`#[cfg(test)]` 门控：release/cdylib 构建里完全消失 ⇒ 零 dead_code） ----------

/// 测试缝起岛入口（`#[cfg(test)]` ⇒ 生产构建里不存在；模式见 `seams`）。
#[cfg(test)]
impl Island {
    pub(crate) fn start_with_seam(
        logf: Logf,
        on_unhealthy: OnUnhealthy,
        seam: u8,
    ) -> std::io::Result<Island> {
        Self::start_inner(logf, on_unhealthy, seam)
    }
}

/// 依赖面**活体证据**的运行时侧（设计 §3.7 用例 8）：在 `current_thread` runtime 上下文里
/// 建一枚回环客户端端点、读回内核分配的端口后丢弃——**不建连接、不做身份**。
///
/// 放在本文件而不是 `tests.rs`：异步栈名字只允许出现在本文件（隔离门层 3）。
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
