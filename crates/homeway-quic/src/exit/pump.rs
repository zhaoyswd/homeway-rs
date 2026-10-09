//! 出口服务流的 **socketpair 适配器 + 异步泵**（M3 §2.2 方案 B′ 的异步侧半边）。
//!
//! 形态（设计 §2.2 的定稿）：
//!
//! ```text
//! QUIC 流 ──(tag 分发)──► [_socketpair：服务侧端 → ServiceIntake]
//!                          └─ 本模块：异步泵 ⇄ QUIC 两侧半边
//! ```
//!
//! - **半关传播**（§1.3/§1.5）：客户端 FIN ⇒ 服务侧 `shutdown(WRITE)`；
//!   服务侧收线（EOF）⇒ 对端 `finish()`（**不用 reset 做正常收口**）——与
//!   `tokio::io::copy_bidirectional` 同义（一个方向收线不打断另一个方向）。
//! - **复位**（§2.2 设计门 N12）：AF_UNIX 没有 TCP 的「`SO_LINGER=0` ⇒ RST」语义，
//!   且今天出口侧本来就是普通 close ⇒ **`reset(code)` = 关 socketpair**；错误码只留在
//!   出口行/计数里，**不跨 socketpair**。服务侧先收线时同样走 FIN。
//!   **实装口径（代码门 r18 C3 订正）**：对端 reset/连接死时，泵侧做的是 `shutdown(SHUT_WR)`
//!   （`upstream` 的 `Err` 分支）——**效果**是「服务侧不再永久阻塞在 read 上」（它见到 EOF），
//!   与「普通 close」在**服务侧可观测面**等价（本轮没有 RST 语义）；两者不等价之处只在
//!   读半边仍可由下行任务持有（故不是整条 fd 的 close）。
//! - **背压**：socketpair 内核缓冲（显式 [`crate::tuning::service_defaults::SOCKPAIR_BYTES`]
//!   /方向）+ QUIC 流窗口 = 端到端真背压；服务侧的阻塞读写语义与今天 UDS 逐字相同。
//! - **不新增线程**：泵是出口 `current_thread` runtime 上的一枚任务（§2.2 的准确说法：
//!   服务自身的 handler 线程不变，泵只借岛 runtime）。
//!
//! **M4 §3.3 泛型化**：[`upstream`]/[`downstream`] 由「`UnixStream` 的两半」放宽为任意
//! `AsyncWrite`/`AsyncRead` ⇒ **dial 腿（`exit/dial.rs`）复用同一份实现**（以 `TcpStream`
//! 的两半为参数），本仓不写第四份泵拷贝（Q-F-B 残余③「三份泵拷贝」只减不增）。
//!
//! **本文件属异步面**（隔离门 ② 条的 `ASYNC_FILES` 显式清单；照 `exit/serve.rs` 先例）。

use std::io;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::Arc;
use std::time::Instant;

use quinn::{RecvStream, SendStream};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{log_due, FaceCtx};
use crate::stream::StreamTag;

/// socketpair 缓冲的**设置**（`SO_SNDBUF|SO_RCVBUF`；§2.2 的「显式 64 KiB/方向」）。
///
/// 注（Linux 事实，如实登记）：内核把 `SO_SNDBUF` 的设定值**翻倍**作内部记账（`getsockopt`
/// 读回 2×）；设置面仍是「每方向 64 KiB」这一设计值，`sum .so`/内存账按设置值记。
fn set_sockbuf(sock: &StdUnixStream, bytes: usize) -> io::Result<()> {
    let v = libc::c_int::try_from(bytes).unwrap_or(libc::c_int::MAX);
    for opt in [libc::SO_SNDBUF, libc::SO_RCVBUF] {
        // SAFETY：`setsockopt` 只读 `v` 的 4 字节；失败原样上抛（调用方按「起不来」处置）。
        let r = unsafe {
            libc::setsockopt(
                std::os::fd::AsRawFd::as_raw_fd(sock),
                libc::SOL_SOCKET,
                opt,
                std::ptr::addr_of!(v).cast(),
                std::mem::size_of_val(&v) as libc::socklen_t,
            )
        };
        if r != 0 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 建一对 socketpair：返回（**服务侧端**，**泵侧端**）。
///
/// 两端都置非阻塞：服务侧端由 [`crate::exit::intake::ServiceIntake::accept`] 交出去之前
/// 会置回阻塞（服务本体的语义要阻塞 fd），泵侧端直接交 `tokio::net::UnixStream`。
pub(crate) fn socketpair(bytes: usize) -> io::Result<(StdUnixStream, StdUnixStream)> {
    let (svc, pump) = StdUnixStream::pair()?;
    for s in [&svc, &pump] {
        set_sockbuf(s, bytes)?;
        s.set_nonblocking(true)?;
    }
    Ok((svc, pump))
}

/// 泵：QUIC 流 ⇄ socketpair（半关传播 + 逐向计数）。
///
/// `sock` = 泵侧端（非阻塞；`from_std` 的前置）。收工时**两侧都收**（一个方向收线不打断
/// 另一个方向——文件服务的「写关流即提交」依赖单向半关）。
pub(crate) async fn run(
    mut send: SendStream,
    mut recv: RecvStream,
    sock: StdUnixStream,
    tag: StreamTag,
    conn_id: u64,
    ctx: Arc<FaceCtx>,
) {
    let t0 = Instant::now();
    let io = match tokio::net::UnixStream::from_std(sock) {
        Ok(io) => io,
        Err(e) => {
            (*ctx.logf)(&format!(
                "quic: 服务流泵起不来（tag={tag} dev={}：socketpair 进 runtime 失败 {e}）——已关流",
                super::serve::dev_of(&ctx, conn_id)
            ));
            let _ = send.finish();
            return;
        }
    };
    let (mut sr, mut sw) = tokio::io::split(io);
    let (up, down) = tokio::join!(
        upstream(&mut recv, &mut sw),
        downstream(&mut sr, &mut send),
    );
    // 计数与结束行（受理行在入队时已落——见 `serve.rs`）
    ctx.stats
        .stream_bytes_in
        .fetch_add(up.bytes, std::sync::atomic::Ordering::SeqCst);
    ctx.stats
        .stream_bytes_out
        .fetch_add(down.bytes, std::sync::atomic::Ordering::SeqCst);
    let n = ctx
        .stats
        .streams_closed
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 服务流结束（tag={tag}，↑{}B ↓{}B，耗时 {:?}）",
            up.bytes,
            down.bytes,
            t0.elapsed()
        ));
    }
}

/// 一个方向的搬运结果（M4 §3.3：dial 腿的「目标侧中断」行需要**结束原因**）。
///
/// `bytes` = 本方向搬运字节数；`err` = **对端侧**异常结束的错误原文（`None` = 正常 FIN/EOF
/// 或「本侧已收摊」）。socketpair 腿（[`run`]）**不读** `err`——它今天就是对 AF_UNIX 的
/// 读错误静默 `finish()`（行为逐字保留）；dial 腿（`exit/dial.rs`）用它产出口可观测行。
pub(crate) struct CopyEnd {
    pub(crate) bytes: u64,
    pub(crate) err: Option<io::Error>,
}

/// 上行（客户端 → 服务/目标）：QUIC 读 → 对端写。
///
/// - 对端 FIN ⇒ `shutdown(WRITE)`（**半关传播**：服务侧读到 EOF 而不是错误）；
/// - 对端 reset / 连接死 ⇒ 关 socketpair（§2.2-N12：普通 close，错误码不跨 socketpair）；
/// - 对端侧已收线（写失败 `EPIPE`）⇒ 结束本方向（`err` 带回原文；socketpair 腿不用它，
///   dial 腿落「目标侧中断」行），下行任务照常收尾。
///
/// **泛型化（M4 §3.3）**：`sw` 由 `WriteHalf<UnixStream>` 放宽为任意 `AsyncWrite + Unpin`
/// ——dial 腿以 `tokio::net::TcpStream` 的写半调用**同一份实现**（不写第四份泵拷贝）。
///
/// 返回本方向的搬运量与结束原因（见 [`CopyEnd`]）。
pub(crate) async fn upstream<W: AsyncWrite + Unpin>(recv: &mut RecvStream, sw: &mut W) -> CopyEnd {
    let mut buf = vec![0u8; COPY_BUF];
    let mut n = 0u64;
    loop {
        match recv.read(&mut buf).await {
            // 空块（0B 的 STREAM 帧）不是结束形态（quinn 只在 FIN/复位时给 None/Err）
            Ok(Some(0)) => continue,
            Ok(Some(k)) => {
                n += k as u64;
                if let Err(e) = sw.write_all(&buf[..k]).await {
                    // 对端侧没了（EPIPE/ECONNRESET）：本方向收摊
                    return CopyEnd { bytes: n, err: Some(e) };
                }
            }
            Ok(None) => {
                let _ = sw.shutdown().await; // 客户端 FIN ⇒ 对端见 EOF（§1.3 半关）
                return CopyEnd { bytes: n, err: None };
            }
            Err(_) => {
                // 对端 reset / 连接死：**关服务侧的写向**（§2.2-N12 的「普通 close」在泵侧
                // 的落点）——否则服务侧会**永远阻塞在 read 上**（它无从知道客户端已经走了；
                // 今天 UDS 形态下这个信号由 close 给出）。只 `shutdown(WRITE)` 不 drop：
                // 读半边仍归下行任务（服务收线那半仍要传播）。
                let _ = sw.shutdown().await;
                return CopyEnd { bytes: n, err: None };
            }
        }
    }
}

/// 下行（服务/目标 → 客户端）：对端读 → QUIC 写。
///
/// 对端侧 EOF ⇒ `finish()`：**不用 reset 做正常收口**（§1.3）；今天 UDS 形态下出口侧同样
/// 是普通 close。对端**读错误**（AF_UNIX「对端带未读数据关闭」/ TCP RST）同样 `finish()`
/// 收口，但把原文带回 `err`（dial 腿落「目标侧中断」行；socketpair 腿不读它 ⇒ 行为不变）。
///
/// 泛型化同 [`upstream`]（`sr` = `TcpStream` 读半）。
///
/// 返回本方向的搬运量与结束原因（见 [`CopyEnd`]）。
pub(crate) async fn downstream<R: AsyncRead + Unpin>(sr: &mut R, send: &mut SendStream) -> CopyEnd {
    let mut buf = vec![0u8; COPY_BUF];
    let mut n = 0u64;
    loop {
        match sr.read(&mut buf).await {
            Ok(0) => {
                let _ = send.finish();
                return CopyEnd { bytes: n, err: None };
            }
            Ok(k) => {
                n += k as u64;
                if send.write_all(&buf[..k]).await.is_err() {
                    return CopyEnd { bytes: n, err: None }; // 客户端已 reset/连接死
                }
            }
            Err(e) => {
                let _ = send.finish(); // 对端收线（含 ECONNRESET 一档）⇒ 普通 FIN
                return CopyEnd { bytes: n, err: Some(e) };
            }
        }
    }
}

/// 两个方向的搬运块（8 KiB：与 `tokio::io::copy` 的默认块同量级，平衡 syscall 次数与内存）。
const COPY_BUF: usize = 8 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（socketpair 两向缓冲 = 设计值 64 KiB）**：
    ///
    /// 平台事实（本机实测，写死在此防误判）：
    /// - **Linux**（产品平台）：内核把 `SO_SNDBUF/SO_RCVBUF` 的设定值翻倍记账 ⇒ 读回 ≥ 设定值；
    /// - **darwin**（开发台）：`SO_RCVBUF` 读回 = 设定值；AF_UNIX 的 `SO_SNDBUF` 被内核夹到
    ///   系统上限（实测读回 4097，设定 64 KiB 不生效）⇒ 只断言「读回 > 0」。
    ///
    /// 两平台共同断言 = `SO_RCVBUF ≥ 设定值`（内存账的承重方向：接收窗决定在途上限）。
    #[test]
    fn socketpair_buffers_are_set_to_the_designed_value() {
        let bytes = crate::tuning::service_defaults::SOCKPAIR_BYTES;
        let (a, b) = socketpair(bytes).expect("建 socketpair");
        let get = |s: &StdUnixStream, opt: libc::c_int| -> usize {
            let mut v: libc::c_int = 0;
            let mut len = std::mem::size_of_val(&v) as libc::socklen_t;
            let r = unsafe {
                libc::getsockopt(
                    std::os::fd::AsRawFd::as_raw_fd(s),
                    libc::SOL_SOCKET,
                    opt,
                    std::ptr::addr_of_mut!(v).cast(),
                    &mut len,
                )
            };
            assert_eq!(r, 0, "getsockopt");
            v as usize
        };
        for s in [&a, &b] {
            let rcv = get(s, libc::SO_RCVBUF);
            assert!(rcv >= bytes, "SO_RCVBUF 未按设计值设置：读回 {rcv} < {bytes}");
            let snd = get(s, libc::SO_SNDBUF);
            if cfg!(target_os = "linux") {
                assert!(snd >= bytes, "SO_SNDBUF 未按设计值设置：读回 {snd} < {bytes}");
            } else {
                assert!(snd > 0, "SO_SNDBUF 读回 {snd}");
            }
        }
    }
}
