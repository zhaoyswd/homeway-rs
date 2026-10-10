//! **M6.6 差分量测插桩**（临时物；本批收口整文件删除）。
//!
//! 定位（M6.6 任务 A）：**同一套插桩同时量两臂**，产出「逐段 Δ 表 + 系统调用计数/单元」。
//! 因此本文件在**两个臂树里逐字节相同**（新臂 = `homeway-quic`，旧臂 = `homeway-core`，
//! 旧锚 `4841b20`）——只有**挂点**按各自数据面结构落（见两树各自的挂点表注释）。
//!
//! 三类读数：
//! 1. **段计时**：线程 CPU 钟（`CLOCK_THREAD_CPUTIME_ID`）+ 散列闸 1/64 采样（未采样零开销）。
//!    只量**调用点自己**的 CPU 时间（不含等待）——per-call 均值 × 精确次数 = 段成本。
//! 2. **精确计数**：每段调用次数、收发字节、轮询迭代数（原子 `Relaxed`；每段**单写线程**
//!    设计 ⇒ 无跨线程热点行）。
//! 3. **校准参考**（`cal_*`）：独立标定线程每 1s 量两件事——纯 ALU 环（频率/时钟代理）与
//!    管道 `write` 微系统调用（内核进出成本代理）。用途：**把「同样调用为何更贵」拆成
//!    「当时这台机器更贵（环境/频率/争用）」 vs 「本臂真多做了事」**——跨臂比 per-call
//!    成本必须带这一列，否则把环境漂移误读成结构差异（M6.5 的教训）。
//!
//! 纪律：本文件是**临时物**——M6.6 收口时整文件删除（`grep -rn "m66_"` 零命中）。
#![allow(dead_code)]

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// 线程 CPU 钟（纳秒；非 vDSO，调用本身≈0.5–1µs ⇒ 只在采样点取）。
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

/// 采样闸（线程局部散列：对任何调用步距都均匀——M6.5 v3 的教训：取模闸会与固定步距共振）。
#[inline]
pub(crate) fn sampled() -> bool {
    use std::cell::Cell;
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

/// 一段（**单写线程**约定：每段只由一个线程写，读侧只在 dump 时读）。
#[derive(Default)]
pub(crate) struct Seg {
    ns: AtomicU64,
    n: AtomicU64,
}

impl Seg {
    /// 采样起点（未采样 ⇒ `None`）。
    #[inline]
    pub(crate) fn start(&self) -> Option<u64> {
        if on() && sampled() {
            Some(cpu_ns())
        } else {
            None
        }
    }

    /// 采样收尾。
    #[inline]
    pub(crate) fn end(&self, t: Option<u64>) {
        if let Some(t0) = t {
            let now = cpu_ns();
            self.ns.fetch_add(now.saturating_sub(t0), Ordering::Relaxed);
            self.n.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// 只计数不计时（未采样也要精确次数时用——次数在 `end` 里采样式累计，
    /// 需要「精确全量次数」的段另有独立计数器）。
    #[inline]
    pub(crate) fn tick(&self) {
        self.n.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn count(&self) -> u64 {
        self.n.load(Ordering::Relaxed)
    }

    /// 采样均值（ns/call；无样本 ⇒ 0）。
    pub(crate) fn mean_ns(&self) -> u64 {
        let n = self.n.load(Ordering::Relaxed);
        if n == 0 {
            0
        } else {
            self.ns.load(Ordering::Relaxed) / n
        }
    }
}

fn cnt() -> &'static [AtomicU64] {
    static C: std::sync::OnceLock<Vec<AtomicU64>> = std::sync::OnceLock::new();
    C.get_or_init(|| (0..C_N).map(|_| AtomicU64::new(0)).collect())
}

/// 精确计数位（不采样；`add` 用 `Relaxed`）。
pub(crate) const C_DN_WRITE: usize = 0; // TUN 写（下行投递）——包数
pub(crate) const C_DN_BYTES: usize = 1; // TUN 写字节
pub(crate) const C_UP_READ: usize = 2; // TUN 读成功（上行包）
pub(crate) const C_UP_BYTES: usize = 3; // TUN 读字节
pub(crate) const C_UP_EAGAIN: usize = 4; // TUN 读 EAGAIN
pub(crate) const C_UDP_RX: usize = 5; // UDP 收（系统调用次数）
pub(crate) const C_UDP_TX: usize = 6; // UDP 发（系统调用次数）
pub(crate) const C_RX_PKTS: usize = 7; // 协议层交付的明文包数（= 单位数）
pub(crate) const C_LOOP: usize = 8; // 引擎/岛主循环迭代数
pub(crate) const C_WAKE: usize = 9; // 跨线程唤醒次数（通道投递 + 管道写合计）
pub(crate) const C_READ_CALLS: usize = 10; // TUN 读调用总数（含 EAGAIN）
pub(crate) const C_POLL_CALLS: usize = 11; // TUN 读面 poll 调用总数
pub(crate) const C_N: usize = 12;

#[inline]
pub(crate) fn add(slot: usize, v: u64) {
    if ON.load(Ordering::Relaxed) {
        cnt()[slot].fetch_add(v, Ordering::Relaxed);
    }
}

pub(crate) fn get(slot: usize) -> u64 {
    cnt()[slot].load(Ordering::Relaxed)
}

/// 段集合（**跨两臂同名同义**；每段单写线程）。
#[derive(Default)]
pub(crate) struct Diag {
    /// TUN fd 写（`write(2)`，一包一次）——新臂=写线程 / 旧臂=引擎。
    pub(crate) tun_write: Seg,
    /// TUN fd 读（`read(2)`；含 EAGAIN 返——次数见 `C_UP_EAGAIN`）。
    pub(crate) tun_read: Seg,
    /// TUN 读面 `poll(2)` 回退（非阻塞形态才有）。
    pub(crate) tun_read_poll: Seg,
    /// UDP 收（**只量系统调用本身**：`recvfrom`；不含帧解码/剥壳）。
    pub(crate) udp_recv: Seg,
    /// UDP 发（**只量系统调用本身**：`sendto`；不含封帧）。
    pub(crate) udp_send: Seg,
    /// 主循环等待/迭代（新臂=岛 `select` 一轮 / 旧臂=引擎 `poll(2)`）。
    pub(crate) drv_poll: Seg,
    /// 主循环一轮的**全部**工作（新臂=`run_driver` 一轮 / 旧臂=`pump_once`）。
    pub(crate) step: Seg,
    /// 下行协议解析**用户态**部分（新臂=腿帧剥壳+紧凑拷贝 / 旧臂=帧解码+拷出）。
    pub(crate) proto_dn: Seg,
    /// 下行密码学（旧臂=boringtun `decapsulate`；新臂=quinn 内部 ⇒ 0，走 residual 差集）。
    pub(crate) proto_dec: Seg,
    /// 上行密码学（旧臂=boringtun `encapsulate`；新臂=0，见上）。
    pub(crate) proto_enc: Seg,
    /// 上行出厂用户态（新臂=`send_datagram_checked` 全量 / 旧臂=帧编码+腿帧标签）。
    pub(crate) proto_up: Seg,
    /// 跨线程投递：读线程 → 岛/引擎（新臂=通道 `send` / 旧臂=mpsc `send` + 管道 `write`）。
    pub(crate) xwake_in: Seg,
    /// 跨线程投递：岛 → 写线程（**新臂独有**：批抽干 + `to_vec` **同步**部分；不含 await）。
    pub(crate) xwake_out: Seg,
    /// 跨线程投递：入队 + 唤醒（**新臂独有**；同步）。
    pub(crate) xout_push: Seg,
    /// 写线程/引擎的唤醒接收（新臂=`rx.recv()` 的 futex 面；旧臂=管道 drain `read`）。
    pub(crate) wake_recv: Seg,
    /// 命令处置（新臂=`handle_cmd` / 旧臂=`handle_cmd`）。
    pub(crate) cmd_handle: Seg,
    /// 校准：纯 ALU 环 ns/迭代（最近一次；频率/时钟代理）。
    pub(crate) cal_alu_ns: AtomicU64,
    /// 校准：管道 `write(2)` ns/次（最近一次；内核进出代理）。
    pub(crate) cal_sys_ns: AtomicU64,
    /// 校准：ALU 环次数 / 管道写次数（换算用）。
    pub(crate) cal_alu_n: AtomicU64,
    pub(crate) cal_sys_n: AtomicU64,
    /// dump 序号（每次 dump 自增）。
    pub(crate) seq: AtomicU64,
    /// dump 节流计数（下行包数累计，由 dump 线程维护）。
    pub(crate) dump_acc: AtomicU64,
    /// 进程启动时刻（uptime 列）。
    pub(crate) t0: AtomicU64,
}

static ON: AtomicBool = AtomicBool::new(false);
static INIT: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// 插桩开关（诊断构建缺省**开**——设备侧无法注入 env；`HOMEWAY_M66_DIAG=0` 显式关）。
pub(crate) fn on() -> bool {
    *INIT.get_or_init(|| {
        let v = std::env::var("HOMEWAY_M66_DIAG").map(|v| v != "0").unwrap_or(true);
        ON.store(v, Ordering::Relaxed);
        if v {
            let d = diag();
            d.t0.store(process_ms(), Ordering::Relaxed);
        }
        v
    })
}

/// 全局表。
pub(crate) fn diag() -> &'static Diag {
    static D: std::sync::OnceLock<Diag> = std::sync::OnceLock::new();
    D.get_or_init(Diag::default)
}

/// 进程启动以来的毫秒（`/proc/self/stat` 的 starttime 不用——用一次 `Instant` 近似）。
fn process_ms() -> u64 {
    static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    T0.get_or_init(Instant::now).elapsed().as_millis() as u64
}

/// 一行读数（**累计值**；分析侧对相邻两行做差即得区间量）。
pub(crate) fn dump_line(tag: &str) -> String {
    let d = diag();
    let seg = |name: &str, s: &Seg| {
        format!(
            "{name}(n={}us={})",
            s.count(),
            if s.mean_ns() == 0 {
                "0".to_string()
            } else {
                format!("{}", s.mean_ns() / 1000)
            }
        )
    };
    let g = |slot: usize| get(slot);
    format!(
        "m66diag[{tag}] seq={} up_ms={} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} {} cnt(dn_write={} dn_bytes={} up_read={} up_bytes={} up_ea={} udp_rx={} udp_tx={} rx_pkts={} loop={} wake={} read_calls={} poll_calls={}) cal(alu_ns={} alu_n={} sys_ns={} sys_n={})",
        d.seq.fetch_add(1, Ordering::Relaxed),
        process_ms(),
        seg("tun_write", &d.tun_write),
        seg("tun_read", &d.tun_read),
        seg("tun_read_poll", &d.tun_read_poll),
        seg("udp_recv", &d.udp_recv),
        seg("udp_send", &d.udp_send),
        seg("drv_poll", &d.drv_poll),
        seg("step", &d.step),
        seg("proto_dn", &d.proto_dn),
        seg("proto_dec", &d.proto_dec),
        seg("proto_enc", &d.proto_enc),
        seg("proto_up", &d.proto_up),
        seg("xwake_in", &d.xwake_in),
        seg("xwake_out", &d.xwake_out),
        seg("xout_push", &d.xout_push),
        seg("wake_recv", &d.wake_recv),
        seg("cmd_handle", &d.cmd_handle),
        g(C_DN_WRITE),
        g(C_DN_BYTES),
        g(C_UP_READ),
        g(C_UP_BYTES),
        g(C_UP_EAGAIN),
        g(C_UDP_RX),
        g(C_UDP_TX),
        g(C_RX_PKTS),
        g(C_LOOP),
        g(C_WAKE),
        g(C_READ_CALLS),
        g(C_POLL_CALLS),
        d.cal_alu_ns.load(Ordering::Relaxed),
        d.cal_alu_n.load(Ordering::Relaxed),
        d.cal_sys_ns.load(Ordering::Relaxed),
        d.cal_sys_n.load(Ordering::Relaxed),
    )
}

/// dump 判据（下行包距上次 dump 每 [`DUMP_EVERY`] 条为真；由「每包都过的线程」调用）。
pub(crate) const DUMP_EVERY: u64 = 4096;

/// 返回 `true` 表示调用方应打印一行（`dump_line`）。
pub(crate) fn dump_due() -> bool {
    if !on() {
        return false;
    }
    let d = diag();
    d.dump_acc.fetch_add(1, Ordering::Relaxed) % DUMP_EVERY == 0
}

/// 起校准线程（每 1s 量 ALU 环与管道写；**只为跨臂可比**——见 `cal_*` 说明）。
pub(crate) fn spawn_calibrator() {
    if !on() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("m66-cal".to_string())
        .spawn(|| {
            // 管道（自读自写；1 字节 × N + 读空）
            let mut fds = [0i32; 2];
            let rc = unsafe { libc::pipe(fds.as_mut_ptr()) };
            if rc != 0 {
                return;
            }
            let (rd, wr) = (fds[0], fds[1]);
            let mut sink = [0u8; 4096];
            loop {
                std::thread::sleep(Duration::from_millis(1000));
                // ALU 环（xorshift；`black_box` 防优化）
                const ALU_N: u64 = 200_000;
                let t0 = cpu_ns();
                let mut x: u64 = 0x2545_F491_4F6C_DD1D;
                for _ in 0..ALU_N {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                }
                std::hint::black_box(x);
                let t1 = cpu_ns();
                // 管道写（1 字节 × SYS_N，随后读空）
                const SYS_N: u64 = 200;
                let t2 = cpu_ns();
                let one = [b'x'];
                for _ in 0..SYS_N {
                    unsafe { libc::write(wr, one.as_ptr().cast(), 1) };
                }
                unsafe { libc::read(rd, sink.as_mut_ptr().cast(), 4096) };
                let t3 = cpu_ns();
                let d = diag();
                d.cal_alu_ns
                    .store(t1.saturating_sub(t0) / ALU_N, Ordering::Relaxed);
                d.cal_alu_n.store(ALU_N, Ordering::Relaxed);
                d.cal_sys_ns
                    .store(t3.saturating_sub(t2) / SYS_N, Ordering::Relaxed);
                d.cal_sys_n.store(SYS_N, Ordering::Relaxed);
            }
        });
}
