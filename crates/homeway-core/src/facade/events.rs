//! 状态推送的最小等价面（7d；真 IPC 长连接推送是 ArkTS 侧的——跨进程推送通道在
//! 扩展进程，Rust 核在扩展进程内。本模块给 Rust 侧一个**可轮询**的等价语义：
//! 有界事件队列 + 最新快照缓存，NAPI 壳/轮询器从这里取）。
//!
//! 语义对照：App 侧的 IPC 推送是「状态变更即刻送达 + 冷启动读快照」；本面的等价 =
//! - `push`：状态/事件入队（非阻塞；队满丢最旧并记 drop 计数——推送语义宁可丢旧
//!   不阻塞核内路径）；
//! - `snapshot`：最新一份状态（冷启动兜底）；
//! - `drain`：取走全部积压事件（轮询器 250ms/拍等价节拍由调用方定）。

use std::collections::VecDeque;
use std::sync::Mutex;

/// 事件队列上限（状态类事件的高水位——正常每分钟个位数）。
pub const QUEUE_CAP: usize = 128;

/// 核侧事件（App 界面驱动的最小集：状态迁移 + 链路变化 + 失败归因）。
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum CoreEvent {
    /// 服务会话/隧道状态迁移（state 词面 = 状态 JSON 的 state 同源）。
    StateChanged { from: String, to: String, reason: String },
    /// 链路确立/切换（via/ep 词面 = link 段同源）。
    LinkChanged { via: String, ep: String },
    /// 恢复阶梯动作（档位 + 归因；判据行同源词面）。
    Recover { level: u8, cause: String },
}

/// 有界事件队列 + 最新快照（线程安全）。
#[derive(Debug, Default)]
pub struct EventHub {
    queue: Mutex<VecDeque<CoreEvent>>,
    dropped: Mutex<u64>,
    latest: Mutex<Vec<CoreEvent>>,
}

impl EventHub {
    pub fn new() -> Self {
        Self::default()
    }

    /// 入队（非阻塞；队满丢最旧 + drop 计数）。
    pub fn push(&self, ev: CoreEvent) {
        let mut q = self.queue.lock().expect("事件锁中毒");
        if q.len() >= QUEUE_CAP {
            q.pop_front();
            *self.dropped.lock().expect("事件锁中毒") += 1;
        }
        q.push_back(ev);
    }

    /// 取走全部积压（轮询器每拍调；空拍 = 空 Vec）。
    pub fn drain(&self) -> Vec<CoreEvent> {
        let mut q = self.queue.lock().expect("事件锁中毒");
        q.drain(..).collect()
    }

    /// 队满丢弃累计（诊断面）。
    pub fn dropped(&self) -> u64 {
        *self.dropped.lock().expect("事件锁中毒")
    }

    /// 最新状态事件缓存（冷启动快照兜底——`push_state` 时刷新）。
    pub fn push_state(&self, ev: CoreEvent) {
        self.push(ev.clone());
        let mut latest = self.latest.lock().expect("快照锁中毒");
        latest.push(ev);
        if latest.len() > 4 {
            latest.remove(0);
        }
    }

    /// 冷启动读（最近几条状态事件）。
    pub fn latest(&self) -> Vec<CoreEvent> {
        self.latest.lock().expect("快照锁中毒").clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_drain_fifo() {
        let hub = EventHub::new();
        hub.push(CoreEvent::LinkChanged { via: "direct".into(), ep: "1.2.3.4:41641".into() });
        hub.push(CoreEvent::Recover { level: 1, cause: "巡检失败".into() });
        let got = hub.drain();
        assert_eq!(got.len(), 2);
        assert_eq!(got[0], CoreEvent::LinkChanged { via: "direct".into(), ep: "1.2.3.4:41641".into() });
        // 排空后再取 = 空
        assert!(hub.drain().is_empty());
    }

    /// 队满丢最旧 + drop 计数（推送语义：宁可丢旧不阻塞）。
    #[test]
    fn bounded_drop_oldest() {
        let hub = EventHub::new();
        for i in 0..(QUEUE_CAP + 10) {
            hub.push(CoreEvent::Recover { level: 1, cause: format!("c{i}") });
        }
        assert_eq!(hub.dropped(), 10);
        let got = hub.drain();
        assert_eq!(got.len(), QUEUE_CAP);
        assert_eq!(got[0], CoreEvent::Recover { level: 1, cause: format!("c{}", 10) });
    }

    /// 状态快照缓存：冷启动读最近 4 条。
    #[test]
    fn state_snapshot_cache() {
        let hub = EventHub::new();
        for st in ["starting", "ready", "failed", "starting", "ready"] {
            hub.push_state(CoreEvent::StateChanged { from: "-".into(), to: st.into(), reason: String::new() });
        }
        let latest = hub.latest();
        assert_eq!(latest.len(), 4);
        assert_eq!(latest[3], CoreEvent::StateChanged { from: "-".into(), to: "ready".into(), reason: String::new() });
    }
}
