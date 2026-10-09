//! 出口侧**服务流受理**（M3 §1.1/§1.2/§1.6 的出口面）。
//!
//! **本文件属异步面**（隔离门 ② 条的 `ASYNC_FILES` 显式清单）。
//!
//! 本期落地范围（逐条对齐 §11 的切片边界）：
//! - **`probe`（tag=5）真回显**（§1.2 末行：客户端写 N(≥1)B ⇒ 出口原样回显直到客户端
//!   半关）。它与岛侧「`Cmd::Probe` 换 STREAM 回显 + `uni=0`」**必须同切片落地**
//!   （§1.7-N13）——否则中间切片上巡检定音失效；本文件即那个「另一半」；
//! - **tag 1–4**：`reset(0x22)`（服务不可用）+ 行 + 计数。**S2 接线前的事实**：QUIC 档
//!   确实还不提供这四个服务（它们仍走本机 UDS），故 `0x22` 是语义正确的暂态；S2 把
//!   这四条臂换成 `ServiceIntake` 入队 + 泵即可（**本文件就是 S2 的 accept 骨架**）；
//! - **非法 tag** ⇒ `reset(0x21)`；**读了 TAG_READ_BUDGET 仍未读到 tag** ⇒ `reset(0x27)`
//!   （**reset 而非 drop**：quinn 的未 accept/未 reset 流不归还并发额度）。
//!
//! 未落地（**S2**，如实登记）：`0x24`（未绑定连接上的服务流——准入协议把「首条 bidi
//! 流」固定为控制流，故「准入前的服务流」在本循环里不可表达）、intake 容量/公平性、
//! socketpair 泵、应用层 busy 路径。本文件不改这三条的语义。

use std::sync::Arc;
use std::time::Instant;

use quinn::{ReadExactError, RecvStream, SendStream, VarInt};
use tokio::task::JoinSet;

use crate::stream::{reset, StreamTag, TAG_READ_BUDGET};

use super::{log_due, FaceCtx};

/// 每连接**服务流受理循环**（准入完成后由主循环起；连接结束即返回）。
///
/// 「每流一枚任务」（§2.1 的骨架：**并发**，不与 accept 串行）——`JoinSet` 托底（连接
/// 结束/收工时 `drop` 一起 abort，**不新增线程**：全部任务跑在出口的 `current_thread`
/// runtime 上）。
pub(crate) async fn serve_streams(conn: quinn::Connection, conn_id: u64, ctx: Arc<FaceCtx>) {
    let mut tasks: JoinSet<()> = JoinSet::new();
    loop {
        tokio::select! {
            // 已结束的流任务先收割（防长连接上条目累积；`tasks` 空时该臂自动关闭）
            Some(_) = tasks.join_next(), if !tasks.is_empty() => {}
            pair = conn.accept_bi() => {
                let Ok((send, recv)) = pair else {
                    break; // 连接结束（对端关/空闲回收/本地关）
                };
                let ctx = Arc::clone(&ctx);
                tasks.spawn(handle_stream(send, recv, conn_id, ctx));
            }
        }
    }
    drop(tasks); // 在途回显任务随连接一起收（`JoinSet` drop = abort）
}

/// 单条服务流：读 tag（预算内）⇒ 分发（本期：probe 回显 / 其余拒）。
///
/// **`0x27`（tag 读取超时）的可达性（如实登记）**：QUIC 的流开通 = 对端发出的**首个
/// STREAM 帧**（`open_bi()` 本身不通知对端）⇒ 1B 的 tag 不存在「部分到达」形态，本臂在
/// 正常客户端下**不可触发**（只有首字节在链路上被延迟 > `TAG_READ_BUDGET` 才命中，如
/// 长时间丢包/手写对端）。保留它的理由：①§1.6 的码表要求「不给对端留无限期槽位」的
/// 防线存在；②对端读侧的 `0x27 ⇒ StreamErr::BadTag` 分类是**可构造可测**的（见
/// `crate::stream` 的单测），协议面闭环不依赖本臂被触发。
/// **对端开流后立刻半关（FIN，无 tag）**：静默收（不计拒、不留槽）——那不是错误形态。
async fn handle_stream(mut send: SendStream, mut recv: RecvStream, conn_id: u64, ctx: Arc<FaceCtx>) {
    let mut tag_byte = [0u8; 1];
    let tag = match tokio::time::timeout(TAG_READ_BUDGET, recv.read_exact(&mut tag_byte)).await {
        // 到点：**reset 而非 drop**（未 reset 的流会让对端的并发额度不归还）
        Err(_elapsed) => {
            let _ = send.reset(VarInt::try_from(reset::TAG_READ_TIMEOUT).expect("复位码在 VarInt 值域内"));
            refuse(&ctx, conn_id, None, reset::TAG_READ_TIMEOUT, "tag 读取超时");
            return;
        }
        // 对端开流即关（0B 的 FIN）：无 tag 可判，按「无意义流」静默收（不计拒）
        Ok(Err(ReadExactError::FinishedEarly(0))) => return,
        // 流被对端 reset / 连接死：同样无可判
        Ok(Err(_)) => return,
        Ok(Ok(())) => tag_byte[0],
    };
    let Some(tag) = StreamTag::from_byte(tag) else {
        let _ = send.reset(VarInt::try_from(reset::TAG_UNKNOWN).expect("复位码在 VarInt 值域内"));
        refuse(&ctx, conn_id, None, reset::TAG_UNKNOWN, "未知 tag");
        return;
    };
    match tag {
        StreamTag::Probe => echo(send, recv, conn_id, &ctx).await,
        // S2 接线前：这四个服务不在 QUIC 档（仍走本机 UDS）⇒ `0x22`（服务不可用），
        // 与 §1.6「files 监听失败 ⇒ 该服务 QUIC 腿一并停用」同一条保守语义（设计门 N16）
        StreamTag::Files | StreamTag::Term | StreamTag::Speedtest | StreamTag::Dial => {
            let _ = send.reset(VarInt::try_from(reset::SERVICE_DISABLED).expect("复位码在 VarInt 值域内"));
            refuse(&ctx, conn_id, Some(tag), reset::SERVICE_DISABLED, "服务不可用（S2 接线前）");
        }
    }
}

/// `probe` 回显（§1.2：**原样回显直到客户端半关**）。
///
/// 用 `tokio::io::copy`（quinn 的 `RecvStream: AsyncRead` / `SendStream: AsyncWrite`）：
/// 「读多少写多少，读到 FIN 即 `finish`」正是半关语义的原生表达；逐块手写循环等价但更易错。
/// 失败（对端 reset/连接死）按结束收——回显流没有协议错误面。
async fn echo(mut send: SendStream, mut recv: RecvStream, conn_id: u64, ctx: &Arc<FaceCtx>) {
    let t0 = Instant::now();
    let n = ctx
        .stats
        .streams_open
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 服务流已受理（tag={} dev={} 第 {n} 次）",
            StreamTag::Probe.text(),
            dev_of(ctx, conn_id)
        ));
    }
    let bytes = tokio::io::copy(&mut recv, &mut send).await.unwrap_or(0);
    let _ = send.finish(); // 客户端半关 ⇒ 我方 FIN（正常收工，**不用 reset**，§1.3）
    ctx.stats
        .stream_bytes
        .fetch_add(bytes, std::sync::atomic::Ordering::SeqCst);
    if log_due(n) {
        // ↑ = 客户端上行（本端读到的量）、↓ = 下发给客户端的量——回显下两者相等
        (*ctx.logf)(&format!(
            "quic: 服务流结束（tag={}，↑{bytes}B ↓{bytes}B，耗时 {:?}）",
            StreamTag::Probe.text(),
            t0.elapsed()
        ));
    }
}

/// 服务流拒绝：计数 + 归因行（E-q5 族，§8.2-7；出口排障的唯一可观测面）。
fn refuse(ctx: &Arc<FaceCtx>, conn_id: u64, tag: Option<StreamTag>, code: u64, why: &str) {
    let n = ctx
        .stats
        .stream_refused
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        + 1;
    if !log_due(n) {
        return;
    }
    let tag_text = tag.map(|t| t.text()).unwrap_or("—");
    (*ctx.logf)(&format!(
        "quic: 服务流拒（dev={} tag={tag_text}；{why}（0x{code:02x}）；第 {n} 次）",
        dev_of(ctx, conn_id)
    ));
}

/// 连接的设备短指纹（已绑定 ⇒ 8B devTag 的 4B hex；未绑定/未知 ⇒ `—`）。
fn dev_of(ctx: &Arc<FaceCtx>, conn_id: u64) -> String {
    match ctx.bridge.dev_of_conn(conn_id) {
        Some(d) => super::bridge::dev_short(&d),
        None => "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复位码与行文的关系（本文件只会产出这三码；改表即红——防「拒了但说不清」）。
    #[test]
    fn serve_only_emits_the_three_designed_codes() {
        assert_eq!(reset::TAG_UNKNOWN, 0x21);
        assert_eq!(reset::SERVICE_DISABLED, 0x22);
        assert_eq!(reset::TAG_READ_TIMEOUT, 0x27);
        // 白名单映射（对端读侧）与出口写侧同表：三个码都能被客户端识别成 typed 错误
        assert!(crate::stream::StreamErr::from_reset_code(reset::TAG_UNKNOWN).is_some());
        assert!(crate::stream::StreamErr::from_reset_code(reset::SERVICE_DISABLED).is_some());
        assert!(crate::stream::StreamErr::from_reset_code(reset::TAG_READ_TIMEOUT).is_some());
    }
}
