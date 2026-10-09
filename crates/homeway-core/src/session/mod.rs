//! session：服务会话状态机（Go `hostsession.Session` 服务会话形态的 Rust 收敛）。
//!
//! 生命周期 `Starting → Ready →（Stop）Idle / Failed`；暖机 12s（超时软失败继续、
//! 硬失败收工）；60s 巡检（保活/补注册 5min/失败证据门 10min 窗/3 连败进阶梯）；
//! 拨号 healing（4s 首试 → 阶梯 **R2 起跑** → 余预算重试）；阶梯连续耗尽 2 次 →
//! 整会话重建（10min 限频）。**一切判据行带 `服务会话: ` 前缀**（Go service.go:206
//! WithPrefix——R2 评审中-2 整改：bind/wgcore 族在服务会话里同样带前缀）。
//!
//! 所有权模型（R2 评审中-11）：patrol 线程独占巡检节拍状态；拨号线程只投事件（经
//! RecoverGate）+ 读快照；当前世代 `RwLock<Arc<Client>>`（rebuild 换代后拨号自动落
//! 到新 Client）。跨线程可变态全部走小临界区，不持锁跨 channel 等待（R1 纪律 3）。

pub mod recover;

use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::go_fmt::{fmt_duration_go_ms, fmt_duration_go_secs};
use crate::identity::{self, Identity, IdentitySource};
use crate::token::Token;
use crate::wgcore::{Client, ConnErr, CoreConfig, CLIENT_CLOSE_BUDGET};
use crate::wtransport::endpoint_cache::{EndpointCache, EndpointSource};
use crate::wtransport::{Candidate, Via};

pub use recover::{Action, ActionError, LadderRc, Level, RefreshRegOutcome};

// ---------- 节拍常量（Go service.go:38-75 / patrolrule.go；单一真源 connection-lifecycle.md §9） ----------

/// 暖机窗口：一发出口可达探测；超时 = 软失败（会话照常起，首个拨号触发注册）。
pub const WARM_TIMEOUT: Duration = Duration::from_secs(12);
/// 巡检间隔（保活/可达性观测，与隧道侧同节拍）。
pub const PATROL_INTERVAL: Duration = Duration::from_secs(60);
/// 每拍探测预算。
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 连续计入证据的失败到这个数进阶梯（坑 60 恢复语义）。
pub const FAIL_STREAK_RESET: u32 = 3;
/// healing 拨号首段短预算（挂起唤醒后旧路径大概率作废，4s 足够分诊）。
pub const DIAL_FIRST_TRY: Duration = Duration::from_secs(4);
/// Stop 等收工上限。
pub const STOP_WAIT: Duration = Duration::from_secs(6);
/// 阶梯连续耗尽这个次数整会话重建。
pub const LADDER_EXHAUST_REBUILD: u32 = 2;
/// 整会话重建限频。
pub const REBUILD_COOLDOWN: Duration = Duration::from_secs(600);
/// 巡检连败证据时间窗（相邻计入失败的间隔超过它 ⇒ 计数作废）。
pub const PATROL_FAIL_WINDOW: Duration = Duration::from_secs(600);
/// 巡检失败拍的本地噪声回看窗（10s 探测 + 5s 尾窗）。
pub const NOISE_WINDOW: Duration = Duration::from_secs(15);
/// 补注册周期（出口设备表按最近注册判活跃）。
pub const REG_REFRESH_EVERY: Duration = Duration::from_secs(300);
/// 中继停留升直连的拍数（服务域新增——Go 该行为隧道域产出，R2 按同节拍移植）。
pub const RELAY_UPGRADE_EVERY: u32 = 5;
/// 本地错误长停逃逸阈值。
pub const NOISE_ESCALATE_AFTER: Duration = Duration::from_secs(180);

/// 会话状态（状态面 state 值；Go svcState* 同串）。
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

/// 链路快照（link 段；`at` = unix 毫秒，0 = 未探过）。
#[derive(Debug, Clone, Default)]
pub struct LinkSnapshot {
    pub via: String,
    pub ep: String,
    pub rtt_ms: i64,
    pub at_ms: i64,
}

/// 会话快照（状态 JSON 的数据源；形状对齐 Go hostsession.Snapshot 的服务形态子集）。
#[derive(Debug, Clone)]
pub struct SessionSnapshot {
    pub state: SessState,
    pub reason: String,
    pub since: Instant,
    pub link: Option<LinkSnapshot>,
    pub identity: Option<(String, String)>, // (dev 短指纹, pub 短指纹)
    pub stats: Option<(u64, u64)>,          // (rx, tx) WG 传输层累计
}

/// 会话装配参数。
pub struct SessionConfig {
    pub token: Token,
    /// 身份目录（None = 临时身份）。
    pub identity_dir: Option<PathBuf>,
    /// 端点缓存目录（None = 不落盘）。
    pub endpoint_cache_dir: Option<PathBuf>,
    /// 原始日志面（Session 在其上加 `服务会话: ` 前缀）。
    pub logf: Arc<dyn Fn(&str) + Send + Sync>,
    /// 【test-seams】中继锁定（relay-lock 注入，连接开始前生效）：模拟「直连路径
    /// 全被 NAT 丢弃」的真机中继形态——非中继源按从未到达处理。同机回环上直连
    /// 永远通（连出口的盲打都能把客户端采纳翻成直连），中继驻留/升级条纹不注入
    /// 就测不出来。含 hint 抑制（直连不可达时 hint 无意义）。
    pub relay_only: bool,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SessionErr {
    #[error("token 里没有任何可用端点")]
    NoCandidates,
    #[error("身份装配失败：{0}")]
    Identity(String),
    #[error("数据面装配失败：{0}")]
    DataPlane(std::io::Error),
}

/// 阶梯轮结果对耗尽计数的记账（小临界区）。
#[derive(Default)]
struct LadderState {
    exhausted: u32,
    rebuild_at: Option<Instant>,
}

struct Shared {
    client: RwLock<Arc<Client>>,
    /// hint 事件队列（驱动线程只投递；hint 处理线程消费——RPC/落盘/打洞都在处理线程）。
    hint_tx: std::sync::mpsc::Sender<SocketAddr>,
    /// 缓存落盘信号（cap=1 语义：去抖线程合并写）。
    save_tx: std::sync::mpsc::Sender<()>,
    /// punchTo 节流（5s——hint 抖动防镜像风暴）。
    last_punch: Mutex<Option<Instant>>,
    /// 世代号（rebuild +1；拨号/恢复入口防陈旧比对用）。
    gen: AtomicU64,
    snapshot: Mutex<SessionSnapshot>,
    gate: recover::RecoverGate,
    ladder: Mutex<LadderState>,
    cache: Option<Mutex<EndpointCache>>,
    static_cands: Vec<Candidate>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    stop: AtomicBool,
    /// 重建用的装配材料（token 的端点展开 = static_cands；Token 本体不存——
    /// 重建只消费身份材料 + 候选表）。Secret 非 Copy（F8d）：持有者唯一、Drop 擦除。
    secret: crate::token::Secret,
    peer_pub: [u8; 32],
    identity: Identity,
    tunnel_ip: std::net::Ipv4Addr,
    /// 【test-seams】hint 抑制（relay-lock 的一部分——直连不可达时 hint 无意义）。
    #[cfg(feature = "test-seams")]
    suppress_hints: std::sync::atomic::AtomicBool,
    /// 【test-seams】中继锁定（relay-lock）——重建世代要重装 bind 锁定。
    relay_lock: bool,
    /// 域名端点重解析编排（P0-4；无域名条目 = None——Rearm/RearmSoft/旁路探测拍触发）。
    /// RwLock：构造期（Shared 建好后）才填——回调持 Weak 回指 Shared。
    domain_refresher: RwLock<Option<Arc<crate::wtransport::domain_eps::DomainRefresher>>>,
    /// 域名条目候选（最近一次解析产物；static_cands = IP 字面量，两者相加 = 静态面）。
    domain_cands: Mutex<Vec<Candidate>>,
}

impl Shared {
    fn current(&self) -> Arc<Client> {
        crate::syncutil::read_unpoison(&self.client).clone()
    }

    fn gen_now(&self) -> u64 {
        self.gen.load(Ordering::Acquire)
    }

    fn set_state(&self, state: SessState, reason: &str) {
        let mut s = crate::syncutil::lock_unpoison(&self.snapshot);
        s.state = state;
        s.reason = reason.to_owned();
        s.since = Instant::now();
    }

    /// **除非当前是 Failed** 才置态（同一临界区内判定 + 写；返回是否写入）——Q-F F6：
    /// `stop()` 的入口（Stopping）与末尾（Idle）都用它，「读—写」两次取锁之间的窗口
    /// 由此闭合（Failed 终态保留是设计约定；代码门 r15 新增 4）。
    fn set_state_unless_failed(&self, state: SessState, reason: &str) -> bool {
        let mut s = crate::syncutil::lock_unpoison(&self.snapshot);
        if s.state == SessState::Failed {
            return false;
        }
        s.state = state;
        s.reason = reason.to_owned();
        s.since = Instant::now();
        true
    }

    fn set_link(&self, via: Via, ep: Option<SocketAddr>, rtt: Duration) {
        let mut s = crate::syncutil::lock_unpoison(&self.snapshot);
        s.link = Some(LinkSnapshot {
            via: via.as_str().to_owned(),
            ep: ep.map(|a| a.to_string()).unwrap_or_default(),
            rtt_ms: rtt.as_millis() as i64,
            at_ms: now_unix_ms(),
        });
    }

    fn merged_candidates(&self) -> Vec<Candidate> {
        let domain = crate::syncutil::lock_unpoison(&self.domain_cands).clone();
        let mut base = self.static_cands.clone();
        base.extend(domain);
        match &self.cache {
            Some(c) => {
                let c = crate::syncutil::lock_unpoison(c);
                c.merge(&base, SystemTime::now())
            }
            None => base,
        }
    }

    /// 真实往返落已验证（同址 1h 去重由调用方节流；来源 = static 内 → Token 否则 Hint）。
    fn mark_round_trip(&self, addr: SocketAddr) {
        if let Some(c) = &self.cache {
            let src = if self.static_cands.iter().any(|c| c.addr == addr) {
                EndpointSource::Token
            } else {
                EndpointSource::Hint
            };
            crate::syncutil::lock_unpoison(c).mark_verified(addr, src, SystemTime::now());
        }
    }
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// 服务会话句柄。
pub struct Session {
    shared: Arc<Shared>,
    /// 后台线程柄（stop 时 join 收口——中-4：不泄漏）。Mutex 包一层：stop 改 &self
    /// （服务桥 dial 与 status 共享 `Arc<Session>`——评审 r2-M-10② 的共享前提）。
    patrol: Mutex<Option<JoinHandle<()>>>,
    hint: Mutex<Option<JoinHandle<()>>>,
    save: Mutex<Option<JoinHandle<()>>>,
}

impl Session {
    /// 装配 + 暖机 + 起巡检（C12/C1/C2/C3/C13/暖机/C16 判据行在此产出）。
    /// 暖机硬错返回 `Ok` 且 state=Failed（上层可读原因重试）；装配类错误直接 `Err`。
    pub fn start(cfg: SessionConfig) -> Result<Session, SessionErr> {
        let logf: Arc<dyn Fn(&str) + Send + Sync> = {
            let raw = cfg.logf;
            Arc::new(move |s: &str| raw(&format!("服务会话: {s}")))
        };
        (logf)("启动（无 TUN 服务会话）"); // C12

        // 端点解析：直连 + 中继（type=1）都进候选——中继是直连的后备（DirectFirst 窗口）。
        // 域名条目按 P0-4 展开（split_and_resolve：IP 字面量直入 + 域名建会话解析一次
        // + 原文保留给重解析）。
        // M1 S1c：WG 档候选**不吃 QUIC 类端点**（§2.1 末段）——过滤在
        // `token::wg_endpoint_refs`（与 tun_exec / daemon reach 共用同一条规则）。
        let ep_refs: Vec<crate::token::EndpointRef> = crate::token::wg_endpoint_refs(&cfg.token.endpoints);
        let inputs = crate::wtransport::domain_eps::split_and_resolve(&ep_refs, &logf);
        let candidates: Vec<Candidate> = inputs.candidates;
        let domain_eps = inputs.domains;
        let inputs_static_base = inputs.static_base;
        let domain_initial = inputs.domain_initial;
        if candidates.is_empty() {
            return Err(SessionErr::NoCandidates);
        }
        // C14 参照点探测目标（设计门 A3 + 代码门 M6：抽纯函数以钉住**站点接线**——
        // Go `cands[0]` 同义项 = 首个已解析的 token 端点候选；不是 `static_cands[0]`
        // 〔只装 IP 字面量〕，也不是 `merged_candidates()[0]`〔含学习缓存条目〕）。
        // 借用早收：C14 站点不再借 candidates。
        let probe_target = c14_probe_target(&candidates);
        #[allow(unused_variables)]
        let direct_cands: Vec<SocketAddr> =
            candidates.iter().filter(|c| !c.relay).map(|c| c.addr).collect();
        // ---- 身份（C1 全支文案同串，R1 低-13 整改）----
        let (ident, src, warn) =
            identity::load_or_create(cfg.identity_dir.as_deref(), &cfg.token.peer_id)
                .map_err(|e| SessionErr::Identity(e.to_string()))?;
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

        let client = build_client(&cfg.token, &ident, &candidates, &logf)?;
        let tunnel_ip = crate::tunnel_addr::derive_tunnel_ip(&cfg.token.secret, &ident.public_key());

        let cache = cfg.endpoint_cache_dir.map(|dir| {
            let mut c = EndpointCache::open(&dir, cfg.token.peer_id);
            c.set_logger(Arc::clone(&logf));
            c
        });

        let (hint_tx, hint_rx) = std::sync::mpsc::channel::<SocketAddr>();
        let (save_tx, save_rx) = std::sync::mpsc::channel::<()>();
        let shared = Arc::new(Shared {
            client: RwLock::new(Arc::new(client)),
            hint_tx,
            save_tx,
            last_punch: Mutex::new(None),
            gen: AtomicU64::new(1),
            snapshot: Mutex::new(SessionSnapshot {
                state: SessState::Starting,
                reason: String::new(),
                since: Instant::now(),
                link: None,
                identity: Some((ident.short_dev(), ident.short_pub())),
                stats: Some((0, 0)),
            }),
            gate: recover::RecoverGate::new(),
            ladder: Mutex::new(LadderState::default()),
            cache: cache.map(Mutex::new),
            static_cands: inputs_static_base,
            domain_cands: Mutex::new(domain_initial.clone()),
            domain_refresher: RwLock::new(None),
            logf: Arc::clone(&logf),
            stop: AtomicBool::new(false),
            secret: cfg.token.secret.clone(),
            peer_pub: *cfg.token.peer_id.as_bytes(),
            identity: ident,
            tunnel_ip,
            #[cfg(feature = "test-seams")]
            suppress_hints: std::sync::atomic::AtomicBool::new(cfg.relay_only),
            relay_lock: cfg.relay_only,
        });

        // P0-4：域名重解析编排装配（回调持 Weak 回指——Shared 生命周期自洽）。
        if !domain_eps.is_empty() {
            let w1 = Arc::downgrade(&shared);
            let w2 = Arc::downgrade(&shared);
            let w3 = Arc::downgrade(&shared);
            let rf = Arc::new(crate::wtransport::domain_eps::DomainRefresher::new(
                domain_eps,
                domain_initial,
                Arc::clone(&logf),
                Arc::new(move |fresh: &[Candidate]| {
                    if let Some(sh) = w1.upgrade() {
                        *crate::syncutil::lock_unpoison(&sh.domain_cands) = fresh.to_vec();
                        let merged = sh.merged_candidates();
                        sh.current().set_candidates(merged);
                    }
                }),
                Arc::new(move || {
                    let sh = w2.upgrade()?;
                    let snap = sh.current().snapshot();
                    if snap.via == Via::Relay {
                        snap.ep
                    } else {
                        None
                    }
                }),
                Arc::new(move || {
                    if let Some(sh) = w3.upgrade() {
                        // F3b/N1：域名刷新回调线程必须能退出——有界（动作预算同刻度）
                        let _ = sh.current().rearm_soft_bounded(recover::ACTION);
                        if let Some(c) = &sh.cache {
                            crate::syncutil::lock_unpoison(c).note_rearm();
                        }
                        let merged = sh.merged_candidates();
                        sh.current().set_candidates(merged);
                    }
                }),
            ));
            *crate::syncutil::write_unpoison(&shared.domain_refresher) = Some(rf);
        }
        // C3（第二字段 = 本设备派生隧道地址——评审中-4）
        (logf)(&format!(
            "新栈会话已建立（token 端点 {} 个，后端隧道地址 {}）",
            cfg.token.endpoints.len(),
            shared.tunnel_ip
        ));
        // C13 候选清单（标记·学习）
        {
            let merged = shared.merged_candidates();
            let parts: Vec<String> = merged
                .iter()
                .map(|c| {
                    // 代码门 M3：「在 token 候选里」= Go `DescribeCandidatesWithLearned(list, cands)`
                    // 的 `cands`（token 序，IP 字面量 **+ 域名首解**）——此前用
                    // `static_cands`（只装 IP 字面量）⇒ 域名解析出的候选被误打 `·学习`。
                    let known = candidates.iter().any(|s| s.addr == c.addr);
                    let learned = if known { "" } else { "·学习" };
                    let tag = if c.relay {
                        "中继"
                    } else {
                        crate::wtransport::bind::candidate_tag(c.addr, false)
                    };
                    format!("{}（{tag}{learned}）", c.addr)
                })
                .collect();
            (logf)(&format!(
                "候选端点（{} 条，标记·学习=来自巡检缓存/中继 hint）：{}",
                merged.len(),
                parts.join("、")
            ));
        }

        // ---- C14 出口能力（Q-H F17；Go hostsession/session.go:252-280 同位置同语义）----
        // 一次性参照点探测（明文一问一答，5s 预算，旁路纪律——不参与健康判定）；
        // spawn 失败 = 记行不静默（Q-F F7 纪律）。
        if let Some(target) = probe_target {
            let logf_c14 = Arc::clone(&logf);
            match std::thread::Builder::new()
                .name("homeway-caps".into())
                .stack_size(256 * 1024)
                .spawn(move || {
                    match crate::probe::ping_ex(target, 16, Duration::from_secs(5)) {
                        Ok(r) => (logf_c14)(&format_outbound_caps_line(&r)),
                        Err(e) => (logf_c14)(&format_outbound_caps_fail(&e)),
                    }
                })
            {
                Ok(_) => {}
                Err(e) => (logf)(
                    &format!("出口能力探测线程启动失败（{e}）——本轮无出口能力行"),
                ),
            }
        } else {
            (logf)("出口能力：无已解析候选（token 端点全部不可解析）——跳过参照点探测");
        }

        // ---- hint 回调（驱动线程执行——只投队列，重活在处理线程）+ 两个后台线程 ----
        install_hint_callback(&shared);
        // 测试缝（relay-lock）：直连形态按从未到达处理——须在首包前生效
        if cfg.relay_only {
            shared.current().set_relay_only();
        }
        let hint_handle = spawn_hint_handler(Arc::clone(&shared), hint_rx);
        let save_handle = spawn_save_loop(Arc::clone(&shared), save_rx);

        // ---- 暖机：一发出口可达探测（12s；超时软失败，硬错收工）----
        let client = shared.current();
        let warm_started = Instant::now();
        match client.path_probe(WARM_TIMEOUT) {
            Ok(()) => {
                let rtt = warm_started.elapsed();
                let snap = client.snapshot();
                shared.set_link(snap.via, snap.ep, rtt);
                (logf)(&format!("暖机就绪（出口可达，rtt={}ms）", rtt.as_millis()));
            }
            Err(ConnErr::Timeout) => {
                (logf)(&format!(
                    "暖机 {} 内出口未应答：按软失败继续（首个拨号会触发注册）",
                    fmt_duration_go_ms(WARM_TIMEOUT)
                ));
            }
            Err(e) => {
                (logf)(&format!("暖机硬失败：{e}"));
                shared.set_state(SessState::Failed, &format!("出口不可达：{e}"));
                return Ok(Session { shared, patrol: Mutex::new(None), hint: Mutex::new(hint_handle), save: Mutex::new(save_handle) });
            }
        }

        shared.set_state(SessState::Ready, "");
        (logf)("就绪（会话在位，无桥直通）"); // C16（CLI 形态无桥）

        // ---- 巡检线程（controller）----
        // F7-1/F7-2：spawn 失败不静默（只记行）；线程体套 catch_unwind——落空行为
        // 定死（设计门 C7h）：记行 + **不改域态**（服务域无 unhealthy 通道；不把可用
        // 会话打成 Failed——数据面不依赖巡检）。
        let sh = Arc::clone(&shared);
        let logf2 = Arc::clone(&logf);
        let patrol = match std::thread::Builder::new()
            .name("homeway-patrol".into())
            .spawn(move || {
                let sh2 = Arc::clone(&sh);
                if std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || patrol_loop(sh)))
                    .is_err()
                {
                    (sh2.logf)("巡检线程 panic（已兜住）—— 本会话失去自愈巡检");
                }
            }) {
            Ok(h) => Some(h),
            Err(e) => {
                crate::syncutil::log_spawn_failed(&logf2, "巡检线程", &e, "本会话失去自愈巡检");
                None
            }
        };

        Ok(Session { shared, patrol: Mutex::new(patrol), hint: Mutex::new(hint_handle), save: Mutex::new(save_handle) })
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let mut s = crate::syncutil::lock_unpoison(&self.shared.snapshot).clone();
        // stats 现读现给（随世代清零）
        let e = self.shared.current().snapshot();
        s.stats = Some((e.rx, e.tx));
        s
    }

    /// 当前世代号（拨号入口防陈旧的比对基线）。
    pub fn generation(&self) -> u64 {
        self.shared.gen_now()
    }

    /// 当前世代的数据面句柄（speedtest 等**非**恢复感知的直连面；恢复感知拨号走
    /// healing_dial_*）。
    /// 【test-seams】hint 抑制注入（no-hint——见 Shared::suppress_hints 注释）。
    #[cfg(feature = "test-seams")]
    pub fn debug_suppress_hints(&self) {
        self.shared.suppress_hints.store(true, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn client(&self) -> Arc<Client> {
        self.shared.current()
    }

    /// 公开恢复面（隧道域入口语义：`ClientCoreTunRecover(from)` 同义；CLI 测试钩子）。
    /// 耗尽记账在 gate 的 run 回调内（每轮恰好一次——被合并的等待者不重复记账，
    /// Go noteLadderResult 同位；评审中-1）；重建决策在 merge 之后（临界区外）。
    pub fn recover(&self, from: Level, cause: &str) -> LadderRc {
        self.recover_until(from, cause, None).0
    }

    /// 带期限的恢复入口（Q-F F3-1/F3-2）：
    /// - `deadline` 透到阶梯本体与闸等待（等待到点 ⇒ `(Deadline, false)`）；
    /// - **等待方到点时不触发重建决策**（设计门 C4：在途轮未完就 `rebuild_session →
    ///   old.stop()` 会拆掉在途轮正持有的 Client）；
    /// - `Deadline` 不计入耗尽（[`recover::exhausted_delta`] 单源）。
    pub fn recover_until(
        &self,
        from: Level,
        cause: &str,
        deadline: Option<Instant>,
    ) -> (LadderRc, bool) {
        let (rc, waited) = self.shared.gate.merge_until(from, deadline, |lvl, d| {
            let rc = self.recover_round_until(lvl, cause, d);
            self.note_ladder_result(&rc);
            rc
        });
        if waited {
            self.maybe_rebuild_if_exhausted();
        }
        (rc, waited)
    }

    /// 陈旧会话恢复（拨号失败/巡检连败/挂起空窗共用；**R2 起跑**）。
    /// 入口防陈旧：世代已换代就不跑（Go curSession 比对同义——按 Go 语义返回 0 形态，
    /// 不触发耗尽记账）。
    pub fn recover_stale(&self, gen: u64, cause: &str) -> LadderRc {
        if self.shared.gen_now() != gen {
            return LadderRc::Recovered(Level::R2);
        }
        self.recover(Level::R2, cause)
    }

    /// 带期限的陈旧会话恢复（healing_dial 面；F3）。
    pub fn recover_stale_until(
        &self,
        gen: u64,
        cause: &str,
        deadline: Option<Instant>,
    ) -> (LadderRc, bool) {
        if self.shared.gen_now() != gen {
            return (LadderRc::Recovered(Level::R2), true);
        }
        self.recover_until(Level::R2, cause, deadline)
    }

    /// gate 的 run 回调（每轮恰好执行一次——耗尽记账在 merge 返回后，Go 同位）。
    fn recover_round_until(&self, from: Level, cause: &str, deadline: Option<Instant>) -> LadderRc {
        session_recover_round(&self.shared, from, cause, deadline)
    }

    /// 耗尽记账（单源判据 `recover::exhausted_delta`；`Deadline`/`Stale` 不动计数）。
    fn note_ladder_result(&self, rc: &LadderRc) {
        let mut st = crate::syncutil::lock_unpoison(&self.shared.ladder);
        match recover::exhausted_delta(rc) {
            Some(false) => st.exhausted = 0,
            Some(true) => st.exhausted += 1,
            None => {}
        }
    }

    /// 耗尽计数到阈值整会话重建（触发即归零；限频在 rebuild 内）。
    fn maybe_rebuild_if_exhausted(&self) {
        let n = {
            let mut st = crate::syncutil::lock_unpoison(&self.shared.ladder);
            if st.exhausted < LADDER_EXHAUST_REBUILD {
                return;
            }
            std::mem::take(&mut st.exhausted) // 先取后清（评审中-2：归零前取值）
        };
        rebuild_session(&self.shared, &format!("恢复阶梯连续 {n} 轮走完仍未恢复"));
    }

    /// 恢复感知的隧道端口拨号（Go healingDial：4s 首试 → recover_stale → 余预算重试；
    /// 重建若已触发，重试自动打到当前世代）。
    pub fn healing_dial_port(&self, port: u16, budget: Duration) -> Result<u64, ConnErr> {
        let dst = SocketAddrV4::new(crate::wgcore::SERVER_TUNNEL_IP, port);
        self.healing_dial(dst, budget)
    }

    /// 恢复感知的任意目标拨号（DialAddr 同路径）。
    pub fn healing_dial_addr(&self, dst: SocketAddrV4, budget: Duration) -> Result<u64, ConnErr> {
        self.healing_dial(dst, budget)
    }

    /// 拨号主体（Q-F F3：首试 + 阶梯（含等待） + 尾试合计受调用方同一预算约束）。
    /// 与隧道域共用形状（`tun_exec::dial_with_recover` 的可测主体——本函数是服务域
    /// 版本，错误面是 [`ConnErr`] 而非 io::Error）。
    fn healing_dial(&self, dst: SocketAddrV4, budget: Duration) -> Result<u64, ConnErr> {
        let t0 = Instant::now();
        let deadline = t0 + budget;
        let gen = self.shared.gen_now();
        match self.shared.current().connect_deadline(dst, DIAL_FIRST_TRY.min(budget)) {
            Ok(id) => {
                self.mark_session_ready();
                return Ok(id);
            }
            Err(e) => {
                // 调用方预算不比首试段长：没有重试意义
                if t0.elapsed() >= budget {
                    return Err(e);
                }
            }
        }
        let (rc, _waited) = self.recover_stale_until(gen, "拨号失败", Some(deadline));
        if matches!(rc, LadderRc::Deadline) {
            return Err(ConnErr::Timeout);
        }
        let remain = budget
            .checked_sub(t0.elapsed())
            .unwrap_or(Duration::from_millis(1));
        let id = self.shared.current().connect_deadline(dst, remain)?;
        self.mark_session_ready();
        Ok(id)
    }

    /// 会话跑通一次（首个成功往返）：采纳地址落已验证（中继跳过——Go markSessionReady）。
    fn mark_session_ready(&self) {
        let snap = self.shared.current().snapshot();
        if snap.via == Via::Direct {
            if let Some(ep) = snap.ep {
                self.shared.mark_round_trip(ep);
            }
        }
    }

    /// 收工（幂等）：置 stop → **五段共用一个 6s 预算**（Q-F F6-4）→ Idle
    /// （failed 终态保留——低-15：失败原因在状态面不丢）。
    ///
    /// 五段（顺序即证据面）：① 巡检 join；② hint join；③ 缓存落盘线程 join；
    /// ④ 缓存终写（**`try_lock` 快跳**）；⑤ `Client::stop_within`（到点 detach，
    /// wake fd 交收割线程——设计门 C2/D3）。到点一律**记行 + 放行自退**。
    /// **残余（登记）**：五段都不再等待**锁/线程**（此前 hint/save/client 的 join
    /// 无上界），但段④拿到锁后的落盘 I/O 仍无期限（`EndpointCache::save` 的读盘/
    /// 写/rename——锁空闲 + 卡文件系统形态下 6s 仍可被击穿）。
    ///
    /// 预算**范围声明**：本预算只覆盖 `session::Session::stop()`——不含隧道域
    /// `Finish::drop`/`request_stop` 的 `c.stop()` 与 `rebuild_session→old.stop()`
    /// （残余登记，设计 §7-4/§7-5）。
    pub fn stop(&self) {
        if self.shared.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        // failed 终态保留（低-15）：置 Stopping 走「除非 Failed」（否则 set_state 会把
        // failed 覆盖掉，失败实例入槽后 status 面就再也读不到失败原因——F2 依赖）。
        self.shared.set_state_unless_failed(SessState::Stopping, "");
        let deadline = Instant::now() + STOP_WAIT;
        // ① 巡检线程
        if let Some(h) = self.patrol.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if !crate::syncutil::join_bounded(h, deadline) {
                // 有界等待放弃（patrol 卡在长阶梯/探测里——Go serviceStopWait 同义；
                // 线程随后自行退出，JoinHandle 析构即分离）
                (self.shared.logf)("收工等待巡检线程超时（STOP_WAIT）——放行自退");
            }
        }
        // ② hint 处理线程
        if let Some(h) = self.hint.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if !crate::syncutil::join_bounded(h, deadline) {
                (self.shared.logf)("收工等待 hint 线程超时（STOP_WAIT）——放行自退");
            }
        }
        // ③ 缓存落盘线程
        if let Some(h) = self.save.lock().unwrap_or_else(|e| e.into_inner()).take() {
            if !crate::syncutil::join_bounded(h, deadline) {
                (self.shared.logf)("收工等待缓存落盘线程超时（STOP_WAIT）——放行自退");
            }
        }
        // ④ 缓存终写：**try_lock 快跳**（设计门 C1——`EndpointCache::save` 持锁做
        // 读盘/建目录/写/rename，全无期限；拿不到锁说明去抖线程正在写 ⇒ 不等待也不
        // 越预算，去抖线程近期已落盘，终写尽力而为）。**残余**：拿到锁后的 I/O 仍
        // 无期限（登记 §5.3）
        if let Some(c) = &self.shared.cache {
            match c.try_lock() {
                Ok(mut g) => {
                    let _ = g.save(SystemTime::now());
                }
                Err(std::sync::TryLockError::Poisoned(e)) => {
                    let _ = e.into_inner().save(SystemTime::now());
                }
                Err(std::sync::TryLockError::WouldBlock) => {
                    (self.shared.logf)("收工缓存终写跳过（锁被在途落盘占用）——去抖线程近期写已在盘上");
                }
            }
        }
        // ⑤ 停当前世代（有界；到点 detach ⇒ wake fd 交收割线程）
        if !self.shared.current().stop_within(deadline) {
            (self.shared.logf)("收工等待 client 线程超时（STOP_WAIT）——放行自退（引擎线程由收割线程收口）");
        }
        // 终态：同一临界区内判「除非 Failed」（收工窗口内巡检/重建可能把会话打成
        // Failed——失败原因不得被 Idle 覆盖；HEAD 的窗口语义保留）
        if self.shared.set_state_unless_failed(SessState::Idle, "已收工") {
            (self.shared.logf)(&format!("已收工（state={}）", SessState::Idle.as_str()));
        } else {
            (self.shared.logf)("已收工（state=failed 终态保留）");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 【test-seams】合成会话（Q-F F2：ServiceExec 的 Ok-but-failed / Ready 分支测试用）。
///
/// 形态：真**惰性** Client（黑洞候选——只起引擎线程，不发包；镜像
/// `wgcore::tests::engine_probe_blackhole_times_out` 的可行构造）+ 快照直置目标态。
/// 只有 `cfg(test)` 可见（生产 API 零改动；本批消费方 `service_exec` 单测在 crate 内）。
#[cfg(test)]
impl Session {
    pub(crate) fn synthetic_failed_for_test(reason: &str) -> Session {
        Session::synthetic_for_test(SessState::Failed, reason, None, None)
    }

    /// 完整注入形态（F6-4 段 4 断言用：缓存目录 + 日志面）。
    pub(crate) fn synthetic_for_test(
        state: SessState,
        reason: &str,
        cache_dir: Option<PathBuf>,
        logf: Option<crate::Logf>,
    ) -> Session {
        let ident = Identity::ephemeral().expect("临时身份");
        let secret = crate::token::Secret::from([0x2a; 32]);
        let peer_pub = crate::token::PeerId::from([0x3b; 32]);
        let logf: Arc<dyn Fn(&str) + Send + Sync> = logf.unwrap_or_else(|| Arc::new(|_s: &str| {}));
        let client = build_client(
            &Token {
                peer_id: peer_pub,
                secret: secret.clone(),
                endpoints: vec![],
                rpk: None,
            },
            &ident,
            &[Candidate {
                // TEST-NET-3 不可达地址：起得来、发不出（惰性）
                addr: "203.0.113.1:41641".parse().unwrap(),
                relay: false,
            }],
            &logf,
        )
        .expect("合成会话的惰性客户端可起");
        let tunnel_ip = crate::tunnel_addr::derive_tunnel_ip(&secret, &ident.public_key());
        let cache = cache_dir.map(|dir| {
            let mut c = EndpointCache::open(&dir, peer_pub);
            c.set_logger(Arc::clone(&logf));
            Mutex::new(c)
        });
        let (hint_tx, _hint_rx) = std::sync::mpsc::channel::<SocketAddr>();
        let (save_tx, _save_rx) = std::sync::mpsc::channel::<()>();
        let shared = Arc::new(Shared {
            client: RwLock::new(Arc::new(client)),
            hint_tx,
            save_tx,
            last_punch: Mutex::new(None),
            gen: AtomicU64::new(1),
            snapshot: Mutex::new(SessionSnapshot {
                state,
                reason: reason.to_owned(),
                since: Instant::now(),
                link: None,
                identity: Some((ident.short_dev(), ident.short_pub())),
                stats: Some((0, 0)),
            }),
            gate: recover::RecoverGate::new(),
            ladder: Mutex::new(LadderState::default()),
            cache,
            static_cands: vec![],
            domain_cands: Mutex::new(vec![]),
            domain_refresher: RwLock::new(None),
            logf,
            stop: AtomicBool::new(false),
            secret,
            peer_pub: *peer_pub.as_bytes(),
            identity: ident,
            tunnel_ip,
            #[cfg(feature = "test-seams")]
            suppress_hints: std::sync::atomic::AtomicBool::new(false),
            relay_lock: false,
        });
        Session {
            shared,
            patrol: Mutex::new(None),
            hint: Mutex::new(None),
            save: Mutex::new(None),
        }
    }
}

/// C14 出口能力行（Q-H F17；**纯函数**便于单测）：成功形态逐字对齐 Go
/// `hostsession/session.go:269` 的格式串——
/// `出口能力：构建 %s ｜ 默认路径 UDP：DNS:53 %s / 通用（非 53）%s / 实测 %s ｜ 探测往返 %v`
/// （`build` 空 ⇒ `（未标注）`；位映射 = udpcap bit0/bit1/bit2/bit4，与出口
/// `engine.rs` 的 UDPCAP_* 及 Go 常量逐位一致；`rtt` = Go `Duration.Round(ms)` 形态）。
pub(crate) fn format_outbound_caps_line(r: &crate::probe::PingResult) -> String {
    const UDPCAP_DNS: u8 = 1 << 0;
    const UDPCAP_GENERIC: u8 = 1 << 1;
    const UDPCAP_OBSERVED: u8 = 1 << 2;
    const UDPCAP_SEEN: u8 = 1 << 4;
    let build = if r.build.is_empty() { "（未标注）" } else { r.build.as_str() };
    let dns = if r.flags & UDPCAP_DNS != 0 { "可用" } else { "不可用" };
    let gen = if r.flags & UDPCAP_GENERIC != 0 { "可用" } else { "不可用" };
    let saw = if r.flags & UDPCAP_OBSERVED != 0 {
        "有回包（可用）"
    } else if r.flags & UDPCAP_SEEN != 0 {
        "无回包 —— 这条路不回这类 UDP，QUIC 会超时回落 TCP"
    } else {
        "还没实测样本（这台出口还没转发过 UDP）"
    };
    format!(
        "出口能力：构建 {build} ｜ 默认路径 UDP：DNS:53 {dns} / 通用（非 53）{gen} / 实测 {saw} ｜ 探测往返 {}",
        crate::go_fmt::fmt_duration_go_ms(r.rtt)
    )
}

/// C14 探测目标（Q-H F17 + 代码门 M6；**纯函数**——站点必须经它取目标，单测因此
/// 能钉住「首项 = token 序首个已解析候选」而非只钉数据源形状）：
/// `None` = 无候选（入口已由 `NoCandidates` 早退保证，实现仍不索引）。
pub(crate) fn c14_probe_target(
    candidates: &[crate::wtransport::Candidate],
) -> Option<std::net::SocketAddr> {
    candidates.first().map(|c| c.addr)
}

/// C14 失败形态（Q-H F17；纯函数便于单测）：`%v` = Rust io 错误文案——**平台文案
/// 差异已登记**（与 E20a 的「Rust 按断点分两类」同类纪律；不伪造 Go 错误串）。
pub(crate) fn format_outbound_caps_fail(e: &std::io::Error) -> String {
    format!("出口能力：参照点探测失败（{e}）—— 本机网络到出口的 UDP 不通或出口未应答")
}

fn build_client(
    token: &Token,
    ident: &Identity,
    candidates: &[Candidate],
    logf: &Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<Client, SessionErr> {
    Client::start(CoreConfig {
        peer_id: token.peer_id,
        // Secret 非 Copy（F8d）：调用方持有 &Token ⇒ 克隆一份交引擎
        secret: token.secret.clone(),
        identity: ident.clone(),
        candidates: candidates.to_vec(),
        logf: Arc::clone(logf),
    })
    .map_err(SessionErr::DataPlane)
}

/// 阶梯动作面：引擎 RPC 的有界包装（动作预算 2s；R3 附带候选重投）。
struct EngineTransport<'a> {
    shared: &'a Shared,
    client: &'a Client,
}

impl recover::LadderTransport for EngineTransport<'_> {
    fn apply(&mut self, a: Action) -> Result<(), ActionError> {
        match a {
            Action::ResetPeerSession => self
                .client
                .reset_peer_session_bounded(recover::ACTION)
                .map_err(action_err),
            Action::Rebind => self.client.rebind_bounded(recover::ACTION).map_err(action_err),
            Action::Rearm => {
                // R3 = 清采纳重赛跑 + 候选重投（cache.Merge(static)——Go Transport.Rearm）
                self.client
                    .rearm_bounded(recover::ACTION)
                    .map_err(action_err)?;
                let cands = self.shared.merged_candidates();
                self.client.set_candidates(cands);
                // 域名重解析并发另跑（P0-4：Rearm 动作本体零 DNS 等待——恢复阶梯的
                // 动作预算 2s 有界，塞进 5s DNS 预算会把蜂窝下常态 DNS 空等打成
                // rc=-3 误升整套重建）。
                if let Some(rf) =
                    crate::syncutil::read_unpoison(&self.shared.domain_refresher).clone()
                {
                    rf.refresh_async();
                }
                Ok(())
            }
        }
    }

    fn refresh_reg(&mut self) -> Result<RefreshRegOutcome, ActionError> {
        match self.client.refresh_reg_bounded(recover::ACTION) {
            Ok(true) => Ok(RefreshRegOutcome::Sent),
            Ok(false) => Ok(RefreshRegOutcome::Skipped),
            Err(e) => Err(action_err(e)),
        }
    }

    fn note_path_alive(&mut self) {
        // 中继采纳路径不落已验证（Go NotePathAlive 跳中继）
        let snap = self.client.snapshot();
        if snap.via == Via::Direct {
            if let Some(ep) = snap.ep {
                self.shared.mark_round_trip(ep);
            }
        }
    }
}

fn action_err(e: ConnErr) -> ActionError {
    match e {
        ConnErr::Timeout => ActionError::Timeout,
        other => ActionError::Failed(other.to_string()),
    }
}

// ---------- hint 处理 / 落盘去抖 / 打洞 / 旁路探测 ----------

/// hint 回调安装（start 与 rebuild_session 共用——评审中-5：重建换入的新世代必须
/// 重装，否则 HINT 通路静默失效）。
fn install_hint_callback(shared: &Arc<Shared>) {
    let sh = Arc::clone(shared);
    shared
        .current()
        .set_on_hint(Arc::new(move |addr: &str| {
            // 回调纪律：不阻塞、不 RPC 回引擎（死锁）；解析失败静默丢
            #[cfg(feature = "test-seams")]
            if sh.suppress_hints.load(std::sync::atomic::Ordering::SeqCst) {
                return;
            }
            if let Ok(ap) = addr.parse::<SocketAddr>() {
                // 地址键归一（M2 代码门 G3，同类 D1）：hint 串是**学习集/候选集的入口**
                // （缓存键、Merge 候选键、日志）——mapped 与纯 v4 两形态不得各占一份。
                let _ = sh.hint_tx.send(crate::udpbatch::unmap_v4_in6(ap));
            }
        }));
}

/// hint 处理线程（Go Transport 的 hint 链路：观察 → 候选重投 → 打洞）。
/// 驱动线程只投队列；RPC（set_candidates/rearm_soft）与落盘信号都在这里做。
/// 退出 = stop 标志（recv_timeout 轮询——中-4：通道因 Shared 环引用不会自然关闭）。
fn spawn_hint_handler(shared: Arc<Shared>, rx: std::sync::mpsc::Receiver<SocketAddr>) -> Option<JoinHandle<()>> {
    let logf = Arc::clone(&shared.logf);
    match std::thread::Builder::new()
        .name("homeway-hint".into())
        .spawn(move || {
            while !shared.stop.load(Ordering::SeqCst) {
                let Ok(addr) = rx.recv_timeout(Duration::from_millis(500)) else {
                    continue;
                };
                // 缓存观察（最新鲜的不可信线索）
                if let Some(c) = &shared.cache {
                    crate::syncutil::lock_unpoison(c)
                        .observe(addr, EndpointSource::Hint, SystemTime::now());
                }
                // 候选重投（学习到的地址进赛跑集）
                let merged = shared.merged_candidates();
                shared.current().set_candidates(merged);
                let _ = shared.save_tx.send(());
                punch_to(&shared, addr);
            }
        }) {
        Ok(h) => Some(h),
        Err(e) => {
            // F7-1：spawn 失败不静默（只记行——hint 通路缺失不动数据面）
            crate::syncutil::log_spawn_failed(&logf, "hint 处理线程", &e, "本会话不做 hint 打洞（候选仍按巡检刷新）");
            None
        }
    }
}

/// 收到对端地址线索后打一发「握手兼打洞」（Go punchTo：节流 5s + RearmSoft + 拨 :1）。
/// 不碰 wireguard 内部：软赛跑清采纳重武装后，出站包镜像到全部候选（含刚学到的
/// hint 地址），那发 WG 握手同时充当打洞包；拨 :1 拿 RST = 路径通了。
fn punch_to(shared: &Arc<Shared>, addr: SocketAddr) {
    // F6-5：停机中不发起新探测（在途成本收窄——path_probe 5s 预算不改）
    if shared.stop.load(Ordering::SeqCst) {
        return;
    }
    {
        let mut lp = crate::syncutil::lock_unpoison(&shared.last_punch);
        if lp.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        *lp = Some(Instant::now());
    }
    let client = shared.current();
    // F3b/N1：hint 线程必须能退出——无界 RPC 改有界（动作预算同刻度）
    let _ = client.rearm_soft_bounded(recover::ACTION);
    if let Some(c) = &shared.cache {
        crate::syncutil::lock_unpoison(c).note_rearm();
    }
    if let Some(rf) = crate::syncutil::read_unpoison(&shared.domain_refresher).clone() {
        rf.refresh_async();
    }
    (shared.logf)(&format!("中继 hint {addr} → 重新武装候选赛跑，打一发握手兼打洞"));
    // 打洞探测（5s 预算；refused = 路径通——RST 说明握手与路径都通了）
    match client.path_probe(Duration::from_secs(5)) {
        Ok(()) => {
            (shared.logf)("打洞后探测成功：会话可能已漂移到直连（看 link 行确认）");
            let snap = client.snapshot();
            if snap.via == Via::Direct {
                if let Some(ep) = snap.ep {
                    shared.mark_round_trip(ep);
                }
            }
        }
        Err(e) => {
            (shared.logf)(&format!("打洞后探测未成功（{e}）—— 继续停留在原路径（中继/旧直连）"));
        }
    }
}

/// 缓存落盘去抖（Go FIX-16：信号合并 + 1s 去抖窗；收口由 Session::stop 终写）。
/// 退出 = stop 标志（中-4）。
fn spawn_save_loop(shared: Arc<Shared>, rx: std::sync::mpsc::Receiver<()>) -> Option<JoinHandle<()>> {
    let logf = Arc::clone(&shared.logf);
    match std::thread::Builder::new()
        .name("homeway-cache-save".into())
        .spawn(move || {
            loop {
                if shared.stop.load(Ordering::SeqCst) {
                    return;
                }
                let Ok(()) = rx.recv_timeout(Duration::from_millis(500)) else {
                    continue;
                };
                std::thread::sleep(Duration::from_secs(1)); // 去抖窗（窗内信号合并）
                while rx.try_recv().is_ok() {}
                if let Some(c) = &shared.cache {
                    if let Err(e) = crate::syncutil::lock_unpoison(c).save(SystemTime::now()) {
                        (shared.logf)(&format!("端点缓存落盘失败：{e}"));
                    }
                }
            }
        }) {
        Ok(h) => Some(h),
        Err(e) => {
            // F7-1：spawn 失败不静默（端点缓存只靠收工终写兜底）
            crate::syncutil::log_spawn_failed(&logf, "缓存落盘线程", &e, "端点缓存只靠收工终写（无去抖落盘）");
            None
        }
    }
}

/// 旁路探测候选（Go ProbeCandidates：只打直连条目——探测中继端点拿到的是中继自己的
/// 列表，污染候选表；应答端点经消费卫兵后入缓存）。
fn run_probe_candidates(shared: &Arc<Shared>) {
    // 域名同步重解析（Go ProbeCandidates：3s 预算，等待有界——DNS 慢不能拖死探测
    // 本身；单飞由调用频度〔60s 巡检拍〕天然限住）。
    if let Some(rf) = crate::syncutil::read_unpoison(&shared.domain_refresher).clone() {
        if let Some(fresh) = rf.refresh_sync(crate::wtransport::domain_eps::PROBE_SYNC_BUDGET) {
            *crate::syncutil::lock_unpoison(&shared.domain_cands) = fresh;
            let merged = shared.merged_candidates();
            shared.current().set_candidates(merged);
        }
    }
    // 只打直连条目（探测中继端点拿到的是中继自己的列表，污染候选表）
    let targets: Vec<SocketAddr> = shared
        .merged_candidates()
        .into_iter()
        .filter(|c| !c.relay)
        .map(|c| c.addr)
        .collect();
    if targets.is_empty() {
        return;
    }
    let sh = Arc::clone(shared);
    let mut on_ep = move |ep: SocketAddr| {
        if let Some(c) = &sh.cache {
            crate::syncutil::lock_unpoison(c)
                .observe(ep, EndpointSource::Probe, SystemTime::now());
        }
        let merged = sh.merged_candidates();
        sh.current().set_candidates(merged);
        let _ = sh.save_tx.send(());
    };
    crate::probe::probe_candidates(&targets, Duration::from_secs(4), shared.logf.as_ref(), &mut on_ep);
}

// ---------- 巡检（controller；拍头基准） ----------

/// relayUpgradeStreak：连续停留中继的拍数（一旦不是中继就清零）。
/// 纯函数（Go tunmode.go:139-141 同义——「纯函数，单测覆盖」）。
fn relay_upgrade_streak(via: Via, streak: u32) -> u32 {
    if via == Via::Relay {
        streak + 1
    } else {
        0
    }
}

/// relayUpgradeDue：该不该做这一轮升级尝试（via=="relay" && streak >= 5）。
fn relay_upgrade_due(via: Via, streak: u32) -> bool {
    via == Via::Relay && streak >= RELAY_UPGRADE_EVERY
}

struct PatrolBeat {
    streak: u32,
    last_counted: Option<Instant>,
    last_reg: Option<Instant>,
    noise_since: Option<Instant>,
    gated: bool,
    relay_streak: u32,
}

fn patrol_loop(shared: Arc<Shared>) {
    let mut beat = PatrolBeat {
        streak: 0,
        last_counted: None,
        last_reg: None,
        noise_since: None,
        gated: false,
        relay_streak: 0,
    };
    let mut last_tick = Instant::now();
    loop {
        // 睡到下一拍（500ms 粒度检查收工）
        let next = last_tick + PATROL_INTERVAL;
        while Instant::now() < next {
            if shared.stop.load(Ordering::SeqCst) {
                return;
            }
            let nap = Duration::from_millis(500).min(next.saturating_duration_since(Instant::now()));
            std::thread::sleep(nap);
        }
        // 拍头基准（Go patrol :593-595：now/gap 在探测与阶梯之前取——阶梯耗时不计入
        // 空窗判定）
        let now = Instant::now();
        let gap = now.duration_since(last_tick);
        last_tick = now;
        let gen = shared.gen_now();
        let client = shared.current();

        // 巡检空窗 = 进程被冻结过：主动恢复，不等连败
        if gap > 2 * PATROL_INTERVAL {
            (shared.logf)(&format!(
                "巡检空窗 {}（判为进程被挂起）—— 主动重绑本地 socket",
                fmt_duration_go_secs(gap)
            ));
            session_recover(&shared, gen, "挂起唤醒");
        }

        // 本拍探测（10s）
        let started = Instant::now();
        // F8c：保留真因（此前 `.is_ok()` 丢弃错误 ⇒ 失败行恒写「探测超时」= 归因失真；
        // 对齐隧道域同族行 `probe.unwrap_err()` 形态）
        let probe = client.path_probe(PROBE_TIMEOUT);
        let probe_ok = probe.is_ok();
        let rtt = started.elapsed();

        // 补注册（5min；成功才推进时刻）
        if should_refresh_reg(beat.last_reg, now)
            && client
                .refresh_reg_result_bounded(recover::ACTION)
                .unwrap_or(false) {
                beat.last_reg = Some(now);
            }

        if probe_ok {
            beat.streak = 0;
            if beat.gated {
                beat.gated = false;
                (shared.logf)("巡检恢复：门控态结束（成功拍清零）");
            }
            let snap = client.snapshot();
            shared.set_link(snap.via, snap.ep, rtt);
            // 耗尽计数清零（巡检确认健康——很久以前的一次耗尽不得与之后拼对）
            crate::syncutil::lock_unpoison(&shared.ladder).exhausted = 0;
            (shared.logf)(&format!(
                "link: via={} ep={} rtt={}ms（服务会话巡检）",
                snap.via.as_str(),
                snap.ep.map(|a| a.to_string()).unwrap_or_default(),
                rtt.as_millis()
            ));
            // 中继升直连（Go tunmode.go:987-1002：**软赛跑组合动作**——RearmSoft +
            // 候选重投（cache.Merge(static)，Go Transport.RearmSoft 复合）+ 一发
            // PathProbe 载体 + 成功且 via 变化打升级成功行并刷 link；Rust 无域名候选
            // ⇒ 无 refreshDomain 对应面〔登记〕）
            beat.relay_streak = relay_upgrade_streak(snap.via, beat.relay_streak);
            if relay_upgrade_due(snap.via, beat.relay_streak) {
                {
                    beat.relay_streak = 0;
                    (shared.logf)(&format!(
                        "RELAY-UPGRADE：已在中继停留 {}，重新武装赛跑试直连（下一发出站包镜像到全部候选）",
                        fmt_duration_go_secs(RELAY_UPGRADE_EVERY * PATROL_INTERVAL)
                    ));
                    let _ = client.rearm_soft_bounded(recover::ACTION);
                    if let Some(c) = &shared.cache {
                        crate::syncutil::lock_unpoison(c).note_rearm();
                    }
                    let merged = shared.merged_candidates();
                    shared.current().set_candidates(merged);
                    if let Some(rf) =
                        crate::syncutil::read_unpoison(&shared.domain_refresher).clone()
                    {
                        rf.refresh_async();
                    }
                    let _ = shared.save_tx.send(());
                    // 这一发探测包就是「镜像出去试直连」的出站包（perTry 10s 窗）
                    let ustarted = Instant::now();
                    if client.path_probe(PROBE_TIMEOUT).is_ok() {
                        let st2 = client.snapshot();
                        if st2.via != snap.via {
                            let urtt = ustarted.elapsed();
                            (shared.logf)(&format!(
                                "RELAY-UPGRADE：升级成功 → via={} ep={} rtt={}ms",
                                st2.via.as_str(),
                                st2.ep.map(|a| a.to_string()).unwrap_or_default(),
                                urtt.as_millis()
                            ));
                            shared.set_link(st2.via, st2.ep, urtt);
                        }
                    }
                }
            } else {
                beat.relay_streak = 0;
            }
            // 旁路探测（endpoint-freshness：结果只进缓存与日志，不影响健康判定；
            // 8s 看门狗——超时放弃本轮，探测线程自行收尾）
            {
                let sh = Arc::clone(&shared);
                let logf2 = Arc::clone(&shared.logf);
                match std::thread::Builder::new()
                    .name("homeway-probe-cands".into())
                    .spawn(move || run_probe_candidates(&sh))
                {
                    // 看门狗：探测线程内部有 4s/候选预算
                    Ok(h) => {
                        let _ = h.join().map_err(|_| ());
                    }
                    // F7-1/N6：spawn 失败=panic 面 ⇒ 改记行（本拍无旁路探测，不影响健康判定）
                    Err(e) => crate::syncutil::log_spawn_failed(
                        &logf2,
                        "homeway-probe-cands",
                        &e,
                        "本拍无旁路候选探测（只影响端点新鲜度学习）",
                    ),
                }
            }
            continue;
        }

        // 失败拍：证据门推进（门控路径五分支；CLI 形态 demand 恒 true——「无需求清零」
        // 分支不可达〔登记〕）
        let noise = client
            .snapshot()
            .last_local_send_err
            .is_some_and(|t| t.elapsed() < NOISE_WINDOW);
        // 长停逃逸（本地错误持续超阈值 ⇒ 按质量失败计）
        let noise = {
            let (esc, ns) = noise_escalated(noise, beat.noise_since, now);
            beat.noise_since = ns;
            if esc {
                (shared.logf)(&format!(
                    "本地发送错误持续 {}（长停逃逸）：按质量失败计，进入正常升级链",
                    fmt_duration_go_secs(NOISE_ESCALATE_AFTER)
                ));
                false
            } else {
                noise
            }
        };
        let (n, counted) = patrol_evidence_gate(noise, true, beat.streak, beat.last_counted, now);
        beat.streak = n;
        if !counted {
            if !beat.gated {
                beat.gated = true;
                let why = if noise { "本地发送错误（环境噪声）" } else { "无需求" };
                (shared.logf)(&format!("巡检失败被门控拦下（{why}）→ 计数清零仅记录"));
            }
            continue;
        }
        if beat.gated {
            beat.gated = false;
            // CLI 形态无需求源：该行本不可达；保留分支完整性（Go 需求恢复行）
            (shared.logf)("需求恢复（无需求）：巡检失败重新计入证据");
        }
        beat.last_counted = Some(now);
        // F8c（§5.1 判据变更记录）：归因写真实错误（此前恒「探测超时」）
        (shared.logf)(&format!(
            "巡检失败（连续 {n}）：{}",
            probe.as_ref().err().map(|e| e.to_string()).unwrap_or_default()
        ));
        if n >= FAIL_STREAK_RESET {
            session_recover(&shared, gen, "巡检连续失败");
            (shared.logf)(&format!(
                "连续 {n} 次失败：已重绑本地 socket 并补注册（下一发探测全新握手）"
            ));
            beat.streak = 0;
        }
    }
}

/// 巡检/挂起路径的恢复入口（`deadline=None`——巡检不受调用方预算约束，逐字同旧
/// 行为；带期限的拨号路径走 `Session::recover_until`）。
fn session_recover(shared: &Arc<Shared>, gen: u64, cause: &str) -> LadderRc {
    if shared.gen_now() != gen {
        return LadderRc::Stale;
    }
    let (rc, _waited) = shared.gate.merge_until(Level::R2, None, |lvl, deadline| {
        let rc = session_recover_round(shared, lvl, cause, deadline);
        // 记账在 run 回调内（每轮恰好一次——合并等待者不重复 +1；评审中-1）；
        // Deadline/Stale 不计数（F3-3/C4：单源判据）
        let mut st = crate::syncutil::lock_unpoison(&shared.ladder);
        match recover::exhausted_delta(&rc) {
            Some(false) => st.exhausted = 0,
            Some(true) => st.exhausted += 1,
            None => {}
        }
        rc
    });
    let rebuild = {
        let mut st = crate::syncutil::lock_unpoison(&shared.ladder);
        if st.exhausted >= LADDER_EXHAUST_REBUILD {
            (true, std::mem::take(&mut st.exhausted))
        } else {
            (false, 0)
        }
    };
    if rebuild.0 {
        rebuild_session(shared, &format!("恢复阶梯连续 {} 轮走完仍未恢复", rebuild.1));
    }
    rc
}

fn session_recover_round(
    shared: &Arc<Shared>,
    from: Level,
    cause: &str,
    deadline: Option<Instant>,
) -> LadderRc {
    let client = shared.current();
    let mut tr = EngineTransport { shared, client: &client };
    let mut probe = |d: Duration| client.path_probe(d).is_ok();
    let mut deps = recover::LadderDeps {
        probe: &mut probe,
        tr: &mut tr,
        logf: shared.logf.as_ref(),
        pre_probe: recover::PRE_PROBE,
        verify: recover::VERIFY,
        deadline,
    };
    recover::run_ladder(&mut deps, from, cause)
}

/// 整会话重建（Go rebuildSession：force-stop 同机理，进程内完成；桥不动语义在 CLI
/// 形态无桥，拨号面经世代号自动重定向）。
fn rebuild_session(shared: &Arc<Shared>, reason: &str) {
    let (logf, sh) = (&shared.logf, &**shared);
    {
        let mut st = crate::syncutil::lock_unpoison(&sh.ladder);
        if st.rebuild_at.is_some_and(|t| t.elapsed() < REBUILD_COOLDOWN) {
            (logf)(&format!(
                "REBUILD 整会话重建被限频（{} 内已重建过，继续观察）：原因={reason}",
                fmt_duration_go_ms(REBUILD_COOLDOWN)
            ));
            return;
        }
        st.rebuild_at = Some(Instant::now());
    }
    (logf)(&format!(
        "REBUILD 整会话重建（{reason}）：拆旧会话换新（force-stop 同机理，进程内完成）"
    ));
    if let Some(c) = &sh.cache {
        if let Err(e) = crate::syncutil::lock_unpoison(c).save(SystemTime::now()) {
            (logf)(&format!("端点缓存落盘失败：{e}"));
        }
    }
    match build_client(&token_of(sh), &sh.identity, &sh.static_cands, logf) {
        Ok(new) => {
            let new = Arc::new(new);
            // 孤儿守卫（Go FIX-04 同义；评审低-14）：构建窗口内收工 ⇒ 新会话弃用
            if sh.stop.load(Ordering::SeqCst) {
                // Q-G F5：有界收工（到点 detach——引擎线程自行退出）
                if !new.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
                    (logf)("等待 client 线程收工超时（CLIENT_CLOSE_BUDGET）——放行自退（引擎线程由收割线程收口）");
                }
                (logf)("REBUILD 会话已在构建窗口内收工——新会话弃用（防孤儿）");
                return;
            }
            let old = {
                let mut w = crate::syncutil::write_unpoison(&sh.client);
                let old = std::mem::replace(&mut *w, Arc::clone(&new));
                sh.gen.fetch_add(1, Ordering::Release);
                old
            };
            install_hint_callback(shared); // 新世代重装 hint 回调（评审中-5）
            if shared.relay_lock {
                shared.current().set_relay_only(); // 测试缝随世代重装
            }
            // Q-G F5：旧会话有界收工（build 期间可能挂在引擎 RPC 上——到点 detach）
            if !old.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
                (logf)("等待 client 线程收工超时（CLIENT_CLOSE_BUDGET）——放行自退（引擎线程由收割线程收口）");
            }
            (logf)("REBUILD 新会话已换入（首个出站包将重新注册+赛跑）");
        }
        Err(e) => {
            (logf)(&format!("REBUILD 新会话建立失败：{e}（按 failed 收工）"));
            sh.set_state(SessState::Failed, &format!("重建失败：{e}"));
        }
    }
}

/// 重组最小 Token（重建只需 peer/secret；候选 = static_cands 已另存）。
fn token_of(sh: &Shared) -> Token {
    Token {
        peer_id: crate::token::PeerId::from(sh.peer_pub),
        secret: sh.secret.clone(),
        endpoints: vec![],
        // 重建路径不消费 RPK（钉定值是岛侧输入，S2 接线时随 token 解析带过）
        rpk: None,
    }
}


// ---------- 纯决策函数（Go patrolrule.go / PatrolEvidenceGate 同串语义） ----------

/// 补注册到点判定（ShouldRefreshReg：last 无基线 = 首拍就补）。
fn should_refresh_reg(last: Option<Instant>, now: Instant) -> bool {
    last.is_none_or(|t| now.duration_since(t) >= REG_REFRESH_EVERY)
}

/// 巡检证据门五分支真值表（成功清零在调用方；噪声/无需求清零 / 窗口作废 / +1）。
fn patrol_evidence_gate(
    local_noise: bool,
    demand: bool,
    fail_streak: u32,
    last_counted: Option<Instant>,
    now: Instant,
) -> (u32, bool) {
    if local_noise || !demand {
        return (0, false);
    }
    let mut fail_streak = fail_streak;
    if fail_streak > 0 && last_counted.is_some_and(|t| now.duration_since(t) > PATROL_FAIL_WINDOW) {
        fail_streak = 0;
    }
    (fail_streak + 1, true)
}

/// 噪声长停逃逸状态机（NoiseEscalated：首拍记起点、持续超阈值判逃逸并重计时）。
fn noise_escalated(noise: bool, since: Option<Instant>, now: Instant) -> (bool, Option<Instant>) {
    if !noise {
        return (false, None);
    }
    match since {
        None => (false, Some(now)),
        Some(t) if now.duration_since(t) >= NOISE_ESCALATE_AFTER => (true, None),
        Some(t) => (false, Some(t)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evidence_gate_five_branches() {
        // 噪声/无需求 → 清零不计
        assert_eq!(patrol_evidence_gate(true, true, 2, None, Instant::now()), (0, false));
        assert_eq!(patrol_evidence_gate(false, false, 2, None, Instant::now()), (0, false));
        // 正常计数
        let t0 = Instant::now();
        assert_eq!(
            patrol_evidence_gate(false, true, 1, Some(t0), t0 + Duration::from_secs(60)),
            (2, true)
        );
        assert_eq!(patrol_evidence_gate(false, true, 0, None, t0), (1, true));
        // 窗口作废：上次计入距今 > 10min ⇒ 重来
        assert_eq!(
            patrol_evidence_gate(false, true, 2, Some(t0), t0 + Duration::from_secs(601)),
            (1, true),
            "窗口作废后从 1 起计"
        );
    }

    #[test]
    fn noise_escalation_state_machine() {
        let base = Instant::now();
        assert_eq!(noise_escalated(true, None, base), (false, Some(base)), "首拍记起点");
        assert_eq!(
            noise_escalated(true, Some(base), base + Duration::from_secs(100)),
            (false, Some(base)),
            "持续但未满 3min"
        );
        assert_eq!(
            noise_escalated(true, Some(base), base + NOISE_ESCALATE_AFTER),
            (true, None),
            "满 3min 逃逸 + 重计时"
        );
        assert_eq!(noise_escalated(false, Some(base), base), (false, None), "噪声消失清零");
    }

    #[test]
    fn refresh_reg_schedule() {
        let base = Instant::now();
        assert!(should_refresh_reg(None, base), "无基线首拍即补");
        assert!(!should_refresh_reg(Some(base), base + REG_REFRESH_EVERY - Duration::from_secs(1)));
        assert!(should_refresh_reg(Some(base), base + REG_REFRESH_EVERY));
    }

    // ---------- Q-F F3 / F6 / F2 的会话面回归 ----------

    /// F3：期限已到 ⇒ 阶梯立即放弃（Deadline）、**不计入 exhausted**、无重建。
    #[test]
    fn recover_deadline_not_counted_and_no_rebuild() {
        let s = Session::synthetic_failed_for_test("出口不可达：合成");
        let (rc, waited) = s.recover_until(
            Level::R2,
            "拨号失败",
            Some(Instant::now() - Duration::from_millis(1)),
        );
        assert_eq!(rc, LadderRc::Deadline);
        assert!(waited, "执行方真跑了一轮（第二返回值 = true）");
        assert_eq!(
            crate::syncutil::lock_unpoison(&s.shared.ladder).exhausted,
            0,
            "Deadline 不计耗尽（设计门 C4/D12）"
        );
        assert!(
            crate::syncutil::lock_unpoison(&s.shared.ladder).rebuild_at.is_none(),
            "不得触发整会话重建"
        );
        s.stop();
    }

    /// F3：等待方到点 ⇒ 跳过重建决策（在途轮未完不得被 rebuild 拆掉 Client）。
    #[test]
    fn recover_wait_timeout_skips_rebuild() {
        let s = Arc::new(Session::synthetic_failed_for_test("出口不可达：合成"));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        // 占住闸：模拟「在途轮」（无期限，等释放）
        let s2 = Arc::clone(&s);
        let exec = std::thread::spawn(move || {
            s2.shared.gate.merge_until(Level::R2, None, move |_lvl, _d| {
                let _ = release_rx.recv();
                // 在途轮结果 = 耗尽（若等待方到点仍触发重建决策，这里就会把
                // exhausted 顶到阈值以上）
                LadderRc::Exhausted
            })
        });
        std::thread::sleep(Duration::from_millis(50)); // 等置位
        let (rc, waited) = s.recover_until(
            Level::R3,
            "拨号失败",
            Some(Instant::now() + Duration::from_millis(100)),
        );
        assert_eq!(rc, LadderRc::Deadline);
        assert!(!waited, "等待方到点 ⇒ 第二返回值 false");
        assert!(
            crate::syncutil::lock_unpoison(&s.shared.ladder).rebuild_at.is_none(),
            "等待到点不得触发重建（在途轮未完）"
        );
        assert_eq!(
            crate::syncutil::lock_unpoison(&s.shared.ladder).exhausted,
            0,
            "等待方不记账"
        );
        let _ = release_tx.send(());
        let (rc_exec, waited_exec) = exec.join().unwrap();
        assert_eq!((rc_exec, waited_exec), (LadderRc::Exhausted, true));
        s.stop();
    }

    /// F6-4 段 4（设计门 C1）：缓存锁被在途落盘占住 ⇒ `stop()` **快跳不等待**
    /// （`try_lock`）+ 记行跳过终写；failed 终态保留（F2 依赖）。
    #[test]
    fn stop_skips_cache_final_write_when_lock_busy() {
        let dir = std::env::temp_dir().join(format!("hw-qf-sess-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let (logf, logs) = {
            let (tx, rx) = std::sync::mpsc::channel::<String>();
            let l: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |s: &str| {
                let _ = tx.send(s.to_owned());
            });
            (l, rx)
        };
        let s = Arc::new(Session::synthetic_for_test(
            SessState::Failed,
            "出口不可达：合成",
            Some(dir.clone()),
            Some(logf),
        ));
        let cache = Arc::clone(&s.shared);
        std::thread::scope(|scope| {
            // 持缓存锁 1.2s（阻塞获取会等满它；try_lock 必须快跳）
            scope.spawn(|| {
                let guard = crate::syncutil::lock_unpoison(cache.cache.as_ref().unwrap());
                std::thread::sleep(Duration::from_millis(1200));
                drop(guard);
            });
            std::thread::sleep(Duration::from_millis(120)); // 确保锁已被持有
            let t0 = Instant::now();
            s.stop();
            assert!(
                t0.elapsed() < Duration::from_millis(600),
                "锁忙时 stop 必须快跳（实耗 {:?}）",
                t0.elapsed()
            );
        });
        let lines: Vec<String> = logs.try_iter().collect();
        assert!(
            lines.iter().any(|l| l.contains("收工缓存终写跳过（锁被在途落盘占用）")),
            "跳过终写必须记行：{lines:?}"
        );
        // failed 终态保留（F2：失败实例入槽后 status 面仍读得到 failed+原因）
        let snap = s.snapshot();
        assert_eq!(snap.state, SessState::Failed);
        assert_eq!(snap.reason, "出口不可达：合成");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// F6-4 段 5：`stop_within` 的**正常路径**（期限内收工 ⇒ true，wake fd 本次关）
    /// 与**重入**（已收工 ⇒ 直接 true，不重复等待/不重复关 fd）。
    /// 到点 detach + 收割线程收口 fd 的覆盖在 `wgcore::tests::
    /// stop_within_detaches_and_reaper_closes_wake_fd`（需 Client 私有字段访问）。
    #[test]
    fn stop_within_normal_path_and_reentrant() {
        use crate::wgcore::{Client, CoreConfig};
        let ident = Identity::ephemeral().expect("临时身份");
        let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_s: &str| {});
        let client = Arc::new(
            Client::start(CoreConfig {
                peer_id: crate::token::PeerId::from([9u8; 32]),
                secret: crate::token::Secret::from([8u8; 32]),
                identity: ident,
                candidates: vec![],
                logf,
            })
            .expect("客户端可起"),
        );
        // 正常路径：期限内收工 ⇒ true 且本次关 fd（wake_wr 已被取空）
        assert!(
            client.stop_within(Instant::now() + Duration::from_secs(5)),
            "引擎正常退出 ⇒ 期限内收工"
        );
        // 重入调用：直接 true（不重复等待/不重复关 fd）
        assert!(client.stop_within(Instant::now() + Duration::from_millis(1)));
    }

    /// F6-3：Drop 路径零 panic（毒锁 + drop 会话 ⇒ catch_unwind 必须 Ok）。
    #[test]
    fn drop_session_never_panics() {
        let s = Session::synthetic_failed_for_test("出口不可达：合成");
        // 毒掉快照锁（持锁线程 panic）
        {
            let sh = Arc::clone(&s.shared);
            let _ = std::thread::spawn(move || {
                let _g = sh.snapshot.lock().unwrap();
                panic!("毒锁注入");
            })
            .join();
        }
        assert!(s.shared.snapshot.is_poisoned(), "前置：锁确已中毒");
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(s)));
        assert!(r.is_ok(), "Drop 链不得 panic（锁中毒 + 收工全链）");
    }
}

#[cfg(test)]
mod upgrade_streak_tests {
    use super::*;

    /// Go tunmode.go:139-150 纯函数语义：成功拍推进 / 非 relay 清零 / 触发归零 /
    /// due 双条件。
    #[test]
    fn streak_and_due_semantics() {
        // 推进：relay 拍 +1
        assert_eq!(relay_upgrade_streak(Via::Relay, 0), 1);
        assert_eq!(relay_upgrade_streak(Via::Relay, 4), 5);
        // 非 relay 清零（direct/none 都清）
        assert_eq!(relay_upgrade_streak(Via::Direct, 4), 0);
        assert_eq!(relay_upgrade_streak(Via::None, 4), 0);
        // due：relay 且 streak >= 5
        assert!(!relay_upgrade_due(Via::Relay, 4));
        assert!(relay_upgrade_due(Via::Relay, 5));
        assert!(!relay_upgrade_due(Via::Direct, 100));
    }

    // ---------- Q-H F17：C14 出口能力行 ----------

    /// 本地 UDP 桩：收请求 → `probe::respond_ex` 应答（build/flags 注入）。
    fn ping_stub(build: &str, flags: u8) -> std::net::SocketAddr {
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap();
        let build = build.to_owned();
        std::thread::spawn(move || {
            let mut buf = [0u8; 512];
            if let Ok((n, from)) = sock.recv_from(&mut buf) {
                if let Some(resp) = crate::probe::respond_ex(&buf[..n], &build, flags, &[]) {
                    let _ = sock.send_to(&resp, from);
                }
            }
        });
        addr
    }

    /// ① 纯函数：四组位组合 + 空 build + rtt 两档（含 C14 真实样例逐字）。
    #[test]
    fn c14_line_format_matches_go() {
        use crate::probe::PingResult;
        let r = |flags: u8, build: &str, rtt: Duration| PingResult {
            rtt,
            build: build.to_owned(),
            flags,
            endpoints: Vec::new(),
        };
        // C14 真实样例（INTEROP-CRITERIA C14 行）逐字同串。
        assert_eq!(
            format_outbound_caps_line(&r(0b0011, "homewayd-dev", Duration::ZERO)),
            "出口能力：构建 homewayd-dev ｜ 默认路径 UDP：DNS:53 可用 / 通用（非 53）可用 / 实测 还没实测样本（这台出口还没转发过 UDP） ｜ 探测往返 0s"
        );
        // bit2 = 有回包（实测可用）。
        assert!(format_outbound_caps_line(&r(0b0111, "b", Duration::from_millis(3)))
            .contains("实测 有回包（可用）"));
        // bit4 = 有转发会话但无回包。
        assert!(format_outbound_caps_line(&r(0b10000, "b", Duration::from_millis(3)))
            .contains("实测 无回包 —— 这条路不回这类 UDP，QUIC 会超时回落 TCP"));
        // 无位 = 还没实测样本；DNS 无位 = 不可用。
        let l = format_outbound_caps_line(&r(0, "", Duration::from_micros(1500)));
        assert!(l.contains("构建 （未标注） ｜"), "{l}");
        assert!(l.contains("DNS:53 不可用 / 通用（非 53）不可用"), "{l}");
        assert!(l.ends_with("探测往返 2ms"), "1.5ms Round(ms) = 2ms：{l}");
    }

    /// ② 本地 UDP 桩端到端：整行同串（前缀段）且 rtt 域名面 = 毫秒形态。
    #[test]
    fn c14_line_end_to_end_against_local_stub() {
        let addr = ping_stub("homewayd-test", 0b0011);
        let r = crate::probe::ping_ex(addr, 16, Duration::from_secs(2)).expect("本地桩必须应答");
        let line = format_outbound_caps_line(&r);
        assert!(
            line.starts_with(
                "出口能力：构建 homewayd-test ｜ 默认路径 UDP：DNS:53 可用 / 通用（非 53）可用 / 实测 还没实测样本（这台出口还没转发过 UDP） ｜ 探测往返 "
            ),
            "{line}"
        );
    }

    /// ③ 失败形态（无应答、50ms 预算）：错误 + 失败行文案。
    #[test]
    fn c14_failure_line_on_no_response() {
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = sock.local_addr().unwrap(); // 绑而不答
        let e = crate::probe::ping_ex(addr, 16, Duration::from_millis(50)).unwrap_err();
        let line = format_outbound_caps_fail(&e);
        assert!(line.starts_with("出口能力：参照点探测失败（"), "{line}");
        assert!(line.ends_with("）—— 本机网络到出口的 UDP 不通或出口未应答"), "{line}");
    }

    /// ④ 目标选取（设计门 A3 + 代码门 M6）：**站点经 `c14_probe_target` 取目标**
    /// （纯函数单测钉接线——只钉 split_and_resolve 形状时，把站点改回
    /// `static_cands.first()` 也不会红）。
    #[test]
    fn c14_target_is_first_resolved_candidate() {
        use crate::token::{EndpointKind, EndpointRef};
        use crate::wtransport::Candidate as WC;
        let logf: crate::Logf = Arc::new(|_| {});
        // 空表 ⇒ None（不索引）。
        assert!(c14_probe_target(&[]).is_none());
        // 纯静态列表 ⇒ 首项（这是**可以**命中的场景，但域名-only 下不可用它）。
        let statics = [
            WC { addr: "10.1.2.3:40003".parse().unwrap(), relay: false },
            WC { addr: "10.1.2.4:40004".parse().unwrap(), relay: false },
        ];
        assert_eq!(c14_probe_target(&statics).unwrap().port(), 40003);
        // 域名-only token（static_base 空）：候选非空且首项 = 域名解析地址。
        let dom = EndpointRef::new("localhost:40001", EndpointKind::Direct);
        let inputs = crate::wtransport::domain_eps::split_and_resolve(&[dom], &logf);
        assert!(inputs.static_base.is_empty(), "域名条目不进 static_base（旧 A3 误取面）");
        let t = c14_probe_target(&inputs.candidates).expect("域名首解必须有候选");
        assert!(t.ip().is_loopback(), "目标 = 域名解析地址：{t}");
        assert_eq!(t.port(), 40001);
        // 混合（域名在前）：目标 = 域名解析地址，不是后面的静态 IP。
        let mixed = [
            EndpointRef::new("localhost:40002", EndpointKind::Direct),
            EndpointRef::new("10.1.2.3:40003", EndpointKind::Direct),
        ];
        let inputs2 = crate::wtransport::domain_eps::split_and_resolve(&mixed, &logf);
        let t2 = c14_probe_target(&inputs2.candidates).unwrap();
        assert_eq!(t2.port(), 40002, "目标 = token 序首个已解析候选");
        assert!(t2.ip().is_loopback());
        // C13（M3）：·学习 判定集 = token 已解析候选（域名首解也算 known）。
        let known = |addr: std::net::SocketAddr| inputs2.candidates.iter().any(|c| c.addr == addr);
        assert!(known(t2), "域名首解候选必须在 known 集里（Go cands 同义）");
    }
}
