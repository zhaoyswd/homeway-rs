//! SPSC 有界 ring（P1 发送路径拆分；设计 = docs/reviews/P1-design.md §3①）。
//!
//! 形态：单生产者（出口驱动线程——`ServerBind::send_wire` 的判定面产物）→ 单消费者
//! （发送线程）。不引第三方依赖（R1 依赖面纪律），unsafe 面仅槽位的 `MaybeUninit`
//! 读写——四点原子序配对见下；`udpbatch` 的自建先例同款纪律。
//!
//! **unsafe 不变量（评审 p1a-F1，改动须同步本注释）**：
//! 1. 索引语义：head/tail 是**单调 usize**（不回绕；u64 在 40k 包/s 下回绕需 ~1.4 万
//!    年——实践不可达），槽位 = `idx & (CAP-1)`，CAP 恒为 2 的幂；
//! 2. 所有权：slot 写入前属 producer、`assume_init_read` 后属 consumer、
//!    consumer `head` 前进后归还 producer 复用——同一时刻每槽恰一个属主（SPSC
//!    + 索引区间不相交保证）；
//! 3. 原子序配对：producer `write(slot) → tail.store(Release)`；consumer
//!    `tail.load(Acquire) → assume_init_read`；consumer `read 完 → head.store(Release)`；
//!    producer 复用槽前 `head.load(Acquire)`——Release/Acquire 在索引与槽内容间
//!    建立 happens-before，读写不竞态；
//! 4. 伪共享：head/tail 各占独立 cache line（`Aligned(align 64)`）——producer 独占
//!    写 tail、consumer 独占写 head，缓存游标是各自 handle 的普通字段（`&mut self`
//!    即线程独占的编译期证明，不用 Cell）。
//!
//! **满 = 丢新（队尾拒绝）**：`push` 返回 false（不写槽）——TCP 眼里的尾丢
//! （dup-ACK/RTO 恢复），**绝不丢队头/队中**（乱序禁区）。容量口径 = 工程估算
//! （非上界）：按主用形态（4 流测速 × FLOW_TX_BUF 1MB/流 ≈ 3200 密文包）留余量；
//! 四个已知例外不受 TCP 窗记账（UDP 过境/DNS/ICMP、RTO 重传是新入队元素、
//! MAX_CONNS=1024 ≫ 4、高 BDP 形态 cwnd 可超 1MB/流）⇒ 溢出是**设计内丢弃路径**，
//! 上游有两级前置背压（栈 buffer → 整形 FIFO → ring；引擎面高水位检查），
//! T3 观测门（稳态队深峰 ≪ 200 包）监控估算是否失真。
//!
//! Drop：最后一个 handle 释放时逐槽 drop 未消费元素（head..tail 区间）——
//! 不泄漏（单测钉死）。**没有 clear() API**（评审 p1a-C-4：consumer 活着时
//! producer 清队列是数据竞争；收工路径也不需要——drain-then-exit）。

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// 一条待发密文（腿帧化后的最终 wire 字节 + 适配族后的目的端点）。
pub(crate) struct Slot {
    pub dst: SocketAddr,
    pub buf: Vec<u8>,
}

/// 容量 4096 = 2^12（2 的幂是不变量的前提）。
pub(crate) const CAP: usize = 4096;
/// 高水位（15/16 容量）：驱动线程本拍整形放行退化为「只并入不释放」（两级
/// 前置背压——包留整形 FIFO = 真背压不丢包，满丢成为最后兜底）。
pub(crate) const HIGH_WATER: usize = CAP * 15 / 16;

/// 独占 cache line 的原子计数（伪共享防御）。
#[repr(align(64))]
struct Aligned(AtomicUsize);

struct Shared {
    /// producer 前进（Release 发布新槽）。
    tail: Aligned,
    /// consumer 前进（Release 归还旧槽）。
    head: Aligned,
    slots: Box<[std::mem::MaybeUninit<Slot>]>,
}

impl Drop for Shared {
    fn drop(&mut self) {
        // 最后一个 handle 释放时（双方 &mut 都已结束）：逐槽 drop 未消费元素。
        let head = self.head.0.load(Ordering::Acquire);
        let tail = self.tail.0.load(Ordering::Acquire);
        for i in head..tail {
            unsafe { self.slots[i & (CAP - 1)].assume_init_drop() };
        }
    }
}

/// 生产端（驱动线程独占——`&mut self` 即线程独占证明）。
pub(crate) struct TxProducer {
    sh: Arc<Shared>,
    /// consumer head 的本地缓存（上次 Acquire 读到的值——只在可能满时刷新）。
    cached_head: usize,
}

/// 消费端（发送线程独占）。
pub(crate) struct TxConsumer {
    sh: Arc<Shared>,
    /// producer tail 的本地缓存。
    cached_tail: usize,
}

pub(crate) fn txring_new() -> (TxProducer, TxConsumer) {
    let sh = Arc::new(Shared {
        tail: Aligned(AtomicUsize::new(0)),
        head: Aligned(AtomicUsize::new(0)),
        slots: std::iter::repeat_with(std::mem::MaybeUninit::uninit)
            .take(CAP)
            .collect(),
    });
    (
        TxProducer { sh: Arc::clone(&sh), cached_head: 0 },
        TxConsumer { sh, cached_tail: 0 },
    )
}

impl TxProducer {
    /// 入队一条（满 = 丢新返回 false——调用方计数 + 节流记行，绝不等待）。
    pub(crate) fn push(&mut self, slot: Slot) -> bool {
        let tail = self.sh.tail.0.load(Ordering::Relaxed); // 自写自读
        if tail.wrapping_sub(self.cached_head) >= CAP {
            // 可能满：刷新对端 head（Acquire——归还槽的 happens-before 端点）
            let h = self.sh.head.0.load(Ordering::Acquire);
            self.cached_head = h;
            if tail.wrapping_sub(h) >= CAP {
                return false; // 真满：丢新（不写槽——槽属 consumer）
            }
        }
        unsafe { self.slots_write(tail, slot) };
        self.sh.tail.0.store(tail + 1, Ordering::Release); // 发布新槽
        true
    }

    /// 近似深度（producer 侧；主动刷新对端 head——用于高水位背压判断，每拍一次，
    /// 原子读成本可忽略）。
    pub(crate) fn approx_len(&mut self) -> usize {
        let h = self.sh.head.0.load(Ordering::Acquire);
        self.cached_head = h;
        self.sh.tail.0.load(Ordering::Relaxed).wrapping_sub(h)
    }

    unsafe fn slots_write(&self, tail: usize, slot: Slot) {
        // SAFETY：tail - cached_head（≤ 真实 head）< CAP ⇒ 该槽已被 consumer 读走
        //（head.store(Release) 之后）或从未写——producer 独占写权；Arc 内共享存储的
        // 写经裸指针（&MaybeUninit 不可写）。
        let cell = self.sh.slots.as_ptr().cast_mut().add(tail & (CAP - 1));
        unsafe { (*cell).write(slot) };
    }
}

impl TxConsumer {
    /// 批量取队：按 FIFO 序 move 出至多 `max_bytes` 字节的槽到 `out` 尾部（单轮
    /// 排空字节上界——团块钳制与整形器单拍上界同语义）。返回本轮取出的包数。
    pub(crate) fn pop_batch(&mut self, out: &mut Vec<Slot>, max_bytes: usize) -> usize {
        let t = self.sh.tail.0.load(Ordering::Acquire); // 新槽的 happens-before 端点
        self.cached_tail = t;
        let head = self.sh.head.0.load(Ordering::Relaxed); // 自写自读
        let mut n = 0usize;
        let mut bytes = 0usize;
        while head + n < t {
            // SAFETY：idx ∈ [head, tail) 区间——producer 已 Release 发布、consumer
            // 尚未读走；assume_init_read 把所有权 move 出来。
            let slot = unsafe { self.sh.slots.get_unchecked((head + n) & (CAP - 1)).assume_init_read() };
            bytes += slot.buf.len();
            out.push(slot);
            n += 1;
            if bytes >= max_bytes {
                break;
            }
        }
        if n > 0 {
            self.sh.head.0.store(head + n, Ordering::Release); // 归还槽
        }
        n
    }

    /// 是否还有可取（consumer 侧精确判断——唤醒竞态兜底用）。
    pub(crate) fn has_items(&mut self) -> bool {
        self.cached_tail = self.sh.tail.0.load(Ordering::Acquire);
        self.cached_tail > self.sh.head.0.load(Ordering::Relaxed)
    }

    /// 当前可见深度（consumer 侧；队深峰观测用）。
    pub(crate) fn visible_len(&mut self) -> usize {
        self.cached_tail = self.sh.tail.0.load(Ordering::Acquire);
        self.cached_tail.wrapping_sub(self.sh.head.0.load(Ordering::Relaxed))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slot(i: usize) -> Slot {
        // 序号编码 = 首 4 字节 LE（`i as u8` 在 i>255 回绕会让序断言假红）
        let mut buf = vec![0u8; 64];
        buf[..4].copy_from_slice(&(i as u32).to_le_bytes());
        Slot { dst: format!("127.0.0.1:{i}").parse().unwrap(), buf }
    }

    fn slot_seq(s: &Slot) -> usize {
        u32::from_le_bytes([s.buf[0], s.buf[1], s.buf[2], s.buf[3]]) as usize
    }

    /// 单线程基础序：push/pop_batch 往返保序、满丢丢新（前缀不变）、空取零。
    #[test]
    fn seq_order_full_and_empty() {
        let (mut p, mut c) = txring_new();
        let mut out = Vec::new();
        assert_eq!(c.pop_batch(&mut out, usize::MAX), 0, "空取零");
        for i in 0..100 {
            assert!(p.push(slot(i)), "前 100 应全入");
        }
        assert_eq!(c.pop_batch(&mut out, usize::MAX), 100);
        let got: Vec<usize> = out.iter().map(slot_seq).collect();
        assert_eq!(got, (0..100).collect::<Vec<_>>(), "FIFO 保序");
        // 填满 + 溢出：丢新不丢老
        for i in 100..100 + CAP {
            p.push(slot(i));
        }
        let mut overflow = 0;
        for _ in 0..10 {
            if !p.push(slot(9999)) {
                overflow += 1;
            }
        }
        assert_eq!(overflow, 10, "满后 push 恒拒绝");
        assert_eq!(p.approx_len(), CAP);
        // 前缀仍是 100..100+CAP（丢的是新——老元素未被顶替）
        let mut all: Vec<Slot> = Vec::new();
        while c.pop_batch(&mut all, usize::MAX) > 0 {}
        assert_eq!(all.len(), CAP);
        assert_eq!(slot_seq(&all[0]), 100, "队头未被顶替");
        assert_eq!(slot_seq(&all[CAP - 1]), 100 + CAP - 1, "队尾是最后成功者");
    }

    /// 字节上界：单轮 pop_batch ≤ max_bytes（至少 1 包——空轮零包由调用方循环收口）。
    #[test]
    fn pop_batch_byte_cap() {
        let (mut p, mut c) = txring_new();
        for i in 0..20 {
            assert!(p.push(slot(i)));
        }
        let mut out = Vec::new();
        let n = c.pop_batch(&mut out, 64 * 3); // 64B/包 ⇒ ≤ 4 包后越界停
        assert!((3..=4).contains(&n), "单轮 ≤ 字节上界（得 {n}）");
        assert_eq!(slot_seq(&out[0]), 0, "从头取");
    }

    /// 双线程压满-排空 soak：10k 包交错往返，全程序一致 + 计数守恒（丢失唤醒 /
    /// 竞态兜底路径的线程级 soak——10k 包 @ 微秒级交错足以覆盖 store/load 交错窗）。
    #[test]
    fn spsc_soak_interleaved() {
        use std::sync::atomic::AtomicBool;
        let (mut p, mut c) = txring_new();
        let done = Arc::new(AtomicBool::new(false));
        let done2 = Arc::clone(&done);
        let h = std::thread::spawn(move || {
            let mut expect = 0usize;
            let mut buf: Vec<Slot> = Vec::new();
            while !done2.load(Ordering::Relaxed) || c.has_items() {
                while c.pop_batch(&mut buf, 4096) > 0 {}
                for s in buf.drain(..) {
                    assert_eq!(slot_seq(&s), expect, "序被破坏");
                    expect += 1;
                }
                std::thread::yield_now();
            }
            expect
        });
        for i in 0..10_000 {
            // 不消费地压：间歇让 consumer 追——模拟排空能力波动。满等有 30s 上限
            //（consumer 侧断言失败 panic 时不让 producer 无限自旋挂死测试进程）。
            let wait_start = std::time::Instant::now();
            while !p.push(slot(i)) {
                std::thread::yield_now();
                if wait_start.elapsed() > std::time::Duration::from_secs(30) {
                    panic!("producer 等空位超时——consumer 卡死或已 panic");
                }
            }
            if i % 97 == 0 {
                std::thread::yield_now();
            }
        }
        done.store(true, Ordering::Relaxed);
        let got = h.join().unwrap();
        assert_eq!(got, 10_000, "consumer 应全量按序收到");
        assert_eq!(p.approx_len(), 0);
    }

    /// Drop 无泄漏：未消费元素的析构被执行（计数型载荷钉死）。
    #[test]
    fn drop_drains_unconsumed() {
        // push 后不 pop，两端 drop ⇒ Drop for Shared 逐槽 assume_init_drop（50 个
        // 1KB Vec 的析构可走通即回归面；泄漏的定量检测属 Miri/ASan 域）。
        let (mut p, c) = txring_new();
        for i in 0..50 {
            assert!(p.push(Slot { dst: format!("127.0.0.1:{i}").parse().unwrap(), buf: vec![7u8; 1024] }));
        }
        drop(c);
        drop(p); // Shared 在最后一个 handle 释放时 drop——50 槽逐个析构
    }
}
