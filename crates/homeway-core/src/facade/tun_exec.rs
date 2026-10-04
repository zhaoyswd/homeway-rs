//! 隧道域真执行体（语义真源 `baseline:clientcore/cmd/clientcore/tunmode.go` 的
//! runTun2Tailcat + 巡检 goroutine + startDemandPusher + stats ticker + 隧道桥）。
//!
//! 生命周期（工单①世代生命周期，Go 单 goroutine 生命线的 Rust 等价）：
//!
//! ```text
//! prepare（启动即返）→ 世代线程:
//!   起会话（identity + Client）→ 暖机 PathProbe（20s 软失败）
//!   → stage ready → 等 attach fd（60s 死线 → idle/"attach-timeout" 自收工）
//!   → attach（fd 交 WG 引擎，L3 直通）→ stage attached
//!   → 起桥（三座）+ 巡检 + demand pusher + stats
//!   → 等 stop → 收工（桥停 → client 停）→ finish_generation
//! ```
//!
//! 一切退出路径都过 `finish_generation`（终态/放锁/done——防单飞锁泄漏）。
//! 巡检语义对齐**隧道域**（与 `crate::session` 的服务域不同）：失败当拍 R1
//! （补注册+丢会话）、3 连败 R2 起跑、阶梯失败 markUnhealthy("patrol") 交扩展
//! 重建、demand 门控、挂起空窗 R1、中继升直连 5 拍。

use std::io::{self, Read, Write};
use std::net::{SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::identity::{self, Identity};
use crate::session::recover::{
    self, Action, ActionError, LadderRc, LadderTransport, Level, RecoverGate, RefreshRegOutcome,
};
use crate::token::Token;
use crate::wgcore::{Client, ConnErr, CoreConfig};
use crate::wtransport::endpoint_cache::{EndpointCache, EndpointSource};
use crate::wtransport::{Candidate, Via};
use crate::Logf;

use super::bridge_host::{BridgeHost, BridgeStream, WriteHalf};
use super::demand::{self, DemandSignals};
use super::portfwd::PortForwardRule;
use super::stage::TunStage;
use super::tun_shared::{lock_unpoison, TunShared};
use super::tun_status::{LinkIn, RunnerIn, TransportIn};
use super::{TunConfigJson, TunError, TunExecutor, ATTACH_DEADLINE, ATTACH_TIMEOUT_REASON, WARM_TIMEOUT};

/// 巡检间隔（固定 60s——保活 + 可达性；与连接策略真源同值）。
pub const PATROL_INTERVAL: Duration = Duration::from_secs(60);
/// 巡检每拍探测预算。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// 连续计入证据的失败到这个数进 R2 阶梯。
const FAIL_STREAK_LADDER: u32 = 3;
/// 巡检连败证据时间窗（相邻计入失败的间隔超过它 ⇒ 计数作废）。
const PATROL_FAIL_WINDOW: Duration = Duration::from_secs(600);
/// 巡检失败拍的本地噪声回看窗（10s 探测 + 5s 尾窗）。
const NOISE_WINDOW: Duration = Duration::from_secs(15);
/// 本地错误长停逃逸阈值。
const NOISE_ESCALATE_AFTER: Duration = Duration::from_secs(180);
/// 补注册周期（出口设备表按最近注册判活跃）。
const REG_REFRESH_EVERY: Duration = Duration::from_secs(300);
/// 中继停留升直连的拍数。
const RELAY_UPGRADE_EVERY: u32 = 5;
/// demand pusher 节拍（D4：App 出站新鲜 + 接收静默 → 立即下推，不等巡检拍）。
const PUSHER_TICK: Duration = Duration::from_secs(1);
/// stats 周期的下限（防 cfg.stats_secs 填 0 打爆日志；Go normalize 同义下限）。
const STATS_SECS_MIN: i64 = 5;
/// stats 周期缺省（Go normalizeTunConfig 的 StatsSecs 缺省 60）。
const STATS_SECS_DEFAULT: i64 = 60;
/// 旁路探测预算（endpoint-freshness；结果只进缓存与日志）。
const PROBE_CANDIDATES_BUDGET: Duration = Duration::from_secs(4);

// ---------------------------------------------------------------------------
// 会话流适配器（桥远端——工单⑤ dial_port 接缝：流 id ≠ fd，适配成 Read/Write 两半）
// ---------------------------------------------------------------------------

/// 经会话的流连接（`Arc<Client>` + 流 id）。整体 drop 时关流（防半开连接累积）。
struct SharedConn {
    client: Arc<Client>,
    id: u64,
}

impl Drop for SharedConn {
    fn drop(&mut self) {
        let _ = self.client.close(self.id);
    }
}

/// 读半：`client.read(id)` 阻塞到有数据；`Err(Closed)` = EOF。
pub struct SessionReadHalf {
    shared: Arc<SharedConn>,
    buf: Vec<u8>,
    off: usize,
}

impl Read for SessionReadHalf {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.off < self.buf.len() {
            let n = (self.buf.len() - self.off).min(out.len());
            out[..n].copy_from_slice(&self.buf[self.off..self.off + n]);
            self.off += n;
            return Ok(n);
        }
        match self.shared.client.read(self.shared.id) {
            Ok(data) => {
                let n = data.len().min(out.len());
                out[..n].copy_from_slice(&data[..n]);
                self.buf = data;
                self.off = n;
                Ok(n)
            }
            Err(ConnErr::Closed) => Ok(0),
            Err(e) => Err(io::Error::other(e.to_string())),
        }
    }
}

/// 写半：`client.write(id, data)`（栈缓冲满时阻塞回压）；close_write = FIN。
pub struct SessionWriteHalf {
    shared: Arc<SharedConn>,
}

impl Write for SessionWriteHalf {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.shared
            .client
            .write(self.shared.id, data.to_vec())
            .map_err(|e| io::Error::other(e.to_string()))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WriteHalf for SessionWriteHalf {
    fn close_write(&mut self) {
        let _ = self.shared.client.shutdown(self.shared.id);
    }
}

/// 恢复感知的会话流拨号（Go DialTCPPort 的等价面：healing_dial_port——首段短试 →
/// 阶梯 R2 → 余预算重试）。隧道域桥消费。
pub fn session_connect(run: &GenRun, port: u16, budget: Duration) -> io::Result<Box<dyn BridgeStream>> {
    let dst = SocketAddrV4::new(crate::wgcore::SERVER_TUNNEL_IP, port);
    let gen_client = run.current_client();
    let id = healing_dial(&gen_client, run, dst, budget)?;
    let shared = Arc::new(SharedConn { client: gen_client, id });
    Ok(Box::new(SessionStream { shared }))
}

/// 会话流整体（实现 BridgeStream：拆两半给泵）。
pub(crate) struct SessionStream {
    shared: Arc<SharedConn>,
}

impl SessionStream {
    /// 从会话句柄 + 流 id 构造（服务桥消费——dial_via_run）。
    pub(crate) fn shared(client: Arc<Client>, id: u64) -> Self {
        SessionStream { shared: Arc::new(SharedConn { client, id }) }
    }
}

impl BridgeStream for SessionStream {
    fn into_halves(
        self: Box<Self>,
    ) -> io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)> {
        Ok((
            Box::new(SessionReadHalf { shared: Arc::clone(&self.shared), buf: Vec::new(), off: 0 }),
            Box::new(SessionWriteHalf { shared: Arc::clone(&self.shared) }),
        ))
    }
}

/// 恢复感知拨号（Session::healing_dial 的隧道域内联版：4s 首试 → 阶梯 R2 → 余预算）。
fn healing_dial(
    client: &Arc<Client>,
    run: &GenRun,
    dst: SocketAddrV4,
    budget: Duration,
) -> io::Result<u64> {
    const FIRST_TRY: Duration = Duration::from_secs(4);
    let t0 = Instant::now();
    match client.connect_deadline(dst, FIRST_TRY) {
        Ok(id) => return Ok(id),
        Err(e) => {
            if t0.elapsed() >= budget {
                return Err(io::Error::new(io::ErrorKind::TimedOut, e.to_string()));
            }
        }
    }
    // 拨号失败 → 阶梯（世代还在就恢复它；引擎收工 = 如实报错）
    let rc = run.recover(Level::R2, "拨号失败");
    if matches!(rc, LadderRc::Recovered(_)) {
        let remain = budget.saturating_sub(t0.elapsed()).max(Duration::from_millis(1));
        return client
            .connect_deadline(dst, remain)
            .map_err(|e| io::Error::other(e.to_string()));
    }
    Err(io::Error::new(
        io::ErrorKind::NotConnected,
        format!("拨号失败且阶梯未恢复（rc={:?}）", rc.as_rc()),
    ))
}

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
    /// 当前数据面（rebuild 换代；拨号/恢复自动落新世代）。
    client: RwLock<Arc<Client>>,
    /// 恢复闸（按世代隔离）。
    gate: RecoverGate,
    /// 端点学习缓存（None = 不落盘不学）。
    cache: Option<Mutex<EndpointCache>>,
    static_cands: Vec<Candidate>,
    /// 重建材料（Token 的身份面；候选 = static_cands 另存）。
    secret: [u8; 32],
    #[allow(dead_code)] // 重建材料位（对齐 Go tunRun 的会话材料；隧道域不 rebuild——阶梯失败交扩展）
    peer_pub: [u8; 32],
    identity: Identity,
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
    /// portfwd 表（最小面：热替换存表 + runner 状态；监听器执行体后续接）。
    pub pf_rules: Mutex<Vec<PortForwardRule>>,
    /// 世代时间起点（elapsed 面板用）。
    pub started: Instant,
}

impl GenRun {
    pub fn current_client(&self) -> Arc<Client> {
        self.client.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 恢复入口（gate 单飞；隧道域 rc 契约）。
    pub fn recover(&self, from: Level, cause: &str) -> LadderRc {
        let sh = self;
        sh.gate.merge(from, |lvl| run_round(sh, lvl, cause))
    }

    fn merged_candidates(&self) -> Vec<Candidate> {
        match &self.cache {
            Some(c) => {
                let c = lock_unpoison(c);
                c.merge(&self.static_cands, SystemTime::now())
            }
            None => self.static_cands.clone(),
        }
    }

    /// 真实往返落已验证（来源 = static 内 → Token 否则 Hint）。
    fn mark_round_trip(&self, addr: SocketAddr) {
        if let Some(c) = &self.cache {
            let src = if self.static_cands.iter().any(|c| c.addr == addr) {
                EndpointSource::Token
            } else {
                EndpointSource::Hint
            };
            lock_unpoison(c).mark_verified(addr, src, SystemTime::now());
        }
    }
}

/// 阶梯一轮（EngineTransport 的隧道域版——Client 动作面 + 候选重投）。
fn run_round(run: &GenRun, from: Level, cause: &str) -> LadderRc {
    let client = run.current_client();
    let mut tr = TunnelTransport { run, client: &client };
    let mut probe = |d: Duration| client.path_probe(d).is_ok();
    let mut deps = recover::LadderDeps {
        probe: &mut probe,
        tr: &mut tr,
        logf: &|s: &str| (run.logf)(s),
        pre_probe: recover::PRE_PROBE,
        verify: recover::VERIFY,
    };
    recover::run_ladder(&mut deps, from, cause)
}

struct TunnelTransport<'a> {
    run: &'a GenRun,
    client: &'a Client,
}

impl LadderTransport for TunnelTransport<'_> {
    fn apply(&mut self, a: Action) -> Result<(), ActionError> {
        match a {
            Action::ResetPeerSession => self
                .client
                .reset_peer_session_bounded(recover::ACTION)
                .map_err(action_err),
            Action::Rebind => self.client.rebind_bounded(recover::ACTION).map_err(action_err),
            Action::Rearm => {
                // R3 = 清采纳重赛跑 + 候选重投（Go Transport.Rearm 复合）
                self.client
                    .rearm_bounded(recover::ACTION)
                    .map_err(action_err)?;
                let cands = self.run.merged_candidates();
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
        let snap = self.client.snapshot();
        if snap.via == Via::Direct {
            if let Some(ep) = snap.ep {
                self.run.mark_round_trip(ep);
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

// ---------------------------------------------------------------------------
// TunnelExec（TunExecutor 真实现）
// ---------------------------------------------------------------------------

/// 隧道域执行体（真 hub：Client + L3 直通 + 巡检 + 桥）。
/// 世代登记经 `Arc<Mutex<Option<Arc<GenRun>>>>`（世代线程构建后写回——warmup 是
/// `&self`，不持自引用）。
pub struct TunnelExec {
    state: Arc<Mutex<Option<Arc<GenRun>>>>,
    demand: Arc<DemandSignals>,
}

impl TunnelExec {
    /// 构造（demand 与 ClientCore 共享——巡检/demand pusher 的信号源）。
    pub fn new(demand: Arc<DemandSignals>) -> Arc<Self> {
        Arc::new(TunnelExec {
            state: Arc::new(Mutex::new(None)),
            demand,
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
            mtu: cfg.mtu.clamp(1280, 2000) as u32,
            stats_secs: if cfg.stats_secs > 0 {
                cfg.stats_secs.max(STATS_SECS_MIN)
            } else {
                STATS_SECS_DEFAULT
            },
            diag_fd_secs: cfg.diag_fd_secs.max(0),
            dial_ms: if cfg.dial_ms > 0 { cfg.dial_ms } else { 9000 },
            identity_dir: (!cfg.identity_dir.is_empty()).then(|| PathBuf::from(&cfg.identity_dir)),
            endpoint_cache_dir: (!cfg.endpoint_cache_dir.is_empty())
                .then(|| PathBuf::from(&cfg.endpoint_cache_dir)),
            port_forwards: cfg.port_forwards.clone(),
        };
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
            run.stop.store(true, Ordering::Release);
            let _ = run.ev_tx.try_send(GenEvent::Stop);
            // preparing 阶段没有 fd 循环可打断，暖机探测可能挂在引擎 RPC 上——
            // 关客户端是最可靠的第二条打断路径（Go tunStopWait 同义）。
            if run.tun_shared.stage.snapshot().stage == TunStage::Preparing {
                run.current_client().stop();
            }
        }
    }

    fn recover(&self, from: i64, cause: &str) -> i32 {
        let Some(run) = self.gen_run() else {
            return -2; // 没有 attached 隧道（不构成网络结论）
        };
        let lvl = Level::clamp(from);
        run.recover(lvl, cause).as_rc()
    }

    fn runner(&self) -> Option<RunnerIn> {
        let run = self.gen_run()?;
        let (rd, wr) = run.current_client().tun_stats();
        // link 兜底 via="none"（评审 r2-我-1：暖机软失败期 link 未写过 ⇒ 整块 runner
        // 消失 ⇒ exitIp/bridgeAuth 缺 ⇒ 扩展「拒绝建接口（DNS 将无处可去）」——Go 的
        // 软失败自愈路径在 Rust 变启动失败。runner 块只要求世代在场，link 键恒在）
        let link = lock_unpoison(&run.link).clone().unwrap_or(LinkIn {
            via: "none".into(),
            ep: String::new(),
            rtt_ms: 0,
            at_ms: 0,
        });
        let bridge = lock_unpoison(&run.bridge)
            .as_ref()
            .map(|b| {
                let st = b.status();
                super::tun_status::BridgeIn {
                    auth_hex: st.auth_hex,
                    files_sock: st.files_sock,
                    term_sock: st.term_sock,
                    speed_sock: st.speed_sock,
                }
            });
        Some(RunnerIn {
            fd_read_bytes: rd,
            fd_write_bytes: wr,
            pf_accepted: 0,
            pf_fails: 0,
            exit_ip: crate::wgcore::SERVER_TUNNEL_IP.to_string(),
            link,
            port_forwards: portfwd_states(&run),
            bridge,
        })
    }

    fn transport(&self) -> Option<TransportIn> {
        let run = self.gen_run()?;
        let client = run.current_client();
        Some(TransportIn {
            identity: Some((run.identity.short_dev(), run.identity.short_pub())),
            tun_ip: Some(run.tun_ip_string()),
            outbound_at_ms: Some(client.last_outbound_unix_ms()).filter(|v| *v != 0),
            local_err: None, // bind 全候选发送统计面未实现（登记：runner 键 localErr* 暂缺）
        })
    }

    fn request_port_forwards(&self, rules: Vec<PortForwardRule>) -> i32 {
        if let Some(run) = self.gen_run() {
            *lock_unpoison(&run.pf_rules) = rules;
            return 0;
        }
        -1
    }
}

fn portfwd_states(run: &GenRun) -> Vec<super::tun_status::PfStateIn> {
    lock_unpoison(&run.pf_rules)
        .iter()
        .map(|r| super::tun_status::PfStateIn {
            listen: r.listen,
            target: if r.target_ip.is_empty() { format!(":{}", r.target_port) } else { format!("{}:{}", r.target_ip, r.target_port) },
            state: "listening".into(),
            err: String::new(),
            code: String::new(),
            conns: 0,
        })
        .collect()
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
    #[allow(dead_code)] // 桥拨号预算配置位（现行桥面用 bridge_host::DIAL_TIMEOUT 常量）
    dial_ms: i64,
    identity_dir: Option<PathBuf>,
    endpoint_cache_dir: Option<PathBuf>,
    port_forwards: Vec<PortForwardRule>,
}

impl GenRun {
    fn tun_ip_string(&self) -> String {
        crate::tunnel_addr::derive_tun_ip(
            &crate::token::Secret::from(self.secret),
            &self.identity.public_key(),
        )
        .to_string()
    }
}

/// 世代主线程体。`state` = TunnelExec 的世代登记槽（构建好 GenRun 后写回——
/// request_stop/recover/runner 经它取当前世代）。
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
    (logf)("传输：新栈（wg-native-stack）");

    // ---- 装配（identity/Client/缓存——Go startSession 段判据行同串）----
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
        shared.stage.set_if_current(gen, TunStage::Failed, "core", "token 里没有任何可用端点", false);
        shared.finish_generation(gen);
        return;
    }
    let (ident, src, warn) = match identity::load_or_create(cfg.identity_dir.as_deref(), &cfg.token.peer_id) {
        Ok(v) => v,
        Err(e) => {
            shared.stage.set_if_current(gen, TunStage::Failed, "core", &format!("身份装配失败：{e}"), false);
            shared.finish_generation(gen);
            return;
        }
    };
    log_identity(&logf, &ident, src, warn, &cfg.identity_dir);
    let client = match Client::start(CoreConfig {
        peer_id: cfg.token.peer_id,
        secret: cfg.token.secret,
        identity: ident.clone(),
        candidates: candidates.clone(),
        logf: Arc::clone(&logf),
    }) {
        Ok(c) => c,
        Err(e) => {
            shared
                .stage
                .set_if_current(gen, TunStage::Failed, "core", &format!("新栈启动失败：数据面装配失败：{e}"), false);
            shared.finish_generation(gen);
            return;
        }
    };
    let client = Arc::new(client);
    let tunnel_ip = crate::tunnel_addr::derive_tunnel_ip(&cfg.token.secret, &ident.public_key());
    (logf)(&format!("新栈会话已建立（token 端点 {} 个，后端隧道地址 {}）", cfg.token.endpoints.len(), tunnel_ip));

    let cache = cfg.endpoint_cache_dir.map(|dir| {
        let mut c = EndpointCache::open(&dir, cfg.token.peer_id);
        c.set_logger(Arc::clone(&logf));
        c
    });

    let (ev_tx, ev_rx) = mpsc::sync_channel::<GenEvent>(8);
    let run = Arc::new(GenRun {
        gen,
        stop: Arc::new(AtomicBool::new(false)),
        ev_tx,
        client: RwLock::new(Arc::clone(&client)),
        gate: RecoverGate::new(),
        cache: cache.map(Mutex::new),
        static_cands: candidates.clone(),
        secret: *cfg.token.secret.as_bytes(),
        peer_pub: *cfg.token.peer_id.as_bytes(),
        identity: ident,
        link: Mutex::new(None),
        bridge: Mutex::new(None),
        logf: Arc::clone(&logf),
        tun_shared: Arc::clone(&shared),
        demand: Arc::clone(&demand),
        pf_rules: Mutex::new(cfg.port_forwards.clone()),
        started: Instant::now(),
    });
    *lock_unpoison(&state) = Some(Arc::clone(&run));

    // 候选清单（标记·学习）
    {
        let merged = run.merged_candidates();
        let parts: Vec<String> = merged
            .iter()
            .map(|c| {
                let known = run.static_cands.iter().any(|s| s.addr == c.addr);
                let learned = if known { "" } else { "·学习" };
                let tag = if c.relay {
                    "中继"
                } else if is_lan_addr(c.addr) {
                    "LAN"
                } else {
                    "公网v4"
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

    // 世代退出收尾（一切路径经它——Go defer run.finish + closeClientOnce 的合成：
    // client 停 + finish_generation；guard 挂在函数栈上，return 即收）
    struct Finish {
        shared: Arc<TunShared>,
        gen: u64,
        client: Arc<Client>,
    }
    impl Drop for Finish {
        fn drop(&mut self) {
            self.client.stop();
            self.shared.finish_generation(self.gen);
        }
    }
    let _finish = Finish { shared: Arc::clone(&shared), gen, client: Arc::clone(&client) };

    // ---- 暖机（20s；软失败继续——判据 = 隧道内 RST 探测）----
    let warm_started = Instant::now();
    let meowed = match client.path_probe(WARM_TIMEOUT) {
        Ok(()) => {
            let rtt = warm_started.elapsed();
            let snap = client.snapshot();
            *lock_unpoison(&run.link) = Some(LinkIn {
                via: snap.via.as_str().to_owned(),
                ep: snap.ep.map(|a| a.to_string()).unwrap_or_default(),
                rtt_ms: rtt.as_millis() as i64,
                at_ms: now_unix_ms(),
            });
            shared.stage.set_ready_by("wg");
            (logf)("warmup pong: 就绪（判据=wg）");
            true
        }
        Err(ConnErr::Timeout) => {
            (logf)(&format!(
                "暖机 {} 内未收到 meowed：按软失败继续（attach 后由流量触发重试注册）",
                crate::go_fmt::fmt_duration_go_ms(WARM_TIMEOUT)
            ));
            false
        }
        Err(ConnErr::EngineGone) => {
            // stop 打断（request_stop 关了客户端）或引擎异常退出
            if run.stop.load(Ordering::Acquire) {
                shared.stage.set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
                (logf)("暖机期间收到停止信号，收工（不等 attach）");
            } else {
                shared.stage.set_if_current(gen, TunStage::Failed, "core", "暖机期间引擎异常退出", false);
            }
            return;
        }
        Err(e) => {
            (logf)(&format!("warmup ping: {e}"));
            shared
                .stage
                .set_if_current(gen, TunStage::Failed, "core", &format!("建立隧道会话失败：{e}"), false);
            return;
        }
    };
    if run.stop.load(Ordering::Acquire) {
        shared.stage.set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
        (logf)("暖机期间收到停止信号，收工（不等 attach）");
        return;
    }
    shared.stage.set_if_current(gen, TunStage::Ready, "", "", meowed);
    // running 行打真实派生地址（L3 直通后 TUN 实际地址）
    (logf)(&format!("running (mtu={} tunIp={})", cfg.mtu, run.tun_ip_string()));

    // ---- 等 attach（60s 死线；fd 通道 = TunShared）----
    let fd_rx = shared.attach_receiver();
    let deadline = Instant::now() + ATTACH_DEADLINE;
    let fd = loop {
        if run.stop.load(Ordering::Acquire) {
            shared.stage.set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
            (logf)("prepare 就绪后、attach 之前收到停止信号，收工");
            return;
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            // 防止单飞锁被「无人推进的世代」长期占住（design 决策 5）
            shared.stage.set_if_current(gen, TunStage::Idle, "attach-timeout", ATTACH_TIMEOUT_REASON, false);
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
                shared.stage.set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
                return;
            }
        }
    };

    // fd 所有权在扩展（坑 50）：attach 失败不 close，由扩展 destroy 回收。
    // fd 错误回调 → markUnhealthy("fd")（写序：先 why 后 healthy）。
    {
        let sh = Arc::clone(&shared);
        let raw = Arc::clone(&raw_logf);
        client.set_on_tun_error(Box::new(move |msg: &str| {
            raw(&format!("tier-core: {msg}"));
            sh.mark_unhealthy("fd");
        }));
    }
    if let Err(e) = client.attach_fd(fd, cfg.mtu) {
        if run.stop.load(Ordering::Acquire) {
            shared.stage.set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
            return;
        }
        shared
            .stage
            .set_if_current(gen, TunStage::Failed, "attach", &format!("attach 失败：{e}"), false);
        return;
    }
    shared.stage.set_if_current(gen, TunStage::Attached, "", "", meowed);
    (logf)(&format!("attached（数据面已接管 fd={fd}，L3 直通）"));

    // ---- 隧道桥（attached 后才起——「会话在桥在」）----
    let bridge = Arc::new(BridgeHost::new(
        "隧道桥",
        cfg.identity_dir.clone(),
        Arc::clone(&logf),
        {
            let run2 = Arc::clone(&run);
            Box::new(move |port, budget| session_connect(&run2, port, budget))
        },
    ));
    bridge.start();
    *lock_unpoison(&run.bridge) = Some(Arc::clone(&bridge));

    // ---- 巡检 / stats / demand pusher ----
    let patrol_handle = {
        let run2 = Arc::clone(&run);
        std::thread::Builder::new()
            .name("homeway-patrol".into())
            .spawn(move || patrol_loop(run2, ev_rx))
            .ok()
    };
    let pusher_handle = {
        let run2 = Arc::clone(&run);
        std::thread::Builder::new()
            .name("homeway-demand-push".into())
            .spawn(move || demand_pusher_loop(run2))
            .ok()
    };
    let stats_handle = {
        let run2 = Arc::clone(&run);
        std::thread::Builder::new()
            .name("homeway-stats".into())
            .spawn(move || stats_loop(run2, cfg.stats_secs, cfg.diag_fd_secs))
            .ok()
    };

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
    for h in [patrol_handle, pusher_handle, stats_handle].into_iter().flatten() {
        while !h.is_finished() && Instant::now() < join_deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if h.is_finished() {
            let _ = h.join();
        }
    }
    // 桥停（先关监听；在途桥接连接随 client 收工自然断——Go defer LIFO 同序）
    let bridge = lock_unpoison(&run.bridge).take();
    if let Some(b) = bridge {
        b.stop();
    }
    // _finish（Drop）：client.stop() + finish_generation
}

fn log_identity(
    logf: &Logf,
    ident: &Identity,
    src: identity::IdentitySource,
    warn: Option<String>,
    dir: &Option<PathBuf>,
) {
    use identity::IdentitySource;
    let dir_text = dir.as_ref().map(|d| d.display().to_string()).unwrap_or_default();
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
        (logf)(&format!("身份：不可持久化（{w}）—— 本次用临时身份建连；重连会换钥匙（出口会多占一条记录）"));
    }
}

// ---------------------------------------------------------------------------
// 巡检（隧道域：demand 门控 + 失败当拍 R1 + 3 连败 R2 + 不健康交扩展）
// ---------------------------------------------------------------------------

fn patrol_loop(run: Arc<GenRun>, ev_rx: mpsc::Receiver<GenEvent>) {
    let mut fail_streak: u32 = 0;
    let mut relay_streak: u32 = 0;
    let mut last_reg: Option<Instant> = None;
    let mut last_loop_at = Instant::now();
    let mut last_counted: Option<Instant> = None;
    let mut gated = false;
    let mut noise_since: Option<Instant> = None;
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
        // 挂起空窗检测（>2×间隔 = 进程被冻结过）：不等本拍探测失败，直接 R1 起跑
        let now = Instant::now();
        let gap = now.duration_since(last_loop_at);
        last_loop_at = now;
        if gap > 2 * PATROL_INTERVAL {
            (run.logf)(&format!(
                "巡检空窗 {}（判为进程被挂起）→ 阶梯恢复",
                crate::go_fmt::fmt_duration_go_secs(gap)
            ));
            let run2 = Arc::clone(&run);
            let _ = std::thread::spawn(move || {
                run2.recover(Level::R1, "挂起唤醒");
            });
        }
        // 拍头取走 App 出站计数（自上一拍以来的出站 = 本拍需求的 TUN 位）
        let out_pkts = run.current_client().swap_out_pkts();
        // 本拍探测（10s）
        let client = run.current_client();
        let started = Instant::now();
        let probe = client.path_probe(PROBE_TIMEOUT);
        let rtt = started.elapsed();
        // 旁路观测（endpoint-freshness：只写缓存与日志，不影响健康判定）
        {
            let run2 = Arc::clone(&run);
            let _ = std::thread::spawn(move || run_probe_candidates(&run2));
        }
        // 成功面：链路快照 + 落已验证 + 中继升直连条纹
        if let Ok(()) = probe {
            let snap = client.snapshot();
            if snap.via == Via::Direct {
                if let Some(ep) = snap.ep {
                    run.mark_round_trip(ep);
                }
            }
            *lock_unpoison(&run.link) = Some(LinkIn {
                via: snap.via.as_str().to_owned(),
                ep: snap.ep.map(|a| a.to_string()).unwrap_or_default(),
                rtt_ms: rtt.as_millis() as i64,
                at_ms: now_unix_ms(),
            });
            (run.logf)(&format!(
                "link: via={} ep={} rtt={}ms（新栈状态快照）",
                snap.via.as_str(),
                snap.ep.map(|a| a.to_string()).unwrap_or_default(),
                rtt.as_millis()
            ));
            relay_streak = relay_upgrade_streak(snap.via, relay_streak);
            if relay_upgrade_due(snap.via, relay_streak) {
                relay_streak = 0;
                (run.logf)(&format!(
                    "RELAY-UPGRADE：已在中继停留 {}，重新武装赛跑试直连（下一发出站包镜像到全部候选）",
                    crate::go_fmt::fmt_duration_go_secs(RELAY_UPGRADE_EVERY * PATROL_INTERVAL)
                ));
                let _ = client.rearm_soft();
                let merged = run.merged_candidates();
                client.set_candidates(merged);
                let ustarted = Instant::now();
                if client.path_probe(PROBE_TIMEOUT).is_ok() {
                    let st2 = client.snapshot();
                    if st2.via != snap.via {
                        (run.logf)(&format!(
                            "RELAY-UPGRADE：升级成功 → via={} ep={} rtt={}ms",
                            st2.via.as_str(),
                            st2.ep.map(|a| a.to_string()).unwrap_or_default(),
                            ustarted.elapsed().as_millis()
                        ));
                        *lock_unpoison(&run.link) = Some(LinkIn {
                            via: st2.via.as_str().to_owned(),
                            ep: st2.ep.map(|a| a.to_string()).unwrap_or_default(),
                            rtt_ms: ustarted.elapsed().as_millis() as i64,
                            at_ms: now_unix_ms(),
                        });
                    }
                }
            }
        }
        // 周期补注册（失败拍也发——注册走原始 UDP，不依赖 WG 会话与探测结果）
        if last_reg.is_none_or(|t| now.duration_since(t) >= REG_REFRESH_EVERY)
            && client.refresh_reg_result().unwrap_or(false)
        {
            last_reg = Some(now);
        }
        // ---- 失败证据的需求门控（demand-driven-recovery）----
        let (demand_active, demand_why) = run.demand.patrol_demand(out_pkts, Instant::now());
        run.demand.note_demand(demand_active, demand_why, now_unix_ms());
        // 环境噪声：采纳路径粘性信号（15s 尾窗）——全候选发送统计面 bind 未实现〔登记〕
        let local_noise = client
            .snapshot()
            .last_local_send_err
            .is_some_and(|t| t.elapsed() < NOISE_WINDOW);
        let (esc, ns) = noise_escalated(local_noise, noise_since, now);
        let local_noise = if esc {
            (run.logf)(&format!(
                "本地发送错误持续 {}（长停逃逸）：按质量失败计，进入正常升级链",
                crate::go_fmt::fmt_duration_go_secs(NOISE_ESCALATE_AFTER)
            ));
            noise_since = ns;
            false
        } else {
            noise_since = ns;
            local_noise
        };
        let probe_ok = probe.is_ok();
        let (new_streak, counted) =
            evidence_gate(local_noise, demand_active, fail_streak, last_counted, now, probe_ok);
        fail_streak = new_streak;
        if !probe_ok {
            if !counted {
                // 环境噪声（挂起禁发）/ 零流量需求期：该拍失败不构成路径质量证据
                if !gated {
                    gated = true;
                    let why = if local_noise {
                        "本地发送错误（环境噪声）"
                    } else {
                        demand_why
                    };
                    (run.logf)(&format!("巡检失败被门控拦下（{why}）→ 计数清零仅记录"));
                }
            } else {
                if gated {
                    gated = false;
                    (run.logf)(&format!("需求恢复（{demand_why}）：巡检失败重新计入证据"));
                }
                last_counted = Some(now);
                (run.logf)(&format!("对端巡检失败 {}/{}: {}", fail_streak, FAIL_STREAK_LADDER, probe.unwrap_err()));
                // 失败当拍进 R1（补注册 + 丢会话，保采纳）——出口重启/记录被回收时
                // 手机侧没有别的信号，原本要等 3 连败，现在本次巡检内就能恢复。
                let rc = run.recover(Level::R1, "巡检失败");
                if matches!(rc, LadderRc::Recovered(_)) {
                    (run.logf)("巡检失败后阶梯已恢复（不用等 3 连败）");
                    fail_streak = 0;
                    continue;
                }
            }
        } else if gated {
            // 成功拍：门控态结束的边沿（期间静默；计数已由纯函数清零）
            gated = false;
            (run.logf)("巡检恢复：门控态结束（成功拍清零）");
        }
        if fail_streak >= FAIL_STREAK_LADDER {
            (run.logf)("对端连续 3 次不可达，进恢复阶梯（R2 换源起跑，不拆隧道）");
            let rc = run.recover(Level::R2, "巡检3连败");
            if matches!(rc, LadderRc::Recovered(_)) {
                (run.logf)("阶梯恢复成功（巡检 3 连败后），对端恢复可达，继续巡检");
                fail_streak = 0;
                probe_now = true;
                continue;
            }
            (run.logf)("阶梯未恢复，标记隧道不健康（交给扩展重建）");
            run.tun_shared.mark_unhealthy("patrol");
            return;
        }
    }
}

/// 证据门（hostsession.PatrolEvidenceGate 六分支：成功拍清零）。
fn evidence_gate(
    local_noise: bool,
    demand: bool,
    fail_streak: u32,
    last_counted: Option<Instant>,
    now: Instant,
    probe_ok: bool,
) -> (u32, bool) {
    if probe_ok {
        return (0, false);
    }
    if local_noise || !demand {
        return (0, false);
    }
    let fail_streak =
        if fail_streak > 0 && last_counted.is_some_and(|t| now.duration_since(t) > PATROL_FAIL_WINDOW) {
            0
        } else {
            fail_streak
        };
    (fail_streak + 1, true)
}

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

fn relay_upgrade_streak(via: Via, streak: u32) -> u32 {
    if via == Via::Relay { streak + 1 } else { 0 }
}

fn relay_upgrade_due(via: Via, streak: u32) -> bool {
    via == Via::Relay && streak >= RELAY_UPGRADE_EVERY
}

/// 旁路探测候选（只打直连条目——探测中继端点拿到的是中继自己的列表，污染候选表）。
fn run_probe_candidates(run: &Arc<GenRun>) {
    let targets: Vec<SocketAddr> = run
        .merged_candidates()
        .into_iter()
        .filter(|c| !c.relay)
        .map(|c| c.addr)
        .collect();
    if targets.is_empty() {
        return;
    }
    let sh = Arc::clone(run);
    let logf = Arc::clone(&run.logf);
    let mut on_ep = move |ep: SocketAddr| {
        if let Some(c) = &sh.cache {
            lock_unpoison(c).observe(ep, EndpointSource::Probe, SystemTime::now());
        }
        let merged = sh.merged_candidates();
        sh.current_client().set_candidates(merged);
    };
    crate::probe::probe_candidates(&targets, PROBE_CANDIDATES_BUDGET, logf.as_ref(), &mut on_ep);
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
            std::thread::sleep(Duration::from_millis(100).min(next.saturating_duration_since(Instant::now())));
        }
        let client = run.current_client();
        let snap = client.snapshot();
        if snap.rx != last_rx {
            last_rx = snap.rx;
            recv_at = Some(Instant::now());
        }
        // 单调时基（评审 r2-H1：last_outbound_at 返回的 Instant 曾由 unix epoch ns
        // 换算——减出 56 年前的时刻 ⇒ should_push 恒 false、D4 整条静默失效。现改
        // TunCounters 的相对单调读数，unix ns 只留给 JSON 面）
        let out_at = client.last_outbound_at();
        let has_fresh_local_err = snap.last_local_send_err.is_some_and(|t| t.elapsed() < NOISE_WINDOW);
        let since = last_push.map(|t| t.elapsed());
        if demand::should_push(out_at, recv_at, Instant::now(), has_fresh_local_err, since) {
            last_push = Some(Instant::now());
            (run.logf)("待发包下推：App 出站新鲜且接收静默 → 立即下推阶梯（不等巡检拍）");
            let run2 = Arc::clone(&run);
            let _ = std::thread::spawn(move || {
                run2.recover(Level::R1, "待发包下推");
            });
        }
    }
}

// ---------------------------------------------------------------------------
// stats 线程（与设备 /proc/net/dev vpn-tun 行对表）
// ---------------------------------------------------------------------------

fn stats_loop(run: Arc<GenRun>, stats_secs: i64, diag_fd_secs: i64) {
    let tick = Duration::from_secs(stats_secs.max(STATS_SECS_MIN) as u64);
    let mut elapsed: i64 = 0;
    let mut base_done = false;
    let mut last_diag: i64 = 0;
    loop {
        // 分片等待（评审 r2-H3：整段 sleep(tick)〔默认 60s〕让收工 join 白烧满预算
        // ⇒ tun_stop 常态化 -2 强制放锁——Go 统计 goroutine 的 select{tick, stop} 同义）
        let deadline = Instant::now() + tick;
        while Instant::now() < deadline {
            if run.stop.load(Ordering::Acquire) {
                return;
            }
            let nap = Duration::from_millis(100).min(deadline.saturating_duration_since(Instant::now()));
            std::thread::sleep(nap);
        }
        elapsed += stats_secs.max(STATS_SECS_MIN);
        let (rd, wr) = run.current_client().tun_stats();
        (run.logf)(&format!("stats: fdReadBytes={rd}B fdWriteBytes={wr}B ｜ pf=0/0"));
        // 基线一行 + 可选周期快照（fd 快照面 OHOS 沙箱受限——不打，登记）
        if !base_done || (diag_fd_secs > 0 && elapsed - last_diag >= diag_fd_secs) {
            base_done = true;
            last_diag = elapsed;
        }
    }
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
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
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

fn is_lan_addr(ap: SocketAddr) -> bool {
    let ip = ap.ip();
    if ip.is_loopback() {
        return true;
    }
    if let std::net::IpAddr::V4(v4) = ip {
        let o = v4.octets();
        return o[0] == 10 || o[0] == 172 && (16..=31).contains(&o[1]) || o[0] == 192 && o[1] == 168;
    }
    false
}
