//! 岛内**服务流面**（M3 §1.4/§1.5/§1.6/§1.7）：在册表 + 每流有界待发队列 + 写者任务 +
//! 按需读（读在任务里挂起，取消只经 `Cmd::StreamClose`）。
//!
//! **本文件属异步面**（隔离门 ② 条的 `ASYNC_FILES` 显式清单；理由：写者任务要
//! `write_all().await`、读任务要 `read_chunk().await`、取消点要 `Notify`）。
//!
//! 三条结构不变量（每条都由 `driver.rs`/`tools/check-quic-isolation.sh` 的可判定事实钉住）：
//!
//! 1. **命令循环零写等待**（§1.4）：`Streams::write` 是**同步 fn**（非阻塞判定 → 回执），
//!    `.await` 只出现在写者任务里 ⇒ 单条慢流不可能把岛的命令循环变成队头阻塞。
//! 2. **岛内不许主动排空 RecvStream**（§1.4-①）：读只在收到 `Cmd::StreamRead` 时发生
//!    （[`read_task`]），且回执里带一块；不存在「岛 → 无界通道」的主动泵。
//! 3. **自记账配额**（§1.4-N14）：容量 = `max_bidi − 2`（控制流 + probe 持久流终身占用），
//!    额度耗尽 **快速失败**（`StreamErr::Busy`，不等 `open_bi` 阻塞/超时）。
//!
//! 半关/复位语义（§1.3，逐条对实测锚点）：
//! - `shutdown` = `SendStream::finish()`（**单向**：对端仍可发；本端仍可读）；
//! - `close` = `SendStream::reset(0)`（经写者任务发）+ `RecvStream::stop(0)`（STOP_SENDING）
//!   + 摘表 + 唤醒在途读；**不**用 reset 做正常收工。

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use quinn::{ReadError, RecvStream, SendStream, VarInt};
use tokio::sync::{Mutex as TokioMutex, Notify};

use crate::cmd::{IslandEvent, Logf, OnEvent, StreamReply};
use crate::stream::{StreamErr, StreamId, StreamTag, StreamWriteOut};
use crate::sync_util::lock_unpoison;
use crate::tuning::StreamLimits;

use super::log_due;

/// 读一次的最大块（按需拉取的粒度；与 `wgcore` 的栈读同量级——不是窗口值）。
const READ_CHUNK: usize = 16 * 1024;

/// 待发队列的结束标记（写者任务在队列排空后据此动作）。
const END_NONE: u8 = 0;
/// 半关：排空后 `finish()`。
const END_FINISH: u8 = 1;
/// 关流：排空后 `reset(0)`。
const END_RESET: u8 = 2;

/// 每流**有界待发队列**（§1.4：默认 64 KiB/流，懒分配 = 开流时才建）。
///
/// 形态说明：队列本体是 std `Mutex<VecDeque>`（命令循环里同步 push/取长度），跨线程唤醒
/// 用 `Notify`（写者任务 `notified().await`；`notify_one` 的许可语义把「push 与 await 之间
/// 的窗口」盖住 ⇒ 无丢唤醒）。
struct Pending {
    q: Mutex<Q>,
    cap: usize,
    notify: Notify,
    end: AtomicU8,
}

struct Q {
    items: VecDeque<Vec<u8>>,
    bytes: usize,
}

impl Pending {
    fn new(cap: usize) -> Self {
        Self {
            q: Mutex::new(Q {
                items: VecDeque::new(),
                bytes: 0,
            }),
            cap,
            notify: Notify::new(),
            end: AtomicU8::new(END_NONE),
        }
    }

    /// 非阻塞接纳（**唯一写入口**；返回值语义 = [`StreamWriteOut`]）。
    fn push(&self, mut data: Vec<u8>) -> StreamWriteOut {
        if data.is_empty() {
            // 空写短路（io::Write 约定：空写返 Ok(0)；**不是**背压，不计数——
            // 与 `facade/tun_exec.rs` 的 `SessionWriteHalf::write` 同款）
            return StreamWriteOut { n: 0, back: None };
        }
        let mut q = lock_unpoison(&self.q);
        let avail = self.cap.saturating_sub(q.bytes);
        if avail == 0 {
            drop(q);
            return StreamWriteOut {
                n: 0,
                back: Some(data),
            };
        }
        let n = data.len().min(avail);
        if n < data.len() {
            // 部分接纳：调用方按 io::Write 契约以 `&data[n..]` 重试（`wgcore` 同款：
            // 部分接纳不回带——省一次整段拷贝）；本处 truncate 不重拷前缀。
            data.truncate(n);
        }
        q.bytes += n;
        q.items.push_back(data);
        drop(q);
        self.notify.notify_one();
        StreamWriteOut { n, back: None }
    }

    /// 队列里还有多少字节（在册读数用；测试判据也读它）。
    #[cfg(test)]
    fn queued(&self) -> usize {
        lock_unpoison(&self.q).bytes
    }

    fn finish(&self) {
        self.end.store(END_FINISH, Ordering::SeqCst);
        self.notify.notify_one();
    }

    fn abort(&self) {
        self.end.store(END_RESET, Ordering::SeqCst);
        self.notify.notify_one();
    }
}

/// 一条在册服务流的槽（`Arc` 分享给写者/读任务；本体在 [`Streams`] 的注册表里）。
pub(crate) struct Slot {
    id: StreamId,
    tag: StreamTag,
    /// 待发队列（写者任务独占消费）。
    pending: Arc<Pending>,
    /// 读半边（`TokioMutex` 串行化同流并发读——调用方本应顺序读，这里只是**不 UB**）。
    recv: Arc<TokioMutex<RecvStream>>,
    /// 读任务的取消/唤醒点（`close` 与连接死都打它）。
    cancel: Notify,
    /// 已关（`close`/连接死）⇒ 读任务与后续写一律快速失败。
    closed: AtomicBool,
    /// 写半边已半关（`shutdown`）⇒ 后续写 = `Closed`（对端仍可发 ⇒ 读半边不受影响）。
    write_closed: AtomicBool,
    /// 写者任务已因错误退出（对端 reset / 连接死）⇒ 后续写 = `Closed`。
    send_broken: AtomicBool,
    /// 本流发出/读回字节（关流行的 `↑%dB ↓%dB` 与内存账读数）。
    ///
    /// **口径**：只算应用载荷——开流时入队的 **tag 首字节不计**（协议框架字节；
    /// §8.2-12 的 `stream_bytes` 是应用字节读数）。`untagged` 载着「还欠一个不计数的
    /// 字节」，写者任务在第一次写出时抵扣。
    bytes_out: AtomicU64,
    bytes_in: AtomicU64,
    /// 待抵扣的**非应用**字节（开流时的 tag 首字节 = 1）。
    untagged: AtomicU64,
}

impl Slot {
    /// 流 id（判据行/排障）。
    pub(crate) fn id(&self) -> StreamId {
        self.id
    }

    /// 读半边句柄（读任务用）。
    pub(crate) fn recv(&self) -> &Arc<TokioMutex<RecvStream>> {
        &self.recv
    }

    /// 取消/唤醒点（读任务 `select!` 的另一臂）。
    pub(crate) fn cancel(&self) -> &Notify {
        &self.cancel
    }

    /// 是否已关（读任务在拿锁前后各查一次）。
    pub(crate) fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }
}

/// 流面计数（岛快照 [`crate::cmd::IslandSnapshot`] 的 `streams_*` 位同源）。
#[derive(Default, Debug)]
pub(crate) struct StreamStats {
    open: AtomicU64,
    close: AtomicU64,
    refused: AtomicU64,
    bytes_out: AtomicU64,
    bytes_in: AtomicU64,
    backpressure: AtomicU64,
}

/// 计数快照（岛快照装配读它）。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StreamCounters {
    pub open: u64,
    pub refused: u64,
    pub bytes_out: u64,
    pub bytes_in: u64,
    pub backpressure: u64,
}

/// 服务流注册表（岛线程独占；`Mutex` 只为「表本体在岛线程、槽 `Arc` 进任务」的形态）。
pub(crate) struct Streams {
    inner: Mutex<Inner>,
    limits: StreamLimits,
    stats: Arc<StreamStats>,
    logf: Logf,
    on_event: Arc<Mutex<Option<OnEvent>>>,
}

struct Inner {
    next_id: u64,
    map: HashMap<u64, Arc<Slot>>,
}

impl Streams {
    /// 装配（`limits` = 两端共用的流面限制；`on_event` = 岛的事件回调面）。
    pub(crate) fn new(
        limits: StreamLimits,
        stats: Arc<StreamStats>,
        logf: Logf,
        on_event: Arc<Mutex<Option<OnEvent>>>,
    ) -> Self {
        Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                map: HashMap::new(),
            }),
            limits,
            stats,
            logf,
            on_event,
        }
    }

    /// 容量检查（**先查自记账，再 `open_bi`**；§1.6 的快速失败）。
    pub(crate) fn has_capacity(&self) -> bool {
        lock_unpoison(&self.inner).map.len() < self.limits.service_capacity()
    }

    /// 在册条数（快照与判据行）。
    pub(crate) fn active(&self) -> usize {
        lock_unpoison(&self.inner).map.len()
    }

    /// 有效服务容量（判据行 `在册 n/cap` 的 `cap`）。
    pub(crate) fn capacity(&self) -> usize {
        self.limits.service_capacity()
    }

    /// 计数快照。
    pub(crate) fn counters(&self) -> StreamCounters {
        StreamCounters {
            open: self.stats.open.load(Ordering::SeqCst),
            refused: self.stats.refused.load(Ordering::SeqCst),
            bytes_out: self.stats.bytes_out.load(Ordering::SeqCst),
            bytes_in: self.stats.bytes_in.load(Ordering::SeqCst),
            backpressure: self.stats.backpressure.load(Ordering::SeqCst),
        }
    }

    /// 拒开计数 + 节流记行（C19 族的失败行，§8.2-11；额度耗尽时附本机在册读数）。
    pub(crate) fn note_refused(&self, tag: StreamTag, err: StreamErr) {
        let n = self.stats.refused.fetch_add(1, Ordering::SeqCst) + 1;
        if !log_due(n) {
            return;
        }
        if err == StreamErr::Busy && self.active() >= self.capacity() {
            (*self.logf)(&format!(
                "服务流失败（tag={}；{}；本机在册 {}/{}；第 {n} 次）",
                tag.text(),
                err.text(),
                self.active(),
                self.capacity()
            ));
        } else {
            (*self.logf)(&format!(
                "服务流失败（tag={}；{}；第 {n} 次）",
                tag.text(),
                err.text()
            ));
        }
    }

    /// 装配一条**已开**的流：写 tag 首字节 + 入册；返回槽（写者任务随之起）。
    ///
    /// tag 首字节走待发队列（写者任务的第一次写）——与后续写同一序、同一背压面；
    /// 队列容量远大于 1B ⇒ 必接纳（`debug_assert` 钉住该前提）。
    pub(crate) fn register(&self, tag: StreamTag, recv: RecvStream) -> Arc<Slot> {
        let mut inner = lock_unpoison(&self.inner);
        let id = StreamId::new(inner.next_id);
        inner.next_id += 1;
        let slot = Arc::new(Slot {
            id,
            tag,
            pending: Arc::new(Pending::new(self.limits.pending_bytes)),
            recv: Arc::new(TokioMutex::new(recv)),
            cancel: Notify::new(),
            closed: AtomicBool::new(false),
            write_closed: AtomicBool::new(false),
            send_broken: AtomicBool::new(false),
            bytes_out: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            untagged: AtomicU64::new(1), // tag 首字节（协议框架，不计应用字节）
        });
        let out = slot.pending.push(vec![tag.as_byte()]);
        debug_assert_eq!(out.n, 1, "tag 首字节必被待发队列接纳（容量 ≥ 4 KiB）");
        let _ = out;
        inner.map.insert(id.get(), Arc::clone(&slot));
        let n = self.stats.open.fetch_add(1, Ordering::SeqCst) + 1;
        drop(inner);
        if log_due(n) {
            // C19 族（§8.2-11）的开流行；耗时由调用方在打开路径上补（本行只报 tag）
            (*self.logf)(&format!("服务流已开（tag={}；第 {n} 条）", tag.text()));
        }
        self.emit(IslandEvent::StreamOpened { tag });
        slot
    }

    /// 槽查找（读任务投递用）。
    pub(crate) fn slot_of(&self, id: StreamId) -> Option<Arc<Slot>> {
        lock_unpoison(&self.inner).map.get(&id.get()).cloned()
    }

    /// 写（**同步、非阻塞**：`n` 由待发队列余量给出；§1.4）。
    pub(crate) fn write(&self, id: StreamId, data: Vec<u8>) -> Result<StreamWriteOut, StreamErr> {
        let Some(slot) = self.slot_of(id) else {
            return Err(StreamErr::Closed);
        };
        if slot.closed.load(Ordering::SeqCst) || slot.send_broken.load(Ordering::SeqCst) {
            return Err(StreamErr::Closed);
        }
        if slot.write_closed.load(Ordering::SeqCst) {
            // 半关之后的写 = 错误（§1.3 的复位后写同面：本地已关的方向不能再写）
            return Err(StreamErr::Closed);
        }
        let out = slot.pending.push(data);
        if out.n == 0 && out.back.is_some() {
            let n = self.stats.backpressure.fetch_add(1, Ordering::SeqCst) + 1;
            if log_due(n) {
                (*self.logf)(&format!(
                    "服务流背压（id={} tag={}；待发队列满 {}B；第 {n} 次——调用方走 Ok(0) 退避环）",
                    id,
                    slot.tag.text(),
                    self.limits.pending_bytes
                ));
            }
        }
        Ok(out)
    }

    /// 半关（FIN；§1.3：**单向**——对端仍可发、本端仍可读）。
    pub(crate) fn shutdown(&self, id: StreamId) -> Result<StreamTag, StreamErr> {
        let Some(slot) = self.slot_of(id) else {
            return Err(StreamErr::Closed);
        };
        if slot.closed.load(Ordering::SeqCst) || slot.send_broken.load(Ordering::SeqCst) {
            return Err(StreamErr::Closed);
        }
        if slot.write_closed.swap(true, Ordering::SeqCst) {
            return Err(StreamErr::Closed); // 重复半关
        }
        slot.pending.finish();
        Ok(slot.tag)
    }

    /// 关流/复位（abort；**并取消在途读**——§1.5 的取消面）。
    ///
    /// 与 `wgcore::Cmd::Close` 同款「摘表 + 动作即答」：复位由写者任务在队列排空后发出
    /// （它独占 `SendStream`）；读半边的 `stop`（STOP_SENDING）在无读任务占锁时就地发。
    pub(crate) fn close(&self, id: StreamId) -> Result<Arc<Slot>, StreamErr> {
        let slot = lock_unpoison(&self.inner).map.remove(&id.get());
        let Some(slot) = slot else {
            return Err(StreamErr::Closed);
        };
        slot.closed.store(true, Ordering::SeqCst);
        slot.write_closed.store(true, Ordering::SeqCst);
        slot.pending.abort();
        slot.cancel.notify_one(); // 唤醒在途读（`notify_one` 的许可语义盖住 push/await 窗口）
        if let Ok(mut r) = slot.recv.try_lock() {
            // 无读任务占用 ⇒ 就地 STOP_SENDING（对端停发；§1.3 的复位面）
            let _ = r.stop(VarInt::from_u32(0));
        }
        Ok(slot)
    }

    /// 连接死/被替换：在册流全部按「EOF」收（读 = `Closed`、写 = `Closed`），清表。
    ///
    /// **不发复位**（承载已断，发了也无处去）；`StreamErr::ConnectionLost` 只用于
    /// **无连接时的新开**（见 `driver.rs` 的处置）——「连接死 ⇒ 既有流读回 EOF」正是
    /// 今天 `stackb` 的行为（§1.6 的 EOF 同形性）。
    pub(crate) fn clear_on_connection_loss(&self) -> usize {
        let drained: Vec<Arc<Slot>> = {
            let mut inner = lock_unpoison(&self.inner);
            let mut out = Vec::with_capacity(inner.map.len());
            out.extend(inner.map.drain().map(|(_, v)| v));
            out
        };
        let n = drained.len();
        for slot in &drained {
            slot.closed.store(true, Ordering::SeqCst);
            slot.write_closed.store(true, Ordering::SeqCst);
            slot.pending.abort();
            slot.cancel.notify_one();
            if let Ok(mut r) = slot.recv.try_lock() {
                let _ = r.stop(VarInt::from_u32(0));
            }
        }
        n
    }

    /// 写者任务结束（正常 finish / 对端 reset / 连接死）后的槽侧记账。
    pub(crate) fn writer_finished(&self, slot: &Arc<Slot>, reason: WriterEnd) {
        if reason == WriterEnd::Broken {
            // 写失败（对端 reset / 承载死）⇒ 后续写快速失败（读半边不因此失效）
            slot.send_broken.store(true, Ordering::SeqCst);
        }
        let out = slot.bytes_out.load(Ordering::SeqCst);
        let inn = slot.bytes_in.load(Ordering::SeqCst);
        let n = self.stats.close.fetch_add(1, Ordering::SeqCst) + 1;
        if log_due(n) {
            (*self.logf)(&format!(
                "服务流已关（id={} tag={}，↑{out}B ↓{inn}B；第 {n} 条）",
                slot.id,
                slot.tag.text()
            ));
        }
        self.emit(IslandEvent::StreamClosed { tag: slot.tag });
    }

    /// 字节记账（写者/读任务在岛线程外的同一枚 `AtomicU64` 上加；**只记应用载荷**）。
    pub(crate) fn add_bytes_out(&self, n: u64) {
        self.stats.bytes_out.fetch_add(n, Ordering::SeqCst);
    }

    pub(crate) fn add_bytes_in(&self, n: u64) {
        self.stats.bytes_in.fetch_add(n, Ordering::SeqCst);
    }

    fn emit(&self, ev: IslandEvent) {
        let h = lock_unpoison(&self.on_event).clone();
        if let Some(h) = h {
            h(ev);
        }
    }
}

/// 写者任务的结束原因（判据面区分「正常收工 / 对端复位 / 承载死」）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum WriterEnd {
    /// 半关完成（`finish()` 发出）。
    Finished,
    /// 写失败（对端 reset / 连接死）⇒ 槽标记 `send_broken`。
    Broken,
    /// 关流（`reset(0)` 发出）。
    Reset,
}

/// **写者任务**（每流一枚；`JoinSet` 托底 abort）。
///
/// 循环：取空队列 → 逐块 `write_all().await` → 查结束标记 → 无事则 `notified().await`。
/// 背压在这里变成 await（**不在**命令循环里）；`write()` 的 `n` 只由队列余量决定。
pub(crate) async fn writer_task(
    mut send: SendStream,
    slot: Arc<Slot>,
    streams: Arc<Streams>,
) {
    loop {
        // ① 取空（锁内快照 + 清零字节账；写者独占消费 ⇒ 不会与别的写者抢）
        let batch: Vec<Vec<u8>> = {
            let mut q = lock_unpoison(&slot.pending.q);
            q.bytes = 0;
            q.items.drain(..).collect()
        };
        for chunk in batch {
            if send.write_all(&chunk).await.is_err() {
                streams.writer_finished(&slot, WriterEnd::Broken);
                return;
            }
            // 只算应用载荷：开流时的 tag 首字节（1B）在第一块上抵扣
            let raw = chunk.len() as u64;
            let skip = slot
                .untagged
                .swap(0, Ordering::SeqCst)
                .min(raw);
            let counted = raw - skip;
            slot.bytes_out.fetch_add(counted, Ordering::SeqCst);
            streams.add_bytes_out(counted);
        }
        // ② 结束标记（半关/关流；队列已排空 ⇒ 顺序正确）
        match slot.pending.end.load(Ordering::SeqCst) {
            END_FINISH => {
                let _ = send.finish();
                streams.writer_finished(&slot, WriterEnd::Finished);
                return;
            }
            END_RESET => {
                let _ = send.reset(VarInt::from_u32(0));
                streams.writer_finished(&slot, WriterEnd::Reset);
                return;
            }
            _ => {}
        }
        // ③ 等新数据/结束标记（`notify_one` 的许可语义：push 与 await 之间无丢唤醒）
        slot.pending.notify.notified().await;
    }
}

/// **读任务**（每次 `Cmd::StreamRead` 一枚）：挂到数据/EOF/复位/**取消**为止。
///
/// 取消路径（`Cmd::StreamClose` / 连接死）：`STOP_SENDING` + 回 `Closed`——与
/// 「连接死 ⇒ 读回 EOF」同面（§1.6 的 EOF 同形性）。
pub(crate) async fn read_task(slot: Arc<Slot>, reply: StreamReply<Vec<u8>>, streams: Arc<Streams>) {
    // ① 取读半边（先查已关 + 许可语义；`TokioMutex` 串行化同流并发读）
    let mut recv = loop {
        if slot.is_closed() {
            let _ = reply.send(Err(StreamErr::Closed));
            return;
        }
        tokio::select! {
            biased;
            _ = slot.cancel().notified() => continue,
            guard = slot.recv().lock() => break guard,
        }
    };
    // ② 读一块 / 被取消
    let out = tokio::select! {
        biased;
        _ = slot.cancel().notified() => {
            let _ = recv.stop(VarInt::from_u32(0));
            Err(StreamErr::Closed)
        }
        r = recv.read_chunk(READ_CHUNK, true) => classify_read(r),
    };
    drop(recv);
    if slot.is_closed() {
        // 关流与数据到达竞态：**不交付**已关流的数据（调用方已放弃该流）
        let _ = reply.send(Err(StreamErr::Closed));
        return;
    }
    if let Ok(v) = &out {
        let n = v.len() as u64;
        slot.bytes_in.fetch_add(n, Ordering::SeqCst);
        streams.add_bytes_in(n);
    }
    let _ = reply.send(out);
}

/// 读结论归一（§1.6：白名单复位码 ⇒ typed；对端 FIN/区间外 ⇒ `Closed` = EOF）。
///
/// **连接死也归 EOF（代码门 r18 ①-4 的修法）**：`clear_on_connection_loss`（housekeeping
/// 拍内）会把在册流整体按 EOF 收（§1.6 的 EOF 同形性 / A9「连接死 = 读回 EOF」），但读任务
/// 可能**先**落定在 `ReadError::ConnectionLost` 上 ⇒ 同一条连接死在两个时刻给出「EOF」与
/// 「typed 错误」两种结论（窗口 = 一拍 250ms）。两条出口统一到 **`Closed` = EOF**：消费侧
/// （`facade/quic_stream.rs`）本就把二者都当「服务流结束」收，不存在把连接故障误判成
/// 服务级拒绝的风险（服务级拒绝走白名单复位码那一支）。
fn classify_read(r: Result<Option<quinn::Chunk>, ReadError>) -> Result<Vec<u8>, StreamErr> {
    match r {
        Ok(Some(c)) => Ok(c.bytes.to_vec()),
        Ok(None) => Err(StreamErr::Closed), // 对端 FIN = EOF（幂等）
        Err(ReadError::Reset(code)) => {
            Err(StreamErr::from_reset_code(code.into_inner()).unwrap_or(StreamErr::Closed))
        }
        Err(ReadError::ConnectionLost(_)) => Err(StreamErr::Closed),
        // `ClosedStream`/`UnknownStream`/`ZeroRttRejected`：流面已结束 ⇒ EOF 同面
        Err(_) => Err(StreamErr::Closed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（读结论归一，§1.6/A9；代码门 r18 ①-4）**：连接死、对端 FIN、白名单外复位码
    /// 三条都归 `Closed`（EOF 同形）；只有白名单内的复位码才是 typed 拒绝。
    #[test]
    fn classify_read_maps_connection_loss_to_eof() {
        use quinn::ConnectionError;
        // 连接死（housekeeping 尚未把槽置 closed 的竞态窗）⇒ EOF 面，不是 typed 错误
        assert_eq!(
            classify_read(Err(ReadError::ConnectionLost(ConnectionError::LocallyClosed))),
            Err(StreamErr::Closed),
            "连接死 = 读回 EOF（与 clear_on_connection_loss 同面）"
        );
        // 白名单内复位码 ⇒ typed
        assert_eq!(
            classify_read(Err(ReadError::Reset(VarInt::from_u32(
                crate::stream::reset::SERVICE_DISABLED as u32
            )))),
            Err(StreamErr::NotSupported)
        );
        // 白名单外（对端自选 0x07）⇒ EOF
        assert_eq!(
            classify_read(Err(ReadError::Reset(VarInt::from_u32(7)))),
            Err(StreamErr::Closed)
        );
        // 流面已结束的其余形态 ⇒ EOF
        assert_eq!(
            classify_read(Err(ReadError::ClosedStream)),
            Err(StreamErr::Closed)
        );
    }

    /// 待发队列的**非阻塞接纳**（§1.4 的核心判据之一，纯逻辑可测）：
    /// 满 ⇒ `n=0` + 原 Vec 带回；部分接纳 ⇒ 前缀推进；空写 ⇒ 短路。
    #[test]
    fn pending_queue_admits_nonblocking_and_carries_back() {
        let q = Pending::new(8);
        // 首次接纳 8B（满）
        let out = q.push(vec![1u8; 8]);
        assert_eq!(out.n, 8);
        assert!(out.back.is_none(), "部分/全额接纳不回带");
        assert_eq!(q.queued(), 8);
        // 满 ⇒ n=0 + 原 Vec 带回（**背压**信号）
        let data = vec![2u8; 16];
        let out = q.push(data.clone());
        assert_eq!(out.n, 0);
        assert_eq!(out.back.as_deref(), Some(data.as_slice()), "零接纳带回原 Vec");
        assert_eq!(q.queued(), 8, "零接纳不改队列");
        // 空写短路：不是背压（不产 back）
        let out = q.push(Vec::new());
        assert_eq!(out.n, 0);
        assert!(out.back.is_none(), "空写短路（io::Write 约定）");

        // 部分接纳：容量 20，已占 8 ⇒ 再推 16 只接 12
        let q = Pending::new(20);
        assert_eq!(q.push(vec![0u8; 8]).n, 8);
        let out = q.push(vec![3u8; 16]);
        assert_eq!(out.n, 12, "接纳 = 余量（调用方按 io::Write 切余量）");
        assert!(out.back.is_none());
        assert_eq!(q.queued(), 20);
        // 排空后可再接纳（写者消费面）
        {
            let mut g = lock_unpoison(&q.q);
            g.items.clear();
            g.bytes = 0;
        }
        assert_eq!(q.push(vec![4u8; 20]).n, 20);
    }

    /// 结束标记的先后语义（半关/关流都是**原子置位**；写者任务按标记动作）。
    #[test]
    fn pending_end_markers_are_atomic_and_last_write_wins() {
        let q = Pending::new(8);
        assert_eq!(q.end.load(Ordering::SeqCst), END_NONE);
        q.finish();
        assert_eq!(q.end.load(Ordering::SeqCst), END_FINISH);
        q.abort();
        assert_eq!(q.end.load(Ordering::SeqCst), END_RESET, "关流覆盖半关（更终态）");
    }
}
