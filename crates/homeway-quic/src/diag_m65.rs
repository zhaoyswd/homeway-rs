//! **M6.5 临时诊断插桩 v2**（本批收口前删除；诊断构建缺省开）。
//!
//! v1 用 `Instant`（墙钟）测段——把「等待」也算进去了（读到：read 线程 poll 墙钟 32 s，
//! 绝大部分是等包）。v2 改用**线程 CPU 钟**（`CLOCK_THREAD_CPUTIME_ID`）⇒ 段耗时 = CPU
//! 时间（不含等待），并把每段的调用次数一并记下（per-call 成本 = ns/n）。
//!
//! **开销控制**：每段按 1/[`SAMPLE_EVERY`] 采样（线程局部计数器），未采样的调用零开销
//! （不取钟）。报告按 per-call 均值 × 段总次数还原。
//!
//! 纪律：本文件是**临时物**——M6.5 收口时整文件删除。

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// 线程 CPU 钟（纳秒；Linux `CLOCK_THREAD_CPUTIME_ID`——非 vDSO，调用本身≈0.5–1µs，
/// 故只在采样点取）。
pub(crate) fn cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY：ts 是本栈上的有效 timespec；clock_gettime 只写它。
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        return 0;
    }
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

/// 采样闸（线程局部）：返回 `true` 时本线程本次调用应测时。
///
/// **v2 教训**：`v % 64 == 0` 在固定步距（如每轮恰好 4 次 start）下会永远只命中同一段
/// （奇偶/周期共振）——读线程的 read 段就 0 样本。v3 改用**散列闸**（对任何步距都均匀）。
pub(crate) fn sampled() -> bool {
    thread_local! {
        static N: Cell<u32> = const { Cell::new(0) };
    }
    N.with(|n| {
        let v = n.get().wrapping_add(1);
        n.set(v);
        let mut h = v.wrapping_mul(0x9E37_79B9);
        h ^= h >> 15;
        h = h.wrapping_mul(0x85EB_CA6B);
        h ^= h >> 13;
        (h & 63) == 0
    })
}

/// 一段的累计（采样点的 CPU ns + 调用次数）。
#[derive(Default)]
pub(crate) struct Seg {
    pub ns: AtomicU64,
    pub n: AtomicU64,
}

impl Seg {
    /// 采样点：返回起始 CPU 钟（未采样 ⇒ `None`）。
    #[inline]
    pub(crate) fn start(&self) -> Option<u64> {
        if sampled() {
            Some(cpu_ns())
        } else {
            None
        }
    }

    /// 采样点收尾（t = `start` 的返回值）。
    #[inline]
    pub(crate) fn end(&self, t: Option<u64>) {
        if let Some(t0) = t {
            let now = cpu_ns();
            self.ns.fetch_add(now.saturating_sub(t0), Ordering::Relaxed);
            self.n.fetch_add(1, Ordering::Relaxed);
        }
    }

}

/// 全段表（诊断期内静态；字段名即段名）。
#[derive(Default)]
pub(crate) struct Diag {
    // —— TUN 读线程（uplink 入口）——
    pub read_syscall: Seg,
    pub read_poll: Seg,
    pub read_chan_send: Seg,
    // —— 岛线程 ——
    pub sock_recv: Seg,
    pub sock_send: Seg,
    pub island_tunpkt: Seg,
    pub pump_deliver: Seg,
    // —— TUN 写线程（downlink 出口）——
    pub write_syscall: Seg,
    // —— 计数（不测时的量）——
    pub read_ok: AtomicU64,
    pub read_eagain: AtomicU64,
    pub read_eintr: AtomicU64,
    pub read_bytes: AtomicU64,
    /// 轮询调用次数。
    pub poll_calls: AtomicU64,
    /// 轮询**立即返回**（wall < 200µs）的次数——热自旋嫌疑位。
    pub poll_imm: AtomicU64,
    /// 轮询报可读、紧随其后的 read 仍 EAGAIN 的次数（真自旋签名）。
    pub poll_but_eagain: AtomicU64,
    pub island_pkts: AtomicU64,
    pub pump_pkts: AtomicU64,
    /// socket 收发调用次数（免去按采样比推算）。
    pub sock_recv_calls: AtomicU64,
    pub sock_send_calls: AtomicU64,
    pub write_pkts: AtomicU64,
    pub write_wb: AtomicU64,
    /// 采样时钟自身的开销标定（ns/次，进程启动时算一次）。
    pub clock_overhead_ns: AtomicU64,
}

fn enabled_cached() -> bool {
    static ON: AtomicBool = AtomicBool::new(false);
    static INIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *INIT.get_or_init(|| {
        // 诊断构建：缺省 **开**（设备侧无法注入 env）；`HOMEWAY_M65_DIAG=0` 显式关。
        let on = std::env::var("HOMEWAY_M65_DIAG").map(|v| v != "0").unwrap_or(true);
        ON.store(on, Ordering::Relaxed);
        on
    })
}

/// 插桩开关（诊断构建缺省开；读一次缓存）。
pub(crate) fn on() -> bool {
    enabled_cached()
}

/// 全局计数表。
pub(crate) fn diag() -> &'static Diag {
    static D: std::sync::OnceLock<Diag> = std::sync::OnceLock::new();
    D.get_or_init(|| {
        let d = Diag::default();
        // 时钟开销标定：1000 次 cpu_ns 对
        let t0 = cpu_ns();
        let mut acc = 0u64;
        for _ in 0..1000 {
            acc = acc.wrapping_add(std::hint::black_box(cpu_ns()));
        }
        let t1 = cpu_ns();
        let _ = acc;
        d.clock_overhead_ns.store((t1.saturating_sub(t0)) / 1000, Ordering::Relaxed);
        d
    })
}

/// 一行诊断（读线程周期性驱动；报 per-call CPU ns 与计数）。
pub(crate) fn dump_line(tag: &str) -> String {
    let d = diag();
    let g = |a: &AtomicU64| a.load(Ordering::Relaxed);
    format!(
        "m65diag[{tag}] clk_ovh_ns={} read(ok={} ea={} eintr={} B={} syscall={}n/{}ns poll={}calls/{}imm/{}pbe/{}n/{}ns snd={}n/{}ns) island(pkts={} tunpkt={}n/{}ns pump={}n/{}ns) sock(recv={}c/{}n/{}ns send={}c/{}n/{}ns) write(pkts={} wb={} syscall={}n/{}ns)",
        g(&d.clock_overhead_ns),
        g(&d.read_ok),
        g(&d.read_eagain),
        g(&d.read_eintr),
        g(&d.read_bytes),
        g(&d.read_syscall.n),
        g(&d.read_syscall.ns),
        g(&d.poll_calls),
        g(&d.poll_imm),
        g(&d.poll_but_eagain),
        g(&d.read_poll.n),
        g(&d.read_poll.ns),
        g(&d.read_chan_send.n),
        g(&d.read_chan_send.ns),
        g(&d.island_pkts),
        g(&d.island_tunpkt.n),
        g(&d.island_tunpkt.ns),
        g(&d.pump_deliver.n),
        g(&d.pump_deliver.ns),
        g(&d.sock_recv_calls),
        g(&d.sock_recv.n),
        g(&d.sock_recv.ns),
        g(&d.sock_send_calls),
        g(&d.sock_send.n),
        g(&d.sock_send.ns),
        g(&d.write_pkts),
        g(&d.write_wb),
        g(&d.write_syscall.n),
        g(&d.write_syscall.ns),
    )
}
