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
//!    `connect_deadline` —— QUIC 档的服务流拨号只走岛的 STREAM 命令面。WG 档与
//!    「岛不在 ⇒ 回落 WG」的派发在 `tun_exec` 的桥构造处（那里调 `session_connect`）。
//! 2. **阶梯豁免集**（§1.6 设计门 3-3）：服务级拒绝（[`StreamErr::is_ladder_exempt`]：
//!    `NotSupported/Busy/Unbound/BadTag`）**不是连接故障** ⇒ 立即收口、不进恢复；
//!    连接面失败（`Closed/Timeout/ConnectionLost`）在调用方预算内**重试一次**。
//!    **不注入 WG 恢复阶梯**：QUIC 档不得产生 C11 族行（§3.4），QUIC 档的恢复动作
//!    归 S4 的阶梯重写（本切片只做「换轨 + 归因」，不动阶梯结构）。
//! 3. **读无限期**（§1.5）：[`Read`] 的读挂到数据/EOF/取消为止——files 空闲 5min、
//!    term 腿可挂数小时，**不得**套 RPC 预算；取消只经关流（`Drop`/`close_write`）。
//!    写走 §1.4 的 `n=0` 分级退避环（`tun_exec::write_retry_backoff` 同款）。

use std::io::{self, Read, Write};
use std::net::SocketAddrV4;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeway_quic::stream::DIAL_OK;
use homeway_quic::{Cmd, Island, StreamErr, StreamId, StreamReply, StreamTag};

use crate::Logf;

use super::bridge_host::{BridgeStream, WriteHalf};
use super::tun_exec::{write_retry_backoff, GenRun, EXIT_RPC_BUDGET, FIRST_TRY};

/// 虚拟端口 → 服务 tag（§5.1：`7802/7724/7803 → 1/2/3`；**其余端口无 tag**）。
///
/// 两个端口源都是**既有配置面**（E1 值域不变）：files/speedtest 用常量，term 走
/// `term_port()`（`HOMEWAY_TERM_PORT` 可改——A13 登记的可配面，本函数是它的唯一消费
/// 判据：改端口不破 tag 映射）。
pub(crate) fn tag_for_port(port: u16) -> Option<StreamTag> {
    use super::bridge_host::{port as bport, term_port};
    if port == bport::FILES {
        Some(StreamTag::Files)
    } else if port == term_port() {
        Some(StreamTag::Term)
    } else if port == bport::SPEEDTEST {
        Some(StreamTag::Speedtest)
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
/// 语义（§1.6 的三条一起）：
/// - 首试成功 ⇒ 返回；
/// - 首试是**服务级拒绝**（阶梯豁免集）⇒ **原样返回、不重试**（服务不存在/入口满/未绑定
///   /tag 非法都不是连接故障，重试只会把「出口没有该服务」放大成延时）；
/// - 首试是**连接面失败** ⇒ 预算未尽则重试一次（S4 的阶梯重写接手 QUIC 档恢复动作）。
pub(crate) fn dial_with<F, T>(
    logf: &Logf,
    tag: StreamTag,
    budget: Duration,
    mut open: F,
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
        "quic: 服务流重试（tag={}；首试 {}；余预算内再试一次）",
        tag.text(),
        e.text()
    ));
    let remain = budget
        .saturating_sub(t0.elapsed())
        .max(Duration::from_millis(1));
    open(remain)
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
    let (id, rest) = dial_target_raw(&island, dst, budget)?;
    // ★ RAII 守卫：构造即接管「此后任何失败路径也要关流」（见本函数 doc 的 H1 段）
    let shared = Arc::new(StreamShared { island, id });
    Ok(Box::new(QuicStream::from_shared(shared).with_pending(rest)))
}

/// `STREAM[dial]` 的**原始主体**（开流 → 写 6B 目标 → 读 1B 回执）⇒ `(id, 回执余量)`。
///
/// 两个消费方共用**唯一**一份回执值空间处理（防两处漂移，M5 S2a）：
/// - `dial_target`（隧道域 portfwd：`GenRun` 面）；
/// - `host_session::HostSession::dial_addr`（宿主会话：`&self` 面；余量交调用方的
///   `PendingBuf`）。
///
/// **调用方契约**：取到 `id` 之后**立刻**构造关流守卫（`StreamShared`/`HostStream` 的
/// drop 面）——否则开流成功而后续失败的路径会漏一个流槽（本函数 doc 的 H1 段）。
pub(crate) fn dial_target_raw(
    island: &Arc<Island>,
    dst: SocketAddrV4,
    budget: Duration,
) -> io::Result<(StreamId, Vec<u8>)> {
    let deadline = Instant::now().checked_add(budget);
    let wait = remain(deadline).map_err(stream_err_to_io)?;
    let id = island_stream_cmd(
        island,
        |reply| Cmd::StreamOpen {
            tag: StreamTag::Dial,
            reply,
        },
        Some(wait),
    )
    .map_err(stream_err_to_io)?;
    write_dial_frame_raw(island, id, dst)?;
    let rest = read_dial_ack(deadline, |wait| read_block(island, id, Some(wait)))?;
    Ok((id, rest))
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
    dial_with(logf, tag, budget, |b| open_stream(island, tag, b))
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

/// ③ 写 6B 目标帧（§2.2：`n == 6` 则成，否则 [`io::ErrorKind::WriteZero`]）。
fn write_dial_frame_raw(island: &Arc<Island>, id: StreamId, dst: SocketAddrV4) -> io::Result<()> {
    let frame = homeway_quic::stream::dial_target(dst);
    let n = match write_block(island, id, frame.to_vec()) {
        Ok(w) => w.n,
        Err(e) => return Err(stream_err_to_io(e)),
    };
    if n != frame.len() {
        return Err(io::Error::new(
            io::ErrorKind::WriteZero,
            format!("QUIC 服务流：dial 目标帧只接纳 {n}/{}B", frame.len()),
        ));
    }
    Ok(())
}

/// 连续空块上限（**纯防御**，正常形态不可达）：岛侧若持续回 0 长块，下面的续读环否则永不收敛
/// （设计只说「预算内续读」；`remain(None)` 的天文配置形态下更无全局时限）。32 = 远大于任何
/// 分帧边界能产生的空块数，到点按**协议面无进展**收（`InvalidData` ⇒ pf 链按拨号失败处置）。
const MAX_EMPTY_ACK_CHUNKS: u32 = 32;

/// ④ 读 1B 回执（值空间穷举见 §2.2 的 ④ 表；返回**回执之后的余量**）。
///
/// `read` 注入成闭包（生产 = `Cmd::StreamRead` 的有界等待；单测注入假块）——值空间的每一行
/// 都要有用例，而真块时序（空块 / 首块 > 1B / 对端 FIN）在真岛上不可控。
fn read_dial_ack<F>(deadline: Option<Instant>, mut read: F) -> io::Result<Vec<u8>>
where
    F: FnMut(Duration) -> Result<Vec<u8>, StreamErr>,
{
    let mut empty_chunks: u32 = 0;
    loop {
        let wait = remain(deadline).map_err(stream_err_to_io)?;
        match read(wait) {
            // 空块（0B 的 STREAM 帧）：**不是**结束形态 ⇒ 预算内续读（不得 `[0]` 索引 ⇒ panic）；
            // 连续空块有界（[`MAX_EMPTY_ACK_CHUNKS`]）
            Ok(chunk) if chunk.is_empty() => {
                empty_chunks += 1;
                if empty_chunks > MAX_EMPTY_ACK_CHUNKS {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "QUIC 服务流：dial 回执连续 {MAX_EMPTY_ACK_CHUNKS} 个空块（协议面无进展）"
                        ),
                    ));
                }
                continue;
            }
            // 成功：首字节 = DIAL_OK，其后即目标侧裸字节（**余量必须带回**，§1.2 铁律）
            Ok(chunk) if chunk[0] == DIAL_OK => return Ok(chunk[1..].to_vec()),
            // 首字节非法 = 协议面错（**不得**当 EOF/成功）
            Ok(chunk) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("QUIC 服务流：dial 回执首字节非法（{:#04x}）", chunk[0]),
                ))
            }
            // `Closed`（对端 FIN / 白名单外复位码 / `ConnectionLost`）**显式归拨号失败**
            // （`stream_err_to_io(Closed)` = `Other` ⇒ pf 链 fails+1 + RST）
            Err(e) => return Err(stream_err_to_io(e)),
        }
    }
}

/// QUIC 档的一条服务流连接（`BridgeStream`；应用层帧逐字节不变）。
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
    let id = dial_with(&run.logf, tag, budget, |b| open_stream(&island, tag, b))
        .map_err(stream_err_to_io)?;
    Ok(Box::new(QuicStream::new(island, id)))
}

/// `StreamErr` → `io::Error`（§1.6 的**消费点迁移表**；桥宿主的 refused 判定只认 kind）。
///
/// - `NotSupported`（`0x22`=出口无该服务）⇒ `ConnectionRefused`：保持今天「出口活着、
///   端口没服务 ⇒ 客户端零字节 EOF ⇒ not_supported」的既有判定链（`bridge_host::is_refused_like`、
///   App 测速桥的 `link_down` 分流）；
/// - **新分支**（§1.6 要求有用例）：`Busy`/`Unbound`/`BadTag` ⇒ `ErrorKind::Other`；
/// - `Timeout` ⇒ `TimedOut`（与 `conn_err_to_io` 同款）；
/// - `Closed`/`ConnectionLost` ⇒ `Other`（与今天 `ConnErr::{Closed,EngineGone}` 的落点同形）。
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

/// 流句柄的共享体（两半各持一份 `Arc`；**Drop = 关流**——防半开连接累积，与
/// `tun_exec::SharedConn` 同款且同预算）。
struct StreamShared {
    island: Arc<Island>,
    id: StreamId,
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
pub(crate) struct QuicStream {
    shared: Arc<StreamShared>,
    /// 交付前**已在手**的余量（dial 缝的 1B 回执之后可能立刻跟目标字节 ⇒ 首块
    /// `[0x01, payload…]`；余量丢失 = **应用层帧错位**，§1.2 铁律）。开流时为空。
    pending: Vec<u8>,
}

impl QuicStream {
    fn new(island: Arc<Island>, id: StreamId) -> Self {
        QuicStream::from_shared(Arc::new(StreamShared { island, id }))
    }

    /// 从既有共享体构造（dial 缝用它把已持的 RAII 守卫交给流本体）。
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
#[derive(Default)]
struct PendingBuf {
    buf: Vec<u8>,
    off: usize,
}

impl PendingBuf {
    fn new(buf: Vec<u8>) -> Self {
        PendingBuf { buf, off: 0 }
    }

    /// 取一段（`None` = 余量已空 ⇒ 调用方转 `Cmd::StreamRead`）。
    fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.off >= self.buf.len() {
            return None;
        }
        let n = (self.buf.len() - self.off).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.off..self.off + n]);
        self.off += n;
        Some(n)
    }
}

impl Read for QuicReadHalf {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(n) = self.pending.take(out) {
            return Ok(n);
        }
        let id = self.shared.id;
        match island_stream_cmd(&self.shared.island, |reply| Cmd::StreamRead { id, reply }, None) {
            Ok(data) => {
                let n = data.len().min(out.len());
                out[..n].copy_from_slice(&data[..n]);
                self.pending = PendingBuf::new(data);
                self.pending.off = n;
                Ok(n)
            }
            // 对端 FIN / 白名单外的复位 / 本端关流 ⇒ EOF（§1.6 的 EOF 同形性）
            Err(StreamErr::Closed) => Ok(0),
            Err(e) => Err(stream_err_to_io(e)),
        }
    }
}

/// 写半：`Cmd::StreamWrite`（**非阻塞**：`n` 由待发队列余量给出）；`n=0` 走 §1.4 的
/// Ok(0) 分级退避环（**不得**按 `io::Write` 的「`Ok(0)` = 通道关」处理——那是本仓
/// 被真机打过的坑）。
struct QuicWriteHalf {
    shared: Arc<StreamShared>,
}

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
                Err(e) => return Err(stream_err_to_io(e)),
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

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
        use super::super::bridge_host::{port as bport, term_port};
        assert_eq!(tag_for_port(bport::FILES), Some(StreamTag::Files));
        assert_eq!(tag_for_port(bport::SPEEDTEST), Some(StreamTag::Speedtest));
        // term 端口是**可配面**（HOMEWAY_TERM_PORT）——映射随 `term_port()` 走，
        // 不写死字面量（写死 = 改端口即拨错服务）
        assert_eq!(tag_for_port(term_port()), Some(StreamTag::Term));
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
                });
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
                });
            assert_eq!(r, Err(e));
            assert_eq!(n.load(Ordering::SeqCst), 2, "{e:?} 须重试一次");
        }
        // ③ 首试成功 ⇒ 只尝试一次（策略与成功值无关：用 () 型注入，不依赖岛内类型构造面）
        let n = AtomicUsize::new(0);
        let r: Result<u8, StreamErr> =
            dial_with(&quiet(), StreamTag::Files, Duration::from_secs(15), |_b| {
                n.fetch_add(1, Ordering::SeqCst);
                Ok(7)
            });
        assert_eq!(r, Ok(7), "首试成功必须直接返回");
        assert_eq!(n.load(Ordering::SeqCst), 1);
        // ④ 预算已耗尽（budget=0）⇒ 连接面失败也不重试（不越界）
        let n = AtomicUsize::new(0);
        let r: Result<u8, StreamErr> =
            dial_with(&quiet(), StreamTag::Files, Duration::ZERO, |_b| {
                n.fetch_add(1, Ordering::SeqCst);
                Err(StreamErr::Closed)
            });
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

    /// **判据（dial 回执值空间穷举，§2.2 的 ④ 表逐行）**：八行各有断言——
    /// `[0x01,rest]` / 空块续读 / 首字节非 0x01 / `Busy` / `Refused` / `Timeout` /
    /// `Closed`（**显式归拨号失败，不得读成 EOF/成功**）/ `ConnectionLost`；另加
    /// `NotSupported/Unbound/BadTag` 的防御面。
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

        // ③ 首字节非 0x01 ⇒ InvalidData（协议面错）
        let e = read_dial_ack(far, |_w| Ok(vec![0x22, 1, 2])).expect_err("非法首字节");
        assert_eq!(e.kind(), io::ErrorKind::InvalidData, "{e:?}");
        assert!(e.to_string().contains("dial 回执首字节非法"), "{e}");

        // ④ 岛内快速失败 / 出口复位码 / 空面：逐行映射
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
            assert_eq!(e.kind(), kind, "{se:?} 的 io kind");
        }
        // ⑤ `Closed`（对端 FIN / 白名单外复位 / ConnectionLost）⇒ **显式拨号失败**
        //    （**不得**沿读半的 `Closed ⇒ Ok(0)` 读成「成功无回执」）
        let e = read_dial_ack(far, |_w| Err(StreamErr::Closed)).expect_err("Closed 必须是失败");
        assert_eq!(e.kind(), io::ErrorKind::Other);
        assert!(
            e.to_string().contains("服务流已关闭"),
            "Closed 的归因串：{e}"
        );

        // ⑥ 预算耗尽（deadline 已过）⇒ 不再调 read，直接 TimedOut
        let past = Some(Instant::now() - Duration::from_millis(1));
        let e = read_dial_ack(past, never).expect_err("预算耗尽");
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e:?}");
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
}
