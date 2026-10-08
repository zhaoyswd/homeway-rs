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
//!   → portfwd 装表（Go 同序：桥之前）+ stale 复查
//!   → 起桥（三座）+ 巡检 + demand pusher + stats
//!   → 等 stop → 收工（pf 停 → 桥停 → client 停）→ finish_generation
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
use crate::wgcore::{Client, ConnErr, CoreConfig, CLIENT_CLOSE_BUDGET};
use crate::wtransport::endpoint_cache::{EndpointCache, EndpointSource};
use crate::wtransport::{Candidate, Via};
use crate::Logf;

use super::bridge_host::{BridgeHost, BridgeStream, WriteHalf};
use super::demand::{self, DemandSignals};
use super::portfwd::{PfLimits, PfRuntime, PfSetup, PortForwardRule};
use super::stage::TunStage;
use super::tun_shared::{lock_unpoison, TunShared};
use super::tun_status::{LinkIn, RunnerIn, TransportIn};
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
/// 旁路探测预算（endpoint-freshness；结果只进缓存与日志——Go 8s 同值，
/// 评审 r2-L5 对齐）。
const PROBE_CANDIDATES_BUDGET: Duration = Duration::from_secs(8);
/// mtu 缺省（Go Normalize：MTU ≤0 → 1280）。cfg.mtu 只进日志行（running /
/// wgcore 应用面就绪），数据面恒 1280——P2 升档门已随 v0.2.2 简洁化批删除
///（docs/BASELINE.md 偏离表的 clamp 条目同批撤销）。
const MTU_DEFAULT: i64 = 1280;
/// 桥拨号预算缺省（Go Normalize：DialMs ≤0 → 15000——评审 r2-L5；随 tunConfig
/// 的 dialMs 热传入 BridgeHost）。
const DIAL_MS_DEFAULT: i64 = 15000;

/// 退出路径 RPC 的预算（Q-F F3b/N1：close_write / SharedConn::drop 走在**必须退出**
/// 的收工链上——引擎卡死时无界等待会把桥泵与世代收工钉住。2s = 阶梯动作预算同刻度，
/// 正常引擎回执即时）。
const EXIT_RPC_BUDGET: Duration = Duration::from_secs(2);

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
        // Q-F F3b/N1：关流走在必须退出的收工链上（桥泵收口）——用有界面，引擎卡死
        // 时不让 Drop 挂死线程（代价：极端形态下引擎内槽位滞留，由引擎收工统一回收）
        let _ = self.client.close_bounded(self.id, EXIT_RPC_BUDGET);
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
    /// 背压语义（R8-8b 上行 bulk 断流根因修复）：栈内 tx 缓冲满时 `send_slice` 返回
    /// **Ok(0)**——io::Write 契约里 Ok(0) = 通道关（`write_all` 随即以 WriteZero 报错），
    /// 桥泵据此拆连接 ⇒ 上行 bulk 一进慢链路（缓冲被 cwnd 门限速填满）就整轮断流
    /// （真机实测：4 会话请求后 <1s 全 EOF；R7 E2E 的「上行帧中途断」同根因——
    /// host 形态 CLI 走 ClientConn 的零进展重试环，此桥路径裸露）。这里把 Ok(0)
    /// 展开成**有界等待重试**（Go net.Conn.Write 的阻塞语义）：重试节拍**分级退避**
    /// （F8a：前 50 拍 2ms ⇒ 其后 10ms ⇒ 100 拍后 20ms 封顶——停滞期不再固定
    /// ~500 次/s 的 `client.write` RPC 唤醒税），无进展上限 10s（远大于窗口时长，
    /// 防死锁兜底）。
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0); // 空写短路（评审 r1-补1：send_slice(&[]) 恒 Ok(0) 会被
                          // 背压环当「缓冲满」空转 10s——io::Write 约定空写返 Ok(0)）
        }
        let no_progress = Instant::now();
        let mut attempt: u32 = 0;
        // R8-3 F12：零接纳时引擎把原 Vec 带回（WriteOut.back）——背压重试环不再
        // 每拍重拷整段（此前 data.to_vec() 在循环内，2ms 节拍 × 整段 = 停滞期
        // 常驻拷贝面）。
        let mut pending: Option<Vec<u8>> = Some(data.to_vec());
        loop {
            let chunk = match pending.take() {
                Some(v) => v,
                None => data.to_vec(), // 防御面：回执未带（不发生——引擎零接纳必带）
            };
            match self.shared.client.write(self.shared.id, chunk) {
                Ok(w) if w.n == 0 => {
                    pending = w.back;
                    if no_progress.elapsed() > Duration::from_secs(10) {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "写通道长时间无进展（栈内发送缓冲不排空）",
                        ));
                    }
                    std::thread::sleep(write_retry_backoff(attempt));
                    attempt = attempt.saturating_add(1);
                }
                Ok(w) => return Ok(w.n),
                Err(e) => return Err(io::Error::other(e.to_string())),
            }
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 背压重试节拍（F8a 纯函数）：前 50 拍 2ms、51–100 拍 10ms、其后 20ms 封顶。
/// 10s 无进展上界与「空写短路」语义不随本表变化。
fn write_retry_backoff(attempt: u32) -> Duration {
    if attempt < 50 {
        Duration::from_millis(2)
    } else if attempt < 100 {
        Duration::from_millis(10)
    } else {
        Duration::from_millis(20)
    }
}

impl WriteHalf for SessionWriteHalf {
    fn close_write(&mut self) {
        // Q-F F3b/N1：半关走在桥泵收口路径上——有界（同 EXIT_RPC_BUDGET）
        let _ = self.shared.client.shutdown_bounded(self.shared.id, EXIT_RPC_BUDGET);
    }
}

/// 恢复感知的会话流拨号（Go DialTCPPort 的等价面：healing_dial_port——首段短试 →
/// 阶梯 R2 → 余预算重试）。隧道域**桥**消费（pf 走 [`session_connect_target`] 裸拨
/// ——D11：桥的首试失败要触发恢复，pf 的常态拒绝不许）。
pub fn session_connect(
    run: &GenRun,
    port: u16,
    budget: Duration,
) -> io::Result<Box<dyn BridgeStream>> {
    let dst = SocketAddrV4::new(crate::wgcore::SERVER_TUNNEL_IP, port);
    let gen_client = run.current_client();
    // 装配期拨号（世代在世但 Client 未起——理论窗口；桥 attached 后才有，防御性收口）
    let gen_client = gen_client
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "世代装配中（数据面未就绪）"))?;
    let id = healing_dial(&gen_client, run, dst, budget)?;
    Ok(Box::new(SessionStream::shared(gen_client, id)))
}

/// **裸拨**（Go `DialTCPPort`/`DialTCP` 的等价面 = `Client::connect_deadline` 直接建连）。
///
/// Q-F-B F2-1/D11：portfwd 的入站连接**不复用** `healing_dial`——「目标拒绝」是端口
/// 转发的常态（浏览器探测、目标服务没起），复用会把常态失败升级成 R2 恢复动作并刷
/// 未节流的 RECOVER 行。恢复能力不因此丢失：`patrol` 派生线程独立驱动恢复（设计门
/// 第二轮已核）。
pub fn session_connect_target(
    run: &GenRun,
    dst: SocketAddrV4,
    budget: Duration,
) -> io::Result<Box<dyn BridgeStream>> {
    let gen_client = run.current_client();
    // 装配期拨号（同上——pf 的 accept 线程只活在 attached 后，防御性收口）
    let gen_client = gen_client
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "世代装配中（数据面未就绪）"))?;
    let id = gen_client.connect_deadline(dst, budget).map_err(conn_err_to_io)?;
    Ok(Box::new(SessionStream::shared(gen_client, id)))
}

/// portfwd 拨号缝的生产实现（`PfRuntime` 全注入面——闭包持 `Weak<GenRun>` 防 Arc 环：
/// 世代收工后 `upgrade()` 失败 = 拨号失败如实收口（conn 线程记 fails + RST + 行））。
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
    session_connect_target(&r, dst, budget)
}

/// `ConnErr` → `io::Error` 的类型归因（评审 r2-M9：桥宿主的「出口活着、端口没服务」
/// 判定只认 kind，不做字符串嗅探）。
fn conn_err_to_io(e: ConnErr) -> io::Error {
    match e {
        ConnErr::Refused => io::Error::new(io::ErrorKind::ConnectionRefused, e.to_string()),
        ConnErr::Timeout => io::Error::new(io::ErrorKind::TimedOut, e.to_string()),
        other => io::Error::other(other.to_string()),
    }
}

/// 会话流整体（实现 BridgeStream：拆两半给泵）。
pub(crate) struct SessionStream {
    shared: Arc<SharedConn>,
}

impl SessionStream {
    /// 从会话句柄 + 流 id 构造（服务桥消费——dial_via_run）。
    pub(crate) fn shared(client: Arc<Client>, id: u64) -> Self {
        SessionStream {
            shared: Arc::new(SharedConn { client, id }),
        }
    }
}

impl BridgeStream for SessionStream {
    fn into_halves(
        self: Box<Self>,
    ) -> io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)> {
        Ok((
            Box::new(SessionReadHalf {
                shared: Arc::clone(&self.shared),
                buf: Vec::new(),
                off: 0,
            }),
            Box::new(SessionWriteHalf {
                shared: Arc::clone(&self.shared),
            }),
        ))
    }
}

/// 恢复感知拨号（Session::healing_dial 的隧道域内联版：4s 首试 → 阶梯 R2 → 余预算）。
/// 错误归因类型化（评审 r2-M9）：Refused → io ErrorKind::ConnectionRefused——桥宿主
/// 的「出口活着、端口没服务」判定只认 kind，不再字符串嗅探。
///
/// Q-F F3（设计门 D4/D9/C4）：**阶梯与阶梯等待计入调用方同一预算**——`deadline`
/// 透给恢复闸（等待到点 ⇒ `(Deadline,false)`，不发布不清闸）；首试与尾试各按剩余
/// 夹取。残余越界上界 = 一个动作预算（`recover::ACTION`=2s，动作不可中断）。
fn healing_dial(
    client: &Arc<Client>,
    run: &GenRun,
    dst: SocketAddrV4,
    budget: Duration,
) -> io::Result<u64> {
    dial_with_recover(client, dst, budget, FIRST_TRY, |lvl, deadline, cause| {
        run.recover_until(lvl, cause, deadline)
    })
}

/// 阶梯首试预算（隧道域；服务域用 `session::DIAL_FIRST_TRY` 同值）。
const FIRST_TRY: Duration = Duration::from_secs(4);

/// 恢复感知拨号的可测主体（两域共用形；单测注入「阻塞到期限」的桩闭包 ⇒ 断言
/// 预算内返回——预算须 > `first_try` 且断言桩被调用过，否则假绿）。
fn dial_with_recover<F>(
    client: &Arc<Client>,
    dst: SocketAddrV4,
    budget: Duration,
    first_try: Duration,
    recover: F,
) -> io::Result<u64>
where
    F: FnOnce(Level, Option<Instant>, &str) -> (LadderRc, bool),
{
    let t0 = Instant::now();
    let deadline = t0 + budget;
    match client.connect_deadline(dst, first_try.min(budget)) {
        Ok(id) => return Ok(id),
        Err(e) => {
            if t0.elapsed() >= budget {
                return Err(io::Error::new(io::ErrorKind::TimedOut, e.to_string()));
            }
        }
    }
    // 拨号失败 → 阶梯（世代还在就恢复它；引擎收工 = 如实报错）；期限随调用方预算
    let (rc, _waited) = recover(Level::R2, Some(deadline), "拨号失败");
    match rc {
        LadderRc::Recovered(_) => {
            let remain = budget
                .saturating_sub(t0.elapsed())
                .max(Duration::from_millis(1));
            client.connect_deadline(dst, remain).map_err(conn_err_to_io)
        }
        LadderRc::Deadline => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "拨号失败且恢复预算耗尽（放弃等待）",
        )),
        other => Err(io::Error::new(
            io::ErrorKind::NotConnected,
            format!("拨号失败且阶梯未恢复（rc={:?}）", other.as_rc()),
        )),
    }
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
    /// 当前数据面（rebuild 换代；拨号/恢复自动落新世代）。**装配期后填**
    /// （评审 r2-M2：GenRun 在 identity/Client::start 之前就建好登记——窗口内
    /// request_stop/recover 打得到世代句柄，Go beginTunRun 时序同构）。
    client: RwLock<Option<Arc<Client>>>,
    /// 恢复闸（按世代隔离）。
    gate: RecoverGate,
    /// 端点学习缓存（None = 不落盘不学）。
    cache: Option<Mutex<EndpointCache>>,
    static_cands: Vec<Candidate>,
    /// 域名条目候选（最近一次解析产物；静态面 = static_cands + 本组——P0-4）。
    domain_cands: Mutex<Vec<Candidate>>,
    /// 域名重解析编排（无域名条目 = None；Rearm/RearmSoft/旁路探测拍触发）。
    domain_refresher: RwLock<Option<Arc<crate::wtransport::domain_eps::DomainRefresher>>>,
    /// 重建材料（Token 的身份面；候选 = static_cands 另存）。Secret 非 Copy
    /// （F8d）——持有者唯一、Drop 擦除。
    secret: crate::token::Secret,
    #[allow(dead_code)]
    // 重建材料位（对齐 Go tunRun 的会话材料；隧道域不 rebuild——阶梯失败交扩展）
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
    /// portfwd 运行时（真监听器：整表热替换 / 世代收工 / 真计数——Q-F-B F1）。
    /// 拨号缝全注入（闭包持 `Weak<GenRun>`——无 Arc 环）、停止位 = 本世代 `stop` 克隆。
    pub pf: PfRuntime,
    /// 世代时间起点（elapsed 面板用）。
    pub started: Instant,
    /// 缓存落盘信号（hint/探测观察到新端点时投；save 线程去抖消费——我-2③）。
    save_tx: Option<mpsc::SyncSender<()>>,
    /// hint 打洞节流（Go punchTo 的 5s 节流位）。
    last_punch: Mutex<Option<Instant>>,
}

impl GenRun {
    /// 当前数据面（装配期 = None——评审 r2-M2 的窗口语义；拨号/巡检等
    /// 后装配路径不会被踩到，调用方按各自语义处理 None）。
    pub fn current_client(&self) -> Option<Arc<Client>> {
        self.client
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// 数据面回填（Client::start 成功后；一次性）。
    fn set_client(&self, c: Arc<Client>) {
        *self.client.write().unwrap_or_else(|e| e.into_inner()) = Some(c);
    }

    /// 恢复入口（gate 单飞；隧道域 rc 契约）。
    pub fn recover(&self, from: Level, cause: &str) -> LadderRc {
        self.recover_until(from, cause, None).0
    }

    /// 带期限的恢复入口（Q-F F3-1/F3-2）：`deadline` 透到阶梯本体（每档起点/动作前
    /// 检查）与闸等待（等待方到点返回 `(Deadline,false)`，在途轮照常跑完）。
    /// 第二返回值 = 是否等到了结果（执行方恒 true）。
    pub fn recover_until(
        &self,
        from: Level,
        cause: &str,
        deadline: Option<Instant>,
    ) -> (LadderRc, bool) {
        let sh = self;
        sh.gate
            .merge_until(from, deadline, |lvl, d| run_round(sh, lvl, cause, d))
    }

    fn merged_candidates(&self) -> Vec<Candidate> {
        let domain = lock_unpoison(&self.domain_cands).clone();
        let mut base = self.static_cands.clone();
        base.extend(domain);
        match &self.cache {
            Some(c) => {
                let c = lock_unpoison(c);
                c.merge(&base, SystemTime::now())
            }
            None => base,
        }
    }

    /// 域名重解析触发（Rearm/RearmSoft 面——无域名 = no-op）。
    fn domain_refresh_async(&self) {
        if let Some(rf) = self
            .domain_refresher
            .read()
            .map(|g| g.clone())
            .unwrap_or(None)
        {
            rf.refresh_async();
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

    /// 请求缓存落盘（去抖窗由 save 线程合并——我-2③；无缓存/无线程时静默）。
    fn schedule_save(&self) {
        if let Some(tx) = &self.save_tx {
            let _ = tx.try_send(());
        }
    }
}

/// 阶梯一轮（EngineTransport 的隧道域版——Client 动作面 + 候选重投）。
fn run_round(run: &GenRun, from: Level, cause: &str, deadline: Option<Instant>) -> LadderRc {
    let Some(client) = run.current_client() else {
        // 装配期窗口（评审 r2-M2）：本地动作面不在——按本地动作失败收轮（-4），
        // 不让恢复闸挂死
        (run.logf)(&format!(
            "RECOVER {cause}：世代装配中（数据面未起），本地动作失败收轮"
        ));
        return LadderRc::ActionFailed("世代装配中".into());
    };
    let mut tr = TunnelTransport {
        run,
        client: &client,
    };
    let mut probe = |d: Duration| client.path_probe(d).is_ok();
    let mut deps = recover::LadderDeps {
        probe: &mut probe,
        tr: &mut tr,
        logf: &|s: &str| (run.logf)(s),
        pre_probe: recover::PRE_PROBE,
        verify: recover::VERIFY,
        deadline,
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
            Action::Rebind => self
                .client
                .rebind_bounded(recover::ACTION)
                .map_err(action_err),
            Action::Rearm => {
                // R3 = 清采纳重赛跑 + 候选重投（Go Transport.Rearm 复合）+ 域名重解析
                // 并发另跑（P0-4：动作本体零 DNS 等待）。
                self.client
                    .rearm_bounded(recover::ACTION)
                    .map_err(action_err)?;
                let cands = self.run.merged_candidates();
                self.client.set_candidates(cands);
                self.run.domain_refresh_async();
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
            endpoint_cache_dir: (!cfg.endpoint_cache_dir.is_empty())
                .then(|| PathBuf::from(&cfg.endpoint_cache_dir)),
            port_forwards: cfg.port_forwards.clone(),
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
                // preparing 阶段没有 fd 循环可打断，暖机探测可能挂在引擎 RPC 上——
                // 关客户端是最可靠的第二条打断路径（Go tunStopWait 同义）。
                if run.tun_shared.stage.snapshot().stage == TunStage::Preparing {
                    if let Some(c) = run.current_client() {
                        // Q-G F5：**有界**收工（暖机探测可能挂在引擎 RPC 上——到点
                        // detach，wake fd 交收割线程；无界会拖死 tun_stop 的等待）
                        if !c.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
                            (run.logf)("等待 client 线程收工超时（CLIENT_CLOSE_BUDGET）——放行自退（引擎线程由收割线程收口）");
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

    fn recover(&self, from: i64, cause: &str) -> i32 {
        let Some(run) = self.gen_run() else {
            return -2; // 没有 attached 隧道（不构成网络结论）
        };
        // 陈旧世代 → -2（复核 r3-F12：Go recoverTunnelReady 在该窗口明确 -2 并警告
        // 非 -2 会让扩展做无意义恢复、甚至触发整套重建）
        if run.gen != run.tun_shared.gen.load(Ordering::Acquire) {
            return -2;
        }
        let lvl = Level::clamp(from);
        run.recover(lvl, cause).as_rc()
    }

    fn runner(&self) -> Option<RunnerIn> {
        Some(runner_of(self.gen_run()?.as_ref()))
    }

    fn transport(&self) -> Option<TransportIn> {
        Some(transport_of(self.gen_run()?.as_ref()))
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

/// 【test-seams】最小世代（Q-F F1 状态面集成 + 热替换 rc 断言用）：真惰性 Client +
/// 空桥/空缓存/空域名面 + portfwd 运行时（拨号桩恒失败——本缝不建隧道）。
/// `tun_shared.gen` 与 `run.gen` 对齐（否则 stale 复查恒 `-1`，正例不可测）。
#[cfg(test)]
impl GenRun {
    pub(crate) fn synthetic_for_test(logf: Logf) -> GenRun {
        let ident = crate::identity::Identity::ephemeral().expect("临时身份");
        let client = Client::start(CoreConfig {
            peer_id: crate::token::PeerId::from([1u8; 32]),
            secret: crate::token::Secret::from([2u8; 32]),
            identity: ident.clone(),
            candidates: vec![Candidate {
                addr: "203.0.113.1:41641".parse().unwrap(),
                relay: false,
            }],
            logf: Arc::clone(&logf),
        })
        .expect("惰性客户端可起");
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
            client: RwLock::new(Some(Arc::new(client))),
            gate: RecoverGate::new(),
            cache: None,
            static_cands: vec![],
            domain_cands: Mutex::new(vec![]),
            domain_refresher: RwLock::new(None),
            secret: crate::token::Secret::from([2u8; 32]),
            peer_pub: [3u8; 32],
            identity: ident,
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
            save_tx: None,
            last_punch: Mutex::new(None),
        }
    }
}

/// runner 块的组装（TunnelExec::runner 与对账快照共用——评审 r2-M11 的完全对齐：
/// runner 只要求**世代在场**〔Go setRunner 在 startSession 后即设〕，不要求 link
/// 已写过；link 缺席兜 via="none"）。
pub(crate) fn runner_of(run: &GenRun) -> RunnerIn {
    let (rd, wr) = run
        .current_client()
        .map(|c| c.tun_stats())
        .unwrap_or((0, 0));
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
        exit_ip: crate::wgcore::SERVER_TUNNEL_IP.to_string(),
        link,
        port_forwards: run.pf.snapshot_states(),
        bridge,
    }
}

/// transport 块的组装（同 runner_of 的共享件）。
pub(crate) fn transport_of(run: &GenRun) -> TransportIn {
    let client = run.current_client();
    TransportIn {
        identity: Some((run.identity.short_dev(), run.identity.short_pub())),
        tun_ip: Some(run.tun_ip_string()),
        outbound_at_ms: client
            .as_ref()
            .map(|c| c.last_outbound_unix_ms())
            .filter(|v| *v != 0),
        // bind 全候选发送统计面（拍板①：采纳/累计本地错误数——tunStatusJSON 的
        // demand.localErr* 两键源；装配期 = None）
        local_err: client.as_ref().map(|c| c.local_err_counters()),
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
    endpoint_cache_dir: Option<PathBuf>,
    port_forwards: Vec<PortForwardRule>,
}

impl GenRun {
    fn tun_ip_string(&self) -> String {
        crate::tunnel_addr::derive_tun_ip(&self.secret, &self.identity.public_key()).to_string()
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
    (logf)("传输：新栈（wg-native-stack）");

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
    // P0-4：域名端点展开（IP 字面量直入 + 域名建会话解析一次 + 原文留给重解析）。
    let ep_refs: Vec<crate::token::EndpointRef> = cfg
        .token
        .endpoints
        .iter()
        .map(|e| crate::token::EndpointRef::new(e.addr.as_str(), e.kind))
        .collect();
    let inputs = crate::wtransport::domain_eps::split_and_resolve(&ep_refs, &logf);
    let candidates: Vec<Candidate> = inputs.candidates;
    let domain_eps = inputs.domains;
    // 静态基座 = 仅 IP 字面量（评审 4.3：域名首解析产物进 domain_cands 组——
    // 否则旧解析地址永不退场、且与 domain 组在 merged_candidates 里重复）。
    let inputs_static_base = inputs.static_base;
    let domain_initial = inputs.domain_initial;
    if candidates.is_empty() {
        shared.stage.set_if_current(
            gen,
            TunStage::Failed,
            "core",
            "token 里没有任何可用端点",
            false,
        );
        return;
    }
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
    // 缓存先开（Client 起来前的候选合并要用——我-2①）
    let cache = cfg.endpoint_cache_dir.map(|dir| {
        let mut c = EndpointCache::open(&dir, cfg.token.peer_id);
        c.set_logger(Arc::clone(&logf));
        c
    });
    let (ev_tx, ev_rx) = mpsc::sync_channel::<GenEvent>(8);
    let (hint_tx, hint_rx) = mpsc::sync_channel::<SocketAddr>(8);
    let (save_tx, save_rx) = mpsc::sync_channel::<()>(1);
    // portfwd 运行时先建材料（拨号闭包经 `Arc::new_cyclic` 拿 `Weak<GenRun>`——
    // 运行时结构上不依赖 GenRun，但生产实现要回指它；Weak 破环 = 世代可回收，D7）。
    let pf_budget = Duration::from_millis(cfg.dial_ms.max(1) as u64);
    let run = Arc::new_cyclic(|me: &std::sync::Weak<GenRun>| {
        let w = std::sync::Weak::clone(me);
        GenRun {
            gen,
            stop: Arc::clone(&stop),
            ev_tx,
            client: RwLock::new(None),
            gate: RecoverGate::new(),
            cache: cache.map(Mutex::new),
            static_cands: inputs_static_base,
            secret: cfg.token.secret.clone(),
            peer_pub: *cfg.token.peer_id.as_bytes(),
            identity: ident.clone(),
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
            save_tx: Some(save_tx),
            last_punch: Mutex::new(None),
            domain_cands: Mutex::new(domain_initial.clone()),
            domain_refresher: RwLock::new(None),
        }
    });
    // P0-4：域名重解析编排装配（回调持 Weak 回指 GenRun）。
    if !domain_eps.is_empty() {
        let w1 = Arc::downgrade(&run);
        let w2 = Arc::downgrade(&run);
        let w3 = Arc::downgrade(&run);
        let rf = Arc::new(crate::wtransport::domain_eps::DomainRefresher::new(
            domain_eps,
            domain_initial,
            Arc::clone(&logf),
            Arc::new(move |fresh: &[Candidate]| {
                if let Some(r) = w1.upgrade() {
                    *lock_unpoison(&r.domain_cands) = fresh.to_vec();
                    if let Some(c) = r.current_client() {
                        let merged = r.merged_candidates();
                        c.set_candidates(merged);
                    }
                }
            }),
            Arc::new(move || {
                let r = w2.upgrade()?;
                let c = r.current_client()?;
                let snap = c.snapshot();
                if snap.via == Via::Relay { snap.ep } else { None }
            }),
            Arc::new(move || {
                if let Some(r) = w3.upgrade() {
                    if let Some(c) = r.current_client() {
                        let _ = c.rearm_soft_bounded(recover::ACTION);
                        if let Some(ec) = &r.cache {
                            lock_unpoison(ec).note_rearm();
                        }
                        let merged = r.merged_candidates();
                        c.set_candidates(merged);
                    }
                }
            }),
        ));
        *run
            .domain_refresher
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(rf);
    }
    // ---- 世代句柄登记（Client::start **之前**——评审 r2-M2：窗口内 request_stop/
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
            if let Some(c) = self
                .run
                .client
                .write()
                .unwrap_or_else(|e| e.into_inner())
                .take()
            {
                // Q-G F5：有界收工（Drop 里 spawn 收割线程允许——Q-F 段⑤先例；
                // 到点 detach，引擎线程自行退出）
                if !c.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
                    (self.run.logf)("等待 client 线程收工超时（CLIENT_CLOSE_BUDGET）——放行自退（引擎线程由收割线程收口）");
                }
            }
            // 缓存终写（我-2③：世代收尾前把学到/验证过的端点落盘——Go closeClientOnce
            // 路径的 save 同义；进程被杀时由去抖线程的先前行兜底）
            if let Some(c) = self.run.cache.as_ref() {
                let _ = lock_unpoison(c).save(SystemTime::now());
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

    let client = match Client::start(CoreConfig {
        peer_id: cfg.token.peer_id,
        // Secret 非 Copy（F8d）：cfg.token 在下方 derive_tunnel_ip 仍要借 secret
        secret: cfg.token.secret.clone(),
        identity: ident.clone(),
        candidates: candidates.clone(),
        logf: Arc::clone(&logf),
    }) {
        Ok(c) => c,
        Err(e) => {
            shared.stage.set_if_current(
                gen,
                TunStage::Failed,
                "core",
                &format!("新栈启动失败：数据面装配失败：{e}"),
                false,
            );
            return;
        }
    };
    let client = Arc::new(client);
    // 装配窗口内收到 stop（评审 r2-M2 的窗口收口）：停在装配完成点，不进暖机
    if run.stop.load(Ordering::Acquire) {
        // Q-G F5：有界收工（本处在世代线程内联执行——预算语义 = 「本线程不无限等」，
        // 与 `session::rebuild_session` 的两处一致）
        if !client.stop_within(Instant::now() + CLIENT_CLOSE_BUDGET) {
            (logf)("等待 client 线程收工超时（CLIENT_CLOSE_BUDGET）——放行自退（引擎线程由收割线程收口）");
        }
        shared
            .stage
            .set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
        (logf)("装配期间收到停止信号，收工（不进暖机）");
        return;
    }
    run.set_client(Arc::clone(&client));
    let tunnel_ip = crate::tunnel_addr::derive_tunnel_ip(&cfg.token.secret, &ident.public_key());
    (logf)(&format!(
        "新栈会话已建立（token 端点 {} 个，后端隧道地址 {}）",
        cfg.token.endpoints.len(),
        tunnel_ip
    ));

    // ---- 端点学习缓存接线（我-2 三缺的修复面）----
    // ① 首次候选合并喂给 bind（学习缓存里的历史端点进赛跑集——Go Transport 构造
    //    里的 bind.SetCandidates(Merge(static))）
    client.set_candidates(run.merged_candidates());
    // ② hint 链路（中继观察的对端地址线索 → 缓存 + 候选重投 + 打洞 + 落盘信号）
    // ③ 落盘去抖线程（信号合并 + 1s 去抖窗；终写在上面的 Finish guard）
    if run.cache.is_some() {
        install_tunnel_hint(&client, hint_tx);
        spawn_tunnel_hint_handler(Arc::clone(&run), hint_rx);
        spawn_tunnel_save_loop(Arc::clone(&run), save_rx);
    } else {
        drop(hint_rx);
        drop(save_rx);
    }

    // 候选清单（标记·学习）
    {
        let merged = run.merged_candidates();
        let parts: Vec<String> = merged
            .iter()
            .map(|c| {
                let known = run.static_cands.iter().any(|s| s.addr == c.addr);
                let learned = if known { "" } else { "·学习" };
                let tag = crate::wtransport::bind::candidate_tag(c.addr, c.relay);
                format!("{}（{tag}{learned}）", c.addr)
            })
            .collect();
        (logf)(&format!(
            "候选端点（{} 条，标记·学习=来自巡检缓存/中继 hint）：{}",
            merged.len(),
            parts.join("、")
        ));
    }

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
                shared.stage.set_if_current(
                    gen,
                    TunStage::Idle,
                    "stopped",
                    "被停止请求中断",
                    false,
                );
                (logf)("暖机期间收到停止信号，收工（不等 attach）");
            } else {
                shared.stage.set_if_current(
                    gen,
                    TunStage::Failed,
                    "core",
                    "暖机期间引擎异常退出",
                    false,
                );
            }
            return;
        }
        Err(e) => {
            (logf)(&format!("warmup ping: {e}"));
            shared.stage.set_if_current(
                gen,
                TunStage::Failed,
                "core",
                &format!("建立隧道会话失败：{e}"),
                false,
            );
            return;
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

    // fd 所有权在扩展（坑 50）：attach 失败不 close，由扩展 destroy 回收。
    // fd 错误回调 → markUnhealthy_if_current("fd")（写序：先 why 后 healthy；世代
    // 守卫在 TunShared 内——评审 r2-M1：旧世代 fd 被 destroy 后报错是必然事件）。
    {
        let sh = Arc::clone(&shared);
        let raw = Arc::clone(&raw_logf);
        client.set_on_tun_error(Box::new(move |msg: &str| {
            raw(&format!("tier-core: {msg}"));
            sh.mark_unhealthy_if_current(gen, "fd");
        }));
    }
    if let Err(e) = client.attach_fd(fd, cfg.mtu) {
        if run.stop.load(Ordering::Acquire) {
            shared
                .stage
                .set_if_current(gen, TunStage::Idle, "stopped", "被停止请求中断", false);
            return;
        }
        shared.stage.set_if_current(
            gen,
            TunStage::Failed,
            "attach",
            &format!("attach 失败：{e}"),
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
    let bridge = Arc::new(BridgeHost::new(
        "隧道桥",
        cfg.identity_dir.clone(),
        Arc::clone(&logf),
        {
            let run2 = Arc::clone(&run);
            Arc::new(move |port, budget| session_connect(&run2, port, budget))
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
    // _finish（Drop）：client.stop() + 缓存终写 + finish_generation
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

// ---------------------------------------------------------------------------
// hint 链路 + 缓存落盘（我-2：隧道域端点学习缓存三缺的修复件——服务域 session/mod.rs
// 的 spawn_hint_handler/spawn_save_loop 的隧道域对应物）
// ---------------------------------------------------------------------------

/// hint 回调安装（回调纪律：驱动线程内只投通道——解析失败静默丢）。
fn install_tunnel_hint(client: &Arc<Client>, hint_tx: mpsc::SyncSender<SocketAddr>) {
    client.set_on_hint(Arc::new(move |addr: &str| {
        if let Ok(ap) = addr.parse::<SocketAddr>() {
            let _ = hint_tx.try_send(ap);
        }
    }));
}

/// hint 处理线程（Go Transport 的 hint 链路：观察 → 候选重投 → 落盘信号 → 打洞）。
fn spawn_tunnel_hint_handler(run: Arc<GenRun>, rx: mpsc::Receiver<SocketAddr>) {
    let logf = Arc::clone(&run.logf);
    let spawned = std::thread::Builder::new()
        .name("homeway-tun-hint".into())
        .spawn(move || {
            while !run.stop.load(Ordering::Acquire) {
                let Ok(addr) = rx.recv_timeout(Duration::from_millis(500)) else {
                    continue;
                };
                // 缓存观察（最新鲜的不可信线索）
                if let Some(c) = run.cache.as_ref() {
                    lock_unpoison(c).observe(addr, EndpointSource::Hint, SystemTime::now());
                }
                // 候选重投（学习到的地址进赛跑集）
                let merged = run.merged_candidates();
                if let Some(cl) = run.current_client() {
                    cl.set_candidates(merged);
                }
                run.schedule_save();
                tunnel_punch_to(&run, addr);
            }
        });
    if let Err(e) = spawned {
        crate::syncutil::log_spawn_failed(
            &logf,
            "homeway-tun-hint",
            &e,
            "本世代不做 hint 打洞（候选仍按巡检刷新）",
        );
    }
}

/// 收到对端地址线索后打一发「握手兼打洞」（Go punchTo：节流 5s + RearmSoft + 拨 :1）。
fn tunnel_punch_to(run: &Arc<GenRun>, addr: SocketAddr) {
    // F6-5：停机中不发起新探测（在途成本收窄——path_probe 5s 预算不改）
    if run.stop.load(Ordering::Acquire) {
        return;
    }
    {
        let mut lp = lock_unpoison(&run.last_punch);
        if lp.is_some_and(|t| t.elapsed() < Duration::from_secs(5)) {
            return;
        }
        *lp = Some(Instant::now());
    }
    let Some(client) = run.current_client() else {
        return;
    };
    // F3b/N1：hint 线程必须能退出——无界 RPC 改有界（recover::ACTION 同刻度）
    let _ = client.rearm_soft_bounded(recover::ACTION);
    if let Some(c) = &run.cache {
        lock_unpoison(c).note_rearm();
    }
    run.domain_refresh_async();
    (run.logf)(&format!(
        "中继 hint {addr} → 重新武装候选赛跑，打一发握手兼打洞"
    ));
    // 打洞探测（5s 预算；refused = 路径通——RST 说明握手与路径都通了）
    match client.path_probe(Duration::from_secs(5)) {
        Ok(()) => {
            (run.logf)("打洞后探测成功：会话可能已漂移到直连（看 link 行确认）");
            let snap = client.snapshot();
            if snap.via == Via::Direct {
                if let Some(ep) = snap.ep {
                    run.mark_round_trip(ep);
                }
            }
        }
        Err(e) => {
            (run.logf)(&format!(
                "打洞后探测未成功（{e}）—— 继续停留在原路径（中继/旧直连）"
            ));
        }
    }
}

/// 缓存落盘去抖（Go FIX-16：信号合并 + 1s 去抖窗；收口终写在 Finish guard）。
fn spawn_tunnel_save_loop(run: Arc<GenRun>, rx: mpsc::Receiver<()>) {
    let logf = Arc::clone(&run.logf);
    let spawned = std::thread::Builder::new()
        .name("homeway-tun-cache-save".into())
        .spawn(move || {
            loop {
                if run.stop.load(Ordering::Acquire) {
                    return;
                }
                let Ok(()) = rx.recv_timeout(Duration::from_millis(500)) else {
                    continue;
                };
                std::thread::sleep(Duration::from_secs(1)); // 去抖窗（窗内信号合并）
                while rx.try_recv().is_ok() {}
                if let Some(c) = run.cache.as_ref() {
                    if let Err(e) = lock_unpoison(c).save(SystemTime::now()) {
                        (run.logf)(&format!("端点缓存落盘失败：{e}"));
                    }
                }
            }
        });
    if let Err(e) = spawned {
        crate::syncutil::log_spawn_failed(
            &logf,
            "homeway-tun-cache-save",
            &e,
            "端点缓存只靠世代收工终写（无去抖落盘）",
        );
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
        // 拍头取走 App 出站计数（自上一拍以来的出站 = 本拍需求的 TUN 位）+ 全候选
        // 发送统计（拍板①：Go SwapSendStats——尝试>0 且全部本地失败 = 环境性禁发）
        let Some(client) = run.current_client() else {
            return; // 数据面不在（装配窗口——巡检只活在 attached 后，防御性收口）
        };
        let out_pkts = client.swap_out_pkts();
        let (send_tries, send_local_fails) = client.swap_send_stats();
        // 本拍探测（10s）
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
                let _ = client.rearm_soft_bounded(recover::ACTION);
                if let Some(c) = &run.cache {
                    lock_unpoison(c).note_rearm();
                }
                let merged = run.merged_candidates();
                client.set_candidates(merged);
                run.domain_refresh_async();
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
            && client.refresh_reg_result_bounded(recover::ACTION).unwrap_or(false)
        {
            last_reg = Some(now);
        }
        // ---- 失败证据的需求门控（demand-driven-recovery）----
        let (demand_active, demand_why) = run.demand.patrol_demand(out_pkts, Instant::now());
        run.demand
            .note_demand(demand_active, demand_why, now_unix_ms());
        // 环境噪声双信号（Go tunmode.go:1024-1027 同构——拍板①补全）：
        // ① 全候选发送统计：尝试>0 且全部本地失败 = 环境性禁发（挂起 EPERM 全候选
        //    皆败；蜂窝下 LAN 候选 ENETUNREACH 但中继发得出去 ⇒ 不算全失败）——
        //    覆盖非采纳（赛跑）态下采纳路径粘性信号够不着的盲区；
        // ② 采纳路径粘性（15s 尾窗）——① 不成立时的回看窗。
        let mut local_noise = send_tries > 0 && send_local_fails == send_tries;
        if !local_noise {
            local_noise = client
                .snapshot()
                .last_local_send_err
                .is_some_and(|t| t.elapsed() < NOISE_WINDOW);
        }
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
        let (new_streak, counted) = evidence_gate(
            local_noise,
            demand_active,
            fail_streak,
            last_counted,
            now,
            probe_ok,
        );
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
                (run.logf)(&format!(
                    "对端巡检失败 {}/{}: {}",
                    fail_streak,
                    FAIL_STREAK_LADDER,
                    probe.unwrap_err()
                ));
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
            run.tun_shared.mark_unhealthy_if_current(run.gen, "patrol");
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
    let fail_streak = if fail_streak > 0
        && last_counted.is_some_and(|t| now.duration_since(t) > PATROL_FAIL_WINDOW)
    {
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
    if via == Via::Relay {
        streak + 1
    } else {
        0
    }
}

fn relay_upgrade_due(via: Via, streak: u32) -> bool {
    via == Via::Relay && streak >= RELAY_UPGRADE_EVERY
}

/// 旁路探测候选（只打直连条目——探测中继端点拿到的是中继自己的列表，污染候选表）。
fn run_probe_candidates(run: &Arc<GenRun>) {
    // 域名同步重解析（Go ProbeCandidates 的 3s 有界形态——单飞由 60s 巡检拍限住）。
    if let Some(rf) = run
        .domain_refresher
        .read()
        .map(|g| g.clone())
        .unwrap_or(None)
    {
        if let Some(fresh) =
            rf.refresh_sync(crate::wtransport::domain_eps::PROBE_SYNC_BUDGET)
        {
            *lock_unpoison(&run.domain_cands) = fresh;
            if let Some(cl) = run.current_client() {
                let merged = run.merged_candidates();
                cl.set_candidates(merged);
            }
        }
    }
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
        if let Some(cl) = sh.current_client() {
            cl.set_candidates(merged);
        }
        sh.schedule_save(); // 我-2③：探测线索也触发落盘去抖
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
            std::thread::sleep(
                Duration::from_millis(100).min(next.saturating_duration_since(Instant::now())),
            );
        }
        let Some(client) = run.current_client() else {
            continue;
        };
        let snap = client.snapshot();
        if snap.rx != last_rx {
            last_rx = snap.rx;
            recv_at = Some(Instant::now());
        }
        // 单调时基（评审 r2-H1：last_outbound_at 返回的 Instant 曾由 unix epoch ns
        // 换算——减出 56 年前的时刻 ⇒ should_push 恒 false、D4 整条静默失效。现改
        // TunCounters 的相对单调读数，unix ns 只留给 JSON 面）
        let out_at = client.last_outbound_at();
        // 噪声窗 = 出站新鲜窗（评审 r2-L7：Go shouldPush 用 outboundFresh(5s)——
        // 此前误用巡检的 15s 尾窗）
        let has_fresh_local_err = snap
            .last_local_send_err
            .is_some_and(|t| t.elapsed() < demand::OUTBOUND_FRESH);
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
        let (rd, wr) = run
            .current_client()
            .map(|c| c.tun_stats())
            .unwrap_or((0, 0));
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
    use crate::identity::Identity;
    use crate::token::{self, EndpointRef, PeerId, Secret, TokenSpec};
    use crate::wtransport::Candidate;

    /// TEST-NET-3 黑洞 Client（惰性：只起引擎线程，镜像包无人应答——镜像
    /// `wgcore::tests::engine_probe_blackhole_times_out` 的可行构造）。
    fn blackhole_client() -> Arc<Client> {
        let ident = Identity::ephemeral().expect("临时身份");
        let logf: Logf = Arc::new(|_s: &str| {});
        Arc::new(
            Client::start(CoreConfig {
                peer_id: PeerId::from([1u8; 32]),
                secret: Secret::from([2u8; 32]),
                identity: ident,
                candidates: vec![Candidate {
                    addr: "203.0.113.1:41641".parse().unwrap(),
                    relay: false,
                }],
                logf,
            })
            .expect("黑洞客户端可起"),
        )
    }

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
        let (logf, _lines) = {
            let (tx, rx) = mpsc::channel::<String>();
            let l: Logf = Arc::new(move |s: &str| {
                let _ = tx.send(s.to_owned());
            });
            (l, rx)
        };
        let run = GenRun::synthetic_for_test(logf);
        let ok_port = free_port();
        let squatter = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy_port = squatter.local_addr().unwrap().port();
        run.pf.install(&[
            rule(ok_port, "", 0),
            rule(busy_port, "10.0.0.9", 8080),
        ]);
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
        assert_eq!(f1.code, "bind_failed", "真 bind 失败 = 真值（tier 渲染「端口被占用」）");
        assert_eq!(f1.target, "10.0.0.9:8080");
        // 收尾：停监听器 + 停合成世代的引擎线程
        run.pf.stop_all();
        assert!(runner_of(&run).port_forwards.is_empty(), "收工后空表");
        drop(squatter);
        if let Some(c) = run.current_client() {
            c.stop();
        }
    }

    /// Q-F-B F4-3/§5.2 #15：**世代收工**的两条通路——① 停止位（`gen_stop` = `run.stop`）
    /// 让 accept 线程 ≤1 拍自退 ⇒ 端口立即释放（无孤儿监听器）；② `stop_all` 清表
    /// （收工序 / `Finish::drop` 兜底调的就是它）。设计 §1-F1-3 的 panic 收口句据此订正：
    /// 未换入的监听器由 `install` 的暂存守卫置停止位，不依赖 `stop_all` 的视野。
    #[test]
    fn generation_stop_flag_releases_ports_and_teardown_clears_states() {
        let (logf, _lines) = {
            let (tx, rx) = mpsc::channel::<String>();
            let l: Logf = Arc::new(move |s: &str| {
                let _ = tx.send(s.to_owned());
            });
            (l, rx)
        };
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
        // 状态表在 stop_all 之前仍是旧表（Go「stopPortForwards 之前不动作」同义；
        // 窗口 = 世代主线程 ≤200ms 的停止位轮询）
        assert_eq!(runner_of(&run).port_forwards.len(), 1);
        // ② 收工清表（gen_loop 收工段与 Finish::drop 兜底都调它）
        run.pf.stop_all();
        assert!(runner_of(&run).port_forwards.is_empty(), "收工后空表");
        if let Some(c) = run.current_client() {
            c.stop();
        }
    }

    /// Q-F-B F4-4：热替换 rc 回 Go 语义——活世代 ⇒ `0`（**真装表**）；无世代 / 换代
    /// （gen 不符）/ 收口（stop 位）⇒ `-1` 且**无孤儿监听器**。
    #[test]
    fn request_port_forwards_rc_zero_with_live_gen_stale_minus_one() {
        let (logf, _lines) = {
            let (tx, rx) = mpsc::channel::<String>();
            let l: Logf = Arc::new(move |s: &str| {
                let _ = tx.send(s.to_owned());
            });
            (l, rx)
        };
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
        // 収口（stop 位）⇒ -1 且撤回监听器（不留孤儿）
        run.stop.store(true, Ordering::Release);
        assert_eq!(exec.request_port_forwards(rules.clone()), -1);
        assert!(runner_of(&run).port_forwards.is_empty(), "无孤儿监听器（states 已清）");
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
        if let Some(c) = run.current_client() {
            c.stop();
        }
    }

    /// F3：拨号预算把**阶梯等待**纳入同一上界——桩闭包阻塞到期限，`dial_with_recover`
    /// 仍在调用方预算内返回 TimedOut（首试预算注入缩短，无需 4s 真等）。
    #[test]
    fn dial_with_recover_bounded_by_caller_budget() {
        let client = blackhole_client();
        let called = Arc::new(AtomicBool::new(false));
        let called2 = Arc::clone(&called);
        let saw_deadline = Arc::new(AtomicBool::new(false));
        let saw2 = Arc::clone(&saw_deadline);
        let dst: SocketAddrV4 = "10.7.0.1:7802".parse().unwrap();
        let budget = Duration::from_millis(600);
        let t0 = Instant::now();
        let r = dial_with_recover(
            &client,
            dst,
            budget,
            Duration::from_millis(50), // 首试（注入；生产 = FIRST_TRY 4s）
            move |lvl, deadline, _cause| {
                called2.store(true, Ordering::Release);
                saw2.store(deadline.is_some(), Ordering::Release);
                assert_eq!(lvl, Level::R2, "拨号失败后从 R2 起跑");
                // 桩「阻塞到期限」：执行方模拟阶梯跑满调用方预算
                std::thread::sleep(Duration::from_millis(400));
                (LadderRc::Deadline, false)
            },
        );
        assert!(called.load(Ordering::Acquire), "桩必须被调用（否则本测假绿）");
        assert!(saw_deadline.load(Ordering::Acquire), "期限必须透给阶梯");
        let e = r.expect_err("预算耗尽 ⇒ Err");
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        assert!(
            t0.elapsed() < Duration::from_secs(3),
            "预算内返回（实耗 {:?}）",
            t0.elapsed()
        );
        client.stop();
    }

    /// F3：`Deadline` 的边界映射 = -1（不是 -3：tier 对 -3 渲染「本机网络栈没准备好」
    /// = 错误归因）。
    #[test]
    fn ladder_deadline_maps_to_minus_one() {
        assert_eq!(LadderRc::Deadline.as_rc(), -1);
    }

    /// 复核 r3-F1：空候选早退路径（token 端点列表空 ⇒ 候选空）必须放单飞锁——下一次
    /// prepare 可受理。回归背景：EarlyFinish 守卫引入前该路径漏 finish_generation ⇒
    /// tun_prepare 之后恒 -1、tun_stop 恒 -2（接线即坏同形态）。
    /// 注（P0-4 起）：域名端点不再天然构成空候选（建会话时解析一次、失败跳过）——
    /// 空候选的确定性构造改用空端点列表。
    #[test]
    fn tunnel_empty_candidates_releases_lock() {
        let peer = PeerId::from([1u8; 32]);
        let secret = Secret::from([2u8; 32]);
        let eps: [EndpointRef; 0] = [];
        let tok = token::encode(&TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
        })
        .expect("空端点 token 可编码");
        let demand = Arc::new(DemandSignals::new());
        let exec = TunnelExec::new(Arc::clone(&demand));
        let core = ClientCore::with_shared(exec, demand);
        assert_eq!(
            core.tun_prepare(&format!(r#"{{"token":"{tok}"}}"#), true),
            0
        );
        // 等世代线程走到 failed 终态（放锁发生在 finish_generation）
        let deadline = Instant::now() + Duration::from_secs(3);
        while core.tun_status().contains("\"state\":\"preparing\"") {
            assert!(Instant::now() < deadline, "空候选应在 3s 内落 failed 终态");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            core.tun_status().contains("\"state\":\"failed\""),
            "空候选 = failed 终态"
        );
        // 锁已放：下一次 prepare 可受理（不是 -1 忙）
        assert_eq!(
            core.tun_prepare(&format!(r#"{{"token":"{tok}"}}"#), true),
            0
        );
        // 收尾：等第二次也落终态再停（避免测试进程留下游离世代线程）
        let deadline = Instant::now() + Duration::from_secs(3);
        while core.tun_status().contains("\"state\":\"preparing\"") {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(core.tun_stop(), 0, "失败终态后 stop 应即收 0");
        let _ = TunStage::Idle; // 引用 stage 枚举（保持 import）
    }
}
