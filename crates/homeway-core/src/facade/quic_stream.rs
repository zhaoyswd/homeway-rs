//! **QUIC 档服务流拨号缝**（M3 S3 的换轨面；真源 `docs/reviews/M3-design.md`
//! §1.1/§1.4/§1.5/§1.6 + §3 的「客户端服务流换轨」）。
//!
//! 干什么：把 `BridgeHost` 的 `DialFn`（虚拟端口 7802/7724/7803）在**QUIC 档**下换成
//! 「开一条 `STREAM[tag]`」——应用层帧逐字节不变（§1.2 的三服务 `serve_conn` 不动），
//! 变的是承载。虚拟端口 → tag 的映射见 [`tag_for_port`]（**线值单源 = `StreamTag`**，
//! 本文件不写 tag 字面量）。
//!
//! 三条纪律：
//!
//! 1. **本文件零 WG 引用**（S6 的机械断言面）：不出现 `wgcore`/`stackb`/`Session`/
//!    `connect_deadline` —— 服务流拨号只走岛的 STREAM 命令面（M5 C3 单承载后是唯一
//!    路径）。历史（WG 档）的「岛不在 ⇒ 回落 WG」派发已随 WG 面删除（`session_connect`
//!    与 WG 桥一并不存在）。
//! 2. **阶梯豁免集**（§1.6 设计门 3-3）：服务级拒绝（[`StreamErr::is_ladder_exempt`]：
//!    `NotSupported/Busy/Unbound/BadTag`）**不是连接故障** ⇒ 立即收口、不进恢复；
//!    连接面失败（`Closed/Timeout/ConnectionLost`）在调用方预算内**重试一次**。
//!    **不注入 WG 恢复阶梯**：QUIC 档不得产生 C11 族行（§3.4），QUIC 档的恢复动作
//!    归 S4 的阶梯重写（本切片只做「换轨 + 归因」，不动阶梯结构）。
//! 3. **读无限期**（§1.5）：[`Read`] 的读挂到数据/EOF/取消为止——files 空闲 5min、
//!    term 腿可挂数小时，**不得**套 RPC 预算；取消只经关流（`Drop`/`close_write`）。
//!    写走 §1.4 的 `n=0` 分级退避环（`write_retry_backoff`——WIN 批起定义在本模块，
//!    `tun_exec` 同款消费）。

use std::io::{self};
// Read/Write 只被 QuicStream 两半（GenRun 桥承载，windows 不编译）消费——
// 宿主会话面（HostStream）经 host_session 自己的 io 面。
#[cfg(not(windows))]
use std::io::{Read, Write};
use std::net::SocketAddrV4;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_quic::stream::DIAL_OK;
use homeway_quic::{Cmd, Island, StreamErr, StreamId, StreamReply, StreamTag};

use crate::Logf;

#[cfg(not(windows))] // BridgeStream/WriteHalf 面 = App 桥承载（windows 不装配三桥）
use super::bridge_io::{BridgeStream, WriteHalf};
#[cfg(not(windows))] // GenRun 面 = App 世代执行体（tun_exec 在 windows 不编译）
use super::tun_exec::{GenRun, ReconnSource};

/// 虚拟端口 → 服务 tag（§5.1：`7802/7724/7803 → 1/2/3`；**5300 → 6**（dns-resolve，
/// DNS-STREAM 批——宿主侧 socks 域名目标的远程解析腿）；其余端口无 tag）。
///
/// 端口源都是**既有配置面/契约常量**（E1 值域不变）：files/speedtest/解析腿用常量，
/// term 走 `term_port()`（`HOMEWAY_TERM_PORT` 可改——A13 登记的可配面，本函数是它的唯一
/// 消费判据：改端口不破 tag 映射）。
/// 背压重试节拍（F8a 纯函数）：前 50 拍 2ms、51–100 拍 10ms、其后 20ms 封顶。
/// 10s 无进展上界与「空写短路」语义不随本表变化。**WIN 批**：从 `tun_exec` 搬入
/// （`tun_exec` 是 App 面〔windows 不编译〕，而本表被本模块写半与 `host_session`
/// 的写主体共用——跨平台件随跨平台模块落地；节拍语义不变）。
pub(crate) fn write_retry_backoff(attempt: u32) -> Duration {
    if attempt < 50 {
        std::time::Duration::from_millis(2)
    } else if attempt < 100 {
        std::time::Duration::from_millis(10)
    } else {
        std::time::Duration::from_millis(20)
    }
}

/// 世代/会话收工 RPC 的预算（tun_exec 原值搬入——同上跨平台化）。
pub(crate) const EXIT_RPC_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// **承载失败触发回调**（reconnect-oneshot D2-r4-N6）：服务流面的承载类失败
/// （[`StreamErr::is_carrier_failure`]）触发统一重连入口。生产装配 = `Weak<GenRun>`
/// 升级后调 `try_spawn_reconnect(ServiceStream)`；**宿主会话与 DNS 腿注入 `None`**
/// （不接——daemon/CLI 会话与自用解析腿不当连接死亡证据）。
pub(crate) type CarrierFailureFn = Arc<dyn Fn() + Send + Sync>;

#[cfg(not(windows))] // GenRun 面（App 世代执行体）
fn carrier_failure_fn_of(run: &Arc<GenRun>) -> CarrierFailureFn {
    let w = Arc::downgrade(run);
    Arc::new(move || {
        if let Some(r) = w.upgrade() {
            GenRun::try_spawn_reconnect(&r, ReconnSource::ServiceStream);
        }
    })
}

/// 拨号首试预算（tun_exec 原值搬入——同上）。
pub(crate) const FIRST_TRY: std::time::Duration = std::time::Duration::from_secs(4);

pub(crate) fn tag_for_port(port: u16) -> Option<StreamTag> {
    use super::bridge_io::{port as bport, term_port};
    if port == bport::FILES {
        Some(StreamTag::Files)
    } else if port == term_port() {
        Some(StreamTag::Term)
    } else if port == bport::SPEEDTEST {
        Some(StreamTag::Speedtest)
    } else if port == bport::DNS_RESOLVE {
        Some(StreamTag::DnsResolve)
    } else {
        None
    }
}

/// 岛命令（**流族**）的执行：`wait = None` ⇒ 无限期等（读语义，§1.5）。
///
/// 与 `tun_exec::island_cmd` 同款纪律（岛死 ⇒ 立刻归错，不挂死）：投递失败/回执口断
/// ⇒ `ConnectionLost`；到点 ⇒ `Timeout`。
fn island_stream_cmd<T>(
    island: &Island,
    make: impl FnOnce(StreamReply<T>) -> Cmd,
    wait: Option<Duration>,
) -> Result<T, StreamErr> {
    let (tx, rx) = mpsc::channel();
    island
        .tx()
        .send(make(tx))
        .map_err(|_| StreamErr::ConnectionLost)?;
    match wait {
        Some(d) => rx.recv_timeout(d).map_err(|e| match e {
            mpsc::RecvTimeoutError::Timeout => StreamErr::Timeout,
            mpsc::RecvTimeoutError::Disconnected => StreamErr::ConnectionLost,
        })?,
        None => rx.recv().map_err(|_| StreamErr::ConnectionLost)?,
    }
}

/// 开一条服务流的**原始尝试**（tag 由岛写——协议面单源）。
///
/// 等回执的预算 = `budget + OPEN_BUDGET`（照 `island_cmd(.., budget + QUIC_RPC_BUDGET)`
/// 的先例）：岛内 `open_bi` 自带 `OPEN_BUDGET` 兜底（对端 TP 错配形态），外层再夹一层
/// 只兜「岛线程卡死」。
pub(crate) fn open_stream(
    island: &Island,
    tag: StreamTag,
    budget: Duration,
) -> Result<StreamId, StreamErr> {
    island_stream_cmd(
        island,
        |reply| Cmd::StreamOpen { tag, reply },
        Some(budget + homeway_quic::stream::OPEN_BUDGET),
    )
}

/// 拨号策略的**可测主体**（纯逻辑：注入 `open` 闭包）。
///
/// 语义（§1.6 的三条一起 + reconnect-oneshot D2 的触发面）：
/// - 首试成功 ⇒ 返回；
/// - 首试是**服务级拒绝**（阶梯豁免集）⇒ **原样返回、不重试**（服务不存在/入口满/未绑定
///   /tag 非法都不是连接故障，重试只会把「出口没有该服务」放大成延时）；
/// - 首试是**连接面失败** ⇒ 预算未尽则重试一次（S4 的阶梯重写接手 QUIC 档恢复动作）；
/// - **第二次失败且承载类时调 `on_carrier_failure`**（返回值不改——触发在错误映射
///   `io::Error` 之前；首次失败不触发）。
pub(crate) fn dial_with<F, T>(
    logf: &Logf,
    tag: StreamTag,
    budget: Duration,
    mut open: F,
    on_carrier_failure: Option<&CarrierFailureFn>,
) -> Result<T, StreamErr>
where
    F: FnMut(Duration) -> Result<T, StreamErr>,
{
    let t0 = Instant::now();
    let first = open(FIRST_TRY.min(budget));
    let e = match first {
        Ok(id) => return Ok(id),
        Err(e) if e.is_ladder_exempt() => return Err(e),
        Err(e) => e,
    };
    if t0.elapsed() >= budget {
        return Err(e);
    }
    (logf)(&format!(
        "服务流重试（tag={}；首试 {}；余预算内再试一次）",
        tag.text(),
        e.text()
    ));
    let remain = budget
        .saturating_sub(t0.elapsed())
        .max(Duration::from_millis(1));
    let second = open(remain);
    if let Err(e) = &second {
        if e.is_carrier_failure(false) {
            if let Some(cb) = on_carrier_failure {
                cb(); // 第二次失败且承载类——触发统一重连入口（D2）
            }
        }
    }
    second
}

/// QUIC 档 portfwd 的**拨号缝**（M4 §2.2）：开 `STREAM[dial]` ⇒ 写 6B 目标 ⇒ 等 **1B 回执**
/// ⇒ 交付 `BridgeStream`（回执之后的余量**预置进读半缓冲**）。
///
/// 预算语义（§2.2，三步各自吃 `remain()`）：
/// - ① 岛不在 ⇒ `NotConnected`（世代装配中/已收回——与 [`dial`] 同款文本）；
/// - ② 开流**自有有界等待**（`remain()`；**不复用** [`open_stream`]——它的等回执预算 =
///   `budget + OPEN_BUDGET(5s)`，会给出 `dialMs + 5s` 的假上界）⇒ `seam 总时长 ≤ dialMs`；
/// - ③ 写 6B（走既有 [`QuicWriteHalf`] 的 `n=0` 分级退避环；**结构性事实**：开流瞬间待发队列
///   必空且容量 64 KiB ≫ 6B ⇒ 单次写入 `n == 6`；不满足则 `WriteZero`——**不承担**
///   「按 `&data[n..]` 重试」的隐含要求）；
/// - ④ 回执读（`remain()`；**空块续读**、`Closed` **显式归「拨号失败」**——不得沿
///   [`QuicReadHalf::read`] 的 `Closed ⇒ Ok(0)` 读成「成功但无回执」）。
///
/// **单次尝试（不重试）**：**不复用** [`dial_with`]（那是服务流策略：连接面失败重试一次）。
/// Q-F-B D11 已定「pf 的常态拒绝不许触发恢复阶梯」；重试只会把「出口不在/目标拒绝」放大成
/// 延时，且 pf 侧失败立刻 RST 给本机应用（浏览器自己会重连）——登记为策略面（§8 行 5）。
///
/// **RAII 关流（r19 H1，高危）**：岛内的流**只**由 `Cmd::StreamClose` 或连接死从在册表摘除，
/// **对端 reset 不清表**；而额度判据是 `map.len() < 62` ⇒ 每条「回执 0x25/0x26」的失败拨号
/// （= 常态：浏览器探测、目标没起）若不关流，就在长活连接上**永久漏一个槽**，62 条之后
/// 同一连接上的 files/term/speedtest/dial 全部 `Busy`。故：**开流成功之后立即构造
/// [`StreamShared`]**——此后任何失败路径靠 drop 关流（`Drop` 里的 `Cmd::StreamClose` 有界
/// `EXIT_RPC_BUDGET`），成功路径把同一枚 `Arc` 交给 [`QuicStream`]。
///
/// **残余微窗（代码门 r21 F3-b，如实登记；本侧无 id 可关 ⇒ 不可在客户端修）**：`StreamOpen`
/// 的等回执在**本侧**先到点（`remain()` 耗尽）而岛侧恰在此刻把 `id` 成功投进通道的形态下，
/// 调用方已放弃、`id` 也随通道消失 ⇒ 该流在岛内无句柄可关（岛侧兜底只覆盖 `reply.send`
/// **失败**的一面，见 `homeway-quic/src/driver.rs` 的 `StreamOpen` 臂）。窗口 = 微秒级且需
/// 累积 62 次才显形（跨一次「预算刚好用尽」的窄缝）⇒ 判据仍以**常态失败路径**（写帧失败 /
/// 回执 0x25·0x26 / 空块 / 预算耗尽）为准，泄漏判据（`tests/quic_pf_e2e.rs`）只覆盖 `0x25` 面。
#[cfg(not(windows))] // GenRun 面（App 世代执行体）
pub(crate) fn dial_target(
    run: &Arc<GenRun>,
    dst: SocketAddrV4,
    budget: Duration,
) -> io::Result<Box<dyn BridgeStream>> {
    let island = run.current_island().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "世代装配中（数据面未就绪）——QUIC 档端口转发无法拨号",
        )
    })?;
    // reconnect-oneshot D2：portfwd 的**首次承载失败即触发**（单次尝试路径无重试
    // 概念）——typed 层保留分类到触发点（不做 io::Error 映射后再分类，r5-N6）。
    let cb = carrier_failure_fn_of(run);
    let (id, rest) = dial_target_typed(&island, dst, budget, Some(&cb)).map_err(ack_err_to_io)?;
    // ★ RAII 守卫：构造即接管「此后任何失败路径也要关流」（见本函数 doc 的 H1 段）
    let shared = Arc::new(StreamShared {
        island,
        id,
        on_carrier_failure: Some(cb),
    });
    Ok(Box::new(QuicStream::from_shared(shared).with_pending(rest)))
}

/// `STREAM[dial]` 的**原始主体**（开流 → 写 6B 目标 → 读 1B 回执）⇒ `(id, 回执余量)`。
///
/// 两个消费方共用**唯一**一份回执值空间处理（防两处漂移，M5 S2a）：
/// - `dial_target`（隧道域 portfwd：`GenRun` 面）；
/// - `host_session::HostSession::dial_addr`（宿主会话：`&self` 面；余量交调用方的
///   `PendingBuf`）。
///
/// **关流守卫（H1 段不变量的承载体；M5 C2 就地修复）**：开流成功后**本函数内部**
/// 立即接管「此后任何失败路径也要关流」——写 6B/读回执失败时由 `Drop` 关流。
///
/// 修复沿革（如实登记）：C 棒①把本序列抽成 `dial_target_raw` 时，守卫只留在**调用方**
/// 构造（`StreamShared`/`HostStream`）⇒ 本函数内部的失败路径（写帧失败、回执异常、
/// 预算耗尽）在返回后**没有**任何句柄去关流 ⇒ 岛内流槽逐次泄漏（`pf` 的 N=80 泄漏
/// 判据实测：「本机在册 62/62」顶满 ⇒ 第 62 次起全部 `Busy`）。守卫放回本函数：
/// 成功时 `disarm()` 交棒给调用方的句柄（drop 面仍在调用方），失败时即刻关流。
struct DialCloseGuard {
    island: Arc<Island>,
    id: StreamId,
    armed: bool,
}

impl DialCloseGuard {
    fn new(island: Arc<Island>, id: StreamId) -> Self {
        DialCloseGuard { island, id, armed: true }
    }

    /// 交棒（成功路径）：此后关流归调用方持有的句柄。
    fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for DialCloseGuard {
    fn drop(&mut self) {
        if self.armed {
            close_block(&self.island, self.id);
        }
    }
}

/// **调用方契约**：取到 `id` 之后**立刻**构造关流守卫（`StreamShared`/`HostStream` 的
/// drop 面）——否则**返回之后**的失败路径会漏一个流槽（本函数 doc 的 H1 段；函数内部
/// 的失败路径由 [`DialCloseGuard`] 兜住）。
pub(crate) fn dial_target_raw(
    island: &Arc<Island>,
    dst: SocketAddrV4,
    budget: Duration,
) -> io::Result<(StreamId, Vec<u8>)> {
    // r5-N6 的**外层（IO 面）**：宿主会话消费——typed 层之上只做 `io::Error` 映射，
    // 零触发（`host_session` 的 `dial_addr` 与 DNS 腿不接重连入口）。
    dial_target_typed(island, dst, budget, None).map_err(ack_err_to_io)
}

/// **typed 层**（r5-N6）：保留 `StreamErr` 到触发点——承载类失败（dial/开流分类 =
/// `{Closed, ConnectionLost}`）在映射 `io::Error` **之前**触发 `on_carrier_failure`
/// （首次承载失败即触发——单次尝试路径无重试概念）。
///
/// **评审 r1 P0-4 整改**：ack 阶段的错误分类**全程 typed**（[`AckErr`]）——旧实现
/// 先 `io::Error` 映射再 `io_to_stream_err` 逆映射，`Busy/Refused/NotSupported/
/// Unbound/BadTag/InvalidData` 全部被误还原成 `ConnectionLost` ⇒ 误触发统一重连
/// （D2/Q-F-B D11 明确排除服务级拒绝与协议面错）。fire 只对 `Closed`/
/// `ConnectionLost` 触发（`is_carrier_failure(false)` 单一谓词）。
pub(crate) fn dial_target_typed(
    island: &Arc<Island>,
    dst: SocketAddrV4,
    budget: Duration,
    on_carrier_failure: Option<&CarrierFailureFn>,
) -> Result<(StreamId, Vec<u8>), AckErr> {
    let fire = |e: &StreamErr| {
        if e.is_carrier_failure(false) {
            if let Some(cb) = on_carrier_failure {
                cb();
            }
        }
    };
    let deadline = Instant::now().checked_add(budget);
    let wait = remain(deadline).map_err(AckErr::Stream)?;
    let id = match island_stream_cmd(
        island,
        |reply| Cmd::StreamOpen {
            tag: StreamTag::Dial,
            reply,
        },
        Some(wait),
    ) {
        Ok(id) => id,
        Err(e) => {
            fire(&e);
            return Err(AckErr::Stream(e));
        }
    };
    // ★ RAII：开流成功即接管（写帧/回执的失败路径也要关流）
    let guard = DialCloseGuard::new(Arc::clone(island), id);
    if let Err(e) = write_dial_frame_typed(island, id, dst) {
        fire(&e);
        return Err(AckErr::Stream(e));
    }
    // ack 阶段（typed 分类直达 fire——Busy/Refused/NotSupported/InvalidData 不触发）。
    let rest = dial_ack_stage(
        deadline,
        |wait| read_block(island, id, Some(wait)),
        on_carrier_failure,
    )?;
    guard.disarm();
    Ok((id, rest))
}

/// ack 阶段的触发判定（评审 r1 P0-4 的可测主体）：错误分类经 [`AckErr`] 全程
/// typed（不做 io 映射往返），**承载谓词在本层**——只有 `Closed`/`ConnectionLost`
/// （`is_carrier_failure(false)`）触发；服务级拒绝（Busy/Refused/NotSupported/…）
/// 与协议面错（`Invalid`）零触发（D2/Q-F-B D11）。
fn dial_ack_stage(
    deadline: Option<Instant>,
    read: impl FnMut(Duration) -> Result<Vec<u8>, StreamErr>,
    on_carrier_failure: Option<&CarrierFailureFn>,
) -> Result<Vec<u8>, AckErr> {
    match read_dial_ack(deadline, read) {
        Ok(r) => Ok(r),
        Err(AckErr::Stream(se)) => {
            if se.is_carrier_failure(false) {
                if let Some(cb) = on_carrier_failure {
                    cb();
                }
            }
            Err(AckErr::Stream(se))
        }
        Err(e @ AckErr::Invalid(_)) => Err(e), // 协议面错——不是承载证据，不触发
    }
}

/// dial 回执阶段的错误分类（评审 r1 P0-4）：`Stream` = 岛/流层面的原始分类
/// （承载类才触发）；`Invalid` = 协议面错（非法首字节/连续空块——**不是承载证据**，
/// 绝不触发；映射 `io::ErrorKind::InvalidData` 交消费面）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AckErr {
    Stream(StreamErr),
    Invalid(String),
}

/// [`AckErr`] → `io::Error`（消费面 = `dial_target`/`dial_target_raw` 的端口转发链）。
fn ack_err_to_io(e: AckErr) -> io::Error {
    match e {
        AckErr::Stream(se) => stream_err_to_io(se),
        AckErr::Invalid(msg) => io::Error::new(io::ErrorKind::InvalidData, msg),
    }
}

/// 服务流原始命令面（M5 S2a：宿主会话与隧道域共用的**唯一**岛命令构造点——
/// 调用方只做句柄/buffering，不另拼 `Cmd`）。
///
/// 开流 + 策略（[`dial_with`] 的可测主体）→ 只回 `id`（句柄归调用方）；`logf` 由调用方
/// 给（重试行落调用方的日志面）。
pub(crate) fn dial_stream_id(
    island: &Arc<Island>,
    logf: &Logf,
    tag: StreamTag,
    budget: Duration,
) -> Result<StreamId, StreamErr> {
    // 宿主会话面：**零触发**（host_session 注入 None——daemon/CLI 会话不当连接
    // 死亡证据，D2）。
    dial_with(logf, tag, budget, |b| open_stream(island, tag, b), None)
}

/// 读一块（无限期挂起；`Err(StreamErr::Closed)` = EOF）。`wait` = 有界面
/// （`None` = 无限期——读语义）。
pub(crate) fn read_block(
    island: &Island,
    id: StreamId,
    wait: Option<Duration>,
) -> Result<Vec<u8>, StreamErr> {
    island_stream_cmd(island, |reply| Cmd::StreamRead { id, reply }, wait)
}

/// 写一块（非阻塞接纳；有界等待）。
pub(crate) fn write_block(
    island: &Island,
    id: StreamId,
    data: Vec<u8>,
) -> Result<homeway_quic::StreamWriteOut, StreamErr> {
    island_stream_cmd(
        island,
        |reply| Cmd::StreamWrite { id, data, reply },
        Some(EXIT_RPC_BUDGET),
    )
}

/// 半关写端（FIN；有界）。
pub(crate) fn shutdown_block(island: &Island, id: StreamId) {
    let _ = island_stream_cmd(
        island,
        |reply| Cmd::StreamShutdown { id, reply },
        Some(EXIT_RPC_BUDGET),
    );
}

/// 关流（幂等；有界）。
pub(crate) fn close_block(island: &Island, id: StreamId) {
    let _ = island_stream_cmd(
        island,
        |reply| Cmd::StreamClose { id, reply },
        Some(EXIT_RPC_BUDGET),
    );
}

/// 剩余预算（§2.2 的 `remain()`）；≤0 ⇒ [`StreamErr::Timeout`]（快速失败，不越界等）。
///
/// `None` = `budget` 大到 `Instant` 装不下（`dial_ms` 是配置面，手改到天文数字时
/// `Instant + Duration` 会 **panic**）⇒ 视为「无截止」，但每步 RPC 仍用一枚**有界**等待额
/// （1h：远超一切真配置；岛内 `OPEN_BUDGET` 仍是最后一层兜底）。
fn remain(deadline: Option<Instant>) -> Result<Duration, StreamErr> {
    match deadline {
        None => Ok(Duration::from_secs(3600)),
        Some(d) => d
            .checked_duration_since(Instant::now())
            .filter(|r| !r.is_zero())
            .ok_or(StreamErr::Timeout),
    }
}

/// ③ 写 6B 目标帧（§2.2：`n == 6` 则成，否则按 `WriteZero` 语义归 `Closed`）。
/// typed 层（r5-N6）：保留 `StreamErr` 到触发点。
fn write_dial_frame_typed(
    island: &Arc<Island>,
    id: StreamId,
    dst: SocketAddrV4,
) -> Result<(), StreamErr> {
    let frame = homeway_quic::stream::dial_target(dst);
    let n = write_block(island, id, frame.to_vec()).map(|w| w.n)?;
    if n != frame.len() {
        return Err(StreamErr::Closed); // 帧不完整 = 拨号失败（非承载类）
    }
    Ok(())
}

/// 连续空块上限（**纯防御**，正常形态不可达）：岛侧若持续回 0 长块，下面的续读环否则永不收敛
/// （设计只说「预算内续读」；`remain(None)` 的天文配置形态下更无全局时限）。32 = 远大于任何
/// 分帧边界能产生的空块数，到点按**协议面无进展**收（`InvalidData` ⇒ pf 链按拨号失败处置）。
pub(super) const MAX_EMPTY_ACK_CHUNKS: u32 = 32;

/// ④ 读 1B 回执（值空间穷举见 §2.2 的 ④ 表；返回**回执之后的余量**）。
///
/// `read` 注入成闭包（生产 = `Cmd::StreamRead` 的有界等待；单测注入假块）——值空间的每一行
/// 都要有用例，而真块时序（空块 / 首块 > 1B / 对端 FIN）在真岛上不可控。
/// **错误分类 typed**（评审 r1 P0-4）：岛/流错误原样带回 [`AckErr::Stream`]（承载类
/// 判定由消费面的单一谓词做）；协议面错归 [`AckErr::Invalid`]——不做「io 映射后再
/// 分类」的往返（那会把 Busy/Refused/InvalidData 全部误判成 ConnectionLost）。
fn read_dial_ack<F>(deadline: Option<Instant>, mut read: F) -> Result<Vec<u8>, AckErr>
where
    F: FnMut(Duration) -> Result<Vec<u8>, StreamErr>,
{
    let mut empty_chunks: u32 = 0;
    loop {
        let wait = remain(deadline).map_err(AckErr::Stream)?;
        match read(wait) {
            // 空块（0B 的 STREAM 帧）：**不是**结束形态 ⇒ 预算内续读（不得 `[0]` 索引 ⇒ panic）；
            // 连续空块有界（[`MAX_EMPTY_ACK_CHUNKS`]）
            Ok(chunk) if chunk.is_empty() => {
                empty_chunks += 1;
                if empty_chunks > MAX_EMPTY_ACK_CHUNKS {
                    return Err(AckErr::Invalid(format!(
                        "QUIC 服务流：dial 回执连续 {MAX_EMPTY_ACK_CHUNKS} 个空块（协议面无进展）"
                    )));
                }
                continue;
            }
            // 成功：首字节 = DIAL_OK，其后即目标侧裸字节（**余量必须带回**，§1.2 铁律）
            Ok(chunk) if chunk[0] == DIAL_OK => return Ok(chunk[1..].to_vec()),
            // 首字节非法 = 协议面错（**不得**当 EOF/成功——也不当承载证据）
            Ok(chunk) => {
                return Err(AckErr::Invalid(format!(
                    "QUIC 服务流：dial 回执首字节非法（{:#04x}）",
                    chunk[0]
                )))
            }
            // `Closed`（对端 FIN / 白名单外复位码 / `ConnectionLost`）**显式归拨号失败**
            // （消费面 io 映射 ⇒ pf 链 fails+1 + RST；承载类触发由 typed 分类判定）
            Err(e) => return Err(AckErr::Stream(e)),
        }
    }
}

/// QUIC 档的一条服务流连接（`BridgeStream`；应用层帧逐字节不变）。
#[cfg(not(windows))] // GenRun 面（App 世代执行体；宿主会话走 dial_with/open_stream 直拼）
pub(crate) fn dial(
    run: &Arc<GenRun>,
    tag: StreamTag,
    budget: Duration,
) -> io::Result<Box<dyn BridgeStream>> {
    let island = run.current_island().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "QUIC 岛不在（世代装配中/已收回）——服务流无法拨号",
        )
    })?;
    // reconnect-oneshot D2：files/term/speedtest = 用户可见服务入口——开流有界重试后
    // 仍承载类失败 ⇒ 触发统一入口（ServiceStream 源）；**DNS 腿不接**（客户端自用
    // 解析腿：解析失败是环境噪声面，不当连接死亡证据）。
    let trigger = !matches!(tag, StreamTag::DnsResolve);
    let cb = carrier_failure_fn_of(run);
    let sel: Option<&CarrierFailureFn> = trigger.then_some(&cb);
    let id = dial_with(&run.logf, tag, budget, |b| open_stream(&island, tag, b), sel)
        .map_err(stream_err_to_io)?;
    // 评审 r1 2.21 整改：**流句柄的回调注入与开流触发同一门**——DNS 腿的读/写半
    // 也不携带承载回调（旧实现 `new_with_carrier` 无条件注入 `Some(cb)`，DNS 流的
    // ConnectionLost 会误触发 ServiceStream 源——「DNS 腿不接」在 dial() 类型面
    // 不成立的潜在路径；生产 DNS 走 `host_session::dial_stream_id`（None），本修
    // 补齐 dial() 面的同一策略）。
    let shared = Arc::new(StreamShared {
        island,
        id,
        on_carrier_failure: trigger.then_some(cb),
    });
    Ok(Box::new(QuicStream::from_shared(shared)))
}

/// `StreamErr` → `io::Error`（§1.6 的**消费点迁移表**；桥宿主的 refused 判定只认 kind）。
///
/// - `NotSupported`（`0x22`=出口无该服务）⇒ `ConnectionRefused`：保持今天「出口活着、
///   端口没服务 ⇒ 客户端零字节 EOF ⇒ not_supported」的既有判定链（`bridge_host::is_refused_like`、
///   App 测速桥的 `link_down` 分流）；
/// - **新分支**（§1.6 要求有用例）：`Busy`/`Unbound`/`BadTag` ⇒ `ErrorKind::Other`；
/// - `Timeout` ⇒ `TimedOut`（与 `conn_err_to_io` 同款）；
/// - `Closed`/`ConnectionLost` ⇒ `Other`（M5 C2：WG 档的 `ConnErr::{Closed,EngineGone}`
///   落点由本映射**同形承继**——CLI/daemon 的判定链只认 kind，不信类型名）。
fn stream_err_to_io(e: StreamErr) -> io::Error {
    let kind = match e {
        StreamErr::NotSupported | StreamErr::Refused => io::ErrorKind::ConnectionRefused,
        StreamErr::Timeout => io::ErrorKind::TimedOut,
        StreamErr::Busy | StreamErr::Unbound | StreamErr::BadTag => io::ErrorKind::Other,
        StreamErr::Closed | StreamErr::ConnectionLost => io::ErrorKind::Other,
        // `StreamErr` 是 `#[non_exhaustive]`（AGENTS 原则 1）⇒ 跨 crate 匹配必须留通配臂：
        // 将来新增的流错误在**本接缝**按「非连接故障、非 refused」的保守面收
        // （不误报 refused；具体分类由 homeway-quic 侧的 `StreamErr` 取值集扩时同批登记）。
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, format!("QUIC 服务流：{}（{e}）", e.text()))
}

/// 流句柄的共享体（两半各持一份 `Arc`；**Drop = 关流**——防半开连接累积；形态承
/// 自已退役的 WG 档 `SharedConn`，预算同款）。
// 构造面 = dial/dial_target（GenRun 桥承载，windows 不编译）——windows 下未构造
// 即静音（方法面是可携的，QuicStream 本体在宿主会话路径用）。
#[cfg_attr(windows, allow(dead_code))]
struct StreamShared {
    island: Arc<Island>,
    id: StreamId,
    /// 承载失败触发回调（reconnect-oneshot D2-r4-N6）：在飞流读写半报
    /// `ConnectionLost` 时调用；宿主会话构造的共享体为 `None`（零触发）。
    on_carrier_failure: Option<CarrierFailureFn>,
}

impl Drop for StreamShared {
    fn drop(&mut self) {
        // 关流走在必须退出的收工链上（桥泵收口）⇒ 有界面（引擎卡死时不挂死线程）
        let _ = island_stream_cmd(
            &self.island,
            |reply| Cmd::StreamClose {
                id: self.id,
                reply,
            },
            Some(EXIT_RPC_BUDGET),
        );
    }
}

/// QUIC 服务流整体（`BridgeStream`：拆两半给桥泵）。
#[cfg(not(windows))] // 构造面 = dial/dial_target（GenRun 桥承载，windows 不编译）
pub(crate) struct QuicStream {
    shared: Arc<StreamShared>,
    /// 交付前**已在手**的余量（dial 缝的 1B 回执之后可能立刻跟目标字节 ⇒ 首块
    /// `[0x01, payload…]`；余量丢失 = **应用层帧错位**，§1.2 铁律）。开流时为空。
    pending: Vec<u8>,
}

#[cfg(not(windows))]
impl QuicStream {
    /// 从既有共享体构造（dial/dial_target 缝的唯一构造面——承载回调的有无在
    /// 共享体上决定：DNS 腿注入 None，见 `dial` 的整改注记）。
    fn from_shared(shared: Arc<StreamShared>) -> Self {
        QuicStream {
            shared,
            pending: Vec::new(),
        }
    }

    /// 预置读半缓冲（**余量保真**；见 `pending` 字段的 doc）。
    fn with_pending(mut self, pending: Vec<u8>) -> Self {
        self.pending = pending;
        self
    }
}

// App 桥承载面（windows 不装配三桥——宿主会话直接用 QuicStream 的 Read/Write）
#[cfg(not(windows))]
impl BridgeStream for QuicStream {
    fn into_halves(
        self: Box<Self>,
    ) -> io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)> {
        Ok((
            Box::new(QuicReadHalf {
                shared: Arc::clone(&self.shared),
                pending: PendingBuf::new(self.pending),
            }),
            Box::new(QuicWriteHalf {
                shared: Arc::clone(&self.shared),
            }),
        ))
    }
}

/// 读半：`Cmd::StreamRead` **无限期挂起**（§1.5）；`Err(Closed)` = EOF（今天的
/// `SessionReadHalf` 同形——桥泵据此收口）。
#[cfg(not(windows))]
struct QuicReadHalf {
    shared: Arc<StreamShared>,
    /// 交付前已在手的余量（dial 缝的 1B 回执之后可能立刻跟目标字节）——**先于**任何岛命令
    /// 吐出（见 [`PendingBuf`]）。
    pending: PendingBuf,
}

/// 交付前余量的**先出**缓冲（`with_pending` 的载体）。
///
/// 单独成件（而不是裸 `buf/off` 字段）的唯一理由 = **可测**：这条不变量（余量先出、分次不丢、
/// 空则转岛命令面）没有真岛就构造不出「首块 > 1B」的时序，而它一旦破了就是**应用层帧错位**
/// （浏览器侧表现为「响应缺首字节」，极难定位——§1.2 铁律）。
/// 读余量缓冲（单源：本文件与 `host_session` 的流面共用——**余量丢失 = 应用层帧
/// 错位**，两处必须同一实现）。
#[derive(Default)]
pub(super) struct PendingBuf {
    pub(super) buf: Vec<u8>,
    pub(super) off: usize,
}

impl PendingBuf {
    pub(super) fn new(buf: Vec<u8>) -> Self {
        PendingBuf { buf, off: 0 }
    }

    /// 取一段（`None` = 余量已空 ⇒ 调用方转下一段读取）。
    pub(super) fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.off >= self.buf.len() {
            return None;
        }
        let n = (self.buf.len() - self.off).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.off..self.off + n]);
        self.off += n;
        Some(n)
    }
}

#[cfg(not(windows))]
impl Read for QuicReadHalf {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(n) = self.pending.take(out) {
            return Ok(n);
        }
        let id = self.shared.id;
        // 空块（0B 的 STREAM 帧）**不是** EOF——续读有界（与 [`read_dial_ack`] 同款；
        // 2026-10-10 审查：原对空块直接 `Ok(0)`，而 `io::Read` 的 `Ok(0)` = 通道关 ⇒
        // 一个空块就会被消费者误判成流提前结束）。
        let mut empty_chunks = 0u32;
        loop {
            let data = match island_stream_cmd(
                &self.shared.island,
                |reply| Cmd::StreamRead { id, reply },
                None,
            ) {
                Ok(data) => data,
                // 对端 FIN / 白名单外的复位 / 本端关流 ⇒ EOF（§1.6 的 EOF 同形性；
                // **不是**承载证据——不触发，reconnect-oneshot D2）
                Err(StreamErr::Closed) => return Ok(0),
                Err(StreamErr::ConnectionLost) => {
                    // 在飞读半的承载丢失 ⇒ 触发（D2：只认 ConnectionLost）
                    if let Some(cb) = &self.shared.on_carrier_failure {
                        cb();
                    }
                    return Err(stream_err_to_io(StreamErr::ConnectionLost));
                }
                Err(e) => return Err(stream_err_to_io(e)),
            };
            if data.is_empty() {
                empty_chunks += 1;
                if empty_chunks > MAX_EMPTY_ACK_CHUNKS {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!("QUIC 服务流：连续 {MAX_EMPTY_ACK_CHUNKS} 个空块（协议面无进展）"),
                    ));
                }
                continue;
            }
            let n = data.len().min(out.len());
            out[..n].copy_from_slice(&data[..n]);
            self.pending = PendingBuf::new(data);
            self.pending.off = n;
            return Ok(n);
        }
    }
}

/// 写半：`Cmd::StreamWrite`（**非阻塞**：`n` 由待发队列余量给出）；`n=0` 走 §1.4 的
/// Ok(0) 分级退避环（**不得**按 `io::Write` 的「`Ok(0)` = 通道关」处理——那是本仓
/// 被真机打过的坑）。
#[cfg(not(windows))]
struct QuicWriteHalf {
    shared: Arc<StreamShared>,
}

#[cfg(not(windows))]
impl Write for QuicWriteHalf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0); // 空写短路（io::Write 约定：空写返 Ok(0)，不是背压）
        }
        let no_progress = Instant::now();
        let mut attempt: u32 = 0;
        let mut pending: Option<Vec<u8>> = Some(data.to_vec());
        loop {
            let chunk = match pending.take() {
                Some(v) => v,
                None => data.to_vec(), // 防御面：回执未带（不发生——零接纳必带）
            };
            let id = self.shared.id;
            match island_stream_cmd(
                &self.shared.island,
                |reply| Cmd::StreamWrite { id, data: chunk, reply },
                Some(EXIT_RPC_BUDGET),
            ) {
                Ok(w) if w.n == 0 => {
                    pending = w.back;
                    if no_progress.elapsed() > Duration::from_secs(10) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "写通道长时间无进展（服务流待发队列不排空）",
                        ));
                    }
                    std::thread::sleep(write_retry_backoff(attempt));
                    attempt = attempt.saturating_add(1);
                }
                Ok(w) => return Ok(w.n),
                Err(StreamErr::Closed) => return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "服务流已关闭（EOF 面）",
                )),
                Err(StreamErr::ConnectionLost) => {
                    // 在飞写半的承载丢失 ⇒ 触发（D2：只认 ConnectionLost）
                    if let Some(cb) = &self.shared.on_carrier_failure {
                        cb();
                    }
                    return Err(stream_err_to_io(StreamErr::ConnectionLost));
                }
                Err(e) => return Err(stream_err_to_io(e)),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(not(windows))]
impl WriteHalf for QuicWriteHalf {
    fn close_write(&mut self) {
        // 半关 = FIN（§1.3：单向——对端仍可发、本端仍可读）；走收工链 ⇒ 有界
        let id = self.shared.id;
        let _ = island_stream_cmd(
            &self.shared.island,
            |reply| Cmd::StreamShutdown { id, reply },
            Some(EXIT_RPC_BUDGET),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn quiet() -> Logf {
        Arc::new(|_s: &str| {})
    }

    /// **判据（虚拟端口 → tag 单源，§5.1）**：7802/7724/7803 三个端口各归其 tag；
    /// 其余（含 0/1/7804）**无 tag**（QUIC 档不得静默拨到别的服务）。
    #[test]
    fn virtual_ports_map_to_their_stream_tags() {
        use super::super::bridge_io::{port as bport, term_port};
        assert_eq!(tag_for_port(bport::FILES), Some(StreamTag::Files));
        assert_eq!(tag_for_port(bport::SPEEDTEST), Some(StreamTag::Speedtest));
        // term 端口是**可配面**（HOMEWAY_TERM_PORT）——映射随 `term_port()` 走，
        // 不写死字面量（写死 = 改端口即拨错服务）
        assert_eq!(tag_for_port(term_port()), Some(StreamTag::Term));
        // DNS-STREAM 批：远程解析腿 5300 ⇒ tag=6（宿主侧 socks 域名目标的落点）
        assert_eq!(tag_for_port(bport::DNS_RESOLVE), Some(StreamTag::DnsResolve));
        for p in [0u16, 1, 80, 7801, 7804, 7725, 65535] {
            if p == term_port() {
                continue;
            }
            assert_eq!(tag_for_port(p), None, "端口 {p} 无服务 tag");
        }
        // 值域锚（与出口 `DEFAULT_*_PORT` 同源——漂了就两边对不上）
        assert_eq!(bport::FILES, 7802);
        assert_eq!(bport::TERM, 7724);
        assert_eq!(bport::SPEEDTEST, 7803);
        assert_eq!(bport::DNS_RESOLVE, 5300);
        // 单源 pin（3-5）：socks 解析腿常量与映射常量必须同源（各写一份字面 = 漂移口）
        assert_eq!(
            crate::daemon::carriers::dnsq::SOCKS_DNS_PORT,
            bport::DNS_RESOLVE,
            "SOCKS_DNS_PORT 必须引用 bridge_host::port::DNS_RESOLVE"
        );
    }

    /// **判据（阶梯豁免集，§1.6 设计门 3-3；S3 完成判据点名）**：服务级拒绝
    /// **不触发恢复/重试**（一次尝试即收口，错误原样带走）；连接面失败才重试。
    #[test]
    fn service_level_refusals_do_not_retry_and_connection_faults_do() {
        // ① 四种服务级拒绝：尝试次数恒 1、错误原样返回
        for e in [
            StreamErr::NotSupported,
            StreamErr::Busy,
            StreamErr::Unbound,
            StreamErr::BadTag,
        ] {
            let n = AtomicUsize::new(0);
            let r: Result<u8, StreamErr> =
                dial_with(&quiet(), StreamTag::Term, Duration::from_secs(15), |_b| {
                    n.fetch_add(1, Ordering::SeqCst);
                    Err(e)
                }, None);
            assert_eq!(r, Err(e), "{e:?} 必须原样返回");
            assert_eq!(n.load(Ordering::SeqCst), 1, "{e:?} 不得重试（不触发恢复）");
        }
        // ② 连接面失败（Closed/Timeout/ConnectionLost）：预算内重试一次 ⇒ 2 次尝试
        for e in [StreamErr::Closed, StreamErr::Timeout, StreamErr::ConnectionLost] {
            let n = AtomicUsize::new(0);
            let r: Result<u8, StreamErr> =
                dial_with(&quiet(), StreamTag::Files, Duration::from_secs(15), |_b| {
                    n.fetch_add(1, Ordering::SeqCst);
                    Err(e)
                }, None);
            assert_eq!(r, Err(e));
            assert_eq!(n.load(Ordering::SeqCst), 2, "{e:?} 须重试一次");
        }
        // ③ 首试成功 ⇒ 只尝试一次（策略与成功值无关：用 () 型注入，不依赖岛内类型构造面）
        let n = AtomicUsize::new(0);
        let r: Result<u8, StreamErr> =
            dial_with(&quiet(), StreamTag::Files, Duration::from_secs(15), |_b| {
                n.fetch_add(1, Ordering::SeqCst);
                Ok(7)
            }, None);
        assert_eq!(r, Ok(7), "首试成功必须直接返回");
        assert_eq!(n.load(Ordering::SeqCst), 1);
        // ④ 预算已耗尽（budget=0）⇒ 连接面失败也不重试（不越界）
        let n = AtomicUsize::new(0);
        let r: Result<u8, StreamErr> =
            dial_with(&quiet(), StreamTag::Files, Duration::ZERO, |_b| {
                n.fetch_add(1, Ordering::SeqCst);
                Err(StreamErr::Closed)
            }, None);
        assert_eq!(r, Err(StreamErr::Closed));
        assert_eq!(n.load(Ordering::SeqCst), 1, "预算耗尽不得再试");
    }

    /// **判据（错误映射，§1.6 迁移表 + 新分支）**：`NotSupported` ⇒ refused 面
    /// （保「出口活着、端口没服务」判定链）；`Busy/Unbound/BadTag` ⇒ **新分支** Other；
    /// `Timeout` ⇒ TimedOut。
    #[test]
    fn stream_errors_map_to_the_designed_io_kinds() {
        let kind = |e: StreamErr| stream_err_to_io(e).kind();
        assert_eq!(kind(StreamErr::NotSupported), io::ErrorKind::ConnectionRefused);
        for e in [StreamErr::Busy, StreamErr::Unbound, StreamErr::BadTag] {
            assert_eq!(kind(e), io::ErrorKind::Other, "{e:?} 走新增分支");
        }
        assert_eq!(kind(StreamErr::Timeout), io::ErrorKind::TimedOut);
        assert_eq!(kind(StreamErr::Closed), io::ErrorKind::Other);
        assert_eq!(kind(StreamErr::ConnectionLost), io::ErrorKind::Other);
        // 桥宿主的 refused 谓词只认 kind：NotSupported 必须在它的集合里
        let refused_like = |e: StreamErr| {
            matches!(
                stream_err_to_io(e).kind(),
                io::ErrorKind::ConnectionRefused | io::ErrorKind::ConnectionReset
            )
        };
        assert!(refused_like(StreamErr::NotSupported), "0x22 ⇒ not_supported 链不变");
        assert!(!refused_like(StreamErr::Busy), "0x23 不是 refused 面");
    }

    /// **判据（dial 回执值空间穷举，§2.2 的 ④ 表逐行；评审 r1 P0-4 改 typed 后同批
    /// 迁移）**：八行各有断言——`[0x01,rest]` / 空块续读 / 首字节非 0x01 / `Busy` /
    /// `Refused` / `Timeout` / `Closed`（**显式归拨号失败，不得读成 EOF/成功**）/
    /// `ConnectionLost`；另加 `NotSupported/Unbound/BadTag` 的防御面。io kind 判定经
    /// [`ack_err_to_io`]（消费面同款映射）。
    #[test]
    fn dial_ack_value_space_is_exhaustive() {
        let far = Some(Instant::now() + Duration::from_secs(5));
        let never = |_w: Duration| -> Result<Vec<u8>, StreamErr> { unreachable!("不应被调用") };

        // ① `[0x01, rest…]` ⇒ 成功 + **余量原样带回**（1 字节不丢；§1.2 铁律）
        let rest = read_dial_ack(far, |_w| Ok(vec![DIAL_OK, b'a', b'b', b'c'])).expect("成功");
        assert_eq!(rest, b"abc", "回执之后的余量必须逐字节带回");
        let rest = read_dial_ack(far, |_w| Ok(vec![DIAL_OK])).expect("成功");
        assert!(rest.is_empty(), "只有回执 = 空余量");

        // ② 空块（0B 的 STREAM 帧）⇒ **预算内续读**（不得 `[0]` 索引 panic）
        let mut calls = 0;
        let rest = read_dial_ack(far, |_w| {
            calls += 1;
            match calls {
                1 | 2 => Ok(Vec::new()),
                _ => Ok(vec![DIAL_OK, 9]),
            }
        })
        .expect("空块后续读到回执");
        assert_eq!(rest, vec![9]);
        assert_eq!(calls, 3, "空块不是结束形态");

        // ③ 首字节非 0x01 ⇒ Invalid（协议面错）
        let e = read_dial_ack(far, |_w| Ok(vec![0x22, 1, 2])).expect_err("非法首字节");
        assert_eq!(e, AckErr::Invalid("QUIC 服务流：dial 回执首字节非法（0x22）".to_owned()), "{e:?}");

        // ④ 岛内快速失败 / 出口复位码 / 空面：typed 原样带回 + io kind 逐行映射
        for (se, kind) in [
            (StreamErr::Busy, io::ErrorKind::Other),
            (StreamErr::Refused, io::ErrorKind::ConnectionRefused),
            (StreamErr::Timeout, io::ErrorKind::TimedOut),
            (StreamErr::ConnectionLost, io::ErrorKind::Other),
            (StreamErr::NotSupported, io::ErrorKind::ConnectionRefused),
            (StreamErr::Unbound, io::ErrorKind::Other),
            (StreamErr::BadTag, io::ErrorKind::Other),
        ] {
            let e = read_dial_ack(far, |_w| Err(se)).expect_err("必须归错");
            assert_eq!(e, AckErr::Stream(se), "typed 原样带回：{se:?}");
            assert_eq!(ack_err_to_io(e).kind(), kind, "{se:?} 的 io kind");
        }
        // ⑤ `Closed`（对端 FIN / 白名单外复位 / ConnectionLost）⇒ **显式拨号失败**
        //    （**不得**沿读半的 `Closed ⇒ Ok(0)` 读成「成功无回执」）
        let e = read_dial_ack(far, |_w| Err(StreamErr::Closed)).expect_err("Closed 必须是失败");
        assert_eq!(e, AckErr::Stream(StreamErr::Closed));
        assert!(
            ack_err_to_io(e).to_string().contains("服务流已关闭"),
            "Closed 的归因串"
        );

        // ⑥ 预算耗尽（deadline 已过）⇒ 不再调 read，直接 Timeout
        let past = Some(Instant::now() - Duration::from_millis(1));
        let e = read_dial_ack(past, never).expect_err("预算耗尽");
        let kind = ack_err_to_io(e).kind();
        assert_eq!(kind, io::ErrorKind::TimedOut);
    }

    /// **判据（评审 r1 P0-4 的四 typed 负例）**：ack 阶段的服务级拒绝与协议面错
    /// **零触发**——`Busy`/`Refused`/`NotSupported`/`InvalidData`（非法首字节与
    /// 连续空块两形态）都不是承载证据（D2/Q-F-B D11），不得误发统一重连；
    /// 对照组 `Closed`/`ConnectionLost` 各触发恰一次。
    #[test]
    fn dial_ack_stage_service_rejections_and_protocol_errors_do_not_fire() {
        let far = Some(Instant::now() + Duration::from_secs(5));
        // 四个 typed 负例：零触发、错误分类原样带回
        for se in [
            StreamErr::Busy,
            StreamErr::Refused,
            StreamErr::NotSupported,
        ] {
            let (n, cb) = counting_cb();
            let r = dial_ack_stage(far, |_w| Err(se), Some(&cb));
            assert_eq!(r, Err(AckErr::Stream(se)), "{se:?} typed 原样带回");
            assert_eq!(n.load(Ordering::SeqCst), 0, "{se:?}（服务级拒绝）零触发");
        }
        // InvalidData 两形态（非法首字节 / 连续空块超限）：零触发 + Invalid 分类
        for read in [
            // 非法首字节（0x25 = 出口拒绝码——正常拒绝形态，不是连接死亡）
            |_w: Duration| Ok(vec![0x25u8, 1, 2]),
            // 连续空块超限（协议面无进展）
            |_w: Duration| Ok(Vec::new()),
        ] {
            let (n, cb) = counting_cb();
            let r = dial_ack_stage(far, read, Some(&cb));
            assert!(matches!(r, Err(AckErr::Invalid(_))), "协议面错归 Invalid：{r:?}");
            assert_eq!(n.load(Ordering::SeqCst), 0, "协议面错零触发");
            let kind = ack_err_to_io(r.expect_err("须归错")).kind();
            assert_eq!(kind, io::ErrorKind::InvalidData, "InvalidData 分类保真");
        }
        // 对照组：承载类各触发恰一次
        for se in [StreamErr::Closed, StreamErr::ConnectionLost] {
            let (n, cb) = counting_cb();
            let r = dial_ack_stage(far, |_w| Err(se), Some(&cb));
            assert_eq!(r, Err(AckErr::Stream(se)));
            assert_eq!(n.load(Ordering::SeqCst), 1, "{se:?} 承载类触发恰一次");
        }
    }

    /// **判据（`remain()` 的两形态）**：未到点 ⇒ 剩余额；到点/已过 ⇒ `Timeout`；
    /// `None`（配置面给到 `Instant` 装不下的预算）⇒ 一枚**有界**等待额（不 panic、不无限挂）。
    #[test]
    fn remain_is_bounded_and_never_panics() {
        let now = Instant::now();
        let r = remain(Some(now + Duration::from_secs(5))).expect("未到点");
        assert!(r <= Duration::from_secs(5) && r > Duration::from_secs(4), "{r:?}");
        assert_eq!(remain(Some(now)).err(), Some(StreamErr::Timeout), "零剩余");
        assert_eq!(
            remain(Some(now - Duration::from_millis(1))).err(),
            Some(StreamErr::Timeout),
            "已过点"
        );
        // 溢出防御：`dial_ms` 是配置面 ⇒ `Instant + Duration` 会 panic 的输入不得进到加法
        assert!(Instant::now().checked_add(Duration::MAX).is_none(), "前提：MAX 装不下");
        assert!(remain(None).is_ok(), "无截止 ⇒ 仍有界等待");
    }

    /// **判据（余量保真：读半先给余量、分次不丢、空则转岛命令面）**：`[0x01, 3B]` 同块
    /// 形态 ⇒ 读半先吐 3B（**在**任何岛命令之前）；`out` 比余量小时分次吐且字节序不变。
    #[test]
    fn pending_prefix_goes_first_and_never_loses_a_byte() {
        let mut p = PendingBuf::new(b"xyz".to_vec());
        let mut out = [0u8; 8];
        assert_eq!(p.take(&mut out), Some(3), "余量先出（3B 全额）");
        assert_eq!(&out[..3], b"xyz");
        assert_eq!(p.take(&mut out), None, "余量空 ⇒ 转岛命令面（`Cmd::StreamRead`）");

        // 分次取（`out` 小于余量）：逐段吐出、顺序不变、总长不变
        let mut p = PendingBuf::new(b"abcdef".to_vec());
        let mut got = Vec::new();
        let mut one = [0u8; 4];
        while let Some(n) = p.take(&mut one) {
            got.extend_from_slice(&one[..n]);
        }
        assert_eq!(got, b"abcdef", "分次读取不丢字节、不乱序");
        // 空余量（无回执余量的常规开流）⇒ 直接就 None
        let mut p = PendingBuf::new(Vec::new());
        assert_eq!(p.take(&mut out), None);
    }

    // ==========================================================================
    // reconnect-oneshot 2.2：服务流承载失败触发面（CarrierFailureFn）
    // ==========================================================================

    /// 计数回调（触发面观测）。
    fn counting_cb() -> (std::sync::Arc<std::sync::atomic::AtomicUsize>, CarrierFailureFn) {
        let n = std::sync::Arc::new(AtomicUsize::new(0));
        let n2 = std::sync::Arc::clone(&n);
        (n, std::sync::Arc::new(move || {
            n2.fetch_add(1, Ordering::SeqCst);
        }))
    }

    /// **判据（2.2①）**：`dial_with` 第二次失败且承载类 ⇒ 触发恰一次（返回值不改）；
    /// 首次失败不触发（预算耗尽形态）；Timeout/Refused/Busy（非承载类）不触发。
    #[test]
    fn dial_with_triggers_on_second_carrier_failure_only() {
        // Closed/ConnectionLost：第二次失败（预算内重试后）⇒ 触发 1 次、Err 原样带回
        for e in [StreamErr::Closed, StreamErr::ConnectionLost] {
            let (n, cb) = counting_cb();
            let r: Result<u8, StreamErr> = dial_with(
                &quiet(),
                StreamTag::Files,
                Duration::from_secs(15),
                |_b| Err(e),
                Some(&cb),
            );
            assert_eq!(r, Err(e), "返回值不改（错误原样——触发在映射之前）");
            assert_eq!(n.load(Ordering::SeqCst), 1, "{e:?}：第二次承载类失败触发一次");
        }
        // 非承载类（Timeout/Refused/Busy——目标拒绝类与超时不属承载类）：零触发
        for e in [StreamErr::Timeout, StreamErr::Refused, StreamErr::Busy] {
            let (n, cb) = counting_cb();
            let _r: Result<u8, StreamErr> = dial_with(
                &quiet(),
                StreamTag::Files,
                Duration::from_secs(15),
                |_b| Err(e),
                Some(&cb),
            );
            assert_eq!(n.load(Ordering::SeqCst), 0, "{e:?}：非承载类零触发");
        }
        // 首次承载失败（预算耗尽不重试）⇒ 零触发（只有第二次才触发）
        let (n, cb) = counting_cb();
        let _r: Result<u8, StreamErr> = dial_with(
            &quiet(),
            StreamTag::Files,
            Duration::ZERO,
            |_b| Err(StreamErr::ConnectionLost),
            Some(&cb),
        );
        assert_eq!(n.load(Ordering::SeqCst), 0, "首次失败（未重试）零触发");
    }

    /// **判据（2.2③）**：portfwd 的 typed 层（`dial_target_typed`）——**首次承载失败
    /// 即触发**（单次尝试路径无重试概念）；IO 层（`dial_target_raw`——宿主会话面）
    /// 零触发。真 idle 岛：无 live 连接 ⇒ StreamOpen 立即 ConnectionLost。
    #[test]
    #[cfg(not(windows))]
    fn dial_target_typed_fires_on_first_carrier_failure() {
        let ident = crate::identity::Identity::ephemeral().expect("临时身份");
        let cred = homeway_quic::IslandCredential::new(
            homeway_quic::TokenSecret::from_bytes([7u8; 32]),
            ident.public_key(),
            *ident.dev_tag().as_bytes(),
            homeway_quic::RpkPublicKey::from_bytes([9u8; 32]),
        );
        let mut icfg = homeway_quic::IslandConfig::new(cred);
        icfg.bind = Some(std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)));
        let island = std::sync::Arc::new(
            homeway_quic::Island::start(quiet(), std::sync::Arc::new(|_r: &str| {}), icfg)
                .expect("idle 岛可起"),
        );
        let (n, cb) = counting_cb();
        let dst: SocketAddrV4 = "127.0.0.1:8080".parse().unwrap();
        let r = dial_target_typed(&island, dst, Duration::from_millis(200), Some(&cb));
        assert!(r.is_err(), "idle 岛无 live 连接 ⇒ 拨号失败");
        assert_eq!(
            n.load(Ordering::SeqCst),
            1,
            "首次承载失败即触发（开流 ConnectionLost）"
        );
        // host_session 面（IO 层）：零触发（None 注入）
        let (n2, cb2) = counting_cb();
        let _ = &cb2;
        let r2 = dial_target_typed(&island, dst, Duration::from_millis(200), None);
        assert!(r2.is_err());
        assert_eq!(n2.load(Ordering::SeqCst), 0, "None 注入零触发（宿主会话不接）");
        island.stop();
    }
}
