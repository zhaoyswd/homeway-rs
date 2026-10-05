//! 事件总线（语义真源 `baseline:clientcore/facade/bus.go`，spec「事件流」）：
//! 全局单调 seq（每次启动从 0 计、随代际唯一化）+ 环形重放窗（4096 条 / 1MiB）+
//! 域过滤分发 + 游标回放。
//!
//! 「不重不漏」对**总线已发事件**是无条件承诺（在线 = 送达或断连；每订阅者队列
//! 有界 512，满 = overrun：停投 + 标记——连接层收到即发 `goodbye(overrun)` 断连，
//! 前端 resubscribe + 全量重快照恢复）。断连后续播依赖重放窗 + 代际。事件自包含
//! （带 seq），前端按同键比 seq 幂等覆盖（at-least-once 语义）。
//!
//! 并发面（Rust 形态）：一把总线锁（取号/入环/域匹配/拷贝投递目标），投递在锁外
//! 逐订阅者加各自的队列锁；订阅者域集合只在总线锁内改（与 Go 的「订阅域增删只在
//! b.mu 内」同纪律；锁序恒为 总线锁 → 订阅者槽，单向）。订阅回放段（replay）与
//! 门闩（latch）的交付次序保证由 server 的单一 writer 落实（见 server.rs——Go 侧
//! B3 的 heldFrames/复检暂存在单 writer 形态下收敛为复合出队项）。

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::Value;

use super::vocab;

// 总线默认参数（design D5；不改契约形状）。
pub const DEFAULT_RING_ENTRIES: usize = 4096;
pub const DEFAULT_RING_BYTES: usize = 1 << 20;
pub const DEFAULT_SUB_QUEUE: usize = 512;

/// 订阅路径哨兵（server 层映射到错误码）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SubscribeErr {
    /// 游标过旧（超出环形重放窗）或代际失配 → cursor_stale（resync：前端全量重快照，
    /// MUST NOT 静默跳段）。
    #[error("cursor_stale：{0}")]
    Stale(String),
    /// 游标超前于当前序号（值域外）→ bad_request。
    #[error("cursor 超前于当前序号")]
    Future,
    /// 其余（词表外订阅域 / 代际缺失）→ bad_request。
    #[error("{0}")]
    Other(String),
}

/// 总线里的一条事件（= EventBody 的内存形态）。
#[derive(Debug, Clone)]
pub struct Event {
    pub seq: u64,
    pub domain: String,
    pub kind: String,
    pub payload: Value,
}

struct BusInner {
    seq: u64,
    ring: VecDeque<Event>,
    ring_bytes: usize,
    subs: Vec<Arc<Subscriber>>,
}

/// 进程内单例事件总线。代际 = 本次守护进程启动随机（前端持旧代际游标订阅将得到
/// cursor_stale）。
pub struct Bus {
    gen: String,
    ring_cap: usize,
    ring_max_bytes: usize,
    sub_queue: usize,
    inner: Mutex<BusInner>,
}

/// 一个连接的订阅态（queue 有界；满 = overrun）。
pub struct Subscriber {
    queue: Mutex<SubQueue>,
    /// 订阅域集合（**只在总线锁内改**——Bus::subscribe / unsubscribe_domains；
    /// 读面 s_matches 同在总线锁内调用）。
    domains: Mutex<Vec<String>>,
    /// 订阅确认在途门闩：置位期间 writer 不消费在线队列（事件堆在 queue 里；
    /// 队列满照常 overrun——与 Go 门闩期 heldFrames 触顶断连同款终态）。
    latched: AtomicBool,
    /// 订阅回放段（subscribe 在总线锁内拷入；writer 在写出订阅确认后经
    /// [`take_replay`] 原子取走，先于在线队列写出——「确认 → 回放 → 在线」次序）。
    replay: Mutex<Option<Vec<Event>>>,
    /// 订阅视图声明原文（`view=host=<id>[,host=<id>]*`；参与需求合成——facade 期
    /// 语义，B0-2 后续棒消费；空串/未知格式保守忽略）。
    pub view: Mutex<String>,
}

struct SubQueue {
    items: VecDeque<Event>,
    overrun: bool,
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for Subscriber {
    fn default() -> Self {
        Self::new()
    }
}

impl Subscriber {
    fn new() -> Subscriber {
        Subscriber {
            queue: Mutex::new(SubQueue { items: VecDeque::new(), overrun: false }),
            domains: Mutex::new(Vec::new()),
            latched: AtomicBool::new(false),
            replay: Mutex::new(None),
            view: Mutex::new(String::new()),
        }
    }

    /// 非阻塞取一条在线事件（None = 空；调用方随后查 [`Subscriber::overrun`]）。
    pub fn try_pop(&self) -> Option<Event> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).items.pop_front()
    }

    /// 是否已因 overrun 被总线停投（连接层据此 `goodbye(overrun)` 断连）。
    pub fn overrun(&self) -> bool {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).overrun
    }

    /// 门闩置位/清位（订阅确认写出前后；server 单 writer 调用）。
    pub fn set_latched(&self, on: bool) {
        self.latched.store(on, Ordering::SeqCst);
    }

    pub fn latched(&self) -> bool {
        self.latched.load(Ordering::SeqCst)
    }

    /// 原子取走回放段（换出为 None）。
    pub fn take_replay(&self) -> Vec<Event> {
        self.replay.lock().unwrap_or_else(|e| e.into_inner()).take().unwrap_or_default()
    }

    fn domains_guard(&self) -> MutexGuard<'_, Vec<String>> {
        self.domains.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Bus {
    /// 建总线（代际 = 16 字节随机 hex——每次启动唯一）。
    pub fn new() -> Bus {
        let mut rnd = [0u8; 16];
        getrandom::getrandom(&mut rnd).expect("系统随机源不可用");
        Bus::with_generation(hex_str(&rnd))
    }

    pub fn with_generation(gen: String) -> Bus {
        Bus {
            gen,
            ring_cap: DEFAULT_RING_ENTRIES,
            ring_max_bytes: DEFAULT_RING_BYTES,
            sub_queue: DEFAULT_SUB_QUEUE,
            inner: Mutex::new(BusInner { seq: 0, ring: VecDeque::new(), ring_bytes: 0, subs: Vec::new() }),
        }
    }

    pub fn generation(&self) -> &str {
        &self.gen
    }

    /// 当前已发出的最大序号（快照序号源；无事件 = 0）。
    pub fn current_seq(&self) -> u64 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).seq
    }

    /// 发布一条事件（kind/payload 词表校验在 vocab::EventPayload 构造期天然成立）。
    /// 返回发出序号。
    pub fn publish(&self, payload: vocab::EventPayload) -> u64 {
        let ev = Event {
            seq: 0,
            domain: payload.domain().to_owned(),
            kind: payload.kind().to_owned(),
            payload: payload.to_value(),
        };
        self.publish_event(ev)
    }

    fn publish_event(&self, mut ev: Event) -> u64 {
        let matched: Vec<Arc<Subscriber>>;
        {
            let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
            in_.seq += 1;
            ev.seq = in_.seq;
            // 入环（两上限：条数 + payload 字节量；字节量超限先淘汰最老直至装得下
            // 或环空——环形窗淘汰只影响重放，不影响已发事件的 seq 连续性）。
            let payload_bytes = ev.payload.to_string().len();
            while in_.ring.len() == self.ring_cap
                || (!in_.ring.is_empty() && in_.ring_bytes + payload_bytes > self.ring_max_bytes)
            {
                match in_.ring.pop_front() {
                    Some(old) => {
                        in_.ring_bytes =
                            in_.ring_bytes.saturating_sub(old.payload.to_string().len());
                    }
                    None => break,
                }
            }
            in_.ring_bytes += payload_bytes;
            in_.ring.push_back(ev.clone());
            matched = in_
                .subs
                .iter()
                .filter(|s| s.domains_guard().iter().any(|d| d == &ev.domain))
                .cloned()
                .collect();
        }
        // 投递（总线锁外；每订阅者队列锁内非阻塞 push，满 = overrun 停投）。
        for sub in matched {
            let overran = {
                let mut q = sub.queue.lock().unwrap_or_else(|e| e.into_inner());
                if q.overrun {
                    continue;
                }
                if q.items.len() >= self.sub_queue {
                    q.overrun = true;
                    true
                } else {
                    q.items.push_back(ev.clone());
                    false
                }
            };
            if overran {
                self.remove_sub(&sub);
            }
        }
        ev.seq
    }

    /// 建订阅者（未挂到总线；subscribe 时生效）。
    pub fn new_subscriber(&self) -> Arc<Subscriber> {
        Arc::new(Subscriber::new())
    }

    /// 挂订阅 + 游标回放（原子：总线锁内检查游标、回放拷入 replay 段、注册生效）。
    /// 同一订阅者重复订阅 = **替换**（域集合整体换为新载荷集合——spec delta 3b
    /// 钉死，非并集）；replay 段同样 = 覆盖（前端按 seq 幂等去重无损）。
    ///
    ///   - generation 缺失 → `Other`（bad_request；代际声明必填）；
    ///   - generation != 当前代际 → `Stale`（前端全量重快照）；
    ///   - cursor 超前于当前序号 → `Future`（bad_request）；
    ///   - cursor+1 早于环内最老 seq（游标过旧/环已被冲刷）→ `Stale`；
    ///   - 回放 = 窗内 `(cursor, 当前]` 的订阅域事件，拷入 replay（长度只受环窗界）。
    ///
    /// domains 为空数组 = 合法（订阅确认回显空集，不推任何域——前端可用它只取回放）。
    pub fn subscribe(
        &self,
        sub: &Arc<Subscriber>,
        domains: &[String],
        cursor: Option<u64>,
        generation: &str,
        view: &str,
    ) -> Result<(), SubscribeErr> {
        for d in domains {
            if !vocab::valid_domain(d) {
                return Err(SubscribeErr::Other(format!("词表外订阅域 {d:?}")));
            }
        }
        let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if generation.is_empty() {
            return Err(SubscribeErr::Other("订阅必须携带代际（generation 为空）".to_owned()));
        }
        if generation != self.gen {
            return Err(SubscribeErr::Stale(format!(
                "代际失配（订阅带 {generation:?}，当前 {:?}）",
                self.gen
            )));
        }
        let replay = match cursor {
            Some(c) => {
                if c > in_.seq {
                    return Err(SubscribeErr::Future);
                }
                // 环空（尚无事件）时任何 cursor 都可从当前续播；环非空时要求
                // cursor+1 不早于最老（否则窗内已缺段 = cursor_stale，MUST NOT 静默跳段）。
                if let Some(oldest) = in_.ring.front() {
                    if c + 1 < oldest.seq {
                        return Err(SubscribeErr::Stale(format!(
                            "游标 {c} 早于重放窗起点 {}",
                            oldest.seq
                        )));
                    }
                }
                in_.ring
                    .iter()
                    .filter(|ev| ev.seq > c && domains.iter().any(|d| d == &ev.domain))
                    .cloned()
                    .collect::<Vec<_>>()
            }
            None => Vec::new(),
        };
        *sub.replay.lock().unwrap_or_else(|e| e.into_inner()) = Some(replay);
        *sub.view.lock().unwrap_or_else(|e| e.into_inner()) = view.to_owned();
        *sub.domains_guard() = domains.to_vec();
        if !in_.subs.iter().any(|s| Arc::ptr_eq(s, sub)) {
            in_.subs.push(Arc::clone(sub));
        }
        Ok(())
    }

    /// 退订（幂等；摘除即整体停投）。
    pub fn unsubscribe(&self, sub: &Arc<Subscriber>) {
        self.remove_sub(sub);
    }

    /// 从订阅中摘除若干域（幂等；未含域 = 无操作；摘空即整体从总线摘除）。
    /// 域集合的增删只在总线锁内发生（与 publish 的域匹配读面同拍——exec-r1 H1
    /// 同款竞态的纪律）。
    pub fn unsubscribe_domains(&self, sub: &Arc<Subscriber>, domains: &[String]) {
        let mut in_ = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        sub.domains_guard().retain(|d| !domains.contains(d));
        if sub.domains_guard().is_empty() {
            in_.subs.retain(|s| !Arc::ptr_eq(s, sub));
        }
    }

    fn remove_sub(&self, sub: &Arc<Subscriber>) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).subs.retain(|s| !Arc::ptr_eq(s, sub));
    }
}

fn hex_str(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::vocab::EventPayload;

    fn bus() -> Bus {
        Bus::with_generation("gen-1".to_owned())
    }

    fn added(host: &str) -> EventPayload {
        EventPayload::SessionAdded { host: host.to_owned(), name: String::new(), added_at: 0 }
    }

    #[test]
    fn seq_monotonic_and_ring_bounded() {
        let b = bus();
        for i in 0..(DEFAULT_RING_ENTRIES + 100) {
            let seq = b.publish(added(&format!("h{i}")));
            assert_eq!(seq, i as u64 + 1);
        }
        let in_ = b.inner.lock().unwrap();
        assert_eq!(in_.ring.len(), DEFAULT_RING_ENTRIES);
        assert_eq!(in_.ring.front().unwrap().seq, 101);
        assert_eq!(in_.ring.back().unwrap().seq, DEFAULT_RING_ENTRIES as u64 + 100);
    }

    #[test]
    fn subscribe_replay_and_cursors() {
        let b = bus();
        for i in 0..5 {
            b.publish(added(&format!("h{i}")));
        }
        let sub = b.new_subscriber();
        // 代际缺失 / 失配。
        assert!(matches!(b.subscribe(&sub, &["session".into()], None, "", ""), Err(SubscribeErr::Other(_))));
        assert!(matches!(
            b.subscribe(&sub, &["session".into()], None, "gen-2", ""),
            Err(SubscribeErr::Stale(_))
        ));
        // 词表外域。
        assert!(matches!(
            b.subscribe(&sub, &["nope".into()], None, "gen-1", ""),
            Err(SubscribeErr::Other(_))
        ));
        // 正常：cursor=2 → 回放 seq 3..=5。
        b.subscribe(&sub, &[vocab::DOMAIN_SESSION.to_owned()], Some(2), "gen-1", "").unwrap();
        let seqs: Vec<u64> = sub.take_replay().iter().map(|e| e.seq).collect();
        assert_eq!(seqs, vec![3, 4, 5]);
        assert!(sub.take_replay().is_empty(), "取走后为空");
        // 超前游标。
        assert!(matches!(
            b.subscribe(&sub, &["session".into()], Some(99), "gen-1", ""),
            Err(SubscribeErr::Future)
        ));
        // 在线投递。
        b.publish(EventPayload::SessionRemoved { host: "h1".to_owned(), reason: "user".to_owned() });
        assert_eq!(sub.try_pop().unwrap().seq, 6);
        assert!(sub.try_pop().is_none());
    }

    #[test]
    fn overrun_terminates_subscription() {
        let b = bus();
        let sub = b.new_subscriber();
        b.subscribe(&sub, &[vocab::DOMAIN_SESSION.to_owned()], None, "gen-1", "").unwrap();
        sub.set_latched(true); // 门闩期 writer 不消费 → 队列堆满
        for i in 0..=DEFAULT_SUB_QUEUE {
            b.publish(added(&format!("h{i}")));
        }
        assert!(sub.overrun(), "队列满应标记 overrun");
        // 停投后再发不炸（overrun 后跳过投递）。
        b.publish(added("x"));
        let in_ = b.inner.lock().unwrap();
        assert!(in_.subs.is_empty(), "overrun 后订阅者应被摘除");
    }

    #[test]
    fn replace_semantics_resets_domains() {
        let b = bus();
        b.publish(added("h"));
        let sub = b.new_subscriber();
        b.subscribe(&sub, &[vocab::DOMAIN_SESSION.to_owned()], None, "gen-1", "").unwrap();
        // 替换订阅：换 link 域——session 域事件不再投给该订阅者。
        b.subscribe(&sub, &[vocab::DOMAIN_LINK.to_owned()], None, "gen-1", "").unwrap();
        b.publish(added("h2"));
        assert!(sub.try_pop().is_none());
        b.publish(EventPayload::LinkChanged {
            host: "h2".to_owned(),
            via: "direct".to_owned(),
            ep: "1.2.3.4:1".to_owned(),
            rtt_ms: 5,
            at: 1,
        });
        assert_eq!(sub.try_pop().unwrap().kind, vocab::KIND_LINK_CHANGED);
    }

    #[test]
    fn unsubscribe_domains_partial() {
        let b = bus();
        let sub = b.new_subscriber();
        b.subscribe(
            &sub,
            &[vocab::DOMAIN_SESSION.to_owned(), vocab::DOMAIN_LINK.to_owned()],
            None,
            "gen-1",
            "",
        )
        .unwrap();
        b.unsubscribe_domains(&sub, &[vocab::DOMAIN_SESSION.to_owned()]);
        b.publish(added("h"));
        assert!(sub.try_pop().is_none(), "session 域已摘");
        // 摘空 = 整体停投。
        b.unsubscribe_domains(&sub, &[vocab::DOMAIN_LINK.to_owned()]);
        let in_ = b.inner.lock().unwrap();
        assert!(in_.subs.is_empty());
    }
}
