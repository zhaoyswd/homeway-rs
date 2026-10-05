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
use crate::wgcore::{Client, ConnErr, CoreConfig};
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
    /// 重建只消费身份材料 + 候选表）。
    secret: [u8; 32],
    peer_pub: [u8; 32],
    identity: Identity,
    tunnel_ip: std::net::Ipv4Addr,
    /// 【test-seams】hint 抑制（relay-lock 的一部分——直连不可达时 hint 无意义）。
    #[cfg(feature = "test-seams")]
    suppress_hints: std::sync::atomic::AtomicBool,
    /// 【test-seams】中继锁定（relay-lock）——重建世代要重装 bind 锁定。
    relay_lock: bool,
}

impl Shared {
    fn current(&self) -> Arc<Client> {
        self.client.read().expect("世代锁中毒").clone()
    }

    fn gen_now(&self) -> u64 {
        self.gen.load(Ordering::Acquire)
    }

    fn set_state(&self, state: SessState, reason: &str) {
        let mut s = self.snapshot.lock().expect("快照锁中毒");
        s.state = state;
        s.reason = reason.to_owned();
        s.since = Instant::now();
    }

    fn set_link(&self, via: Via, ep: Option<SocketAddr>, rtt: Duration) {
        let mut s = self.snapshot.lock().expect("快照锁中毒");
        s.link = Some(LinkSnapshot {
            via: via.as_str().to_owned(),
            ep: ep.map(|a| a.to_string()).unwrap_or_default(),
            rtt_ms: rtt.as_millis() as i64,
            at_ms: now_unix_ms(),
        });
    }

    fn merged_candidates(&self) -> Vec<Candidate> {
        match &self.cache {
            Some(c) => {
                let c = c.lock().expect("缓存锁中毒");
                c.merge(&self.static_cands, SystemTime::now())
            }
            None => self.static_cands.clone(),
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
            c.lock().expect("缓存锁中毒").mark_verified(addr, src, SystemTime::now());
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

        // 端点解析：直连 + 中继（type=1）都进候选——中继是直连的后备（DirectFirst 窗口）
        let candidates: Vec<Candidate> = cfg
            .token
            .endpoints
            .iter()
            .filter_map(|e| {
                e.addr.parse().ok().map(|addr| Candidate {
                    addr,
                    relay: e.kind == crate::token::EndpointKind::Relay,
                })
            })
            .collect();
        if candidates.is_empty() {
            return Err(SessionErr::NoCandidates);
        }
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
            static_cands: candidates,
            logf: Arc::clone(&logf),
            stop: AtomicBool::new(false),
            secret: *cfg.token.secret.as_bytes(),
            peer_pub: *cfg.token.peer_id.as_bytes(),
            identity: ident,
            tunnel_ip,
            #[cfg(feature = "test-seams")]
            suppress_hints: std::sync::atomic::AtomicBool::new(cfg.relay_only),
            relay_lock: cfg.relay_only,
        });

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
                    let known = shared.static_cands.iter().any(|s| s.addr == c.addr);
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
        let sh = Arc::clone(&shared);
        let patrol = std::thread::Builder::new()
            .name("homeway-patrol".into())
            .spawn(move || patrol_loop(sh))
            .ok();

        Ok(Session { shared, patrol: Mutex::new(patrol), hint: Mutex::new(hint_handle), save: Mutex::new(save_handle) })
    }

    pub fn snapshot(&self) -> SessionSnapshot {
        let mut s = self.shared.snapshot.lock().expect("快照锁中毒").clone();
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
        let rc = self.shared.gate.merge(from, |lvl| {
            let rc = self.recover_round(lvl, cause);
            self.note_ladder_result(&rc);
            rc
        });
        self.maybe_rebuild_if_exhausted();
        rc
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

    /// gate 的 run 回调（每轮恰好执行一次——耗尽记账在 merge 返回后，Go 同位）。
    fn recover_round(&self, from: Level, cause: &str) -> LadderRc {
        session_recover_round(&self.shared, from, cause)
    }

    fn note_ladder_result(&self, rc: &LadderRc) {
        let mut st = self.shared.ladder.lock().expect("阶梯锁中毒");
        match rc {
            LadderRc::Recovered(_) => st.exhausted = 0,
            _ => st.exhausted += 1,
        }
    }

    /// 耗尽计数到阈值整会话重建（触发即归零；限频在 rebuild 内）。
    fn maybe_rebuild_if_exhausted(&self) {
        let n = {
            let mut st = self.shared.ladder.lock().expect("阶梯锁中毒");
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

    fn healing_dial(&self, dst: SocketAddrV4, budget: Duration) -> Result<u64, ConnErr> {
        let t0 = Instant::now();
        let gen = self.shared.gen_now();
        match self.shared.current().connect_deadline(dst, DIAL_FIRST_TRY) {
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
        self.recover_stale(gen, "拨号失败");
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

    /// 收工（幂等）：置 stop → join 三线程（patrol 有界 STOP_WAIT——中-3）→
    /// 缓存终写 → 停当前世代 → Idle（failed 终态保留——低-15：失败原因在状态面不丢）。
    pub fn stop(&self) {
        if self.shared.stop.swap(true, Ordering::SeqCst) {
            return;
        }
        self.shared.set_state(SessState::Stopping, "");
        if let Some(h) = self.patrol.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let deadline = Instant::now() + STOP_WAIT;
            while !h.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(50));
            }
            if h.is_finished() {
                let _ = h.join();
            } else {
                // 有界等待放弃（patrol 卡在长阶梯/探测里——Go serviceStopWait 同义；
                // 线程随后自行退出，JoinHandle 析构即分离）
                (self.shared.logf)("收工等待巡检线程超时（STOP_WAIT）——放行自退");
            }
        }
        if let Some(h) = self.hint.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
        if let Some(h) = self.save.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = h.join();
        }
        if let Some(c) = &self.shared.cache {
            let _ = c.lock().expect("缓存锁中毒").save(SystemTime::now());
        }
        self.shared.current().stop();
        let was_failed = self.shared.snapshot.lock().expect("快照锁中毒").state == SessState::Failed;
        if !was_failed {
            self.shared.set_state(SessState::Idle, "已收工");
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

fn build_client(
    token: &Token,
    ident: &Identity,
    candidates: &[Candidate],
    logf: &Arc<dyn Fn(&str) + Send + Sync>,
) -> Result<Client, SessionErr> {
    Client::start(CoreConfig {
        peer_id: token.peer_id,
        secret: token.secret,
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
                let _ = sh.hint_tx.send(ap);
            }
        }));
}

/// hint 处理线程（Go Transport 的 hint 链路：观察 → 候选重投 → 打洞）。
/// 驱动线程只投队列；RPC（set_candidates/rearm_soft）与落盘信号都在这里做。
/// 退出 = stop 标志（recv_timeout 轮询——中-4：通道因 Shared 环引用不会自然关闭）。
fn spawn_hint_handler(shared: Arc<Shared>, rx: std::sync::mpsc::Receiver<SocketAddr>) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("homeway-hint".into())
        .spawn(move || {
            while !shared.stop.load(Ordering::SeqCst) {
                let Ok(addr) = rx.recv_timeout(Duration::from_millis(500)) else {
                    continue;
                };
                // 缓存观察（最新鲜的不可信线索）
                if let Some(c) = &shared.cache {
                    c.lock()
                        .expect("缓存锁中毒")
                        .observe(addr, EndpointSource::Hint, SystemTime::now());
                }
                // 候选重投（学习到的地址进赛跑集）
                let merged = shared.merged_candidates();
                shared.current().set_candidates(merged);
                let _ = shared.save_tx.send(());
                punch_to(&shared, addr);
            }
        })
        .ok()
}

/// 收到对端地址线索后打一发「握手兼打洞」（Go punchTo：节流 5s + RearmSoft + 拨 :1）。
/// 不碰 wireguard 内部：软赛跑清采纳重武装后，出站包镜像到全部候选（含刚学到的
/// hint 地址），那发 WG 握手同时充当打洞包；拨 :1 拿 RST = 路径通了。
fn punch_to(shared: &Arc<Shared>, addr: SocketAddr) {
    {
        let mut lp = shared.last_punch.lock().expect("punch 锁中毒");
        if lp.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        *lp = Some(Instant::now());
    }
    let client = shared.current();
    let _ = client.rearm_soft();
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
    std::thread::Builder::new()
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
                    if let Err(e) = c.lock().expect("缓存锁中毒").save(SystemTime::now()) {
                        (shared.logf)(&format!("端点缓存落盘失败：{e}"));
                    }
                }
            }
        })
        .ok()
}

/// 旁路探测候选（Go ProbeCandidates：只打直连条目——探测中继端点拿到的是中继自己的
/// 列表，污染候选表；应答端点经消费卫兵后入缓存）。
fn run_probe_candidates(shared: &Arc<Shared>) {
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
            c.lock()
                .expect("缓存锁中毒")
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
        let probe_ok = client.path_probe(PROBE_TIMEOUT).is_ok();
        let rtt = started.elapsed();

        // 补注册（5min；成功才推进时刻）
        if should_refresh_reg(beat.last_reg, now)
            && client.refresh_reg_result().unwrap_or(false) {
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
            shared.ladder.lock().expect("阶梯锁中毒").exhausted = 0;
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
                    let _ = client.rearm_soft();
                    let merged = shared.merged_candidates();
                    shared.current().set_candidates(merged);
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
                let h = std::thread::spawn(move || run_probe_candidates(&sh));
                let _ = h.join().map_err(|_| ()); // 看门狗：探测线程内部有 4s/候选预算
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
        (shared.logf)(&format!("巡检失败（连续 {n}）：探测超时"));
        if n >= FAIL_STREAK_RESET {
            session_recover(&shared, gen, "巡检连续失败");
            (shared.logf)(&format!(
                "连续 {n} 次失败：已重绑本地 socket 并补注册（下一发探测全新握手）"
            ));
            beat.streak = 0;
        }
    }
}

fn session_recover(shared: &Arc<Shared>, gen: u64, cause: &str) -> LadderRc {
    if shared.gen_now() != gen {
        return LadderRc::Stale;
    }
    let rc = shared.gate.merge(Level::R2, |lvl| {
        let rc = session_recover_round(shared, lvl, cause);
        // 记账在 run 回调内（每轮恰好一次——合并等待者不重复 +1；评审中-1）
        let mut st = shared.ladder.lock().expect("阶梯锁中毒");
        match rc {
            LadderRc::Recovered(_) => st.exhausted = 0,
            _ => st.exhausted += 1,
        }
        rc
    });
    let rebuild = {
        let mut st = shared.ladder.lock().expect("阶梯锁中毒");
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

fn session_recover_round(shared: &Arc<Shared>, from: Level, cause: &str) -> LadderRc {
    let client = shared.current();
    let mut tr = EngineTransport { shared, client: &client };
    let mut probe = |d: Duration| client.path_probe(d).is_ok();
    let mut deps = recover::LadderDeps {
        probe: &mut probe,
        tr: &mut tr,
        logf: shared.logf.as_ref(),
        pre_probe: recover::PRE_PROBE,
        verify: recover::VERIFY,
    };
    recover::run_ladder(&mut deps, from, cause)
}

/// 整会话重建（Go rebuildSession：force-stop 同机理，进程内完成；桥不动语义在 CLI
/// 形态无桥，拨号面经世代号自动重定向）。
fn rebuild_session(shared: &Arc<Shared>, reason: &str) {
    let (logf, sh) = (&shared.logf, &**shared);
    {
        let mut st = sh.ladder.lock().expect("阶梯锁中毒");
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
        if let Err(e) = c.lock().expect("缓存锁中毒").save(SystemTime::now()) {
            (logf)(&format!("端点缓存落盘失败：{e}"));
        }
    }
    match build_client(&token_of(sh), &sh.identity, &sh.static_cands, logf) {
        Ok(new) => {
            let new = Arc::new(new);
            // 孤儿守卫（Go FIX-04 同义；评审低-14）：构建窗口内收工 ⇒ 新会话弃用
            if sh.stop.load(Ordering::SeqCst) {
                new.stop();
                (logf)("REBUILD 会话已在构建窗口内收工——新会话弃用（防孤儿）");
                return;
            }
            let old = {
                let mut w = sh.client.write().expect("世代锁中毒");
                let old = std::mem::replace(&mut *w, Arc::clone(&new));
                sh.gen.fetch_add(1, Ordering::Release);
                old
            };
            install_hint_callback(shared); // 新世代重装 hint 回调（评审中-5）
            if shared.relay_lock {
                shared.current().set_relay_only(); // 测试缝随世代重装
            }
            old.stop();
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
        secret: crate::token::Secret::from(sh.secret),
        endpoints: vec![],
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
}
