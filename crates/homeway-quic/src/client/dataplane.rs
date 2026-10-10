//! 岛数据面（M1 设计 §2.4/§6.1/§6.4）：TUN ⇄ DATAGRAM 的两向投递与**四类丢弃计数**。
//!
//! 上行（TUN → DATAGRAM，本模块 [`send_datagram_checked`]）：
//!
//! ```text
//! ① 准入窗：Live 只在**四帧准入完成**（收到 `A4`）之后才存在 ⇒ 本函数只可能跑在窗后
//!    （窗前的包由宿主按「未登记」丢弃，见 driver 的 TunPacket 分支）
//! ② max_datagram_size() 检包       ⇒ 超限 = 丢 + 计 `超限`（§6.4 映射表）
//! ③ datagram_send_buffer_space() 预检 ⇒ 不足 = 丢 + 计 `发送缓冲满`
//! ④ send_datagram(Bytes)           ⇒ 零拷贝（Box<[u8]> → Vec → Bytes 复用同一分配）
//! ```
//!
//! **禁用裸 `send_datagram`**（§0.3 P4 实测：缓冲满时它静默淘汰最旧且恒返 Ok）——本文件
//! 是本 crate 客户端侧**唯一**的 `send_datagram` 调用点（S6-2 的代码门专项 grep）。
//!
//! 下行（DATAGRAM → TUN，[`pump_return`]）：`read_datagram()` → **有界队列**
//! （[`crate::tun::ReturnPath`]，2048 条）→ 专用 std 写线程；队列满 = 丢 + 计
//! `回程队列满`（TCP 会重传）。隧道面未附加 ⇒ 丢 + 计 `未登记`（无处可写）。
//!
//! 单线程前提（与出口面同款）：岛 runtime 是 `current_thread`，且预检与发送之间**无
//! `await`** ⇒ 不存在「预检通过后、发送前」被他处插入的窗口（`datagram_send_buffer_space()`
//! 两次加锁读可用的前提，§6.4 纪律）。

use std::sync::Arc;

use bytes::Bytes;
use quinn::{Connection, SendDatagramError};

use crate::cmd::DropReason;
use crate::tun::{PushOutcome, ReturnPath};

/// 丢弃上报口（岛宿主注入：入快照 + N-c 行 + 事件回调）。
pub(crate) type DropNote = Arc<dyn Fn(DropReason, &str) + Send + Sync + 'static>;

/// 丢弃上报口（**带条数**；M6.5 回程批化的落点：丢一批 = 一次上报 N 条，计数仍精确）。
pub(crate) type DropNoteN = Arc<dyn Fn(DropReason, u64, &str) + Send + Sync + 'static>;

/// 发送结果（测试面：`Dropped` 与计数一一对应）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SendOutcome {
    Sent,
    Dropped,
}

/// quinn 发送错误的归类（§6.4 的「按变体映射」）。
///
/// `TooLarge` = 本机侧单报文上限（MTU 变小后 quinn 会拒）⇒ `超限`；
/// `UnsupportedByPeer`/`Disabled`/`ConnectionLost` = 连接未就绪/未登记/已断 ⇒ `未登记`。
pub(crate) fn classify_send_error(e: &SendDatagramError) -> DropReason {
    match e {
        SendDatagramError::TooLarge => DropReason::TooLarge,
        _ => DropReason::Unregistered,
    }
}

/// 上行发送（**唯一入口**）：检包 + 预检 + 分类计数（见模块头四步）。
pub(crate) fn send_datagram_checked(
    conn: &Connection,
    pkt: Box<[u8]>,
    note: &DropNote,
) -> SendOutcome {
    let len = pkt.len();
    match conn.max_datagram_size() {
        // 连接未就绪（对端尚未确认 DATAGRAM 参数 / 握手未完）⇒ 归 `未登记`
        None => {
            note(
                DropReason::Unregistered,
                "连接未就绪（max_datagram_size 未知）—— 登记前丢弃",
            );
            return SendOutcome::Dropped;
        }
        Some(mds) if len > mds => {
            note(
                DropReason::TooLarge,
                &format!("包 {len}B > max_datagram_size {mds}B"),
            );
            return SendOutcome::Dropped;
        }
        Some(_) => {}
    }
    let space = conn.datagram_send_buffer_space();
    if space < len {
        // 裸 `send_datagram` 在这个点会静默淘汰最旧并返 Ok（P4）——这里丢 + 计数（不静默）
        note(
            DropReason::SendBufferFull,
            &format!("缓冲空余 {space}B < 包 {len}B（不静默：丢 + 计数）"),
        );
        return SendOutcome::Dropped;
    }
    // 零拷贝：`Box<[u8]>` → `Vec<u8>`（复用同一分配）→ `Bytes`（同样接管该分配）
    match conn.send_datagram(Bytes::from(Vec::from(pkt))) {
        Ok(()) => SendOutcome::Sent,
        Err(e) => {
            note(classify_send_error(&e), &format!("发送失败（{e}）"));
            SendOutcome::Dropped
        }
    }
}

/// 回程泵（每连接一枚 `JoinSet` 任务）：`read_datagram` → 有界队列 → std 写线程。
///
/// 退出：连接死（`read_datagram` 出错）/ **写线程已退（`PushOutcome::Gone`——S6 的 A3）** /
/// 任务被 abort（收工）。
/// 队列满 ⇒ 丢 + 计 `回程队列满`（丢新——与出口的 ring「队尾丢」语义同向：TCP 会重传）。
pub(crate) async fn pump_return(conn: Connection, ret: Arc<ReturnPath>, note: DropNoteN) {
    // M6.5 临时插桩（删除见 `diag_m65.rs` 模块头）
    let m65_on = crate::diag_m65::on();
    // **M6.5 批化**（证据：M6.5 真机逐段表——回程泵每包 4.7µs + 写线程每包一次唤醒/投递）：
    // 先**不等**地把队列里已有的包抽干成一批（至多 [`crate::tun::RETURN_BATCH_MAX`]）再一次
    // 投递；抽干后再回到 `read_datagram().await` 正常等待。丢弃语义不变（丢新/丢 + 计数/收口）。
    loop {
        let mut batch = drain_ready(&conn, m65_on);
        if batch.is_empty() {
            // 没有现成包：正常等待下一个（或收口）
            let dg = match conn.read_datagram().await {
                Ok(d) => d,
                Err(_) => return, // 连接死/被替换：本任务收口（新连接会另起一枚）
            };
            let t = m65_on
                .then(|| crate::diag_m65::diag().pump_deliver.start())
                .flatten();
            batch.push(dg.to_vec().into_boxed_slice());
            if m65_on {
                let d = crate::diag_m65::diag();
                d.pump_deliver.end(t);
                d.pump_pkts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }
        let n = batch.len() as u64;
        match ret.try_push_batch(batch) {
            PushOutcome::Pushed => {}
            PushOutcome::Full => {
                note(
                    DropReason::ReturnQueueFull,
                    n,
                    &format!("回程队列满（在途上限 {} 包）", crate::tun::RETURN_QUEUE_MAX),
                );
            }
            // 消费者（TUN 写线程）已退：**不**把它计成「队列满」（队列没满，是没人收）——
            // 记一行真因后收口。数据面此刻已不可信（fd 失效 ⇒ 世代层按 `fd` 分类重建），
            // 继续空转只会逐包拷贝 + 误计。
            PushOutcome::Gone => {
                note(
                    DropReason::Unregistered,
                    n,
                    "回程面已终止（TUN 写线程已退）—— 回程泵收口",
                );
                return;
            }
        }
    }
}

/// **同步抽干**已就绪的 datagram（至多 [`crate::tun::RETURN_BATCH_MAX`] 条；不等）。
///
/// 手法：就地 poll 一次 `ReadDatagram`——它的 poll 先看接收队列（拿得到就 `Ready`），
/// 拿不到才 `Pending`；非阻塞 ⇒ 一次 noop waker 的 context 足够，不新增依赖。
/// **同步 fn**：把 waker/context 关在函数内（async 体里夹带非 `Send` 值会破坏任务 `Send`）。
///
/// 拷贝一手的理由（**不是**零拷贝的地方）：`read_datagram` 给的 `Bytes` 是 quinn
/// 接收池缓冲的切片，直接入队会把池块按队列长度钉住（内存面不可预测）；拷进自有
/// `Vec` 后队列内存 = ≤ 上限 × 包长（与设计 §6.3/§6.4 的预算口径一致）。
fn drain_ready(conn: &Connection, m65_on: bool) -> crate::tun::ReturnBatch {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    let mut batch: crate::tun::ReturnBatch = Vec::new();
    while batch.len() < crate::tun::RETURN_BATCH_MAX {
        let mut fut = Box::pin(conn.read_datagram());
        match std::future::Future::poll(fut.as_mut(), &mut cx) {
            std::task::Poll::Ready(Ok(d)) => {
                let t = m65_on
                    .then(|| crate::diag_m65::diag().pump_deliver.start())
                    .flatten();
                batch.push(d.to_vec().into_boxed_slice());
                if m65_on {
                    let dg = crate::diag_m65::diag();
                    dg.pump_deliver.end(t);
                    dg.pump_pkts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            }
            // 队列空（正常）或连接错（由 await 分支收口）
            std::task::Poll::Ready(Err(_)) | std::task::Poll::Pending => break,
        }
    }
    batch
}
