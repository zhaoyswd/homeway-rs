//! 出口侧**服务流受理**（M3 §1.1/§1.2/§1.6/§2.1 的出口面）。
//!
//! **本文件属异步面**（隔离门 ② 条的 `ASYNC_FILES` 显式清单）。
//!
//! 分发骨架（§2.1；每流一枚任务、**不与 accept 串行**）：
//!
//! ```text
//! accept_bi() ─► 每流一个 task
//!                 │  读 1B tag（预算 TAG_READ_BUDGET=5s；到点 ⇒ reset(0x27)）
//!                 ├─ 未绑定连接 ⇒ reset(0x24) + 行 + 计数
//!                 ├─ tag ∉ {1..5} ⇒ reset(0x21) + 行 + 计数
//!                 ├─ 服务未启用（无 intake）⇒ reset(0x22) + 行 + 计数
//!                 ├─ 入口队列满 ⇒ reset(0x23) + 行 + 计数
//!                 └─ tag 1/2/3 ⇒ socketpair + 入队 + 泵（[`super::pump`]）
//!                    tag 4（dial）⇒ 真拨号腿（[`super::dial`]；0x25/0x26 双码）
//!                    tag 5（probe）⇒ 回显任务直连（**不进队列**，不占服务资源）
//! ```
//!
//! 「应用层零改动」（§1.2）的落点：三个服务收到的是**一条 UnixStream**（socketpair 服务侧
//! 端）——帧层 / per-syscall 期限 / `poll(2)` 语义 / `try_clone` / `shutdown_both` / busy
//! 路径一行不改；本文件只做「tag → 服务入口」的搬运。
//!
//! **0x24（未绑定）的可达性（如实登记）**：受理循环由控制流任务在**准入通过之后**启动
//! （`conn::control` 的 `join!`），故「准入前的服务流」在本循环里不可表达；但**准入后绑定
//! 被摘除**（设备被摘除/轮换：`ExitQuic::unbind_pub` 先摘绑定再关连接）与受理之间存在真
//! 竞态窗 ⇒ 本臂是那条窗的防线（拒绝 + 行 + 计数），不是死码。
//! **0x27（tag 读取超时）的可达性**：QUIC 的流开通 = 对端发出的**首个 STREAM 帧**
//! （`open_bi()` 本身不通知对端）⇒ 1B 的 tag 不存在「部分到达」形态，本臂在正常客户端下
//! **不可触发**（只有首字节在链路上被延迟 > `TAG_READ_BUDGET` 才命中，如长时间丢包/手写
//! 对端）。保留理由：①§1.6 的码表要求「不给对端留无限期槽位」的防线存在；②对端读侧的
//! `0x27 ⇒ StreamErr::BadTag` 分类是**可构造可测**的（见 `crate::stream` 的单测）。

use std::sync::Arc;
use std::time::Instant;

use quinn::{ReadExactError, RecvStream, SendStream, VarInt};
use tokio::task::JoinSet;

use crate::stream::{reset, StreamTag, TAG_READ_BUDGET};
use crate::tuning::service_defaults;

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
    drop(tasks); // 在途泵/回显任务随连接一起收（`JoinSet` drop = abort；socketpair 随任务 drop
                 // 关闭 ⇒ 服务侧会话看到 EOF/错误，与今天 UDS 形态同款）
}

/// 单条服务流：读 tag（预算内）⇒ 受理前置 ⇒ 分发。
async fn handle_stream(mut send: SendStream, mut recv: RecvStream, conn_id: u64, ctx: Arc<FaceCtx>) {
    let mut tag_byte = [0u8; 1];
    let tag = match tokio::time::timeout(TAG_READ_BUDGET, recv.read_exact(&mut tag_byte)).await {
        // 到点：**reset 而非 drop**（未 reset 的流会让对端的并发额度不归还）
        Err(_elapsed) => {
            let _ = send.reset(varint(reset::TAG_READ_TIMEOUT));
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
        let _ = send.reset(varint(reset::TAG_UNKNOWN));
        refuse(&ctx, conn_id, None, reset::TAG_UNKNOWN, "未知 tag");
        return;
    };
    // ---- 受理前置：服务流只在**已绑定**连接上受理（§1.1；未绑定 ⇒ §1.6 的 0x24）----
    if ctx.bridge.dev_of_conn(conn_id).is_none() {
        let _ = send.reset(varint(reset::UNBOUND));
        refuse(&ctx, conn_id, Some(tag), reset::UNBOUND, "连接未绑定");
        return;
    }
    match tag {
        StreamTag::Probe => echo(send, recv, conn_id, &ctx).await,
        // M4 S1 换轨：这条臂换成真拨号腿（`exit/dial.rs`）——读 6B `[4B IPv4][2B BE port]`
        // ⇒ 地址类判定 ⇒ 拨号 ⇒ 1B 回执 ⇒ 泛型泵。**帧不改**（M3 定稿沿用；§1.1）。
        StreamTag::Dial => super::dial::dial_serve(send, recv, conn_id, &ctx).await,
        StreamTag::Files | StreamTag::Term | StreamTag::Speedtest => {
            serve_via_intake(send, recv, tag, conn_id, &ctx).await
        }
    }
}

/// tag 1/2/3：socketpair + 入队（服务入口）+ 泵。
///
/// 未启用（`intakes.slot(tag) == None`：出口 QUIC 面没拿到该服务的 intake——监听失败 /
/// `HOMEWAY_TERM=off`）⇒ `0x22`（§1.6 设计门 N16 的**保守选择**：UDS bind 失败 ⇒ 该服务
/// QUIC 腿一并停用，与今天「整服务不可用」同形）。
async fn serve_via_intake(
    mut send: SendStream,
    recv: RecvStream,
    tag: StreamTag,
    conn_id: u64,
    ctx: &Arc<FaceCtx>,
) {
    let Some(tx) = ctx.intakes.slot(tag) else {
        let _ = send.reset(varint(reset::SERVICE_DISABLED));
        refuse(ctx, conn_id, Some(tag), reset::SERVICE_DISABLED, "服务不可用（未启用）");
        return;
    };
    // 服务侧端（交 intake）/ 泵侧端（本端）；两向缓冲 = §2.2 的显式 64 KiB
    let (svc_end, pump_end) = match super::pump::socketpair(service_defaults::SOCKPAIR_BYTES) {
        Ok(v) => v,
        // 本机资源面失败（fd/内存）：没有对应的复位码表条目 ⇒ 归「服务不可用」，
        // 但行文要能区分（排障时「0x22 但原因不同」是两回事）
        Err(e) => {
            let _ = send.reset(varint(reset::SERVICE_DISABLED));
            refuse(
                ctx,
                conn_id,
                Some(tag),
                reset::SERVICE_DISABLED,
                &format!("服务不可用（socketpair 建不起来：{e}）"),
            );
            return;
        }
    };
    if let Err(full) = tx.try_enqueue(svc_end) {
        // 入口队列满（§1.7：容量 = 在册上限 + K）⇒ 0x23 + 行（带当刻 n/cap）
        let _ = send.reset(varint(reset::INTAKE_FULL));
        refuse(
            ctx,
            conn_id,
            Some(tag),
            reset::INTAKE_FULL,
            &format!("入口队列满 {}/{}", full.queued, full.capacity),
        );
        return;
    }
    // 受理计数 + 行（E-q5；**入队即受理**——此后服务自身的在册闸可能回应用层 busy，
    // 那是服务语义不是出口拒入，故不再计拒）
    let n = ctx.stats.streams_open.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    if log_due(n) {
        (*ctx.logf)(&format!(
            "quic: 服务流已受理（tag={tag} dev={} 第 {n} 次）",
            dev_of(ctx, conn_id)
        ));
    }
    super::pump::run(send, recv, pump_end, tag, conn_id, Arc::clone(ctx)).await;
}

/// `probe` 回显（§1.2：**原样回显直到客户端半关**）。
///
/// 用 `tokio::io::copy`（quinn 的 `RecvStream: AsyncRead` / `SendStream: AsyncWrite`）：
/// 「读多少写多少，读到 FIN 即 `finish`」正是半关语义的原生表达；逐块手写循环等价但更易错。
/// 失败（对端 reset/连接死）按结束收——回显流没有协议错误面。
/// **不进 intake 队列**（§2.1：不占服务资源），但同样计入每连接流额度与受理计数。
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
        .stream_bytes_in
        .fetch_add(bytes, std::sync::atomic::Ordering::SeqCst);
    ctx.stats
        .stream_bytes_out
        .fetch_add(bytes, std::sync::atomic::Ordering::SeqCst);
    ctx.stats
        .streams_closed
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
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
///
/// `pub(super)`：dial 腿（`exit/dial.rs`）复用同一枚计数/行文实现（**拒绝行的单源**——
/// 两处各写一份 `format!` 就是「行文与码各写一份」的漂移入口）。
pub(super) fn refuse(ctx: &Arc<FaceCtx>, conn_id: u64, tag: Option<StreamTag>, code: u64, why: &str) {
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

/// 复位码 → `VarInt`（码表恒在 `VarInt` 值域内 ⇒ `expect` 是常量断言，不是运行期风险）。
///
/// `pub(super)`：dial 腿（`exit/dial.rs`）写 `0x25/0x26` 时复用（同 `refuse` 的理由）。
pub(super) fn varint(code: u64) -> VarInt {
    VarInt::try_from(code).expect("复位码在 VarInt 值域内")
}

/// 连接的设备短指纹（已绑定 ⇒ 8B devTag 的 4B hex；未绑定/未知 ⇒ `—`）。
pub(crate) fn dev_of(ctx: &Arc<FaceCtx>, conn_id: u64) -> String {
    match ctx.bridge.dev_of_conn(conn_id) {
        Some(d) => super::bridge::dev_short(&d),
        None => "—".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 复位码与行文的关系（本文件 + `exit/dial.rs` 一起产出全部七码；改表即红——防「拒了
    /// 但说不清」）。
    #[test]
    fn serve_only_emits_the_designed_codes() {
        assert_eq!(reset::TAG_UNKNOWN, 0x21);
        assert_eq!(reset::SERVICE_DISABLED, 0x22);
        assert_eq!(reset::INTAKE_FULL, 0x23);
        assert_eq!(reset::UNBOUND, 0x24);
        assert_eq!(reset::DIAL_REFUSED, 0x25);
        assert_eq!(reset::DIAL_TIMEOUT, 0x26);
        assert_eq!(reset::TAG_READ_TIMEOUT, 0x27);
        // 白名单映射（对端读侧）与出口写侧同表：本文件（0x21–0x24/0x27）与 dial 腿
        // （0x25/0x26）写出的七个码都能被客户端识别成 typed 错误。
        for code in [
            reset::TAG_UNKNOWN,
            reset::SERVICE_DISABLED,
            reset::INTAKE_FULL,
            reset::UNBOUND,
            reset::DIAL_REFUSED,
            reset::DIAL_TIMEOUT,
            reset::TAG_READ_TIMEOUT,
        ] {
            assert!(
                crate::stream::StreamErr::from_reset_code(code).is_some(),
                "{code:#x} 必须在对端白名单里"
            );
        }
        // M4 起**两码真产出**（`exit/dial.rs` 的拒（0x25）/超期（0x26））：白名单区间内每个码
        // 都必须有写侧归属——区间里出现「无人写」的码 = 码表与实现漂移。
        for code in reset::WHITELIST_MIN..=reset::WHITELIST_MAX {
            assert!(
                crate::stream::StreamErr::from_reset_code(code).is_some(),
                "{code:#x} 在白名单区间内但无映射"
            );
        }
    }

    /// 服务入口容量 = 在册上限 + K（§1.7 设计门 2-5 的构造性保证）——三个服务的实例值
    /// 由调用侧（`homeway-core` 装配点）按本常量算出，本断言钉住它不被改窄成「= 在册上限」。
    #[test]
    fn intake_capacity_keeps_the_busy_path_reachable() {
        assert!(service_defaults::intake_capacity(16) > 16, "必须在册上限之上留 K");
        assert_eq!(service_defaults::intake_capacity(0), 4, "K 单独成立");
    }
}
