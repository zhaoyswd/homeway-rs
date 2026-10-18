//! 数据面的 **TUN fd 侧**（M1 设计 §2.4/§6.4）：专用 std 读线程 + 有界回程队列 + 专用
//! std 写线程。形态镜像 `homeway-core::wgcore` 的 `tun_read_loop` / `write_fd_all`
//! （M0 设计 §3.6-6：**阻塞 syscall 只许留在专用 std 线程**，岛内只做非阻塞）。
//!
//! 与 `wgcore` 的两处**有意差异**（都写在明面上）：
//!
//! 1. **零 `std::thread::sleep`**（隔离门层 3 禁阻塞 sleep）：`wgcore` 在「读返 0 且
//!    poll 立返可读」的空读形态用一次地板睡眠兜底；本模块改用 **`poll` 一份小片**
//!    （[`DEAD_CONFIRM_DELAY`]）——但该小片对**恒可读** fd 是电平触发立即返回、不
//!    构成等待（2026-10-15 事故形态），防自旋由 [`StuckReadGuard`] 双阈值判死兜底。
//! 2. **本模块不做 fd 判死的分类**：判死（`POLLHUP/POLLERR/POLLNVAL`、`read` 返错、
//!    写超预算）一律**记行 + 投 `Cmd::TunFdDead`**（镜像 `wgcore` 的同名命令），由岛
//!    线程按既有取值集打 `fd` 分类（`mark_unhealthy_if_current(gen,"fd")` 的岛侧落点）。
//!
//! fd 所有权：**在扩展**（`Cmd::TunAttach` 的语义），本模块**从不 close**；线程退出靠
//! ①岛收工（命令通道断 ⇒ `send` 返 `Err` ⇒ 读线程自退，M0 设计 §3.6-2④）或 ②fd 失效
//! （读/写返错）。两个线程**分离**（不 join）——与 `wgcore` 同形，收工预算不押在阻塞读上。

use std::io;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::cmd::{CarrierFlag, Cmd, Logf, OnPacketDemand};
use crate::driver::IslandTx;

/// TUN 读线程名（与 `wgcore` 的 `homeway-tun-read` 同形）。
pub(crate) const READ_THREAD: &str = "homeway-tun-read";
/// 回程写线程名（`wgcore` 的写面在引擎线程里做，岛侧独立成线程 ⇒ 新名）。
pub(crate) const WRITE_THREAD: &str = "homeway-tun-write";

/// 回程队列上限（设计 §6.4 矩阵：**2048 条**≈2.6MB @1280B；满 ⇒ 丢新 + 计数）。
///
/// **M6.7 实测（2048 → 4096 被否）**：抬到 4096 后真机 `回程队列满` 逐轮仍 400–500 次
/// （只把「首次溢出」推迟；溢出是**稳态**的——写线程 28k 包/s 上限 < 外承载突发 30–45k 包/s），
/// 且 T2 中位无同向改善 ⇒ **撤回**（不付 +2.6MB 最坏内存账换无收益；见 M6.7 记录 §B）。
pub(crate) const RETURN_QUEUE_MAX: usize = 2048;

/// TUN 读线程 → 岛的**在途上限**（设计 §6.4 矩阵：**有界通道 4096 条**≈5MB @1280B；
/// 满 ⇒ **丢新 + 计数**（归 `未登记`——「投递不进」面；TUN 读线程**不阻塞**）。
///
/// 落法（**形态偏离，语义等价**，见 commit 偏离说明）：岛的命令通道是 M0 的 unbounded
/// `Cmd` 通道（控制面命令也要走它、必须不阻塞），把整条通道改成有界会波及控制面；这里
/// 在**生产侧**用一枚原子计数器把 `Cmd::TunPacket` 的在途条数卡在 4096（单生产者 +
/// 岛侧消费即减 ⇒ 计数精确、无竞态），满则丢新。**等价于一个容量 4096 的有界队列**。
pub(crate) const CMD_INFLIGHT_MAX: usize = 4096;

/// 丢弃上报的**批量**粒度：读线程每攒够这么多条 `丢新` 才投一条 `DatagramDropped`
/// （前提见 [`CMD_INFLIGHT_MAX`] 的等价性说明）；计数仍精确，尾批（≤63 条）在岛收工后
/// 无人可报（丢失，登记在案）。
const DROP_REPORT_BATCH: u64 = 64;

/// poll 片长（镜像 `wgcore::POLL_SLICE`；stop/退出判定的粒度就是它）。
const POLL_SLICE: Duration = Duration::from_millis(500);
/// 空读判死的确认片（镜像 `wgcore::DEAD_CONFIRM_DELAY` 的量级；用 poll 而非 sleep）。
const DEAD_CONFIRM_DELAY: Duration = Duration::from_millis(50);
/// 僵死读（poll 恒报可读而 read 恒返空/EAGAIN）的判死阈值：拍数与墙钟**双条件**
/// （2026-10-15 事故：恒可读 fd 下 poll 是电平触发立即返回，防自旋的「兜底片」
/// 不睡——仅拍数会被微秒级紧环瞬间凑满而误判；墙钟条件同时把该形态的自旋
/// 时长钉死在秒级上界内）。
const STUCK_READ_MAX: u32 = 100;
const STUCK_DEAD_AFTER: Duration = Duration::from_secs(1);
// 口径：紧环形态下墙钟主导（判死前 ≤1s 满核、一次性）；「慢形态」（每拍 poll 真等
// 到片末）下拍数主导（≤ STUCK_READ_MAX×POLL_SLICE ≈ 50s 判死，期间睡在 poll 里
// 不烧 CPU）——两形态都满足合取阈值，别把 50s 判死时延当回归。真源 = tier 仓
// openspec/changes/event-driven-heal（wg-native-transport「TUN fd 僵死形态的防自旋」
// + design D4）。
/// TUN 写总预算（镜像 `wgcore::write_fd_all` 的 5s——超预算按超时收 ⇒ 记行 + 卸面）。
const WRITE_BUDGET: Duration = Duration::from_secs(5);

/// TUN 面的共享计数（**语义镜像** `wgcore::TunCounters`：需求信号 + 字节对表；
/// `write_pkts` 是岛侧新增的回程包计数——快照 `packets_out` 的源）。
#[derive(Debug, Default)]
pub(crate) struct TunCounters {
    /// 读（应用出站方向）累计字节。
    pub(crate) read_bytes: AtomicU64,
    /// 写（应用入站方向）累计字节；同时是回程写线程的活体证据。
    pub(crate) write_bytes: AtomicU64,
    /// 写（应用入站方向）累计**包**数（快照 `packets_out`）。
    pub(crate) write_pkts: AtomicU64,
    /// App 出站包计数（demand-driven-recovery D1：巡检拍「取走清零」消费）。
    pub(crate) out_pkts: AtomicI64,
    /// 最近出站包时刻（unix nano；0 = 本世代从未有过应用出站）。
    pub(crate) last_outbound_ns: AtomicI64,
    /// 同一时刻的单调相对读数（自进程起点的 ns；下推器 D4 的时基——评审核对
    /// `wgcore` 的 r2-H1 口径：单调面与 unix 面分离）。
    pub(crate) last_outbound_mono_ns: AtomicI64,
    /// `Cmd::TunPacket` 在途条数（**有界通道的等价实现**；见 [`CMD_INFLIGHT_MAX`]）。
    inflight: AtomicU64,
    /// 岛侧收到的 `TunPacket` 总数（**快照 `packets_in` 的原子直读面**；M6.5：从
    /// 每包一次快照互斥锁改成原子计数——热路径不再为读数上锁）。
    pub(crate) pkts_in: AtomicU64,
    /// datagram 发送缓冲占用（快照 `send_buffer_used` 的原子面；同上，避免每包上锁）。
    pub(crate) send_buf_used: AtomicU64,
}

impl TunCounters {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 写面累计包数（快照 `packets_out` 的读面）。
    pub(crate) fn write_pkts(&self) -> u64 {
        self.write_pkts.load(Ordering::Relaxed)
    }

    /// 取走并清零 App 出站包计数（镜像 `wgcore::Client::swap_out_pkts` 的语义）。
    pub(crate) fn swap_out_pkts(&self) -> i64 {
        self.out_pkts.swap(0, Ordering::Relaxed)
    }

    /// 占一个在途名额（满 ⇒ `false` = **丢新**；调用方计数不投递）。
    pub(crate) fn try_begin_send(&self) -> bool {
        let mut cur = self.inflight.load(Ordering::Relaxed);
        loop {
            if cur as usize >= CMD_INFLIGHT_MAX {
                return false;
            }
            match self
                .inflight
                .compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return true,
                Err(now) => cur = now,
            }
        }
    }

    /// 交还在途名额（岛侧消费掉一条 `TunPacket`，或读线程投递失败）。
    pub(crate) fn done_send(&self) {
        let mut cur = self.inflight.load(Ordering::Relaxed);
        while cur > 0 {
            match self.inflight.compare_exchange_weak(
                cur,
                cur - 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(now) => cur = now,
            }
        }
    }

    /// 最近出站包时刻（**单调面**换算；`None` = 本世代从未有过应用出站）——
    /// 镜像 `wgcore::Client::last_outbound_at`（评审 r2-H1：不许用 unix 面换算 `Instant`）。
    pub(crate) fn last_outbound_at(&self) -> Option<Instant> {
        let ns = self.last_outbound_mono_ns.load(Ordering::Relaxed);
        if ns <= 0 {
            return None;
        }
        process_mono_start().checked_add(Duration::from_nanos(ns as u64))
    }
}

/// 进程单调起点（`last_outbound_mono_ns` 的基准；懒初始化——与 `wgcore` 同款）。
fn process_mono_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// 回程队列的**批次**（M6.5：一批最多 [`RETURN_BATCH_MAX`] 包——一次唤醒投一批，
/// 写线程一次取一批；上限口径仍按**包**计，见 [`ReturnPath::inflight`]）。
pub(crate) type ReturnBatch = Vec<Box<[u8]>>;

/// 单批最大包数（M6.5 批化：批量大小取 16——足够把「每包一次唤醒 + 一次队列投递」
/// 摊薄一个量级，又不至于把回程时延拖过 1 个 MTU 的发送时间）。
pub(crate) const RETURN_BATCH_MAX: usize = 16;

/// 回程通道（岛 → TUN）：有界队列的**生产者面**（消费者 = 写线程）。
///
/// 生产者 = 岛线程内的回程泵（[`crate::client::dataplane::pump_return`]）；`try_push_batch`
/// **非阻塞**（绝不把阻塞传染进岛 runtime）。**有界口径按包**：`inflight` 计在途包数
/// （上限仍 = [`RETURN_QUEUE_MAX`]），到顶丢新 + 计数（与 M1 §6.4 的预算口径一致）。
pub(crate) struct ReturnPath {
    tx: SyncSender<ReturnBatch>,
    /// 在途包数（岛侧加、写线程写完后减；单生产者 + 单消费者 ⇒ 计数精确）。
    inflight: Arc<std::sync::atomic::AtomicUsize>,
    /// 写线程存活位（退出前置 false）：**`Gone` 优先于包级 `Full`**——消费者已死时
    /// 包级计数可能残留（写线程在写中退出 ⇒ 整批未减），不能把「没人收」报成「队列满」。
    writer_alive: Arc<std::sync::atomic::AtomicBool>,
}

/// 投递结果（**两种失败必须可分**：满 = 队列在但来不及排空；Gone = 消费者已死）。
///
/// 为什么分开（代码门 r13 的 A3）：写线程因 fd 失效退出后（`write_loop` 投 `Cmd::TunFdDead`），
/// 队列仍在、`try_push` 恒失败——若与「队列满」同归一类，日志会写成
/// `回程队列满（上限 2048 条）`，与事实（没有消费者）不符，误导排障；且泵会一直空转
/// 拷贝 + 丢弃。`Gone` 让泵**收口并如实记因**。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PushOutcome {
    Pushed,
    /// 队列满（丢新；调用方计 `回程队列满`）。
    Full,
    /// 写线程已退（fd 失效/收工）：泵应收口，不再逐包计 `回程队列满`。
    Gone,
}

impl ReturnPath {
    /// 起写线程并接上队列（fd 的所有权在扩展；本模块不 close）。
    pub(crate) fn new(
        fd: crate::sysio::RawIo,
        tx: IslandTx,
        counters: Arc<TunCounters>,
        logf: Logf,
    ) -> io::Result<Arc<Self>> {
        // 队列容量按**批**给（每批 ≤ RETURN_BATCH_MAX 包；包级闸由 inflight 卡）
        let (queue_tx, rx) = sync_channel::<ReturnBatch>(RETURN_QUEUE_MAX / RETURN_BATCH_MAX);
        let inflight = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writer_alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let inflight_w = Arc::clone(&inflight);
        let alive_w = Arc::clone(&writer_alive);
        std::thread::Builder::new()
            .name(WRITE_THREAD.into())
            .spawn(move || write_loop(fd, rx, tx, counters, logf, inflight_w, alive_w))?;
        Ok(Arc::new(Self {
            tx: queue_tx,
            inflight,
            writer_alive,
        }))
    }

    /// 投一批（**包级**有界：在途包数超 [`RETURN_QUEUE_MAX`] ⇒ `Full`；消费者已死 ⇒ `Gone`）。
    ///
    /// 不阻塞：批队列满同样归 `Full`（调用方丢 + 计数；TCP 会重传）。
    pub(crate) fn try_push_batch(&self, batch: ReturnBatch) -> PushOutcome {
        if batch.is_empty() {
            return PushOutcome::Pushed;
        }
        use std::sync::atomic::Ordering as O;
        let n = batch.len();
        // 消费者已死 ⇒ 先归 `Gone`（A3：不得把「没人收」误报成「队列满」）
        if !self.writer_alive.load(O::Relaxed) {
            return PushOutcome::Gone;
        }
        // 包级闸：**先记账后投递**（单生产者 + 写线程只减 ⇒ 记账面无并发加法）。
        // 2026-10-10 审查 中-1：原「先 try_send 后 fetch_add」存在窄窗——岛线程在两步
        // 之间被抢占时，写线程可能抢先收走整批并 `release`，`saturating_sub` 把尚未
        // 记账的名额钳成 0→0 空转；生产者恢复后照常 `fetch_add` ⇒ 每次泄漏 ≤1 批的
        // 幽灵名额，累积到 [`RETURN_QUEUE_MAX`] 即**永久**误报「回程队列满」（下行
        // 黑洞，TUN 附加生命周期内不可自愈）。先加后发 + 失败回退后，名额恒 ≥
        // 队内+在写包数，钳位只剩防御意义。
        self.inflight.fetch_add(n, O::Relaxed);
        if self.inflight.load(O::Relaxed) > RETURN_QUEUE_MAX {
            release(&self.inflight, n);
            return PushOutcome::Full;
        }
        match self.tx.try_send(batch) {
            Ok(()) => PushOutcome::Pushed,
            Err(TrySendError::Full(_)) => {
                release(&self.inflight, n);
                PushOutcome::Full
            }
            // 写线程已退（fd 失效/收工）：**单独一类**（丢弃语义同「丢」，但归因不同——见 PushOutcome）
            Err(TrySendError::Disconnected(_)) => {
                release(&self.inflight, n);
                PushOutcome::Gone
            }
        }
    }
}

/// 起 TUN 读线程（**分离**——不等 join；退出面见模块头）。
///
/// `carrier`/`on_demand`（reconnect-oneshot D2）：读线程见到出站包且承载位 false 时
/// 就地触发统一重连入口——**只允许原子操作 + 起线程，禁日志禁锁禁阻塞**（契约严于
/// 岛回调）。`None` = 无承载面（测试/宿主形态——读面零触发）。
pub(crate) fn spawn_reader(
    fd: crate::sysio::RawIo,
    tx: IslandTx,
    counters: Arc<TunCounters>,
    logf: Logf,
    carrier: Option<Arc<CarrierFlag>>,
    on_demand: Option<OnPacketDemand>,
) -> io::Result<()> {
    let blocking = set_blocking_read(fd, &logf);
    (*logf)(&format!(
        "TUN 读面形态：{}（M6.5 逐段成本整改：阻塞读消「read+poll」系统调用对）",
        if blocking {
            "阻塞读（每包一次 read）"
        } else {
            "非阻塞读 + poll（回退形态）"
        }
    ));
    std::thread::Builder::new()
        .name(READ_THREAD.into())
        .spawn(move || read_loop(fd, tx, counters, logf, carrier, on_demand))?;
    Ok(())
}

/// M6.5：把 fd 置成**阻塞读**（清 `O_NONBLOCK`）。
///
/// 真机逐段表（M6.5）显示读线程的 CPU 几乎全在「非阻塞 read 返 EAGAIN + poll 等」的
/// 系统调用对上（read≈39µs/次、poll≈96µs/次；每上行包 1.84 次 read + 0.85 次 poll）——
/// 阻塞读把等待收回内核等待队列，**每包只花一次 read**。
///
/// 读循环仍保留 EAGAIN → poll 的回退支（fd 天生非阻塞/标志不可清 ⇒ 行为与改前一致，
/// 零回归）。副作用（**登记在案**）：写线程 `write_fd_all` 在 tun 队列满时改为**阻塞在
/// write 内**（原为 `WouldBlock` → `POLLOUT` 有界等待）；fd 死亡仍以写错误
/// （EBADF/EPIPE/EIO）面呈现。
fn set_blocking_read(fd: crate::sysio::RawIo, logf: &Logf) -> bool {
    // WIN 批：非阻塞位切换经 sysio（unix = fcntl 原语义；windows = FIONBIO——该面
    // 在 windows 无运行期来源，仅编译面保通）。
    match crate::sysio::set_nonblocking(fd, false) {
        Ok(()) => true,
        Err(e) => {
            (*logf)(&format!(
                "TUN fd 读阻塞化失败（{e}）—— 保持非阻塞读 + poll 形态"
            ));
            false
        }
    }
}

/// 单 fd poll 等待（片长 [`POLL_SLICE`]；EINTR 内部重试；到 `deadline` 返 `TimedOut`；
/// 片到无事件 = `Ready::default()`）。形态镜像 `wgcore::poll_fd`。
fn poll_fd(fd: crate::sysio::RawIo, events: i16, deadline: Instant) -> io::Result<Ready> {
    use crate::sysio::{PollEnt, POLLERR, POLLHUP, POLLIN, POLLNVAL, POLLOUT};
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "tun fd 等待超预算"));
        }
        let mut pfd = PollEnt {
            raw: fd,
            events,
            revents: 0,
        };
        let slice = POLL_SLICE.min(deadline.saturating_duration_since(now));
        let ms = slice.as_millis().min(i32::MAX as u128) as i32;
        // WIN 批：poll 经 sysio（unix = libc::poll 原语义；windows = WSAPoll）。
        let r = match crate::sysio::poll(std::slice::from_mut(&mut pfd), ms) {
            Ok(r) => r,
            Err(e) => {
                if e.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(e);
            }
        };
        if r == 0 {
            return Ok(Ready::default()); // 片到无事件：超时，不判死
        }
        let rev = pfd.revents;
        let mut ready = Ready {
            readable: rev & POLLIN != 0,
            writable: rev & POLLOUT != 0,
            hup: rev & POLLHUP != 0,
            err: rev & POLLERR != 0,
            nval: rev & POLLNVAL != 0,
        };
        if !ready.readable && !ready.writable && !ready.hup && !ready.err && !ready.nval {
            ready.err = true; // 只认出未知位 ⇒ 保守异常（同 wgcore）
        }
        return Ok(ready);
    }
}

/// poll 结果（镜像 `wgcore::Ready` 的字段面）。
#[derive(Debug, Default)]
struct Ready {
    readable: bool,
    writable: bool,
    hup: bool,
    err: bool,
    nval: bool,
}

/// TUN 全量写（部分写回补 + POLLOUT 等待**有总预算**——镜像 `wgcore::write_fd_all`；
/// 预算检查写死在循环顶，不能只靠 `poll_fd` 的内部期限）。
fn write_fd_all(fd: crate::sysio::RawIo, mut buf: &[u8]) -> io::Result<()> {
    let deadline = Instant::now() + WRITE_BUDGET;
    while !buf.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(io::ErrorKind::TimedOut, "tun fd 写等待超预算"));
        }
        // WIN 批：裸写经 sysio（unix = write(2) 原语义；windows = send）。
        let n = match crate::sysio::write(fd, buf) {
            Ok(n) => n as isize,
            Err(e) => match e.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    let r = poll_fd(fd, crate::sysio::POLLOUT, deadline)?;
                    if r.hup || r.err || r.nval {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "tun fd 已失效（POLLHUP/POLLERR/POLLNVAL）——立即出线",
                        ));
                    }
                    continue; // writable 或片到 ⇒ 回循环顶复检期限
                }
                _ => return Err(e),
            },
        };
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// 僵死读记账（防自旋，2026-10-15 事故修复）：「poll 立返可读而本轮未取得数据」的
/// 连拍计数 + 首拍时刻。双阈值（[`STUCK_READ_MAX`] 拍 ∧ [`STUCK_DEAD_AFTER`] 墙钟）
/// 同时满足才判死——poll 对恒可读 fd 是电平触发立即返回，「先转真等待」不可实现
/// （模块零 `std::thread::sleep` 铁律），唯一有界处置是按 fd 失效收工；取得数据或
/// 片睡到底（健康节拍）即断链清零。
#[derive(Debug, Default)]
struct StuckReadGuard {
    reads: u32,
    since: Option<Instant>,
}

impl StuckReadGuard {
    /// 记一拍「可读而不可得」；返回 `true` = 双阈值命中（应判死）。
    fn note_readable_empty(&mut self) -> bool {
        self.note_readable_empty_at(Instant::now())
    }

    /// 时钟可注入的判位点（单测不真睡 [`STUCK_DEAD_AFTER`]）。
    fn note_readable_empty_at(&mut self, now: Instant) -> bool {
        self.reads = self.reads.saturating_add(1);
        self.since.get_or_insert(now);
        self.reads >= STUCK_READ_MAX
            && now.duration_since(self.since.expect("上方已置位")) >= STUCK_DEAD_AFTER
    }

    /// 取得数据或 poll 片睡到底 ⇒ 僵死链断。
    fn note_progress(&mut self) {
        self.reads = 0;
        self.since = None;
    }

    /// 判死记行用的摘要（拍数 + 实际历时）。
    fn describe(&self) -> String {
        let dur = self.since.map(|t| t.elapsed()).unwrap_or(Duration::ZERO);
        format!("{} 拍/{:?}", self.reads, dur)
    }

    /// 判死记行（两条支路共用；归因文案与「对端不可达（patrol）」可区分）。
    fn dead_msg(&self) -> String {
        format!(
            "tun fd 僵死：poll 恒可读而 read 恒空（{}，双阈值判死——标记隧道不健康）",
            self.describe()
        )
    }
}

/// poll 结果是否报了任何被识别的事件位（纯函数——单测钉死「静默片不计数」的
/// 高危回归：bug 形态 = 把健康空闲的静默片计拍 ⇒ 空闲 fd 约 50s 被误判死）。
/// `writable` 不参与（本模块只等 POLLIN）。
fn poll_reports_event(r: &Ready) -> bool {
    r.readable || r.hup || r.err || r.nval
}

/// 读循环（应用出站方向）：裸 read → 计数 → `Cmd::TunPacket` 投岛。
///
/// - **读面需求触发（reconnect-oneshot D2）**：成功 read 之后、packet-info 剥离与
///   在途闸/队满判定**之前**——承载位 false 时无论该包最终是否被丢都必须触发
///   （队满丢弃路径不得吞掉触发，恰是 2026-10-17 事故形态；r3-R2）。先自增载位假包
///   计数、再调回调（回调 = facade 的 `Weak<GenRun>` 升级 + 统一入口，全程原子）；
/// - OHOS 的 VPN fd 是**非阻塞**的（`wgcore` 真机实证；`spawn_reader` 会先尝试
///   `set_blocking_read`，不可清时落「非阻塞读 + poll」回退形态——OHOS 即此）⇒
///   EAGAIN 走 poll(POLLIN, 一片)；
/// - `n == 0`（EOF/空读）：`hup|err|nval` ⇒ 判死（记行 + `TunFdDead` + 退出），否则
///   用一小片 poll 兜底（替代 `wgcore` 的地板睡眠）后继续；
/// - **僵死形态**（poll 恒报可读而 read 恒返空/EAGAIN——2026-10-15 事故：恒定约 7%
///   单核零日志自旋）⇒ [`StuckReadGuard`] 双阈值（拍数 ∧ 墙钟）判死，同 `TunFdDead`
///   收工——poll 对恒可读 fd 电平触发立即返回，等待不可实现（零 sleep 铁律）；
/// - 投包失败（岛已收工/通道断）⇒ **自退**（M0 设计 §3.6-2④；不重试、不挂死）；
/// - fd 读错 ⇒ 记行 + `TunFdDead`（岛侧打 `fd` 分类）+ 退出。
fn read_loop(
    fd: crate::sysio::RawIo,
    tx: IslandTx,
    counters: Arc<TunCounters>,
    logf: Logf,
    carrier: Option<Arc<CarrierFlag>>,
    on_demand: Option<OnPacketDemand>,
) {
    let mut buf = vec![0u8; 65535];
    let mut pending_drops: u64 = 0;
    let mut stuck = StuckReadGuard::default();
    loop {
        // WIN 批：裸读经 sysio（unix = read(2) 原语义；windows = recv——错误经
        // WSAGetLastError 归一到 io::Error，sysio 单源）。四态直排（评审 L5：修前
        // 用 -11 假 EAGAIN 哨兵过桥——语义分叉面收进 match 本身）。
        let n: usize = match crate::sysio::read(fd, &mut buf) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // WouldBlock 回退支（fd 非阻塞/标志不可清时才会走到）：等 POLLIN 一片。
            // 三分处置（镜像 `wgcore`）：片到（静默）= 继续；poll 出错 = 判死。
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let stuck_dead = match poll_fd(fd, crate::sysio::POLLIN, Instant::now() + POLL_SLICE) {
                    // 报了任何被识别的事件位（含 POLLIN|POLLHUP 同置——与 n==0 支口径
                    // 一致）⇒ 立返可读而本轮无数据，记一拍僵死（真有数据会在下一轮
                    // read 取得并清零）。
                    Ok(r) if poll_reports_event(&r) => stuck.note_readable_empty(),
                    // 静默片（`poll_fd` 的「片到无事件」= `Ok(Ready::default())` 全
                    // false——`Err(TimedOut)` 只在进循环即超期时出现，本调用点不可达）
                    // = 健康停泊节拍（EAGAIN 空闲主路径），僵死链断。
                    Ok(_) => {
                        stuck.note_progress();
                        false
                    }
                    Err(pe) if pe.kind() == io::ErrorKind::TimedOut => {
                        stuck.note_progress();
                        false
                    }
                    Err(pe) => {
                        let msg = format!("tun fd poll 失败：{pe}（标记隧道不健康）");
                        (logf)(&msg);
                        let _ = tx.send(Cmd::TunFdDead { msg });
                        return;
                    }
                };
                if stuck_dead {
                    let msg = stuck.dead_msg();
                    (logf)(&msg);
                    let _ = tx.send(Cmd::TunFdDead { msg });
                    return;
                }
                continue;
            }
            Err(e) => {
                let msg = format!("tun fd 读取失败：{e}（标记隧道不健康）");
                (logf)(&msg);
                let _ = tx.send(Cmd::TunFdDead { msg });
                return;
            }
        };
        if n == 0 {
            // EOF/空读：判死**只看 hup||err||nval**（darwin 上 EOF 恒带 POLLIN），
            // 且判死前确认一片（poll 本身即等待——不用 sleep）。
            let first = poll_fd(fd, crate::sysio::POLLIN, Instant::now() + POLL_SLICE);
            let (dead, readable) = match &first {
                Ok(r) => (r.hup || r.err || r.nval, r.readable),
                Err(pe) if pe.kind() == io::ErrorKind::TimedOut => (false, false),
                Err(_) => (true, false),
            };
            let dead = if dead {
                match poll_fd(fd, crate::sysio::POLLIN, Instant::now() + DEAD_CONFIRM_DELAY) {
                    Ok(r) => r.hup || r.err || r.nval,
                    Err(pe) if pe.kind() == io::ErrorKind::TimedOut => false,
                    Err(_) => true, // poll 本身失败（EBADF/EIO…）⇒ 判死
                }
            } else {
                false
            };
            if dead {
                let msg = "tun fd 已失效（POLLHUP/POLLERR/POLLNVAL，确认一片后仍成立）";
                (logf)(&format!("{msg}（标记隧道不健康）"));
                let _ = tx.send(Cmd::TunFdDead { msg: msg.into() });
                return;
            }
            // 「read 返 0 且 poll 立返可读」的空读形态：兜底片对恒可读 fd 同样立返
            // （2026-10-15 事故路径——原「防热自旋」兜底在此形态是空操作）⇒ 记一拍
            // 僵死；不可读（片睡到底）则僵死链断。
            if readable {
                let _ = poll_fd(fd, crate::sysio::POLLIN, Instant::now() + DEAD_CONFIRM_DELAY);
                if stuck.note_readable_empty() {
                    let msg = stuck.dead_msg();
                    (logf)(&msg);
                    let _ = tx.send(Cmd::TunFdDead { msg });
                    return;
                }
            } else {
                stuck.note_progress();
            }
            continue;
        }
        stuck.note_progress(); // 取得数据：僵死链断
        // ---- 读面需求触发（reconnect-oneshot D2：成功 read 之后、PI 剥离与在途闸/
        // 队满判定之前——队满丢弃路径不得吞掉触发）----
        if let Some(c) = &carrier {
            if !c.alive() {
                c.bump_demand_pkt(); // 载位假包 P（调入口前自增——r4-R16）
                if let Some(cb) = &on_demand {
                    cb(); // 契约：只允许原子操作 + 起线程（禁日志禁锁禁阻塞）
                }
            }
        }
        let mut n = n;
        // packet-info 头自动探测（镜像 `wgcore` 的 r2-L4：4 字节 PI + 合法 IP 版本号才剥）
        let looks_like_pi = n >= 5
            && ((buf[2] == 0x08 && buf[3] == 0x00 && buf[4] >> 4 == 4)
                || (buf[2] == 0x86 && buf[3] == 0xdd && buf[4] >> 4 == 6));
        if n >= 5 && buf[0] == 0 && buf[1] == 0 && looks_like_pi {
            buf.copy_within(4..n, 0);
            n -= 4;
        }
        let pkt: Box<[u8]> = buf[..n].to_vec().into_boxed_slice();
        counters.read_bytes.fetch_add(n as u64, Ordering::Relaxed);
        // 需求信号（demand-driven-recovery D1：只计 App 源；巡检拍取走清零）
        counters.out_pkts.fetch_add(1, Ordering::Relaxed);
        counters.last_outbound_ns.store(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as i64)
                .unwrap_or(0),
            Ordering::Relaxed,
        );
        counters.last_outbound_mono_ns.store(
            process_mono_start().elapsed().as_nanos() as i64,
            Ordering::Relaxed,
        );
        // 在途闸（设计 §6.4 的「有界通道 4096 条」等价实现）：满 ⇒ **丢新 + 计数**，
        // 读线程**不阻塞**（丢的包由端到端 TCP 重传吸收）。
        if !counters.try_begin_send() {
            pending_drops += 1;
            if pending_drops >= DROP_REPORT_BATCH {
                let _ = tx.send(Cmd::DatagramDropped {
                    reason: crate::cmd::DropReason::Unregistered,
                    n: pending_drops,
                });
                pending_drops = 0;
            }
            continue;
        }
        if tx.send(Cmd::TunPacket(pkt)).is_err() {
            // 岛已收工（通道断）——读线程自退（不重试、不挂死）
            counters.done_send();
            (logf)("homeway-tun-read: 投递失败（岛已收工）—— 读线程退出");
            return;
        }
        if pending_drops > 0 {
            let _ = tx.send(Cmd::DatagramDropped {
                reason: crate::cmd::DropReason::Unregistered,
                n: pending_drops,
            });
            pending_drops = 0;
        }
    }
}

/// 写循环（应用入站方向）：有界队列 → `write_fd_all`（含期限）。
///
/// 退出：通道断（岛收工/回程泵随连接结束而 drop）/ fd 写失败（记行 + `TunFdDead`
/// ⇒ 岛侧打 `fd` 分类 + 卸面）。
fn write_loop(
    fd: crate::sysio::RawIo,
    rx: Receiver<ReturnBatch>,
    tx: IslandTx,
    counters: Arc<TunCounters>,
    logf: Logf,
    inflight: Arc<std::sync::atomic::AtomicUsize>,
    writer_alive: Arc<std::sync::atomic::AtomicBool>,
) {
    loop {
        let batch = match rx.recv() {
            Ok(p) => p,
            Err(_) => {
                writer_alive.store(false, Ordering::Relaxed);
                return; // 生产者全 drop（岛收工）⇒ 写线程退出
            }
        };
        // M6.5 批化：一次唤醒写一整批；批内逐包 write（TUN 语义：一次 write = 一包，不能合并）
        for pkt in &batch {
            match write_fd_all(fd, pkt) {
                Ok(()) => {
                    counters.write_bytes.fetch_add(pkt.len() as u64, Ordering::Relaxed);
                    counters.write_pkts.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    let msg = format!("tun fd 写入失败：{e}（标记隧道不健康）");
                    (logf)(&msg);
                    let _ = tx.send(Cmd::TunFdDead { msg });
                    // 退出前：整批名额就地归还 + 置死活位（`Gone` 优先于包级 `Full`）
                    release(&inflight, batch.len());
                    writer_alive.store(false, Ordering::Relaxed);
                    return;
                }
            }
        }
        // 交还在途名额（包级；整批一次性减）
        release(&inflight, batch.len());
    }
}

/// 归还在途名额（`n` 包；下限 0；单生产者 + 单消费者 ⇒ 竞态面只有「写线程减、岛侧加」）。
fn release(inflight: &std::sync::atomic::AtomicUsize, n: usize) {
    use std::sync::atomic::Ordering as O;
    let mut cur = inflight.load(O::Relaxed);
    loop {
        let next = cur.saturating_sub(n);
        match inflight.compare_exchange_weak(cur, next, O::Relaxed, O::Relaxed) {
            Ok(_) => break,
            Err(now) => {
                if now == 0 {
                    break;
                }
                cur = now;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::atomic::AtomicUsize;

    use super::*;
    use crate::cmd::OnPacketDemand;

    /// 测试用投递口（无接收者——写线程的 `TunFdDead` 投递恒失败，与「岛已收工」同面；
    /// `#[cfg(test)]` 缝定义在 `driver.rs`，本文件不命名异步栈类型）。
    fn cmd_tx() -> IslandTx {
        IslandTx::dead_for_test()
    }

    /// **判据（防自旋，2026-10-15 事故）**：僵死读的判死是**双阈值合取**——拍数到
    /// 量而墙钟未到 ⇒ 不判（瞬时抖动不误杀）；两界都过 ⇒ 判死（自旋时长钉死在
    /// 秒级）。时钟经 `note_readable_empty_at` 注入，测试不真睡 [`STUCK_DEAD_AFTER`]。
    #[test]
    fn stuck_guard_requires_both_count_and_wallclock() {
        let mut g = StuckReadGuard::default();
        let t0 = Instant::now();
        // 微秒级紧环凑满拍数（每拍 8ms 假时钟推进 ⇒ 120 拍仅 0.96s，墙钟未满）：
        // 拍数到量而墙钟未到 ⇒ 全程不判死（>= 语义下避开 100×10ms=1s 的恰界）。
        for i in 0..(STUCK_READ_MAX + 20) {
            let now = t0 + Duration::from_millis(u64::from(i) * 8);
            assert!(
                !g.note_readable_empty_at(now),
                "第 {} 拍：墙钟 {:?} 未满 {:?}，不得判死",
                i + 1,
                now.duration_since(t0),
                STUCK_DEAD_AFTER
            );
        }
        // 两界都过（>100 拍且假时钟 >1s）⇒ 判死。
        let t_dead = t0 + STUCK_DEAD_AFTER + Duration::from_millis(50);
        assert!(
            g.note_readable_empty_at(t_dead),
            "拍数与墙钟双阈值命中后必须判死"
        );
    }

    /// **判据**：取得数据或片睡到底 ⇒ 僵死链断清零——健康包流/空闲停泊不累积。
    #[test]
    fn stuck_guard_resets_on_progress() {
        let mut g = StuckReadGuard::default();
        let t0 = Instant::now();
        // 攒到接近阈值，然后一次健康节拍清零；再从零攒并跨过双阈值 ⇒ 判死成立
        //（清零是真的清零，不是「压线续命」）。
        for i in 0..(STUCK_READ_MAX - 1) {
            g.note_readable_empty_at(t0 + Duration::from_millis(u64::from(i) * 20));
        }
        g.note_progress();
        assert_eq!(g.reads, 0, "清零后拍数归零");
        assert!(g.since.is_none(), "清零后起始时刻复位");
        for i in 0..(STUCK_READ_MAX + 5) {
            let now = t0 + Duration::from_secs(2) + Duration::from_millis(u64::from(i) * 20);
            // 清零后重新起算：墙钟恒已越过 STUCK_DEAD_AFTER ⇒ 第 100 拍起判死成立。
            let dead = g.note_readable_empty_at(now);
            assert_eq!(dead, i + 1 >= STUCK_READ_MAX, "第 {} 拍的判死态", i + 1);
        }
    }

    /// **判据（空闲停泊不误伤）**：健康 fd（peer 在位、无数据、非阻塞 ⇒ EAGAIN →
    /// poll 片睡到底）下读循环**不退出**——墙钟窗口内线程仍在（片睡节拍 ≥500ms，
    /// 无自旋）；对端关闭（EOF+HUP）⇒ 既有判死路径在秒级内退出。
    #[test]
    fn read_loop_parks_on_idle_fd_and_exits_on_peer_close() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        a.set_nonblocking(true).expect("置非阻塞（走 EAGAIN→poll 路径）");
        let counters = TunCounters::new();
        let logf = Arc::new(|_s: &str| {}) as Logf;
        let h = std::thread::spawn(move || {
            read_loop(a.as_raw_fd(), cmd_tx(), counters, logf, None, None);
        });
        // 隔离门层 3 禁 std::thread::sleep（全文件面——含测试）：用有界让权等待
        // 走完同样的墙钟窗（测试线程自旋 700ms 可接受；读环停泊无外部可观测信号，
        // 只能按时间窗断言）。
        let park_deadline = Instant::now() + Duration::from_millis(700);
        while Instant::now() < park_deadline {
            std::thread::yield_now();
        }
        assert!(!h.is_finished(), "空闲 fd 上读循环必须停泊存活（EAGAIN→静默片停泊）");
        drop(b); // 对端关闭 ⇒ EOF + POLLHUP ⇒ 既有判死路径
        // 有界等待：判死路径失效时本测试必须失败而不是挂死（评审中-2）。
        let deadline = Instant::now() + Duration::from_secs(3);
        while !h.is_finished() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(h.is_finished(), "对端关闭后读循环应在秒级内判死退出");
        let _ = h.join();
    }

    /// **判据（评审高-1 的回归钉）**：健康空闲的「片到无事件」（`poll_fd` 返
    /// `Ok(Ready::default())` 全 false）**不是**僵死拍——bug 形态把静默片计拍
    /// （1 拍/500ms）⇒ 空闲 fd 约 50s 被 `TunFdDead` 误判死（评审实测 100 拍/
    /// 49.5s）。报了任何被识别事件位（readable/hup/err/nval）才算一拍。
    #[test]
    fn silent_poll_slice_is_not_a_beat() {
        assert!(
            !poll_reports_event(&Ready::default()),
            "静默片是健康停泊节拍，不得计僵死拍"
        );
        for r in [
            Ready { readable: true, ..Ready::default() },
            Ready { hup: true, ..Ready::default() },
            Ready { err: true, ..Ready::default() },
            Ready { nval: true, ..Ready::default() },
        ] {
            assert!(poll_reports_event(&r), "事件位（{:?}）必须计拍", r);
        }
        // writable 不参与（本模块只等 POLLIN）。
        assert!(!poll_reports_event(&Ready { writable: true, ..Ready::default() }));
    }

    /// 写面：有界队列 → `write_fd_all`（真 fd：socketpair 的另一端），包尺寸/内容逐字节。
    #[test]
    fn write_path_round_trips_through_bounded_queue() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (logf, _rx) = (Arc::new(|_s: &str| {}) as Logf, ());
        let counters = TunCounters::new();
        let ret =
            ReturnPath::new(a.as_raw_fd(), cmd_tx(), Arc::clone(&counters), logf).expect("写线程可起");
        assert_eq!(ret.try_push_batch(vec![vec![1, 2, 3, 4].into_boxed_slice()]), PushOutcome::Pushed);
        assert_eq!(ret.try_push_batch(vec![vec![9u8; 64].into_boxed_slice()]), PushOutcome::Pushed);
        let mut got = [0u8; 4];
        (&b).read_exact(&mut got).expect("读到第一包");
        assert_eq!(got, [1, 2, 3, 4]);
        let mut got2 = [0u8; 64];
        (&b).read_exact(&mut got2).expect("读到第二包");
        assert_eq!(got2, [9u8; 64]);
        // 计数（有界轮询上界：只判上界，flake 口径②）
        let deadline = Instant::now() + Duration::from_secs(2);
        while counters.write_pkts() < 2 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert_eq!(counters.write_pkts(), 2, "回程包计数（快照 packets_out 的源）");
        assert_eq!(counters.write_bytes.load(Ordering::Relaxed), 68);
    }

    /// **判据（设计 §6.4 矩阵：TUN 读线程 → 岛的「有界通道 4096 条」）**：在途闸满 ⇒
    /// `try_begin_send` 恒 `false`（读线程据此**丢新 + 计数**，不阻塞）；消费/失败交还
    /// 名额后恢复（计数精确 —— 单生产者 + 岛侧消费即还）。
    #[test]
    fn tun_inflight_is_bounded_and_reopens_after_release() {
        let c = TunCounters::new();
        for i in 0..CMD_INFLIGHT_MAX {
            assert!(c.try_begin_send(), "第 {} 个名额应可占", i + 1);
        }
        assert!(
            !c.try_begin_send(),
            "第 {} 个必须被拒（上限 {CMD_INFLIGHT_MAX}）",
            CMD_INFLIGHT_MAX + 1
        );
        c.done_send();
        assert!(c.try_begin_send(), "交还一个名额后应可再占");
        // 交还到 0 后不越界（`done_send` 幂等保护：0 不再减）
        for _ in 0..(CMD_INFLIGHT_MAX + 8) {
            c.done_send();
        }
        assert_eq!(c.inflight.load(Ordering::Relaxed), 0, "在途计数不得下溢");
        assert!(c.try_begin_send(), "归零后仍可用");
    }

    /// 队列满：`try_push_batch` 非阻塞返 `Full`（岛侧据此计 `回程队列满`）。
    ///
    /// **M6.5 口径（批化后）**：上限按**包**卡（[`RETURN_QUEUE_MAX`]），批粒度容差 ≤ 1 批；
    /// 构造同旧版（socketpair 读端不读 + 灌满内核缓冲 ⇒ 写线程卡在第一批写里 ⇒ 队列填满）。
    #[test]
    fn return_queue_full_is_non_blocking() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        a.set_nonblocking(true).expect("置非阻塞（灌缓冲用）");
        // 灌满内核发送缓冲（方向与岛的写线程一致：都写进 b 的接收缓冲）
        let filler = vec![0u8; 256 * 1024];
        let mut written = 0usize;
        while written < filler.len() {
            match (&a).write(&filler[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(_) => break, // WouldBlock = 缓冲已满
            }
        }
        assert!(written > 0, "至少写进一些（缓冲非零）");
        let mut got = [0u8; 1];
        assert!(
            b.set_nonblocking(true).is_ok(),
            "读端也置非阻塞（本用例不读，只求不阻塞）"
        );
        let _ = (&b).read(&mut got); // 保持 b 打开（fd 所有权在调用侧）
        let (logf, _rx) = (Arc::new(|_s: &str| {}) as Logf, ());
        let counters = TunCounters::new();
        let ret = ReturnPath::new(a.as_raw_fd(), cmd_tx(), counters, logf).expect("写线程可起");
        // 逐批投（每批满 RETURN_BATCH_MAX）：**包级**上限 = RETURN_QUEUE_MAX，容差 ≤ 1 批
        let batch = || -> Vec<Box<[u8]>> { vec![vec![0u8; 8].into_boxed_slice(); RETURN_BATCH_MAX] };
        let mut pushed = 0usize;
        for _ in 0..(RETURN_QUEUE_MAX / RETURN_BATCH_MAX + 8) {
            if ret.try_push_batch(batch()) == PushOutcome::Pushed {
                pushed += RETURN_BATCH_MAX;
            }
        }
        assert!(
            pushed >= RETURN_QUEUE_MAX,
            "队列应被填满（RETURN_QUEUE_MAX={RETURN_QUEUE_MAX}，实际入队 {pushed}）"
        );
        assert!(
            pushed <= RETURN_QUEUE_MAX + RETURN_BATCH_MAX,
            "队列不得越过包级上限 + 1 批（RETURN_QUEUE_MAX={RETURN_QUEUE_MAX}，实际入队 {pushed}）"
        );
        // 上限面：此后 `try_push_batch` 必须**非阻塞返 `Full`**（岛侧据此计 `回程队列满`）
        assert_eq!(
            ret.try_push_batch(batch()),
            PushOutcome::Full,
            "包级上限已到 ⇒ 必须归 `Full`（写线程仍活）"
        );
    }

    /// **判据（代码门 r13 的 A3）**：消费者（写线程）已退 ⇒ `try_push` 归 `Gone`（**不是**
    /// `Full`）——岛侧据此收口回程泵并如实记因，不把「没有消费者」误报成「队列满」。
    ///
    /// 构造：socketpair 读端先 drop ⇒ 写线程首次 `write_fd_all` 得 EPIPE ⇒ `write_loop`
    /// 记行 + 投 `TunFdDead` + `return`（`rx` 随之 drop）⇒ 队列进入 `Disconnected`。
    #[test]
    fn return_push_reports_gone_when_consumer_exits() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        drop(b); // 读端消失：写线程下一次写必然失败
        let (logf, _rx) = (Arc::new(|_s: &str| {}) as Logf, ());
        let counters = TunCounters::new();
        let ret = ReturnPath::new(a.as_raw_fd(), cmd_tx(), counters, logf).expect("写线程可起");
        // 首包进队（写线程随后取走并失败退出）；之后必须观测到 `Gone`（有界轮询上界）
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut outcome = ret.try_push_batch(vec![vec![0u8; 8].into_boxed_slice()]);
        while outcome != PushOutcome::Gone && Instant::now() < deadline {
            std::thread::yield_now();
            outcome = ret.try_push_batch(vec![vec![0u8; 8].into_boxed_slice()]);
        }
        assert_eq!(
            outcome,
            PushOutcome::Gone,
            "写线程已退 ⇒ 必须归 `Gone`（不得与「队列满」混同）"
        );
    }

    // ==========================================================================
    // reconnect-oneshot 1.2：读面需求触发（socketpair 直测——成功 read 之后、PI 剥离
    // 与在途闸/队满判定之前）
    // ==========================================================================

    /// 触发计数面（回调 + 载位假包 P）。
    fn demand_face() -> (Arc<CarrierFlag>, Arc<AtomicUsize>, OnPacketDemand) {
        let carrier = CarrierFlag::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = Arc::clone(&hits);
        let cb: OnPacketDemand = Arc::new(move || {
            h2.fetch_add(1, Ordering::SeqCst);
        });
        (carrier, hits, cb)
    }

    /// **判据（1.2）**：无承载包（carrier=true）⇒ 回调 0 次；健康突发（在途额满丢弃
    /// 形态）⇒ 仍 0 次（判定只认承载位，不认队列丢弃）。
    #[test]
    fn no_trigger_while_carrier_alive_even_under_burst() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (carrier, _hits, cb) = demand_face();
        carrier.mark_alive();
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = Arc::clone(&hits);
        let cb2: OnPacketDemand = Arc::new(move || {
            h2.fetch_add(1, Ordering::SeqCst);
        });
        // 灌满**读线程用的**在途闸（4096）——队满丢弃形态（死通道不发包，reader 存活）
        let counters = TunCounters::new();
        for _ in 0..CMD_INFLIGHT_MAX {
            assert!(counters.try_begin_send());
        }
        let logf = Arc::new(|_s: &str| {}) as Logf;
        let fd = a.as_raw_fd();
        let carrier2 = Arc::clone(&carrier);
        let h = std::thread::spawn(move || {
            read_loop(fd, cmd_tx(), counters, logf, Some(carrier2), Some(cb2));
        });
        let pkt = [0x45u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2];
        for _ in 0..100 {
            let _ = (&b).write_all(&pkt); // socketpair：写 b ⇒ a 端可读
        }
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(hits.load(Ordering::SeqCst), 0, "承载在位 ⇒ 零回调（含队满丢弃）");
        assert_eq!(carrier.demand_pkts(), 0, "载位假包 P 不增");
        // 收口：关闭对端让读循环退出
        drop(b);
        drop(a);
        let _ = h.join();
        let _ = cb;
    }

    /// **判据（1.2 / r3-R2 事故形态）**：承载 false + 队满 ⇒ **仍回调**（触发先于
    /// 在途闸/队满判定——队满丢弃路径不得吞掉触发）。
    #[test]
    fn carrier_false_triggers_even_when_inflight_gate_full() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (carrier, hits, cb) = demand_face();
        // carrier 保持 false（新装配态）——灌满**读线程用的**在途闸（4096）模拟队满
        let counters = TunCounters::new();
        for _ in 0..CMD_INFLIGHT_MAX {
            assert!(counters.try_begin_send());
        }
        let logf = Arc::new(|_s: &str| {}) as Logf;
        let fd = a.as_raw_fd();
        let carrier2 = Arc::clone(&carrier);
        let h = std::thread::spawn(move || {
            read_loop(fd, cmd_tx(), counters, logf, Some(carrier2), Some(cb));
        });
        let pkt = [0x45u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2];
        for _ in 0..10 {
            let _ = (&b).write_all(&pkt); // socketpair：写 b ⇒ a 端可读
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while hits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(hits.load(Ordering::SeqCst) > 0, "承载 false + 队满 ⇒ 仍触发（每包）");
        assert!(carrier.demand_pkts() > 0, "载位假包 P 计数（调入口前自增）");
        drop(b);
        drop(a);
        let _ = h.join();
    }

    /// **判据（1.2）**：早于 attach 的包不丢触发（回调先于读线程装好——SetOnPacketDemand
    /// 在 TunAttach 前发送的 FIFO 保证面；等价注入：包先入 socket 缓冲、读循环后起）。
    #[test]
    fn packets_queued_before_reader_start_still_trigger() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (carrier, hits, cb) = demand_face();
        // 先写包（读线程未起——「早于 attach」的等价形态）
        let pkt = [0x45u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2];
        (&b).write_all(&pkt).expect("先写一包（socketpair：写 b ⇒ a 端可读）");
        let counters = TunCounters::new();
        let logf = Arc::new(|_s: &str| {}) as Logf;
        let fd = a.as_raw_fd();
        let carrier2 = Arc::clone(&carrier);
        let h = std::thread::spawn(move || {
            read_loop(fd, cmd_tx(), counters, logf, Some(carrier2), Some(cb));
        });
        let deadline = Instant::now() + Duration::from_secs(2);
        while hits.load(Ordering::SeqCst) == 0 && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(hits.load(Ordering::SeqCst) > 0, "早于读线程的包仍触发");
        drop(b);
        drop(a);
        let _ = h.join();
    }

    /// **判据（1.2 回调契约）**：在飞期间 10k 包只计数、读路径有界完成（回调内 sleep
    /// 判负——生产回调只做原子操作 + 起线程；本用例以时间上界钉「不因回调阻塞」）。
    #[test]
    fn ten_k_packets_with_stub_callback_complete_bounded() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let (carrier, hits, cb) = demand_face();
        // 灌满在途闸：包走「计数丢弃」形态（死通道不发包——reader 存活吃完全量）
        let counters = TunCounters::new();
        for _ in 0..CMD_INFLIGHT_MAX {
            assert!(counters.try_begin_send());
        }
        let logf = Arc::new(|_s: &str| {}) as Logf;
        let fd = a.as_raw_fd();
        let carrier2 = Arc::clone(&carrier);
        let h = std::thread::spawn(move || {
            read_loop(fd, cmd_tx(), counters, logf, Some(carrier2), Some(cb));
        });
        let pkt = [0x45u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2];
        let t0 = Instant::now();
        let writer = std::thread::spawn(move || {
            for _ in 0..10_000 {
                let _ = (&b).write_all(&pkt); // socketpair：写 b ⇒ a 端可读（缓冲小于
                // 200KB ⇒ writer 与 reader 流水线推进）
            }
            b
        });
        let b = writer.join().expect("writer 收口");
        // 对端关闭 ⇒ EOF——reader 排空缓冲后经判死路径退出（join 即「全量处理完」）
        drop(b);
        h.join().expect("reader 收口");
        // socketpair 是**流**语义（多次 write 会并读合并）——精确包数不可断言；钉三件：
        // ①全量字节被排空（join 完成 = EOF 前 200KB 读尽）；②每次 read 都触发且 P
        // 与之相等；③全程有界（回调只做原子操作——读路径不因触发阻塞）。
        assert!(hits.load(Ordering::SeqCst) >= 1, "触发发生过");
        assert_eq!(
            carrier.demand_pkts(),
            hits.load(Ordering::SeqCst) as u64,
            "P 与 read 次数一致（每次 read 都判定触发）"
        );
        assert!(
            hits.load(Ordering::SeqCst) < 10_000,
            "流合并形态：read 次数少于 write 次数（每次 read 触发恰一次——不重复）"
        );
        // 评审 r1 P1-12 整改：耗时上界收紧（30s → 10s——原子-only 回调的读路径在
        // socketpair 流水线下应为秒级；30s 缓冲过大、抓不住半阻塞退化）。
        assert!(
            t0.elapsed() < Duration::from_secs(10),
            "读路径有界完成（实测 {:?}）",
            t0.elapsed()
        );
        drop(a);
    }

    /// **判据（1.2 负例注入，评审 r1 P1-12 整改）**：回调内 sleep 在读路径耗时上
    /// **可观测**——「回调内阻塞判负」的等价面：生产回调若违背「只做原子操作 +
    /// 起线程」契约，读循环耗时随之增长。socketpair 是**流**语义（快写会并读合并）
    /// ⇒ 逐包 sleep 观测不到；本用例以「writer 与 reader 逐包握手」（写一包等
    /// 回调计数 +1 再写下一包）消除合并，20 包 × 10ms ⇒ ≥150ms 下界钉「回调确实
    /// 在读路径上逐次执行、无掩蔽」；上界同设防挂死。
    #[test]
    fn blocking_callback_slows_read_path_per_read() {
        let (a, b) = UnixStream::pair().expect("socketpair");
        let carrier = CarrierFlag::new();
        let hits = Arc::new(AtomicUsize::new(0));
        let h2 = Arc::clone(&hits);
        let cb: OnPacketDemand = Arc::new(move || {
            std::thread::sleep(Duration::from_millis(10)); // 违约回调（负例注入）
            h2.fetch_add(1, Ordering::SeqCst);
        });
        let counters = TunCounters::new();
        for _ in 0..CMD_INFLIGHT_MAX {
            assert!(counters.try_begin_send());
        }
        let logf = Arc::new(|_s: &str| {}) as Logf;
        let fd = a.as_raw_fd();
        let h = std::thread::spawn(move || {
            read_loop(fd, cmd_tx(), counters, logf, Some(carrier), Some(cb));
        });
        let pkt = [0x45u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 10, 0, 0, 1, 10, 0, 0, 2];
        let t0 = Instant::now();
        let hits_w = Arc::clone(&hits);
        let writer = std::thread::spawn(move || {
            let mut seen = 0usize;
            for _ in 0..20 {
                let _ = (&b).write_all(&pkt);
                // 逐包握手：等本包的回调发生后再写下一包（消除流合并——每次 read
                // 恰一包、阻塞回调的耗时逐次累加进读路径）
                let deadline = Instant::now() + Duration::from_secs(2);
                while hits_w.load(Ordering::SeqCst) <= seen && Instant::now() < deadline {
                    std::thread::yield_now();
                }
                seen = hits_w.load(Ordering::SeqCst);
            }
            b
        });
        let b = writer.join().expect("writer 收口");
        drop(b); // EOF ⇒ reader 排空后退出
        h.join().expect("reader 收口");
        assert_eq!(hits.load(Ordering::SeqCst), 20, "20 包各触发恰一次");
        assert!(
            t0.elapsed() >= Duration::from_millis(150),
            "20 读 × 10ms 阻塞回调 ⇒ 读路径 ≥150ms（实测 {:?}，短于此 = 回调耗时被掩蔽）",
            t0.elapsed()
        );
        assert!(
            t0.elapsed() < Duration::from_secs(5),
            "仍属有界（实测 {:?}）",
            t0.elapsed()
        );
        drop(a);
    }
}
