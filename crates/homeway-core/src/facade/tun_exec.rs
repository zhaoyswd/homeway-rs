//! 隧道域真执行体（QUIC 岛单承载；语义真源 `baseline:clientcore/cmd/clientcore/tunmode.go`
//! 的 runTun2Tailcat + 巡检 goroutine + startDemandPusher + stats ticker + 隧道桥）。
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
//!   → 起桥（三座）+ 巡检 + demand pusher + stats
//!   → 等 stop → 收工（pf 停 → 桥停 → 岛停）→ finish_generation
//! ```
//!
//! 一切退出路径都过 `finish_generation`（终态/放锁/done——防单飞锁泄漏）。
//! 巡检语义：失败当拍**只留痕**（动作面 = 岛内快探阶梯，M3 S4 §3.1——岛的动作快
//! 240×，世代层不重复动作不抢跑），3 连败 markUnhealthy("patrol") 交扩展重建；
//! demand 门控、挂起空窗留痕。（WG 档的 R1/R2/R3 阶梯与中继升直连条纹随 WG 删除。）

use std::io;
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::identity::{self, Identity};
use crate::token::Token;
use crate::tunnel_addr::SERVER_TUNNEL_IP;
use crate::Logf;

use super::bridge_host::{BridgeHost, BridgeStream};
use super::demand::{self, DemandSignals};
use super::portfwd::{PfLimits, PfRuntime, PfSetup, PortForwardRule};
use super::stage::TunStage;
use super::tun_shared::{lock_unpoison, TunShared};
use super::tun_status::{LinkIn, QuicIn, RunnerIn, TransportIn};
use super::{
    TunConfigJson, TunError, TunExecutor, ATTACH_DEADLINE, ATTACH_TIMEOUT_REASON, WARM_TIMEOUT,
};

/// 巡检间隔（固定 60s——保活 + 可达性；与连接策略真源同值）。
pub const PATROL_INTERVAL: Duration = Duration::from_secs(60);
/// 巡检每拍探测预算。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 连续计入证据的失败到这个数进 R2 阶梯。
const FAIL_STREAK_LADDER: u32 = 3;
/// 巡检连败证据时间窗（相邻计入失败的间隔超过它 ⇒ 计数作废）。
const PATROL_FAIL_WINDOW: Duration = Duration::from_secs(600);
/// demand pusher 节拍（D4：App 出站新鲜 + 接收静默 → 立即下推，不等巡检拍）。
const PUSHER_TICK: Duration = Duration::from_secs(1);
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

/// 退出路径 RPC 的预算（Q-F F3b/N1：close_write / `StreamShared::drop` 走在**必须退出**
/// 的收工链上——引擎卡死时无界等待会把桥泵与世代收工钉住。2s = 动作预算同刻度，
/// 正常引擎回执即时）。消费点 = QUIC 服务流（`facade/quic_stream.rs`）。
pub(crate) const EXIT_RPC_BUDGET: Duration = Duration::from_secs(2);

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

/// 背压重试节拍（F8a 纯函数）：前 50 拍 2ms、51–100 拍 10ms、其后 20ms 封顶。
/// 10s 无进展上界与「空写短路」语义不随本表变化。**M3 S3**：QUIC 档写半
/// （`facade/quic_stream.rs`）复用同一张表——节拍随承载变化须登记（§1.4），本节拍
/// **不随承载变**（待发队列 64 KiB vs 旧栈 1 MiB 只影响 `n=0` 的到达频率）。
pub(crate) fn write_retry_backoff(attempt: u32) -> Duration {
    if attempt < 50 {
        Duration::from_millis(2)
    } else if attempt < 100 {
        Duration::from_millis(10)
    } else {
        Duration::from_millis(20)
    }
}

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
    super::quic_stream::dial_target(&r, dst, budget)
}

/// 拨号首试预算（隧道域；宿主会话的服务域用 `host_session` 的同值档——两端「首试」的
/// 语义面一致，便于对照读数）。
pub(crate) const FIRST_TRY: Duration = Duration::from_secs(4);

// ---------------------------------------------------------------------------
// 世代运行态（Go tunRun）
// ---------------------------------------------------------------------------

/// 世代事件（世代主线程的 select 面：stop/kick）。
pub enum GenEvent {
    Stop,
    /// 立即探测（回前台/attach 后）。
    Kick,
}

/// 一个世代的运行态（TunnelExec 持当前世代的 Arc；旧世代迟到收尾无害）。
pub struct GenRun {
    pub gen: u64,
    /// 世代停止位（request_stop 置；所有线程收口判据）。
    pub stop: Arc<AtomicBool>,
    /// 世代事件通道（stop/kick 的唤醒面）。
    pub ev_tx: mpsc::SyncSender<GenEvent>,
    /// QUIC 岛（世代装配期构造；`None` = 未构造/已收回）。**M5 C3 单承载**：
    /// 岛装配失败即本世代失败（无回落档，设计 §4.3③）。
    island: RwLock<Option<Arc<homeway_quic::Island>>>,
    /// L3 是否已承接（装配期置位；`l3_on_island()` 的另一半判据）。
    l3_on_island: AtomicBool,
    /// 身份面（世代归因）。
    identity: Identity,
    /// 本世代隧道地址（`transport.tunIp` 的源；装配期一次派生）。
    tun_ip: String,
    /// 链路快照（runner link 段数据源；巡检写）。
    pub link: Mutex<Option<LinkIn>>,
    /// 隧道桥（attached 后起；None = 未起/已停）。
    pub bridge: Mutex<Option<Arc<BridgeHost>>>,
    /// 日志面（带 `tier-core: ` 前缀；写世代日志文件）。
    pub logf: Logf,
    /// facade 世代共享面（阶段机/健康位/收尾）。
    pub tun_shared: Arc<TunShared>,
    /// 需求信号（ClientCore 的共享面——巡检/demand pusher 消费）。
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

    /// 最近出站包时刻（单调面；下推器时基）。
    fn l3_last_outbound_at(&self) -> Option<Instant> {
        self.engaged_island().and_then(|i| i.last_outbound_at())
    }

    /// 最近出站包时刻（unix 毫秒；`transport.outboundAt` 的源）。
    fn l3_last_outbound_unix_ms(&self) -> i64 {
        self.engaged_island()
            .map(|i| i.last_outbound_unix_ms())
            .unwrap_or(0)
    }

    /// 「对端来的包」计数（下推器的接收静默判据；= 已写回 TUN 的包数）。
    fn l3_rx(&self) -> u64 {
        self.engaged_island()
            .map(|i| i.snapshot().packets_out)
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
    /// [`Self::kick_probe`]（false→true 转变时立即探测一次）。
    pub fn kick_probe(self: &Arc<Self>) {
        if let Some(run) = self.gen_run() {
            let _ = run.ev_tx.try_send(GenEvent::Kick);
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
        let token = crate::token::decode(&cfg.token)
            .map_err(|e| TunError::Core(format!("token 解析失败：{e}")))?;
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
                let _ = run.ev_tx.try_send(GenEvent::Stop);
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

    /// **M5 C3 单承载**：恢复下推恒走岛档（`from` 的档位名只进归因行文本——rc 契约
    /// 面仍由 `facade::host_session::Level` 的 `clamp` 定；见 `facade/mod.rs`）。
    fn recover(&self, _from: i64, cause: &str) -> i32 {
        let Some(run) = self.gen_run() else {
            return -2; // 没有 attached 隧道（不构成网络结论）
        };
        // 陈旧世代 → -2（复核 r3-F12：Go recoverTunnelReady 在该窗口明确 -2 并警告
        // 非 -2 会让扩展做无意义恢复、甚至触发整套重建）
        if run.gen != run.tun_shared.gen.load(Ordering::Acquire) {
            return -2;
        }
        recover_downpush_on_island(&run, cause)
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
        let (ev_tx, _ev_rx) = mpsc::sync_channel::<GenEvent>(8);
        let tun_shared = Arc::new(TunShared::new());
        tun_shared.begin_generation(1);
        // 世代停止位与 pf 的 `gen_stop` **同一枚 Arc**（生产同源：`Arc::clone(&stop)`）——
        // 否则「停止位让 accept 线程自退」这条兜底通路在测试缝里不可达（代码门 r27 P10）
        let stop = Arc::new(AtomicBool::new(false));
        GenRun {
            gen: 1,
            stop: Arc::clone(&stop),
            ev_tx,
            island: RwLock::new(None),
            l3_on_island: AtomicBool::new(false),
            identity: ident.clone(),
            tun_ip: crate::tunnel_addr::derive_tun_ip(
                &crate::token::Secret::from([2u8; 32]),
                &ident.public_key(),
            )
            .to_string(),
            link: Mutex::new(None),
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
    let (ev_tx, ev_rx) = mpsc::sync_channel::<GenEvent>(8);
    // portfwd 运行时先建材料（拨号闭包经 `Arc::new_cyclic` 拿 `Weak<GenRun>`——
    // 运行时结构上不依赖 GenRun，但生产实现要回指它；Weak 破环 = 世代可回收，D7）。
    let pf_budget = Duration::from_millis(cfg.dial_ms.max(1) as u64);
    let run = Arc::new_cyclic(|me: &std::sync::Weak<GenRun>| {
        let w = std::sync::Weak::clone(me);
        GenRun {
            gen,
            stop: Arc::clone(&stop),
            ev_tx,
            island: RwLock::new(None),
            l3_on_island: AtomicBool::new(false),
            identity: ident.clone(),
            tun_ip: crate::tunnel_addr::derive_tun_ip(&cfg.token.secret, &ident.public_key())
                .to_string(),
            link: Mutex::new(None),
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
    let fd_rx = shared.attach_receiver();
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
                let running = shared2.probe_running.load(Ordering::Acquire)
                    && shared2.healthy.load(Ordering::Acquire)
                    && snap.stage == TunStage::Attached;
                let why = lock_unpoison(&shared2.unhealthy_why).clone();
                let input = super::tun_status::TunStatusInput {
                    stage: snap,
                    running,
                    demand: demand2.last(),
                    demand_fg: demand2.fg(),
                    unhealthy_reason: (!why.is_empty()).then_some(why),
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

    // ---- 巡检 / stats / demand pusher（评审 r2-L3：线程体套 catch_unwind——Go 各
    // goroutine 有 recover，panic 分类可达）----
    let patrol_handle = spawn_derived(
        "homeway-patrol",
        Arc::clone(&run),
        "本世代失去自愈巡检（数据面不受影响）",
        move |r| patrol_loop(r, ev_rx),
    );
    let pusher_handle = spawn_derived(
        "homeway-demand-push",
        Arc::clone(&run),
        "本世代无需求下推（退化为巡检节拍）",
        demand_pusher_loop,
    );
    let stats_handle = spawn_derived(
        "homeway-stats",
        Arc::clone(&run),
        "本世代无统计行（对账缺一段观测）",
        move |r| stats_loop(r, cfg.stats_secs, cfg.diag_fd_secs),
    );

    // ---- 等 stop（世代主线程的最后一段；_finish guard 在 return 时统一收尾）----
    loop {
        if run.stop.load(Ordering::Acquire) {
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    (logf)("收到停止信号，正在回收（client/stack）");
    // 巡检/pusher/stats 随 stop 位退出（join 有界——**总预算 2s**，与 STOP_WAIT=3s
    // 留 1s 给桥停/client 收尾；评审 r2-H3：原每线程 2s 总 6s 恒超预算 ⇒ tun_stop
    // 常态化 -2。分片等待改造后三者 ≤~300ms 即退，2s 只是卡死兜底）
    let join_deadline = Instant::now() + Duration::from_secs(2);
    for h in [patrol_handle, pusher_handle, stats_handle]
        .into_iter()
        .flatten()
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
/// 交扩展重建（Go recover 后调 t.markUnhealthy("panic") 同义）。
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
                    "{name}: 线程 panic（已兜住）—— 标记隧道不健康交扩展重建"
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
    #[error("{0}")]
    Detail(String),
}

/// 岛侧候选装配（设计 §2.7 的**候选来源收窄**：只吃 token 的 QUIC 类端点 + 中继端点；
/// 学习缓存/hint 在 M1 只服务 WG 面）。
///
/// 中继候选的 `label` = `sha256(peerId)[:8]`（信封帧 `[0xAA][label8]` 的来源；真源
/// `legframe::relay_id`）——与出口侧腿表同源。
fn quic_candidates(tok: &Token) -> Result<Vec<homeway_quic::Candidate>, QuicFail> {
    let label = crate::legframe::relay_id(tok.peer_id.as_bytes());
    let mut out: Vec<homeway_quic::Candidate> = Vec::new();
    for e in &tok.endpoints {
        let via = match e.kind {
            crate::token::EndpointKind::Quic => homeway_quic::Via::Direct,
            crate::token::EndpointKind::Relay => homeway_quic::Via::Relay { label },
            // WG 类端点**不喂岛**（§2.1 末段「两族候选不得互相投喂」）
            crate::token::EndpointKind::Direct => continue,
        };
        let Ok(addr) = e.addr.parse::<SocketAddrV4>() else {
            continue; // 域名/非法形态不进岛候选（§2.7：域名解析面仍归 WG）
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

/// 岛不健康信号处置（M3 S4 §3.1：**QUIC 档的动作面在岛内快探阶梯**）。
///
/// 岛只在两种情形上报：
/// - **B（世代重建）**：连续 2 次 R 失败且窗 ≥10s ⇒ 本回调 = 交世代层重建（岛已无动作可做）；
/// - `fd`/`panic`：与 WG 面同源，直接分类（无「保连接」动作可做）。
///
/// 语义变化（M1 → M3 S4，登记 S7）：M1 时岛内无阶梯 ⇒ 连接死/迁移未确认即播 `patrol`
/// 并在此处试一次「Rebind 优先」（`quic_rebind_first`）；S4 起 M/R 由岛内阶梯全权处置，
/// 本函数不再做动作——收到信号即如实判不健康。
fn quic_unhealthy_signal(run: &Arc<GenRun>, reason: &str) {
    if reason == "patrol" {
        (run.logf)("岛上报不健康（patrol）—— 岛内快探阶梯已走完 M/R（B 门），交世代重建");
    }
    run.tun_shared.mark_unhealthy_if_current(run.gen, reason);
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
    let cands = quic_candidates(&cfg.token)?;
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
    let _ = island.tx().send(homeway_quic::Cmd::SetCandidates {
        cands: cands.clone(),
    });
    let n = cands.len();
    let outcome = island_cmd(
        &island,
        |reply| homeway_quic::Cmd::Connect {
            cands,
            budget: QUIC_CONNECT_BUDGET,
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

/// **QUIC 档（`l3_on_island() == true` 的世代）的 NAPI 恢复下推**（M4 §5.3；§15-1 裁定
/// 「本期修」）：**快探 + 一次复探**（`FAST_BUDGET` 700ms × `REPROBE_FACTOR` 2 = 1.4s，
/// 与**岛内快探阶梯同一判负粒度**）⇒ 通过 = `0`，两次都失败 = `-1`（「走完未恢复」⇒
/// tier 整套重建）。
///
/// **为什么只快探**（M4 §5.3 的原理由 + M5 C3）：动作面**只在岛内快探阶梯**——L3 在岛上
/// ⇒ 探活结论必须出自岛（世代层不做任何动作）。本档**不产 C11 族行**（该族已随 WG 阶梯删除）。
///
/// **为什么不是「什么都不做返 0」**：返 0 而零证据 = 谎报「某档通过」（本仓纪律：不谎报）；
/// 一次 700ms 快探（+ 复探）是**最便宜的真话**（M3 §3.2：快探 = 3.5s 内定音的唯一判据）。
///
/// **时间上界（实测 + 如实订正设计口径）**：正常形态 = 探段 700ms（首次探通）或
/// 700ms + 1.4s = 2.1s（复探才通/两次都败）；`l3_probe` 的 RPC 等待 = `budget +
/// QUIC_RPC_BUDGET(5s)`。设计 §5.3 记的「最坏 ≈2.1s + 5s = **7.1s**」**只计了一次 RPC
/// 余量**；按代码算的真实最坏 = (0.7s + 5s) + (1.4s + 5s) = **12.1s**（只有「岛 RPC 两次
/// 都卡满」的错配形态可达；首次探通时 = 0.7s + 5s = 5.7s）。**实施期订正（不静默降级）**：
/// 报主会话 + 落 `M4.md` 的 S6 登记补充（预登记 falsify 指标只对**实测**值设 8s 门）。
/// tier 侧是 `clientCoreTunRecoverAsync`（异步导出）⇒ 不占 JS 线程。
///
/// **rc 可达集（§8 行 13 登记）**：本档只产 `0/-1/-2`（`-2` = 既有的「无 attached/stale」
/// 前置，语义扩到「岛不在/未 attach」）；`-3/-4` 在本档**不可达**（回落世代仍可达——它走
/// WG 原路）。
///
/// **归因行（additive；M5 C3 去掉「按承载分档」半句——单承载下无第二档可指）**：
/// `quic: 恢复下推（%s）——岛快探%s：%s`。
fn recover_downpush_on_island(run: &Arc<GenRun>, cause: &str) -> i32 {
    use homeway_quic::tuning::probe_defaults::{FAST_BUDGET, REPROBE_FACTOR};
    // 不设 `deadline`（NAPI 面无线）；探段自带预算，RPC 等待亦为 `budget + QUIC_RPC_BUDGET`。
    let first = l3_probe(run, FAST_BUDGET);
    let (tag, verdict, rc) = match first {
        Ok(()) => ("", "通过".to_owned(), 0),
        Err(e1) => {
            // 一次复探（与岛内阶梯同粒度：`ladder.rs:222` 的 `fast_budget × reprobe_factor`）
            let budget = FAST_BUDGET.saturating_mul(REPROBE_FACTOR);
            match l3_probe(run, budget) {
                Ok(()) => ("+复探", "通过".to_owned(), 0),
                Err(e2) => (
                    "+复探",
                    format!("失败（{e1}；复探：{e2}）"),
                    -1,
                ),
            }
        }
    };
    (run.logf)(&format!("恢复下推（{cause}）——岛快探{tag}：{verdict}"));
    // 耗时口径：实测值（正常形态 0.7s / 2.1s 两档）由用例与真机读数钉（M4.md §S4）；
    // 上界只在**实测**面设门（预登记指标 ≤8s），不在此处断言（见函数头的订正）。
    rc
}

// ---------------------------------------------------------------------------
// 巡检（隧道域：demand 门控 + 失败当拍 R1 + 3 连败 R2 + 不健康交扩展）
// ---------------------------------------------------------------------------

fn patrol_loop(run: Arc<GenRun>, ev_rx: mpsc::Receiver<GenEvent>) {
    let mut fail_streak: u32 = 0;
    let mut last_loop_at = Instant::now();
    let mut last_counted: Option<Instant> = None;
    let mut gated = false;
    let mut probe_now = true; // attach 完就立即探（界面链路条不必空等第一个间隔）
    loop {
        if !probe_now {
            let next = last_loop_at + PATROL_INTERVAL;
            // 等到下一拍或 kick（回前台立即探）/ stop
            loop {
                if run.stop.load(Ordering::Acquire) {
                    (run.logf)("链路巡检退出（隧道已停止）");
                    return;
                }
                match ev_rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(GenEvent::Kick) => {
                        (run.logf)("链路巡检：App 回到前台，立即探测一次");
                        break;
                    }
                    Ok(GenEvent::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                        (run.logf)("链路巡检退出（隧道已停止）");
                        return;
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {
                        if Instant::now() >= next {
                            break;
                        }
                    }
                }
            }
        }
        probe_now = false;
        if run.stop.load(Ordering::Acquire) {
            (run.logf)("链路巡检退出（隧道已停止）");
            return;
        }
        // 挂起空窗检测（>2×间隔 = 进程被冻结过）：动作 = 岛内快探阶梯的 **M→R**
        //（§3.1/§3.2-3）——岛的拍内务同样看得见挂起空窗（`Ladder::due` 的 gap 判据）
        // 并立刻探活；本行只作世代层的现场留痕（世代层不再发动作）。
        let now = Instant::now();
        let gap = now.duration_since(last_loop_at);
        last_loop_at = now;
        if gap > 2 * PATROL_INTERVAL {
            (run.logf)(&format!(
                "巡检空窗 {}（判为进程被挂起）→ 阶梯恢复",
                crate::go_fmt::fmt_duration_go_secs(gap)
            ));
        }
        // 拍头取走 App 出站计数（自上一拍以来的出站 = 本拍需求的 TUN 位）——岛的
        // 同名接口（巡检只活在 attached 后；岛不在 ⇒ 防御性收口）。
        if run.current_island().is_none() {
            return;
        }
        let out_pkts = run.l3_swap_out_pkts();
        // 本拍探测（10s；岛 `Cmd::Probe` = `STREAM[probe]` 回显）
        let probe = l3_probe(&run, PROBE_TIMEOUT);
        // 成功面：链路快照（C10 形态；来源 = 岛快照）
        if let Ok(()) = probe {
            if let Some((via, ep, rtt_ms)) = run.island_link() {
                (run.logf)(&format!(
                    "link: via={via} ep={ep} rtt={rtt_ms}ms（新栈状态快照）"
                ));
                *lock_unpoison(&run.link) = Some(LinkIn {
                    via,
                    ep,
                    rtt_ms,
                    at_ms: now_unix_ms(),
                });
            }
        }
        // ---- 失败证据的需求门控（demand-driven-recovery）----
        let (demand_active, demand_why) = run.demand.patrol_demand(out_pkts, Instant::now());
        run.demand
            .note_demand(demand_active, demand_why, now_unix_ms());
        let probe_ok = probe.is_ok();
        let (new_streak, counted) = evidence_gate(
            demand_active,
            fail_streak,
            last_counted,
            now,
            probe_ok,
        );
        fail_streak = new_streak;
        if !probe_ok {
            if !counted {
                // 零流量需求期：该拍失败不构成路径质量证据
                if !gated {
                    gated = true;
                    (run.logf)(&format!("巡检失败被门控拦下（{demand_why}）→ 计数清零仅记录"));
                }
            } else {
                if gated {
                    gated = false;
                    (run.logf)(&format!("需求恢复（{demand_why}）：巡检失败重新计入证据"));
                }
                last_counted = Some(now);
                (run.logf)(&format!(
                    "对端巡检失败 {}/{}: {}",
                    fail_streak,
                    FAIL_STREAK_LADDER,
                    probe.unwrap_err()
                ));
                // 动作面**只在岛内快探阶梯**（§3.1：快探 700ms/背靠背 → 复探 →
                // M/R → B）——本拍只留痕（不重复动作、不抢跑；岛的动作快 240×）。
                (run.logf)("巡检失败 —— 动作面在岛内快探阶梯（§3.1；本拍不重复动作）");
            }
        } else if gated {
            // 成功拍：门控态结束的边沿（期间静默；计数已由纯函数清零）
            gated = false;
            (run.logf)("巡检恢复：门控态结束（成功拍清零）");
        }
        if fail_streak >= FAIL_STREAK_LADDER {
            // 动作面 = 岛内快探阶梯（§3.1 的 M/R/B）；本支路只是**兜底**
            //（岛的 B 门 = 连续 2 次 R 失败 + 窗 ≥10s ⇒ 10s 量级就会上报；走到这里
            //  说明岛面也异常）⇒ 如实上报 `patrol` 交世代重建。
            (run.logf)(
                "对端连续 3 次巡检不可达（岛内阶梯未见自愈）—— 兜底上报不健康，交扩展重建",
            );
            run.tun_shared.mark_unhealthy_if_current(run.gen, "patrol");
            return;
        }
    }
}

/// 证据门（hostsession.PatrolEvidenceGate 的分支：成功拍清零 / 零需求期不计证据）。
/// **M5 C3**：WG 档的「本地发送错误（环境噪声）」信号随 WG 面退役（无生产者）⇒ 该分支
/// 与 `noise_escalated` 一并删除；剩下的门控维度 = 需求位。
fn evidence_gate(
    demand: bool,
    fail_streak: u32,
    last_counted: Option<Instant>,
    now: Instant,
    probe_ok: bool,
) -> (u32, bool) {
    if probe_ok {
        return (0, false);
    }
    if !demand {
        return (0, false);
    }
    let fail_streak = if fail_streak > 0
        && last_counted.is_some_and(|t| now.duration_since(t) > PATROL_FAIL_WINDOW)
    {
        0
    } else {
        fail_streak
    };
    (fail_streak + 1, true)
}

// ---------------------------------------------------------------------------
// demand pusher（D4：App 出站新鲜 + 接收静默 → 立即下推阶梯）
// ---------------------------------------------------------------------------

fn demand_pusher_loop(run: Arc<GenRun>) {
    let mut last_push: Option<Instant> = None;
    let mut last_rx: u64 = 0;
    let mut recv_at: Option<Instant> = None;
    loop {
        if run.stop.load(Ordering::Acquire) {
            return;
        }
        // 分片等待（评审 r2-H3：整段 sleep(1s) 一样挡收工；100ms 片界查 stop）
        let next = Instant::now() + PUSHER_TICK;
        while Instant::now() < next {
            if run.stop.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(
                Duration::from_millis(100).min(next.saturating_duration_since(Instant::now())),
            );
        }
        if run.current_island().is_none() {
            continue;
        }
        // 需求信号（岛的**同名接口/快照面**）
        let rx = run.l3_rx();
        if rx != last_rx {
            last_rx = rx;
            recv_at = Some(Instant::now());
        }
        // 单调时基（评审 r2-H1：last_outbound_at 返回的 Instant 曾由 unix epoch ns
        // 换算——减出 56 年前的时刻 ⇒ should_push 恒 false、D4 整条静默失效。现改
        // TunCounters 的相对单调读数，unix ns 只留给 JSON 面）
        let out_at = run.l3_last_outbound_at();
        // 噪声窗 = 出站新鲜窗（评审 r2-L7：Go shouldPush 用 outboundFresh(5s)）。
        // **M5 C3**：WG 档的「采纳路径本地错误」信号随 WG 面退役（无生产者）⇒ 该维
        // 恒 `false`（`demand::should_push` 的入参保留：它是 Go `shouldPush` 判据链的
        // 逐条对照面；删维等于改判据语义 ⇒ 登记为观测面收窄）。
        let since = last_push.map(|t| t.elapsed());
        if demand::should_push(out_at, recv_at, Instant::now(), false, since) {
            last_push = Some(Instant::now());
            (run.logf)("待发包下推：App 出站新鲜且接收静默 → 立即下推阶梯（不等巡检拍）");
            // 下推动作 = 岛内快探阶梯（§3.1：在用档 700ms/背靠背 ⇒ 岛自己就是
            //「立即下推」；世代层不再重复动作）——本支路只留痕。
            (run.logf)("待发包下推 —— 岛内快探阶梯为准（§3.1；本拍不重复动作）");
        }
    }
}

// ---------------------------------------------------------------------------
// stats 线程（与设备 /proc/net/dev vpn-tun 行对表）
// ---------------------------------------------------------------------------

/// `diag_fd_secs` = 配置面字段（`tunConfig.diagFdSecs`）：**本核无消费**——fd 快照面在
/// OHOS 沙箱受限（Go 侧 `/proc/self/fd` 快照读不到有意义的东西），Q-F F8b 删掉
/// 「基线一行 + 周期快照」的死分支（分支体只改两个账面变量、无任何观测产出）；
/// 字段保留（删字段会动 `TunConfigJson` serde 面与 Go 配置对齐——登记 §7-9）。
fn stats_loop(run: Arc<GenRun>, stats_secs: i64, _diag_fd_secs: i64) {
    let tick = Duration::from_secs(stats_secs.max(STATS_SECS_MIN) as u64);
    loop {
        // 分片等待（评审 r2-H3：整段 sleep(tick)〔默认 60s〕让收工 join 白烧满预算
        // ⇒ tun_stop 常态化 -2 强制放锁——Go 统计 goroutine 的 select{tick, stop} 同义）
        let deadline = Instant::now() + tick;
        while Instant::now() < deadline {
            if run.stop.load(Ordering::Acquire) {
                return;
            }
            let nap =
                Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now()));
            std::thread::sleep(nap);
        }
        let (rd, wr) = run.l3_tun_stats();
        let c = run.pf.counters();
        (run.logf)(&stats_line(rd, wr, c.accepted(), c.fails()));
    }
}

/// `stats:` 行（纯函数——形态**逐字不变**；Q-F-B F3-2：接线真 pf 计数，单测直喂）。
fn stats_line(fd_read_bytes: u64, fd_write_bytes: u64, pf_accepted: u64, pf_fails: u64) -> String {
    format!("stats: fdReadBytes={fd_read_bytes}B fdWriteBytes={fd_write_bytes}B ｜ pf={pf_accepted}/{pf_fails}")
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
        assert_eq!(write_retry_backoff(0), Duration::from_millis(2));
        assert_eq!(write_retry_backoff(49), Duration::from_millis(2));
        assert_eq!(write_retry_backoff(50), Duration::from_millis(10));
        assert_eq!(write_retry_backoff(99), Duration::from_millis(10));
        assert_eq!(write_retry_backoff(100), Duration::from_millis(20));
        assert_eq!(write_retry_backoff(u32::MAX), Duration::from_millis(20));
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

    /// Q-F-B F3-2：`stats:` 行的**纯函数**（形态逐字不变 + 真值直喂）。
    #[test]
    fn stats_line_reports_real_pf_counts() {
        assert_eq!(
            stats_line(0, 0, 0, 0),
            "stats: fdReadBytes=0B fdWriteBytes=0B ｜ pf=0/0"
        );
        assert_eq!(
            stats_line(1234, 5678, 9, 2),
            "stats: fdReadBytes=1234B fdWriteBytes=5678B ｜ pf=9/2",
            "行文形态不变、值接线真计数"
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
        icfg.bind = Some(SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, 0));
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

    /// **S4（M4 §5.3；§15-1 裁定「本期修」）+ M5 C3 单腿**：NAPI `ClientCoreTunRecover`
    /// 的 rc 契约——
    /// ① 无世代 ⇒ `-2`；② 陈世代 ⇒ `-2`（两条既有前置，逐字）；
    /// ③ 岛承接 ⇒ **岛快探 + 一次复探**：无 live 连接 ⇒ 两次探都失败 ⇒ `-1`
    ///    （=「走完未恢复」⇒ tier 整套重建）+ **additive 行逐字**；且本世代**零**
    ///    `RECOVER` 族行（WG 阶梯已随 S2 删除 ⇒ 不存在第二条恢复路径）。
    #[test]
    fn recover_downpush_single_leg_rc_contract() {
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        // ① 无世代（executor 无 run）⇒ -2
        assert_eq!(exec.recover(3, "扩展下推(档位 3)"), -2, "无 attached 隧道");

        // ③ 岛承接：`l3_on_island() == true`
        let (run, lines) = gen_with_lines("island");
        let island = idle_island(&run);
        run.set_island(Arc::clone(&island));
        run.set_l3_on_island(true);
        *lock_unpoison(&exec.state) = Some(Arc::clone(&run));
        let rc = exec.recover(3, "扩展下推(档位 3)");
        assert_eq!(rc, -1, "两次探都失败 ⇒ -1（不是 0/-3/-4）");
        let seen = drain_lines(&lines);
        let line = "恢复下推（扩展下推(档位 3)）——岛快探+复探：失败（";
        assert!(
            seen.iter().any(|l| l.starts_with(line)),
            "additive 归因行须逐字在场（首缀 {line:?}）：{seen:?}"
        );
        assert_eq!(
            seen.iter().filter(|l| l.starts_with("RECOVER ")).count(),
            0,
            "零 RECOVER 族行（WG 阶梯已删除）：{seen:?}"
        );

        // ② 陈世代 ⇒ -2（既有前置；与判据无关）
        run.tun_shared.gen.store(99, Ordering::Release);
        assert_eq!(exec.recover(3, "扩展下推(档位 3)"), -2, "陈世代 -2");

        *lock_unpoison(&exec.state) = None;
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
}
