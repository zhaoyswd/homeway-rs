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
        let mut st = self.st.lock().expect("legout");
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
        let mut st = self.st.lock().expect("legout");
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
        let mut st = self.st.lock().expect("legout");
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
        let mut st = self.st.lock().expect("legout");
        st.closed_for_prod = true;
        st.tail = Tail::Ended(ended.payload);
        drop(st);
        self.notify();
    }

    /// 立即收尾（不发 ENDED）：排空即关。
    pub fn finish_quit(&self) {
        let mut st = self.st.lock().expect("legout");
        st.closed_for_prod = true;
        st.tail = Tail::Quit;
        drop(st);
        self.notify();
    }

    /// ENDED 载荷（breakLeg 后调用方直发用；无收尾帧返回 None）。
    pub fn ended_payload(&self) -> Option<Vec<u8>> {
        match &self.st.lock().expect("legout").tail {
            Tail::Ended(p) => Some(p.clone()),
            _ => None,
        }
    }

    /// 是否已安排收尾（ENDED 或 QUIT）。
    pub fn is_finishing(&self) -> bool {
        self.st.lock().expect("legout").closed_for_prod
    }

    /// 取走待发队列；ENDED 只在队列排空时交出（保证 ENDED 之前不再插帧）。
    /// 返回 (items, ended_payload, quit)。
    pub fn take(&self) -> (Vec<WriteItem>, Option<Vec<u8>>, bool) {
        let mut st = self.st.lock().expect("legout");
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
        let st = self.st.lock().expect("legout");
        if !st.queue.is_empty() || !matches!(st.tail, Tail::None) {
            return;
        }
        let _ = self.wake.wait_timeout(st, max).expect("legout");
    }

    /// 记一次写停滞/恢复；返回停滞是否已连续超过 limit（= 该断腿了）。
    pub fn note_stall(&self, stalled: bool, limit: Duration) -> bool {
        let mut st = self.st.lock().expect("legout");
        if stalled {
            let since = *st.stalled_since.get_or_insert_with(Instant::now);
            st.stalled_since = Some(since);
            return since.elapsed() > limit;
        }
        st.stalled_since = None;
        false
    }

    /// 当前是否停滞（上限淘汰策略用）。
    pub fn is_stalled(&self) -> bool {
        self.st.lock().expect("legout").stalled_since.is_some()
    }

    /// 已停滞多久（淘汰排序用；未停滞 = 0）。
    pub fn stalled_for(&self) -> Duration {
        match self.st.lock().expect("legout").stalled_since {
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
