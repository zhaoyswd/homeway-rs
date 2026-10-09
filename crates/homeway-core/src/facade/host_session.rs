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
//! 恒 0/false（数据面事实，不动）；「在用档」由**岛内 STREAM 出站信号**补
//! （`homeway-quic/src/driver.rs` 的 `last_stream_out_at`，同批落地）；
//! ② **`attached` 不得当就绪判据**（宿主会话恒 false ⇒ 用它判 = 假死）；本就绪判据 =
//! **准入完成**（`Live` 只在 `hr-reg4` 四帧后构造）+ 暖机探活；
//! ③ **`ladder_probe_ok` 只记阶梯探活**（显式 `Cmd::Probe` 不计）⇒ 本模块的读数行
//! 一律取**自己发出的 `Cmd::Probe` 结论**，不读 `ladder_probe_ok`。
//!
//! 有意缺口（设计 §2.6；S2a 内逐条处置）：
//! - **G5**（`daemon host reach` 过滤键）：本模块只提供会话；过滤键修正落在
//!   `daemon/hosts.rs`（WG 档的 `is_wg()` 反过滤 → `Quic|Relay`）。
//! - **G6**（`dnstest` 的 UDP 服务面无替代）：岛只有 STREAM + L3，无 UDP socket 服务
//!   ⇒ **登记退役**（排障工具，非产品四件套；`homeway-cli` 侧同批处置）。
//! - **G7**（出口 `:5300` 解析腿在宿主会话无 tag 承载）：**登记缺口**（承接 = 出口侧
//!   新增 `STREAM[tag=6]`，属出口协议面增项 ⇒ 归后续棒按设计 §12-C-8 裁决；本棒不
//!   擅动出口 wire）。⇒ `CA5`（socks 域名目标）在本棒起降级为本机解析。

use std::io::{self, Read, Write};
use std::net::SocketAddrV4;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

use super::bridge_host::{BridgeStream, WriteHalf};
use super::tun_exec::write_retry_backoff;

// ---------- 节拍常量（与连接行为真源同值；见 `tier:docs/agents/connection-lifecycle.md`） ----------

/// 暖机窗口：一发出口可达探活；超时 = 软失败（会话照常起，首个拨号触发注册/刷新）。
pub const WARM_TIMEOUT: Duration = Duration::from_secs(12);
/// 巡检间隔（保活/可达性观测 + link 快照刷新；与出口/岛的刷新节拍同值）。
pub const PATROL_INTERVAL: Duration = Duration::from_secs(60);
/// 巡检每拍探活预算。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 收工等收工上限（岛 `stop_within` 的窗口）。
pub const STOP_WAIT: Duration = Duration::from_secs(6);
/// 岛侧赛跑预算（覆盖 LAN/中继握手 + `hr-reg4` 四帧准入；与 `tun_exec` 同值）。
const CONNECT_BUDGET: Duration = Duration::from_secs(5);
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
        ServicePort(super::bridge_host::port::FILES)
    }

    pub fn term() -> ServicePort {
        // term 端口是**可配面**（`HOMEWAY_TERM_PORT`）——真源恒走 `term_port()`。
        ServicePort(super::bridge_host::term_port())
    }

    pub fn speedtest() -> ServicePort {
        ServicePort(super::bridge_host::port::SPEEDTEST)
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
    /// 出口无该服务（`StreamErr::NotSupported` 面；含未知端口）。
    #[error("出口没有该服务")]
    Refused,
    /// 预算耗尽。
    #[error("拨号预算耗尽")]
    Budget,
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
            HostErr::Refused => io::ErrorKind::ConnectionRefused,
            HostErr::Budget => io::ErrorKind::TimedOut,
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
            StreamErr::NotSupported | StreamErr::Refused => HostErr::Refused,
            StreamErr::Timeout => HostErr::Budget,
            StreamErr::Closed | StreamErr::ConnectionLost => HostErr::Closed(e.to_string()),
            other => HostErr::NotReady(other.to_string()),
        }
    }

    /// `io::Error`（`quic_stream` 缝的归错面；kind 已是语义面）→ `HostErr`（类型搬运）。
    fn from_io(e: io::Error) -> HostErr {
        match e.kind() {
            io::ErrorKind::ConnectionRefused => HostErr::Refused,
            io::ErrorKind::TimedOut => HostErr::Budget,
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
        });

        let island = start_island(&cfg.token, &ident, &cands, &inner)?;
        *lock(&inner.island) = Some(Arc::clone(&island));
        let n = cands.len();
        let outcome = island_cmd(
            &island,
            |reply| Cmd::Connect {
                cands: cands.clone(),
                budget: CONNECT_BUDGET,
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
                *lock(&inner.island) = None;
                let _ = island.stop_within(Instant::now() + STOP_WAIT);
                return Err(HostErr::Start(format!("岛建连失败：{e}")));
            }
        }

        // ---- 暖机：一发出口可达探活（WARM_TIMEOUT；超时 = 软失败）----
        let started = Instant::now();
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
                set_state(&inner, SessState::Failed, "出口不可达：无连接面");
                (logf)("暖机硬失败：无连接面（出口不可达）");
                return Ok(HostSession { inner });
            }
        }
        set_state(&inner, SessState::Ready, "");
        (logf)("就绪（会话在位，无桥直通）"); // C16
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
        *lock(&inner.patrol) = patrol;
        Ok(HostSession { inner })
    }

    /// 拨一条服务流（§2.7 R-1：budget = 调用方预算，首试 + 一次重试由
    /// `quic_stream::dial_with` 的策略收口——服务级拒绝不重试）。
    pub fn connect(&self, port: ServicePort, budget: Duration) -> Result<HostStream, HostErr> {
        self.open_tag(port.tag(), budget)
    }

    /// 拨一条**端口转发**腿（`STREAM[dial]` + 6B 目标 + 1B 回执；M4 的 pf 缝同款——
    /// 回执余量随句柄带回，由读面先出）。
    pub fn dial_addr(&self, dst: SocketAddrV4, budget: Duration) -> Result<HostStream, HostErr> {
        let island = self.island()?;
        let (id, rest) = super::quic_stream::dial_target_raw(&island, dst, budget)
            .map_err(HostErr::from_io)?;
        Ok(HostStream {
            shared: Arc::new(StreamShared {
                island,
                id,
                pending: Mutex::new(PendingBuf::new(rest)),
            }),
        })
    }

    /// 探活（显式一发；结论就是本函数返回值——**不读** `ladder_probe_ok`，见模块头 ③）。
    pub fn path_probe(&self, budget: Duration) -> ProbeOutcome {
        probe_once(&self.inner, budget)
    }

    /// 状态快照（键面同形；来源 = 岛快照）。
    pub fn snapshot(&self) -> SessionSnapshot {
        let mut s = lock(&self.inner.snapshot).clone();
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
        if let Some(h) = lock(&self.inner.patrol).take() {
            if !crate::syncutil::join_bounded(h, deadline) {
                (self.inner.logf)("收工等待巡检线程超时（STOP_WAIT）——放行自退");
            }
        }
        let island = lock(&self.inner.island).take();
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

    /// 带期限收工（服务域 `stop` 的既有节奏；`true` = 期限内收完本体）。
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
        lock(&self.inner.island)
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
                pending: Mutex::new(PendingBuf::default()),
            }),
        })
    }

    fn island_stats(&self) -> Option<(u64, u64)> {
        let i = lock(&self.inner.island).clone()?;
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
        let addrs: Vec<SocketAddrV4> = if let Ok(a) = e.addr.parse::<SocketAddrV4>() {
            vec![a]
        } else {
            // 域名形态：解析一次（有界预算；v4 记录才进候选——岛候选面是 v4）
            match resolve_endpoint_v4(&e.addr, logf) {
                Some(a) => vec![a],
                None => continue,
            }
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

/// 域名端点 → 首个 v4 地址（有界预算；失败/无 v4 ⇒ 记行 + `None`）。
fn resolve_endpoint_v4(addr: &str, logf: &Logf) -> Option<SocketAddrV4> {
    let (host, port) = crate::hostdns::split_host_port(addr)?;
    let ips = crate::hostdns::lookup_host(
        &host,
        crate::hostdns::PROBE_SYNC_BUDGET,
        crate::hostdns::ResolveLane::Critical,
    )
    .unwrap_or_default();
    let v4 = ips
        .iter()
        .find_map(|ip| match ip {
            std::net::IpAddr::V4(v4) => Some(*v4),
            std::net::IpAddr::V6(_) => None,
        });
    match v4 {
        Some(ip) => Some(SocketAddrV4::new(ip, port)),
        None => {
            (logf)(&format!(
                "宿主会话：域名端点 {addr} 无可用 v4 记录（本世代跳过该端点）"
            ));
            None
        }
    }
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
    // MTU 上限：宿主会话恒用缺省（`tunConfig.quicMtuCap` 属隧道域旋钮；宿主会话无 L3）。
    let w = Arc::downgrade(inner);
    let on_unhealthy: homeway_quic::OnUnhealthy = Arc::new(move |reason: &str| {
        if let Some(i) = w.upgrade() {
            // 岛内阶梯已走完 M/R（B 门）或 `fd`/`panic` ⇒ 如实置 State（不谎报 ready）。
            (i.logf)(&format!(
                "⚠️ 宿主会话不健康（{reason}）——岛内阶梯已尽力；本会话状态置 failed"
            ));
            set_state(&i, SessState::Failed, &format!("岛不健康（{reason}）"));
        }
    });
    let island = Island::start(Arc::clone(&inner.logf), on_unhealthy, icfg)
        .map_err(|e| HostErr::Start(format!("岛启动失败：{e}")))?;
    let island = Arc::new(island);
    // 「立即装定」显式面（与 tun_exec 同款：构造期回调已是同一份，这一发是显式面）。
    let w2 = Arc::downgrade(inner);
    let h: homeway_quic::OnUnhealthy = Arc::new(move |reason: &str| {
        if let Some(i) = w2.upgrade() {
            (i.logf)(&format!("⚠️ 宿主会话不健康（{reason}）"));
            set_state(&i, SessState::Failed, &format!("岛不健康（{reason}）"));
        }
    });
    let _ = island.tx().send(Cmd::SetOnUnhealthy { h });
    let _ = island.tx().send(Cmd::SetCandidates {
        cands: cands.to_vec(),
    });
    Ok(island)
}

// ---------- 探活 / 快照 ----------

fn probe_once(inner: &Arc<Inner>, budget: Duration) -> ProbeOutcome {
    let island = lock(&inner.island).clone();
    let Some(island) = island else {
        return ProbeOutcome::NoFace;
    };
    match island_cmd(
        &island,
        |reply| Cmd::Probe { budget, reply },
        budget + RPC_BUDGET,
    ) {
        Ok(rtt) => ProbeOutcome::Ok { rtt },
        Err(IslandRpcErr::Island(m)) if m.contains("NotConnected") || m.contains("未持有连接") => {
            ProbeOutcome::NoFace
        }
        Err(_) => ProbeOutcome::Timeout,
    }
}

fn set_link(inner: &Arc<Inner>, rtt: Duration) {
    let island = lock(&inner.island).clone();
    let Some(i) = island else { return };
    let s = i.snapshot();
    let via = s
        .via
        .map(|v| v.link_text().to_owned())
        .unwrap_or_else(|| "none".to_owned());
    let mut snap = lock(&inner.snapshot);
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
    let mut s = lock(&inner.snapshot);
    s.state = state;
    s.reason = reason.to_owned();
    s.since = Instant::now();
}

/// **除非当前是 Failed** 才置态（failed 终态保留；同一临界区内判定 + 写）。
fn set_state_unless_failed(inner: &Arc<Inner>, state: SessState, reason: &str) -> bool {
    let mut s = lock(&inner.snapshot);
    if s.state == SessState::Failed {
        return false;
    }
    s.state = state;
    s.reason = reason.to_owned();
    s.since = Instant::now();
    true
}

/// 巡检（link 快照刷新 + 失败留痕；**恢复动作在岛内阶梯**——本线程不发起 WG 式动作）。
fn patrol_loop(inner: Arc<Inner>) {
    let mut fail_streak: u32 = 0;
    loop {
        // 睡到下一拍（500ms 粒度查收工）。
        let deadline = Instant::now() + PATROL_INTERVAL;
        while Instant::now() < deadline {
            if inner.stop.load(Ordering::SeqCst) {
                (inner.logf)("宿主会话巡检退出（已收工）");
                return;
            }
            std::thread::sleep(Duration::from_millis(500));
        }
        if inner.stop.load(Ordering::SeqCst) {
            (inner.logf)("宿主会话巡检退出（已收工）");
            return;
        }
        match probe_once(&inner, PROBE_TIMEOUT) {
            ProbeOutcome::Ok { rtt } => {
                fail_streak = 0;
                set_link(&inner, rtt);
            }
            ProbeOutcome::Timeout => {
                fail_streak += 1;
                (inner.logf)(&format!(
                    "巡检失败（连续 {fail_streak}）：探活预算内无对端证据（岛内阶梯按自己的节拍处置）"
                ));
            }
            ProbeOutcome::NoFace => {
                fail_streak += 1;
                // 无面 = 连接已断：岛内阶梯据此动作（`admitted` 位保证它不被 TUN 面关掉）。
            }
        }
        // 快照里的 stats 每次读时现取（`snapshot()`），这里只刷新 link。
    }
}

// ---------- 岛命令 ----------

/// 岛 RPC 的失败面（与 `tun_exec::IslandRpc` 同形；本模块自持一份避免跨模块耦合）。
enum IslandRpcErr {
    Gone(String),
    Island(String),
    Timeout(String),
}

impl std::fmt::Display for IslandRpcErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IslandRpcErr::Gone(m) => write!(f, "岛已收工（{m}）"),
            IslandRpcErr::Island(m) => write!(f, "{m}"),
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
        Ok(Err(e)) => Err(IslandRpcErr::Island(e.to_string())),
        Err(e) => Err(IslandRpcErr::Timeout(e.to_string())),
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------- 流句柄（`&self` 面 = daemon 承载面消费；半句柄面 = 桥泵消费） ----------

/// 一条宿主流的共享体（两个面共用：`&self` 的 chunk 面 + 拆半的两半）。
pub(crate) struct StreamShared {
    island: Arc<Island>,
    id: StreamId,
    /// 交付前**已在手**的余量（`STREAM[dial]` 的 1B 回执之后可能立刻跟目标字节——
    /// 余量丢失 = **应用层帧错位**；与 `quic_stream::PendingBuf` 同一不变量）。
    pending: Mutex<PendingBuf>,
}

impl StreamShared {
    fn take_pending(&self, out: &mut [u8]) -> Option<usize> {
        lock(&self.pending).take(out)
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

#[derive(Default)]
struct PendingBuf {
    buf: Vec<u8>,
    off: usize,
}

impl PendingBuf {
    fn new(buf: Vec<u8>) -> Self {
        PendingBuf { buf, off: 0 }
    }

    fn take(&mut self, out: &mut [u8]) -> Option<usize> {
        if self.off >= self.buf.len() {
            return None;
        }
        let n = (self.buf.len() - self.off).min(out.len());
        out[..n].copy_from_slice(&self.buf[self.off..self.off + n]);
        self.off += n;
        Some(n)
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
        match self.shared.read_cmd() {
            Ok(data) => {
                let n = data.len().min(out.len());
                out[..n].copy_from_slice(&data[..n]);
                let mut p = lock(&self.shared.pending);
                p.buf = data;
                p.off = n;
                Ok(n)
            }
            Err(StreamErr::Closed) => Ok(0), // 对端 FIN / 本端关流 ⇒ EOF
            Err(e) => Err(HostErr::from_stream(e).into()),
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

/// 写主体（`n=0` 分级退避环；10s 无进展上界——与 `quic_stream::QuicWriteHalf` 同款）。
fn write_with_backoff(shared: &Arc<StreamShared>, data: &[u8]) -> io::Result<usize> {
    let no_progress = Instant::now();
    let mut attempt: u32 = 0;
    loop {
        match super::quic_stream::write_block(&shared.island, shared.id, data.to_vec()) {
            Ok(w) if w.n == 0 => {
                if no_progress.elapsed() > Duration::from_secs(10) {
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
            ServicePort::from_bridge_port(super::super::bridge_host::port::FILES)
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
        assert_eq!(kind(HostErr::Budget), io::ErrorKind::TimedOut);
        assert_eq!(kind(HostErr::Closed("x".into())), io::ErrorKind::Other);
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
