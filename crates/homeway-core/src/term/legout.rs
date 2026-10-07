//! legout — 腿的出站队列（R6 6f-3b）。行为真源 = baseline 克隆 `pkg/term/term_leg.go`
//! 的 `legOut`（design D5/B'：会话级生产者只入队、永不阻塞；每腿唯一写者消化）。
//!
//! raw 腿与 surface 腿的队列语义不同（Go 同款）：
//! - **raw 腿**：实时字节不排队——写者直接从会话字节环按本腿偏移读（偏移只由写者
//!   推进，D10-2）；队列里只有控制帧（STATE latest-wins / ERROR 回执）与收尾帧
//!   （ENDED）。attach 回放由写者按握手计划执行。
//! - **surface 腿**：全部下行帧入队，体积由 `per_leg_queue_bytes` 封顶（背压失败
//!   模式二：丢弃待发 + 调用方标记需全量）。
//!
//! 唤醒 = Condvar（Go 的 wake 通道同义：合并语义——已有待处理唤醒时再投递即并窗）。

use std::collections::VecDeque;
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use super::frames::Op;

/// 一帧待发项。
#[derive(Debug, Clone)]
pub struct WriteItem {
    pub op: Op,
    pub payload: Vec<u8>,
}

impl WriteItem {
    pub fn new(op: Op, payload: Vec<u8>) -> Self {
        WriteItem { op, payload }
    }
}

/// 收尾决定。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Tail {
    None,
    /// ENDED：队列排空后发出、再关 conn（ENDED 先于 close）。
    Ended(Vec<u8>),
    /// 立即收尾（不发 ENDED）：排空即关。
    Quit,
}

struct OutState {
    queue: VecDeque<WriteItem>,
    qbytes: usize,
    tail: Tail,
    /// 收尾后置位：生产者据此丢弃入队，防泄漏。
    closed_for_prod: bool,
    /// raw 腿的停滞记账（写超时 ≠ 死亡）。
    stalled_since: Option<Instant>,
}

/// 一条腿的出站状态。
pub struct LegOut {
    st: Mutex<OutState>,
    wake: Condvar,
}

impl Default for LegOut {
    fn default() -> Self {
        Self::new()
    }
}

impl LegOut {
    /// 锁获取（中毒恢复口径统一，F7③）：持锁线程 panic 后队列状态可能半更新——
    /// **可接受的不一致面 + 处置保守**（例：`enqueue` 的 `qbytes += len` 与
    /// `push_back` 之间 panic 会让 `qbytes` 永久漂移；现实无 panic 点——分配失败是
    /// abort），后果 = 该腿持续「超限入队失败 → needSnapshot」，处置路径保守（断腿/全量），
    /// 而不是让整个 term 面级联崩溃。
    fn lock(&self) -> std::sync::MutexGuard<'_, OutState> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn new() -> Self {
        LegOut {
            st: Mutex::new(OutState {
                queue: VecDeque::new(),
                qbytes: 0,
                tail: Tail::None,
                closed_for_prod: false,
                stalled_since: None,
            }),
            wake: Condvar::new(),
        }
    }

    fn notify(&self) {
        self.wake.notify_all();
    }

    /// 唤醒写者（非阻塞；合并语义）。
    pub fn wake_writer(&self) {
        self.notify();
    }

    /// 入队一帧。surface 腿受 `cap_bytes`（>0）封顶——超限返回 false（调用方
    /// 标记需全量）；raw 腿控制帧传 0（latest-wins 通道，不会堆积）。收尾后一律拒绝。
    pub fn enqueue(&self, it: WriteItem, cap_bytes: usize) -> bool {
        let mut st = self.lock();
        if st.closed_for_prod {
            return false;
        }
        if cap_bytes > 0 && st.qbytes + it.payload.len() > cap_bytes {
            return false;
        }
        st.qbytes += it.payload.len();
        st.queue.push_back(it);
        drop(st);
        self.notify();
        true
    }

    /// 原子入队一组帧（快照 = 分片 + DONE）。整组要么全进、要么全弃。
    pub fn enqueue_group(&self, items: Vec<WriteItem>, cap_bytes: usize) -> bool {
        if items.is_empty() {
            return true;
        }
        let total: usize = items.iter().map(|i| i.payload.len()).sum();
        let mut st = self.lock();
        if st.closed_for_prod {
            return false;
        }
        if cap_bytes > 0 && st.qbytes + total > cap_bytes {
            return false;
        }
        st.qbytes += total;
        st.queue.extend(items);
        drop(st);
        self.notify();
        true
    }

    /// 入队 STATE 帧（latest-wins：丢掉还在排队的旧 STATE——状态只有最新值有意义）。
    pub fn enqueue_state(&self, it: WriteItem) {
        let mut st = self.lock();
        if st.closed_for_prod {
            return;
        }
        let mut keep: VecDeque<WriteItem> = VecDeque::with_capacity(st.queue.len());
        for old in std::mem::take(&mut st.queue) {
            if old.op == Op::STATE {
                st.qbytes -= old.payload.len();
            } else {
                keep.push_back(old);
            }
        }
        st.queue = keep;
        st.qbytes += it.payload.len();
        st.queue.push_back(it);
        drop(st);
        self.notify();
    }

    /// 安排收尾帧（ENDED）：写者排空队列后发出、再关 conn。
    pub fn finish_ended(&self, ended: WriteItem) {
        let mut st = self.lock();
        st.closed_for_prod = true;
        st.tail = Tail::Ended(ended.payload);
        drop(st);
        self.notify();
    }

    /// 立即收尾（不发 ENDED）：排空即关。
    pub fn finish_quit(&self) {
        let mut st = self.lock();
        st.closed_for_prod = true;
        st.tail = Tail::Quit;
        drop(st);
        self.notify();
    }

    /// ENDED 载荷（breakLeg 后调用方直发用；无收尾帧返回 None）。
    pub fn ended_payload(&self) -> Option<Vec<u8>> {
        match &self.lock().tail {
            Tail::Ended(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// 是否已安排收尾（ENDED 或 QUIT）。
    pub fn is_finishing(&self) -> bool {
        self.lock().closed_for_prod
    }

    /// 取走待发队列；ENDED 只在队列排空时交出（保证 ENDED 之前不再插帧）。
    /// 返回 (items, ended_payload, quit)。
    pub fn take(&self) -> (Vec<WriteItem>, Option<Vec<u8>>, bool) {
        let mut st = self.lock();
        if matches!(st.tail, Tail::Quit) {
            return (Vec::new(), None, true);
        }
        let items: Vec<WriteItem> = st.queue.drain(..).collect();
        st.qbytes = 0;
        if items.is_empty() {
            if let Tail::Ended(p) = st.tail.clone() {
                return (Vec::new(), Some(p), false);
            }
            return (Vec::new(), None, false);
        }
        (items, None, false)
    }

    /// 阻塞等新事件（队列空时）。带超时轮询上限（服务关停位的检查节拍）。
    pub fn wait(&self, max: Duration) {
        let st = self.lock();
        if !st.queue.is_empty() || !matches!(st.tail, Tail::None) {
            return;
        }
        let _ = self.wake.wait_timeout(st, max).unwrap_or_else(|e| e.into_inner());
    }

    /// 记一次写停滞/恢复；返回停滞是否已连续超过 limit（= 该断腿了）。
    pub fn note_stall(&self, stalled: bool, limit: Duration) -> bool {
        let mut st = self.lock();
        if stalled {
            let since = *st.stalled_since.get_or_insert_with(Instant::now);
            st.stalled_since = Some(since);
            return since.elapsed() > limit;
        }
        st.stalled_since = None;
        false
    }

    /// 实时停滞快照（F4）：单次持锁取 `(是否停滞, 已停滞时长)`——消
    /// `is_stalled().then(stalled_for)` 两次取锁的 TOCTOU；未停滞返回 None。
    /// 淘汰排序的**唯一真源**（Go `evictForSlotLocked` 读实时 `isStalled/stalledFor` 同义）。
    pub fn stalled_snapshot(&self) -> Option<Duration> {
        self.lock().stalled_since.map(|s| s.elapsed())
    }

    /// 当前是否停滞（上限淘汰策略用）。
    pub fn is_stalled(&self) -> bool {
        self.lock().stalled_since.is_some()
    }

    /// 已停滞多久（淘汰排序用；未停滞 = 0）。
    pub fn stalled_for(&self) -> Duration {
        match self.lock().stalled_since {
            Some(s) => s.elapsed(),
            None => Duration::ZERO,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(op: Op, n: usize) -> WriteItem {
        WriteItem::new(op, vec![0u8; n])
    }

    /// 队列封顶（失败模式二）+ latest-wins STATE。
    #[test]
    fn queue_cap_and_latest_wins() {
        let out = LegOut::new();
        assert!(out.enqueue(item(Op::DATA, 100), 150));
        assert!(!out.enqueue(item(Op::DATA, 100), 150), "超体积上限丢弃");
        // STATE latest-wins：排队的旧 STATE 被丢掉
        out.enqueue_state(item(Op::STATE, 40));
        out.enqueue_state(item(Op::STATE, 50));
        let (items, ended, quit) = out.take();
        assert_eq!(items.len(), 2, "DATA + 新 STATE");
        assert_eq!(items[0].op, Op::DATA);
        assert_eq!(items[1].op, Op::STATE);
        assert_eq!(items[1].payload.len(), 50);
        assert!(ended.is_none() && !quit);
        // cap=0 = 不封顶（raw 控制帧）
        assert!(out.enqueue(item(Op::STATE, 100_000), 0));
    }

    /// 收尾语义：ENDED 排空后交出；quit 立即；收尾后生产者入队被拒。
    #[test]
    fn finish_ended_then_quit() {
        let out = LegOut::new();
        out.enqueue(item(Op::DATA, 10), 0);
        out.finish_ended(WriteItem::new(Op::ENDED, vec![1, 2, 3]));
        assert!(!out.enqueue(item(Op::DATA, 5), 0), "收尾后拒绝入队");
        assert!(!out.enqueue_group(vec![item(Op::DATA, 5)], 0));
        // 队列非空 → 先交 items
        let (items, ended, quit) = out.take();
        assert_eq!(items.len(), 1);
        assert!(ended.is_none() && !quit);
        // 排空 → 交 ENDED
        let (items, ended, quit) = out.take();
        assert!(items.is_empty());
        assert_eq!(ended, Some(vec![1, 2, 3]));
        assert!(!quit);
        // quit 形态
        out.finish_quit();
        let (_, _, quit) = out.take();
        assert!(quit);
    }

    /// F7③：毒锁恢复——持锁线程 panic 后队列仍可用（其余调用不级联崩溃）。
    #[test]
    fn poisoned_lock_recovers() {
        use std::sync::Arc;
        let out = Arc::new(LegOut::new());
        let o2 = Arc::clone(&out);
        let h = std::thread::spawn(move || {
            let _g = o2.st.lock().unwrap();
            panic!("poison");
        });
        assert!(h.join().is_err(), "持锁线程 panic");
        assert!(out.enqueue(item(Op::DATA, 3), 0), "毒锁恢复后仍可入队");
        let (items, _, _) = out.take();
        assert_eq!(items.len(), 1);
        assert!(!out.is_stalled());
        out.finish_quit();
        let (_, _, quit) = out.take();
        assert!(quit);
    }

    /// F4：单次持锁的停滞快照（TOCTOU 消除版）；未停滞 = None。
    #[test]
    fn stalled_snapshot_single_lock() {
        let out = LegOut::new();
        assert!(out.stalled_snapshot().is_none(), "未停滞 ⇒ None");
        assert!(!out.note_stall(true, Duration::from_secs(60)));
        let d = out.stalled_snapshot().expect("停滞中 ⇒ Some");
        assert!(d < Duration::from_secs(60));
        assert!(!out.note_stall(false, Duration::from_secs(60)));
        assert!(out.stalled_snapshot().is_none(), "恢复 ⇒ None");
    }

    /// 停滞记账：超限判定 + 恢复清零。
    #[test]
    fn stall_accounting() {
        let out = LegOut::new();
        assert!(!out.is_stalled());
        assert!(!out.note_stall(true, Duration::from_secs(60)), "首个停滞拍未超限");
        assert!(out.is_stalled());
        assert!(!out.note_stall(false, Duration::from_secs(60)));
        assert!(!out.is_stalled());
        // 注入已停滞起点：直接构造超限（不睡 60s）
        {
            let mut st = out.st.lock().unwrap();
            st.stalled_since = Some(Instant::now() - Duration::from_secs(61));
        }
        assert!(out.note_stall(true, Duration::from_secs(60)), "连续超限 ⇒ 断腿");
        assert!(out.stalled_for() >= Duration::from_secs(61));
    }
}
