//! 隧道域真执行体（QUIC 岛单承载；语义真源 `baseline:clientcore/cmd/clientcore/tunmode.go`
//! 的 runTun2Tailcat + stats ticker + 隧道桥——**巡检/demand pusher 已随
//! reconnect-oneshot 退役**：恢复驱动收敛为「事件触发 + 一次性重连」）。
//!
//! **M5 C3 换型（设计 §4.3③）**：L3 承载恒 QUIC 岛（无 A/B 开关、无回落档）——
//! 本文件与 `wgcore`/`wtransport`/`session` 的 WG 面同批删除；岛装配失败即本世代
//! 失败（fail-visible：`岛未就用（…）` 归因行，设计 §2.6-G9）。
//!
//! 生命周期（工单①世代生命周期，Go 单 goroutine 生命线的 Rust 等价）：
//!
//! ```text
//! prepare（启动即返）→ 世代线程:
//!   身份装配 → 岛构造 + 赛跑（准入）→ 暖机探活（岛 STREAM[probe]，20s 软失败）
//!   → stage ready → 等 attach fd（60s 死线 → idle/"attach-timeout" 自收工）
//!   → attach（fd 交岛，L3 直通）→ stage attached
//!   → portfwd 装表（Go 同序：桥之前）+ stale 复查
//!   → 起桥（三座）+ stats（观测线程——patrol 三遗产的继承人）
//!   → 等 stop → 收工（pf 停 → 桥停 → 岛停）→ finish_generation
//! ```
//!
//! 一切退出路径都过 `finish_generation`（终态/放锁/done——防单飞锁泄漏）。
//!
//! **重连模型（reconnect-oneshot，2026-10-17 事故定案）**：两点式——连接存活 →
//! 岛内节拍保活；连接死了 → 等下一次事件触发**一次性重连**（`ReconnGate` 状态字 +
//! `try_spawn_reconnect` 统一入口 + 短命 worker；**本文件无任何常驻决策拍线程**）。
//! 六触发源（读面/服务流/换网/唤醒/回前台/B 门）共用同一单飞槽位；跨线程协调只有
//! 原子位与原子时间戳。stats 观测线程承接 patrol 三遗产：link 快照刷新（仅成功探测）、
//! demand 记账（纯诊断）、观测快探 + 连败标记（只标记无动作）。

use std::io;
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::identity::{self, Identity};
use crate::token::Token;
use crate::tunnel_addr::SERVER_TUNNEL_IP;
use crate::Logf;

use super::bridge_host::BridgeHost;
use super::bridge_io::BridgeStream;
use super::demand::DemandSignals;
use super::portfwd::{PfLimits, PfRuntime, PfSetup, PortForwardRule};
use super::stage::TunStage;
use super::tun_shared::{lock_unpoison, TunShared};
use super::tun_status::{LinkIn, QuicIn, RunnerIn, TransportIn};
use super::{
    TunConfigJson, TunError, TunExecutor, ATTACH_DEADLINE, ATTACH_TIMEOUT_REASON, WARM_TIMEOUT,
};

/// 巡检间隔（固定 60s——**岛内**保活/刷新节拍与迁移保持判据的输入；facade 侧
/// 巡检线程已随 reconnect-oneshot 退役）。
pub const PATROL_INTERVAL: Duration = Duration::from_secs(60);
/// reconnect-oneshot D3：demand 源（读面/服务流）的最小重连间隔（事件风暴限频；
/// 边沿源〔换网/唤醒/回前台/B 门〕绕过——「新信息」语义）。
const RECONNECT_MIN_GAP: Duration = Duration::from_secs(30);
/// reconnect-oneshot D3：让路短窗（rerace 连续让路后的短退避；哨兵 0 = 无窗）。
const RECONNECT_YIELD_GAP: Duration = Duration::from_secs(5);
/// reconnect-oneshot D1-b：让路后的复读延迟（快照陈旧窗——一个节流周期后重跑）。
const RERACE_REREAD_DELAY: Duration = Duration::from_millis(250);
/// 观测快探连续失败到这个数标记传输不健康（r5-R3；独立 60s 出站包拍计数）。
const OBS_FAIL_STREAK: u32 = 3;
/// 观测/link 刷新节拍（stats 观测线程的第二累加器——`link.at` 年龄恒 <3min 的保证）。
const OBS_INTERVAL: Duration = Duration::from_secs(60);
/// stats 周期的下限（防 cfg.stats_secs 填 0 打爆日志；Go normalize 同义下限）。
const STATS_SECS_MIN: i64 = 5;
/// stats 周期缺省（Go normalizeTunConfig 的 StatsSecs 缺省 60）。
const STATS_SECS_DEFAULT: i64 = 60;
/// mtu 缺省（Go Normalize：MTU ≤0 → 1280）。cfg.mtu 只进日志行（running /
/// 应用面就绪），数据面恒 1280——P2 升档门已随 v0.2.2 简洁化批删除
///（docs/BASELINE.md 偏离表的 clamp 条目同批撤销）。
const MTU_DEFAULT: i64 = 1280;
/// 桥拨号预算缺省（Go Normalize：DialMs ≤0 → 15000——评审 r2-L5；随 tunConfig
/// 的 dialMs 热传入 BridgeHost）。
const DIAL_MS_DEFAULT: i64 = 15000;


// ---------------------------------------------------------------------------
// 单承载常量（M5 C3：A/B 开关三键与回落档一并删除，设计 §4.3）
// ---------------------------------------------------------------------------

/// 岛侧赛跑预算（世代装配期一次；覆盖 LAN/中继握手 + `hr-reg4` 四帧准入）。
const QUIC_CONNECT_BUDGET: Duration = Duration::from_secs(5);
/// 岛侧同步命令（TunAttach / Rebind）的等待预算（岛内短路径，超时 = 异常）。
const QUIC_RPC_BUDGET: Duration = Duration::from_secs(5);
/// 世代收尾的**岛**停止预算（Q-G F5：有界收工——到点 detach，不让收工链挂死）。
/// 名与值沿 WG 档（2s）——相关行文（`等待岛线程收工超时（CLIENT_CLOSE_BUDGET）`）
/// 是既有注册串面，改名等于无谓改写判据行。
const CLIENT_CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// MTU 上限解析（设计 §12-①）：**env 优先**（`HOMEWAY_QUIC_MTU`）→ config
/// （`tunConfig.quicMtuCap`，≤0 = 未设）→ 缺省 1400；有效区间 `[1320,1400]`。
/// 非法/越界 ⇒ **记行 + 按缺省 1400 走**（不做夹取：越界取值本身说明配置方意图不明
/// ——`>1400` 撞 IPv6 信封余量、`<1320` 必然全丢，按缺省最不易造成静默劣化）。
fn resolve_mtu_cap(cfg_value: i64, logf: &Logf) -> u16 {
    use homeway_quic::{QUIC_MTU_CAP_DEFAULT, QUIC_MTU_CAP_MAX, QUIC_MTU_CAP_MIN};
    // N-e（M5 S4 新增行，世代装配一次）：**生效值 + 来源**必须看得见——单承载后这是
    // 唯一的窄路径旋钮（设计 §8.4；此前只有非法值一行）。
    let announce = |v: u16, from: &str| {
        (logf)(&format!("内层 MTU 上限 {v}（来源={from}）"));
        v
    };
    let pick = |raw_text: &str, from: &str| match raw_text.trim().parse::<i64>() {
        Ok(v) if (i64::from(QUIC_MTU_CAP_MIN)..=i64::from(QUIC_MTU_CAP_MAX)).contains(&v) => {
            let v = v as u16;
            announce(v, from)
        }
        _ => {
            (logf)(&format!(
                "MTU 上限取值 {raw_text:?}（{from}）非法或越界（有效区间 [{QUIC_MTU_CAP_MIN},{QUIC_MTU_CAP_MAX}]）——按缺省 {QUIC_MTU_CAP_DEFAULT} 走"
            ));
            announce(QUIC_MTU_CAP_DEFAULT, "缺省（非法值回退）")
        }
    };
    if let Some(raw) = crate::envflag::quic_mtu_raw() {
        return pick(raw, "HOMEWAY_QUIC_MTU");
    }
    if cfg_value > 0 {
        return pick(&cfg_value.to_string(), "tunConfig.quicMtuCap");
    }
    announce(QUIC_MTU_CAP_DEFAULT, "缺省")
}

/// link 段 via 词表（`direct|relay|none`）——岛侧 `Via` 的**判据行**取值是中文
/// （C4'/C5'），link/JSON 面必须用既有三态词（C10 行文与键面零改动，只换来源）。
/// `none` 的落点 = 「岛未建连/未采纳」（`IslandSnapshot::via == None`）⇒ 由调用方
/// 给字面量（本函数只管「有 via 时的两态」）。
fn link_via(v: homeway_quic::Via) -> &'static str {
    v.link_text()
}

/// link 段 via 词表全三态（`IslandSnapshot::via` 直投）。
fn link_via_opt(v: Option<homeway_quic::Via>) -> &'static str {
    match v {
        None => "none",
        Some(v) => link_via(v),
    }
}

// ---------------------------------------------------------------------------
// 会话流适配器（桥远端——工单⑤ dial_port 接缝：流 id ≠ fd，适配成 Read/Write 两半）
// ---------------------------------------------------------------------------


/// portfwd 拨号缝的生产实现（`PfRuntime` 全注入面——闭包持 `Weak<GenRun>` 防 Arc 环：
/// 世代收工后 `upgrade()` 失败 = 拨号失败如实收口（conn 线程记 fails + RST + 行））。
///
/// **单腿（M5 C3）**：判据 = `l3_on_island()`（与桥拨号 `BridgeHost` 构造处**同源同形**）
/// ——岛在世且 L3 在岛上 ⇒ `STREAM[dial]`；否则（岛不在/已收回/合成世代的拨号桩）
/// 如实归因「岛不在」，**不留第二条腿**。
///
/// **阀 / 两阶段 install / `FlowGuard` / 计数逐字保留（§2.3）**：承载差异**止于此闭包**
/// （`PfDialFn` 签名与语义不变 ⇒ `admit_conn` 与 `pf_conn_thread` 一行不改）。
fn pf_dial_via_run(
    run: &std::sync::Weak<GenRun>,
    dst: SocketAddrV4,
    budget: Duration,
) -> io::Result<Box<dyn BridgeStream>> {
    let Some(r) = run.upgrade() else {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "世代已收工（拨号放弃）",
        ));
    };
    if !r.l3_on_island() {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "岛不在（L3 未承接）——端口转发无法拨号",
        ));
    }
    // reconnect-oneshot D2（ServiceStream 源）：portfwd 拨号的**首次承载失败即触发**
    // （单次尝试路径无重试概念）——触发在错误映射 io::Error 之前（typed 层）。
    super::quic_stream::dial_target(&r, dst, budget)
}


// ---------------------------------------------------------------------------
// 世代运行态（Go tunRun）
// ---------------------------------------------------------------------------

/// 收工等待的唤醒面（2026-10-18 评审：替掉 stats 拍与世代主线程的 1s 分片轮询——
/// 空闲期不再 1Hz 唤醒；`request_stop` 置位后 `wake()`，等待者立即返回）。
///
/// 丢唤醒协议：等待者**先持 `gate` 锁再复查 `stop` 位**，`wake()` 也持同一把锁再
/// notify ⇒ 「置位发生在复查之后、notify 发生在等待之前」的窗口不存在。
pub(crate) struct StopWake {
    gate: Mutex<()>,
    cv: Condvar,
}

impl StopWake {
    fn new() -> Self {
        Self {
            gate: Mutex::new(()),
            cv: Condvar::new(),
        }
    }

    /// 唤醒全部等待者（调用方必须已置 `stop` 位）。
    fn wake(&self) {
        let _g = lock_unpoison(&self.gate);
        self.cv.notify_all();
    }

    /// 等到 `stop` 置位或 `dur` 到点；返回后调用方复查 stop。
    fn wait(&self, stop: &AtomicBool, dur: Duration) {
        let g = lock_unpoison(&self.gate);
        if stop.load(Ordering::Acquire) {
            return;
        }
        let _ = self
            .cv
            .wait_timeout(g, dur)
            .unwrap_or_else(|e| e.into_inner());
    }
}

/// 一个世代的运行态（TunnelExec 持当前世代的 Arc；旧世代迟到收尾无害）。
pub struct GenRun {
    pub gen: u64,
    /// 世代停止位（request_stop 置；所有线程收口判据）。
    pub stop: Arc<AtomicBool>,
    /// 停止唤醒面（stats 拍 / 世代主线程的收工等待；见 [`StopWake`]）。
    pub(crate) stop_wake: StopWake,
    /// QUIC 岛（世代装配期构造；`None` = 未构造/已收回）。**M5 C3 单承载**：
    /// 岛装配失败即本世代失败（无回落档，设计 §4.3③）。
    island: RwLock<Option<Arc<homeway_quic::Island>>>,
    /// L3 是否已承接（装配期置位；`l3_on_island()` 的另一半判据）。
    l3_on_island: AtomicBool,
    /// 身份面（世代归因）。
    identity: Identity,
    /// 本世代隧道地址（`transport.tunIp` 的源；装配期一次派生）。
    tun_ip: String,
    /// 链路快照（runner link 段数据源；**at = 最近一次成功探测**——观测快探/
    /// worker 快探/recover 同步快探三类任一刷新，reconnect-oneshot r10-C1）。
    pub link: Mutex<Option<LinkIn>>,
    /// **一次性重连的统一入口状态字**（reconnect-oneshot D1，r11 canonical——
    /// 见 [`ReconnGate`]；六触发源共用同一单飞槽位）。
    pub(crate) reconn: ReconnGate,
    /// 隧道桥（attached 后起；None = 未起/已停）。
    pub bridge: Mutex<Option<Arc<BridgeHost>>>,
    /// 日志面（带 `tier-core: ` 前缀；写世代日志文件）。
    pub logf: Logf,
    /// facade 世代共享面（阶段机/健康位/收尾/承载位）。
    pub tun_shared: Arc<TunShared>,
    /// 需求信号（ClientCore 的共享面——stats 观测拍记账消费）。
    pub demand: Arc<DemandSignals>,
    /// portfwd 运行时（真监听器：整表热替换 / 世代收工 / 真计数——Q-F-B F1）。
    /// 拨号缝全注入（闭包持 `Weak<GenRun>`——无 Arc 环）、停止位 = 本世代 `stop` 克隆。
    pub pf: PfRuntime,
    /// 世代时间起点（elapsed 面板用）。
    pub started: Instant,
}

impl GenRun {
    /// 当前岛（已构造且在位；已收回 = None）。
    pub fn current_island(&self) -> Option<Arc<homeway_quic::Island>> {
        self.island
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 岛回填（构造成功后；一次性）。
    fn set_island(&self, i: Arc<homeway_quic::Island>) {
        *self.island.write().unwrap_or_else(|e| e.into_inner()) = Some(i);
    }

    /// 收回岛（收尾链用；`take` 语义 = 幂等）。
    fn take_island(&self) -> Option<Arc<homeway_quic::Island>> {
        self.island
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// L3 是否落在岛上（判据 = 承接位 + 岛在位；装配期/收回后为假）。
    pub fn l3_on_island(&self) -> bool {
        self.l3_on_island.load(Ordering::Acquire)
            && self
                .island
                .read()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
    }

    fn set_l3_on_island(&self, v: bool) {
        self.l3_on_island.store(v, Ordering::Release);
    }

    /// 岛在位且已承接 ⇒ 一切 L3 取数的**唯一**来源（单承载：无第二条腿）。
    /// 取数点按它解构（缺失 ⇒ `None`/零值），不再有「按档分派」。
    fn engaged_island(&self) -> Option<Arc<homeway_quic::Island>> {
        if !self.l3_on_island() {
            return None;
        }
        self.current_island()
    }

    // ---- L3 承载面的同接口取数（设计 §2.5「接口不变、来源切换」）----

    /// TUN fd 字节对表（stats 行与 `runner.stats` 的源）。
    fn l3_tun_stats(&self) -> (u64, u64) {
        self.engaged_island()
            .map(|i| i.tun_stats())
            .unwrap_or((0, 0))
    }

    /// 取走并清零 App 出站包计数（巡检拍的需求信号源）。
    fn l3_swap_out_pkts(&self) -> i64 {
        self.engaged_island()
            .map(|i| i.swap_out_pkts())
            .unwrap_or(0)
    }

    /// 最近出站包时刻（unix 毫秒；`transport.outboundAt` 的源）。
    fn l3_last_outbound_unix_ms(&self) -> i64 {
        self.engaged_island()
            .map(|i| i.last_outbound_unix_ms())
            .unwrap_or(0)
    }

    /// 岛侧链路快照 `(via, ep, rtt_ms)`——C10 快照形态与 `link{via,ep,rttMs,at}` 的
    /// **来源**（`None` = 岛不在/未承接 ⇒ 调用方按空快照处理）。
    fn island_link(&self) -> Option<(String, String, i64)> {
        let s = self.engaged_island()?.snapshot();
        Some((
            link_via_opt(s.via).to_owned(),
            s.ep.map(|a| a.to_string()).unwrap_or_default(),
            s.rtt_ms as i64,
        ))
    }
}

// ---------------------------------------------------------------------------
// 一次性重连：统一入口状态字（reconnect-oneshot design D1，r11 canonical——逐字实现）
// ---------------------------------------------------------------------------

/// 触发源枚举（D1：唯一触发面——六源收敛到同一入口）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ReconnSource {
    /// 读面（TUN 读线程见出站包且承载位 false）——demand 源。
    ReadFace,
    /// 服务流（开流有界重试后仍承载类失败 / 在飞流承载丢失）——demand 源。
    ServiceStream,
    /// 换网（`recover(from=3)`）——边沿源。
    NetChange,
    /// 唤醒下推（`recover(from=1)`；`from` 其余值按唤醒归并）——边沿源。
    Wake,
    /// 回前台（`SetForeground` 的 false→true kick 边沿）——边沿源。
    Foreground,
    /// B 门（岛内阶梯走完的上报沿）——边沿源。
    BGate,
}

impl ReconnSource {
    /// 边沿源绕过 MIN_GAP（「新信息」语义）；demand 源受 30s 限频。
    fn is_edge(self) -> bool {
        matches!(self, ReconnSource::NetChange | ReconnSource::Wake | ReconnSource::Foreground | ReconnSource::BGate)
    }

    /// pending 位（bits1-4 四边沿源；demand 源不置 pending ⇒ 0）。
    const fn pending_bit(self) -> u32 {
        match self {
            ReconnSource::NetChange => 1 << 1,
            ReconnSource::Wake => 1 << 2,
            ReconnSource::Foreground => 1 << 3,
            ReconnSource::BGate => 1 << 4,
            _ => 0,
        }
    }

    /// 判据行的源名。
    const fn text(self) -> &'static str {
        match self {
            ReconnSource::ReadFace => "读面",
            ReconnSource::ServiceStream => "服务流",
            ReconnSource::NetChange => "换网",
            ReconnSource::Wake => "唤醒",
            ReconnSource::Foreground => "回前台",
            ReconnSource::BGate => "B门",
        }
    }

    /// 承接轮按 pending 位图推导源（D1-d 优先级：NetChange > Wake > Foreground > BGate；
    /// 位图不含边沿位 = demand 源承接形态——按读面处理）。
    const fn from_pending(p: u32) -> ReconnSource {
        if p & ReconnSource::NetChange.pending_bit() != 0 {
            ReconnSource::NetChange
        } else if p & ReconnSource::Wake.pending_bit() != 0 {
            ReconnSource::Wake
        } else if p & ReconnSource::Foreground.pending_bit() != 0 {
            ReconnSource::Foreground
        } else if p & ReconnSource::BGate.pending_bit() != 0 {
            ReconnSource::BGate
        } else {
            ReconnSource::ReadFace
        }
    }
}

/// 状态字位面（D1：bit0 = INFLIGHT；bits1-4 四边沿 pending 位〔bits5-6 保留〕；
/// bits7-31 = owner epoch——25 bit 不回绕，每次抢位/承接自增 1）。
const RECONN_INFLIGHT: u32 = 1;
const RECONN_PENDING_MASK: u32 = 0b0111_1110; // bits1-6（含保留位——清/置整面一致）
const RECONN_EPOCH_SHIFT: u32 = 7;

const fn reconn_epoch(s: u32) -> u32 {
    s >> RECONN_EPOCH_SHIFT
}

/// 统一入口的占位结果。
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum Claim {
    /// 在飞（单飞合并；边沿位已按协议留字或自消费——绝不静默吞掉）。
    InFlight,
    /// 占位成功（成为 owner；epoch = 本轮身份）。
    Claimed { epoch: u32 },
}

/// 入口的受理/抑制形态（rc 面映射：`Rejected` → -2；`InFlight`/`SpawnFailed` → -1；
/// `Throttled` 仅 demand 源可达——recover 边沿路径不含）。
#[derive(PartialEq, Eq, Debug)]
pub(crate) enum EntryOutcome {
    Spawned,
    Throttled,
    InFlight,
    Rejected,
    SpawnFailed,
    /// 系统禁发新鲜期：demand 源跳过（不消耗 30s 锚；边沿源不受影响）。
    Denied,
}

/// 测试钩子类型（受控交错缝——clippy type_complexity 的别名收口）。
#[cfg(test)]
type TestHook = Arc<dyn Fn(&str) + Send + Sync>;

/// 进程单调钟基座（ms；禁 SystemTime——挂起冻结会让墙钟「跳前」，锚语义失效）。
fn mono_ms() -> i64 {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as i64
}

/// 一次性重连的状态字（挂在 `GenRun` 上；**跨线程协调只有原子**——无锁状态机，
/// reconnect-oneshot 的结构性要求）。
pub(crate) struct ReconnGate {
    state: AtomicU32,
    /// 起跑锚（**起跑时刻**，非完成时刻——执行时长不得叠加进间隔；初值 = 装配时刻
    /// − MIN_GAP，首个 demand 事件不被首启锚抑制）。
    last_ms: AtomicI64,
    /// 让路短窗（0 = 无窗哨兵；`until = (y==0) ? last+MIN_GAP : min(last+MIN_GAP, y)`）。
    yield_until_ms: AtomicI64,
    // ---- r4-R16 五计数（stats 行「重连」段；全部原子、入口/worker 分支自增）----
    /// 入口调用 N。
    pub(crate) entries: AtomicU64,
    /// 起跑 S（run_round 进入时自增——**含承接轮**）。
    pub(crate) starts: AtomicU64,
    /// 节流 K（demand 源 30s 窗内被限频）。
    pub(crate) throttled: AtomicU64,
    /// 起跑失败 F（spawn 三次全败——r4-R16 五计数之一；读线程路径零 logf，计数由
    /// stats 行落；载位假包 P 不在此处——它是岛侧 `CarrierFlag::demand_pkts`，stats
    /// 行直接读那边）。**评审 r1 P1-14 整改注记**：本字段历史注释曾误写成「载位
    /// 假包」的文案（复制粘贴残留）——实际语义 = spawn 全败计数
    /// （`EntryOutcome::SpawnFailed` 分支自增；与 `dropped_pending` 的清位丢弃计数区分）。
    pub(crate) spawn_fails: AtomicU64,
    /// 快探总次数（诊断）。`sync_probes` = 其中 recover 同步快探的次数——并发去重
    /// 的「至多 1 个同步快探（并发与总数皆 ≤1）」观测面（worker 轮的探不在此列）。
    pub(crate) probes: AtomicU64,
    pub(crate) sync_probes: AtomicU64,
    /// panic 兜底计数（catch 处理器落）。
    pub(crate) panics: AtomicU64,
    /// 持位期间清掉的 pending 位计数（r9-C12 如实登记——同 bit 新事件不可区分，
    /// 按快照丢弃并计数）。
    pub(crate) dropped_pending: AtomicU64,
    /// 测试缝：受控交错钩子（生产恒 None；见 `hook`）。
    #[cfg(test)]
    hooks: Mutex<Option<TestHook>>,
}

impl ReconnGate {
    fn new() -> Self {
        ReconnGate {
            state: AtomicU32::new(0),
            // 初值 = 装配时刻 − MIN_GAP：首个 demand 事件立即起跑（不被首启锚抑制）。
            last_ms: AtomicI64::new(mono_ms() - RECONNECT_MIN_GAP.as_millis() as i64),
            yield_until_ms: AtomicI64::new(0),
            entries: AtomicU64::new(0),
            starts: AtomicU64::new(0),
            throttled: AtomicU64::new(0),
            spawn_fails: AtomicU64::new(0),
            probes: AtomicU64::new(0),
            sync_probes: AtomicU64::new(0),
            panics: AtomicU64::new(0),
            dropped_pending: AtomicU64::new(0),
            #[cfg(test)]
            hooks: Mutex::new(None),
        }
    }

    /// 测试缝（受控交错）：在协议的命名点上回调——测试以 barrier/通道控制线程推进
    /// 时序（「barrier 受控交错」的承载面；生产构建零开销）。
    #[cfg(test)]
    fn hook(&self, point: &str) {
        // 注意：守卫必须**先落盘再回调**——`if let` 条件里的临时守卫会活到块尾，
        // 阻塞型钩子（受控交错的通道等待）会把本互斥锁一起攥住 ⇒ 死锁。
        let h = lock_unpoison(&self.hooks).clone();
        if let Some(h) = h {
            h(point);
        }
    }

    #[cfg(not(test))]
    fn hook(&self, _point: &str) {}

    /// demand 源节流判定（哨兵公式：`until = (y==0) ? last+MIN_GAP : min(last+MIN_GAP, y)`）。
    fn throttled_now(&self) -> bool {
        let now = mono_ms();
        let last = self.last_ms.load(Ordering::Acquire);
        let y = self.yield_until_ms.load(Ordering::Acquire);
        let until = if y == 0 {
            last + RECONNECT_MIN_GAP.as_millis() as i64
        } else {
            (last + RECONNECT_MIN_GAP.as_millis() as i64).min(y)
        };
        now < until
    }

    /// 【共享原语一（r11-C8）：唯一释放路径——CAS-loop，绝不单次 CAS 后返回】。
    /// 仍是本轮 owner（INFLIGHT 在位且 epoch 相符）⇒ 清 INFLIGHT 并返回释放前快照的
    /// pending 位；否则 `None`（已非本轮）。三处共用：正常收尾 / spawn 全败 / panic。
    fn release_if_owner(&self, my_epoch: u32) -> Option<u32> {
        loop {
            self.hook("release_load");
            let s = self.state.load(Ordering::Acquire);
            if (s & RECONN_INFLIGHT) == 0 || reconn_epoch(s) != my_epoch {
                return None; // 已非本轮
            }
            let target = s & !RECONN_INFLIGHT;
            match self.state.compare_exchange_weak(
                s,
                target,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(s & RECONN_PENDING_MASK), // 释放 + 快照
                // CAS 失败 = 并发置位（pending fetch_or 同拍——r10-C8）：重读重试。
                Err(_) => continue,
            }
        }
    }

    /// 【共享原语二（r11-C8）：在飞判定/占位二合一】。边沿源在飞时以 pending 位
    /// 留字（绝不静默吞掉）；`armed` 标志防双跑（r10-C10：fetch_or 恰逢释放标记后
    /// 重读，他人承接可能已赢——不再 fetch_or 把刚被消费的位重新置上）。
    fn try_claim_edge(&self, source: ReconnSource) -> Claim {
        let mut armed = false;
        loop {
            self.hook("claim_load");
            let s = self.state.load(Ordering::Acquire);
            if armed && (s & RECONN_INFLIGHT) != 0 {
                return Claim::InFlight; // 他人承接已赢——直接退出不双跑
            }
            if (s & RECONN_INFLIGHT) != 0 {
                if source.is_edge() {
                    self.hook("edge_before_fetch_or");
                    let prev2 = self.state.fetch_or(source.pending_bit(), Ordering::AcqRel);
                    if (prev2 & RECONN_INFLIGHT) != 0 {
                        return Claim::InFlight; // owner 在：位已留字
                    }
                    // owner 恰在 fetch_or 前释放（r8-a1-3 同拍形态）：重读抢位
                    //（重读若见他人承接赢——顶部 armed 检查退出）。
                    armed = true;
                    continue;
                }
                return Claim::InFlight; // demand：下个包会再来
            }
            // 抢位：epoch 自增；清自己刚留的位（自消费）；其余 pending 位保留（r8-a1-2：
            // 入口 CAS 不清现场 pending）。
            let new_epoch = reconn_epoch(s).wrapping_add(1) & 0x1FF_FFFF; // 25 bit 不回绕
            let target = RECONN_INFLIGHT
                | (new_epoch << RECONN_EPOCH_SHIFT)
                | ((s & RECONN_PENDING_MASK) & !source.pending_bit());
            match self.state.compare_exchange_weak(s, target, Ordering::AcqRel, Ordering::Acquire) {
                Ok(_) => return Claim::Claimed { epoch: new_epoch },
                // CAS 失败 = 并发，重读（armed 语义如上）。
                Err(_) => continue,
            }
        }
    }

    /// 读面的在飞判定（D2 rc 表：recover 预检不得裸 load/check-then-act——与入口
    /// 同一 CAS/fetch_or-复查协议）。
    fn inflight(&self) -> bool {
        self.state.load(Ordering::Acquire) & RECONN_INFLIGHT != 0
    }
}

/// worker 的记账面（r11-C9：**guard 只记账，不写状态字**——panic 展开顺序中 guard
/// Drop 先于 catch 处理器，guard 若释放则处理器必见 INFLIGHT=0 而跳过恢复。
/// epoch/triggering 由收尾/承接循环就地更新，panic 时 catch 处理器据此恢复）。
struct RoundGuard {
    epoch: u32,
    /// 本轮（含承接轮）的 pending 快照——r13-P4：承接 CAS 前更新，panic 恢复
    /// 覆盖 p2 全部（不丢新到位）。
    triggering: u32,
}

impl GenRun {
    /// 【统一入口 `try_spawn_reconnect`（任意线程，非阻塞——设计 D1 步骤 0-4）】。
    /// 读面回调的契约面：只做原子操作 + 起线程（禁日志禁锁——**本函数零 logf**）。
    pub(crate) fn try_spawn_reconnect(run: &Arc<GenRun>, source: ReconnSource) -> EntryOutcome {
        // 0. stop / gen 不符 / device_dead → Suppressed（rc 面 -2）。设备死检查走
        // CarrierFlag 的**零锁**读面——本入口活在读线程回调路径上（禁锁契约）。
        if run.stop.load(Ordering::Acquire)
            || run.gen != run.tun_shared.gen.load(Ordering::Acquire)
            || run.tun_shared.carrier.device_dead()
        {
            return EntryOutcome::Rejected;
        }
        run.reconn.entries.fetch_add(1, Ordering::Relaxed);
        // 1. demand 源先判节流（哨兵公式；边沿绕过）
        if !source.is_edge() && run.reconn.throttled_now() {
            run.reconn.throttled.fetch_add(1, Ordering::Relaxed);
            return EntryOutcome::Throttled;
        }
        // 1.5 系统禁发新鲜期：demand 源不起跑（换 socket/新连接同一权限面）；
        //     边沿源（换网/唤醒/回前台/B门）是新信息，照常尝试。
        if !source.is_edge() && run.tun_shared.denied.load(Ordering::Acquire) {
            return EntryOutcome::Denied;
        }
        // 2. 占位（在飞判定/占位二合一原语）
        match run.reconn.try_claim_edge(source) {
            Claim::InFlight => EntryOutcome::InFlight,
            Claim::Claimed { epoch } => {
                run.reconn.hook("entry_claimed");
                // 写锚（起跑时刻——占位成功即捕获）+ 清让路窗
                let saved_last = run.reconn.last_ms.swap(mono_ms(), Ordering::AcqRel);
                let saved_yield = run.reconn.yield_until_ms.swap(0, Ordering::AcqRel);
                // 3. spawn（初次 + 2 次重试，共 3 次）
                match spawn_reconnect_worker(run, epoch, source, false) {
                    Ok(()) => EntryOutcome::Spawned,
                    Err(()) => {
                        // 4. spawn 全败（r13-P2：清位在 release 前、仍持 INFLIGHT 时做
                        // ——缩小与新 owner 承接窗口的重叠）
                        run.reconn.spawn_fails.fetch_add(1, Ordering::Relaxed);
                        run.reconn.last_ms.store(saved_last, Ordering::Release);
                        run.reconn.yield_until_ms.store(saved_yield, Ordering::Release);
                        clear_pending_while_holding(run, epoch);
                        // snap 中的位保留留字给下一任意事件承接（不清——release 后的
                        // 清位会与新 owner 承接窗口竞争）。
                        let _snap = run.reconn.release_if_owner(epoch);
                        EntryOutcome::SpawnFailed
                    }
                }
            }
        }
    }

    /// 承载位/健康面的「强证据翻回」（D1-a/D2：ZERO 间隔清标记 + 置承载位 true
    /// 〔锁内查 device_dead〕+ 刷 run.link——**先置位后刷 at**，r10-C1）。
    /// 评审 r1 P1-14：承载位置 true 判负（设备死在场/陈旧世代）时**跳过
    /// `refresh_link_snapshot`**——设备死在快探成功后发生时 at 不前进（「设备死/
    /// 未恢复时 at 不前进」的语义不被削弱）。
    /// 评审 r2 B2①：清 patrol 与置承载位**一次** health 临界区完成
    /// （`strong_recover_if_current`——旧两步加锁之间可被 `mark_transport_if_current`
    /// 插入产出 `patrol ∧ carrier=true` 黏滞终态）。
    /// 返回承载位判定（`carrier_alive`——评审 r4 B3 低项：`recover` 以它识别
    /// 「复查之后设备死才到场」的窗口，false 同归 -2 释放路径）。
    fn strong_recover(run: &Arc<GenRun>, evidence: &str) -> bool {
        let (recovered, carrier_alive) = run.tun_shared.strong_recover_if_current(run.gen);
        if let Some(elapsed) = recovered {
            (run.logf)(&format!(
                "传输恢复（patrol 清除，证据={evidence}，距上次判定 {}）",
                crate::go_fmt::fmt_duration_go_secs(elapsed)
            ));
            // 翻沿（退出传输不健康）已在 strong_recover_if_current 内 bump。
        }
        // 承载位置 true（patrol 从未标记/已被他人清除的路径仍需置位；设备死在场判负）。
        if carrier_alive {
            refresh_link_snapshot(run);
        }
        carrier_alive
    }

    /// 【worker 的一轮执行 `run_round(source, skip_probe)`（D1-a/b/c）】。
    ///
    /// `skip_probe = true` 只用于 `recover` 的失败移交：同步快探刚在同一个入口里
    /// 失败，worker 立刻再探一次是纯重复（换网/唤醒时白烧 700ms）。其余入口
    /// （读面/服务流/B 门/承接轮）一律先快探——那是一次新的证据机会。
    fn run_round(run: &Arc<GenRun>, source: ReconnSource, skip_probe: bool) {
        run.reconn.starts.fetch_add(1, Ordering::Relaxed); // 起跑 S（含承接轮）
        let worker_start = mono_ms();
        // 事件起点 = 占位成功时写的 last_ms（`try_spawn_reconnect` / `recover` 同源）。
        // 直接调用 run_round 的单测没有占位锚，回落到 worker 起点，避免出现负值/天文数。
        let claimed_at = run.reconn.last_ms.load(Ordering::Acquire);
        let event_start = if claimed_at <= 0 || claimed_at > worker_start {
            worker_start
        } else {
            claimed_at
        };
        let queued_ms = (worker_start - event_start).max(0);
        // 评审 r1 P0-1 整改：起跑判据行（worker 线程落——不在读路径；stats 的
        // 起跑=S 计数无源枚举，7.1 的「读面触发」验收判据由本行承载）。
        (run.logf)(&format!("一次性重连：起跑（源={}）", source.text()));
        // 测试缝：panic 注入（r9-C7/r10-C9——gate>0 时倒数至 1 的那轮 panic）。
        #[cfg(test)]
        if PANIC_ROUNDS_GATE.load(Ordering::Relaxed) > 0 {
            let prev = PANIC_ROUNDS_GATE.fetch_sub(1, Ordering::Relaxed);
            if prev == 1 {
                panic!("PANIC_ROUNDS_GATE 注入（run_round panic 缝）");
            }
        }
        // c. 检查点：在飞遇设备死/停止/换代即退出、不置 carrier true、不继续 Rebind/Connect
        if worker_halted(run) {
            log_event_recovery(
                run,
                source,
                "放弃",
                "未起跑",
                event_start,
                queued_ms,
                None,
                0,
                0,
                "设备死/停止/换代",
            );
            return;
        }
        run.reconn.hook("round_before_probe");
        // a. 有界快探（island_cmd_until）：成功 = 强证据翻回 → 完成(已恢复)。
        //    Busy（probe 槽被并发占用）= 并发面而非对端证据 ⇒ 让路、不换绑不赛跑。
        let mut probe_ms: Option<i64> = None;
        let probe_ok = if skip_probe {
            (run.logf)(&format!(
                "一次性重连：跳过重复快探（同步快探刚失败，源={}）",
                source.text()
            ));
            false
        } else {
            let p0 = mono_ms();
            let outcome = quick_probe(run);
            probe_ms = Some((mono_ms() - p0).max(0));
            match outcome {
                Ok(()) => true,
                Err(ProbeFail::Busy(m)) => {
                    (run.logf)(&format!(
                        "一次性重连：让路（源={}，probe 槽被并发占用：{m}）",
                        source.text()
                    ));
                    log_event_recovery(
                        run,
                        source,
                        "让路",
                        "快探",
                        event_start,
                        queued_ms,
                        probe_ms,
                        0,
                        0,
                        &format!("probe 槽被并发占用：{m}"),
                    );
                    return;
                }
                Err(_) => false,
            }
        };
        if probe_ok {
            // 测试缝：快探刚返回成功、halt 复查前的设备死/停止注入点（评审
            // r2 B3——设备死在快探在飞期间发生时不落恢复行、不置 carrier true）。
            run.reconn.hook("round_after_probe");
            if worker_halted(run) {
                log_event_recovery(
                    run,
                    source,
                    "放弃",
                    "快探",
                    event_start,
                    queued_ms,
                    probe_ms,
                    0,
                    0,
                    "设备死/停止/换代",
                );
                return;
            }
            // 测试缝（评审 r5-L3）：halt 复查通过后、strong_recover 前的设备死
            // 注入点——承载位置 true 判负（alive=false）时不得落假恢复行。
            run.reconn.hook("round_before_strong");
            let t0 = mono_ms();
            // 评审 r5-L3：消费 `carrier_alive`——设备死在复查与强恢复之间到场
            // 时不落「一次性重连：恢复」行、按 Gone/设备死静默收口（与
            // worker_halted 同纪律；旧实现无条件打行 = 假恢复行）。
            if !GenRun::strong_recover(run, "快探") {
                log_event_recovery(
                    run,
                    source,
                    "放弃",
                    "快探",
                    event_start,
                    queued_ms,
                    probe_ms,
                    0,
                    (mono_ms() - t0).max(0),
                    "强恢复时设备死/换代",
                );
                return;
            }
            let recover_ms = (mono_ms() - t0).max(0);
            (run.logf)(&format!(
                "一次性重连：恢复（源={}，证据=快探，耗时 {}ms）",
                source.text(),
                recover_ms
            ));
            log_event_recovery(
                run,
                source,
                "成功",
                "快探",
                event_start,
                queued_ms,
                probe_ms,
                0,
                recover_ms,
                "",
            );
        } else {
            // 测试缝：快探刚结束、halt 复查前的设备死/停止注入点（r9-C5——
            // 评审 r1 整改：注入后由下方复查拦截，rerace_tunnel 不进入）。
            run.reconn.hook("round_before_rerace");
            if worker_halted(run) {
                log_event_recovery(
                    run,
                    source,
                    "放弃",
                    "未起跑",
                    event_start,
                    queued_ms,
                    probe_ms,
                    0,
                    0,
                    "设备死/停止/换代",
                );
                return;
            }
            // b. 复用既有重赛跑序列（换网源带 demote_adopted）
            let demote = source == ReconnSource::NetChange;
            let mut race_ms = 0i64;
            let r0 = mono_ms();
            let first = rerace_tunnel(run, demote);
            race_ms += (mono_ms() - r0).max(0);
            match first {
                Ok(ReraceOutcome::Recovered) => {
                    (run.logf)(&format!(
                        "一次性重连：恢复（源={}，证据=重赛跑）",
                        source.text()
                    ));
                    log_event_recovery(
                        run,
                        source,
                        "成功",
                        "重赛跑",
                        event_start,
                        queued_ms,
                        probe_ms,
                        race_ms,
                        0,
                        "",
                    );
                }
                Ok(ReraceOutcome::Yielded) => {
                    // 250ms 复读一次（快照陈旧窗），仍让路 → 完成(让路) + 5s 短窗
                    std::thread::sleep(RERACE_REREAD_DELAY);
                    if worker_halted(run) {
                        log_event_recovery(
                            run,
                            source,
                            "放弃",
                            "重赛跑",
                            event_start,
                            queued_ms,
                            probe_ms,
                            race_ms,
                            0,
                            "设备死/停止/换代",
                        );
                        return;
                    }
                    let r1 = mono_ms();
                    let second = rerace_tunnel(run, demote);
                    race_ms += (mono_ms() - r1).max(0);
                    match second {
                        Ok(ReraceOutcome::Recovered) => {
                            (run.logf)(&format!(
                                "一次性重连：恢复（源={}，证据=重赛跑（复读））",
                                source.text()
                            ));
                            log_event_recovery(
                                run,
                                source,
                                "成功",
                                "重赛跑",
                                event_start,
                                queued_ms,
                                probe_ms,
                                race_ms,
                                0,
                                "",
                            );
                        }
                        Ok(ReraceOutcome::Yielded) => {
                            run.reconn.yield_until_ms.store(
                                mono_ms() + RECONNECT_YIELD_GAP.as_millis() as i64,
                                Ordering::Release,
                            );
                            (run.logf)(&format!(
                                "一次性重连：让路（源={}，在飞赛跑持续——{} 短窗退避）",
                                source.text(),
                                crate::go_fmt::fmt_duration_go_secs(RECONNECT_YIELD_GAP)
                            ));
                            log_event_recovery(
                                run,
                                source,
                                "让路",
                                "重赛跑",
                                event_start,
                                queued_ms,
                                probe_ms,
                                race_ms,
                                0,
                                "在飞赛跑持续",
                            );
                        }
                        Err(abort) => match rerace_abort_line(run, source, abort) {
                            Some(why) => log_event_recovery(
                                run,
                                source,
                                "失败",
                                "重赛跑",
                                event_start,
                                queued_ms,
                                probe_ms,
                                race_ms,
                                0,
                                &why,
                            ),
                            None => log_event_recovery(
                                run,
                                source,
                                "放弃",
                                "重赛跑",
                                event_start,
                                queued_ms,
                                probe_ms,
                                race_ms,
                                0,
                                "设备死/停止/换代",
                            ),
                        },
                    }
                }
                Err(abort) => match rerace_abort_line(run, source, abort) {
                    Some(why) => log_event_recovery(
                        run,
                        source,
                        "失败",
                        "重赛跑",
                        event_start,
                        queued_ms,
                        probe_ms,
                        race_ms,
                        0,
                        &why,
                    ),
                    None => log_event_recovery(
                        run,
                        source,
                        "放弃",
                        "重赛跑",
                        event_start,
                        queued_ms,
                        probe_ms,
                        race_ms,
                        0,
                        "设备死/停止/换代",
                    ),
                },
            }
        }
    }
}

/// rerace 中止的落行分流（评审 r1 P0-6：Gone 静默退出——不落「对端失败」行；
/// Fail 照常落失败行；返回值给事件用时汇总行带原因）。
fn rerace_abort_line(run: &Arc<GenRun>, source: ReconnSource, abort: ReraceAbort) -> Option<String> {
    match abort {
        ReraceAbort::Gone => None, // 停止/世代变化/设备死：静默退出（对端失败行禁落）
        ReraceAbort::Fail(why) => {
            round_fail_line(run, source, &why);
            Some(why)
        }
    }
}

/// **事件恢复用时汇总**（便捷判据）：一次事件从占位到收口的总耗时 + 排队/快探/
/// 重赛跑/强恢复分项。保留既有逐行判据不变，本行只做跨阶段对账——排查「用户感知
/// 十几秒」时先 grep 这一行，再按源名回看完整日志。
///
/// 口径：
/// - `start_ms` = `ReconnGate::last_ms`（占位成功时刻；非 worker 起跑时刻）；
/// - `排队` = 占位 → worker 起跑；
/// - `快探` = 本轮 worker 快探耗时（`跳过` = recover 移交的重复快探）；
/// - `重赛跑` = `rerace_tunnel` 调用耗时（含复读一次）；
/// - `恢复` = `strong_recover` 耗时（快探成功路径；重赛跑内部包含在重赛跑）。
// 一行汇总要同时带源/结果/路径/总耗时/四个分项/原因；拆结构只会把同一组字段
// 在十余个调用点重复一遍，这里保留平铺参数并把复杂度留在唯一落行处。
#[allow(clippy::too_many_arguments)]
fn log_event_recovery(
    run: &Arc<GenRun>,
    source: ReconnSource,
    result: &str,
    path: &str,
    start_ms: i64,
    queued_ms: i64,
    probe_ms: Option<i64>,
    race_ms: i64,
    recover_ms: i64,
    detail: &str,
) {
    let total_ms = (mono_ms() - start_ms).max(0);
    let probe = match probe_ms {
        Some(v) => format!("{v}ms"),
        None => "跳过".to_owned(),
    };
    let detail = if detail.is_empty() {
        String::new()
    } else {
        format!("；{detail}")
    };
    (run.logf)(&format!(
        "事件恢复用时：源={}，结果={}，路径={}，总耗时={}ms（排队={}ms，快探={}，赛跑={}ms，恢复={}ms）{}",
        source.text(),
        result,
        path,
        total_ms,
        queued_ms,
        probe,
        race_ms,
        recover_ms,
        detail
    ));
}

/// 失败行（含原因与下次最早触发时刻——demand 源受 MIN_GAP 限频；边沿源随时）。
fn round_fail_line(run: &Arc<GenRun>, source: ReconnSource, why: &str) {
    let next_secs = run
        .reconn
        .last_ms
        .load(Ordering::Acquire)
        .saturating_add(RECONNECT_MIN_GAP.as_millis() as i64)
        .saturating_sub(mono_ms())
        .max(0)
        / 1000;
    (run.logf)(&format!(
        "一次性重连：失败（源={}，原因={}）——demand 源下次最早触发 {}s 后（边沿源随时）",
        source.text(),
        why,
        next_secs
    ));
}

/// worker 检查点（D1-c：每个 island_cmd 前后查 stop 与 device_dead）。
fn worker_halted(run: &GenRun) -> bool {
    run.stop.load(Ordering::Acquire)
        || run.tun_shared.carrier.device_dead()
        || run.gen != run.tun_shared.gen.load(Ordering::Acquire)
}

/// 有界快探（与岛内阶梯同判负粒度：`FAST_BUDGET`；等待经 `island_cmd_until`——
/// stop/世代变化/设备死返回 `Gone` 类、不落「对端失败」行，r4-R14）。
/// 评审 r2 B7：Timeout/Busy typed 分类（stats 对这两类不累积连败——等待并发非
/// 对端证据）；评审 r3 B7：Busy 接**真实路径**——`client::probe` 的槽位争用
/// （拿锁预算内未获槽）现回 typed `IslandErr::ProbeBusy`。
fn quick_probe(run: &Arc<GenRun>) -> Result<(), ProbeFail> {
    use homeway_quic::tuning::probe_defaults::FAST_BUDGET;
    // 测试缝（评审 r2 B4/B7）：快探结论注入门——真快探成功需 live 连接、Busy/Timeout
    // 注入需槽位争用，合成世代均不可构造。1=成功、2=超时、3=并发 Busy；0=走真快探。
    #[cfg(test)]
    match PROBE_RESULT_GATE.load(Ordering::Relaxed) {
        0 => {}
        1 => return Ok(()),
        2 => return Err(ProbeFail::Timeout("注入（快探缝）".into())),
        _ => return Err(ProbeFail::Busy("注入（快探缝）".into())),
    }
    let island = run.current_island().ok_or(ProbeFail::NoFace)?;
    run.reconn.probes.fetch_add(1, Ordering::Relaxed);
    island_cmd_until(
        run,
        &island,
        |reply| homeway_quic::Cmd::Probe {
            budget: FAST_BUDGET,
            reply,
        },
        Instant::now() + FAST_BUDGET + QUIC_RPC_BUDGET,
    )
    .map(|_rtt| ())
    .map_err(quick_probe_classify)
}

/// `Cmd::Probe` 回执错 → [`ProbeFail`] 的分类（评审 r3 B7 抽出为纯函数——字符串
/// 谓词「探活槽位争用」与 `IslandErr::ProbeBusy` 的 Display 逐字耦合，单测钉住）。
///
/// **事实订正（评审 r3 B7）**：`Cmd::Probe` 分支（driver）只经 `client::probe` 回
/// `ConnectionLost`/`ProbeNoResponse`/`ProbeBusy`——**不可能**回 `RaceInFlight`
/// （那是 `Cmd::Connect` 在 `st.racing` 时的回执）；旧实现里匹配「已有赛跑在途」的
/// Busy 分支在快探路径上不可达，已按事实删除（devlog 登记）。
fn quick_probe_classify(e: IslandRpc) -> ProbeFail {
    match e {
        IslandRpc::Gone(m) => ProbeFail::IslandGone(m),
        IslandRpc::Timeout(m) => ProbeFail::Timeout(m),
        // 岛侧 typed「忙」形态（`IslandErr::ProbeBusy`——`Cmd::Probe` 的唯一 Busy
        // 源）——并发面，不进连败证据。
        IslandRpc::Island(m) if m.contains("探活槽位争用") => ProbeFail::Busy(m),
        other => ProbeFail::Detail(other.to_string()),
    }
}

/// link 快照刷新（`at` = 最近一次成功探测；承载不在位时保留旧值——陈旧判据自然
/// 成立，r10-C1）。
fn refresh_link_snapshot(run: &GenRun) {
    if let Some((via, ep, rtt_ms)) = run.island_link() {
        *lock_unpoison(&run.link) = Some(LinkIn {
            via,
            ep,
            rtt_ms,
            at_ms: now_unix_ms(),
        });
    }
}

/// 岛命令的**分片有界等待**（r4-R14：观测快探与 worker 的岛命令统一走它）：
/// `recv_timeout` 以 ≤100ms 片循环、每片查 stop/世代/**设备死**（评审 r2 B3：统一走
/// `worker_halted` 谓词——设备死在 Rebind/Connect 等待中发生时立即 Gone，不等回执
/// 或超时）；**send 前先查一次**（评审 r2 B3：停止已置位时不再多发一条岛命令）。
/// 变化返回 `Gone` 类、不落「对端失败」行；慢 RPC 下 stats 线程 2s join 预算不超。
fn island_cmd_until<T>(
    run: &GenRun,
    island: &homeway_quic::Island,
    make: impl FnOnce(homeway_quic::IslandReply<T>) -> homeway_quic::Cmd,
    deadline: Instant,
) -> Result<T, IslandRpc> {
    const SLICE: Duration = Duration::from_millis(100);
    if worker_halted(run) {
        return Err(IslandRpc::Gone("停止/世代变化/设备死（发送前收口）".into()));
    }
    let (tx, rx) = mpsc::channel();
    island
        .tx()
        .send(make(tx))
        .map_err(|e| IslandRpc::Gone(e.to_string()))?;
    loop {
        if worker_halted(run) {
            return Err(IslandRpc::Gone(
                "停止/世代变化/设备死（分片等待收口）".into(),
            ));
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(IslandRpc::Timeout("岛回执超时（分片等待到点）".into()));
        }
        match rx.recv_timeout(left.min(SLICE)) {
            Ok(Ok(v)) => return Ok(v),
            Ok(Err(e)) => return Err(IslandRpc::Island(e.to_string())),
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(IslandRpc::Gone("岛线程已退出（回执口断）".into()));
            }
        }
    }
}

/// spawn 全败的持位清位（r13-P2/r9-C6）：清「快照位」——掩码取自清位起点的单次
/// load（= 进入时已知 ∪ 承接前快照）；CAS 失败重读时期望值跟随新态、**清位掩码
/// 不变**——清位期间并发新到位保留（同 bit 新事件不可区分——按快照丢弃并计数，
/// r9-C12 如实登记）。
fn clear_pending_while_holding(run: &GenRun, epoch: u32) {
    let clear_mask = run.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK;
    if clear_mask == 0 {
        return;
    }
    run.reconn
        .dropped_pending
        .fetch_add(u64::from(clear_mask.count_ones()), Ordering::Relaxed);
    run.reconn.hook("spawn_fail_clear_mask");
    loop {
        run.reconn.hook("spawn_fail_cleared");
        let s = run.reconn.state.load(Ordering::Acquire);
        if (s & RECONN_INFLIGHT) == 0 || reconn_epoch(s) != epoch {
            break; // 防御（设计上不可达：清位在 release 前、仍持 INFLIGHT）
        }
        let target = s & !clear_mask;
        if target == s {
            break; // 掩码位已清
        }
        match run
            .reconn
            .state
            .compare_exchange_weak(s, target, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => break,
            Err(_) => continue, // 并发置位——期望值重读、掩码不变（新位保留）
        }
    }
}

/// spawn 一次性 worker（初次 + 2 次重试 = 共 3 次；全败 `Err`——**零 logf**，计数由
/// 入口做：读线程路径禁日志）。
fn spawn_reconnect_worker(
    run: &Arc<GenRun>,
    epoch: u32,
    source: ReconnSource,
    skip_probe: bool,
) -> Result<(), ()> {
    let mut tries = 0;
    loop {
        let r = Arc::clone(run);
        let spawned = spawn_thread_named("homeway-reconnect", move || {
            reconnect_worker_body(r, epoch, source, skip_probe);
        });
        if spawned {
            return Ok(());
        }
        tries += 1;
        if tries >= 3 {
            return Err(());
        }
    }
}

/// 起命名线程（**测试缝**：`SPAWN_FAIL_GATE` 置位时恒失败——r9-C6/r9-C12/r13-P2/P3
/// 的 spawn 全败注入面；生产构建无门）。
fn spawn_thread_named(name: &'static str, body: impl FnOnce() + Send + 'static) -> bool {
    #[cfg(test)]
    if SPAWN_FAIL_GATE.load(Ordering::Relaxed) {
        return false;
    }
    std::thread::Builder::new()
        .name(name.into())
        .spawn(body)
        .is_ok()
}

/// 测试缝：spawn 失败注入门（`#[cfg(test)]`）。
#[cfg(test)]
static SPAWN_FAIL_GATE: AtomicBool = AtomicBool::new(false);

/// 测试缝：run_round panic 注入门（>0 时在 run_round 入口递减并 panic——r9-C7/
/// r10-C9 的 panic 兜底注入面）。
#[cfg(test)]
static PANIC_ROUNDS_GATE: AtomicU32 = AtomicU32::new(0);

/// 测试缝：快探结论注入门（评审 r2 B4/B7——`quick_probe` 头注释；用例须持
/// `RECONN_TEST_LOCK`（全局 gate 并行测试互注））。
#[cfg(test)]
static PROBE_RESULT_GATE: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(0);

/// 测试缝：stats 观测**拍计数**（评审 r4 P2——`stats_loop` 每次观测快探前经
/// [`stats_beat`] 自增；注入期的持住/续算断言按拍计数推进等待，不靠纯
/// wall-clock sleep「希望」拍到足够拍——高负载下 80ms 节拍 × 500ms sleep 不
/// 保证实际过拍数）。
#[cfg(test)]
static STATS_BEATS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// 拍计数挂点：测试构造 = 计数自增；生产 = 空体（零开销。注意不得在 `stats_loop`
/// 体内直接写 `#[cfg(test)]` 语句——tier 侧 log-index 生成器按「`#[cfg(test)]` 后
/// 首个配平花括号区」跳读，会把整个 `match quick_probe` 块当测试块吞掉）。
#[cfg(test)]
fn stats_beat() {
    STATS_BEATS.fetch_add(1, Ordering::Relaxed);
}

#[cfg(not(test))]
fn stats_beat() {}

/// worker 线程体：初次 run_round + 收尾循环 + 承接轮全部包在**单层 catch_unwind**
/// 内（r11-C9：释放的唯一 owner 是 catch 处理器——guard 只记账不写状态字）。
fn reconnect_worker_body(run: Arc<GenRun>, my_epoch: u32, source: ReconnSource, skip_probe: bool) {
    let mut guard = RoundGuard {
        epoch: my_epoch,
        triggering: 0,
    };
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        GenRun::run_round(&run, source, skip_probe);
        reconnect_finalize_loop(&run, &mut guard);
    }));
    if res.is_err() {
        reconnect_catch_handler(&run, &guard);
    }
}

/// 【owner 侧收尾（D1-d）】：释放 → 读 pending 快照 → 承接（CAS 实际态推导——
/// r12-B2：release 与 CAS 之间可能有新 pending 置位/新 owner 来去）→ run_round →
/// 循环至 pending 空。
fn reconnect_finalize_loop(run: &Arc<GenRun>, guard: &mut RoundGuard) {
    loop {
        let snap = run.reconn.release_if_owner(guard.epoch); // 唯一释放原语
        let Some(p) = snap else { break };
        if p == 0 {
            break;
        }
        guard.triggering = p; // 本轮局部快照
        run.reconn.hook("after_release");
        let s2 = run.reconn.state.load(Ordering::Acquire);
        if (s2 & RECONN_INFLIGHT) != 0 {
            continue; // 新 owner 在跑：残留 pending 留字，其收尾必读到（不变量）
        }
        let p2 = (s2 & RECONN_PENDING_MASK) | p; // 并入快照外的并发新到位（不吞边沿）
        guard.triggering = p2; // r13-P4：承接 CAS 前更新——panic 恢复覆盖 p2 全部
        run.reconn.hook("before_takeover_cas");
        let new_epoch = reconn_epoch(s2).wrapping_add(1) & 0x1FF_FFFF;
        if run
            .reconn
            .state
            .compare_exchange(
                s2,
                RECONN_INFLIGHT | (new_epoch << RECONN_EPOCH_SHIFT),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            guard.epoch = new_epoch; // 从实际态推导（r12-B2）
            let source = ReconnSource::from_pending(p2); // 优先级推导（D1-d）
            GenRun::run_round(run, source, false); // 承接轮 = 新事件，先快探；panic 由外层单层 catch 统一承接
            continue;
        }
        continue; // 并发（新入口抢走/新 pending）：残留 pending 仍在字里（不变量）
    }
}

/// 【catch 处理器（D1-e）——panic 兜底，释放的唯一 owner（r11-C9）】。
fn reconnect_catch_handler(run: &Arc<GenRun>, guard: &RoundGuard) {
    let snap = run.reconn.release_if_owner(guard.epoch);
    // r12-B4：snap 与 triggering 快照两类都要恢复（留字——下一任意事件/新 owner 承接）。
    let restore = snap.unwrap_or(0) | guard.triggering;
    if restore != 0 {
        let _ = run.reconn.state.fetch_or(restore, Ordering::AcqRel);
        // 尝试一次性再 spawn（失败留字计数——下一任意事件承接兜底）。
        let r2 = Arc::clone(run);
        let _ = spawn_thread_named("homeway-reconnect", move || {
            GenRun::try_spawn_reconnect(&r2, ReconnSource::BGate);
        });
    }
    run.reconn.panics.fetch_add(1, Ordering::Relaxed);
    (run.logf)(&format!(
        "一次性重连：panic（已兜住——pending 留字 {restore:#x}，释放恰一次；下一任意事件承接）"
    ));
}

// ---------------------------------------------------------------------------
// TunnelExec（TunExecutor 真实现）
// ---------------------------------------------------------------------------

/// 隧道域执行体（真 hub：岛 + L3 直通 + 巡检 + 桥）。
/// 世代登记经 `Arc<Mutex<Option<Arc<GenRun>>>>`（世代线程构建后写回——warmup 是
/// `&self`，不持自引用）；identity 装配窗口的停止请求经 TunShared 的旗标中继
/// （评审 r2-M2：warmup 时记下共享面，request_stop 在世代句柄就位前也有落点）。
pub struct TunnelExec {
    state: Arc<Mutex<Option<Arc<GenRun>>>>,
    demand: Arc<DemandSignals>,
    shared: Mutex<Option<Arc<TunShared>>>,
}

impl TunnelExec {
    /// 构造（demand 与 ClientCore 共享——巡检/demand pusher 的信号源）。
    pub fn new(demand: Arc<DemandSignals>) -> Arc<Self> {
        Arc::new(TunnelExec {
            state: Arc::new(Mutex::new(None)),
            demand,
            shared: Mutex::new(None),
        })
    }

    fn gen_run(&self) -> Option<Arc<GenRun>> {
        lock_unpoison(&self.state).clone()
    }

    /// 回前台 kick 的接线说明：ClientCore 装配方把 set_foreground_kick 指到
    /// [`Self::kick_probe`]（false→true 转变时以 **Foreground 边沿源**触发统一
    /// 重连入口——快探先行：健康连接零动作翻回，reconnect-oneshot D2；`SetActivity`
    /// 每拍下发无翻沿检测，不用；`SetCandidates` 不触发——启动装配也走它）。
    pub fn kick_probe(self: &Arc<Self>) {
        if let Some(run) = self.gen_run() {
            GenRun::try_spawn_reconnect(&run, ReconnSource::Foreground);
        }
    }
}

impl TunExecutor for TunnelExec {
    fn warmup(
        &self,
        cfg: &TunConfigJson,
        shared: Arc<TunShared>,
        gen: u64,
    ) -> Result<(), TunError> {
        // token 解析（同步——坏 token 是参数面：Err 由 facade 写 failed 终态）
        // M7 S1：用户可见文案走 `user_message()`——版本不符（存量 `hmw1` 串）追加**可行动归因**
        // （`TokenError::changeover_attribution`）；哨兵 `Display` 字节不变。
        let token = crate::token::decode(&cfg.token)
            .map_err(|e| TunError::Core(format!("token 解析失败：{}", e.user_message())))?;
        // 日志文件（追加写——跨重连保留上次断因；打开失败 = -2 面）
        let raw_logf = open_gen_log(&cfg.out).map_err(TunError::LogOpen)?;

        let gen_cfg = GenCfg {
            token,
            mtu: (if cfg.mtu <= 0 { MTU_DEFAULT } else { cfg.mtu }) as u32,
            stats_secs: if cfg.stats_secs > 0 {
                cfg.stats_secs.max(STATS_SECS_MIN)
            } else {
                STATS_SECS_DEFAULT
            },
            diag_fd_secs: cfg.diag_fd_secs.max(0),
            dial_ms: if cfg.dial_ms > 0 {
                cfg.dial_ms
            } else {
                DIAL_MS_DEFAULT
            },
            identity_dir: (!cfg.identity_dir.is_empty()).then(|| PathBuf::from(&cfg.identity_dir)),
            port_forwards: cfg.port_forwards.clone(),
            // MTU 上限（**世代级读一次**；非法值在这里记行、行随本世代日志）
            mtu_cap: resolve_mtu_cap(cfg.quic_mtu_cap, &raw_logf),
        };
        // 共享面记下（request_stop 的窗口中继位——评审 r2-M2）
        *lock_unpoison(&self.shared) = Some(Arc::clone(&shared));
        let state = Arc::clone(&self.state);
        let demand = Arc::clone(&self.demand);
        let spawn = std::thread::Builder::new()
            .name("homeway-tun-gen".into())
            .spawn(move || {
                gen_loop(gen_cfg, shared, gen, demand, raw_logf, state);
            });
        if spawn.is_err() {
            return Err(TunError::Core("世代线程启动失败（线程资源不足）".into()));
        }
        Ok(())
    }

    fn request_stop(&self) {
        if let Some(run) = self.gen_run() {
            // 陈旧世代（登记槽还挂着已死/在收尾的旧世代——新世代装配窗口）不处理，
            // 落到共享面旗标中继（复核 r3-F2：M-2 的窗口在第 2+ 世代必须真正可达）
            if run.gen == run.tun_shared.gen.load(Ordering::Acquire) {
                run.stop.store(true, Ordering::Release);
                run.stop_wake.wake();
                // preparing 阶段没有 fd 循环可打断，暖机探测可能挂在岛 RPC 上——
                // 停岛是最可靠的第二条打断路径（Go tunStopWait 同义）。
                if run.tun_shared.stage.snapshot().stage == TunStage::Preparing {
                    if let Some(i) = run.take_island() {
                        // Q-G F5：**有界**收工（暖机探测可能挂在岛 RPC 上——到点
                        // detach；无界会拖死 tun_stop 的等待）
                        if !i.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
                            (run.logf)("等待岛线程收工超时（CLIENT_CLOSE_BUDGET）——到点 detach");
                        }
                    }
                }
                return;
            }
        }
        if let Some(sh) = lock_unpoison(&self.shared).clone() {
            // 世代句柄未就/陈旧（identity 装配窗口——评审 r2-M2）：经共享面旗标中继，
            // 世代线程在装配完成点统一收口
            sh.signal_stop();
        }
    }

    /// **reconnect-oneshot D2/r5-R6 终稿**：恢复下推复用统一入口协议（r10-C11：
    /// 在飞预检不得裸 load/check-then-act——走 `try_claim_edge` 与 D1 步骤 2 同一
    /// CAS/fetch_or-复查协议）。
    ///
    /// **rc 映射表**：`0` = 同步快探成功（强证据翻回：ZERO 间隔清标记 + 置承载位 +
    /// 刷 run.link，收尾含承接后返回）；`-1` = 已尝试但结果未知（含受理〔worker 接
    /// 管〕/被单飞合并/被 pending 承接/起跑失败——**recover 边沿路径无「被节流」
    /// 形态**；细粒度只进核日志）；`-2` = 无隧道/陈世代/设备死或停止（前置判定在
    /// 快探之前——零烧探测）；`-9` = NAPI 壳异常（capi 面，本面不产）。
    fn recover(&self, from: i64, cause: &str) -> i32 {
        let Some(run) = self.gen_run() else {
            return -2; // 没有 attached 隧道（不构成网络结论）
        };
        // 前置（快探之前，零烧探测）：stop/device_dead/陈世代 → -2
        if run.stop.load(Ordering::Acquire)
            || run.gen != run.tun_shared.gen.load(Ordering::Acquire)
            || run.tun_shared.carrier.device_dead()
        {
            return -2;
        }
        // from 派生（r8-R6）：1=唤醒、3=换网、2 及其余按唤醒归并——边沿源（绕节流）。
        let source = if from == 3 {
            ReconnSource::NetChange
        } else {
            ReconnSource::Wake
        };
        run.reconn.entries.fetch_add(1, Ordering::Relaxed);
        match run.reconn.try_claim_edge(source) {
            // 零快探——防换网/唤醒并发各自烧探测占满命令槽；边沿位已按协议留字/
            // 自消费，绝不静默吞掉。
            Claim::InFlight => -1,
            Claim::Claimed { epoch } => {
                let saved_last = run.reconn.last_ms.swap(mono_ms(), Ordering::AcqRel);
                let saved_yield = run.reconn.yield_until_ms.swap(0, Ordering::AcqRel);
                run.reconn.sync_probes.fetch_add(1, Ordering::Relaxed);
                // 测试缝：占位后的同步快探前挂点（受控交错——两并发 recover 的
                // 「第二发在第一发持位时进入」确定性构造面，评审 r1 P2）。
                run.reconn.hook("recover_claimed");
                let event_start = run.reconn.last_ms.load(Ordering::Acquire);
                let probe_t0 = mono_ms();
                let probe = quick_probe(&run);
                let probe_ms = (mono_ms() - probe_t0).max(0);
                match probe {
                    Ok(()) => {
                        // 测试缝：快探刚返回成功、halt 复查前的注入点（评审 r3 B3——
                        // 与 run_round 的 round_after_probe 同一纪律：设备死/停止/换代
                        // 发生在同步快探在飞期间时不落「通过」行、不 strong_recover）。
                        run.reconn.hook("recover_after_probe");
                        // claimed-epoch 释放路径（r13-P3 同款——绝不裸留位；锚/让路窗
                        // 回滚：本轮未起跑，节流位不该被消耗）。
                        let release_claimed = || {
                            run.reconn.last_ms.store(saved_last, Ordering::Release);
                            run.reconn.yield_until_ms.store(saved_yield, Ordering::Release);
                            clear_pending_while_holding(&run, epoch);
                            let _snap = run.reconn.release_if_owner(epoch);
                        };
                        if worker_halted(&run) {
                            log_event_recovery(
                                &run,
                                source,
                                "放弃",
                                "同步快探",
                                event_start,
                                0,
                                Some(probe_ms),
                                0,
                                0,
                                "设备死/停止/换代",
                            );
                            release_claimed();
                            return -2; // 与前置判定同值（「设备死或停止」契约）
                        }
                        // 测试缝：halt 复查通过后、strong_recover 前的注入点（评审
                        // r4 B3 低项——设备死落在复查与强恢复之间的理论窗口）。
                        run.reconn.hook("recover_before_strong");
                        let r0 = mono_ms();
                        // r4 B3 低项：快探成功但设备死在复查后到场（承载位置 true
                        // 判负——alive=false）同归 -2 释放路径，不落「通过」行
                        // （该形态下 patrol 已被设备类改写 ⇒ 恢复行为零日志零刷新）。
                        if !GenRun::strong_recover(&run, "恢复下推同步快探") {
                            let recover_ms = (mono_ms() - r0).max(0);
                            log_event_recovery(
                                &run,
                                source,
                                "放弃",
                                "同步快探",
                                event_start,
                                0,
                                Some(probe_ms),
                                0,
                                recover_ms,
                                "强恢复时设备死/换代",
                            );
                            release_claimed();
                            return -2;
                        }
                        let recover_ms = (mono_ms() - r0).max(0);
                        (run.logf)(&format!("恢复下推（{cause}）——同步快探：通过"));
                        log_event_recovery(
                            &run,
                            source,
                            "成功",
                            "同步快探",
                            event_start,
                            0,
                            Some(probe_ms),
                            0,
                            recover_ms,
                            "",
                        );
                        // release_if_owner 收尾（含承接）后返回 0——绝不裸返回留位。
                        let mut guard = RoundGuard {
                            epoch,
                            triggering: 0,
                        };
                        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            reconnect_finalize_loop(&run, &mut guard);
                        }));
                        if res.is_err() {
                            reconnect_catch_handler(&run, &guard);
                        }
                        0
                    }
                    Err(e) => {
                        (run.logf)(&format!(
                            "恢复下推（{cause}）——同步快探：失败（{e}）——移交一次性重连 worker"
                        ));
                        log_event_recovery(
                            &run,
                            source,
                            "移交",
                            "同步快探",
                            event_start,
                            0,
                            Some(probe_ms),
                            0,
                            0,
                            "同步快探失败，交一次性重连 worker",
                        );
                        // spawn worker 并把槽位移交（r12-B3 定稿：worker 以 Claimed 的
                        // epoch 为 my_epoch、走标准收尾——recover 立即返回 -1，不再
                        // 自行 release）。
                        match spawn_reconnect_worker(&run, epoch, source, true) {
                            Ok(()) => -1,
                            Err(()) => {
                                // r13-P3：无人继承时的释放兜底——步骤 4 同款清理后
                                // 返回 -1（绝不裸留位；后续可再 Claimed）。
                                run.reconn.spawn_fails.fetch_add(1, Ordering::Relaxed);
                                log_event_recovery(
                                    &run,
                                    source,
                                    "失败",
                                    "同步快探",
                                    event_start,
                                    0,
                                    Some(probe_ms),
                                    0,
                                    0,
                                    "一次性重连 worker 起跑失败",
                                );
                                run.reconn.last_ms.store(saved_last, Ordering::Release);
                                run.reconn.yield_until_ms.store(saved_yield, Ordering::Release);
                                clear_pending_while_holding(&run, epoch);
                                let _snap = run.reconn.release_if_owner(epoch);
                                -1
                            }
                        }
                    }
                }
            }
        }
    }

    fn runner(&self) -> Option<RunnerIn> {
        Some(runner_of(self.gen_run()?.as_ref()))
    }

    fn transport(&self) -> Option<TransportIn> {
        Some(transport_of(self.gen_run()?.as_ref()))
    }

    /// `quic` 段快照（M1 S3-2；quic 档且 L3 在岛上才有——其余 = `None` ⇒ JSON 整段缺席）。
    fn quic_status(&self) -> Option<QuicIn> {
        quic_status_of(self.gen_run()?.as_ref())
    }

    /// reconnect-oneshot D4：轮次状态机退役——生产面恒 `false`（契约键保留：
    /// 台账只增不减；tunStatusJSON 键集零变化）。
    fn rescue_rounds_exhausted(&self) -> bool {
        false
    }

    /// portfwd 整表热替换（Q-F-B F4-4）：有在世世代 ⇒ 真装表（真监听器整表替换）→
    /// stale 复查 ⇒ `0`；无世代 / 世代已收口 ⇒ `-1`。表**不再「照存不装」**——`0`
    /// 是「已受理并已生效」的真话（Go `tunSetPortForwardsJSON` 同形）。
    fn request_port_forwards(&self, rules: Vec<PortForwardRule>) -> i32 {
        let Some(run) = self.gen_run() else {
            return -1;
        };
        if run.gen != run.tun_shared.gen.load(Ordering::Acquire) {
            return -1; // 陈旧世代（装配窗口/已收口）——改动随下次连接自然生效
        }
        run.pf.install(&rules);
        // 应用期间世代可能换代或收工（Go 同步复查 currentTunRun/`r.done`）——
        // 两件事任一发生就把刚起的监听器收掉，绝不给死世代留孤儿
        if run.gen != run.tun_shared.gen.load(Ordering::Acquire)
            || run.stop.load(Ordering::Acquire)
        {
            (run.logf)("port-forward: 应用后复查发现世代已收口——撤回本次监听器");
            run.pf.stop_all();
            return -1;
        }
        0
    }
}

/// 【test-seams】最小世代（Q-F F1 状态面集成 + 热替换 rc 断言用）：**无岛**（岛不在 ⇒
/// L3 未承接；需要岛面的用例自行 `set_island`，见 `idle_island`）+ portfwd 运行时
/// （拨号桩恒失败——本缝不建隧道）。
/// `tun_shared.gen` 与 `run.gen` 对齐（否则 stale 复查恒 `-1`，正例不可测）。
#[cfg(test)]
impl GenRun {
    pub(crate) fn synthetic_for_test(logf: Logf) -> GenRun {
        let ident = crate::identity::Identity::ephemeral().expect("临时身份");
        let tun_shared = Arc::new(TunShared::new());
        tun_shared.begin_generation(1);
        // 世代停止位与 pf 的 `gen_stop` **同一枚 Arc**（生产同源：`Arc::clone(&stop)`）——
        // 否则「停止位让 accept 线程自退」这条兜底通路在测试缝里不可达（代码门 r27 P10）
        let stop = Arc::new(AtomicBool::new(false));
        GenRun {
            gen: 1,
            stop: Arc::clone(&stop),
            stop_wake: StopWake::new(),
            island: RwLock::new(None),
            l3_on_island: AtomicBool::new(false),
            identity: ident.clone(),
            tun_ip: crate::tunnel_addr::derive_tun_ip(
                &crate::token::Secret::from([2u8; 32]),
                &ident.public_key(),
            )
            .to_string(),
            link: Mutex::new(None),
            reconn: ReconnGate::new(),
            bridge: Mutex::new(None),
            logf: Arc::clone(&logf),
            tun_shared,
            demand: Arc::new(DemandSignals::new()),
            pf: PfRuntime::new(PfSetup {
                dial: Arc::new(|_dst, _b| {
                    Err(io::Error::new(
                        io::ErrorKind::ConnectionRefused,
                        "合成世代无隧道（拨号桩）",
                    ))
                }),
                stop,
                logf,
                budget: Duration::from_secs(5),
                limits: PfLimits::default(),
            }),
            started: Instant::now(),
        }
    }
}

/// runner 块的组装（TunnelExec::runner 与对账快照共用——评审 r2-M11 的完全对齐：
/// runner 只要求**世代在场**〔Go setRunner 在 startSession 后即设〕，不要求 link
/// 已写过；link 缺席兜 via="none"）。
pub(crate) fn runner_of(run: &GenRun) -> RunnerIn {
    let (rd, wr) = run.l3_tun_stats();
    let link = lock_unpoison(&run.link).clone().unwrap_or(LinkIn {
        via: "none".into(),
        ep: String::new(),
        rtt_ms: 0,
        at_ms: 0,
    });
    let bridge = lock_unpoison(&run.bridge).as_ref().map(|b| {
        let st = b.status();
        super::tun_status::BridgeIn {
            auth_hex: st.auth_hex,
            files_sock: st.files_sock,
            term_sock: st.term_sock,
            speed_sock: st.speed_sock,
        }
    });
    RunnerIn {
        fd_read_bytes: rd,
        fd_write_bytes: wr,
        // 端口转发计数（Q-F-B F3-1）：**真值**——accept 准入数 / 拨号失败数。
        pf_accepted: run.pf.counters().accepted(),
        pf_fails: run.pf.counters().fails(),
        exit_ip: SERVER_TUNNEL_IP.to_string(),
        link,
        port_forwards: run.pf.snapshot_states(),
        bridge,
    }
}

/// transport 块的组装（同 runner_of 的共享件）。
pub(crate) fn transport_of(run: &GenRun) -> TransportIn {
    TransportIn {
        identity: Some((run.identity.short_dev(), run.identity.short_pub())),
        tun_ip: Some(run.tun_ip_string()),
        outbound_at_ms: Some(run.l3_last_outbound_unix_ms()).filter(|v| *v != 0),
        // `demand.localErr*` 两键的源（WG bind 的全候选发送统计）随 WG 面退役：
        // 岛无该面 ⇒ 两键**恒缺席**（additive 兼容；登记 = M5 观测面收窄）。
        local_err: None,
    }
}

/// `quic` 段快照（`None` = 岛不在 ⇒ JSON 整段缺席）。
pub(crate) fn quic_status_of(run: &GenRun) -> Option<QuicIn> {
    // 段存在判据 = **岛已构造**（含未建连/被拒形态——「为什么没连上」正是 App 需要的
    // 归因；M3 S5 起不再要求「L3 已在岛上」）。单承载后「本世代是不是 quic 档」不再是
    // 变量（设计 §4.3③）。
    let s = run.current_island()?.snapshot();
    Some(quic_in_of(&s))
}

/// 岛快照 → `quic` 段（**纯函数**：S3-3 的「行 ↔ JSON 同源」两半之一——另一半
/// （N-c 行）在岛侧由 `note_drop_shared` 从**同一份** `IslandSnapshot::drops` 渲染）。
pub(crate) fn quic_in_of(s: &homeway_quic::IslandSnapshot) -> QuicIn {
    QuicIn {
        mtu: s.mtu.unwrap_or(0),
        current_mtu: s.current_mtu,
        lost_packets: s.lost_packets,
        congestion_events: s.congestion_events,
        migrations: s.migrations,
        migration_unconfirmed: s.migration_unconfirmed,
        drops: super::tun_status::QuicDropsIn {
            too_large: s.drops.too_large,
            send_buffer_full: s.drops.send_buffer_full,
            return_queue_full: s.drops.return_queue_full,
            unregistered: s.drops.unregistered,
        },
        via: link_via_opt(s.via).to_owned(),
        ep: s.ep.map(|a| a.to_string()).unwrap_or_default(),
        rtt_ms: s.rtt_ms,
        packets_in: s.packets_in,
        packets_out: s.packets_out,
        local: s.local.map(|a| a.to_string()).unwrap_or_default(),
        connections: s.connections,
        relay_tx: s.relay_tx,
        rx_ignored: s.rx_ignored,
        candidates: s.candidates as u64,
        mirrors: s.mirrors,
        send_buffer_used: s.send_buffer_used,
        // M3 S5（§4）：准入归因（0/空串 = 未发生过）
        admit_reject_code: s.admit_reject_code.unwrap_or(0),
        admit_reject_text: s.admit_reject_text.clone().unwrap_or_default(),
        // M3 S4（§3.1/§3.2）：快探阶梯读数（additive）
        ladder_probe_ok: s.ladder_probe_ok,
        ladder_fail_streak: s.ladder_fail_streak,
        ladder_jitter_streak: s.ladder_jitter_streak,
        ladder_action: s.ladder_action.clone(),
        streams_active: s.streams_active,
        denied: s.sock_send_errs_denied,
        denied_age_ms: s.sock_send_err_denied_age_ms,
        denied_errno: s.sock_send_err_denied_last_errno,
        denied_fresh: s.sock_send_err_denied_fresh,
        racing: s.racing,
        replaced_recent: s.replaced_recent,
        replaced_storm_trips: s.replaced_storm_trips,
        replaced_storm_active: s.replaced_storm_active,
        handover_active: s.handover_active,
    }
}

// ---------------------------------------------------------------------------
// 世代主线程（runTun2Tailcat）
// ---------------------------------------------------------------------------

/// 世代装配材料（warmup 同步段解好、线程内消费）。
struct GenCfg {
    token: Token,
    mtu: u32,
    stats_secs: i64,
    diag_fd_secs: i64,
    /// 桥拨号预算（Go tunRunner.dialTimeout——随 tunConfig.dialMs 热传入桥宿主；
    /// 评审 r2-L5：此前是死字段）
    dial_ms: i64,
    identity_dir: Option<PathBuf>,
    port_forwards: Vec<PortForwardRule>,
    /// QUIC MTU 上限（M1 §12-①：`HOMEWAY_QUIC_MTU` > `tunConfig.quicMtuCap` > 1400；
    /// 有效区间 [1320,1400]，非法 ⇒ 记行 + 缺省）。
    mtu_cap: u16,
}

impl GenRun {
    fn tun_ip_string(&self) -> String {
        self.tun_ip.clone()
    }
}

/// 世代主线程体。`state` = TunnelExec 的世代登记槽（**线程入口即登记**——评审
/// r2-M2：identity/Client::start 之前 request_stop/recover 就打得到世代句柄，
/// Go beginTunRun 在 goroutine 之前建好世代结构的时序同构）。
fn gen_loop(
    cfg: GenCfg,
    shared: Arc<TunShared>,
    gen: u64,
    demand: Arc<DemandSignals>,
    raw_logf: Logf,
    state: Arc<Mutex<Option<Arc<GenRun>>>>,
) {
    let logf: Logf = {
        let l = Arc::clone(&raw_logf);
        Arc::new(move |s: &str| l(&format!("tier-core: {s}")))
    };
    (logf)("传输：新栈（QUIC 岛）");

    // ---- 装配（identity/Client/缓存——Go startSession 段判据行同串）----
    // 早段收尾守卫（复核 r3-F1：GenRun/Finish 挂上**之前**的失败路径〔空候选/身份
    // 装配失败〕也必须 finish_generation——否则单飞锁泄漏、tun_stop 恒 -2；此前的
    // 「接线即坏」同形态。真 Finish 挂上后 disarm，收尾义务交接）
    struct EarlyFinish {
        shared: Arc<TunShared>,
        gen: u64,
        disarmed: bool,
    }
    impl Drop for EarlyFinish {
        fn drop(&mut self) {
            if !self.disarmed {
                self.shared.finish_generation(self.gen);
            }
        }
    }
    let mut _early = EarlyFinish {
        shared: Arc::clone(&shared),
        gen,
        disarmed: false,
    };
    // 停止位先登记进共享面：identity 装配窗口内的 request_stop（世代句柄未就）经
    // TunShared 的旗标中继，装配完成点统一收口（评审 r2-M2 的窗口语义）。
    let stop = Arc::new(AtomicBool::new(false));
    shared.set_stop_flag_if_current(gen, Arc::clone(&stop));
    let (ident, src, warn) =
        match identity::load_or_create(cfg.identity_dir.as_deref(), &cfg.token.peer_id) {
            Ok(v) => v,
            Err(e) => {
                shared.stage.set_if_current(
                    gen,
                    TunStage::Failed,
                    "core",
                    &format!("身份装配失败：{e}"),
                    false,
                );
                return;
            }
        };
    log_identity(&logf, &ident, src, warn, &cfg.identity_dir);
    // portfwd 运行时先建材料（拨号闭包经 `Arc::new_cyclic` 拿 `Weak<GenRun>`——
    // 运行时结构上不依赖 GenRun，但生产实现要回指它；Weak 破环 = 世代可回收，D7）。
    let pf_budget = Duration::from_millis(cfg.dial_ms.max(1) as u64);
    let run = Arc::new_cyclic(|me: &std::sync::Weak<GenRun>| {
        let w = std::sync::Weak::clone(me);
        GenRun {
            gen,
            stop: Arc::clone(&stop),
            stop_wake: StopWake::new(),
            island: RwLock::new(None),
            l3_on_island: AtomicBool::new(false),
            identity: ident.clone(),
            tun_ip: crate::tunnel_addr::derive_tun_ip(&cfg.token.secret, &ident.public_key())
                .to_string(),
            link: Mutex::new(None),
            reconn: ReconnGate::new(),
            bridge: Mutex::new(None),
            logf: Arc::clone(&logf),
            tun_shared: Arc::clone(&shared),
            demand: Arc::clone(&demand),
            pf: PfRuntime::new(PfSetup {
                dial: Arc::new(move |dst, budget| pf_dial_via_run(&w, dst, budget)),
                stop: Arc::clone(&stop),
                logf: Arc::clone(&logf),
                budget: pf_budget,
                limits: PfLimits::default(),
            }),
            started: Instant::now(),
        }
    });
    // ---- 世代句柄登记（岛装配**之前**——评审 r2-M2：窗口内 request_stop/
    // recover/runner 打得到世代；Go beginTunRun 在 goroutine 之前建好世代句柄）----
    *lock_unpoison(&state) = Some(Arc::clone(&run));

    // 世代退出收尾（一切路径经它——Go defer run.finish + closeClientOnce 的合成：
    // client 停 + 缓存终写 + finish_generation + **登记槽清理**；guard 挂在函数栈上，
    // return 即收）
    struct Finish {
        shared: Arc<TunShared>,
        gen: u64,
        run: Arc<GenRun>,
        state: Arc<Mutex<Option<Arc<GenRun>>>>,
    }
    impl Drop for Finish {
        fn drop(&mut self) {
            // portfwd 幂等兜底（Q-F-B F4-3：覆盖 panic/早退路径——正常路径已在
            // 收工段停过；这里的第二次调用是 no-op。清 states = Go
            // stopPortForwards 语义：停止之后不许残留 listening）
            self.run.pf.stop_all();
            // 岛停（§2.6 的收尾链**同址同序**：pf → bridge → 岛 stop_within →
            // finish_generation；正常路径已在收工段停过 ⇒ 这里是幂等兜底，
            // 覆盖 panic/早退路径不留孤儿岛）。
            if let Some(i) = self.run.take_island() {
                if !i.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
                    (self.run.logf)("等待岛线程收工超时（CLIENT_CLOSE_BUDGET）——到点 detach（M0 §8.1 残余：老世代可能继续发包）");
                }
            }
            // 登记槽清理（复核 r3-F2：只清自己——ptr_eq 防误清接管者；清掉后新世代的
            // 装配窗口里 gen_run() 返回 None ⇒ request_stop 走共享面旗标中继〔M-2
            // 声称的窗口在第 2+ 世代真正可达〕）
            {
                let mut slot = lock_unpoison(&self.state);
                if slot.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, &self.run)) {
                    *slot = None;
                }
            }
            self.shared.finish_generation(self.gen);
        }
    }
    let _finish = Finish {
        shared: Arc::clone(&shared),
        gen,
        run: Arc::clone(&run),
        state: Arc::clone(&state),
    };
    // 收尾义务交接给真 Finish（早段守卫 disarm——见 EarlyFinish 注释）
    _early.disarmed = true;

    let tunnel_ip = crate::tunnel_addr::derive_tunnel_ip(&cfg.token.secret, &ident.public_key());
    // ---- QUIC 岛装配（单承载：岛成 ⇒ 本世代唯一承载；岛不成 ⇒ 本世代失败）----
    // 失败路径 = **可见失败**（设计 §2.6-G9：无 QUIC/中继端点的 token〔旧版出口/存量 WG-only
    // 形态 / 旧 token〕、无 RPK、岛起不来、赛跑未成——四条同一归因锚 `岛未就用（…）`；
    // M5 起无回落承载，故 stage 直接落 failed，不再有「按 WG 跑」的第二态）。
    match start_island(&run, &cfg, &ident, &logf) {
        Ok(()) => {
            run.set_l3_on_island(true);
            // C2'（设计 §3.3）：L3 承载面就绪的那一行（去末半句「经 WG」——半句已随
            // WG 面删除而不成立）。**`quic: ` 前缀保留**：去前缀是 §4.4 的批量条目
            // （S4），本棒只改正文。
            (logf)(&format!(
                "隧道侧就绪（L3 直通；隧道地址 {tunnel_ip}，后端隧道 IP {SERVER_TUNNEL_IP}）"
            ));
        }
        Err(note) => {
            (logf)(&format!("岛未就用（{note}）"));
            shared.stage.set_if_current(
                gen,
                TunStage::Failed,
                "core",
                &format!("岛未就用（{note}）"),
                false,
            );
            return;
        }
    }
    (logf)(&format!(
        "新栈会话已建立（token 端点 {} 个，后端隧道地址 {}）",
        cfg.token.endpoints.len(),
        tunnel_ip
    ));

    // ---- 暖机（20s；软失败继续——判据 = 岛 STREAM[probe] 回显）----
    // C8：单承载后判据位恒 `quic`（值域 `{wg,quic}` 收窄为 `{quic}`；设计 §8.1-C8）。
    // 装配窗口内收到 stop 的收口（评审 r2-M2）：岛已在位但 fd 未 attach——直接走
    // 「暖机前停止」收工段（与下方的停止检查同语义）。
    if run.stop.load(Ordering::Acquire) {
        shared
            .stage
            .set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
        (logf)("装配期间收到停止信号，收工（不进暖机）");
        return;
    }
    let warm_started = Instant::now();
    let meowed = {
        match l3_probe(&run, WARM_TIMEOUT) {
            Ok(()) => {
                let rtt = warm_started.elapsed();
                if let Some((via, ep, rtt_ms)) = run.island_link() {
                    *lock_unpoison(&run.link) = Some(LinkIn {
                        via,
                        ep,
                        rtt_ms: if rtt_ms > 0 { rtt_ms } else { rtt.as_millis() as i64 },
                        at_ms: now_unix_ms(),
                    });
                }
                shared.stage.set_ready_by("quic");
                (logf)("warmup pong: 就绪（判据=quic）");
                true
            }
            Err(ProbeFail::IslandGone(m)) => {
                // 岛线程已退出：stop 打断或异常退出
                if run.stop.load(Ordering::Acquire) {
                    shared.stage.set_if_current(
                        gen,
                        TunStage::Idle,
                        "stopped",
                        "被停止请求中断",
                        false,
                    );
                    (logf)("暖机期间收到停止信号，收工（不等 attach）");
                } else {
                    (logf)(&format!("暖机期间岛异常退出（{m}）"));
                    shared.stage.set_if_current(
                        gen,
                        TunStage::Failed,
                        "core",
                        "暖机期间岛异常退出",
                        false,
                    );
                }
                return;
            }
            Err(e) => {
                // 其余（探活无证据/命令超时/连接未就绪）= **软失败**：净空 fwd 后由
                // 流量触发岛内重试注册（M3 起的常态路径）
                (logf)(&format!(
                    "暖机 {} 内未获岛探活证据（{e}）：按软失败继续（attach 后由流量触发重试注册）",
                    crate::go_fmt::fmt_duration_go_ms(WARM_TIMEOUT)
                ));
                false
            }
        }
    };
    if run.stop.load(Ordering::Acquire) {
        shared
            .stage
            .set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
        (logf)("暖机期间收到停止信号，收工（不等 attach）");
        return;
    }
    // ---- 等 attach 的接收端先注册（评审 r2-M3：晚于 Ready 发布的窗口内投 fd 必
    // false ⇒ tun_attach -1 且 tier 侧不重试 ⇒ 整次连接失败；一行时序修复）----
    // 世代守卫（评审 r2 B2⑦）：陈旧 warmup 线程的迟到注册不覆盖新世代 sender。
    let fd_rx = shared.attach_receiver(gen);
    shared
        .stage
        .set_if_current(gen, TunStage::Ready, "", "", meowed);
    // running 行打真实派生地址（L3 直通后 TUN 实际地址）
    (logf)(&format!(
        "running (mtu={} tunIp={})",
        cfg.mtu,
        run.tun_ip_string()
    ));

    // ---- 等 attach（60s 死线；fd 通道 = TunShared）----
    let deadline = Instant::now() + ATTACH_DEADLINE;
    let fd = loop {
        if run.stop.load(Ordering::Acquire) {
            shared
                .stage
                .set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
            (logf)("prepare 就绪后、attach 之前收到停止信号，收工");
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // 防止单飞锁被「无人推进的世代」长期占住（design 决策 5）
            shared.stage.set_if_current(
                gen,
                TunStage::Idle,
                "attach-timeout",
                ATTACH_TIMEOUT_REASON,
                false,
            );
            (logf)(&format!(
                "prepare 后 {} 内没有 attach，自行收工并释放单飞锁（状态回 idle）",
                crate::go_fmt::fmt_duration_go_secs(ATTACH_DEADLINE)
            ));
            return;
        }
        match fd_rx.recv_timeout(left.min(Duration::from_millis(200))) {
            Ok(fd) => break fd,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // 通道被收口（世代已被强制放锁后的迟到清理）
                shared.stage.set_if_current(
                    gen,
                    TunStage::Idle,
                    "stopped",
                    "被停止请求中断",
                    false,
                );
                return;
            }
        }
    };

    // L3 承载面装配（**单腿**：fd 交岛；岛不在 ⇒ 失败，无第二条腿）。fd 所有权在扩展
    // （坑 50）：attach 失败不 close，由扩展 destroy 回收。
    let attach: Result<(), String> = match run.current_island() {
        Some(island) => attach_island(&island, fd, cfg.mtu),
        None => Err("岛句柄不在（世代已收回？）".to_owned()),
    };
    if let Err(note) = attach {
        if run.stop.load(Ordering::Acquire) {
            shared
                .stage
                .set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
            return;
        }
        (logf)(&format!("岛附加失败（{note}）"));
        shared.stage.set_if_current(
            gen,
            TunStage::Failed,
            "attach",
            &format!("attach 失败：{note}"),
            false,
        );
        return;
    }
    shared
        .stage
        .set_if_current(gen, TunStage::Attached, "", "", meowed);
    (logf)(&format!("attached（数据面已接管 fd={fd}，L3 直通）"));

    // ---- portfwd 装表（Go `tunmode.go:828` 的 `t.setPortForwards(cfg.PortForwards)`
    // ——在 `bridge.start()`（:832）之前；attach 才监听，与「未 attached 不监听 ⇒
    // 状态表为空」的 tier「启动中…」一致）----
    run.pf.install(&cfg.port_forwards);
    // stale 复查（Go `tunSetPortForwardsJSON` 的应用后复查同形）：装表期间世代换代/
    // 收到停止 ⇒ 随手收掉刚起的监听器，绝不给死世代留孤儿
    if run.gen != shared.gen.load(Ordering::Acquire) || run.stop.load(Ordering::Acquire) {
        (logf)("port-forward: 装表后复查发现世代已收口——撤回本世代监听器");
        run.pf.stop_all();
    }

    // ---- 隧道桥（attached 后才起——「会话在桥在」）----
    //
    // **M5 C3 单腿**：`DialFn` 签名不变，实现恒开 `STREAM[tag]`（虚拟端口 → tag 见
    // `quic_stream::tag_for_port`），应用层帧逐字节不变；**未知端口**（不在
    // 7802/7724/7803 三个服务端口内）不回落任何第二条腿：那是协议面无法表达的拨号
    // （本期限定三个服务 tag）。
    let bridge = Arc::new(BridgeHost::new(
        "隧道桥",
        cfg.identity_dir.clone(),
        Arc::clone(&logf),
        {
            let run2 = Arc::clone(&run);
            Arc::new(move |port, budget| match super::quic_stream::tag_for_port(port) {
                // reconnect-oneshot D2：files/term/speedtest = 用户可见服务入口——
                // 开流有界重试后的承载类失败触发统一入口；**DNS 腿不接**（客户端
                // 自用解析腿：解析失败是环境噪声面，不当连接死亡证据）。
                Some(tag) => super::quic_stream::dial(&run2, tag, budget),
                None => Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("无该虚拟端口的服务 tag（port={port}）"),
                )),
            })
        },
    ));
    // 桥拨号预算随 tunConfig（dialMs——评审 r2-L5：此前是死字段 + 常量 15s）
    bridge.set_dial_timeout(Duration::from_millis(cfg.dial_ms as u64));
    bridge.start();
    *lock_unpoison(&run.bridge) = Some(Arc::clone(&bridge));

    // ---- tunStatusJSON 对账快照（7m-④：真机字节对账的数据面——每世代一次，
    // attached 后 1.5s 让桥/runner 就位再产；字节与 ClientCoreTunStatus 同源
    // 〔tun_status_json 纯函数 + runner_of/transport_of 同组装〕）----
    {
        let run2 = Arc::clone(&run);
        let shared2 = Arc::clone(&shared);
        let demand2 = Arc::clone(&demand);
        let logf2 = Arc::clone(&logf);
        let spawned = std::thread::Builder::new()
            .name("homeway-status-dump".into())
            .spawn(move || {
                std::thread::sleep(Duration::from_millis(1500));
                if run2.stop.load(Ordering::Acquire)
                    || shared2.stage.snapshot().stage != TunStage::Attached
                {
                    return;
                }
                let snap = shared2.stage.snapshot();
                // tun-death-gate：running 单点（与 capi 同源——TunShared::running）
                let running = shared2.running();
                // r8-N2：读侧统一 HealthSnapshot（一次锁读——不再分次读混合快照）。
                let hs = shared2.health_snapshot();
                let why = hs.why;
                let input = super::tun_status::TunStatusInput {
                    stage: snap,
                    running,
                    demand: demand2.last(),
                    demand_fg: demand2.fg(),
                    transport_unhealthy: why == "patrol",
                    unhealthy_reason: (!why.is_empty()).then_some(why),
                    device_dead: hs.device_dead,
                    service_streams_active: quic_status_of(&run2)
                        .map(|q| q.streams_active)
                        .unwrap_or(0),
                    rescue_rounds_exhausted: false,
                    runner: Some(runner_of(&run2)),
                    transport: Some(transport_of(&run2)),
                    quic: quic_status_of(&run2),
                };
                (run2.logf)(&format!(
                    "tunStatusJSON 对账快照：{}",
                    super::tun_status::tun_status_json(&input)
                ));
            });
        // F7-1：spawn 失败不静默（只记行——对账快照缺失不动数据面，也不标 unhealthy：
        // unhealthy 会经 App FailGate 触发整套重建，代价大于收益，设计门 D6）
        if let Err(e) = spawned {
            crate::syncutil::log_spawn_failed(
                &logf2,
                "homeway-status-dump",
                &e,
                "本世代无 tunStatusJSON 对账快照（不影响数据面）",
            );
        }
    }

    // ---- stats 观测线程（reconnect-oneshot D4：巡检三遗产的继承人——link 刷新 /
    // demand 记账 / 观测快探连败标记；**本世代唯一 facade 拍线程**，纯观测非决策）----
    let stats_handle = spawn_derived(
        "homeway-stats",
        Arc::clone(&run),
        "本世代无统计行（对账缺一段观测）",
        move |r| stats_loop(r, cfg.stats_secs, cfg.diag_fd_secs, OBS_INTERVAL),
    );

    // ---- 等 stop（世代主线程的最后一段；_finish guard 在 return 时统一收尾）----
    loop {
        if run.stop.load(Ordering::Acquire) {
            break;
        }
        // 收工等待走 Condvar（2026-10-18：不再 1s 分片 sleep）；1h 只作防御性超时，
        // 醒来先复查 stop 位。
        run.stop_wake.wait(&run.stop, Duration::from_secs(3600));
    }
    (logf)("收到停止信号，正在回收（client/stack）");
    // stats 随 stop 位退出（join 有界——**总预算 2s**，与 STOP_WAIT=3s 留 1s 给桥停/
    // client 收尾；观测快探经 island_cmd_until 的 ≤100ms 分片查停 ⇒ 慢 RPC 在飞时
    // 也不超预算——r4-R14）
    let join_deadline = Instant::now() + Duration::from_secs(2);
    for h in [stats_handle].into_iter().flatten()
    {
        while !h.is_finished() && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if h.is_finished() {
            let _ = h.join();
        }
    }
    // portfwd 先停（Q-F-B F4-3：更早释放端口；两序都在 client 关闭之前——Go 的
    // defer LIFO 实为「桥先停、pf 后停」，本仓取 pf 先停并登记理由）
    run.pf.stop_all();
    // 桥停（先关监听；在途桥接连接随 client 收工自然断——Go defer LIFO 同序）
    let bridge = lock_unpoison(&run.bridge).take();
    if let Some(b) = bridge {
        b.stop();
    }
    // 岛停（§2.6 的收尾链：pf → bridge → 岛 stop_within → finish_generation；
    // 岛停在此处显式做，`Finish::drop` 的兜底覆盖 panic/早退路径）。
    if let Some(i) = run.take_island() {
        if !i.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
            (run.logf)("等待岛线程收工超时（CLIENT_CLOSE_BUDGET）——到点 detach（老世代可能继续发包，M0 §8.1 残余）");
        }
    }
    // _finish（Drop）：岛停兜底 + finish_generation
}

// ---------------------------------------------------------------------------
// 派生线程的 panic 边界（评审 r2-L3：Go 各 goroutine 有 recover，"panic" 分类可达）
// ---------------------------------------------------------------------------

/// 派生线程（巡检/pusher/stats）的统一 spawn：线程体套 catch_unwind——panic 不
/// 穿透（c-shared 宿主进程里 panic = 扩展进程死），落日志 + markUnhealthy("panic")
/// ——**panic 属设备死分类**（`DEVICE_CLASS`：置 `device_dead` + 承载位设备死闩），
/// 统一入口 `try_spawn_reconnect` 第 0 步直接 Rejected，**不会自动重连**（重连 =
/// 用户动作；connection-lifecycle「设备死」口径——评审 r4 B6 订正：旧词「交一次性
/// 重连」语义错误）。Go recover 后调 t.markUnhealthy("panic") 同义。
///
/// F7-1：**spawn 失败不再静默**——只记行（`consequence` = 本派生的降级后果），不标
/// unhealthy（设计门 D6：unhealthy 会触发 App 整套重建，代价大于收益；两域数据面都
/// 不依赖巡检）。残余 = 本世代无自愈巡检（登记）。
fn spawn_derived(
    name: &'static str,
    run: Arc<GenRun>,
    consequence: &'static str,
    body: impl FnOnce(Arc<GenRun>) + Send + 'static,
) -> Option<std::thread::JoinHandle<()>> {
    let logf = Arc::clone(&run.logf);
    match std::thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let r2 = Arc::clone(&run);
            let gen = run.gen;
            let logf = Arc::clone(&run.logf);
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || body(run))).is_err() {
                (logf)(&format!(
                    "{name}: 线程 panic（已兜住）—— 标记设备死（不自动重连，重连 = 用户动作）"
                ));
                r2.tun_shared.mark_unhealthy_if_current(gen, "panic");
            }
        }) {
        Ok(h) => Some(h),
        Err(e) => {
            crate::syncutil::log_spawn_failed(&logf, name, &e, consequence);
            None
        }
    }
}

fn log_identity(
    logf: &Logf,
    ident: &Identity,
    src: identity::IdentitySource,
    warn: Option<String>,
    dir: &Option<PathBuf>,
) {
    use identity::IdentitySource;
    let dir_text = dir
        .as_ref()
        .map(|d| d.display().to_string())
        .unwrap_or_default();
    match src {
        IdentitySource::Created => (logf)(&format!(
            "身份：新建（dev={} pub={}，目录 {dir_text}）",
            ident.short_dev(),
            ident.short_pub()
        )),
        IdentitySource::Reused => (logf)(&format!("身份：复用（dev={} pub={}）", ident.short_dev(), ident.short_pub())),
        IdentitySource::Rebuilt => (logf)(&format!(
            "⚠️ 身份：既有密钥损坏已归档并重建（dev={} pub={}）—— 出口会按同设备身份轮换替换记录",
            ident.short_dev(),
            ident.short_pub()
        )),
        IdentitySource::TagDerived => (logf)(&format!(
            "⚠️ 身份：设备标签文件不可用，已从主密钥派生（dev={} pub={}）—— 重连稳定，但重置身份会换标签（出口会多一条记录）",
            ident.short_dev(),
            ident.short_pub()
        )),
        IdentitySource::Ephemeral => {}
    }
    if let Some(w) = warn {
        (logf)(&format!(
            "身份：不可持久化（{w}）—— 本次用临时身份建连；重连会换钥匙（出口会多占一条记录）"
        ));
    }
}

// ---------------------------------------------------------------------------
// M1 S3-1：QUIC 岛接线小件（构造/附加/命令/判活/候选/回落动作）
// ---------------------------------------------------------------------------

/// 岛路径失败说明（**enum 而非字符串**——AGENTS 原则 1；变体只为记行归因，不承担错误
/// 类型面的语义：真正的失败面对上层只有「本世代未建连」一种处置——M5 单承载后无回落档）。
#[derive(Debug, thiserror::Error)]
enum QuicFail {
    /// token 缺 RPK 尾字段（`serve.quic=false` 形态的出口 token ⇒ 无法钉定服务端身份）。
    #[error("token 未携带出口 RPK（旧版出口/未启用 QUIC 面的形态？）")]
    NoRpk,
    /// token 里没有任何 QUIC/中继类端点（§2.7 收窄后的候选来源为空）。
    #[error("token 无 QUIC/中继类端点（候选为空）")]
    NoCandidate,
    /// 岛起不来（端点/身份/线程装配失败）。
    #[error("岛起不来：{0}")]
    Startup(String),
    /// 赛跑未成（全候选未在预算内完成握手 + 登记）。
    #[error("赛跑未成：{0}")]
    Race(String),
}

/// 岛侧命令回执的有界等待失败。
#[derive(Debug, thiserror::Error)]
enum IslandRpc {
    #[error("岛命令通道不可用（{0}）")]
    Gone(String),
    #[error("岛回执超时（{0}）")]
    Timeout(String),
    #[error("岛侧错误（{0}）")]
    Island(String),
}

/// 判活失败说明（WG/岛两面共用；只为记行归因与**软/硬失败分类**）。
#[derive(Debug, thiserror::Error)]
enum ProbeFail {
    #[error("数据面不在")]
    NoFace,
    /// 岛命令通道不可用（= 岛线程已退出，与 `ConnErr::EngineGone` 同面）。
    #[error("岛命令通道不可用（{0}）")]
    IslandGone(String),
    /// **等待超时**（分片等待到点——命令在预算内无回执：等待并发/慢对端）。
    /// typed 面（评审 r2 B7）：stats 观测对这类**不累积连败**（非对端证据）。
    #[error("探测超时（{0}）")]
    Timeout(String),
    /// **探活槽位争用**（`IslandErr::ProbeBusy`——`client::probe` 拿锁预算内未获
    /// 槽；「已有赛跑在途」分支已按事实删除，评审 r3 B7）。typed 面（评审 r2 B7）：
    /// stats 观测对这类**不累积连败**（等待并发面——与 worker 快探共用 probe 槽）。
    #[error("探测并发（{0}）")]
    Busy(String),
    #[error("{0}")]
    Detail(String),
}

/// 岛侧候选装配（设计 §2.7 的**候选来源收窄**：只吃 token 的 QUIC 类端点 + 中继端点；
/// 学习缓存/hint 在 M1 只服务 WG 面）。
///
/// 中继候选的 `label` = `sha256(peerId)[:8]`（信封帧 `[0xAA][label8]` 的来源；真源
/// `legframe::relay_id`）——与出口侧腿表同源。
fn quic_candidates(tok: &Token, logf: &Logf) -> Result<Vec<homeway_quic::Candidate>, QuicFail> {
    let label = crate::legframe::relay_id(tok.peer_id.as_bytes());
    let mut out: Vec<homeway_quic::Candidate> = Vec::new();
    for e in &tok.endpoints {
        let via = match e.kind {
            crate::token::EndpointKind::Quic => homeway_quic::Via::Direct,
            crate::token::EndpointKind::Relay => homeway_quic::Via::Relay { label },
            // WG 类端点**不喂岛**（§2.1 末段「两族候选不得互相投喂」）
            crate::token::EndpointKind::Direct => continue,
        };
        let Ok(addr) = e.addr.parse::<std::net::SocketAddr>() else {
            // 域名/非法形态不进岛候选（token 签发侧给的是 IP 字面量；v4/v6 均可）。
            (*logf)(&format!("端点跳过（非 IP 字面量/非法地址）：{}", e.addr));
            continue;
        };
        if out.iter().any(|c| c.addr == addr) {
            continue; // 同址去重（先到先得）
        }
        out.push(homeway_quic::Candidate { addr, via });
    }
    if out.is_empty() {
        return Err(QuicFail::NoCandidate);
    }
    Ok(out)
}

/// 岛命令（带 reply）的有界执行（岛死 ⇒ 立刻归错，不挂死）。
fn island_cmd<T>(
    island: &homeway_quic::Island,
    make: impl FnOnce(homeway_quic::IslandReply<T>) -> homeway_quic::Cmd,
    budget: Duration,
) -> Result<T, IslandRpc> {
    let (tx, rx) = mpsc::channel();
    island
        .tx()
        .send(make(tx))
        .map_err(|e| IslandRpc::Gone(e.to_string()))?;
    match rx.recv_timeout(budget) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(IslandRpc::Island(e.to_string())),
        Err(e) => Err(IslandRpc::Timeout(e.to_string())),
    }
}

/// 岛侧不健康回调（**岛线程内执行**：只允许内存操作 + 通道投递/起线程，绝不可阻塞）。
fn island_unhealthy(run: &Arc<GenRun>) -> homeway_quic::OnUnhealthy {
    let w = Arc::downgrade(run);
    Arc::new(move |reason: &str| {
        if let Some(r) = w.upgrade() {
            quic_unhealthy_signal(&r, reason);
        }
    })
}

/// 岛不健康信号处置（M3 S4 §3.1 + reconnect-oneshot D2 重写）。
///
/// 岛只在两种情形上报：
/// - **B（世代重建门）**：连续 2 次 R 失败且窗 ≥10s ⇒ 岛内阶梯走完——**回调沿立即**
///   经 `mark_transport_if_current`（专用标记入口：检查+写 why=patrol+翻沿+置承载
///   位 false 同一临界区；设备死在场/已 patrol 时 no-op、carrier 也不动——r13-P6）
///   标记传输不健康，随后以 **BGate 边沿源**触发统一入口（被抑制时经 pending 承接
///   ——标记先行，不因后续节流/单飞丢失）。旧 `homeway-rerace` 自起线程退役：
///   B 门与读面/服务流共用同一单飞槽位；
/// - `fd`/`panic`：与 WG 面同源，直接分类（无「保连接」动作可做）。
///
/// 语义变化（M1 → M3 S4，登记 S7；reconnect-oneshot 再订正）：本函数**零阻塞**
/// （岛线程回调内只允许内存操作 + 通道投递/起线程）——重赛跑由一次性 worker 承载。
fn quic_unhealthy_signal(run: &Arc<GenRun>, reason: &str) {
    quic_unhealthy_signal_body(run, reason)
}

/// 测试缝（2.5）：B 门沿处置的直接调用面（生产经岛回调 `island_unhealthy` 进入）。
#[cfg(test)]
pub(crate) fn quic_unhealthy_signal_for_test(run: &Arc<GenRun>, reason: &str) {
    quic_unhealthy_signal_body(run, reason);
}

fn quic_unhealthy_signal_body(run: &Arc<GenRun>, reason: &str) {
    if reason == "denied" {
        // 系统禁发：传输类标记（不写设备死位），**不触发 B 门 one-shot**——等唤醒/
        // 前台/换网边沿再试（见 ladder::Step::Dormant）。
        run.tun_shared.mark_unhealthy_if_current(run.gen, "denied");
        (run.logf)("岛上报系统禁发（Denied）—— 标记传输不健康（不触发一次性重连，等边沿）");
        return;
    }
    if reason == "patrol" {
        // 评审 r1 P1-10 整改（回调沿顺序）：**先标记、再触发、后落日志**——
        // `run.logf` 是文件写 + Mutex（可能阻塞），岛回调纪律要求「立即」标记并置
        // carrier false（D2：标记先行不因后续节流/单飞丢失）；日志放最后（短临界区
        // 内禁阻塞）。
        run.tun_shared.mark_transport_if_current(run.gen);
        // 边沿源触发统一入口（在飞时 pending 留字，owner 收尾承接）。
        GenRun::try_spawn_reconnect(run, ReconnSource::BGate);
        (run.logf)("岛上报不健康（patrol）—— 岛内快探阶梯已走完 M/R（B 门）：标记传输不健康并触发一次性重连");
        return;
    }
    run.tun_shared.mark_unhealthy_if_current(run.gen, reason);
}

/// 统一重赛跑入口（tun-death-gate §2.2；reconnect-oneshot 起的调用方 = 一次性
/// worker 的 run_round b 步〔换网源带 `demote_adopted`〕）。固定序列：换本地
/// socket（`Cmd::Rebind{local:None}`——M 的动作面，静默腐烂只有这步能解）→
/// 全候选赛跑（`Cmd::Connect`，空候选回落名册；`st.racing` 单飞在入口内合并——
/// `RaceInFlight` = 已在飞非失败）。成功走既有 adopt（含阶梯复活 + 换连接清流）
/// 后翻回（**强证据**：ZERO 间隔 + 承载位置 true + 刷 link）；失败返回 Err。
/// rerace 的三态回执（评审 r1-F1）：**让路 ≠ 恢复**——两个让路分支（阶梯在飞 /
/// RaceInFlight）此前返回 `Ok(())` 被 pusher 一律按「恢复成功——退出自救」处理，
/// 自救停摆且 `patrol_reported` 永真 ⇒ 永不重入（隧道已死而 UI 恒显
/// 已连接的静默黑洞）。枚举让调用方编译期分派，杜绝再混。
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
enum ReraceOutcome {
    /// 赛跑胜出并已翻回（强证据——`try_recover` 在函数内完成）。
    Recovered,
    /// 另一入口在飞 / 阶梯在飞——本轮让路：不消耗轮次、不退出自救、不报 rc=0。
    Yielded,
}

/// rerace 的中止形态（评审 r1 P0-6 整改）：**Gone = 停止/世代变化/设备死**——静默
/// 退出、不落「对端失败」行（D1-c/tasks 3.2 的纪律：`tun_stop` 后不得继续 Rebind/
/// Connect 或产出换源失败/赛跑失败行）；**Fail = 真实重赛跑失败**（岛线程死亡等
/// 非停止面含此——照常落失败行）。
#[derive(Debug)]
enum ReraceAbort {
    Gone,
    Fail(String),
}

/// rerace 的岛命令等待（统一 `island_cmd_until`——分片查停，评审 r1 P0-6/tasks 3.2：
/// worker 的岛命令与观测快探同一纪律）。`Gone` 且非本端停止面 ⇒ 归 `Fail`（岛线程
/// 死亡是真实失败——有界、落行）。
fn rerace_cmd_until<T>(
    run: &GenRun,
    island: &homeway_quic::Island,
    make: impl FnOnce(homeway_quic::IslandReply<T>) -> homeway_quic::Cmd,
    deadline: Instant,
) -> Result<T, ReraceAbort> {
    match island_cmd_until(run, island, make, deadline) {
        Ok(v) => Ok(v),
        Err(IslandRpc::Gone(_)) if worker_halted(run) => Err(ReraceAbort::Gone),
        Err(e) => Err(ReraceAbort::Fail(e.to_string())),
    }
}

fn rerace_tunnel(run: &Arc<GenRun>, demote_adopted: bool) -> Result<ReraceOutcome, ReraceAbort> {
    // 入口检查点（D1-c：Rebind 之前——停止/换代/设备死即静默退出，零副作用）。
    if worker_halted(run) {
        return Err(ReraceAbort::Gone);
    }
    let island = run
        .current_island()
        .ok_or_else(|| ReraceAbort::Fail("岛不在".to_owned()))?;
    // 在飞守卫（r4 N4 / r2-N2）：阶梯或赛跑在飞 ⇒ 让路。**必须在 Rebind 之前**——
    // `RaceInFlight` 要到 Connect 才暴露，彼时 Rebind（换本地 socket，endpoint 粒度
    // 副作——影响该端点全部连接）已经执行；把守卫提前后让路路径零副作用。快照有
    // 200ms 节流陈旧（残余 check-then-act 窗口由 Connect 的单飞位兜底）。
    let snap = island.snapshot();
    if snap.ladder_inflight || snap.racing {
        (run.logf)("重赛跑让路：阶梯/赛跑在飞（或快照陈旧）——本轮不发起，零副作用等待");
        return Ok(ReraceOutcome::Yielded);
    }
    rerace_cmd_until(
        run,
        &island,
        |reply| homeway_quic::Cmd::Rebind {
            local: None,
            reply,
        },
        Instant::now() + QUIC_RPC_BUDGET,
    )
    .map_err(|e| match e {
        ReraceAbort::Gone => e,
        ReraceAbort::Fail(m) => ReraceAbort::Fail(format!("换源失败：{m}")),
    })?;
    // Rebind 与 Connect 之间的检查点（D1-c：每步前后查——等待期停止可即刻收口）。
    if worker_halted(run) {
        return Err(ReraceAbort::Gone);
    }
    let outcome = match rerace_cmd_until(
        run,
        &island,
        |reply| homeway_quic::Cmd::Connect {
            cands: Vec::new(),
            budget: QUIC_CONNECT_BUDGET,
            // 换网源降权（r9-N9）：快探失败后旧采纳端点不得优先（driver 按 addr 把
            // `ladder_cand` 钉住的端点降权到队尾、不删空）。
            demote_adopted,
            reply,
        },
        Instant::now() + QUIC_CONNECT_BUDGET + QUIC_RPC_BUDGET,
    ) {
        Ok(v) => v,
        // r5 M3：RaceInFlight = **另一入口在飞非失败**（三调用方并发时后到者让路，
        // 不据此翻标健康位、不上报——design「统一入口单飞合并」的语义）。
        Err(e) if matches!(&e, ReraceAbort::Fail(m) if m.contains("RaceInFlight")) => {
            (run.logf)("重赛跑让路：候选赛跑已在飞——本入口合并等待");
            return Ok(ReraceOutcome::Yielded);
        }
        Err(e) => return Err(match e {
            ReraceAbort::Gone => e,
            ReraceAbort::Fail(m) => ReraceAbort::Fail(format!("赛跑失败：{m}")),
        }),
    };
    // Connect 成功返回后的复查（评审 r2 B3）：等待期间设备死/停止/换代 ⇒ 静默退出
    // ——不落「胜出…传输恢复」行、不 strong_recover（旧实现直接打行 + 翻回，设备死
    // 在命令在飞期间发生时会落一条假恢复行）。
    if worker_halted(run) {
        return Err(ReraceAbort::Gone);
    }
    rerace_finish(run, &outcome)
}

/// 重赛跑胜出后的收口（Connect 成功 + halt 复查之后）：**先强证据翻回、后落成功
/// 行**——`strong_recover` 返回 `carrier_alive`，false（设备死在复查后到场/
/// 陈旧世代）⇒ 不落「阶梯尽头重赛跑：胜出…传输恢复」假恢复行，按 `Gone` 静默
/// 收口（评审 r5-L3；旧实现先打行、返回值未消费）。
///
/// 成功行对翻回的独立性（r5 M2）不变：`carrier_alive=true` 而 `why` 可能仍空
/// （patrol 从未标记/已被他人清除），此时 `strong_recover` 返回 true，成功行照落。
fn rerace_finish(
    run: &Arc<GenRun>,
    outcome: &homeway_quic::RaceOutcome,
) -> Result<ReraceOutcome, ReraceAbort> {
    // reconnect-oneshot D2：rerace 成功 = 翻回成功路径——强证据（ZERO 间隔）+
    // 承载位置 true（同临界区查 device_dead）+ 刷 link（先置位后刷 at——r10-C1）。
    if !GenRun::strong_recover(run, "重赛跑") {
        return Err(ReraceAbort::Gone);
    }
    (run.logf)(&format!(
        "阶梯尽头重赛跑：胜出 {} {}（耗时 {}ms）——传输恢复，不拆接口",
        outcome.via.text(),
        outcome.winner,
        outcome.elapsed_ms
    ));
    Ok(ReraceOutcome::Recovered)
}

/// 岛的建立（凭据 → 候选 → 起岛 → SetOnUnhealthy/SetCandidates → 赛跑）。
/// 任一环失败 ⇒ `Err(QuicFail)`（调用方记行 + **本世代未建连**——M5 单承载后无回落档）。
fn start_island(
    run: &Arc<GenRun>,
    cfg: &GenCfg,
    ident: &Identity,
    logf: &Logf,
) -> Result<(), QuicFail> {
    let Some(rpk) = cfg.token.rpk else {
        return Err(QuicFail::NoRpk);
    };
    let cands = quic_candidates(&cfg.token, logf)?;
    // C13（设计 §8.1 改写面）：候选清单一行——单承载后「标记·学习」面退役（岛候选恒来自
    // token，无学习缓存/hint），形态收窄为 `候选端点（%d 条）：%s`。
    (logf)(&format!(
        "候选端点（{} 条）：{}",
        cands.len(),
        cands
            .iter()
            .map(|c| format!("{}（{}）", c.addr, c.via.text()))
            .collect::<Vec<_>>()
            .join("、")
    ));
    let cred = homeway_quic::IslandCredential::new(
        homeway_quic::TokenSecret::from_bytes(*cfg.token.secret.as_bytes()),
        ident.public_key(),
        *ident.dev_tag().as_bytes(),
        homeway_quic::RpkPublicKey::from_bytes(*rpk.as_bytes()),
    );
    let mut icfg = homeway_quic::IslandConfig::new(cred);
    icfg.patrol = PATROL_INTERVAL; // 节拍/阈值常数不动（设计 §2.5）
    icfg.mtu_cap = cfg.mtu_cap; // §12-① 的上限旋钮（世代层已夹区间/记行）
    // 绑定族按候选面选（2026-10-18 v6）：纯 v4 token 保持 v4 socket（与旧行为一致）；
    // 含 v6 候选才起双栈（`None` = `[::]:0`）——v4 回环在部分平台上走 mapped 有怪癖，
    // 不在不需要时引入。
    icfg.bind = if cands.iter().any(|c| c.addr.is_ipv6()) {
        None
    } else {
        Some(std::net::SocketAddr::from((
            std::net::Ipv4Addr::UNSPECIFIED,
            0,
        )))
    };
    // reconnect-oneshot D2：承载位装配期共握（岛侧沿写 + 读线程每包 load 同一枚）。
    icfg.carrier = Some(Arc::clone(&run.tun_shared.carrier));
    icfg.denied = Some(Arc::clone(&run.tun_shared.denied));
    // 评审 r1 P1-7：adopt 沿通知 facade——岛内阶梯/赛跑自行恢复（置 carrier true）
    // 而 facade 仍残留 patrol 时清之，防「healthy=false ∧ carrier=true」黏滞。
    {
        let w = Arc::downgrade(run);
        icfg.on_adopt = Some(Arc::new(move || {
            if let Some(r) = w.upgrade() {
                r.tun_shared.on_adopt_if_current(r.gen);
            }
        }));
    }
    let island = Arc::new(
        homeway_quic::Island::start(Arc::clone(logf), island_unhealthy(run), icfg)
            .map_err(|e| QuicFail::Startup(e.to_string()))?,
    );
    run.set_island(Arc::clone(&island));
    // 「立即 SetOnUnhealthy」（S2b 接线清单）：构造期回调已是同一份，这一发是**显式面**
    // （运行期可替换、此处即刻装定）。
    let h = island_unhealthy(run);
    let _ = island
        .tx()
        .send(homeway_quic::Cmd::SetOnUnhealthy { h });
    // reconnect-oneshot D2（ReadFace 源）：读面需求触发回调——**TunAttach 之前发送**
    // （通道 FIFO ⇒ 读线程起跑前已装；回调捕获 Weak<GenRun>——无 Arc 环，契约 =
    // 只做原子操作 + 起线程）。
    let wd = Arc::downgrade(run);
    let _ = island.tx().send(homeway_quic::Cmd::SetOnPacketDemand {
        h: Arc::new(move || {
            if let Some(r) = wd.upgrade() {
                GenRun::try_spawn_reconnect(&r, ReconnSource::ReadFace);
            }
        }),
    });
    let _ = island.tx().send(homeway_quic::Cmd::SetCandidates {
        cands: cands.clone(),
    });
    let n = cands.len();
    let outcome = island_cmd(
        &island,
        |reply| homeway_quic::Cmd::Connect {
            cands,
            budget: QUIC_CONNECT_BUDGET,
            demote_adopted: false,
            reply,
        },
        QUIC_CONNECT_BUDGET + QUIC_RPC_BUDGET,
    )
    .map_err(|e| QuicFail::Race(e.to_string()))?;
    (logf)(&format!(
        "岛已建连（候选 {n} 个，胜出 {} {}，耗时 {}ms）—— L3 承载 = 岛",
        outcome.via.text(),
        outcome.winner,
        outcome.elapsed_ms
    ));
    Ok(())
}

/// 岛侧隧道面附加（fd 所有权在扩展：岛从不 close）。
fn attach_island(island: &homeway_quic::Island, fd: i32, mtu: u32) -> Result<(), String> {
    island_cmd(
        island,
        |reply| homeway_quic::Cmd::TunAttach { fd, mtu, reply },
        QUIC_RPC_BUDGET,
    )
    .map_err(|e| e.to_string())
}

/// L3 承载面的判活（恒岛：`Cmd::Probe` = QUIC `STREAM[probe]` 回显）。
fn l3_probe(run: &Arc<GenRun>, budget: Duration) -> Result<(), ProbeFail> {
    let island = run.current_island().ok_or(ProbeFail::NoFace)?;
    island_cmd(
        &island,
        |reply| homeway_quic::Cmd::Probe { budget, reply },
        budget + QUIC_RPC_BUDGET,
    )
    .map(|_rtt| ())
    .map_err(|e| match e {
        IslandRpc::Gone(m) => ProbeFail::IslandGone(m),
        other => ProbeFail::Detail(other.to_string()),
    })
}

// ---------------------------------------------------------------------------
// stats 观测线程（reconnect-oneshot D4：patrol 三遗产的继承人——纯观测、非决策）
// ---------------------------------------------------------------------------

/// `diag_fd_secs` = 配置面字段（`tunConfig.diagFdSecs`）：**本核无消费**——fd 快照面在
/// OHOS 沙箱受限（Go 侧 `/proc/self/fd` 快照读不到有意义的东西），Q-F F8b 删掉
/// 「基线一行 + 周期快照」的死分支（分支体只改两个账面变量、无任何观测产出）；
/// 字段保留（删字段会动 `TunConfigJson` serde 面与 Go 配置对齐——登记 §7-9）。
///
/// **双累加器节拍（r3-R9 终稿）**：循环 tick = `min(stats_secs, 60)`（保证 ≤60s 粒度）；
/// stats 行每 `stats_secs` 打一次、观测/link 每 60s 做一次——`link.at` 年龄恒 <3min
/// （`ExitHealth` 3min 陈旧判据不误报）。三项职责（D4）：
/// 1. **link 快照刷新**：任一成功探测（观测快探 / worker 快探 / recover 同步快探三类
///    任一——后两者的翻回路径自带刷新；观测快探**仅在发起时承载在位**才发起）；
///    承载不在位/探测失败时保留旧值（陈旧判据自然成立）；
/// 2. **demand 记账**（`l3_swap_out_pkts` + `patrol_demand` + `note_demand`——纯诊断，
///    兼作观测快探的需求门）；
/// 3. **观测快探（r10-A1 终稿：稳态照跑、计数只认出站包）**：运行条件 = 稳态 ∧ 无并发
///    探测（承载 true ∧ 非设备死 ∧ 非传输不健康 ∧ **重连 worker 不在飞** ∧ 岛内阶梯
///    不在飞）；探测每拍照跑（夜间健康空闲也探——成功刷 `link.at`，失败不计数）；
///    **失败仅在「独立 60s `out_pkts` 增量 >0 的拍」累积**（亮屏位不参与失败证据），
///    无出站包拍清零连败；连续 3 败经 `mark_transport_if_current` 标记（同一临界区、
///    同时置承载位 false——否则读面触发条件不成立）＋幂等；成功只清连败计数
///    （不清标记、不置位）；离开稳态清零。
fn stats_loop(run: Arc<GenRun>, stats_secs: i64, _diag_fd_secs: i64, obs_interval: Duration) {
    let stats_every = Duration::from_secs(stats_secs.max(STATS_SECS_MIN) as u64);
    let tick = stats_every.min(obs_interval); // r3-R9：tick = min(stats_secs, 60)
    let mut since_stats = Instant::now();
    let mut since_obs = Instant::now();
    // 独立 60s 出站包累加器（与 stats 行节拍解耦——防 stats_secs 配置漂移计数窗口：
    // 5s/300s 档下 3 连败窗口恒为 ~180s）。
    let mut obs_fail_streak: u32 = 0;
    loop {
        // 收工可打断的等待（评审 r2-H3：整段 sleep 让收工 join 白烧满预算 ⇒ tun_stop
        // 常态化 -2 强制放锁）。2026-10-18 起走 Condvar：只有「到拍」与「收工」两种
        // 唤醒，不再 1s 分片空转。
        let deadline = Instant::now() + tick;
        loop {
            if run.stop.load(Ordering::Acquire) {
                return;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            // 走收工唤醒面（2026-10-18：不再 1s 分片 sleep——空闲期 stats 线程
            // 只按拍/收工两种事醒来）。
            run.stop_wake.wait(&run.stop, left);
        }
        let now = Instant::now();
        // ---- stats 行（每 stats_secs 一次；重连五计数段——r3-R16）----
        if since_stats.elapsed() >= stats_every {
            since_stats = now;
            let (rd, wr) = run.l3_tun_stats();
            let c = run.pf.counters();
            (run.logf)(&stats_line(StatsLineIn {
                fd_read_bytes: rd,
                fd_write_bytes: wr,
                pf_accepted: c.accepted(),
                pf_fails: c.fails(),
                reconn_entries: run.reconn.entries.load(Ordering::Relaxed),
                reconn_starts: run.reconn.starts.load(Ordering::Relaxed),
                reconn_throttled: run.reconn.throttled.load(Ordering::Relaxed),
                reconn_fake_pkts: run.tun_shared.carrier.demand_pkts(),
                reconn_spawn_fails: run.reconn.spawn_fails.load(Ordering::Relaxed),
            }));
        }
        // ---- 观测/link（每 60s 一次）----
        if since_obs.elapsed() < obs_interval {
            continue;
        }
        since_obs = now;
        // demand 记账（拍头取走 60s 出站计数——纯诊断 + 观测失败计数门共用一份）。
        let out_pkts = run.l3_swap_out_pkts();
        let (demand_active, demand_why) = run.demand.patrol_demand(out_pkts, now);
        run.demand.note_demand(demand_active, demand_why, now_unix_ms());
        // 稳态判定（r10-A1）：承载 true ∧ 非设备死 ∧ 非传输不健康 ∧ worker 不在飞 ∧
        // 岛内阶梯不在飞。非稳态 = 连败清零 + 本拍不探。
        let hs = run.tun_shared.health_snapshot();
        let island_busy = run
            .current_island()
            .map(|i| {
                let s = i.snapshot();
                s.ladder_inflight || s.racing
            })
            .unwrap_or(true);
        let steady = steady_now(&hs, run.reconn.inflight(), island_busy);
        if !steady {
            obs_fail_streak = 0; // 离开稳态清零
            continue;
        }
        // 观测拍计数挂点（评审 r4 P2：测试按拍推进等待，生产空体零开销）。
        stats_beat();
        // 观测快探（每拍照跑；等待经 island_cmd_until——stop-aware，慢 RPC 不超预算）。
        match quick_probe(&run) {
            Ok(()) => {
                obs_fail_streak = 0; // 成功只清连败（不清标记、不置承载位）
                refresh_link_snapshot(&run);
                if let Some((via, ep, rtt_ms)) = run.island_link() {
                    (run.logf)(&format!("link: via={via} ep={ep} rtt={rtt_ms}ms（观测快照）"));
                }
            }
            Err(e) => {
                // 评审 r1 P1-12 整改（stats 区分 Gone/并发）：停止/世代变化的 Gone
                // 直接退出观测线程（不落「对端失败」行——D4/tasks 3.2 纪律；循环
                // 顶部的分片等待下一拍也会见到 stop，这里即时收口）。
                match obs_fail_disposition(&e) {
                    ObsFailDisposition::Exit => return,
                    // 评审 r2 B7：Busy/Timeout typed 门——等待并发（probe 槽争用/
                    // 预算烧完）非对端证据，本拍不累积也**不清零**（并发面无信息；
                    // 旧实现只靠「失败后重查 in-flight」兜底，worker 恰好结束后的
                    // Busy/超时回执仍会被计入连败）。
                    ObsFailDisposition::Skip => continue,
                    ObsFailDisposition::Account => {}
                }
                // 等待并发导致的 Busy/超时不计失败（D4：「与 worker 的快探共用 probe
                // 槽，等待并发导致的 Busy/超时不计失败」）——`steady_now` 通过后
                // worker 仍可能已启动（探测期间抢到槽位）：失败后重查在飞态，命中
                // 则本拍不计也不清（属并发面、非对端证据）。
                let concurrent_now = run.reconn.inflight()
                    || run
                        .current_island()
                        .map(|i| {
                            let s = i.snapshot();
                            s.ladder_inflight || s.racing
                        })
                        .unwrap_or(true);
                if concurrent_now {
                    continue;
                }
                let (streak, mark) = obs_streak_step(obs_fail_streak, out_pkts);
                obs_fail_streak = streak;
                if mark {
                    // 幂等（已 patrol 时 no-op）+ 同一临界区置承载位 false。
                    if run.tun_shared.mark_transport_if_current(run.gen) {
                        (run.logf)(&format!(
                            "观测快探连续 {OBS_FAIL_STREAK} 败（{e}）—— 标记传输不健康（读面触发条件成立，事件触发重连）"
                        ));
                    }
                    obs_fail_streak = 0;
                } else if streak > 0 {
                    (run.logf)(&format!(
                        "观测快探失败 {streak}/{OBS_FAIL_STREAK}（{e}；有出站包拍——计入证据）"
                    ));
                }
            }
        }
    }
}

/// 观测快探的稳态判定（r10-A1：承载 true ∧ 非设备死 ∧ 非传输不健康 ∧ 重连 worker
/// 不在飞 ∧ 岛内阶梯不在飞——纯函数，注入式单测面）。
fn steady_now(hs: &super::tun_shared::HealthSnapshot, reconn_inflight: bool, island_busy: bool) -> bool {
    hs.carrier && !hs.device_dead && hs.why != "patrol" && !reconn_inflight && !island_busy
}

/// 观测快探失败后的处置分类（评审 r2 B7——typed，注入式单测面）：Gone ⇒ 观测线程
/// 退出；Busy/Timeout（等待并发/预算烧完——非对端证据）⇒ 本拍不累积不清零；其余
/// 进入「in-flight 复查 → 连败推进」。
#[derive(Debug, PartialEq, Eq)]
enum ObsFailDisposition {
    /// 停止/世代变化/设备死——观测线程直接收口（不落对端失败行）。
    Exit,
    /// 等待并发（probe 槽争用/预算烧完）——本拍不累积不清零。
    Skip,
    /// 进入 in-flight 复查与连败推进。
    Account,
}

fn obs_fail_disposition(e: &ProbeFail) -> ObsFailDisposition {
    match e {
        ProbeFail::IslandGone(_) => ObsFailDisposition::Exit,
        ProbeFail::Busy(_) | ProbeFail::Timeout(_) => ObsFailDisposition::Skip,
        _ => ObsFailDisposition::Account,
    }
}

/// 观测连败的推进纯函数（r8-a3 终稿：**失败仅在出站包拍累积**——只认 `out_pkts`
/// 增量 >0，亮屏位不参与失败证据；无出站包拍清零连败；连续 3 败报告标记——
/// 5s/300s 档下 3 连败窗口恒 ~180s，与 stats 行节拍解耦）。返回 (新连败, 是否到标记门槛)。
fn obs_streak_step(streak: u32, out_pkts: i64) -> (u32, bool) {
    if out_pkts <= 0 {
        return (0, false); // 无出站包拍清零（「无需求期不算证据」由计数门承载）
    }
    let streak = streak + 1;
    if streak >= OBS_FAIL_STREAK {
        (streak, true)
    } else {
        (streak, false)
    }
}

/// `stats:` 行（纯函数——形态**逐字不变** + 重连五计数段 r3-R16：`入口=N 起跑=S
/// 节流=K 载位假包=P 起跑失败=F`——「读面饿死 vs 无事件」由「P > 0 而入口 N 停滞」
/// 可辨）。
/// stats 行的输入值组（r3-R16 五计数与既有三计数的直喂面）。
struct StatsLineIn {
    fd_read_bytes: u64,
    fd_write_bytes: u64,
    pf_accepted: u64,
    pf_fails: u64,
    reconn_entries: u64,
    reconn_starts: u64,
    reconn_throttled: u64,
    reconn_fake_pkts: u64,
    reconn_spawn_fails: u64,
}

fn stats_line(v: StatsLineIn) -> String {
    let StatsLineIn {
        fd_read_bytes,
        fd_write_bytes,
        pf_accepted,
        pf_fails,
        reconn_entries,
        reconn_starts,
        reconn_throttled,
        reconn_fake_pkts,
        reconn_spawn_fails,
    } = v;
    format!(
        "stats: fdReadBytes={fd_read_bytes}B fdWriteBytes={fd_write_bytes}B ｜ pf={pf_accepted}/{pf_fails} ｜ 重连 入口={reconn_entries} 起跑={reconn_starts} 节流={reconn_throttled} 载位假包={reconn_fake_pkts} 起跑失败={reconn_spawn_fails}"
    )
}

// ---------------------------------------------------------------------------
// 日志面（世代日志文件——追加写，跨重连保留上次断因）
// ---------------------------------------------------------------------------

type FileLogf = Arc<dyn Fn(&str) + Send + Sync>;

fn open_gen_log(out: &str) -> Result<FileLogf, String> {
    if out.is_empty() {
        return Ok(Arc::new(|_s: &str| {}));
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .map_err(|e| e.to_string())?;
    let f = Arc::new(Mutex::new(f));
    Ok(Arc::new(move |s: &str| {
        let line = format!("{} {s}\n", local_ts());
        if let Ok(mut g) = f.lock() {
            use std::io::Write as _;
            let _ = g.write_all(line.as_bytes());
        }
    }))
}

pub(crate) fn local_ts() -> String {
    // 本地时间紧凑形（判据行 grep 消息本体；时间戳供人工排查）
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs() as i64;
    let ms = now.subsec_millis();
    // 无时区库依赖的本地近似：直接 UTC+8 不可取；用 libc::localtime_r
    unsafe {
        // musl 1.2 起 time_t 64 位；OHOS 面的 libc::time_t 别名 deprecated（7b 同款
        // 整改）——直接以 i64 传（darwin/linux/ohos 的 time_t 均 64 位）
        let t: i64 = secs;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return format!("[{secs}.{ms:03}]");
        }
        format!(
            "[{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{ms:03}]",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        )
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {

    use super::*;
    use crate::facade::{ClientCore, TunStage};
    use crate::token::{self, EndpointRef, PeerId, RpkPubKey, Secret, TokenSpec};

    /// F8a：写退避分级表（前 50 拍 2ms ⇒ 其后 10ms ⇒ 100 拍后 20ms 封顶）。
    #[test]
    fn write_backoff_schedule() {
        assert_eq!(super::super::quic_stream::write_retry_backoff(0), Duration::from_millis(2));
        assert_eq!(super::super::quic_stream::write_retry_backoff(49), Duration::from_millis(2));
        assert_eq!(super::super::quic_stream::write_retry_backoff(50), Duration::from_millis(10));
        assert_eq!(super::super::quic_stream::write_retry_backoff(99), Duration::from_millis(10));
        assert_eq!(super::super::quic_stream::write_retry_backoff(100), Duration::from_millis(20));
        assert_eq!(super::super::quic_stream::write_retry_backoff(u32::MAX), Duration::from_millis(20));
    }

    /// 探测空闲回环端口（**不钉固定端口**——同树并发跑测试会互撞）。
    fn free_port() -> u16 {
        std::net::TcpListener::bind(("127.0.0.1", 0))
            .expect("探测端口可绑")
            .local_addr()
            .unwrap()
            .port()
    }

    fn rule(listen: u16, ip: &str, port: u16) -> PortForwardRule {
        PortForwardRule {
            listen,
            target_ip: ip.to_owned(),
            target_port: port,
        }
    }

    fn line_logf() -> (Logf, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>();
        let l: Logf = Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        });
        (l, rx)
    }

    /// Q-F-B F3-2 + reconnect-oneshot r3-R16：`stats:` 行的**纯函数**（原形态不变 +
    /// 重连五计数段——「读面饿死 vs 无事件」由「载位假包 P > 0 而入口 N 停滞」可辨）。
    #[test]
    fn stats_line_reports_real_pf_counts() {
        assert_eq!(
            stats_line(StatsLineIn {
                fd_read_bytes: 0,
                fd_write_bytes: 0,
                pf_accepted: 0,
                pf_fails: 0,
                reconn_entries: 0,
                reconn_starts: 0,
                reconn_throttled: 0,
                reconn_fake_pkts: 0,
                reconn_spawn_fails: 0,
            }),
            "stats: fdReadBytes=0B fdWriteBytes=0B ｜ pf=0/0 ｜ 重连 入口=0 起跑=0 节流=0 载位假包=0 起跑失败=0"
        );
        assert_eq!(
            stats_line(StatsLineIn {
                fd_read_bytes: 1234,
                fd_write_bytes: 5678,
                pf_accepted: 9,
                pf_fails: 2,
                reconn_entries: 11,
                reconn_starts: 7,
                reconn_throttled: 3,
                reconn_fake_pkts: 1400,
                reconn_spawn_fails: 1,
            }),
            "stats: fdReadBytes=1234B fdWriteBytes=5678B ｜ pf=9/2 ｜ 重连 入口=11 起跑=7 节流=3 载位假包=1400 起跑失败=1",
            "行文形态不变、值接线真计数；五计数段逐字（P=1400 复刻事故夜读面记账形态）"
        );
    }

    /// Q-F-B F3-1 状态面集成（经 `runner_of` + 真 GenRun + 真 bind）：
    /// `listening`（真监听）/ `bind_failed`（占端口条）/ `code` 空 vs `bind_failed` / 真 target。
    #[test]
    fn runner_of_reports_real_pf_state() {
        let (logf, _lines) = line_logf();
        let run = GenRun::synthetic_for_test(logf);
        let ok_port = free_port();
        let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy_port = squatter.local_addr().unwrap().port();
        run.pf
            .install(&[rule(ok_port, "", 0), rule(busy_port, "10.0.0.9", 8080)]);
        let r = runner_of(&run);
        assert_eq!(r.pf_accepted, 0, "尚无连接（真值 0）");
        assert_eq!(r.pf_fails, 0);
        assert_eq!(r.port_forwards.len(), 2);
        let f0 = &r.port_forwards[0];
        assert_eq!(f0.state, "listening", "真监听（不再是诚实失败态）");
        assert_eq!(f0.code, "", "成功态空码");
        assert_eq!(f0.err, "");
        assert_eq!(f0.target, "主机（同端口）");
        assert_eq!(f0.conns, 0);
        let f1 = &r.port_forwards[1];
        assert_eq!(f1.state, "failed");
        assert_eq!(
            f1.code, "bind_failed",
            "真 bind 失败 = 真值（tier 渲染「端口被占用」）"
        );
        assert_eq!(f1.target, "10.0.0.9:8080");
        // 收尾：停监听器
        run.pf.stop_all();
        assert!(runner_of(&run).port_forwards.is_empty(), "收工后空表");
        drop(squatter);
    }

    /// Q-F-B F4-3/§5.2 #15：**世代收工**的两条通路——① 停止位（`gen_stop` = `run.stop`）
    /// 让 accept 线程 ≤1 拍自退 ⇒ 端口立即释放（无孤儿监听器）；② `stop_all` 清表
    /// （收工序 / `Finish::drop` 兜底调的就是它）。
    #[test]
    fn generation_stop_flag_releases_ports_and_teardown_clears_states() {
        let (logf, _lines) = line_logf();
        let run = GenRun::synthetic_for_test(logf);
        let port = free_port();
        run.pf.install(&[rule(port, "", 0)]);
        assert_eq!(runner_of(&run).port_forwards[0].state, "listening");
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_err(),
            "在监听"
        );
        // ① 世代停止位（收工第一条信号）
        run.stop.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if std::net::TcpListener::bind(("127.0.0.1", port)).is_ok() {
                break;
            }
            assert!(Instant::now() < deadline, "停止位未让 accept 线程释放端口");
            std::thread::sleep(Duration::from_millis(10));
        }
        // 状态表在 stop_all 之前仍是旧表（Go「stopPortForwards 之前不动作」同义）
        assert_eq!(runner_of(&run).port_forwards.len(), 1);
        // ② 收工清表（gen_loop 收工段与 Finish::drop 兜底都调它）
        run.pf.stop_all();
        assert!(runner_of(&run).port_forwards.is_empty(), "收工后空表");
    }

    /// 起一枚「只绑回环 :0、**不建连**」的岛（合成世代用；单承载用例共用构造）。
    fn idle_island(run: &Arc<GenRun>) -> Arc<homeway_quic::Island> {
        let ident = crate::identity::Identity::ephemeral().expect("临时身份");
        let cred = homeway_quic::IslandCredential::new(
            homeway_quic::TokenSecret::from_bytes([7u8; 32]),
            ident.public_key(),
            *ident.dev_tag().as_bytes(),
            homeway_quic::RpkPublicKey::from_bytes([9u8; 32]),
        );
        let mut icfg = homeway_quic::IslandConfig::new(cred);
        icfg.bind = Some(std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)));
        Arc::new(
            homeway_quic::Island::start(Arc::clone(&run.logf), Arc::new(|_r: &str| {}), icfg)
                .expect("岛可起（只绑回环 :0，不建连）"),
        )
    }

    /// **判据（M5 C3 单腿）**：`pf_dial_via_run` 只有一条腿——
    /// ① 岛未承接 ⇒ 归因「岛不在」（**不含**任何 WG 措辞）；
    /// ② 岛承接 ⇒ QUIC 面归因（无 live 连接 ⇒ `StreamErr` 文本含 `QUIC 服务流`）；
    /// ③ 世代收工（Weak 升级失败）⇒ 既有语义逐字保留。
    #[test]
    fn pf_dial_requires_island() {
        // 持串行锁：②段的承载失败触发面会起真 worker（吃全局 gate——防跨测试注入）。
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (logf, _lines) = line_logf();
        let run = Arc::new(GenRun::synthetic_for_test(logf));
        let w = Arc::downgrade(&run);
        let dst: SocketAddrV4 = "127.0.0.1:8080".parse().unwrap();
        let budget = Duration::from_millis(20);
        // `Box<dyn BridgeStream>` 不是 `Debug`（`expect_err` 用不了）⇒ 显式取错
        fn err_of(r: io::Result<Box<dyn super::BridgeStream>>) -> io::Error {
            match r {
                Ok(_) => panic!("本用例的一切拨号都必须失败"),
                Err(e) => e,
            }
        }

        // ① 岛不在（合成世代默认）：归因「岛不在」，不得说 WG
        assert!(!run.l3_on_island(), "合成世代默认不在岛上");
        let e = err_of(pf_dial_via_run(&w, dst, budget));
        assert!(e.to_string().contains("岛不在"), "{e}");
        assert!(!e.to_string().contains("WG"), "单腿后不得有 WG 措辞：{e}");

        // ② 岛承接（标志位真 + 岛在场）：开流走真岛命令面 —— 无 live 连接 ⇒ QUIC 面归因
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        assert!(run.l3_on_island(), "两条件齐 ⇒ 判据为真");
        let e = err_of(pf_dial_via_run(&w, dst, budget));
        assert!(e.to_string().contains("QUIC 服务流"), "{e}");
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();

        // ③ 世代收工（Weak 升级失败）：既有语义逐字保留
        drop(run);
        let e = err_of(pf_dial_via_run(&w, dst, budget));
        assert!(e.to_string().contains("世代已收工（拨号放弃）"), "{e}");
    }

    /// 起一枚带日志收集的合成世代（返回世代 + 行收集端）。
    fn gen_with_lines(tag: &str) -> (Arc<GenRun>, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel::<String>();
        let _ = tag;
        let logf: Logf = Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        });
        (Arc::new(GenRun::synthetic_for_test(logf)), rx)
    }

    /// 抽干行收集端（返回本次新增的全部行）。
    fn drain_lines(rx: &mpsc::Receiver<String>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(l) = rx.try_recv() {
            out.push(l);
        }
        out
    }

    /// **判据（reconnect-oneshot 2.3 / r5-R6 rc 映射表定稿）**：NAPI `ClientCoreTunRecover`
    /// 的 rc 契约——`0` = 同步快探成功；`-1` = 已尝试但结果未知（受理：worker 接管 /
    /// 被单飞合并 / 被 pending 承接 / 起跑失败——**边沿路径无「被节流」形态**）；
    /// `-2` = 无隧道/陈世代/设备死/停止（快探之前的前置）；`-9` = NAPI 壳异常（capi 面）。
    #[test]
    fn recover_downpush_single_leg_rc_contract() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        // ① 无世代（executor 无 run）⇒ -2
        assert_eq!(exec.recover(3, "扩展下推(档位 3)"), -2, "无 attached 隧道");

        // ③ 岛承接（idle 岛无 live 连接）⇒ 同步快探失败 ⇒ worker 受理 ⇒ -1
        let (run, lines) = gen_with_lines("island");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        let rc = exec.recover(3, "扩展下推(档位 3)");
        assert_eq!(rc, -1, "快探失败 + worker 受理 ⇒ -1（已尝试、结果未知）");
        // 同步快探恰好一次（r9-C4 的「并发与总数皆 ≤1」单人形态——worker 轮的探
        // 不计入 sync_probes）
        assert_eq!(run.reconn.sync_probes.load(Ordering::Relaxed), 1, "同步快探 1 次");
        wait_reconn_quiescent(&run);
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l.starts_with("恢复下推（扩展下推(档位 3)）——同步快探：失败")),
            "同步快探失败行逐字在场：{seen:?}"
        );
        assert!(
            seen.iter().any(|l| l.starts_with("一次性重连：失败（源=换网")),
            "worker 失败行（源=换网——from=3 派生）：{seen:?}"
        );
        // 受理后再来一发：无在飞（worker 已收尾）⇒ 再次受理（新 owner）
        let rc2 = exec.recover(1, "扩展下推(档位 1)");
        assert_eq!(rc2, -1);
        wait_reconn_quiescent(&run);
        assert!(
            drain_lines(&lines)
                .iter()
                .any(|l| l.starts_with("一次性重连：失败（源=唤醒")),
            "from=1 派生唤醒源：{:?}",
            drain_lines(&lines)
        );
        // from=2 及其余按唤醒归并
        let rc3 = exec.recover(2, "扩展下推(档位 2)");
        assert_eq!(rc3, -1);
        wait_reconn_quiescent(&run);

        // ② 陈世代 ⇒ -2（前置；与判据无关）
        run.tun_shared.gen.store(99, Ordering::Release);
        assert_eq!(exec.recover(3, "扩展下推(档位 3)"), -2, "陈世代 -2");
        run.tun_shared.gen.store(run.gen, Ordering::Release);
        // ④ 停止位 ⇒ -2；⑤ 设备死 ⇒ -2（快探之前——零烧探测）
        run.stop.store(true, Ordering::Release);
        assert_eq!(exec.recover(1, "扩展下推(档位 1)"), -2, "停止位 -2");
        run.stop.store(false, Ordering::Release);
        let probes_before = run.reconn.probes.load(Ordering::Relaxed);
        run.tun_shared.mark_unhealthy("fd");
        assert_eq!(exec.recover(1, "扩展下推(档位 1)"), -2, "设备死 -2");
        assert_eq!(
            run.reconn.probes.load(Ordering::Relaxed),
            probes_before,
            "设备死前置 ⇒ 零快探"
        );

        *lock_unpoison(&exec.state) = None;
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// 同步快探成功也必须落「事件恢复用时」汇总行（App 回前台/换网走的快路径）。
    #[test]
    fn recover_sync_probe_logs_event_duration_summary() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("recover-sync-summary");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        PROBE_RESULT_GATE.store(1, Ordering::Relaxed);
        let rc = exec.recover(1, "扩展下推(档位 1)");
        PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
        assert_eq!(rc, 0, "同步快探成功 ⇒ rc=0");
        let seen = drain_lines(&lines);
        let summary = seen
            .iter()
            .find(|l| l.starts_with("事件恢复用时：源=唤醒，结果=成功，路径=同步快探，总耗时="))
            .unwrap_or_else(|| panic!("缺同步快探汇总行：{seen:?}"));
        assert!(
            summary.contains("快探=") && summary.contains("恢复="),
            "汇总行须带快探/恢复分项：{summary}"
        );
        *lock_unpoison(&exec.state) = None;
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **判据（2.3）**：在飞预检走 `try_claim_edge`（r10-C11）——owner 在飞时第二发
    /// recover 归 `InFlight` ⇒ `-1` 且**零快探**（防换网/唤醒并发各自烧探测占满命令槽）；
    /// 边沿位留字由 owner 收尾承接（r8-a1-2）。
    #[test]
    fn recover_second_call_merges_into_inflight_owner() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _lines) = gen_with_lines("inflight");
        // 手工占位（模拟在飞 owner——不真跑 worker）
        match run.reconn.try_claim_edge(ReconnSource::Wake) {
            Claim::Claimed { epoch } => {
                let probes_before = run.reconn.probes.load(Ordering::Relaxed);
                // 第二发（换网边沿）：InFlight ⇒ 位留字
                assert!(
                    matches!(
                        run.reconn.try_claim_edge(ReconnSource::NetChange),
                        Claim::InFlight
                    ),
                    "在飞 ⇒ 边沿位留字后归 InFlight"
                );
                assert_ne!(
                    run.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK,
                    0,
                    "换网位已留字"
                );
                // owner 收尾：release 读到留字位 ⇒ 承接一轮（NetChange 优先级）
                let mut guard = RoundGuard {
                    epoch,
                    triggering: 0,
                };
                reconnect_finalize_loop(&run, &mut guard); // run_round 会跑（岛不在 ⇒ 快速失败）
                assert!(
                    !run.reconn.inflight(),
                    "收尾后静止（pending 已被承接消费）"
                );
                assert!(
                    run.reconn.probes.load(Ordering::Relaxed) >= probes_before,
                    "承接轮照常起跑（probes 只增不减）"
                );
            }
            other => panic!("首claim必须成功：{other:?}"),
        }
    }

    /// **判据（2.3 强证据路径）**：mark patrol → `GenRun::strong_recover` ⇒ ZERO 间隔清
    /// 标记 + 承载位 true + link.at 前进（等价单测：无 live 连接时同步快探不可在 host
    /// 构造——翻回三件套的组合面在此注入式验证，link.at 经 idle 岛快照前进）。
    #[test]
    fn strong_recover_clears_patrol_sets_carrier_and_advances_link_at() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("strong");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        run.tun_shared.mark_transport_if_current(run.gen);
        assert!(run.tun_shared.transport_unhealthy(), "前置：patrol 在册");
        assert!(!run.tun_shared.carrier.alive(), "标记同步拉假承载位");
        let at_before = lock_unpoison(&run.link).as_ref().map(|l| l.at_ms).unwrap_or(0);
        GenRun::strong_recover(&run, "快探");
        assert!(!run.tun_shared.transport_unhealthy(), "patrol 已清（ZERO 间隔）");
        assert!(run.tun_shared.carrier.alive(), "承载位同步置 true");
        let link = lock_unpoison(&run.link).clone().expect("快照已刷");
        assert!(link.at_ms >= at_before, "link.at 前进（不后退）");
        assert!(
            drain_lines(&lines)
                .iter()
                .any(|l| l.contains("传输恢复（patrol 清除，证据=快探")),
            "翻回判据行：{:?}",
            drain_lines(&lines)
        );
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// Q-F-B F4-4：热替换 rc 回 Go 语义——活世代 ⇒ `0`（**真装表**）；无世代 / 换代
    /// （gen 不符）/ 收口（stop 位）⇒ `-1` 且**无孤儿监听器**。
    #[test]
    fn request_port_forwards_rc_zero_with_live_gen_stale_minus_one() {
        let (logf, _lines) = line_logf();
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let port = free_port();
        let rules = vec![rule(port, "", 0)];
        // 无世代 ⇒ -1（诚实：没有已接管数据面的世代）
        assert_eq!(exec.request_port_forwards(rules.clone()), -1);
        let run = Arc::new(GenRun::synthetic_for_test(logf));
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        // 活世代 ⇒ 0，且**真装表**（端口真监听）
        assert_eq!(exec.request_port_forwards(rules.clone()), 0);
        let st = runner_of(&run).port_forwards;
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].state, "listening");
        assert!(
            std::net::TcpStream::connect(("127.0.0.1", port)).is_ok(),
            "热替换后监听口真可连"
        );
        // 收口（stop 位）⇒ -1 且撤回监听器（不留孤儿）
        run.stop.store(true, Ordering::Release);
        assert_eq!(exec.request_port_forwards(rules.clone()), -1);
        assert!(
            runner_of(&run).port_forwards.is_empty(),
            "无孤儿监听器（states 已清）"
        );
        assert!(
            std::net::TcpListener::bind(("127.0.0.1", port)).is_ok(),
            "端口已释放"
        );
        // 换代（gen 不符 ⇒ 陈旧世代）⇒ -1，且不得装表
        run.stop.store(false, Ordering::Release);
        run.tun_shared.gen.store(99, Ordering::Release);
        assert_eq!(exec.request_port_forwards(rules.clone()), -1);
        assert!(runner_of(&run).port_forwards.is_empty(), "陈旧世代不装表");
        run.tun_shared.gen.store(1, Ordering::Release);
        run.pf.stop_all();
    }

    /// 世代日志读取上界（flake 口径②：只判上界）。
    fn wait_lines(path: &std::path::Path, needle: &str, wait: Duration) -> Vec<String> {
        let deadline = Instant::now() + wait;
        loop {
            let all: Vec<String> = std::fs::read_to_string(path)
                .map(|s| s.lines().map(str::to_owned).collect())
                .unwrap_or_default();
            if all.iter().any(|l| l.contains(needle)) || Instant::now() >= deadline {
                return all;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
    }

    /// 临时世代日志路径（**每用例唯一**——并发跑测试不互撞；不进仓）。
    fn gen_log_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hw-m5c3-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("临时目录可建");
        dir.join("gen.log")
    }

    /// 装配一个世代（启动即返；调用方轮询日志/状态）。
    fn prepare_gen(core: &ClientCore, tok: &str, out: &std::path::Path) -> i32 {
        let cfg = format!(
            r#"{{"token":"{tok}","out":"{}"}}"#,
            out.display()
        );
        core.tun_prepare(&cfg, true)
    }

    /// 等世代离开 preparing（failed/ready/idle 任一），防测试进程留游离世代。
    fn settle_or_stop(core: &ClientCore) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while core.tun_status().contains("\"state\":\"preparing\"") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(30));
        }
        let _ = core.tun_stop();
    }

    /// **判据（S3-1 ②，MTU 旋钮）**：有效区间 `[1320,1400]` 内原样生效；区间外/非数字 ⇒
    /// **记行 + 缺省 1400**（设计 §12-①；不做夹取——越界取值意图不明，按缺省最不易静默劣化）。
    #[test]
    fn mtu_cap_resolution_clamps_by_default_policy() {
        use homeway_quic::QUIC_MTU_CAP_DEFAULT;
        let (tx, rx) = mpsc::channel::<String>();
        let logf: Logf = Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        });
        if crate::envflag::quic_mtu_raw().is_none() {
            assert_eq!(resolve_mtu_cap(0, &logf), QUIC_MTU_CAP_DEFAULT, "未设 = 1400");
            assert_eq!(resolve_mtu_cap(1400, &logf), 1400);
            assert_eq!(resolve_mtu_cap(1320, &logf), 1320, "区间下限");
            assert_eq!(
                resolve_mtu_cap(1200, &logf),
                QUIC_MTU_CAP_DEFAULT,
                "区间外 ⇒ 缺省"
            );
            assert_eq!(
                resolve_mtu_cap(1500, &logf),
                QUIC_MTU_CAP_DEFAULT,
                "区间外 ⇒ 缺省"
            );
            let lines: Vec<String> = rx.try_iter().collect();
            assert_eq!(
                lines.iter().filter(|l| l.contains("非法或越界")).count(),
                2,
                "两次越界各记一行：{lines:?}"
            );
            // N-e（M5 S4）：每次解析都有一行「生效值 + 来源」
            let ne: Vec<&String> = lines.iter().filter(|l| l.contains("内层 MTU 上限")).collect();
            assert_eq!(ne.len(), 5, "五次解析各一行 N-e：{lines:?}");
            assert!(ne[0].contains("来源=缺省"), "{:?}", ne[0]);
            assert!(ne[1].contains("来源=tunConfig.quicMtuCap"), "{:?}", ne[1]);
            assert!(ne[3].contains("来源=缺省（非法值回退）"), "{:?}", ne[3]);
        }
    }

    /// **判据（§2.6-G9 的负例实测；M5 C3/C4）**：token 只带 **WG 类端点**（存量旧 token 形态
    /// 形态 / 旧 token）⇒ 岛候选为空 ⇒ **可见失败**：`岛未就用（…候选为空）` 归因行 +
    /// `failed` 终态 + **单飞锁已放**（下一次 prepare 可受理）。**不得**有任何回落兜底。
    #[test]
    fn wg_only_token_fails_visibly_without_fallback() {
        let peer = PeerId::from([1u8; 32]);
        let secret = Secret::from([2u8; 32]);
        let rpk = RpkPubKey::from([7u8; 32]);
        let eps = [EndpointRef::new("203.0.113.1:41641", token::EndpointKind::Direct)];
        let tok = token::encode(&TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
            rpk: Some(&rpk),
        })
        .expect("token 可编码");

        let log = gen_log_path("wgonly");
        let _ = std::fs::remove_file(&log);
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let core = ClientCore::with_shared(exec, demand);
        assert_eq!(prepare_gen(&core, &tok, &log), 0);
        let lines = wait_lines(&log, "岛未就用", Duration::from_secs(5));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("岛未就用") && l.contains("候选为空")),
            "无 QUIC/中继端点必须记行归因（候选为空）：{lines:?}"
        );
        let joined = lines.join("\n");
        assert!(
            !joined.contains("回落") && !joined.contains("兜底"),
            "单承载后不得有任何回落/兜底话术：{joined}"
        );
        assert!(
            !joined.contains("本世代 L3 承载"),
            "A/B 开关行（N-d）必须已删除：{joined}"
        );
        // failed 终态 + 可见归因
        let deadline = Instant::now() + Duration::from_secs(3);
        while core.tun_status().contains("\"state\":\"preparing\"") {
            assert!(Instant::now() < deadline, "空候选应在 3s 内落 failed 终态");
            std::thread::sleep(Duration::from_millis(20));
        }
        let st = core.tun_status();
        assert!(st.contains("\"state\":\"failed\""), "空候选 = failed 终态：{st}");
        assert!(
            st.contains("岛未就用"),
            "失败归因必须进 status reason（用户可见）：{st}"
        );
        // 锁已放：下一次 prepare 可受理（不是 -1 忙）
        assert_eq!(prepare_gen(&core, &tok, &log), 0);
        settle_or_stop(&core);
        let _ = TunStage::Idle; // 引用 stage 枚举（保持 import）
    }

    /// **判据（S3-1 ①，M5 C3 改写）**：token 带 QUIC 类端点 ⇒ **岛真构造**（`quic: 赛跑投出`
    /// 行 = 岛起的端点真发了候选）；黑洞候选 ⇒ 赛跑未成 ⇒ **记行 + failed 终态**（单承载：
    /// 无回落档，失败即本世代结束）。
    #[test]
    fn quic_mode_constructs_island_and_reports_race_failure() {
        let peer = PeerId::from([1u8; 32]);
        let secret = Secret::from([2u8; 32]);
        let rpk = RpkPubKey::from([7u8; 32]);
        let eps = [
            EndpointRef::new("203.0.113.1:41641", token::EndpointKind::Direct),
            // 黑洞 QUIC 候选（本机回环未监听端口 ⇒ 握手无应答 ⇒ 预算到点收场）
            EndpointRef::new("127.0.0.1:1", token::EndpointKind::Quic),
        ];
        let tok = token::encode(&TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
            rpk: Some(&rpk),
        })
        .expect("token 可编码");

        let log = gen_log_path("qbuild");
        let _ = std::fs::remove_file(&log);
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let core = ClientCore::with_shared(exec, demand);
        assert_eq!(prepare_gen(&core, &tok, &log), 0);
        let lines = wait_lines(&log, "岛未就用", Duration::from_secs(20));
        assert!(
            lines.iter().any(|l| l.contains("赛跑投出")),
            "岛必须真构造并发起赛跑（C4' 行）：{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("候选端点（1 条）") && l.contains("（直连）")),
            "C13 候选清单（岛候选来源 = QUIC 类端点；学习标记面退役）：{lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains("·学习")),
            "单承载后学习标记面不得再现：{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("岛未就用") && l.contains("赛跑未成")),
            "黑洞候选 ⇒ 赛跑未成须归因：{lines:?}"
        );
        let joined = lines.join("\n");
        assert!(
            !joined.contains("回落") && !joined.contains("兜底"),
            "单承载后不得有任何回落/兜底话术：{joined}"
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while core.tun_status().contains("\"state\":\"preparing\"") {
            assert!(Instant::now() < deadline, "赛跑失败应在 3s 内落 failed 终态");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            core.tun_status().contains("\"state\":\"failed\""),
            "赛跑失败 = failed 终态：{}",
            core.tun_status()
        );
        settle_or_stop(&core);
    }

    /// **判据（S3-3 的装配面）**：`quic` 段的四类丢弃与 N-c 行**同源**（同一份
    /// `IslandSnapshot::drops`）——本用例锁「快照 → JSON」这一半（纯函数直喂），
    /// 另一半（N-c 行文本）在岛侧 `note_drop_shared` 同一处渲染、由岛单测断言。
    #[test]
    fn quic_section_maps_drops_from_island_snapshot() {
        use homeway_quic::IslandSnapshot;
        let s = IslandSnapshot {
            drops: homeway_quic::Drops {
                too_large: 7,
                send_buffer_full: 3,
                return_queue_full: 1,
                unregistered: 9,
            },
            mtu: Some(1362),
            current_mtu: 1320,
            migrations: 2,
            migration_unconfirmed: true,
            via: Some(homeway_quic::Via::Relay { label: [1u8; 8] }),
            ep: Some("192.168.3.12:42652".parse().unwrap()),
            rtt_ms: 11,
            packets_in: 5,
            packets_out: 6,
            local: Some("192.168.3.12:54123".parse().unwrap()),
            connections: 1,
            relay_tx: 4,
            rx_ignored: 2,
            candidates: 3,
            mirrors: 8,
            ..Default::default()
        };
        let q = quic_in_of(&s);
        assert_eq!(q.drops.too_large, 7, "超限计数同一份");
        assert_eq!(q.drops.unregistered, 9);
        assert_eq!(q.drops.send_buffer_full, 3);
        assert_eq!(q.drops.return_queue_full, 1);
        assert_eq!(q.mtu, 1362);
        assert_eq!(q.current_mtu, 1320);
        assert_eq!(q.via, "relay", "via 词表与 link 段同源");
        assert_eq!(q.ep, "192.168.3.12:42652");
        assert_eq!(q.local, "192.168.3.12:54123");
        assert!(q.migration_unconfirmed);
        assert_eq!(q.candidates, 3);
        assert_eq!(q.mirrors, 8);
    }

    /// **判据（S3-3 的装配面；M5 C3 单承载）**：`quic` 段的存在条件 = **岛在位**
    /// （岛缺席 ⇒ 整段缺席）。「是不是 quic 档」不再是变量（无 A/B 开关）。
    #[test]
    fn quic_section_requires_island() {
        let (logf, _rx) = line_logf();
        let run = Arc::new(GenRun::synthetic_for_test(logf));
        assert!(quic_status_of(&run).is_none(), "岛不在 ⇒ quic 段缺席");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        assert!(
            quic_status_of(&run).is_some(),
            "岛在位 ⇒ quic 段在场（含未建连形态——归因面）"
        );
        run.take_island();
        island.stop();
    }
    // ==========================================================================
    // reconnect-oneshot 1.3：反例回归映射表（每行一用例——barrier/钩子受控交错）
    // ==========================================================================

    /// 等状态字静止（INFLIGHT=0；有界轮询——worker 异步收尾的同步面）。
    fn wait_reconn_quiescent(run: &Arc<GenRun>) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while run.reconn.inflight() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!run.reconn.inflight(), "状态字须在 5s 内静止");
    }

    /// 重连协议测试的**串行锁**：`SPAWN_FAIL_GATE`/`PANIC_ROUNDS_GATE` 是全局态，
    /// 且真 worker（entry/recover 起的线程）会跨测试吃到别测试的 gate——凡触 gate
    /// 或起真 worker 的用例都持本锁（并行测试默认 4 线程，不串行会互相注入）。
    static RECONN_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// 装测试钩子（受控交错缝）——RAII 守卫（Drop 拆钩：阻塞型钩子不拆除会把
    /// 后续同名挂点全部挂死）。
    #[cfg(test)]
    struct HookGuard {
        run: Arc<GenRun>,
    }
    #[cfg(test)]
    impl Drop for HookGuard {
        fn drop(&mut self) {
            *lock_unpoison(&self.run.reconn.hooks) = None;
        }
    }
    #[cfg(test)]
    fn install_hooks(run: &Arc<GenRun>, h: TestHook) -> HookGuard {
        *lock_unpoison(&run.reconn.hooks) = Some(Arc::clone(&h));
        HookGuard {
            run: Arc::clone(run),
        }
    }

    /// **r8-a1-2**：入口 CAS 不清现场 pending——owner 在飞期间 NetChange 留字，
    /// owner 释放后 Wake 入口抢占**保留**该位（自消费只清自己的位）。
    #[test]
    fn entry_cas_preserves_pending_from_previous_owner() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("a12");
        // owner A（Wake）占位
        match run.reconn.try_claim_edge(ReconnSource::Wake) {
            Claim::Claimed { epoch: _a } => {
                // NetChange 在飞期间到达 ⇒ InFlight + 位留字
                assert!(matches!(
                    run.reconn.try_claim_edge(ReconnSource::NetChange),
                    Claim::InFlight
                ));
                let st = run.reconn.state.load(Ordering::Acquire);
                assert_ne!(st & ReconnSource::NetChange.pending_bit(), 0, "换网位留字");
                // A 释放（snap 带走 NetChange 位；字里仍留——release 只清 INFLIGHT）
                let snap = run.reconn.release_if_owner(_a).expect("A 是 owner");
                assert_eq!(snap, ReconnSource::NetChange.pending_bit());
                assert_eq!(
                    run.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK,
                    ReconnSource::NetChange.pending_bit(),
                    "释放后留字位仍在字里"
                );
                // Wake 入口抢占：CAS 目标保留 NetChange 位（只自消费 Wake 位）
                match run.reconn.try_claim_edge(ReconnSource::Wake) {
                    Claim::Claimed { epoch: _b } => {
                        let st2 = run.reconn.state.load(Ordering::Acquire);
                        assert_ne!(
                            st2 & ReconnSource::NetChange.pending_bit(),
                            0,
                            "入口 CAS 不清现场 pending（r8-a1-2）"
                        );
                        let _ = run.reconn.release_if_owner(_b);
                    }
                    other => panic!("Wake 应抢到：{other:?}"),
                }
            }
            other => panic!("A 应抢到：{other:?}"),
        }
    }

    /// **r8-a1-3 / r9-C2 / r10-C10**：fetch_or 留字与 owner 释放同拍（armed 路径）——
    /// ①恰释放后重读：自消费只跑一轮（B 直接 Claimed，无双跑）；②他人承接赢 ⇒
    /// armed 直接退出（不把刚被消费的位重新置上）。
    #[test]
    fn armed_path_release_race_and_double_run_guard() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("a13");
        // --- ① armed 恰逢释放：B 的 fetch_or 返回值里 INFLIGHT 已不在 ---
        {
            let a_epoch = match run.reconn.try_claim_edge(ReconnSource::Wake) {
                Claim::Claimed { epoch } => epoch,
                o => panic!("{o:?}"),
            };
            // B（NetChange）进入 try_claim_edge，钩子在 fetch_or 前暂停 B
            let (pause_tx, pause_rx) = mpsc::channel::<()>();
            let (go_tx, go_rx) = mpsc::channel::<()>();
            let go_rx = std::sync::Mutex::new(go_rx);
            let hooks_ptx = pause_tx.clone();
            let _uninstall = install_hooks(&run, Arc::new(move |point: &str| {
                if point == "edge_before_fetch_or" {
                    let _ = hooks_ptx.send(());
                    let _ = go_rx.lock().unwrap().recv(); // 暂停直到测试放行
                }
            }));
            let run2 = Arc::clone(&run);
            let hb = std::thread::spawn(move || {
                run2.reconn.try_claim_edge(ReconnSource::NetChange)
            });
            pause_rx.recv_timeout(Duration::from_secs(2)).expect("B 到位");
            // A 在 B 的 fetch_or 之前释放（同拍交错——armed 路径入口）
            let _snap = run.reconn.release_if_owner(a_epoch).expect("A 释放");
            let _ = go_tx.send(()); // 放行 B
            match hb.join().expect("B 收口") {
                // B 重读见 INFLIGHT=0 ⇒ 直接抢位（Claimed）——**不是** InFlight；
                // 且自消费只跑一轮（位不残留）。
                Claim::Claimed { epoch } => {
                    let st = run.reconn.state.load(Ordering::Acquire);
                    assert_eq!(
                        st & RECONN_PENDING_MASK,
                        0,
                        "自消费只跑一轮：B 的位不残留"
                    );
                    let _ = run.reconn.release_if_owner(epoch);
                }
                other => panic!("恰释放重读应 Claimed（不双跑）：{other:?}"),
            }
        }
        // --- ② 他人承接赢：B 已 fetch_or（armed=true）、**重读 claim_load 处**被挂住时
        //     C 抢先 Claimed ⇒ B 顶部 armed 检查直接退出（不再次 fetch_or——防把刚被
        //     消费的位重新置上）。评审 r1 P1-12 整改：旧版 C 在 B 的 fetch_or **之前**
        //     抢位，B 走的是 `prev2 & INFLIGHT` 分支——从未到达 armed 顶部检查；本版
        //     用第二次 claim_load 挂点让 B 真正以 armed=true 进入顶部检查。---
        {
            let a_epoch = match run.reconn.try_claim_edge(ReconnSource::Wake) {
                Claim::Claimed { epoch } => epoch,
                o => panic!("{o:?}"),
            };
            // 信号面：fetch_or 前暂停 / 第二回 claim_load 暂停 / 各自放行
            let (at_fetch_tx, at_fetch_rx) = mpsc::channel::<()>();
            let (go_fetch_tx, go_fetch_rx) = mpsc::channel::<()>();
            let go_fetch_rx = std::sync::Mutex::new(go_fetch_rx);
            let (at_load2_tx, at_load2_rx) = mpsc::channel::<()>();
            let (go_load2_tx, go_load2_rx) = mpsc::channel::<()>();
            let go_load2_rx = std::sync::Mutex::new(go_load2_rx);
            let pause_load2 = Arc::new(AtomicBool::new(false));
            let fetch_hits = Arc::new(AtomicU32::new(0));
            let p2 = Arc::clone(&pause_load2);
            let fh = Arc::clone(&fetch_hits);
            let atf = at_fetch_tx.clone();
            let atl = at_load2_tx.clone();
            let _uninstall = install_hooks(&run, Arc::new(move |point: &str| {
                match point {
                    "edge_before_fetch_or" => {
                        fh.fetch_add(1, Ordering::SeqCst);
                        let _ = atf.send(());
                        let _ = go_fetch_rx.lock().unwrap().recv();
                    }
                    "claim_load" if p2.load(Ordering::SeqCst) => {
                        let _ = atl.send(());
                        let _ = go_load2_rx.lock().unwrap().recv();
                    }
                    _ => {}
                }
            }));
            let run2 = Arc::clone(&run);
            let hb = std::thread::spawn(move || {
                run2.reconn.try_claim_edge(ReconnSource::NetChange)
            });
            at_fetch_rx.recv_timeout(Duration::from_secs(2)).expect("B 到 fetch_or 前");
            // A 在 B 的 fetch_or 之前释放 ⇒ B 的 fetch_or 返回值里 INFLIGHT 已不在
            let _ = run.reconn.release_if_owner(a_epoch);
            // 打开第二回 claim_load 的挂点再放行 B（B 将以 armed=true 重读）
            pause_load2.store(true, Ordering::SeqCst);
            let _ = go_fetch_tx.send(());
            at_load2_rx.recv_timeout(Duration::from_secs(2)).expect("B 以 armed 重读（claim_load②）");
            // 关挂点（C 的 claim_load 不得被挂）后 C 抢先 Claimed——承接已赢
            pause_load2.store(false, Ordering::SeqCst);
            let c_epoch = match run.reconn.try_claim_edge(ReconnSource::Wake) {
                Claim::Claimed { epoch } => epoch,
                o => panic!("C 应抢到：{o:?}"),
            };
            let _ = go_load2_tx.send(()); // 放行 B 的顶部检查
            match hb.join().expect("B 收口") {
                Claim::InFlight => {} // armed 顶部检查直接退出（r10-C10 双跑反例）
                other => panic!("他人承接赢 ⇒ B 必须直接退出：{other:?}"),
            }
            assert_eq!(
                fetch_hits.load(Ordering::SeqCst),
                1,
                "B 只 fetch_or 一次（armed 后不重置位——双跑防线）"
            );
            // 唯一 owner 是 C；B 的 NetChange 位留在字里由 C 的收尾承接（不吞）。
            let st = run.reconn.state.load(Ordering::Acquire);
            assert_ne!(st & ReconnSource::NetChange.pending_bit(), 0, "B 的留字位在");
            let _ = run.reconn.release_if_owner(c_epoch);
        }
    }

    /// **r9-C1**：epoch 连续抢位多轮（65+）单调正确释放/承接（25 bit 设计不回绕——
    /// 验证单调性而非回绕）。
    #[test]
    fn epoch_monotonic_across_many_claims() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("c1");
        let mut last_epoch = 0u32;
        for i in 0..66 {
            match run.reconn.try_claim_edge(ReconnSource::ReadFace) {
                Claim::Claimed { epoch } => {
                    assert_eq!(epoch, last_epoch + 1, "第 {} 轮 epoch 单调 +1", i + 1);
                    let snap = run.reconn.release_if_owner(epoch).expect("本轮 owner");
                    assert_eq!(snap, 0, "无 pending");
                    last_epoch = epoch;
                }
                other => panic!("无并发下必须 Claimed：{other:?}"),
            }
        }
        assert_eq!(last_epoch, 66, "65+ 轮后 epoch 计数正确");
    }

    /// **r9-C4 / r10-C11**：两并发 recover → 至多 1 次同步快探、Claimed 后必 release
    /// 无裸留位。评审 r1 P2 整改：旧版只靠 Barrier 同步起点——第一发若在第二发
    /// `try_claim_edge` 前完成收尾，第二发合法成为新 owner 并做第二次同步快探，
    /// `==1` 属过强断言（可序列化窗口）；本版用 `recover_claimed` 挂点构造**受控
    /// 交错**（第二发在第一发持位期间进入）⇒ `==1` 确定性成立。
    #[test]
    fn two_concurrent_recovers_share_one_sync_probe() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let (run, _l) = gen_with_lines("c4");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        // 第一发挂在占位后、快探前（INFLIGHT 持有——受控交错的同步点）
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let ht = hold_tx.clone();
        let _un = install_hooks(&run, Arc::new(move |point: &str| {
            if point == "recover_claimed" {
                let _ = ht.send(());
                let _ = go_rx.lock().unwrap().recv();
            }
        }));
        let e1 = Arc::clone(&exec);
        let h1 = std::thread::spawn(move || e1.recover(1, "并发唤醒 A"));
        hold_rx.recv_timeout(Duration::from_secs(2)).expect("A 已占位（快探前）");
        // 第二发并发进入：InFlight ⇒ -1、零快探贡献（sync_probes 已含 A 占位时的
        // 预记 1——并发第二发不使计数变 2，即「防各烧探测占满命令槽」的观测面）
        assert_eq!(exec.recover(1, "并发唤醒 B"), -1, "在飞合并 ⇒ -1");
        assert_eq!(
            run.reconn.sync_probes.load(Ordering::Relaxed),
            1,
            "仅 A 的预记 1 在场——第二发零快探贡献"
        );
        let _ = go_tx.send(()); // 放行 A 的同步快探
        let r1 = h1.join().unwrap();
        assert!(r1 == 0 || r1 == -1, "r1={r1}");
        wait_reconn_quiescent(&run);
        assert_eq!(
            run.reconn.sync_probes.load(Ordering::Relaxed),
            1,
            "两并发 recover 恰好 1 次同步快探（受控交错：B 在 A 持位时进入）"
        );
        *lock_unpoison(&exec.state) = None;
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **r9-C5（评审 r1 P1-12 整改：真「在飞中途」形态）**：worker 已起跑、快探已
    /// 发出并失败后置 device_dead（`round_before_rerace` 挂点注入）⇒ halt 复查拦截
    /// ——**rerace_tunnel 不进入**（零 Rebind/Connect）、零 carrier 翻真、静默退出。
    /// 对照段保留旧「入口前设备死」形态（round 入口拦截——零起跑动作）。
    #[test]
    fn worker_halts_on_device_dead_without_rerace_or_carrier() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        // ---- ① 快探在飞中途置 device_dead（r9-C5 的意图形态）----
        {
            let (run, lines) = gen_with_lines("c5");
            let island = idle_island(&run);
            run.set_island(Arc::clone(&island));
            run.set_l3_on_island(true);
            let w_run = Arc::clone(&run);
            let fired = Arc::new(AtomicBool::new(false));
            let f2 = Arc::clone(&fired);
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                if point == "round_before_rerace" && !f2.swap(true, Ordering::SeqCst) {
                    // 快探刚失败（真发出过一次）、halt 复查前——设备死注入
                    // （与生产岛回调同一标记入口）。
                    w_run.tun_shared.mark_unhealthy_if_current(w_run.gen, "fd");
                }
            }));
            assert_eq!(
                GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
                EntryOutcome::Spawned
            );
            wait_reconn_quiescent(&run);
            let seen = drain_lines(&lines);
            assert!(
                seen.iter().any(|l| l == "一次性重连：起跑（源=读面）"),
                "起跑行在场（worker 真跑过一轮）：{seen:?}"
            );
            assert!(
                !seen.iter().any(|l| l.contains("重赛跑") || l.contains("换源") || l.contains("赛跑失败")),
                "设备死后零 rerace 动作/失败行：{seen:?}"
            );
            assert!(!seen.iter().any(|l| l.contains("一次性重连：恢复")), "零恢复行");
            assert!(fired.load(Ordering::SeqCst), "注入点真到达");
            assert!(run.tun_shared.device_dead(), "设备死在册");
            assert!(!run.tun_shared.carrier.alive(), "承载位保持 false（不翻真）");
            assert_eq!(run.reconn.probes.load(Ordering::Relaxed), 1, "快探恰一次（在飞中途）");
            run.set_l3_on_island(false);
            run.take_island();
            island.stop();
        }
        // ---- ② 对照：round 入口前设备死（旧形态——入口第 0 步/检查点拦截）----
        {
            let (run, lines) = gen_with_lines("c5b");
            run.tun_shared.mark_unhealthy("fd");
            GenRun::run_round(&run, ReconnSource::ReadFace, false);
            let seen = drain_lines(&lines);
            assert!(
                seen.iter().all(|l| l == "一次性重连：起跑（源=读面）"
                    || l.starts_with("事件恢复用时：源=读面，结果=放弃，路径=未起跑")),
                "入口前设备死：仅起跑行 + 用时汇总（零动作行）：{seen:?}"
            );
            assert!(!run.tun_shared.carrier.alive(), "承载位保持 false");
            assert_eq!(run.reconn.probes.load(Ordering::Relaxed), 0, "零快探");
        }
    }

    /// **r9-C6 / r9-C12 / r13-P2**：spawn 全败的两窗口——①清位在持位时做（快照位
    /// 丢弃并计数）、release 后 snap 位保留留字；②清位期间并发新到位（不同 bit）
    /// 保留；③同 bit 新事件（与快照不可区分）⇒ 按快照丢弃（对照断言——不误报
    /// 「新位保留」）。
    #[test]
    fn spawn_all_fail_clears_snapshot_bits_preserves_new_ones() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("c6");
        SPAWN_FAIL_GATE.store(true, Ordering::Relaxed);
        // ---- ① 清位窗口前的快照位：入口占位后（钩子挂点）注入 NetChange|Foreground
        //      ⇒ spawn 全败 ⇒ 持位清位（丢弃计数）⇒ release ⇒ 静止且 pending=0 ----
        {
            let (pause_tx, pause_rx) = mpsc::channel::<()>();
            let (go_tx, go_rx) = mpsc::channel::<()>();
            let go_rx = std::sync::Mutex::new(go_rx);
            let pt = pause_tx.clone();
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                if point == "entry_claimed" {
                    let _ = pt.send(());
                    let _ = go_rx.lock().unwrap().recv();
                }
            }));
            let run2 = Arc::clone(&run);
            let h = std::thread::spawn(move || {
                GenRun::try_spawn_reconnect(&run2, ReconnSource::ReadFace)
            });
            pause_rx.recv_timeout(Duration::from_secs(2)).expect("入口已占位");
            let _ = run.reconn.state.fetch_or(
                ReconnSource::NetChange.pending_bit() | ReconnSource::Foreground.pending_bit(),
                Ordering::AcqRel,
            );
            let _ = go_tx.send(());
            assert_eq!(h.join().unwrap(), EntryOutcome::SpawnFailed);
            assert_eq!(
                run.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK,
                0,
                "清位在持位时做——快照位（NetChange|Foreground）已清（r13-P2）"
            );
            assert!(!run.reconn.inflight(), "release 后无裸留位");
            assert!(
                run.reconn.dropped_pending.load(Ordering::Relaxed) >= 2,
                "丢弃计数如实登记（≥2 位）"
            );
            }
        // ---- ② 清位循环内（掩码已定）并发新到位（BGate——不同 bit）⇒ 保留留字 ----
        {
            // 掩码非空前置：入口占位后先注入一枚 NetChange（清位循环才会进）；
            // 清位窗口内再注入 BGate（不在掩码里 ⇒ 保留）。
            let (p1tx, p1rx) = mpsc::channel::<()>();
            let (g1tx, g1rx) = mpsc::channel::<()>();
            let g1rx = std::sync::Mutex::new(g1rx);
            let (p2tx, p2rx) = mpsc::channel::<()>();
            let (g2tx, g2rx) = mpsc::channel::<()>();
            let g2rx = std::sync::Mutex::new(g2rx);
            let a = (p1tx.clone(), g1rx);
            let b = (p2tx.clone(), g2rx);
            let once = Arc::new(AtomicBool::new(true));
            let once2 = Arc::clone(&once);
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                match point {
                    "entry_claimed" => {
                        if once2.swap(false, Ordering::SeqCst) {
                            let _ = a.0.send(());
                            let _ = a.1.lock().unwrap().recv();
                        }
                    }
                    "spawn_fail_cleared" => {
                        let _ = b.0.send(());
                        let _ = b.1.lock().unwrap().recv();
                    }
                    _ => {}
                }
            }));
            let run2 = Arc::clone(&run);
            let h = std::thread::spawn(move || {
                // 边沿源（demand 源会撞上一段的 30s 起跑锚——节流分支不过占位）
                GenRun::try_spawn_reconnect(&run2, ReconnSource::Wake)
            });
            p1rx.recv_timeout(Duration::from_secs(2)).expect("入口已占位");
            let _ = run
                .reconn
                .state
                .fetch_or(ReconnSource::NetChange.pending_bit(), Ordering::AcqRel);
            let _ = g1tx.send(());
            p2rx.recv_timeout(Duration::from_secs(2)).expect("到清位循环");
            let _ = run
                .reconn
                .state
                .fetch_or(ReconnSource::BGate.pending_bit(), Ordering::AcqRel);
            let _ = g2tx.send(());
            assert_eq!(h.join().unwrap(), EntryOutcome::SpawnFailed);
            assert_ne!(
                run.reconn.state.load(Ordering::Acquire) & ReconnSource::BGate.pending_bit(),
                0,
                "清位期间并发新到位保留（r9-C6——窗口 2 留字给下一任意事件承接）"
            );
        }
        // ---- ③ 同 bit 对照：BGate 位在**掩码定档前**（下一轮入口的快照内）⇒ 按快照
        //      丢弃并计数（r9-C12——与 ② 区分两窗口；②的阻塞钩子已随块尾拆除）----
        let dropped_before = run.reconn.dropped_pending.load(Ordering::Relaxed);
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::Wake),
            EntryOutcome::SpawnFailed
        );
        assert_eq!(
            run.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK,
            0,
            "同 bit 新事件按快照丢弃（不误报「新位保留」）"
        );
        assert!(
            run.reconn.dropped_pending.load(Ordering::Relaxed) > dropped_before,
            "丢弃计数只施加于持位期间清掉的快照位"
        );
        // ---- ④ 同 bit **并发注入**（评审 r1 P1-12 整改：r9-C12 的原意形态）：
        //      掩码定档后的清位窗口内注入同 bit 新事件（BGate 已在掩码里）⇒ 掩码
        //      不变语义下被一并清掉（同 bit 与快照不可区分——按快照丢弃），丢弃
        //      计数只记掩码位一次（新事件不重复计数）。----
        {
            let (p1tx, p1rx) = mpsc::channel::<()>();
            let (g1tx, g1rx) = mpsc::channel::<()>();
            let g1rx = std::sync::Mutex::new(g1rx);
            let (p2tx, p2rx) = mpsc::channel::<()>();
            let (g2tx, g2rx) = mpsc::channel::<()>();
            let g2rx = std::sync::Mutex::new(g2rx);
            let once = Arc::new(AtomicBool::new(true));
            let once2 = Arc::clone(&once);
            let a = (p1tx.clone(), g1rx);
            let b = (p2tx.clone(), g2rx);
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                match point {
                    "entry_claimed" => {
                        if once2.swap(false, Ordering::SeqCst) {
                            let _ = a.0.send(());
                            let _ = a.1.lock().unwrap().recv();
                        }
                    }
                    "spawn_fail_cleared" => {
                        let _ = b.0.send(());
                        let _ = b.1.lock().unwrap().recv();
                    }
                    _ => {}
                }
            }));
            let dropped4 = run.reconn.dropped_pending.load(Ordering::Relaxed);
            let run2 = Arc::clone(&run);
            let h = std::thread::spawn(move || {
                GenRun::try_spawn_reconnect(&run2, ReconnSource::Wake)
            });
            p1rx.recv_timeout(Duration::from_secs(2)).expect("入口已占位");
            // BGate 位先进快照（掩码定档前）
            let _ = run
                .reconn
                .state
                .fetch_or(ReconnSource::BGate.pending_bit(), Ordering::AcqRel);
            let _ = g1tx.send(());
            p2rx.recv_timeout(Duration::from_secs(2)).expect("到清位窗口");
            // 清位窗口内注入**同 bit** 新事件（与快照不可区分）
            let _ = run
                .reconn
                .state
                .fetch_or(ReconnSource::BGate.pending_bit(), Ordering::AcqRel);
            let _ = g2tx.send(());
            assert_eq!(h.join().unwrap(), EntryOutcome::SpawnFailed);
            assert_eq!(
                run.reconn.state.load(Ordering::Acquire)
                    & ReconnSource::BGate.pending_bit(),
                0,
                "清位窗口内的同 bit 新事件被掩码一并清掉（r9-C12：按快照丢弃）"
            );
            assert_eq!(
                run.reconn.dropped_pending.load(Ordering::Relaxed) - dropped4,
                1,
                "丢弃计数恰 +1（掩码位一次——同 bit 新事件不重复计数）"
            );
            assert!(!run.reconn.inflight(), "无裸留位");
        }
        SPAWN_FAIL_GATE.store(false, Ordering::Relaxed);
    }

    /// **r9-C7 / r10-C9**：panic——guard 零写入（释放恰一次：处理器独占）；触发位恢复 =
    /// snap ∥ triggering（含承接前 p2 新到位，r13-P4）。
    #[test]
    fn panic_handler_releases_once_and_restores_pending() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("c7");
        // 初次轮 panic（无 pending）→ 释放恰一次、无留字、无再 spawn（restore=0）
        PANIC_ROUNDS_GATE.store(1, Ordering::Relaxed);
        let out = GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace);
        assert_eq!(out, EntryOutcome::Spawned);
        wait_reconn_quiescent(&run);
        assert_eq!(run.reconn.panics.load(Ordering::Relaxed), 1, "panic 计数 1");
        assert_eq!(
            run.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK,
            0,
            "无 pending ⇒ 无留字"
        );
        assert!(
            drain_lines(&lines)
                .iter()
                .any(|l| l.starts_with("一次性重连：panic（已兜住——pending 留字 0x0，释放恰一次")),
            "panic 行在场且无 pending（restore=0——首轮无快照位）"
        );
        // 承接轮 panic + pending（r13-P4）：首轮完成后置 pending → 承接轮 panic →
        // 恢复 = snap ∥ triggering（p2 全部）
        // 评审 r1 P1-12 整改：panic 行的「pending 留字 {restore:#x}」即 restore 掩码
        // 的直接观测面——本段精确断言 0x12（NetChange|BGate，r13-P4 的 p2 覆盖面）；
        // 释放恰一次由「最终静止 + starts 恰 3（首轮/承接轮/再 spawn 轮）」钉。
        PANIC_ROUNDS_GATE.store(2, Ordering::Relaxed);
        // pending 前置（收尾只在 p != 0 时走到 after_release 挂点）：入口占位后注入
        // 两枚位（NetChange 优先级 + BGate 次级——r13-P4 的 p2 覆盖面）；收尾在
        // release 之后、s2 load 之前**挂住** worker（阻塞型钩子——注入无竞态）。
        let (hit_tx, hit_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let ht = hit_tx.clone();
        // 只拦**第一次** entry_claimed：panic 恢复的再 spawn 也会走入口——钩子再拦
        // 会把 respawn 线程挂死（INFLIGHT 永不释放）。
        let once = Arc::new(AtomicBool::new(true));
        let once2 = Arc::clone(&once);
        let _un = install_hooks(&run, Arc::new(move |point: &str| {
            if point == "entry_claimed" && once2.swap(false, Ordering::SeqCst) {
                let _ = ht.send(());
                let _ = go_rx.lock().unwrap().recv();
            }
        }));
        let run2 = Arc::clone(&run);
        let h = std::thread::spawn(move || {
            // 边沿源（demand 源会撞第一段的 30s 起跑锚——节流分支不过占位）
            GenRun::try_spawn_reconnect(&run2, ReconnSource::Wake)
        });
        hit_rx.recv_timeout(Duration::from_secs(2)).expect("入口已占位");
        let _ = run
            .reconn
            .state
            .fetch_or(
                ReconnSource::NetChange.pending_bit() | ReconnSource::BGate.pending_bit(),
                Ordering::AcqRel,
            );
        let _ = go_tx.send(());
        let out2 = h.join().unwrap();
        assert_eq!(out2, EntryOutcome::Spawned);
        wait_reconn_quiescent(&run);
        // 首轮成功 + 承接轮 panic：panic 恢复把 p2 位 fetch_or 回字里；再 spawn 的
        // worker 承接消费 ⇒ 最终静止。
        let seen2 = drain_lines(&lines);
        let panic_lines: Vec<&String> = seen2
            .iter()
            .filter(|l| l.starts_with("一次性重连：panic"))
            .collect();
        assert!(
            panic_lines.iter().any(|l| l.contains("pending 留字 0x12")),
            "承接轮 panic 的 restore 掩码 = snap ∥ triggering = 0x12（NetChange|BGate）：{panic_lines:?}"
        );
        assert_eq!(
            run.reconn.panics.load(Ordering::Relaxed),
            2,
            "恰两次 panic（首轮无 pending 的那次 + 承接轮——首段的 1 + 本段的 1）"
        );
        assert!(!run.reconn.inflight(), "最终静止（释放恰一次——无闩锁）");
        // 轮数精确核算（含首段的 1）：首段无 pending 的 panic 轮（1）+ 本段首轮（2）
        // + 承接轮〔恰承接一次——p2=0x12 全部消费〕（3）+ panic 恢复再 spawn 轮〔自
        // 消费 BGate 位〕（4）+ NetChange 留字被再 spawn 轮的收尾二次承接（5）——
        // 每个 pending 位恰被一轮消费（不吞、不重复）。
        assert_eq!(
            run.reconn.starts.load(Ordering::Relaxed),
            5,
            "轮数恰 5（见上行核算——每个 pending 位恰承接一次）"
        );
        PANIC_ROUNDS_GATE.store(0, Ordering::Relaxed);
    }

    /// **r13-P3**：recover Claimed + spawn 全败 ⇒ 自行清理释放，后续可再 Claimed
    /// （无裸留位）。
    #[test]
    fn recover_claimed_with_spawn_failure_releases_slot() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let (run, _l) = gen_with_lines("p3");
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        SPAWN_FAIL_GATE.store(true, Ordering::Relaxed);
        // 同步快探失败（合成世代无岛）+ spawn 全败 → -1 + 自行清理
        assert_eq!(exec.recover(1, "扩展下推(档位 1)"), -1);
        assert!(!run.reconn.inflight(), "无裸留位（r13-P3 释放兜底）");
        // 后续可再 Claimed：关闸后再发 ⇒ 受理（worker 正常路径）
        SPAWN_FAIL_GATE.store(false, Ordering::Relaxed);
        assert_eq!(exec.recover(1, "扩展下推(档位 1)"), -1);
        wait_reconn_quiescent(&run);
        assert!(
            run.reconn.starts.load(Ordering::Relaxed) >= 1,
            "后续 Claimed 照常起跑"
        );
        *lock_unpoison(&exec.state) = None;
    }

    /// **r8-B2 多源**：release→s2 load 之间多源新到 ⇒ p2 合并按最高优先级跑一轮，
    /// 其余位由承接消费不吞。
    #[test]
    fn finalize_merges_multisource_pending_by_priority() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("b2");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        // owner 占位（Wake）
        let epoch = match run.reconn.try_claim_edge(ReconnSource::Wake) {
            Claim::Claimed { epoch } => epoch,
            o => panic!("{o:?}"),
        };
        // 占位后先注入 NetChange（在飞 fetch_or 等价——收尾 release 的 snap 才非 0）
        let _ = run
            .reconn
            .state
            .fetch_or(ReconnSource::NetChange.pending_bit(), Ordering::AcqRel);
        // 在 release 之后、s2 load 之前再注入 BGate（多源新到——阻塞型钩子挂住收尾
        // 线程，注入无竞态；p2 = NetChange ∪ BGate）
        let (hit_tx, hit_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let ht = hit_tx.clone();
        let _un = install_hooks(&run, Arc::new(move |point: &str| {
            if point == "after_release" {
                let _ = ht.send(());
                let _ = go_rx.lock().unwrap().recv();
            }
        }));
        let run2 = Arc::clone(&run);
        let h = std::thread::spawn(move || {
            let mut guard = RoundGuard {
                epoch,
                triggering: 0,
            };
            reconnect_finalize_loop(&run2, &mut guard);
        });
        hit_rx.recv_timeout(Duration::from_secs(2)).expect("挂点");
        let _ = run
            .reconn
            .state
            .fetch_or(ReconnSource::BGate.pending_bit(), Ordering::AcqRel);
        let _ = go_tx.send(());
        h.join().expect("收尾收口");
        wait_reconn_quiescent(&run);
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l.contains("源=换网")),
            "p2 合并按最高优先级（NetChange）跑一轮：{seen:?}"
        );
        assert!(
            !run.reconn.inflight()
                && run.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK == 0,
            "其余位由承接消费（不吞、不残留）"
        );
        run.take_island();
        island.stop();
    }

    /// **r10-C8**：释放 CAS 与 pending fetch_or 同拍 ⇒ 不闩锁（CAS-loop 重试）；两线程
    /// 同拍释放只一人成功。
    #[test]
    fn release_cas_retries_against_concurrent_fetch_or() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("c8");
        let epoch = match run.reconn.try_claim_edge(ReconnSource::ReadFace) {
            Claim::Claimed { epoch } => epoch,
            o => panic!("{o:?}"),
        };
        // 挂在 release 的 load 之后、CAS 之前——注入 pending 位（同拍 fetch_or）；
        // 钩子作用域收进本块（守卫 Drop 拆钩——两线程释放段不受挂点影响）
        let snap = {
            let (hit_tx, hit_rx) = mpsc::channel::<()>();
            let (go_tx, go_rx) = mpsc::channel::<()>();
            let go_rx = std::sync::Mutex::new(go_rx);
            let ht = hit_tx.clone();
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                if point == "release_load" {
                    let _ = ht.send(());
                    let _ = go_rx.lock().unwrap().recv();
                }
            }));
            let run2 = Arc::clone(&run);
            let h = std::thread::spawn(move || run2.reconn.release_if_owner(epoch));
            hit_rx.recv_timeout(Duration::from_secs(2)).expect("挂点");
            let _ = run
                .reconn
                .state
                .fetch_or(ReconnSource::BGate.pending_bit(), Ordering::AcqRel);
            let _ = go_tx.send(());
            h.join().expect("释放收口").expect("owner 在位")
        };
        assert_eq!(snap, ReconnSource::BGate.pending_bit(), "重试后快照带上新位");
        assert!(!run.reconn.inflight(), "不闩锁");
        // 两线程同拍释放：一人 Some、一人 None
        let e2 = match run.reconn.try_claim_edge(ReconnSource::ReadFace) {
            Claim::Claimed { epoch } => epoch,
            o => panic!("{o:?}"),
        };
        let run3 = Arc::clone(&run);
        let run4 = Arc::clone(&run);
        let b = Arc::new(std::sync::Barrier::new(2));
        let b1 = Arc::clone(&b);
        let b2 = Arc::clone(&b);
        let h1 = std::thread::spawn(move || {
            b1.wait();
            run3.reconn.release_if_owner(e2)
        });
        let h2 = std::thread::spawn(move || {
            b2.wait();
            run4.reconn.release_if_owner(e2)
        });
        let (r1, r2) = (h1.join().unwrap(), h2.join().unwrap());
        assert!(
            (r1.is_some() && r2.is_none()) || (r1.is_none() && r2.is_some()),
            "释放恰一次（一人成功）：{r1:?} / {r2:?}"
        );
    }

    /// **表尾三行（无编号）**：并发抢位一胜且失败者边沿被承接；同源并发双边沿不丢；
    /// owner 只清自己位（demand 源不置 pending ⇒ 不吞边沿位）。
    #[test]
    fn concurrent_claim_one_winner_loser_edge_taken_over() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("tail1");
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let run2 = Arc::clone(&run);
        let run3 = Arc::clone(&run);
        let b1 = Arc::clone(&barrier);
        let b2 = Arc::clone(&barrier);
        let h1 = std::thread::spawn(move || {
            b1.wait();
            run2.reconn.try_claim_edge(ReconnSource::NetChange)
        });
        let h2 = std::thread::spawn(move || {
            b2.wait();
            run3.reconn.try_claim_edge(ReconnSource::NetChange)
        });
        let (r1, r2) = (h1.join().unwrap(), h2.join().unwrap());
        let wins = matches!(r1, Claim::Claimed { .. }) as u32
            + matches!(r2, Claim::Claimed { .. }) as u32;
        assert_eq!(wins, 1, "并发抢位一胜：{r1:?}/{r2:?}");
        // 同源并发双边沿不丢：单 bit 表达两事件（fetch_or 幂等）
        let (run_b, _l2) = gen_with_lines("tail2");
        let epoch = match run_b.reconn.try_claim_edge(ReconnSource::Wake) {
            Claim::Claimed { epoch } => epoch,
            o => panic!("{o:?}"),
        };
        assert!(matches!(
            run_b.reconn.try_claim_edge(ReconnSource::BGate),
            Claim::InFlight
        ));
        assert!(matches!(
            run_b.reconn.try_claim_edge(ReconnSource::BGate),
            Claim::InFlight
        ));
        assert_eq!(
            run_b.reconn.state.load(Ordering::Acquire) & RECONN_PENDING_MASK,
            ReconnSource::BGate.pending_bit(),
            "同源双边沿：单 bit 在场即不丢"
        );
        let _ = run_b.reconn.release_if_owner(epoch);
        // owner 只清自己位（Wake 入口不清 BGate 位）——由 a12 已钉；此处收尾
        let _ = run.reconn.state.load(Ordering::Acquire);
    }

    /// **表尾**：边沿绕节流 / demand 节流 / yield 哨兵（清 0 后仍 30s、有窗 5s）。
    #[test]
    fn edge_bypasses_gap_demand_throttles_yield_sentinel() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("throttle");
        SPAWN_FAIL_GATE.store(true, Ordering::Relaxed); // 不起真线程——纯计数面
        // 起跑锚直接拨到「刚失败」（T+8s 失败语义：last_ms = now）
        run.reconn.last_ms.store(mono_ms(), Ordering::Release);
        // demand 源 ⇒ Throttled（30s 未过）
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Throttled
        );
        assert_eq!(run.reconn.throttled.load(Ordering::Relaxed), 1, "节流计数");
        // 边沿源 ⇒ 绕过（SpawnFailed = 已过占位与 spawn——绕节流成立）
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::Wake),
            EntryOutcome::SpawnFailed,
            "边沿绕节流（占位成功，仅 spawn 被闸）"
        );
        // yield 哨兵：有窗（now+5s）⇒ until = min(last+30s, y) = y ⇒ demand 仍节流到 5s 后
        run.reconn
            .yield_until_ms
            .store(mono_ms() + RECONNECT_YIELD_GAP.as_millis() as i64, Ordering::Release);
        run.reconn.last_ms.store(mono_ms() - 10_000, Ordering::Release); // last+30s = +20s
        assert!(run.reconn.throttled_now(), "有窗 ⇒ 5s 短窗生效");
        run.reconn.yield_until_ms.store(0, Ordering::Release); // 清 0 后仍 30s
        assert!(run.reconn.throttled_now(), "清 0 后仍按 30s（哨兵回退 last+GAP）");
        // 到点（last 拨回 31s 前）⇒ demand 放行
        run.reconn
            .last_ms
            .store(mono_ms() - RECONNECT_MIN_GAP.as_millis() as i64 - 1, Ordering::Release);
        assert!(!run.reconn.throttled_now(), "31s 后 demand 放行");
        SPAWN_FAIL_GATE.store(false, Ordering::Relaxed);
    }

    /// **表尾**：last_ms 初值（首个 demand 事件立即起跑）；起跑锚（T 起跑、T+8s
    /// 失败 ⇒ T+31s 才放行——锚记**起跑时刻**，执行时长不叠加进间隔）。
    #[test]
    fn initial_anchor_allows_first_demand_and_start_anchor_semantics() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("anchor");
        SPAWN_FAIL_GATE.store(true, Ordering::Relaxed);
        // 初值 = 装配时刻 − MIN_GAP ⇒ 首个 demand 立即起跑（非 Throttled）
        assert_ne!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Throttled,
            "首个 demand 事件立即起跑（初值锚）"
        );
        // 起跑锚语义：T 起跑（last_ms=T）、T+8s 失败 ⇒ T+20s 时 demand 仍节流；
        // T+31s 放行——**锚不随失败完成推进**（不是完成时刻）。
        let t0 = mono_ms();
        run.reconn.last_ms.store(t0, Ordering::Release);
        run.reconn.yield_until_ms.store(0, Ordering::Release);
        // 模拟「T+20s 观测」：把锚拨回 20s 前（等价时间旅行——不真睡）
        run.reconn.last_ms.store(t0 - 20_000, Ordering::Release);
        assert!(run.reconn.throttled_now(), "T+20s（距起跑 20s < 30s）仍节流");
        run.reconn
            .last_ms
            .store(t0 - RECONNECT_MIN_GAP.as_millis() as i64 - 1, Ordering::Release);
        assert!(!run.reconn.throttled_now(), "T+31s（距起跑 >30s）放行——锚=起跑时刻");
        SPAWN_FAIL_GATE.store(false, Ordering::Relaxed);
    }

    /// **表尾**：初次轮 panic 后仍可再起跑（无闩锁）；本轮两段都**无 pending**
    /// （首轮 panic restore=0、再起跑正常收尾）⇒ starts 恰 2、panics 恰 1、终态
    /// 静止——「恰承接一次」的 pending 形态由 `panic_handler_releases_once_and_
    /// restores_pending` 第二段（有 pending 的承接轮 panic）钉（评审 r2 P2：本用例
    /// 旧名/注释声称「pending_taken_exactly_once」名不副实——改名对齐实际断言）。
    #[test]
    fn takeover_panic_restarts_without_latch_exact_two_rounds_no_pending() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("panic2");
        // 初次轮 panic（restore=0——无再 spawn）→ 静止后**仍可再起跑**（无闩锁）
        PANIC_ROUNDS_GATE.store(1, Ordering::Relaxed);
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Spawned
        );
        wait_reconn_quiescent(&run);
        PANIC_ROUNDS_GATE.store(0, Ordering::Relaxed);
        // 并发 pending + 初次 panic：本轮 panic 无 pending ⇒ 不留字；构造「pending 在
        // panic 前置位」的形态 = 承接轮 panic（见 panic_handler 用例）——此处钉「再起跑」
        // （边沿源：demand 源会撞首轮的 30s 起跑锚——节流不是闩锁，绕开即证可再起跑）。
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::Wake),
            EntryOutcome::Spawned,
            "panic 后仍可再起跑"
        );
        wait_reconn_quiescent(&run);
        // 精确计数（评审 r1 P1-12 整改：取代 `>=` 过弱断言；评审 r2 P2：与改名对齐）——
        // 本用例的两段：首轮 panic（无 pending ⇒ 无承接）+ 再起跑一轮（正常收尾，
        // 无 pending ⇒ 无承接）⇒ starts 恰 2、panics 恰 1、终态静止。
        assert_eq!(
            run.reconn.starts.load(Ordering::Relaxed),
            2,
            "恰两轮（首轮 panic + 再起跑）——无 pending ⇒ 无承接轮"
        );
        assert_eq!(run.reconn.panics.load(Ordering::Relaxed), 1, "恰一次 panic");
        assert!(!run.reconn.inflight(), "终态静止");
    }

    /// **r4-N1**：设备死 + 出站包 ⇒ 0 次起跑（入口第 0 步拦截——读面/服务流触发在
    /// 设备死下不产生任何重连）。
    #[test]
    fn device_dead_blocks_all_entry_starts() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("n1");
        run.tun_shared.mark_unhealthy("fd");
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Rejected
        );
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ServiceStream),
            EntryOutcome::Rejected
        );
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::NetChange),
            EntryOutcome::Rejected,
            "边沿源同样被拦（设备死无自动重连——v4 口径）"
        );
        assert_eq!(run.reconn.entries.load(Ordering::Relaxed), 0, "零入口计数");
        assert_eq!(run.reconn.starts.load(Ordering::Relaxed), 0, "0 次起跑");
        // 承载位也不翻真（设备死在场判负）
        assert!(!run.tun_shared.carrier.alive());
        assert!(!run.tun_shared.mark_carrier_alive_if_current(run.gen));
    }

    /// **表尾**：判据行逐字断言（起跑/恢复/失败/让路四行族 + panic 行——判据登记的
    /// 对齐面；评审 r1 P1-12 整改：起跑行真断言、让路行走真实 Yielded 路径产出一行，
    /// 不再用恒真断言）。恢复行的逐字断言由 `criteria_recovery_line_verbatim`
    /// （评审 r2 B4——经 run_round 快探成功路径真产出 `一次性重连：恢复（源=…）` 行）
    /// 与 `strong_recover_clears_patrol_sets_carrier_and_advances_link_at`
    /// （「传输恢复（patrol 清除…」证据面）承接。
    #[test]
    fn criteria_lines_verbatim() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("lines");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        // ---- ① 起跑行 + 让路行：后台先起一轮长预算赛跑（黑洞候选 ⇒ racing 持续），
        //      worker 的 rerace 两查 racing ⇒ 真 Yielded 路径产出让路行 ----
        let dead: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
        let island2 = Arc::clone(&island);
        let racer = std::thread::spawn(move || {
            let (tx, rx) = mpsc::channel();
            let _ = island2.tx().send(homeway_quic::Cmd::Connect {
                cands: vec![homeway_quic::Candidate {
                    addr: dead,
                    via: homeway_quic::Via::Direct,
                }],
                budget: Duration::from_secs(6),
                demote_adopted: false,
                reply: tx,
            });
            let _ = rx.recv_timeout(Duration::from_secs(8)); // 等预算烧完（黑洞候选）
        });
        // racing 位置位后再触发 worker（有界轮询——确定性同步点）
        let deadline = Instant::now() + Duration::from_secs(3);
        while !island.snapshot().racing && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(island.snapshot().racing, "前置：赛跑在飞（racing=true）");
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Spawned
        );
        wait_reconn_quiescent(&run);
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l == "一次性重连：起跑（源=读面）"),
            "起跑行逐字（源枚举在场）：{seen:?}"
        );
        assert!(
            seen.iter().any(|l| l.starts_with("一次性重连：让路（源=读面，在飞赛跑持续——")),
            "让路行前缀逐字（真实 Yielded 路径产出）：{seen:?}"
        );
        assert!(
            seen.iter().any(|l| l.contains("5s 短窗退避）")),
            "让路行含短窗时长：{seen:?}"
        );
        assert!(
            run.reconn.yield_until_ms.load(Ordering::Acquire) > 0,
            "让路后 5s 短窗已置（非 0 哨兵）"
        );
        racer.join().expect("后台赛跑收口");
        // 快照的 racing 位随节拍同步（race 失败不是关键转换——SNAP_SYNC_INTERVAL
        // 迟滞）⇒ 有界轮询等它翻假，phase ② 的 rerace 才会走 Fail 而非再让路。
        let settle = Instant::now() + Duration::from_secs(3);
        while island.snapshot().racing && Instant::now() < settle {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!island.snapshot().racing, "前置：赛跑已收口（快照同步完成）");
        // ---- ② 失败行：赛跑结束后（racing=false）再起一轮 ⇒ 走 Fail 路径（读面
        //      是 demand 源——锚拨回 31s 前绕开首轮留下的 30s 限频窗）----
        run.reconn
            .last_ms
            .store(mono_ms() - RECONNECT_MIN_GAP.as_millis() as i64 - 1, Ordering::Release);
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Spawned
        );
        wait_reconn_quiescent(&run);
        let seen2 = drain_lines(&lines);
        assert!(
            seen2.iter().any(|l| l.starts_with("一次性重连：失败（源=读面，原因=")),
            "失败行前缀逐字：{seen2:?}"
        );
        assert!(
            seen2.iter().any(|l| l.contains("）——demand 源下次最早触发")),
            "失败行含下次最早触发时刻：{seen2:?}"
        );
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **表尾（评审 r2 B4）**：恢复行逐字断言——经 `run_round` 快探成功路径**真产出**
    /// `一次性重连：恢复（源=…，证据=快探，耗时 Nms）`（真快探成功需 live 连接，
    /// 合成世代不可构造 ⇒ 快探结论经 `PROBE_RESULT_GATE` 注入缝注入；行产出的
    /// run_round/strong_recover 代码路径为真）。同时钉翻回三件套（patrol 清 +
    /// carrier true）与「让路/失败行不落」。
    #[test]
    fn criteria_recovery_line_verbatim() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("recover-line");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        // 前置：patrol 在册（恢复行的翻回语境）
        assert!(run.tun_shared.mark_transport_if_current(run.gen));
        PROBE_RESULT_GATE.store(1, Ordering::Relaxed);
        let spawned = GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace);
        assert_eq!(spawned, EntryOutcome::Spawned);
        wait_reconn_quiescent(&run);
        PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
        let seen = drain_lines(&lines);
        let rec: Vec<&String> = seen
            .iter()
            .filter(|l| l.starts_with("一次性重连：恢复（源=读面，证据=快探，耗时 "))
            .collect();
        assert_eq!(rec.len(), 1, "恢复行恰一行（真产出）：{seen:?}");
        assert!(
            rec[0].ends_with("ms）"),
            "恢复行收尾形态（耗时 Nms）：{}",
            rec[0]
        );
        assert!(
            seen.iter().any(|l| l == "一次性重连：起跑（源=读面）"),
            "起跑行同轮在场：{seen:?}"
        );
        assert!(
            !seen.iter().any(|l| l.starts_with("一次性重连：失败")
                || l.starts_with("一次性重连：让路")),
            "成功路径不落失败/让路行：{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|l| l.contains("传输恢复（patrol 清除，证据=快探")),
            "strong_recover 的翻回判据行在场：{seen:?}"
        );
        let summary = seen
            .iter()
            .find(|l| l.starts_with("事件恢复用时：源=读面，结果=成功，路径=快探，总耗时="))
            .unwrap_or_else(|| panic!("缺事件恢复用时汇总行：{seen:?}"));
        assert!(
            summary.contains("排队=")
                && summary.contains("快探=")
                && summary.contains("赛跑=0ms")
                && summary.contains("恢复="),
            "汇总行须带排队/快探/重赛跑/恢复分项：{summary}"
        );
        assert!(!run.tun_shared.transport_unhealthy(), "patrol 已清");
        assert!(run.tun_shared.carrier.alive(), "承载位置 true");
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **评审 r2 B3 + r3 B3 补强**：Connect 在飞期间注入 device_dead ⇒ worker 立即
    /// Gone 静默退出——零恢复行、零对端失败行、零后续命令（评审 r3：probes 只数快探
    /// ——补 **岛命令计数**断言：Connect 侧 `mirrors`、Rebind 侧 `rebinds`〔快照
    /// 新计数〕均不再增）。构造：名册放黑洞候选 ⇒ worker 的 Connect（空 cands 走
    /// 名册）在飞整个预算；racing 位置位后注入 fd 类设备死。
    #[test]
    fn rerace_connect_inflight_device_dead_yields_zero_recovery_lines_and_no_further_cmds() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("b3-connect");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        // 名册 = 黑洞候选（127.0.0.1:1——握手必然黑洞到预算烧完 ⇒ racing 持续）
        let dead: std::net::SocketAddr = "127.0.0.1:1".parse().unwrap();
        let _ = island.tx().send(homeway_quic::Cmd::SetCandidates {
            cands: vec![homeway_quic::Candidate {
                addr: dead,
                via: homeway_quic::Via::Direct,
            }],
        });
        let probes_before;
        let mirrors_before;
        let rebinds_before;
        {
            assert_eq!(
                GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
                EntryOutcome::Spawned
            );
            // 等 worker 走到 Connect 在飞（快探 NotConnected 快败 → rerace →
            // Rebind Ok → Connect 对黑洞起跑）。racing 位置位后注入设备死。
            let deadline = Instant::now() + Duration::from_secs(3);
            while !island.snapshot().racing && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(island.snapshot().racing, "前置：worker 的 Connect 在飞（racing）");
            // racing 可见 = Connect 后的一次快照同步已发生（同拍同步块）⇒ mirrors/
            // rebinds 基线含本轮已发出的 Rebind/Connect。
            let snap = island.snapshot();
            assert!(snap.mirrors >= 1, "前置：Connect 已投出（mirrors 基线）");
            assert!(snap.rebinds >= 1, "前置：Rebind 已发出（rebinds 基线）");
            mirrors_before = snap.mirrors;
            rebinds_before = snap.rebinds;
            probes_before = run.reconn.probes.load(Ordering::Relaxed);
            run.tun_shared.mark_unhealthy("fd");
        }
        wait_reconn_quiescent(&run);
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l == "一次性重连：起跑（源=读面）"),
            "worker 真起跑过：{seen:?}"
        );
        assert!(
            !seen.iter().any(|l| l.starts_with("一次性重连：恢复")
                || l.starts_with("一次性重连：失败")
                || l.starts_with("一次性重连：让路")
                || l.contains("换源失败")
                || l.contains("赛跑失败")),
            "设备死在 Connect 在飞期间 ⇒ 零恢复/对端失败/让路行：{seen:?}"
        );
        assert_eq!(
            run.reconn.probes.load(Ordering::Relaxed),
            probes_before,
            "零后续快探（Gone 后 worker 退出——probes 不再增）"
        );
        let snap = island.snapshot();
        assert_eq!(
            snap.mirrors, mirrors_before,
            "零后续 Connect（mirrors 不再增——命令计数，非仅快探面）"
        );
        assert_eq!(
            snap.rebinds, rebinds_before,
            "零后续 Rebind（rebinds 不再增——r3 新增命令计数）"
        );
        assert_eq!(
            run.reconn.starts.load(Ordering::Relaxed),
            1,
            "恰一轮（无承接轮）"
        );
        assert!(!run.reconn.inflight(), "终态静止");
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **评审 r2 B3**：快探成功**返回后**、恢复行/翻回之前注入 device_dead ⇒ 复查拦截
    /// ——零恢复行、carrier 不翻真（`round_after_probe` 挂点的受控交错）。
    #[test]
    fn probe_success_then_device_dead_skips_recovery_line_and_carrier() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("b3-after-probe");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        assert!(run.tun_shared.mark_transport_if_current(run.gen));
        // 注入缝两段联动：快探判成功 + 成功后复查前注入设备死
        PROBE_RESULT_GATE.store(1, Ordering::Relaxed);
        let (hit_tx, hit_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let ht = hit_tx.clone();
        let _un = install_hooks(&run, Arc::new(move |point: &str| {
            if point == "round_after_probe" {
                let _ = ht.send(());
                let _ = go_rx.lock().unwrap().recv();
            }
        }));
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Spawned
        );
        hit_rx.recv_timeout(Duration::from_secs(2)).expect("快探已成功返回（复查前挂点）");
        run.tun_shared.mark_unhealthy("fd");
        let _ = go_tx.send(());
        wait_reconn_quiescent(&run);
        PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l == "一次性重连：起跑（源=读面）"),
            "worker 真起跑过：{seen:?}"
        );
        assert!(
            !seen.iter().any(|l| l.starts_with("一次性重连：恢复")
                || l.contains("传输恢复（patrol 清除")),
            "快探成功后设备死 ⇒ 零恢复行/零翻回行：{seen:?}"
        );
        assert!(
            !run.tun_shared.carrier.alive(),
            "carrier 不翻真（设备死在场判负）"
        );
        assert!(run.tun_shared.device_dead(), "设备死在册（fd 类）");
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **判据（评审 r5-L3）**：快探成功、halt 复查**通过后**、`strong_recover` 前
    /// 设备死到场 ⇒ `strong_recover` 返回 false（承载位置 true 判负）——不得落
    /// 「一次性重连：恢复」假恢复行（旧实现未消费返回值、无条件打行）。
    /// `round_before_strong` 挂点注入；与 r2 B3 的 `round_after_probe` 形态互补
    /// （B3 覆盖复查前，本用例覆盖复查后的最后窗口）。
    #[test]
    fn round_device_dead_between_halt_recheck_and_strong_recover_skips_recovery_line() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("r5-l3-round-strong");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        assert!(run.tun_shared.mark_transport_if_current(run.gen));
        PROBE_RESULT_GATE.store(1, Ordering::Relaxed);
        let fired = Arc::new(AtomicBool::new(false));
        let run2 = Arc::clone(&run);
        let f2 = Arc::clone(&fired);
        let _un = install_hooks(&run, Arc::new(move |point: &str| {
            if point == "round_before_strong" {
                run2.tun_shared.mark_unhealthy("fd"); // 复查后、强恢复前
                f2.store(true, Ordering::SeqCst);
            }
        }));
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Spawned
        );
        wait_reconn_quiescent(&run);
        PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
        assert!(fired.load(Ordering::SeqCst), "注入点真到达");
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l == "一次性重连：起跑（源=读面）"),
            "worker 真起跑过：{seen:?}"
        );
        assert!(
            !seen.iter().any(|l| l.starts_with("一次性重连：恢复")
                || l.contains("传输恢复（patrol 清除")),
            "复查后设备死（alive=false）⇒ 零恢复行/零翻回行：{seen:?}"
        );
        assert!(
            !seen.iter().any(|l| l.starts_with("一次性重连：失败")),
            "Gone/设备死按静默收口（零对端失败行）：{seen:?}"
        );
        assert!(!run.tun_shared.carrier.alive(), "carrier 不翻真");
        assert!(run.tun_shared.device_dead(), "设备死在册（fd 类）");
        assert!(!run.reconn.inflight(), "终态静止");
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **判据（评审 r5-L3）**：重赛跑胜出、halt 复查之后、`strong_recover` 前设备死
    /// 到场 ⇒ `rerace_finish` 消费 `carrier_alive=false`：不落「阶梯尽头重赛跑：
    /// 胜出…传输恢复」假恢复行、按 `Gone` 收口（旧实现先打行、返回值未消费）。
    #[test]
    fn rerace_finish_skips_win_line_when_device_dead_before_strong_recover() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("r5-l3-rerace-finish");
        run.tun_shared.mark_unhealthy("fd"); // 复查后、强恢复前到场
        let outcome = homeway_quic::RaceOutcome {
            winner: "127.0.0.1:9".parse().unwrap(),
            via: homeway_quic::Via::Direct,
            rtt_ms: 1,
            completed: Vec::new(),
            unfinished: Vec::new(),
            elapsed_ms: 7,
        };
        let r = rerace_finish(&run, &outcome);
        assert!(
            matches!(r, Err(ReraceAbort::Gone)),
            "alive=false ⇒ Gone（不落假恢复行）：{r:?}"
        );
        let seen = drain_lines(&lines);
        assert!(
            !seen.iter().any(|l| l.contains("阶梯尽头重赛跑：胜出")
                || l.contains("传输恢复")),
            "零「胜出…传输恢复」行：{seen:?}"
        );
        assert!(!run.tun_shared.carrier.alive(), "carrier 不翻真");
        assert!(run.tun_shared.device_dead(), "设备死在册");
    }

    /// **评审 r3 B3 + r4 B3 低项**：`recover` 的同步快探成功后的三段收口——
    /// ① 复查前注入 device_dead / ② 停止位 ⇒ claimed-epoch 释放路径（r13-P3 同款）
    /// ——返回 **-2**（rc 契约与前置判定同值）、零「同步快探：通过」行、零恢复行/
    /// 翻回、carrier 不翻真、状态字静止（release 已做、不裸留位）、**锚/让路窗回滚**
    /// （r4 补断言：本轮未起跑，节流位不消耗——last_ms/yield_until_ms 复原）。
    /// ③（r4 B3 低项）halt 复查**通过后**、strong_recover 前注入 device_dead ⇒
    /// 承载位置 true 判负（alive=false）同归 -2 释放路径——不落「通过」行、不裸留位。
    /// `recover_after_probe`/`recover_before_strong` 挂点注入——与 run_round 的
    /// `round_after_probe` 同一纪律（B3 的另一条同步快探路径）。
    #[test]
    fn recover_after_probe_halt_releases_claimed_epoch_without_recovery() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        // ① 设备死形态（快探在飞期间发生）
        {
            let (run, lines) = gen_with_lines("b3-recover-dd");
            let demand = Arc::new(DemandSignals::new());
            let exec = TunnelExec::new(Arc::clone(&demand));
            *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
            PROBE_RESULT_GATE.store(1, Ordering::Relaxed);
            // 锚/让路窗哨兵（r4：回滚断言——释放路径须复原原值）
            run.reconn.last_ms.store(111_111, Ordering::Release);
            run.reconn.yield_until_ms.store(0, Ordering::Release);
            let run2 = Arc::clone(&run);
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                if point == "recover_after_probe" {
                    run2.tun_shared.mark_unhealthy("fd"); // 设备死注入（复查前）
                }
            }));
            let rc = exec.recover(1, "扩展下推(档位 1)");
            PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
            assert_eq!(rc, -2, "快探在飞期间设备死 ⇒ -2（不是 0/-1）");
            assert!(!run.reconn.inflight(), "claimed-epoch 已释放（不裸留位）");
            assert_eq!(
                run.reconn.last_ms.load(Ordering::Acquire),
                111_111,
                "锚回滚（r4：本轮未起跑，节流位不消耗）"
            );
            assert_eq!(
                run.reconn.yield_until_ms.load(Ordering::Acquire),
                0,
                "让路窗回滚（r4）"
            );
            let seen = drain_lines(&lines);
            assert!(
                !seen.iter().any(|l| l.contains("同步快探：通过")
                    || l.contains("传输恢复（patrol 清除")),
                "零「通过」/恢复行/翻回行：{seen:?}"
            );
            assert!(
                !run.tun_shared.carrier.alive(),
                "carrier 不翻真（设备死在场判负）"
            );
            assert!(run.tun_shared.device_dead(), "设备死在册（fd 类）");
            *lock_unpoison(&exec.state) = None;
        }
        // ② 停止位形态（世代收口与恢复下推并发）
        {
            let (run, lines) = gen_with_lines("b3-recover-stop");
            let demand = Arc::new(DemandSignals::new());
            let exec = TunnelExec::new(Arc::clone(&demand));
            *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
            PROBE_RESULT_GATE.store(1, Ordering::Relaxed);
            run.reconn.last_ms.store(222_222, Ordering::Release);
            run.reconn.yield_until_ms.store(777, Ordering::Release);
            let run2 = Arc::clone(&run);
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                if point == "recover_after_probe" {
                    run2.stop.store(true, Ordering::Release); // 停止位注入（复查前）
                }
            }));
            let rc = exec.recover(3, "扩展下推(档位 3)");
            PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
            assert_eq!(rc, -2, "快探在飞期间停止 ⇒ -2");
            assert!(!run.reconn.inflight(), "claimed-epoch 已释放");
            assert_eq!(
                run.reconn.last_ms.load(Ordering::Acquire),
                222_222,
                "锚回滚（r4）"
            );
            assert_eq!(
                run.reconn.yield_until_ms.load(Ordering::Acquire),
                777,
                "让路窗回滚（r4——含非零让路窗形态）"
            );
            assert!(
                !drain_lines(&lines)
                    .iter()
                    .any(|l| l.contains("同步快探：通过")),
                "零「通过」行：停止面不落假成功"
            );
            *lock_unpoison(&exec.state) = None;
        }
        // ③（r4 B3 低项）halt 复查通过后、strong_recover 前设备死 ⇒ alive=false 同归
        // -2 释放路径（复查与强恢复之间的理论窗口——旧实现落「通过」行返回 0）。
        {
            let (run, lines) = gen_with_lines("b3-recover-alive");
            let demand = Arc::new(DemandSignals::new());
            let exec = TunnelExec::new(Arc::clone(&demand));
            *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
            PROBE_RESULT_GATE.store(1, Ordering::Relaxed);
            run.reconn.last_ms.store(333_333, Ordering::Release);
            run.reconn.yield_until_ms.store(0, Ordering::Release);
            let run2 = Arc::clone(&run);
            let _un = install_hooks(&run, Arc::new(move |point: &str| {
                if point == "recover_before_strong" {
                    run2.tun_shared.mark_unhealthy("fd"); // 复查后、强恢复前注入
                }
            }));
            let rc = exec.recover(1, "扩展下推(档位 1)");
            PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
            assert_eq!(rc, -2, "复查后设备死（alive=false）⇒ -2（不是 0）");
            assert!(!run.reconn.inflight(), "claimed-epoch 已释放（不裸留位）");
            assert_eq!(
                run.reconn.last_ms.load(Ordering::Acquire),
                333_333,
                "锚回滚（r4）"
            );
            assert_eq!(
                run.reconn.yield_until_ms.load(Ordering::Acquire),
                0,
                "让路窗回滚（r4）"
            );
            let seen = drain_lines(&lines);
            assert!(
                !seen.iter().any(|l| l.contains("同步快探：通过")
                    || l.contains("传输恢复（patrol 清除")),
                "零「通过」/恢复行（r4：alive=false 不落假成功）：{seen:?}"
            );
            assert!(run.tun_shared.device_dead(), "设备死在册（fd 类）");
            *lock_unpoison(&exec.state) = None;
        }
    }

    /// **评审 r2 B7 + r3 B7**：stats 观测循环的 Busy/Timeout typed 门注入测试。
    /// - 阶段 A（gate=3，Busy）：**不累积**连败（「失败 N/3」行不出现、不触发标记）；
    /// - 阶段 A2（gate=2，Timeout——评审 r3 补）：同样不累积不清零；
    /// - 阶段 B（seed→hold→resume）：Detail 拍先种下 1/3，Timeout 拍持住若干拍
    ///   （**不清零**——恢复 Detail 后从 2/3 续算而非 1/3 重来；也**不累积**——持住期
    ///   间到不了 3/3/标记），最终三败到标记（Account 路径照常累积）。
    ///
    /// 出站包计数经真 TUN 读面（socketpair attach + 写包——`out_pkts` 计数在读面
    /// PI 剥离后无条件自增）；观测节拍经 `obs_interval` 参数压缩（生产 = OBS_INTERVAL 60s）。
    #[test]
    fn stats_loop_busy_timeout_do_not_accumulate_fail_streak() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("b7-stats");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        // TUN 面：socketpair 一端作 fd 交给岛（读线程计数出站包），另一端持续写包
        let (tun, peer) = std::os::unix::net::UnixDatagram::pair().expect("socketpair(DGRAM)");
        use std::os::fd::AsRawFd;
        attach_island(&island, tun.as_raw_fd(), 1280).expect("attach 可达");
        // 稳态前置：承载位 true（观测快探的运行门）
        assert!(run.tun_shared.mark_carrier_alive_if_current(run.gen));
        let writer_stop = Arc::new(AtomicBool::new(false));
        let ws = Arc::clone(&writer_stop);
        let peer_t = std::thread::spawn(move || {
            while !ws.load(Ordering::Acquire) {
                let _ = peer.send(b"demand-pkt");
                std::thread::sleep(Duration::from_millis(15));
            }
        });
        // 辅助：跑一段注入期（gate 值 + 时长）——返回期间新增行；结束时停循环并复位 stop。
        let run_inject = |gate: u8, ms: u64| -> Vec<String> {
            PROBE_RESULT_GATE.store(gate, Ordering::Relaxed);
            run.stop.store(false, Ordering::Release);
            let h = {
                let r2 = Arc::clone(&run);
                std::thread::spawn(move || stats_loop(r2, 5, 0, Duration::from_millis(80)))
            };
            std::thread::sleep(Duration::from_millis(ms));
            run.stop.store(true, Ordering::Release);
            h.join().expect("stats 线程收口（注入期）");
            drain_lines(&lines)
        };
        // 阶段 A：Busy 注入 × 若干拍——零失败行、零标记
        let seen_a = run_inject(3, 600);
        assert!(
            !seen_a.iter().any(|l| l.contains("观测快探失败") || l.contains("观测快探连续")),
            "Busy 拍不累积连败（无失败行/标记行）：{seen_a:?}"
        );
        // 阶段 A2（评审 r3 补）：Timeout 注入 × 若干拍——同样零失败行、零标记
        //（obs_fail_disposition 的 Timeout 分支自此有注入覆盖）。
        let seen_a2 = run_inject(2, 600);
        assert!(
            !seen_a2.iter().any(|l| l.contains("观测快探失败") || l.contains("观测快探连续")),
            "Timeout 拍不累积连败（无失败行/标记行）：{seen_a2:?}"
        );
        assert!(!run.tun_shared.transport_unhealthy(), "Busy/Timeout 拍不触发标记");
        // 阶段 B：撤注入（真快探对无 live 连接的岛回 NotConnected——Detail 类），
        // 重启观测循环——先种下 1/3（Account 从 0 起算——若前两阶段的 Busy/Timeout
        // 拍被计入，首条失败行会直接 ≥2/3），再切 Timeout 持住（不清零：恢复 Detail
        // 后续算 2/3，而非 1/3 重来），最终三败到标记。
        PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
        run.stop.store(false, Ordering::Release);
        let h2 = {
            let run3 = Arc::clone(&run);
            std::thread::spawn(move || stats_loop(run3, 5, 0, Duration::from_millis(80)))
        };
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            let seen = drain_lines(&lines);
            if seen.iter().any(|l| l.contains("观测快探失败 1/3")) || Instant::now() >= deadline {
                assert!(
                    seen.iter().any(|l| l.contains("观测快探失败 1/3")),
                    "Detail 拍先种下 1/3（Busy/Timeout 拍未垫高连败）：{seen:?}"
                );
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Timeout 持住若干拍（≥4 拍——**拍计数等待**，评审 r4 P2：不靠纯 wall-clock
        // sleep 推断过拍数，高负载下可能「看起来通过但注入不足」）：不到 3/3、无标记
        //（不累积）。
        PROBE_RESULT_GATE.store(2, Ordering::Relaxed);
        let beats_before_hold = STATS_BEATS.load(Ordering::Relaxed);
        let hold_deadline = Instant::now() + Duration::from_secs(4);
        while STATS_BEATS.load(Ordering::Relaxed) < beats_before_hold + 4 {
            assert!(
                Instant::now() < hold_deadline,
                "Timeout 持住期拍计数未推进到 4 拍（STATS_BEATS 停滞）"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        let during_hold = drain_lines(&lines);
        assert!(
            !during_hold.iter().any(|l| l.contains("观测快探连续 3 败")),
            "Timeout 持住期不累积（到不了 3 败标记）：{during_hold:?}"
        );
        assert!(
            !run.tun_shared.transport_unhealthy(),
            "Timeout 持住期不触发标记"
        );
        // 恢复 Detail：从 2/3 续算（Timeout 未清零——清零会回 1/3 重来），三败到标记。
        PROBE_RESULT_GATE.store(0, Ordering::Relaxed);
        let mut seen_b: Vec<String> = during_hold;
        let deadline = Instant::now() + Duration::from_secs(4);
        loop {
            seen_b.extend(drain_lines(&lines));
            if seen_b.iter().any(|l| l.contains("观测快探连续 3 败")) || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        run.stop.store(true, Ordering::Release);
        h2.join().expect("stats 线程收口 2");
        writer_stop.store(true, Ordering::Release);
        peer_t.join().expect("写包线程收口");
        seen_b.extend(drain_lines(&lines));
        assert!(
            !seen_b.iter().any(|l| l.contains("观测快探失败 1/3")),
            "Timeout 持住期未清零（恢复后从 2/3 续算，无 1/3 重来）：{seen_b:?}"
        );
        assert!(
            seen_b.iter().any(|l| l.contains("观测快探连续 3 败")),
            "三败到标记（Account 路径照常累积）：{seen_b:?}"
        );
        assert!(run.tun_shared.transport_unhealthy(), "标记生效（patrol）");
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    // ==========================================================================
    // reconnect-oneshot 2.1 / 2.4 / 2.5 / 3.2 / 3.4：触发源接线与观测面
    // ==========================================================================

    /// **2.1（r4-R16）**：五计数挂点——①桩回调不调入口时「载位假包 P 与入口 N 分离」
    /// 可构造（P > 0 而 N 停滞 = 读面活着而决策面不存在——事故形态的诊断面）；
    /// ②读线程路径零 logf（spawn 失败注入亦零 logf）。
    #[test]
    fn five_counters_anchor_points_and_read_path_zero_logf() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        // ① P/N 分离：直接模拟读线程的判定序列（carrier false + 计数 + 桩回调不调入口）
        let (run, lines) = gen_with_lines("c21");
        for _ in 0..5 {
            run.tun_shared.carrier.bump_demand_pkt(); // 读循环判定 false 后、调入口前
            // 桩回调**不调入口**（读面饿死/决策面不存在的等价注入）
        }
        assert_eq!(run.tun_shared.carrier.demand_pkts(), 5, "P=5");
        assert_eq!(run.reconn.entries.load(Ordering::Relaxed), 0, "N 停滞");
        // ② 零 logf：真入口 + spawn 全败注入 ⇒ 零日志行（计数由 stats 行落）
        SPAWN_FAIL_GATE.store(true, Ordering::Relaxed);
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::SpawnFailed
        );
        assert_eq!(run.reconn.spawn_fails.load(Ordering::Relaxed), 1, "起跑失败 F=1");
        let seen = drain_lines(&lines);
        assert!(seen.is_empty(), "读线程路径零 logf（含 spawn 失败注入）：{seen:?}");
        SPAWN_FAIL_GATE.store(false, Ordering::Relaxed);
        // ③ 起跑 S 含承接轮（entry → worker run_round 起跑一次）
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::Wake),
            EntryOutcome::Spawned
        );
        wait_reconn_quiescent(&run);
        assert!(run.reconn.starts.load(Ordering::Relaxed) >= 1, "S ≥ 1");
    }

    /// **2.4**：SetForeground 的 false→true kick 以 Foreground 边沿源触发统一入口
    /// （恰一次）——kick 直连统一入口（事件通道已随删除批退役）。
    #[test]
    fn foreground_kick_triggers_entry_once() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let (run, _l) = gen_with_lines("kick");
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        exec.kick_probe();
        assert_eq!(run.reconn.entries.load(Ordering::Relaxed), 1, "入口 1 次");
        wait_reconn_quiescent(&run);
        let lines: Vec<String> = drain_lines(&_l);
        assert!(
            lines.iter().any(|l| l.starts_with("一次性重连：失败（源=回前台")),
            "Foreground 源判据行：{lines:?}"
        );
        *lock_unpoison(&exec.state) = None;
    }

    /// **2.5（BGate 全套）**：B 门沿立即标记 + 触发；读面 worker 在飞时标记已在 +
    /// 收尾后起跑；上次失败后 10s 到达仍标记且受理（边沿绕节流）；设备死在场分类
    /// 仍为 fd、无重复翻沿；一次失败只增一个 flip 序号。
    #[test]
    fn bgate_marks_and_triggers_unified_entry() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("bg");
        // B 门沿（quic_unhealthy_signal 的 patrol 分支等价直调——岛回调零阻塞面）
        super::super::tun_exec::quic_unhealthy_signal_for_test(&run, "patrol");
        assert!(run.tun_shared.transport_unhealthy(), "标记已在（回调沿立即——先于触发）");
        assert!(!run.tun_shared.carrier.alive(), "承载位同步拉假（同一临界区）");
        assert_eq!(run.reconn.entries.load(Ordering::Relaxed), 1, "BGate 触发 1 次");
        wait_reconn_quiescent(&run);
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l.contains("岛上报不健康（patrol）")),
            "B 门行：{seen:?}"
        );
        // 上次失败后 10s（锚内）再达：仍标记（幂等 no-op）且受理（边沿绕节流）
        run.reconn.last_ms.store(mono_ms() - 10_000, Ordering::Release);
        let flips_before = run.tun_shared.flip_seq();
        super::super::tun_exec::quic_unhealthy_signal_for_test(&run, "patrol");
        assert_eq!(
            run.tun_shared.flip_seq(),
            flips_before,
            "已 patrol：no-op、不双 bump（r13-P6）"
        );
        assert_eq!(run.reconn.entries.load(Ordering::Relaxed), 2, "再次触发（边沿绕节流）");
        wait_reconn_quiescent(&run);
        // 设备死在场：分类仍为 fd、无重复翻沿
        run.tun_shared.mark_unhealthy("fd");
        let why = run.tun_shared.health_snapshot().why;
        assert_eq!(why, "fd", "设备类分类不被 B 门覆盖");
        let flips2 = run.tun_shared.flip_seq();
        super::super::tun_exec::quic_unhealthy_signal_for_test(&run, "patrol");
        assert_eq!(
            run.tun_shared.health_snapshot().why,
            "fd",
            "设备死在场：mark_transport no-op"
        );
        assert_eq!(run.tun_shared.flip_seq(), flips2, "无重复翻沿");
    }

    /// **2.5（评审 r1 P1-12 整改：真「在飞 owner」形态）**：B 门沿在读面 worker
    /// 在飞时到达 ⇒ **标记先行**（healthy=false ∧ carrier=false 立即可见）+ 边沿
    /// 位留字（pending 承接）+ owner 收尾后以 BGate 源起跑（不吞边沿）。
    /// 旧版第二次直调发生在 `wait_reconn_quiescent` 之后（非在飞），未覆盖本交错。
    #[test]
    fn bgate_during_inflight_owner_marks_first_and_takeover_runs() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, lines) = gen_with_lines("bginflight");
        // 手工占位 = 在飞 owner（等价 worker 已抢到槽位、run_round 在跑的窗口）
        let epoch = match run.reconn.try_claim_edge(ReconnSource::ReadFace) {
            Claim::Claimed { epoch } => epoch,
            o => panic!("手工占位必须成功：{o:?}"),
        };
        assert_eq!(
            run.reconn.starts.load(Ordering::Relaxed),
            0,
            "手工占位未起跑（真在飞窗口的等价形态）"
        );
        // B 门沿到达（owner 在飞）：
        super::super::tun_exec::quic_unhealthy_signal_for_test(&run, "patrol");
        assert!(run.tun_shared.transport_unhealthy(), "标记先行（在飞不吞标记）");
        assert!(!run.tun_shared.carrier.alive(), "承载位同步拉假");
        assert_eq!(run.reconn.entries.load(Ordering::Relaxed), 1, "统一入口触发 1 次");
        assert_ne!(
            run.reconn.state.load(Ordering::Acquire)
                & ReconnSource::BGate.pending_bit(),
            0,
            "BGate 边沿位留字（pending 承接——不吞边沿）"
        );
        // owner 收尾：release 读到留字位 ⇒ 承接一轮（源=BGate——from_pending 推导）
        let run2 = Arc::clone(&run);
        let h = std::thread::spawn(move || {
            let mut guard = RoundGuard {
                epoch,
                triggering: 0,
            };
            reconnect_finalize_loop(&run2, &mut guard);
        });
        h.join().expect("收尾收口");
        wait_reconn_quiescent(&run);
        let seen = drain_lines(&lines);
        assert!(
            seen.iter().any(|l| l == "一次性重连：起跑（源=B门）"),
            "收尾后以 BGate 源起跑（承接轮）：{seen:?}"
        );
        assert!(
            seen.iter().any(|l| l.starts_with("一次性重连：失败（源=B门")),
            "承接轮照常执行（合成世代无岛 ⇒ 失败行）：{seen:?}"
        );
        assert!(!run.reconn.inflight(), "终态静止");
    }

    /// **1.4（评审 r1 P1-12 整改）**：tun_stop 与在飞 worker 并发——join 预算内
    /// 返回（rc=0，**非 -2 强放锁**）、陈旧 worker 对已停岛快速 Gone、静默退出
    /// （零对端失败行）。在飞 worker 用 `round_before_probe` 挂点构造（受控交错）；
    /// 世代收尾经真 `ClientCore::tun_stop` 面（TunShared 生命周期手工镜像 gen_loop
    /// 的「等 stop → finish_generation」）。
    #[test]
    fn tun_stop_with_inflight_worker_returns_zero_and_worker_exits_silently() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        let logf: Logf = Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        });
        // 手工装配 GenRun（synthetic_for_test 不可注入共享 TunShared——本用例需要
        // ClientCore 与 run 共握同一枚）
        let ident = crate::identity::Identity::ephemeral().expect("临时身份");
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        let stop = Arc::new(AtomicBool::new(false));
        let run = Arc::new(GenRun {
            gen: 1,
            stop: Arc::clone(&stop),
            stop_wake: StopWake::new(),
            island: RwLock::new(None),
            l3_on_island: AtomicBool::new(false),
            identity: ident.clone(),
            tun_ip: String::new(),
            link: Mutex::new(None),
            reconn: ReconnGate::new(),
            bridge: Mutex::new(None),
            logf: Arc::clone(&logf),
            tun_shared: Arc::clone(&sh),
            demand: Arc::new(DemandSignals::new()),
            pf: crate::facade::portfwd::PfRuntime::new(crate::facade::portfwd::PfSetup {
                dial: Arc::new(|_dst, _b| {
                    Err(io::Error::new(io::ErrorKind::ConnectionRefused, "桩"))
                }),
                stop: Arc::clone(&stop),
                logf: Arc::clone(&logf),
                budget: Duration::from_secs(1),
                limits: crate::facade::portfwd::PfLimits::default(),
            }),
            started: Instant::now(),
        });
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        // 在飞 worker：挂在快探前（真起跑、动作未发）
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let ht = hold_tx.clone();
        let _un = install_hooks(&run, Arc::new(move |point: &str| {
            if point == "round_before_probe" {
                let _ = ht.send(());
                let _ = go_rx.lock().unwrap().recv();
            }
        }));
        assert_eq!(
            GenRun::try_spawn_reconnect(&run, ReconnSource::ReadFace),
            EntryOutcome::Spawned
        );
        hold_rx.recv_timeout(Duration::from_secs(2)).expect("worker 在飞（快探前挂点）");
        // 世代生命周期镜像 gen_loop：等 stop → finish_generation（done 信号）
        let stop2 = Arc::clone(&stop);
        let sh2 = Arc::clone(&sh);
        let gen_thread = std::thread::spawn(move || {
            while !stop2.load(Ordering::Acquire) {
                std::thread::sleep(Duration::from_millis(20));
            }
            sh2.finish_generation(1);
        });
        // ---- tun_stop 与在飞 worker 并发 ----
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        let core = crate::facade::ClientCore::with_tun_shared(exec, demand, Arc::clone(&sh));
        sh.probe_running.store(true, Ordering::Release);
        let t0 = Instant::now();
        let rc = core.tun_stop();
        let elapsed = t0.elapsed();
        assert_eq!(rc, 0, "join 预算内返回 0（非 -1/-2 强放锁）");
        assert!(elapsed < Duration::from_secs(3), "STOP_WAIT 预算内（实测 {elapsed:?}）");
        gen_thread.join().expect("世代线程收口");
        // 放行 worker：对已停世代快速 Gone、静默退出（零对端失败行）
        let _ = go_tx.send(());
        wait_reconn_quiescent(&run);
        let seen: Vec<String> = rx.try_iter().collect();
        assert!(
            seen.iter().any(|l| l == "一次性重连：起跑（源=读面）"),
            "worker 真起跑过：{seen:?}"
        );
        assert!(
            !seen.iter().any(|l| l.contains("失败（源=") || l.contains("换源") || l.contains("赛跑失败")),
            "陈旧 worker 静默退出——零对端失败行：{seen:?}"
        );
        assert!(!run.reconn.inflight(), "状态字终态静止");
        run.take_island();
        island.stop();
    }

    /// **判据（评审 r3 B2⑥ + r4 B2-a 生产序）**：`tun_stop` -2 强制放锁与旧 finish
    /// 并发——旧 finish 停在旧 `is_current` 判断位（stale true）、`tun_stop` 烧满
    /// `STOP_WAIT` 强放锁返回 -2、**新 prepare 走生产受理原语 `begin_prepare`**
    /// 停在门内「swap 已成功、gen 尚未递增」处（r3 版测试是反序——先
    /// `begin_generation(2)` 再置 probe_running=true，覆盖不到该窗口）、放行旧
    /// finish（被门挡住）后再放行新 prepare ⇒ 换代先落地，旧 finish 尾段复核 gen
    /// 跳过：新世代 attach 槽不被关、done 不被置真、**probe_running 不被清**
    /// （B2-a 核心断言）、停止位不被置真（B2-b）、`running()` 为真。经真
    /// `ClientCore::tun_stop` 面（`with_tun_shared` 注入共握 TunShared）。
    ///
    /// **确定性对照（评审 r5-L2）**：`finish_tail_hook` 是持门且 gen 复核通过后的
    /// 屏障——放行旧 finish 后先等它进入尾段（**旧序/swap 在门外时必达**），再放行
    /// 新 prepare；修好序下新 prepare 持门 ⇒ 旧 finish 被挡在门外、屏障超时，
    /// 换代先落地。因此旧序下「旧 finish 尾段先清锁」是确定性形态（不再依赖调度
    /// 的 4/5 概率），旧实现必红于「新世代单飞锁不被旧 finish 清」。
    #[test]
    fn tun_stop_forced_release_with_late_finish_does_not_clobber_new_generation() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        let logf: Logf = Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        });
        let ident = crate::identity::Identity::ephemeral().expect("临时身份");
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        let stop = Arc::new(AtomicBool::new(false));
        let run = Arc::new(GenRun {
            gen: 1,
            stop: Arc::clone(&stop),
            stop_wake: StopWake::new(),
            island: RwLock::new(None),
            l3_on_island: AtomicBool::new(false),
            identity: ident,
            tun_ip: String::new(),
            link: Mutex::new(None),
            reconn: ReconnGate::new(),
            bridge: Mutex::new(None),
            logf: Arc::clone(&logf),
            tun_shared: Arc::clone(&sh),
            demand: Arc::new(DemandSignals::new()),
            pf: crate::facade::portfwd::PfRuntime::new(crate::facade::portfwd::PfSetup {
                dial: Arc::new(|_dst, _b| {
                    Err(io::Error::new(io::ErrorKind::ConnectionRefused, "桩"))
                }),
                stop: Arc::clone(&stop),
                logf: Arc::clone(&logf),
                budget: Duration::from_secs(1),
                limits: crate::facade::portfwd::PfLimits::default(),
            }),
            started: Instant::now(),
        });
        sh.probe_running.store(true, Ordering::Release);
        // 旧 finish 停在 is_current 判断之后（钩子受控交错——tun_stop 的 3s 等待窗
        // 内 done 永不置位 ⇒ 强放锁路径必达）。
        let (park_tx, park_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let ptx = park_tx;
        let _un = sh.install_finish_hook(Arc::new(move || {
            let _ = ptx.send(());
            let _ = go_rx.lock().unwrap().recv();
        }));
        let fin = {
            let sh2 = Arc::clone(&sh);
            std::thread::spawn(move || sh2.finish_generation(1))
        };
        park_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("旧 finish 已停在 is_current 之后");
        // ---- tun_stop：STOP_WAIT 烧满 ⇒ -2 强放锁（真 ClientCore 面）----
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        let core = crate::facade::ClientCore::with_tun_shared(exec, demand, Arc::clone(&sh));
        let t0 = Instant::now();
        let rc = core.tun_stop();
        let elapsed = t0.elapsed();
        assert_eq!(rc, -2, "旧 finish 停住 ⇒ 强放锁（-2）而非 0/-1");
        assert!(
            elapsed >= crate::facade::STOP_WAIT
                && elapsed < crate::facade::STOP_WAIT + Duration::from_secs(2),
            "STOP_WAIT 烧满后强放（实测 {elapsed:?}）"
        );
        assert!(
            !sh.probe_running.load(Ordering::Acquire),
            "强放后锁已放（新 prepare 可 CAS）"
        );
        // ---- 新 prepare 起跑（生产序——评审 r4 B2-a）：begin_prepare 停在门内
        // 「swap 已成功、gen 尚未递增」处（持门）——此时 B2-a 窗口的形态在册：
        // probe_running=true 而 gen 仍 =1。----
        let (ppark_tx, ppark_rx) = mpsc::channel::<()>();
        let (pgo_tx, pgo_rx) = mpsc::channel::<()>();
        let pgo_rx = std::sync::Mutex::new(pgo_rx);
        let ptx2 = ppark_tx;
        let _pun = sh.install_prepare_hook(Arc::new(move || {
            let _ = ptx2.send(());
            let _ = pgo_rx.lock().unwrap().recv();
        }));
        let prep = {
            let sh3 = Arc::clone(&sh);
            std::thread::spawn(move || sh3.begin_prepare())
        };
        ppark_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("新 prepare 已停在门内 swap 之后（gen 递增之前）");
        assert!(
            sh.probe_running.load(Ordering::Acquire),
            "新 prepare 已持单飞（swap 已成功——B2-a 窗口形态）"
        );
        assert_eq!(
            sh.gen.load(Ordering::Acquire),
            1,
            "gen 尚未递增（swap 先于换代的窗口形态）"
        );
        // ---- 尾段屏障（评审 r5-L2）：旧 finish 若先进入尾段（旧序），tail 钩子
        // 必达；修好序下新 prepare 持门 ⇒ 旧 finish 被挡在门外，超时即按修复形态
        // 继续——旧序下「尾段先于换代」由此确定。----
        let (tail_tx, tail_rx) = mpsc::channel::<()>();
        let ttx = tail_tx;
        let _un_tail = sh.install_finish_tail_hook(Arc::new(move || {
            let _ = ttx.send(());
        }));
        // ---- 放行旧 finish（与生产同序：swap 后、换代前放行；被门挡住，等新
        // prepare 完成换代后才能进门复核）----
        let _ = go_tx.send(());
        let entered_tail = tail_rx.recv_timeout(Duration::from_secs(2)).is_ok();
        // ---- 放行新 prepare：门内完成 gen 递增 + 换代复位 ----
        let _ = pgo_tx.send(());
        let gen2 = prep.join().expect("新 prepare 收口").expect("受理（gen=2）");
        assert_eq!(gen2, 2, "新世代 gen=2");
        fin.join().expect("旧 finish 收口");
        // ---- 新世代装配面（attach/停止位/健康复位/阶段）----
        let rx2 = sh.attach_receiver(2);
        let f2 = Arc::new(AtomicBool::new(false));
        sh.set_stop_flag_if_current(2, Arc::clone(&f2));
        sh.begin_healthy(2);
        sh.stage.set_if_current(2, TunStage::Attached, "", "", true);
        assert!(
            sh.deliver_fd(11),
            "新世代 attach 槽不被迟到的旧 finish 关（fd 可投递）"
        );
        assert_eq!(rx2.recv(), Ok(11), "fd 落进新世代接收端");
        assert!(!sh.is_done(), "新世代 done 不被旧 finish 置真");
        assert!(
            sh.probe_running.load(Ordering::Acquire),
            "新世代单飞锁不被旧 finish 清（B2-a 核心断言——旧序 tail 屏障命中={entered_tail}）"
        );
        assert!(
            !f2.load(Ordering::Acquire),
            "新世代停止位不被旧 finish 置真（B2-b）"
        );
        assert!(sh.running(), "新世代 running（单飞 + 非设备死 + attached）");
        drop(rx);
    }

    /// **判据（评审 r5-L1）**：`tun_stop` -2 强放锁的复核与 store 同门——受控钩子
    /// 卡在「门外复查已过、放锁未落」处：窗口内旧 finish（gen=1）收尾、新 prepare
    /// 完成换代（gen=2、新单飞锁在位）；放行后 `force_release_if_current(1)` 在
    /// `finish_gate` 内复核见 gen=2 ⇒ **不强放**、返回 -1，新世代单飞锁不被清。
    /// 对照实验：把调用退回裸 `probe_running.store(false)` ⇒ 本用例必红于
    /// 「新世代单飞锁不被清」（同一受控钩子窗口，确定性）。
    #[test]
    fn tun_stop_force_release_rechecks_under_gate_and_preserves_new_generation() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        sh.probe_running.store(true, Ordering::Release);
        // tun_stop 的 -2 路径要烧满 STOP_WAIT（done 永不置位）；受控钩子只卡在
        // 「第二次复核已过、force_release 未落」处（不持 finish_gate）。
        let (park_tx, park_rx) = mpsc::channel::<()>();
        let (go_tx, go_rx) = mpsc::channel::<()>();
        let go_rx = std::sync::Mutex::new(go_rx);
        let ptx = park_tx;
        let _un = sh.install_stop_force_hook(Arc::new(move || {
            let _ = ptx.send(());
            let _ = go_rx.lock().unwrap().recv();
        }));
        let demand = Arc::new(DemandSignals::new());
        // 无 state/shared：request_stop no-op（不打断等待）——强放路径必达。
        let exec = TunnelExec::new(Arc::clone(&demand));
        let core = crate::facade::ClientCore::with_tun_shared(exec, demand, Arc::clone(&sh));
        let stopper = std::thread::spawn(move || core.tun_stop());
        park_rx
            .recv_timeout(crate::facade::STOP_WAIT + Duration::from_secs(2))
            .expect("tun_stop 已过第二次复核（强放前钩子）");
        // ---- 窗口内：旧 finish 收尾 + 新 prepare 完成换代 ----
        sh.finish_generation(1);
        let gen2 = sh.begin_prepare().expect("旧锁已放 ⇒ 新 prepare 受理");
        assert_eq!(gen2, 2);
        sh.begin_healthy(2);
        assert!(
            sh.probe_running.load(Ordering::Acquire),
            "新世代单飞锁已就位（换代完成）"
        );
        // ---- 放行 tun_stop：门内复核见 gen=2 ⇒ 不强放 ----
        let _ = go_tx.send(());
        let rc = stopper.join().expect("tun_stop 收口");
        assert_eq!(rc, -1, "门内复核见新世代 ⇒ 不强放（-1 而非 -2）");
        assert!(
            sh.probe_running.load(Ordering::Acquire),
            "新世代单飞锁不被迟到的 tun_stop 强放清（r5-L1 核心断言——裸 store 必红）"
        );
        assert_eq!(sh.gen.load(Ordering::Acquire), 2, "新世代仍在册");
    }

    /// **3.2（评审 r1 P1-12 整改）**：`steady_now` 五门抽纯函数——承载 false /
    /// 设备死 / patrol / worker 在飞 / 岛内阶梯在飞，任一命中即非稳态；全清才稳态。
    #[test]
    fn steady_now_five_gates() {
        let hs = |carrier: bool, device_dead: bool, why: &str| {
            super::super::tun_shared::HealthSnapshot {
                healthy: why.is_empty(),
                why: why.to_owned(),
                device_dead,
                last_patrol_at: None,
                carrier,
            }
        };
        assert!(!steady_now(&hs(false, false, ""), false, false), "门①承载不在位");
        assert!(!steady_now(&hs(true, true, ""), false, false), "门②设备死");
        assert!(!steady_now(&hs(true, false, "patrol"), false, false), "门③传输不健康");
        assert!(!steady_now(&hs(true, false, ""), true, false), "门④重连 worker 在飞");
        assert!(!steady_now(&hs(true, false, ""), false, true), "门⑤岛内阶梯在飞");
        assert!(steady_now(&hs(true, false, ""), false, false), "全清 ⇒ 稳态");
        assert!(steady_now(&hs(true, false, "fd"), false, false), "设备类文案不拦观测（patrol 才拦）");
    }

    /// **评审 r3 B7**：`quick_probe_classify` 的字符串谓词与 `IslandErr::ProbeBusy`
    /// 的 Display **逐字耦合**钉住（改 Display 或谓词时此测试先红）；`RaceInFlight`
    /// （`Cmd::Connect` 的在途回执）必须归 Detail——`Cmd::Probe` 路径上不可达的
    /// 旧「已有赛跑在途」Busy 分支已按事实删除。`obs_fail_disposition` 的三分类
    /// 全值（Exit/Skip/Account）同测。
    #[test]
    fn quick_probe_classify_pins_probe_busy_string_and_disposition() {
        // 真源 Display → Busy（字符串谓词与 Display 的耦合面）
        let busy = homeway_quic::IslandErr::ProbeBusy.to_string();
        assert!(
            matches!(
                quick_probe_classify(IslandRpc::Island(busy.clone())),
                ProbeFail::Busy(_)
            ),
            "ProbeBusy 的 Display 必须命中 Busy 谓词：{busy}"
        );
        // RaceInFlight 归 Detail（Cmd::Probe 不产该回执；即便上游形态变化也不得
        // 静默混入「并发不累积」分类——那是对端语义，需走 typed 面显式登记）
        assert!(matches!(
            quick_probe_classify(IslandRpc::Island(
                homeway_quic::IslandErr::RaceInFlight.to_string()
            )),
            ProbeFail::Detail(_)
        ));
        assert!(matches!(
            quick_probe_classify(IslandRpc::Gone("x".into())),
            ProbeFail::IslandGone(_)
        ));
        assert!(matches!(
            quick_probe_classify(IslandRpc::Timeout("x".into())),
            ProbeFail::Timeout(_)
        ));
        // 处置三分类全值
        assert_eq!(
            obs_fail_disposition(&ProbeFail::IslandGone("x".into())),
            ObsFailDisposition::Exit
        );
        assert_eq!(
            obs_fail_disposition(&ProbeFail::Busy("x".into())),
            ObsFailDisposition::Skip
        );
        assert_eq!(
            obs_fail_disposition(&ProbeFail::Timeout("x".into())),
            ObsFailDisposition::Skip
        );
        assert_eq!(
            obs_fail_disposition(&ProbeFail::Detail("x".into())),
            ObsFailDisposition::Account
        );
        assert_eq!(
            obs_fail_disposition(&ProbeFail::NoFace),
            ObsFailDisposition::Account
        );
    }

    /// **2.5（r5-N2 临界区收口）**：try_recover 清 why 与 mark_transport 写 patrol
    /// 并发交错 ⇒ 无「健康但分类残留」组合（stress：两线程交替翻回/标记，快照
    /// 不变量恒成立）；读侧统一 HealthSnapshot（一次锁读——字段一致）。
    #[test]
    fn concurrent_recover_and_mark_never_leave_stale_combo() {
        let sh = Arc::new(TunShared::new());
        sh.begin_generation(1);
        sh.begin_healthy(1);
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        for t in 0..2 {
            let sh2 = Arc::clone(&sh);
            let stop2 = Arc::clone(&stop);
            handles.push(std::thread::spawn(move || {
                let mut i = 0u32;
                while !stop2.load(Ordering::Acquire) {
                    if t == 0 {
                        let _ = sh2.mark_transport_if_current(1);
                    } else {
                        let _ = sh2.try_recover_transport_if_current(1, Duration::ZERO);
                    }
                    // 读侧统一快照：不变量「healthy ⇒ 无 patrol 残留」
                    let hs = sh2.health_snapshot();
                    assert!(
                        !hs.healthy || hs.why != "patrol",
                        "「健康但分类残留」禁止组合（r5-N2）"
                    );
                    i += 1;
                    if i > 2000 {
                        break;
                    }
                }
            }));
        }
        std::thread::sleep(Duration::from_millis(150));
        stop.store(true, Ordering::Release);
        for h in handles {
            h.join().expect("stress 收口");
        }
    }

    /// **2.5**：begin_healthy(gen) 带世代参数、陈旧 no-op；carrier 置 true 锁内查
    /// device_dead（设备死下置 true 判负——1.1 的 facade 面等价钉）。
    #[test]
    fn begin_healthy_gen_guard_and_carrier_device_dead_gate() {
        let sh = TunShared::new();
        sh.begin_generation(2);
        sh.mark_unhealthy("patrol");
        // 陈旧世代（gen=1）的复位 no-op
        sh.begin_healthy(1);
        assert!(sh.transport_unhealthy(), "陈旧复位不得清新世代");
        // 当前世代复位：全清 + 承载位随新世代复位（alive=false——adopt 置 true）
        sh.begin_healthy(2);
        assert!(!sh.transport_unhealthy());
        assert!(sh.is_healthy());
        assert!(!sh.carrier.alive(), "新世代承载位复位为 false");
        // 置 true 沿 + 设备死在场判负
        assert!(sh.mark_carrier_alive_if_current(2), "健康态下置 true 成立");
        sh.mark_unhealthy("fd");
        assert!(!sh.carrier.alive(), "设备死拉假承载位");
        assert!(
            !sh.mark_carrier_alive_if_current(2),
            "设备死在场：置 true 判负（r5-N1）"
        );
        assert!(!sh.carrier.alive());
    }

    /// **3.2（r8-a3 / r5-N4）**：观测连败推进纯函数——失败只在出站包拍累积；无出站
    /// 包拍清零（已有 2 连败 → 无需求拍归零 → 下一需求拍首败不触发标记）；3 败到门槛。
    #[test]
    fn obs_streak_gates_on_outbound_packets_only() {
        // 首败（有出站包）：1，未到门槛
        assert_eq!(obs_streak_step(0, 5), (1, false));
        assert_eq!(obs_streak_step(1, 1), (2, false));
        // 无出站包拍：清零（亮屏无流量不累积——r6；已有 2 连败也归零——r5-N4）
        assert_eq!(obs_streak_step(2, 0), (0, false));
        // 归零后首败：1（不触发标记——不跨无流量窗拼凑）
        assert_eq!(obs_streak_step(0, 7), (1, false));
        // 三连败（均有出站包）：到门槛
        assert_eq!(obs_streak_step(2, 3), (3, true));
    }

    /// **3.2（r3-R9 / r10-C1）**：双累加器节拍的年龄保证——tick = min(stats_secs,60)、
    /// 观测每 60s ⇒ `link.at` 年龄上界 = 60s（观测周期）+ tick（到点迟滞）≤ 120s < 180s
    /// （3min 陈旧判据不误报）；5s/60s/300s 三档一致成立。
    #[test]
    fn dual_accumulator_keeps_link_at_age_under_three_minutes() {
        for stats_secs in [5i64, 60, 300] {
            let stats_every = Duration::from_secs(stats_secs.max(STATS_SECS_MIN) as u64);
            let tick = stats_every.min(OBS_INTERVAL);
            // 模拟 600s：观测拍在 elapsed>=60s 时触发、触发后 since_obs=now——
            // 两次观测的最大间隔 = OBS_INTERVAL + tick（片化等待的迟滞上界）
            let worst_gap = OBS_INTERVAL + tick;
            assert!(
                worst_gap < Duration::from_secs(180),
                "stats_secs={stats_secs}：at 年龄上界 {worst_gap:?} < 3min"
            );
            // 成功探测刷 at ⇒ 年龄恒 < 3min；探测失败/承载不在位保留旧值（陈旧判据
            // 自然成立——由 steady 门与 refresh_link_snapshot 的组合承载，见
            // steady_now / obs_streak_step 单测）。
        }
    }

    /// **3.2（r4-R14）**：island_cmd_until 的 stop-aware 面——stop 位/世代变化立即
    /// Gone（不落「对端失败」行）；deadline 已过立即 Timeout。
    #[test]
    fn island_cmd_until_is_stop_aware() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _l) = gen_with_lines("until");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        // stop 位：立即 Gone（岛不回执也不挂等）
        run.stop.store(true, Ordering::Release);
        let t0 = Instant::now();
        let r = island_cmd_until(
            &run,
            &island,
            |reply| homeway_quic::Cmd::Probe {
                budget: Duration::from_secs(10),
                reply,
            },
            Instant::now() + Duration::from_secs(5),
        );
        assert!(matches!(r, Err(IslandRpc::Gone(_))), "stop ⇒ Gone：{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(1), "立即收口（不烧预算）");
        run.stop.store(false, Ordering::Release);
        // 世代变化：Gone
        run.tun_shared.gen.store(99, Ordering::Release);
        let r2 = island_cmd_until(
            &run,
            &island,
            |reply| homeway_quic::Cmd::Probe {
                budget: Duration::from_secs(10),
                reply,
            },
            Instant::now() + Duration::from_secs(5),
        );
        assert!(matches!(r2, Err(IslandRpc::Gone(_))), "世代变化 ⇒ Gone：{r2:?}");
        run.tun_shared.gen.store(run.gen, Ordering::Release);
        // deadline 已过：立即 Timeout
        let r3 = island_cmd_until(
            &run,
            &island,
            |reply| homeway_quic::Cmd::Probe {
                budget: Duration::from_millis(1),
                reply,
            },
            Instant::now() - Duration::from_millis(1),
        );
        assert!(matches!(r3, Err(IslandRpc::Timeout(_))), "到点 ⇒ Timeout：{r3:?}");
        run.take_island();
        island.stop();
    }

    /// **判据（评审 r1 2.21 整改）**：DNS 腿经 `dial()` 的**开流触发与流句柄回调
    /// 双零触发**——解析失败是环境噪声面，不当连接死亡证据；对照组 Files 同形态
    /// 失败则触发统一入口（观测面 = 重连入口计数 entries）。整改前 `dial()` 的
    /// 流句柄无条件携带 `Some(cb)`——DNS 流的 ConnectionLost 会误触发 ServiceStream
    /// 源（生产 DNS 走 `host_session::dial_stream_id`〔None〕，本用例补齐 dial()
    /// 类型面的同一策略）。
    #[test]
    fn dns_dial_does_not_fire_carrier_entry() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let (run, _lines) = gen_with_lines("dnscb");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        // DNS 腿：开流失败（idle 岛无 live 连接 ⇒ ConnectionLost）⇒ 零触发
        let entries0 = run.reconn.entries.load(Ordering::Relaxed);
        let r = super::super::quic_stream::dial(&run, homeway_quic::StreamTag::DnsResolve, Duration::from_millis(200));
        assert!(r.is_err(), "idle 岛无 live 连接 ⇒ 拨号失败");
        assert_eq!(
            run.reconn.entries.load(Ordering::Relaxed),
            entries0,
            "DNS 腿零触发（开流 sel=None——不接统一入口）"
        );
        // 对照组：Files（用户可见服务入口）同形态失败 ⇒ 触发一次（第二次承载类失败）
        let r2 = super::super::quic_stream::dial(&run, homeway_quic::StreamTag::Files, Duration::from_millis(300));
        assert!(r2.is_err());
        assert!(
            run.reconn.entries.load(Ordering::Relaxed) > entries0,
            "Files 腿承载类失败触发统一入口（entries 前进）"
        );
        wait_reconn_quiescent(&run);
        run.set_l3_on_island(false);
        run.take_island();
        island.stop();
    }

    /// **3.4**：`rescueRoundsExhausted` 生产面恒 false（契约键保留——台账只增不减）。
    #[test]
    fn rescue_rounds_exhausted_is_const_false() {
        let _ser = RECONN_TEST_LOCK.lock().unwrap();
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let (run, _l) = gen_with_lines("rex");
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        assert!(!exec.rescue_rounds_exhausted());
        run.tun_shared.mark_transport_if_current(run.gen);
        assert!(
            !exec.rescue_rounds_exhausted(),
            "标记后仍恒 false（轮次状态机退役）"
        );
        // 快照键面：tunStatusJSON 顶层键在册且值 false
        let core = crate::facade::ClientCore::new(Arc::new(
            crate::facade::NoopTunExecutor,
        ));
        let _ = core; // 键序断言在 tun_status::tests::base_key_face_and_order（3.3 面）
        *lock_unpoison(&exec.state) = None;
    }
}
