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
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

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
fn open_stream(island: &Island, tag: StreamTag, budget: Duration) -> Result<StreamId, StreamErr> {
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
fn dial_with<F, T>(
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
}

impl QuicStream {
    fn new(island: Arc<Island>, id: StreamId) -> Self {
        QuicStream {
            shared: Arc::new(StreamShared { island, id }),
        }
    }
}

impl BridgeStream for QuicStream {
    fn into_halves(
        self: Box<Self>,
    ) -> io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)> {
        Ok((
            Box::new(QuicReadHalf {
                shared: Arc::clone(&self.shared),
                buf: Vec::new(),
                off: 0,
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
    buf: Vec<u8>,
    off: usize,
}

impl Read for QuicReadHalf {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.off < self.buf.len() {
            let n = (self.buf.len() - self.off).min(out.len());
            out[..n].copy_from_slice(&self.buf[self.off..self.off + n]);
            self.off += n;
            return Ok(n);
        }
        let id = self.shared.id;
        match island_stream_cmd(&self.shared.island, |reply| Cmd::StreamRead { id, reply }, None) {
            Ok(data) => {
                let n = data.len().min(out.len());
                out[..n].copy_from_slice(&data[..n]);
                self.buf = data;
                self.off = n;
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
}
