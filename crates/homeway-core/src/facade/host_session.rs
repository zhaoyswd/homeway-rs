//! **宿主会话**（M5 S2a；真源 `docs/reviews/M5-design.md` §2 全节 + §2.7 形态面）。
//!
//! 干什么：把「无 TUN 的服务会话」从 WG 承载换成 **QUIC 岛承载**——CLI host 面
//! （`connect`/`files --host`/`speedtest --host`/`term --host`）、daemon 的 client 角色
//! 承载面（forward/socks/speedtest 三 carrier 的拨号腿）与 App 服务会话
//! （`ClientCoreServiceStart`）三者**共用同一个会话抽象**（设计 §2.1 的同一 `Session`
//! 三消费方；岛化一次、三处同得）。
//!
//! 形态面（§2.7 R-1…R-7，**不照搬 Go `hostsession.Session` 的形状**）：
//!
//! | 项 | 本模块 |
//! |---|---|
//! | 句柄 | [`HostSession`]：`connect(&self, ServicePort, budget) -> Result<HostStream, HostErr>` |
//! | 「端口」 | newtype [`ServicePort`]，只经 [`ServicePort::from_bridge_port`] 构造（真源 = `quic_stream::tag_for_port`） |
//! | 探活 | [`ProbeOutcome`]（`Ok{rtt}`/`Timeout`/`NoFace`），**不是** `Result<(), ConnErr>` |
//! | 错误 | [`HostErr`]（thiserror；`io::Error` 映射**只此一处**） |
//! | 快照 | [`SessionSnapshot`]/[`LinkSnapshot`] 同形重建，字段来源 = **岛快照** |
//! | 生命周期 | `stop()` / `stop_within(deadline)`；`Drop` 不阻塞（有界收工） |
//! | 并发 | 单实例 + `&self` 方法（内部 `Mutex`/原子；**不引第二个 runtime**——岛自带） |
//!
//! 三条语义面（§2.4-A-2 的登记面，**实现按此收口**）：
//! ① **需求信号**：宿主会话无 TUN ⇒ `IslandSnapshot::{packets_in,packets_out,attached}`
//! 恒 0/false（数据面事实，不动）；「在用档」由**岛内 STREAM 动作信号**补
//! （`homeway-quic/src/driver.rs` 的 `last_stream_use_at`：开流成功 / 写出推进，同批落地）；
//! ② **`attached` 不得当就绪判据**（宿主会话恒 false ⇒ 用它判 = 假死）；本就绪判据 =
//! **准入完成**（`Live` 只在 `hr-reg4` 四帧后构造）+ 暖机探活；
//! ③ **`ladder_probe_ok` 只记阶梯探活**（显式 `Cmd::Probe` 不计）⇒ 本模块的读数行
//! 一律取**自己发出的 `Cmd::Probe` 结论**，不读 `ladder_probe_ok`。
//!
//! **恢复模型（2026-10-18 重构）**：岛内阶梯（快探/复探/M/R）照旧在岛线程动作；走完
//! B 门后由本层做**一次性**全名册重建。三个事件源经同一 `RebuildGate`（单飞 + 边沿
//! pending + demand 30s 限频）：B 门上报（边沿）、服务拨号失败（`NoFace`/`Closed`）、
//! 巡检观测（`NoFace` 或连续 3 拍超时）。worker 失败后**不自己重试**，等下一次事件；
//! 旧「自驱 `rebuild_loop` + 1→30s 指数退避」已退役。
//!
//! 有意缺口（设计 §2.6；S2a 内逐条处置）：
//! - **G5**（`daemon host reach` 过滤键）：本模块只提供会话；过滤键修正落在
//!   `daemon/hosts.rs`（WG 档的 `is_wg()` 反过滤 → `Quic|Relay`）。
//! - **G6**（`dnstest` 的 UDP 服务面无替代）：岛只有 STREAM + L3，无 UDP socket 服务
//!   ⇒ **登记退役**（排障工具，非产品四件套；`homeway` 侧同批处置：动词面无
//!   dispatch + 用法行除名）。
//! - **G7**（出口 `:5300` 解析腿在宿主会话无 tag 承载）：**2026-10-12 已裁「承接」并落地**
//!   （DNS-STREAM 批，用户指令）：新增 `STREAM[tag=6]`（`DnsResolve`，文本 `dns`）——
//!   出口侧并入既有服务入口（socketpair + `serve_via_intake`），服务体 =
//!   `server/dns_stream.rs`（每连接一线程，RFC1035 分帧 ⇄ `DnsProxy::answer_stream`）；
//!   客户端 `tag_for_port(5300)` = `Some(DnsResolve)` ⇒ socks **域名目标**恢复可用（CA5
//!   判据面恢复；机制面从「隧道 IP:5300 经拦截层回环豁免」改述为「`STREAM[tag=6]`」）。
//!   `DialErr::NoService` 保留给**真正无 tag** 的端口；「该 tag 在出口未装配」（`reset(0x22)`）
//!   另立 `DialErr::ServiceDisabled`（可行动文案，别混报成「连接被拒（对端 RST）」）。
//!   登记 = `docs/INTEROP-CRITERIA.md` 的 CA5 行 + 判据变更记录 2026-10-12 条 + 计数输入集表
//!   （`qtcp` 输入集新增 tag=6）。

use std::io::{self, Read, Write};
use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use homeway_quic::{
    Cmd, Island, IslandConfig, IslandCredential, RpkPublicKey, StreamErr, StreamId, StreamTag,
    TokenSecret,
};

use crate::identity::{self, Identity, IdentitySource};
use crate::token::Token;
use crate::Logf;

use super::bridge_io::{BridgeStream, WriteHalf};
use super::quic_stream::write_retry_backoff;

// ---------- 节拍常量（与连接行为真源同值；见 `tier:docs/agents/connection-lifecycle.md`） ----------

/// 暖机窗口：一发出口可达探活；超时 = 软失败（会话照常起，首个拨号触发注册/刷新）。
pub const WARM_TIMEOUT: Duration = Duration::from_secs(12);
/// 巡检间隔（保活/可达性观测 + link 快照刷新；也是 `Patrol` 重建事件的采样拍）。
pub const PATROL_INTERVAL: Duration = Duration::from_secs(60);
/// 巡检每拍探活预算。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 收工等收工上限（岛 `stop_within` 的窗口）。
pub const STOP_WAIT: Duration = Duration::from_secs(6);
/// 岛侧赛跑预算（覆盖 LAN/中继握手 + `hr-reg4` 四帧准入；与 `tun_exec` 同值）。
const CONNECT_BUDGET: Duration = Duration::from_secs(5);
/// 一次性重建的 demand 源最小间隔（边沿源绕过；与隧道域 `RECONNECT_MIN_GAP` 同值）。
const REBUILD_MIN_GAP: Duration = Duration::from_secs(30);
/// 重建 worker 的 spawn 重试（初次 + 2 次）。
const REBUILD_SPAWN_TRIES: u32 = 3;
/// 岛命令的有界等待预算（岛内短路径；超时 = 异常）。
const RPC_BUDGET: Duration = Duration::from_secs(5);

// ---------- 服务端口（newtype；§2.7 R-2） ----------

/// 一个**服务端口**（虚拟端口语义：7802 files / 7724 term / 7803 speedtest）。
///
/// 构造只经 [`ServicePort::from_bridge_port`]（内部 = `quic_stream::tag_for_port` 的
/// **同一真源**：未知端口 ⇒ `None` ⇒ 归因「无此服务」）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ServicePort(u16);

impl ServicePort {
    /// 由桥端口构造（未知端口 ⇒ `None`——「出口没有该服务」在拨号前就归因）。
    pub fn from_bridge_port(port: u16) -> Option<ServicePort> {
        super::quic_stream::tag_for_port(port).map(|_| ServicePort(port))
    }

    pub fn files() -> ServicePort {
        ServicePort(super::bridge_io::port::FILES)
    }

    pub fn term() -> ServicePort {
        // term 端口是**可配面**（`HOMEWAY_TERM_PORT`）——真源恒走 `term_port()`。
        ServicePort(super::bridge_io::term_port())
    }

    pub fn speedtest() -> ServicePort {
        ServicePort(super::bridge_io::port::SPEEDTEST)
    }

    /// 原端口值（判据行/日志面用）。
    pub fn get(self) -> u16 {
        self.0
    }

    /// 服务类别 tag（构造序保证 `Some`；`get()` 不再二次解析）。
    fn tag(self) -> StreamTag {
        super::quic_stream::tag_for_port(self.0).expect("构造序保证：端口必有 tag")
    }
}

// ---------- 探活结论 / 错误（§2.7 R-3/R-4） ----------

/// 探活结论（**不是** `Result<(), ConnErr>`；`NoFace` = 无会话面可探）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ProbeOutcome {
    /// 对端有证据（rtt = 本次实测往返）。
    Ok { rtt: Duration },
    /// 预算内无对端证据（连接在但没回音）。
    Timeout,
    /// 无会话面（岛不在/未建连）。
    NoFace,
}

/// 宿主会话错误（§2.7 R-4）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum HostErr {
    /// 岛不在（装配中/已收回/已收工）。
    #[error("宿主会话无面（岛不在/未建连）")]
    NoFace,
    /// 会话未就绪（尚未建连/正在收工）。
    #[error("宿主会话未就绪（{0}）")]
    NotReady(String),
    /// 出口无该服务（`StreamErr::Refused` 面；含 `dial` 目标拒）。
    #[error("出口没有该服务")]
    Refused,
    /// **出口未提供该服务腿**（`StreamErr::NotSupported` 面 = `reset(0x22)`：该 tag 在出口
    /// 未装配/已停用，如 `dns_port=0` 时 DNS 代答未启用）。与 [`Self::Refused`] 分开：
    /// Refused = 服务在但被拒；本支 = **本档没有这条路**（DNS-STREAM 批：承接后 `0x22` 若
    /// 混报成「连接被拒（对端 RST）」= M5 代码门 H-2 点名的归因误导复现）。
    #[error("出口未提供该服务腿（服务未装配/已停用）")]
    ServiceDisabled,
    /// 预算耗尽。
    #[error("拨号预算耗尽")]
    Budget,
    /// 本端服务流额度耗尽（`StreamErr::Busy`；M5 代码门 M-6 —— 此前被并进
    /// `NotReady` ⇒ 承载面报成「会话不在（收工/重建窗口）」，归因误导）。
    #[error("出口服务并发额度已满（Busy）")]
    Busy,
    /// 会话已关闭（连接死/被替换/本端已收工）。
    #[error("会话已关闭（{0}）")]
    Closed(String),
    /// 装配期失败（token/身份/候选/岛启动）。
    #[error("会话装配失败：{0}")]
    Start(String),
}

impl From<HostErr> for io::Error {
    /// 桥面/承载面需要的 `io::ErrorKind` 映射（**只在这一处**写，§2.7 R-4）。
    fn from(e: HostErr) -> io::Error {
        let kind = match e {
            HostErr::NoFace | HostErr::NotReady(_) => io::ErrorKind::NotConnected,
            // 服务未装配与「服务在但被拒」在**桥面**同档（link_down 判据），只有拨号缝
            // （`daemon::map_dial_err` / 解析腿）细分文案 ⇒ io kind 保持 ConnectionRefused。
            HostErr::Refused | HostErr::ServiceDisabled => io::ErrorKind::ConnectionRefused,
            HostErr::Budget => io::ErrorKind::TimedOut,
            // Busy = 本端额度面（不是连接面）：`WouldBlock` 让调用方按「稍后重试」处理，
            // 且不与 refused-like（桥面 link_down 回帧判据）混淆。
            HostErr::Busy => io::ErrorKind::WouldBlock,
            HostErr::Closed(_) => io::ErrorKind::Other,
            HostErr::Start(_) => io::ErrorKind::Other,
        };
        io::Error::new(kind, e.to_string())
    }
}

impl HostErr {
    /// `StreamErr` 的归因（**阶梯豁免集**照旧：服务级拒绝不重试、不升级——设计
    /// `quic_stream.rs` 的三条纪律；此处只做类型搬运）。
    fn from_stream(e: StreamErr) -> HostErr {
        match e {
            StreamErr::NotSupported => HostErr::ServiceDisabled,
            StreamErr::Refused => HostErr::Refused,
            StreamErr::Timeout => HostErr::Budget,
            StreamErr::Busy => HostErr::Busy,
            StreamErr::Closed | StreamErr::ConnectionLost => HostErr::Closed(e.to_string()),
            other => HostErr::NotReady(other.to_string()),
        }
    }

    /// `io::Error`（`quic_stream` 缝的归错面；kind 已是语义面）→ `HostErr`（类型搬运）。
    fn from_io(e: io::Error) -> HostErr {
        match e.kind() {
            io::ErrorKind::ConnectionRefused => HostErr::Refused,
            io::ErrorKind::TimedOut => HostErr::Budget,
            io::ErrorKind::WouldBlock => HostErr::Busy,
            io::ErrorKind::NotConnected => HostErr::NoFace,
            _ => HostErr::Closed(e.to_string()),
        }
    }
}

// ---------- 状态面（K13 同形重建；`status_json`/`hosts` 键面消费） ----------

/// 会话状态（状态面 `state` 值；字符串与既有 `svcState*` 同串——键面零改动）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessState {
    Starting,
    Ready,
    Failed,
    Stopping,
    Idle,
}

impl SessState {
    pub fn as_str(self) -> &'static str {
        match self {
            SessState::Starting => "starting",
            SessState::Ready => "ready",
            SessState::Failed => "failed",
            SessState::Stopping => "stopping",
            SessState::Idle => "idle",
        }
    }
}

/// 链路快照（link 段；`at_ms` = unix 毫秒，0 = 未探过）。
#[derive(Debug, Clone, Default)]
pub struct LinkSnapshot {
    pub via: String,
    pub ep: String,
    pub rtt_ms: i64,
    pub at_ms: i64,
}

/// 会话快照（状态 JSON 的数据源；**字段来源 = 岛快照**，形态与既有键面同形）。
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub state: SessState,
    pub reason: String,
    pub since: Instant,
    pub link: Option<LinkSnapshot>,
    pub identity: Option<(String, String)>,
    /// `(rx, tx)` = 岛的服务流字节累计（宿主会话无 L3；见模块头「三条语义面」①——
    /// 与 WG 档 `Client::snapshot()` 的 rx/tx **同键位、异来源**，登记于 `M5.md`）。
    pub stats: Option<(u64, u64)>,
}

/// 恢复档位（K13：capi `ClientCoreTunRecover(from)` 的 rc 契约输入）。
///
/// M5 起岛自带走内阶梯（快探→复探→M/R→B），本枚举只剩**外部入参的钳位**语义
/// （`Level::clamp`）——`R1/R2/R3` 的档位名**不再进判据行**（C11 族随 WG 阶梯删除，
/// 设计 §8.1：整族删除 ⇒ 名字面保留只为兼容外部入参、不产出行）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    R1 = 1,
    R2 = 2,
    R3 = 3,
}

impl Level {
    /// 把外部输入（capi/CLI int）钳到合法档位（语义与既有 `clampRecoverLevel` 同）。
    pub fn clamp(from: i64) -> Level {
        match from {
            ..=1 => Level::R1,
            2 => Level::R2,
            _ => Level::R3,
        }
    }

    /// 档位数值（**仅供日志留痕**；档位名与 C11 族已随 WG 阶梯删除，不产判据行）。
    pub fn raw(self) -> i64 {
        self as i64
    }
}

// ---------- 装配配置 ----------

/// 宿主会话装配参数。
pub struct HostSessionConfig {
    pub token: Token,
    /// 身份目录（`None` = 临时身份）。
    pub identity_dir: Option<PathBuf>,
    /// 原始日志面。**本模块在其上加 `服务会话: ` 前缀**——与 WG 档
    /// `session::Session::start` 的前缀口径**逐字相同**（R2 前缀硬规则；daemon 的
    /// `hosts: <id> ` 等外层前缀由调用方自带，本层不吞不改）。
    pub logf: Logf,
}

// ---------- 会话本体 ----------

/// 服务会话的一次性重建门（与隧道域同一模式：单飞 + 边沿 pending + demand 30s 限频）。
///
/// 2026-10-18：退役「自驱 `rebuild_loop` + 指数退避」——B 门/拨号失败/巡检观测都经
/// 本门触发**一次**短命 worker；失败后不自己重试，等下一次事件。`state` 只用两 bit
/// （bit0 INFLIGHT / bit1 PENDING），不再有轮次/退避状态。
struct RebuildGate {
    state: AtomicU32,
    /// demand 源上次起跑时刻（边沿源不读不写）。
    last: Mutex<Option<Instant>>,
    /// 【test-seam】实际占位/起跑次数（闸语义的单测观测面；生产零开销）。
    #[cfg(test)]
    spawns: AtomicU64,
}

const REBUILD_INFLIGHT: u32 = 1;
const REBUILD_PENDING: u32 = 2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum RebuildSource {
    /// B 门（岛内阶梯走完）——边沿，绕过 30s。
    BGate,
    /// 服务拨号失败（NoFace/Closed）——demand。
    Dial,
    /// 巡检观测（NoFace 或连续 3 拍超时）——demand。
    Patrol,
}

impl RebuildSource {
    fn is_edge(self) -> bool {
        matches!(self, RebuildSource::BGate)
    }
    const fn text(self) -> &'static str {
        match self {
            RebuildSource::BGate => "B门",
            RebuildSource::Dial => "拨号",
            RebuildSource::Patrol => "巡检",
        }
    }
}

struct Inner {
    /// 当前岛（收工 `take`；`None` = 未建/已收回）。
    island: Mutex<Option<Arc<Island>>>,
    /// 世代号（收工/重建递增；dial 入口据此拒绝陈旧句柄）。
    gen: AtomicU64,
    stop: AtomicBool,
    snapshot: Mutex<SessionSnapshot>,
    logf: Logf,
    /// 巡检线程柄（有界 join）。
    patrol: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 候选名册（token 端点派生；世代重建重赛跑用——与初次建连同一份）。
    cands: Vec<homeway_quic::Candidate>,
    /// 一次性重建门（单飞 + 边沿 pending + demand 限频）。
    rebuild_gate: RebuildGate,
    /// 当前重建 worker 柄（短命；收工时有界 join）。
    rebuild: Mutex<Option<std::thread::JoinHandle<()>>>,
}

/// 宿主会话句柄（单实例 + `&self`；`Arc<HostSession>` 供桥/承载面共享）。
pub struct HostSession {
    inner: Arc<Inner>,
}

impl HostSession {
    /// 装配 + 建连（+ 暖机探活 + 起巡检）。
    ///
    /// 失败面（对齐既有 `Session::start` 的 rc/状态语义）：
    /// - **装配类**（token 无 QUIC 端点/身份不可用/岛起不来）⇒ `Err(HostErr::Start)`;
    /// - **暖机硬失败**（建连后探活报「无连接面」）⇒ `Ok(session)` 且
    ///   `snapshot().state == Failed`（上层读原因；与 WG 档同形——「Ok-but-failed」）。
    pub fn start(cfg: HostSessionConfig) -> Result<HostSession, HostErr> {
        // 前缀口径与 WG 档 session::Session::start 逐字相同（`服务会话: {s}`）——
        // 换承载不改行形（判据行的前缀是 R2 硬规则）。
        let logf: Logf = {
            let raw = cfg.logf;
            Arc::new(move |s: &str| raw(&format!("服务会话: {s}")))
        };
        (logf)("启动（无 TUN 服务会话）"); // C12（M3 起岛化，行文不变）

        // 候选（QUIC 类 + 中继类端点；域名端点解析一次——设计 §2.4-A5 的承接面）。
        let cands = host_candidates(&cfg.token, &logf)?;
        let (ident, src, warn) = identity::load_or_create(cfg.identity_dir.as_deref(), &cfg.token.peer_id)
            .map_err(|e| HostErr::Start(format!("身份装配失败：{e}")))?;
        let dir_text = cfg
            .identity_dir
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        match src {
            IdentitySource::Created => (logf)(&format!(
                "身份：新建（dev={} pub={}，目录 {dir_text}）",
                ident.short_dev(),
                ident.short_pub()
            )),
            IdentitySource::Reused => (logf)(&format!(
                "身份：复用（dev={} pub={}）",
                ident.short_dev(),
                ident.short_pub()
            )),
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

        // 后端隧道地址（C3 第二字段；hw-tun 派生——与出口设备表同源）。
        let tunnel_ip = crate::tunnel_addr::derive_tunnel_ip(&cfg.token.secret, &ident.public_key());
        // C3（第二字段 = 本设备派生隧道地址）。
        (logf)(&format!(
            "新栈会话已建立（token 端点 {} 个，后端隧道地址 {}）",
            cfg.token.endpoints.len(),
            tunnel_ip
        ));

        let inner = Arc::new(Inner {
            island: Mutex::new(None),
            gen: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            snapshot: Mutex::new(SessionSnapshot {
                state: SessState::Starting,
                reason: String::new(),
                since: Instant::now(),
                link: None,
                identity: Some((ident.short_dev(), ident.short_pub())),
                stats: Some((0, 0)),
            }),
            logf: Arc::clone(&logf),
            patrol: Mutex::new(None),
            cands: cands.clone(),
            rebuild_gate: RebuildGate {
                state: AtomicU32::new(0),
                last: Mutex::new(None),
                #[cfg(test)]
                spawns: AtomicU64::new(0),
            },
            rebuild: Mutex::new(None),
        });

        let island = start_island(&cfg.token, &ident, &cands, &inner)?;
        *crate::syncutil::lock_unpoison(&inner.island) = Some(Arc::clone(&island));
        let n = cands.len();
        let outcome = island_cmd(
            &island,
            |reply| Cmd::Connect {
                cands: cands.clone(),
                budget: CONNECT_BUDGET,
                demote_adopted: false,
                reply,
            },
            CONNECT_BUDGET + RPC_BUDGET,
        );
        match outcome {
            Ok(o) => (logf)(&format!(
                "宿主会话已建连（候选 {n} 个，胜出 {} {}，耗时 {}ms）",
                o.via.text(),
                o.winner,
                o.elapsed_ms
            )),
            Err(e) => {
                // 建连失败 = 装配类硬失败（清岛 + 报装配错误——上层按「会话失败」收口）。
                *crate::syncutil::lock_unpoison(&inner.island) = None;
                let _ = island.stop_within(Instant::now() + STOP_WAIT);
                return Err(HostErr::Start(format!("岛建连失败：{e}")));
            }
        }

        // ---- 暖机：一发出口可达探活（WARM_TIMEOUT；超时 = 软失败）----
        let started = Instant::now();
        let mut warm_failed = false;
        match probe_once(&inner, WARM_TIMEOUT) {
            ProbeOutcome::Ok { rtt } => {
                set_link(&inner, rtt);
                (logf)(&format!("暖机就绪（出口可达，rtt={}ms）", rtt.as_millis())); // C7
            }
            ProbeOutcome::Timeout => (logf)(&format!(
                "暖机 {} 内出口未应答：按软失败继续（首个拨号会触发注册）",
                crate::go_fmt::fmt_duration_go_ms(WARM_TIMEOUT)
            )),
            ProbeOutcome::NoFace => {
                // 不在这里 `return Ok(...)`（评审 2026-10-20 A12）：那条路径会返回一个
                // **没有巡检线程**的 failed 会话，之后只能等外部事件救——现在照常起
                // patrol，巡检的 NoFace 分支会触发一次性重建。
                warm_failed = true;
                set_state(&inner, SessState::Failed, "出口不可达：无连接面");
                (logf)("暖机硬失败：无连接面（出口不可达）——仍启动巡检，由巡检事件重连");
            }
        }
        if !warm_failed {
            set_state(&inner, SessState::Ready, "");
            (logf)("就绪（会话在位，无桥直通）"); // C16
        }
        let _ = started;

        // ---- 巡检线程（link 快照刷新 + 失败留痕；**恢复动作在岛内阶梯**）----
        let sh = Arc::clone(&inner);
        let logf2 = Arc::clone(&logf);
        let patrol = match std::thread::Builder::new()
            .name("homeway-host-patrol".into())
            .spawn(move || patrol_loop(sh))
        {
            Ok(h) => Some(h),
            Err(e) => {
                crate::syncutil::log_spawn_failed(
                    &logf2,
                    "宿主会话巡检线程",
                    &e,
                    "本会话失去巡检与 link 快照刷新（拨号面不受影响）",
                );
                None
            }
        };
        *crate::syncutil::lock_unpoison(&inner.patrol) = patrol;
        Ok(HostSession { inner })
    }

    /// 拨一条服务流（§2.7 R-1：budget = 调用方预算，首试 + 一次重试由
    /// `quic_stream::dial_with` 的策略收口——服务级拒绝不重试）。
    pub fn connect(&self, port: ServicePort, budget: Duration) -> Result<HostStream, HostErr> {
        let r = self.open_tag(port.tag(), budget);
        self.note_dial_result(&r);
        r
    }

    /// 拨号失败若是承载类（无面/连接死）⇒ 触发一次性重建（demand 源，30s 限频）。
    /// 服务级拒绝（Refused/ServiceDisabled/Busy/Budget）不触发——那是对端业务面。
    fn note_dial_result(&self, r: &Result<HostStream, HostErr>) {
        if matches!(r, Err(HostErr::NoFace | HostErr::Closed(_))) {
            try_rebuild(&self.inner, RebuildSource::Dial);
        }
    }

    /// 拨一条**端口转发**腿（`STREAM[dial]` + 6B 目标 + 1B 回执；M4 的 pf 缝同款——
    /// 回执余量随句柄带回，由读面先出）。
    pub fn dial_addr(&self, dst: SocketAddrV4, budget: Duration) -> Result<HostStream, HostErr> {
        let r = (|| {
            let island = self.island()?;
            let (id, rest) = super::quic_stream::dial_target_raw(&island, dst, budget)
                .map_err(HostErr::from_io)?;
            Ok(HostStream {
                shared: Arc::new(StreamShared {
                    island,
                    id,
                    pending: Mutex::new(super::quic_stream::PendingBuf::new(rest)),
                    no_progress: super::quic_stream::WRITE_NO_PROGRESS,
                }),
            })
        })();
        self.note_dial_result(&r);
        r
    }

    /// 探活（显式一发；结论就是本函数返回值——**不读** `ladder_probe_ok`，见模块头 ③）。
    pub fn path_probe(&self, budget: Duration) -> ProbeOutcome {
        probe_once(&self.inner, budget)
    }

    /// 状态快照（键面同形；来源 = 岛快照）。
    pub fn snapshot(&self) -> SessionSnapshot {
        let mut s = crate::syncutil::lock_unpoison(&self.inner.snapshot).clone();
        s.stats = self.island_stats();
        s
    }

    /// 恢复下推（capi `ClientCoreTunRecover` 的 rc 契约；**岛内阶梯是动作面**）。
    ///
    /// 语义 = 「快探 + 一次复探」：通过 ⇒ `0`；两次都失败 ⇒ `-1`（走完未恢复 ⇒ 上层
    /// 整套重建）；无面 ⇒ `-2`。**不产 R1/R2/R3 档位行**（C11 族已删，设计 §8.1）。
    /// `from`（档位）只作留痕——岛内阶梯自定档，外部档位不驱动动作（登记见 `M5.md`）。
    pub fn recover(&self, from: Level, cause: &str) -> i32 {
        use homeway_quic::tuning::probe_defaults::{FAST_BUDGET, REPROBE_FACTOR};
        let first = self.path_probe(FAST_BUDGET);
        let (tag, verdict, rc) = match first {
            ProbeOutcome::Ok { .. } => ("", "通过".to_owned(), 0),
            ProbeOutcome::NoFace => ("", "无连接面".to_owned(), -2),
            ProbeOutcome::Timeout => {
                let budget = FAST_BUDGET.saturating_mul(REPROBE_FACTOR);
                match self.path_probe(budget) {
                    ProbeOutcome::Ok { .. } => ("+复探", "通过".to_owned(), 0),
                    ProbeOutcome::NoFace => ("+复探", "无连接面".to_owned(), -2),
                    ProbeOutcome::Timeout => ("+复探", "失败（预算内无对端证据）".to_owned(), -1),
                }
            }
        };
        (self.inner.logf)(&format!(
            "宿主会话恢复下推（{cause}；入参=档位 {}）：岛快探{tag} → {verdict}",
            from.raw()
        ));
        rc
    }

    /// 收工（幂等；有界——到点 detach 由岛的收割线程收口）。
    pub fn stop(&self) {
        if self.inner.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        set_state_unless_failed(&self.inner, SessState::Stopping, "");
        let deadline = Instant::now() + STOP_WAIT;
        if let Some(h) = crate::syncutil::lock_unpoison(&self.inner.patrol).take() {
            if !crate::syncutil::join_bounded(h, deadline) {
                (self.inner.logf)("收工等待巡检线程超时（STOP_WAIT）——放行自退");
            }
        }
        if let Some(h) = crate::syncutil::lock_unpoison(&self.inner.rebuild).take() {
            if !crate::syncutil::join_bounded(h, deadline) {
                (self.inner.logf)("收工等待重建线程超时（STOP_WAIT）——放行自退");
            }
        }
        let island = crate::syncutil::lock_unpoison(&self.inner.island).take();
        self.inner.gen.fetch_add(1, Ordering::Release);
        if let Some(i) = island {
            if !i.stop_within(deadline) {
                (self.inner.logf)("收工等待岛线程超时（STOP_WAIT）——放行自退（岛由收割线程收口）");
            }
        }
        if set_state_unless_failed(&self.inner, SessState::Idle, "已收工") {
            (self.inner.logf)(&format!("已收工（state={}）", SessState::Idle.as_str())); // C17
        } else {
            (self.inner.logf)("已收工（state=failed 终态保留）");
        }
    }

    /// 带期限收工（服务域 `stop` 的既有节奏）。
    ///
    /// 返回值语义（2026-10-10 审查 低-11 澄清，行为保持既有）：`true` = **岛本体**
    /// 收工无异常——赶上了调用方期限，或虽超期但自身耗时 < [`STOP_WAIT`]（超期可能
    /// 来自同预算里巡检线程的先行消耗，不该让岛背 detach 的锅）；`false` = 岛本体
    /// 自身耗时 ≥ [`STOP_WAIT`]（真卡死面）⇒ 调用方走 detach/收割线程兜底。
    pub fn stop_within(&self, deadline: Instant) -> bool {
        let t0 = Instant::now();
        self.stop();
        Instant::now() <= deadline || t0.elapsed() < STOP_WAIT
    }

    /// 已收工谓词（诊断/测试面）。
    pub fn is_stopped(&self) -> bool {
        self.inner.stop.load(Ordering::Acquire)
    }

    fn island(&self) -> Result<Arc<Island>, HostErr> {
        if self.inner.stop.load(Ordering::Acquire) {
            return Err(HostErr::Closed("本会话已收工".to_owned()));
        }
        crate::syncutil::lock_unpoison(&self.inner.island)
            .clone()
            .ok_or(HostErr::NoFace)
    }

    fn open_tag(&self, tag: StreamTag, budget: Duration) -> Result<HostStream, HostErr> {
        let island = self.island()?;
        let id = super::quic_stream::dial_stream_id(&island, &self.inner.logf, tag, budget)
            .map_err(HostErr::from_stream)?;
        Ok(HostStream {
            shared: Arc::new(StreamShared {
                island,
                id,
                pending: Mutex::new(super::quic_stream::PendingBuf::default()),
                no_progress: super::quic_stream::write_no_progress(tag),
            }),
        })
    }

    fn island_stats(&self) -> Option<(u64, u64)> {
        let i = crate::syncutil::lock_unpoison(&self.inner.island).clone()?;
        let s = i.snapshot();
        Some((s.stream_bytes_in, s.stream_bytes_out))
    }

    /// 【test-seams】合成会话（服务域 Ok-but-failed / Ready 两分支的注入面；
    /// 形态照既有 `Session::synthetic_*`——真惰性岛：候选不可达，起得来发不出）。
    #[cfg(test)]
    pub(crate) fn synthetic_for_test(state: SessState, reason: &str) -> HostSession {
        let ident = Identity::ephemeral().expect("临时身份");
        let logf: Logf = Arc::new(|_s: &str| {});
        let inner = Arc::new(Inner {
            island: Mutex::new(None),
            gen: AtomicU64::new(1),
            stop: AtomicBool::new(false),
            snapshot: Mutex::new(SessionSnapshot {
                state,
                reason: reason.to_owned(),
                since: Instant::now(),
                link: None,
                identity: Some((ident.short_dev(), ident.short_pub())),
                stats: Some((0, 0)),
            }),
            logf,
            patrol: Mutex::new(None),
            cands: Vec::new(),
            rebuild_gate: RebuildGate {
                state: AtomicU32::new(0),
                last: Mutex::new(None),
                #[cfg(test)]
                spawns: AtomicU64::new(0),
            },
            rebuild: Mutex::new(None),
        });
        HostSession { inner }
    }
}

impl Drop for HostSession {
    fn drop(&mut self) {
        self.stop();
    }
}

// ---------- 岛装配 ----------

/// token 端点 → 岛候选（**只吃 QUIC 类**：`Quic` ⇒ 直连、`Relay` ⇒ 中继；WG 类端点
/// 不喂岛）。域名形态端点**在本模块解析一次**（设计 §2.4-A5 的承接：`--ddns` 发布的
/// 域名端点在宿主会话里仍可用）；解析失败/无 v4 记录 ⇒ 记行跳过（不静默）。
fn host_candidates(tok: &Token, logf: &Logf) -> Result<Vec<homeway_quic::Candidate>, HostErr> {
    use crate::token::EndpointKind;
    let label = crate::legframe::relay_id(tok.peer_id.as_bytes());
    let mut out: Vec<homeway_quic::Candidate> = Vec::new();
    for e in &tok.endpoints {
        let via = match e.kind {
            EndpointKind::Quic => homeway_quic::Via::Direct,
            EndpointKind::Relay => homeway_quic::Via::Relay { label },
            EndpointKind::Direct => continue, // WG 类端点（不喂岛）
        };
        let addrs: Vec<SocketAddr> = if let Ok(a) = e.addr.parse::<SocketAddr>() {
            vec![a]
        } else {
            // 域名形态：解析一次（有界预算；A/AAAA 都进候选——岛 socket 双栈）
            resolve_endpoints(&e.addr, logf)
        };
        for addr in addrs {
            if out.iter().any(|c| c.addr == addr) {
                continue; // 同址去重（先到先得）
            }
            out.push(homeway_quic::Candidate { addr, via });
        }
    }
    if out.is_empty() {
        // 与 WG 档同串（`SessionErr::NoCandidates` 的 rc/失败面语义等价）。
        return Err(HostErr::Start("token 里没有任何可用端点".to_owned()));
    }
    Ok(out)
}

/// 域名端点 → 全部 A/AAAA 地址（有界预算；失败/无记录 ⇒ 记行 + 空列表）。
fn resolve_endpoints(addr: &str, logf: &Logf) -> Vec<SocketAddr> {
    let Some((host, port)) = crate::hostdns::split_host_port(addr) else {
        return Vec::new();
    };
    let ips = crate::hostdns::lookup_host(
        &host,
        crate::hostdns::PROBE_SYNC_BUDGET,
        crate::hostdns::ResolveLane::Critical,
    )
    .unwrap_or_default();
    let mut out: Vec<SocketAddr> = Vec::new();
    for ip in ips {
        let a = SocketAddr::new(ip, port);
        if !out.contains(&a) {
            out.push(a);
        }
    }
    if out.is_empty() {
        (logf)(&format!(
            "宿主会话：域名端点 {addr} 无可用 A/AAAA 记录（本世代跳过该端点）"
        ));
    }
    out
}

/// 起岛（凭据 → `IslandConfig` → 回调/候选注入）。
fn start_island(
    tok: &Token,
    ident: &Identity,
    cands: &[homeway_quic::Candidate],
    inner: &Arc<Inner>,
) -> Result<Arc<Island>, HostErr> {
    let Some(rpk) = tok.rpk else {
        return Err(HostErr::Start("token 不含服务端 RPK（QUIC 档必须）".to_owned()));
    };
    let cred = IslandCredential::new(
        TokenSecret::from_bytes(*tok.secret.as_bytes()),
        ident.public_key(),
        *ident.dev_tag().as_bytes(),
        RpkPublicKey::from_bytes(*rpk.as_bytes()),
    );
    let mut icfg = IslandConfig::new(cred);
    icfg.patrol = PATROL_INTERVAL;
    // 岛内阶梯 R 成功采纳 = 连接已恢复的**权威沿**（评审 2026-10-20 A12）：没有这个回调时，
    // 岛自己恢复了、facade 却可能永远停在 failed（run_rebuild 被 RaceInFlight 让路/巡检只刷
    // link 的路径都会留下状态差）。回调只把 failed 拉回 ready，不覆盖 stopping/idle。
    {
        let w = Arc::downgrade(inner);
        icfg.on_adopt = Some(Arc::new(move || {
            if let Some(i) = w.upgrade() {
                on_island_adopted(&i);
            }
        }));
    }
    // 绑定族按候选面选（纯 v4 = v4 socket；含 v6 = 双栈 `[::]:0`）。
    icfg.bind = if cands.iter().any(|c| c.addr.is_ipv6()) {
        None
    } else {
        Some(SocketAddr::from((std::net::Ipv4Addr::UNSPECIFIED, 0)))
    };
    // MTU 上限：宿主会话恒用缺省（`tunConfig.quicMtuCap` 属隧道域旋钮；宿主会话无 L3）。
    let w = Arc::downgrade(inner);
    let on_unhealthy: homeway_quic::OnUnhealthy = Arc::new(move |reason: &str| {
        if let Some(i) = w.upgrade() {
            on_island_unhealthy(&i, reason);
        }
    });
    let island = Island::start(Arc::clone(&inner.logf), on_unhealthy, icfg)
        .map_err(|e| HostErr::Start(format!("岛启动失败：{e}")))?;
    let island = Arc::new(island);
    // 「立即装定」显式面（与 tun_exec 同款：构造期回调已是同一份，这一发是显式面）。
    let w2 = Arc::downgrade(inner);
    let h: homeway_quic::OnUnhealthy = Arc::new(move |reason: &str| {
        if let Some(i) = w2.upgrade() {
            on_island_unhealthy(&i, reason);
        }
    });
    let _ = island.tx().send(Cmd::SetOnUnhealthy { h });
    let _ = island.tx().send(Cmd::SetCandidates {
        cands: cands.to_vec(),
    });
    Ok(island)
}

// ---------- 一次性重建（B 门 / 拨号 / 巡检事件触发） ----------

/// 岛内阶梯/赛跑采纳了新连接：**连接已恢复的权威证据**。
///
/// 只把 `failed` 拉回 `ready`（评审 2026-10-20 A12）：`run_rebuild` 因 `RaceInFlight`
/// 让路、或巡检只刷 link 的路径，都可能留下「岛已经恢复、facade 仍报 failed」的状态差；
/// 上层据 failed 报 `service/failed`，用户看到列表/终端不可用。`starting/stopping/idle`
/// 不动（装配与收工有各自的写手，旧岛不得把新状态翻回去）。
fn on_island_adopted(inner: &Arc<Inner>) {
    if inner.stop.load(Ordering::Acquire) {
        return;
    }
    let mut s = crate::syncutil::lock_unpoison(&inner.snapshot);
    if s.state != SessState::Failed {
        return;
    }
    s.state = SessState::Ready;
    s.reason.clear();
    s.since = Instant::now();
    drop(s);
    (inner.logf)("宿主会话：岛内阶梯采纳新连接 —— 状态由 failed 回 ready");
}

/// 岛上报不健康：如实置 failed + 触发**一次**重建（边沿源，绕过节流）。
fn on_island_unhealthy(inner: &Arc<Inner>, reason: &str) {
    (inner.logf)(&format!(
        "⚠️ 宿主会话不健康（{reason}）——岛内阶梯已尽力；置 failed 并触发一次性重建"
    ));
    set_state(inner, SessState::Failed, &format!("岛不健康（{reason}）；一次性重连中"));
    try_rebuild(inner, RebuildSource::BGate);
}

/// 一次性重建的统一入口（任意线程；短命 worker）。
///
/// - 边沿源（B 门）绕过 30s，且在飞时用 `PENDING` 留字（worker 收尾承接）；
/// - demand 源（拨号/巡检）受 `REBUILD_MIN_GAP=30s` 限频；
/// - 单飞 = `REBUILD_INFLIGHT`；worker 失败后**不自己重试**，等下一次事件。
fn try_rebuild(inner: &Arc<Inner>, source: RebuildSource) {
    if inner.stop.load(Ordering::Acquire) {
        return;
    }
    if !source.is_edge() {
        let mut last = crate::syncutil::lock_unpoison(&inner.rebuild_gate.last);
        if last.is_some_and(|t| t.elapsed() < REBUILD_MIN_GAP) {
            return;
        }
        *last = Some(Instant::now());
    }
    loop {
        // **CAS 协议**（评审 2026-10-20 A1）：两处窗口（worker 判定无 pending 后新事件才置位；
        // 入口确认在飞后被切走、worker 已释放）都会留下 `PENDING && !INFLIGHT` 的孤儿边沿。
        // 入口用 CAS 保证「要么真的取得 owner，要么把 pending 留给一个仍然在飞的 worker」。
        let s = inner.rebuild_gate.state.load(Ordering::Acquire);
        if s & REBUILD_INFLIGHT == 0 {
            // 空闲：尝试取得 owner（抢不到就重读——状态只会因 worker 收尾/新事件变化）。
            if inner
                .rebuild_gate
                .state
                .compare_exchange(s, s | REBUILD_INFLIGHT, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                #[cfg(test)]
                inner.rebuild_gate.spawns.fetch_add(1, Ordering::Relaxed);
                break;
            }
            continue;
        }
        if !source.is_edge() {
            return; // demand 源：在飞就让路（不排队）
        }
        // 边沿源：在飞则留 pending。若期间 worker 已释放，CAS 失败 ⇒ 重试并取得 owner。
        if inner
            .rebuild_gate
            .state
            .compare_exchange(s, s | REBUILD_PENDING, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            return;
        }
    }
    spawn_rebuild_worker(inner, source);
}

/// 起短命 worker（初次 + 2 次重试；全败清单飞位并留行）。
fn spawn_rebuild_worker(inner: &Arc<Inner>, source: RebuildSource) {
    let mut tries = 0u32;
    loop {
        let sh = Arc::clone(inner);
        match std::thread::Builder::new()
            .name("homeway-host-rebuild".into())
            .spawn(move || rebuild_worker(sh, source))
        {
            Ok(h) => {
                *crate::syncutil::lock_unpoison(&inner.rebuild) = Some(h);
                return;
            }
            Err(e) => {
                tries += 1;
                if tries >= REBUILD_SPAWN_TRIES {
                    inner
                        .rebuild_gate
                        .state
                        .fetch_and(!(REBUILD_INFLIGHT | REBUILD_PENDING), Ordering::AcqRel);
                    crate::syncutil::log_spawn_failed(
                        &inner.logf,
                        "宿主会话重建线程",
                        &e,
                        "本会话停在 failed（无人重赛跑）——下一次拨号/巡检事件会再触发",
                    );
                    return;
                }
            }
        }
    }
}

/// worker：一次 `run_rebuild` + 边沿 pending 承接；panic 兜底释放单飞位（不留死锁）。
fn rebuild_worker(inner: Arc<Inner>, source: RebuildSource) {
    let mut source = source;
    'round: loop {
        let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_rebuild(&inner, source);
        }));
        if res.is_err() {
            (inner.logf)("一次性重建：panic（已兜住——释放单飞位，等下一次事件）");
            inner
                .rebuild_gate
                .state
                .fetch_and(!(REBUILD_INFLIGHT | REBUILD_PENDING), Ordering::AcqRel);
            return;
        }
        // 收尾也走 CAS（评审 2026-10-20 A1）：先看 pending 再释放 INFLIGHT 有两个窗口，
        // 会把「入口已置 pending」的边沿丢掉。这里 CAS 承接 pending；若期间状态变化
        // （新事件/入口取得 owner 的极端交错）就重读，绝不留下 `PENDING && !INFLIGHT`。
        loop {
            let s = inner.rebuild_gate.state.load(Ordering::Acquire);
            if s & REBUILD_PENDING != 0 {
                if inner
                    .rebuild_gate
                    .state
                    .compare_exchange(
                        s,
                        (s & !REBUILD_PENDING) | REBUILD_INFLIGHT,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    )
                    .is_ok()
                {
                    source = RebuildSource::BGate;
                    continue 'round;
                }
                continue;
            }
            if inner
                .rebuild_gate
                .state
                .compare_exchange(
                    s,
                    s & !REBUILD_INFLIGHT,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return;
            }
        }
    }
}

/// 一轮重建：全名册 `Cmd::Connect`（与初次建连同一条路）。
fn run_rebuild(inner: &Arc<Inner>, source: RebuildSource) {
    if inner.stop.load(Ordering::Acquire) {
        return;
    }
    let Some(island) = crate::syncutil::lock_unpoison(&inner.island).clone() else {
        // 岛已收回（收工路径）：不覆盖 failed 原因（否则与「一次性重连中」竞态），静默放弃。
        (inner.logf)(&format!("一次性重建：放弃（源={}，岛已收回）", source.text()));
        return;
    };
    let n = inner.cands.len();
    (inner.logf)(&format!(
        "一次性重建：起跑（源={}，候选 {n} 个）",
        source.text()
    ));
    let t0 = Instant::now();
    match island_cmd(
        &island,
        |reply| Cmd::Connect {
            cands: inner.cands.clone(),
            budget: CONNECT_BUDGET,
            demote_adopted: false,
            reply,
        },
        CONNECT_BUDGET + RPC_BUDGET,
    ) {
        Ok(o) => {
            if inner.stop.load(Ordering::Acquire) {
                return;
            }
            (inner.logf)(&format!(
                "一次性重建：恢复（源={}，胜出 {} {}，耗时 {}ms）—— 会话回 ready",
                source.text(),
                o.via.text(),
                o.winner,
                t0.elapsed().as_millis()
            ));
            set_state(inner, SessState::Ready, "");
            if let ProbeOutcome::Ok { rtt } = probe_once(inner, PROBE_TIMEOUT) {
                set_link(inner, rtt);
            }
        }
        Err(IslandRpcErr::Island(homeway_quic::IslandErr::RaceInFlight)) => {
            // 岛内阶梯 R 正在赛中：**让路，不是失败**（评审 2026-10-20 A12）。保留当前
            // Starting/Ready/Failed 面，等岛内 R 的结论（成功 ⇒ on_adopt 回 ready；失败
            // ⇒ 岛上报 unhealthy 走 B 门）或下一次事件。把它写成 failed 会让「实际已恢复」
            // 的会话长期报错。
            (inner.logf)(&format!(
                "一次性重建：让路（源={}，岛内阶梯 R 在赛中）—— 等岛内结果/下一次事件",
                source.text()
            ));
        }
        Err(e @ IslandRpcErr::Gone(_))
        | Err(e @ IslandRpcErr::Island(homeway_quic::IslandErr::EngineGone)) => {
            if inner.stop.load(Ordering::Acquire) {
                return;
            }
            (inner.logf)(&format!(
                "一次性重建：停止（源={}，岛本体已不可用：{e}）—— 会话保持 failed",
                source.text()
            ));
            set_state(inner, SessState::Failed, &format!("岛不可用（{e}）"));
        }
        Err(e) => {
            (inner.logf)(&format!(
                "一次性重建：失败（源={}，原因={e}）—— 不自动重试，等下一次事件（拨号/巡检/B门）",
                source.text()
            ));
            set_state(inner, SessState::Failed, "岛不健康；等待下一次事件重连");
        }
    }
}

// ---------- 探活 / 快照 ----------

fn probe_once(inner: &Arc<Inner>, budget: Duration) -> ProbeOutcome {
    let island = crate::syncutil::lock_unpoison(&inner.island).clone();
    let Some(island) = island else {
        return ProbeOutcome::NoFace;
    };
    match island_cmd(
        &island,
        |reply| Cmd::Probe { budget, reply },
        budget + RPC_BUDGET,
    ) {
        Ok(rtt) => ProbeOutcome::Ok { rtt },
        // **typed 判据（M5 代码门 M-1 整改）**：按 `IslandErr` 变体判，不做字符串匹配
        // （`contains("NotConnected")` 那条分支曾恒假——Display 是中文，实际只靠
        // `未持有连接` 四个字，改文案即静默失效）。
        Err(IslandRpcErr::Island(homeway_quic::IslandErr::NotConnected))
        | Err(IslandRpcErr::Island(homeway_quic::IslandErr::EngineGone)) => ProbeOutcome::NoFace,
        Err(_) => ProbeOutcome::Timeout,
    }
}

fn set_link(inner: &Arc<Inner>, rtt: Duration) {
    let island = crate::syncutil::lock_unpoison(&inner.island).clone();
    let Some(i) = island else { return };
    let s = i.snapshot();
    let via = s
        .via
        .map(|v| v.link_text().to_owned())
        .unwrap_or_else(|| "none".to_owned());
    let mut snap = crate::syncutil::lock_unpoison(&inner.snapshot);
    snap.link = Some(LinkSnapshot {
        via: via.clone(),
        ep: s.ep.map(|a| a.to_string()).unwrap_or_default(),
        rtt_ms: rtt.as_millis() as i64,
        at_ms: now_unix_ms(),
    });
    let ep = snap.link.as_ref().map(|l| l.ep.clone()).unwrap_or_default();
    let rtt_ms = rtt.as_millis();
    drop(snap);
    // C10（link 行；来源 = 岛快照，行文与 WG 档同串——设计 §8.1-C10「保留」）。
    (inner.logf)(&format!(
        "link: via={via} ep={ep} rtt={rtt_ms}ms（新栈状态快照/服务会话巡检）"
    ));
}

fn set_state(inner: &Arc<Inner>, state: SessState, reason: &str) {
    let mut s = crate::syncutil::lock_unpoison(&inner.snapshot);
    s.state = state;
    s.reason = reason.to_owned();
    s.since = Instant::now();
}

/// **除非当前是 Failed** 才置态（failed 终态保留；同一临界区内判定 + 写）。
fn set_state_unless_failed(inner: &Arc<Inner>, state: SessState, reason: &str) -> bool {
    let mut s = crate::syncutil::lock_unpoison(&inner.snapshot);
    if s.state == SessState::Failed {
        return false;
    }
    s.state = state;
    s.reason = reason.to_owned();
    s.since = Instant::now();
    true
}

/// 只把 failed 拉回 ready（探活成功/adopt 的恢复沿；其它状态不动）。
fn set_state_if_failed_ready(inner: &Arc<Inner>) -> bool {
    let mut s = crate::syncutil::lock_unpoison(&inner.snapshot);
    if s.state != SessState::Failed {
        return false;
    }
    s.state = SessState::Ready;
    s.reason.clear();
    s.since = Instant::now();
    true
}

/// 巡检（link 快照刷新 + 失败留痕；**恢复动作在岛内阶梯**——本线程不发起 WG 式动作）。
fn patrol_loop(inner: Arc<Inner>) {
    let mut fail_streak: u32 = 0;
    loop {
        // 睡到下一拍（IDLE-WAKE B：粒度 500ms→1s——stop 等待片尾部 ≤1s，仍在
        // join_bounded 6s 预算内；到点重新装填，只认 stop 位）。
        let deadline = Instant::now() + PATROL_INTERVAL;
        while Instant::now() < deadline {
            if inner.stop.load(Ordering::SeqCst) {
                (inner.logf)("宿主会话巡检退出（已收工）");
                return;
            }
            std::thread::sleep(Duration::from_millis(1000).min(
                deadline.saturating_duration_since(Instant::now()),
            ));
        }
        if inner.stop.load(Ordering::SeqCst) {
            (inner.logf)("宿主会话巡检退出（已收工）");
            return;
        }
        match probe_once(&inner, PROBE_TIMEOUT) {
            ProbeOutcome::Ok { rtt } => {
                fail_streak = 0;
                set_link(&inner, rtt);
                // 探活成功 = 连接在的**独立证据**：顺手把 failed 拉回 ready（评审
                // 2026-10-20 A12）。否则「岛已恢复、状态仍 failed」会一直把上层挡在
                // service/failed 上。stopping/idle 不动。
                set_state_if_failed_ready(&inner);
            }
            ProbeOutcome::Timeout => {
                fail_streak += 1;
                (inner.logf)(&format!(
                    "巡检失败（连续 {fail_streak}）：探活预算内无对端证据（岛内阶梯按自己的节拍处置）"
                ));
                // 3 连败 = 观测面确认的失败（与隧道域 stats 观测同门槛）；触发一次重建后
                // 清零——下一次要再攒 3 拍（≈3min），不是自驱重试。
                if fail_streak >= 3 {
                    fail_streak = 0;
                    try_rebuild(&inner, RebuildSource::Patrol);
                }
            }
            ProbeOutcome::NoFace => {
                fail_streak += 1;
                (inner.logf)("巡检无连接面：触发一次性重建（岛内阶梯也在动作）");
                try_rebuild(&inner, RebuildSource::Patrol);
            }
        }
        // 快照里的 stats 每次读时现取（`snapshot()`），这里只刷新 link。
    }
}

// ---------- 岛命令 ----------

/// 岛 RPC 的失败面（与 `tun_exec::IslandRpc` 同形；本模块自持一份避免跨模块耦合）。
enum IslandRpcErr {
    Gone(String),
    /// 岛侧 typed 错误（**保留类型**，不再 `to_string()` 抹平——M5 代码门 M-1）。
    Island(homeway_quic::IslandErr),
    Timeout(String),
}

impl std::fmt::Display for IslandRpcErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IslandRpcErr::Gone(m) => write!(f, "岛已收工（{m}）"),
            IslandRpcErr::Island(e) => write!(f, "{e}"),
            IslandRpcErr::Timeout(m) => write!(f, "岛命令等待超时（{m}）"),
        }
    }
}

fn island_cmd<T>(
    island: &Island,
    make: impl FnOnce(homeway_quic::IslandReply<T>) -> Cmd,
    budget: Duration,
) -> Result<T, IslandRpcErr> {
    let (tx, rx) = mpsc::channel();
    island
        .tx()
        .send(make(tx))
        .map_err(|e| IslandRpcErr::Gone(e.to_string()))?;
    match rx.recv_timeout(budget) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(IslandRpcErr::Island(e)),
        Err(e) => Err(IslandRpcErr::Timeout(e.to_string())),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- 流句柄（`&self` 面 = daemon 承载面消费；半句柄面 = 桥泵消费） ----------

/// 一条宿主流的共享体（两个面共用：`&self` 的 chunk 面 + 拆半的两半）。
pub(crate) struct StreamShared {
    island: Arc<Island>,
    id: StreamId,
    /// 交付前**已在手**的余量（`STREAM[dial]` 的 1B 回执之后可能立刻跟目标字节——
    /// 余量丢失 = **应用层帧错位**；与 `quic_stream::PendingBuf` 同一不变量）。
    pending: Mutex<super::quic_stream::PendingBuf>,
    /// 写无进展上界（speedtest-uplink-resilience D2）：默认 10s，测速 30s。
    no_progress: Duration,
}

impl StreamShared {
    fn take_pending(&self, out: &mut [u8]) -> Option<usize> {
        crate::syncutil::lock_unpoison(&self.pending).take(out)
    }

    /// 读一块（`Closed` ⇒ EOF 面由调用方分档）。
    fn read_cmd(&self) -> Result<Vec<u8>, StreamErr> {
        super::quic_stream::read_block(&self.island, self.id, None)
    }
}

impl Drop for StreamShared {
    fn drop(&mut self) {
        // 关流走在必须退出的收工链上（桥泵收口）⇒ 有界面（岛卡死时不挂死线程）。
        super::quic_stream::close_block(&self.island, self.id);
    }
}

/// 宿主会话的一条服务流。
pub struct HostStream {
    shared: Arc<StreamShared>,
}

impl HostStream {
    /// `&self` 面：读一块（空块续读；EOF ⇒ `Ok(Vec::new())`）——daemon 承载面消费。
    pub fn read_chunk(&self) -> io::Result<Vec<u8>> {
        let mut empty = 0u32;
        loop {
            match self.shared.read_cmd() {
                Ok(v) if v.is_empty() => {
                    empty += 1;
                    if empty > 32 {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "宿主会话流：连续空块（协议面无进展）",
                        ));
                    }
                }
                Ok(v) => return Ok(v),
                Err(StreamErr::Closed) => return Ok(Vec::new()), // EOF
                Err(e) => return Err(HostErr::from_stream(e).into()),
            }
        }
    }

    /// `&self` 面：写（`n=0` 走分级退避环；返回本次推进字节数）。
    pub fn write_chunk(&self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        write_with_backoff(&self.shared, data)
    }

    /// 半关写端（FIN；对端仍可发）。
    pub fn shutdown_write(&self) {
        super::quic_stream::shutdown_block(&self.shared.island, self.shared.id);
    }

    /// 关流（幂等；`Drop` 同款——显式调用只为语义清晰）。
    pub fn close(&self) {
        super::quic_stream::close_block(&self.shared.island, self.shared.id);
    }
}

impl BridgeStream for HostStream {
    fn into_halves(
        self: Box<Self>,
    ) -> io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)> {
        Ok((
            Box::new(HostReadHalf {
                shared: Arc::clone(&self.shared),
            }),
            Box::new(HostWriteHalf {
                shared: Arc::clone(&self.shared),
            }),
        ))
    }
}

struct HostReadHalf {
    shared: Arc<StreamShared>,
}

impl Read for HostReadHalf {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if let Some(n) = self.shared.take_pending(out) {
            return Ok(n);
        }
        // 空块（0B STREAM 帧）不是 EOF——续读有界（评审 r1 补-2：与 quic_stream 的
        // `QuicReadHalf::read`/`read_dial_ack` 对齐；原对空块直接 `Ok(0)` 会被
        // `io::Read` 消费者判成流提前结束）。
        let mut empty_chunks = 0u32;
        loop {
            let data = match self.shared.read_cmd() {
                Ok(data) => data,
                Err(StreamErr::Closed) => return Ok(0), // 对端 FIN / 本端关流 ⇒ EOF
                Err(e) => return Err(HostErr::from_stream(e).into()),
            };
            if data.is_empty() {
                empty_chunks += 1;
                if empty_chunks > super::quic_stream::MAX_EMPTY_ACK_CHUNKS {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "QUIC 服务流：连续 {} 个空块（协议面无进展）",
                            super::quic_stream::MAX_EMPTY_ACK_CHUNKS
                        ),
                    ));
                }
                continue;
            }
            let n = data.len().min(out.len());
            out[..n].copy_from_slice(&data[..n]);
            let mut p = crate::syncutil::lock_unpoison(&self.shared.pending);
            p.buf = data;
            p.off = n;
            return Ok(n);
        }
    }
}

struct HostWriteHalf {
    shared: Arc<StreamShared>,
}

impl Write for HostWriteHalf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        write_with_backoff(&self.shared, data)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WriteHalf for HostWriteHalf {
    fn close_write(&mut self) {
        super::quic_stream::shutdown_block(&self.shared.island, self.shared.id);
    }
}

/// 写主体（`n=0` 分级退避环；无进展上界按流类别取——默认 10s / 测速 30s，
/// 与 `quic_stream::QuicWriteHalf` 同源）。
fn write_with_backoff(shared: &Arc<StreamShared>, data: &[u8]) -> io::Result<usize> {
    let no_progress = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        match super::quic_stream::write_block(&shared.island, shared.id, data.to_vec()) {
            Ok(w) if w.n == 0 => {
                if no_progress.elapsed() > shared.no_progress {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "写通道长时间无进展（服务流待发队列不排空）",
                    ));
                }
                std::thread::sleep(write_retry_backoff(attempt));
                attempt = attempt.saturating_add(1);
            }
            Ok(w) => return Ok(w.n),
            Err(StreamErr::Closed) => {
                return Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "服务流已关闭（EOF 面）",
                ))
            }
            Err(e) => return Err(HostErr::from_stream(e).into()),
        }
    }
}

/// 流 id 面（daemon 承载面的诊断/测试用；不外泄共享体）。
impl HostStream {
    pub fn stream_id(&self) -> StreamId {
        self.shared.id
    }
}

// ---------- 单一真源再导出（调用方不写 tag 字面量） ----------

/// `tag_for_port` 的公开再导出（daemon/CLI 侧只需「端口 → 有无服务」的判定）。
pub fn tag_for_service_port(port: u16) -> Option<StreamTag> {
    super::quic_stream::tag_for_port(port)
}

/// 未知端口 ⇒ 归因文案（与「出口没有该服务」同族）。
pub const NO_SUCH_SERVICE: &str = "无此服务（虚拟端口未映射到任何 STREAM tag）";

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（§2.7 R-2：端口 → tag 单源）**：三个服务端口各归其 tag；未知端口 `None`；
    /// `ServicePort` 构造面拒绝未知端口（「无此服务」在拨号前归因）。
    #[test]
    fn service_port_maps_to_the_single_tag_source() {
        assert_eq!(ServicePort::files().tag(), StreamTag::Files);
        assert_eq!(ServicePort::speedtest().tag(), StreamTag::Speedtest);
        assert_eq!(ServicePort::term().tag(), StreamTag::Term);
        assert!(ServicePort::from_bridge_port(0).is_none());
        assert!(ServicePort::from_bridge_port(80).is_none());
        assert_eq!(
            ServicePort::from_bridge_port(super::super::bridge_io::port::FILES)
                .map(|p| p.get()),
            Some(7802)
        );
        // 真源同源（与本模块的公开再导出同值）
        assert_eq!(tag_for_service_port(7802), Some(StreamTag::Files));
    }

    /// **判据（§2.7 R-4：`io::Error` 映射只此一处）**：桥/承载的面判定只认 kind。
    #[test]
    fn host_err_maps_to_io_kinds_in_one_place() {
        let kind = |e: HostErr| io::Error::from(e).kind();
        assert_eq!(kind(HostErr::NoFace), io::ErrorKind::NotConnected);
        assert_eq!(
            kind(HostErr::NotReady("x".into())),
            io::ErrorKind::NotConnected
        );
        assert_eq!(kind(HostErr::Refused), io::ErrorKind::ConnectionRefused);
        // DNS-STREAM 批（代码门 r34 L5）：服务未装配与「服务在但被拒」在**桥面**同档
        // （`bridge_host::is_refused_like` 的 link_down 判据不变）；细分只在拨号缝文案。
        assert_eq!(
            kind(HostErr::ServiceDisabled),
            io::ErrorKind::ConnectionRefused
        );
        assert_eq!(kind(HostErr::Budget), io::ErrorKind::TimedOut);
        assert_eq!(kind(HostErr::Closed("x".into())), io::ErrorKind::Other);
        // 反向（`from_io`）：ConnectionRefused 仍归 Refused（ServiceDisabled 无独立 io kind
        // ⇒ 只由 `from_stream` 的 0x22 产生，不在此反解）
        assert!(matches!(
            HostErr::from_io(io::Error::from(io::ErrorKind::ConnectionRefused)),
            HostErr::Refused
        ));
        // `StreamErr::NotSupported`（0x22）与 `Refused`（0x25）在 typed 面分档
        assert!(matches!(
            HostErr::from_stream(StreamErr::NotSupported),
            HostErr::ServiceDisabled
        ));
        assert!(matches!(
            HostErr::from_stream(StreamErr::Refused),
            HostErr::Refused
        ));
    }

    /// **判据（K13：状态值域与字符串同串）**。
    #[test]
    fn sess_state_strings_match_the_wire_values() {
        assert_eq!(SessState::Starting.as_str(), "starting");
        assert_eq!(SessState::Ready.as_str(), "ready");
        assert_eq!(SessState::Failed.as_str(), "failed");
        assert_eq!(SessState::Stopping.as_str(), "stopping");
        assert_eq!(SessState::Idle.as_str(), "idle");
    }

    /// **判据（K13：档位钳位契约——capi `ClientCoreTunRecover` 的入参面）**。
    #[test]
    fn level_clamp_is_the_external_contract() {
        assert_eq!(Level::clamp(0), Level::R1);
        assert_eq!(Level::clamp(-5), Level::R1);
        assert_eq!(Level::clamp(1), Level::R1);
        assert_eq!(Level::clamp(2), Level::R2);
        assert_eq!(Level::clamp(3), Level::R3);
        assert_eq!(Level::clamp(99), Level::R3);
    }

    /// **判据（§2.6-G9：旧 token（无 QUIC 端点）⇒ 装配期失败且**不静默**）**：
    /// 归因文案与 WG 档同串（`token 里没有任何可用端点`）。
    #[test]
    fn token_without_quic_endpoints_fails_visibly() {
        let tok = Token {
            peer_id: crate::token::PeerId::from([1u8; 32]),
            secret: crate::token::Secret::from([2u8; 32]),
            endpoints: vec![crate::token::Endpoint {
                addr: "127.0.0.1:1".to_owned(),
                kind: crate::token::EndpointKind::Direct,
            }],
            rpk: None,
        };
        let logf: Logf = Arc::new(|_s: &str| {});
        let e = host_candidates(&tok, &logf).expect_err("WG-only token 在宿主会话无候选");
        assert!(
            matches!(&e, HostErr::Start(m) if m.contains("没有任何可用端点")),
            "{e}"
        );
    }

    /// 岛上报不健康 ⇒ 置 failed、触发一次性重建；无岛（合成会话）⇒ worker 即退、
    /// 单飞位释放——不挂线程、不 panic。
    #[test]
    fn unhealthy_marks_failed_and_rebuild_worker_releases() {
        let s = HostSession::synthetic_for_test(SessState::Ready, "");
        on_island_unhealthy(&s.inner, "patrol");
        let snap = s.snapshot();
        assert_eq!(snap.state, SessState::Failed);
        assert!(
            snap.reason.contains("一次性重连中"),
            "reason 须告知在重连：{}",
            snap.reason
        );
        let deadline = Instant::now() + Duration::from_secs(5);
        while s.inner.rebuild_gate.state.load(Ordering::Acquire) & REBUILD_INFLIGHT != 0
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            s.inner.rebuild_gate.state.load(Ordering::Acquire) & REBUILD_INFLIGHT,
            0,
            "无岛时 worker 须自退并释放单飞位"
        );
        // 第二次上报不 panic（worker 已释放，可再起一轮）
        on_island_unhealthy(&s.inner, "patrol");
        s.stop();
    }

    /// 闸语义：demand 源 30s 限频；边沿（B 门）绕过；单飞不重复起跑。
    #[test]
    fn rebuild_gate_edges_bypass_demand_throttle() {
        let s = HostSession::synthetic_for_test(SessState::Ready, "");
        let wait_idle = || {
            let deadline = Instant::now() + Duration::from_secs(5);
            while s.inner.rebuild_gate.state.load(Ordering::Acquire) & REBUILD_INFLIGHT != 0
                && Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        // 第一次 demand：起跑
        try_rebuild(&s.inner, RebuildSource::Dial);
        wait_idle();
        assert_eq!(s.inner.rebuild_gate.spawns.load(Ordering::Relaxed), 1);
        // 30s 内第二次 demand：被限频，不起跑
        try_rebuild(&s.inner, RebuildSource::Dial);
        wait_idle();
        assert_eq!(
            s.inner.rebuild_gate.spawns.load(Ordering::Relaxed),
            1,
            "demand 源 30s 内不得再起跑"
        );
        // 边沿：绕过限频
        try_rebuild(&s.inner, RebuildSource::BGate);
        wait_idle();
        assert_eq!(
            s.inner.rebuild_gate.spawns.load(Ordering::Relaxed),
            2,
            "B 门边沿必须绕过 30s 限频"
        );
        s.stop();
    }

    /// **判据（合成会话缝：state/reason 直读；不建隧道）**。
    #[test]
    fn synthetic_session_reports_injected_state() {
        let s = HostSession::synthetic_for_test(SessState::Failed, "出口不可达：测试硬失败");
        let snap = s.snapshot();
        assert_eq!(snap.state, SessState::Failed);
        assert_eq!(snap.reason, "出口不可达：测试硬失败");
        assert!(snap.identity.is_some(), "身份面恒在（键面依赖）");
        s.stop();
        // failed 终态保留（收工不覆盖失败原因）
        assert_eq!(s.snapshot().state, SessState::Failed);
    }
}
