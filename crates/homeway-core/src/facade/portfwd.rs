//! 端口转发：整表热替换面 + **真监听器运行时**（语义真源
//! `baseline:clientcore/cmd/clientcore/app_portfwd.go`）。
//!
//! 三消费方共一条路：attach 首启（tunConfig）/ 运行中热替换（NAPI）/ 世代收工（nil 表）。
//! NAPI 面（`tunSetPortForwardsJSON`）的返回码契约：
//! `0` 已应用 / `-1` 当前没有**已接管数据面**的世代（改动随下次连接的 tunConfig 自然
//! 生效，可稍后重试）/ `-2` JSON 非法或校验不过（listen=0、targetPort=0、listen 值域外、
//! 目标非 IPv4 字面量、同表内 listen 重复、**条数超过上限**）。
//!
//! 监听器形态（Q-F-B）：每条映射在 `127.0.0.1:<listen>` 真 bind（仅回环，不暴露局域网），
//! 入站连接经注入的**拨号缝**（生产 = 经隧道裸拨——`GenRun` 面）拨到目标后双向泵转发。
//! 运行时**不依赖 `GenRun`**（拨号与停止位全注入）⇒ 纯回环可测。
//!
//! 目标语义（`pfTargetText`——**主机**措辞是 NAPI 面口径，与 pkg/portfwd.DescribeTarget
//! 的「出口自己」是两处文案）：targetIp 空 ⇒ 拨出口自己；port 0 ⇒ 同监听端口；
//! **环回/未指定**（`127.0.0.0/8`、`0.0.0.0`）⇒ 同样落到「出口本机」（Go 经出口过境
//! 重拨到出口的 127.0.0.1 同效；本仓 WG 档栈显式拒环回，不映射就是「监听中但连不上」）。
//!
//! **「出口本机」的线上形态（M4 §1.3 裁决 D-1）**：`ExitPort(p)` 交给拨号缝的地址 =
//! **`127.0.0.1:p`**（QUIC 档：出口 dial 腿直接拨本机回环）；WG 档由
//! `tun_exec::wg_dial_addr` 在**拨号腿内**替换为 `SERVER_TUNNEL_IP:p`（wire 语义不外泄）。

use std::collections::BTreeSet;
use std::io;
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Deserialize;

use super::bridge_host::BridgeStream;
use super::tun_shared::lock_unpoison;
use crate::Logf;
use crate::PortfwdErr;

/// 监听端口值域下限（`pkg/portfwd` 同源）。
pub const MIN_PORT: u16 = 1024;

/// 规则条数上限（spec「每主机映射数量上限 8 条」镜像；Go 桌面 `facade/forward.go` 同值）。
pub const MAX_PF_RULES: usize = 8;

/// 并发转发流阀（**偏离 Go 的 4096**——口径 = 手机内存预算：引擎每连接 2×1 MiB 缓冲
/// （`wgcore/stackb.rs` 的 `TCP_BUF`）⇒ 阀 256 的缓冲最坏 ≈512 MiB，仍 ≥8× 于浏览器
/// 常态并发。Go 的自证注释称「与 gVisor 流共用」**已陈旧**：读写只在端口转发面）。
pub const MAX_PF_FLOWS: u64 = 256;

/// install 等待旧监听器退出 ack 的预算（**共享**：全部待等监听器同一个 400ms）。
const ACK_BUDGET_INSTALL: Duration = Duration::from_millis(400);
/// 收工等待监听器 ack 的预算（共享 200ms；到点 detach 自退）。
const ACK_BUDGET_STOP: Duration = Duration::from_millis(200);
/// 未 ack 端口的 bind 重试次数 × 间隔（3 × 50ms——旧监听器还没放掉 fd 的窄窗）。
const BIND_RETRY_TIMES: u32 = 3;
const BIND_RETRY_GAP: Duration = Duration::from_millis(50);
/// accept 轮询节拍（`poll(2)`；用户可见延迟路径——连接就绪即接，50ms 只是停止延迟上界）。
const ACCEPT_POLL_MS: i32 = 50;
/// 监听 backlog（Go `net.Listen`：Linux = `somaxconn` / darwin = 128 ⇒ 本批固定 128）。
const LISTEN_BACKLOG: i32 = 128;
/// 每连接线程的显式栈（conn 1 + 泵 2 = 3 线程/连接；默认 2 MiB ×3 无谓吃虚拟内存）。
///
/// **实现注记（代码门 r27 P6 订正，设计 v3 写 128 KiB）**：取 **256 KiB**——与本仓全部
/// 会话面线程同档（`session/mod.rs`/`domain_eps.rs`/`daemon/carriers/forward.rs` 均 256K），
/// 因为 pf 的 conn/泵线程跑的就是同一批 `SessionStream`（`connect_deadline`/`read`/`write`）
/// 代码；Rust 栈溢出的后果是**进程 abort**（手机上 = 核进程死 + 整套 VPN 重建），
/// 而「省虚拟内存」的收益可忽略（3×256 KiB×256 流 ≈192 MiB 虚拟 vs 引擎 512 MiB RSS）。
const PF_THREAD_STACK: usize = 256 * 1024;

/// 一条映射（tunConfig.portForwards 元素 / NAPI 热替换单元的同形状）。
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct PortForwardRule {
    pub listen: u16,
    #[serde(rename = "targetIp", default)]
    pub target_ip: String,
    #[serde(rename = "targetPort", default)]
    pub target_port: u16,
}

/// 整表校验错误（NAPI 门全部折为 `-2`；类型化以便测试断言归因）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum TableErr {
    #[error("非法 JSON：{0}")]
    BadJson(String),
    #[error("listen/targetPort 为 0（listen={listen} targetPort={target_port}）")]
    ZeroPort { listen: u16, target_port: u16 },
    #[error("监听端口 {0} 不在 1024–65535")]
    ListenRange(u16),
    #[error("目标地址须为空（出口自己）或 IPv4 字面量：{0:?}")]
    BadTarget(String),
    #[error("同表内监听端口重复：{0}")]
    DupListen(u16),
    #[error("映射数超过上限（{max}）：{n} 条")]
    TooMany { n: usize, max: usize },
}

/// 整表校验（NAPI 热替换前置；Go tunSetPortForwardsJSON 的循环体逐条对齐 + 条数上限）。
pub fn validate_table(rules: &[PortForwardRule]) -> Result<(), TableErr> {
    if rules.len() > MAX_PF_RULES {
        return Err(TableErr::TooMany { n: rules.len(), max: MAX_PF_RULES });
    }
    let mut seen = std::collections::BTreeSet::new();
    for f in rules {
        if f.listen == 0 || f.target_port == 0 {
            return Err(TableErr::ZeroPort { listen: f.listen, target_port: f.target_port });
        }
        // 值域与目标语义走共享面：App 与桌面同一条规则同一个答案（FIX-43）
        if !(MIN_PORT..=u16::MAX).contains(&f.listen) {
            return Err(TableErr::ListenRange(f.listen));
        }
        if !f.target_ip.is_empty() && f.target_ip.parse::<Ipv4Addr>().is_err() {
            return Err(TableErr::BadTarget(f.target_ip.clone()));
        }
        if !seen.insert(f.listen) {
            // 同表内重复监听端口：第二条注定 EADDRINUSE，界面会显示一条莫名其妙的
            // 「失败」（表单本来拦得住，这里兜住被绕过/损坏的 JSON）。
            return Err(TableErr::DupListen(f.listen));
        }
    }
    Ok(())
}

/// 目标呈现文案（NAPI 面口径；port 0 = 同监听端口——语义真源 facade.DescribeTarget/FIX-46，
/// 措辞按 pfTargetText 的「主机」）。
pub fn pf_target_text(f: &PortForwardRule) -> String {
    if f.target_ip.is_empty() {
        if f.target_port == 0 {
            return "主机（同端口）".to_owned();
        }
        return format!("主机:{}", f.target_port);
    }
    let port = if f.target_port == 0 { f.listen } else { f.target_port };
    format!("{}:{}", f.target_ip, port)
}

/// 拨号目标（target_ip 空 / 环回 / 未指定 = 出口自己；否则经出口拨任意目标）。
///
/// **「出口本机」的载体是 `ExitPort`**（与具体承载无关的语义位）；它**在线上**的地址由各承载的
/// 拨号腿决定（M4 §1.3 裁决 D-1）：QUIC 档 = `127.0.0.1:p`（[`PfDialTarget::resolve`]，出口
/// dial 腿直接拨本机回环）；WG 档 = `SERVER_TUNNEL_IP:p`（`tun_exec::wg_dial_addr` 在 WG 腿内
/// 替换——出口 intercept 的豁免臂再落回出口回环）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PfDialTarget {
    /// 拨出口自己的端口。
    ExitPort(u16),
    /// 经出口拨任意可达目标。
    Remote(SocketAddrV4),
}

impl PfDialTarget {
    /// 真正交给拨号缝的地址（**M4 §1.3 裁决 D-1**：`ExitPort` = 出口**回环**的该端口）。
    ///
    /// 为什么不是 `SERVER_TUNNEL_IP`（WG 的隧道地址常量）：①「出口本机」在 QUIC 档**没有隧道
    /// IP 可指**（出口不持有该地址语义）——继续用它当哨兵就是「为旧承载留隐含依赖」；②回环即
    /// 「出口本机」是**平台无关的真话**（tier spec 已把「出口侧回环目标不经代理」写进需求）；
    /// ③映射收敛在**各承载的拨号腿**内 = 单一职责（QUIC 腿零 WG 常量引用，隔离门可判）；
    /// ④验收口径简单：wire 上出现 `127.0.0.1:p` 就是「出口本机」。
    ///
    /// 登记（§8 行 6）：`targetIp = 100.64.255.1`（手填出口隧道 IP 常量）在 QUIC 档 =
    /// 「字面拨 100.64.255.1」（通常拒），与 WG 档的「豁免臂 ⇒ 出口回环」不同；**不引入别名**
    /// （无兼容包袱常设口径；App 表单不会产生该常量）。
    pub(crate) fn resolve(&self) -> SocketAddrV4 {
        match self {
            PfDialTarget::ExitPort(p) => SocketAddrV4::new(Ipv4Addr::LOCALHOST, *p),
            PfDialTarget::Remote(a) => *a,
        }
    }
}

impl PortForwardRule {
    /// 拨号目标派生。**环回/未指定映射为 `ExitPort`**（= 「出口本机」，其线上形态见
    /// [`PfDialTarget::resolve`]）：Go 经出口过境重拨到出口的 `127.0.0.1`（能通）；WG 档的
    /// 客户端栈显式拒环回（`stackb::connect` 的能力缺失）——不映射就是「监听中但连不上」。
    /// 非法 `target_ip` ⇒ `BadTarget`（**装配期**拒，不 bind）。
    pub fn dial_target(&self) -> Result<PfDialTarget, TableErr> {
        if self.target_ip.is_empty() {
            return Ok(PfDialTarget::ExitPort(self.target_port));
        }
        let ip: Ipv4Addr = self
            .target_ip
            .parse()
            .map_err(|_| TableErr::BadTarget(self.target_ip.clone()))?;
        let port = if self.target_port == 0 { self.listen } else { self.target_port };
        if ip.is_loopback() || ip.is_unspecified() {
            return Ok(PfDialTarget::ExitPort(port));
        }
        Ok(PfDialTarget::Remote(SocketAddrV4::new(ip, port)))
    }
}

/// 一条映射的运行态（pfState；tunStatusJSON.portForwards[] 的数据源）。
///
/// `snapshot()` 是 `portForwards[]` 元素的**唯一组装点**；可变位只有两处：
/// `conns`（活跃转发连接数）与 `late_fail`（迟到失败——accept 致命错误）。
#[derive(Debug)]
pub struct PfState {
    pub listen: u16,
    pub target: String,
    state: PfStateKind,
    err: String,
    /// 失败映射的稳定错误码（portfwd/err 词表）；成功态与「非 bind 失败」为 None
    /// （空串 = App 空码兜底路径——不误归因：`bind_failed` 在 tier 渲染「端口被占用」）。
    code: Option<PortfwdErr>,
    /// 当前活跃转发连接数（accept 准入 +1、连接结束 -1）。
    conns: AtomicI64,
    /// 迟到失败（accept **致命**错误——本批唯一来源）。有值时 `snapshot()` 优先报
    /// failed + 空码 + 该 err。
    ///
    /// **偏离 Go**（设计门 D15）：Go 在 accept 致命错误后仍持 `ln`（端口仍绑、只是
    /// 无人 accept，状态留 `listening`）；Rust 的 fd 单属 accept 线程 ⇒ 退工即释放
    /// 端口 ⇒ 留 `listening` 就是「监听中但连不上」的谎报（本批要消灭的形态）。
    ///
    /// `OnceLock` 承担「写一次、首个原因保留」（`set` 幂等、读无锁）。
    late_fail: std::sync::OnceLock<String>,
}

/// 映射状态（enum 承担词面不变量）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PfStateKind {
    /// 监听器在位。
    Listening,
    /// 监听失败（真 bind 失败 / 迟到失败）。
    Failed,
}

impl PfStateKind {
    /// 线上词面（tier 页面按它分派渲染）。
    pub fn as_str(self) -> &'static str {
        match self {
            PfStateKind::Listening => "listening",
            PfStateKind::Failed => "failed",
        }
    }
}

impl PfState {
    /// 成功监听态。
    pub fn listening(listen: u16, target: String) -> Self {
        PfState {
            listen,
            target,
            state: PfStateKind::Listening,
            err: String::new(),
            code: None,
            conns: AtomicI64::new(0),
            late_fail: std::sync::OnceLock::new(),
        }
    }

    /// 监听失败态（真 bind 失败；单条失败只记状态、不阻断隧道——软失败不回滚哲学）。
    pub fn failed(listen: u16, target: String, err: String) -> Self {
        PfState::failed_with(listen, target, err, Some(PortfwdErr::BindFailed))
    }

    /// 失败态（显式错误码；`None` = 空码兜底路径——装配期破损配置 / 迟到失败：
    /// 这些形态**不是**「端口被占用」，不许假归因）。
    pub fn failed_with(listen: u16, target: String, err: String, code: Option<PortfwdErr>) -> Self {
        PfState {
            listen,
            target,
            state: PfStateKind::Failed,
            err,
            code,
            conns: AtomicI64::new(0),
            late_fail: std::sync::OnceLock::new(),
        }
    }

    /// 迟到失败置位（accept 致命错误 / accept 线程异常退出；幂等——首个原因保留）。
    pub(crate) fn mark_late_failed(&self, err: String) {
        let _ = self.late_fail.set(err);
    }

    /// 当前活跃连接数（测试/观测读）。
    pub fn conns(&self) -> i64 {
        self.conns.load(Ordering::Relaxed)
    }

    /// tunStatusJSON 的 portForwards 元素（快照面；conns 现读现给）。
    pub fn snapshot(&self) -> super::tun_status::PfStateIn {
        let (state, err, code) = match self.late_fail.get().cloned() {
            Some(late) => ("failed", late, String::new()),
            None => (
                self.state.as_str(),
                self.err.clone(),
                self.code.map(|c| c.as_str().to_owned()).unwrap_or_default(),
            ),
        };
        super::tun_status::PfStateIn {
            listen: self.listen,
            target: self.target.clone(),
            state: state.to_owned(),
            err,
            code,
            conns: self.conns.load(Ordering::Relaxed),
        }
    }
}

/// `{"portForwards":[…]}` 的信封解析。
pub fn parse_rules_json(cfg: &str) -> Result<Vec<PortForwardRule>, TableErr> {
    #[derive(Deserialize)]
    struct Envelope {
        #[serde(rename = "portForwards", default)]
        port_forwards: Vec<PortForwardRule>,
    }
    let v: Envelope =
        serde_json::from_str(cfg).map_err(|e| TableErr::BadJson(e.to_string()))?;
    Ok(v.port_forwards)
}

// ---------------------------------------------------------------------------
// 运行时（真监听器）
// ---------------------------------------------------------------------------

/// 运行时上限（注入面：生产 = 8 / 256；测试用小阀值直喂）。
#[derive(Debug, Clone, Copy)]
pub struct PfLimits {
    pub max_rules: usize,
    pub max_flows: u64,
}

impl Default for PfLimits {
    fn default() -> Self {
        PfLimits {
            max_rules: MAX_PF_RULES,
            max_flows: MAX_PF_FLOWS,
        }
    }
}

/// 运行时计数（全部 `AtomicU64`，类型一致）。
#[derive(Debug, Default)]
pub struct PfCounters {
    accepted: AtomicU64,
    fails: AtomicU64,
    flows: AtomicU64,
    flow_rejected: AtomicU64,
}

impl PfCounters {
    /// accept 准入的转发连接数（`stats.pfAccepted` 源）。
    pub fn accepted(&self) -> u64 {
        self.accepted.load(Ordering::Relaxed)
    }
    /// 拨号失败数（`stats.pfFails` 源）。
    pub fn fails(&self) -> u64 {
        self.fails.load(Ordering::Relaxed)
    }
    /// 在册并发流（阀的当前值）。
    pub fn flows(&self) -> u64 {
        self.flows.load(Ordering::Relaxed)
    }
    /// 阀拒绝累计（观测面）。
    pub fn flow_rejected(&self) -> u64 {
        self.flow_rejected.load(Ordering::Relaxed)
    }
}

/// 拨号缝（全注入——运行时**不依赖 `GenRun`**：纯回环可测；生产闭包持
/// `Weak<GenRun>` 防 Arc 环 ⇒ 世代可回收）。
pub type PfDialFn =
    Arc<dyn Fn(SocketAddrV4, Duration) -> io::Result<Box<dyn BridgeStream>> + Send + Sync>;

/// 监听器退出 ack 的等待结果（`Disconnected` = 线程已退出 = fd 已关，**不是**超时）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AckOutcome {
    Acked,
    Disconnected,
    Timeout,
}

/// ack 等待缝（测试注入「永不 ack / 慢 ack」直喂超时路径；生产 = 有界 `recv_timeout`）。
pub type AckWaitFn = Arc<dyn Fn(&mpsc::Receiver<()>, Duration) -> AckOutcome + Send + Sync>;

fn wait_ack_default(rx: &mpsc::Receiver<()>, budget: Duration) -> AckOutcome {
    match rx.recv_timeout(budget) {
        Ok(()) => AckOutcome::Acked,
        Err(mpsc::RecvTimeoutError::Disconnected) => AckOutcome::Disconnected,
        Err(mpsc::RecvTimeoutError::Timeout) => AckOutcome::Timeout,
    }
}

/// 运行时装配参数（注入面）。
pub struct PfSetup {
    pub dial: PfDialFn,
    /// 世代停止位（= `GenRun.stop` 克隆）。
    pub stop: Arc<AtomicBool>,
    pub logf: Logf,
    /// 每连接拨号预算（`dialMs`，缺省 15s）。
    pub budget: Duration,
    pub limits: PfLimits,
}

/// 线程共享面（accept/conn/泵线程只持它，不持整个运行时）。
struct PfContext {
    counters: Arc<PfCounters>,
    dial: PfDialFn,
    logf: Logf,
    budget: Duration,
    limits: PfLimits,
    gen_stop: Arc<AtomicBool>,
    /// 【测试缝】conn 线程 spawn **尝试**计数（阀「未 spawn 线程」的直接证据）。
    #[cfg(test)]
    conn_spawns: AtomicU64,
}

/// 一个在册监听器（**不持 listener fd**——fd 随 accept 线程走；`acked` 收到即在
/// 「fd 确已关」之后，是端口释放的可观测证据）。
struct PfListener {
    listen: u16,
    stop: Arc<AtomicBool>,
    acked: mpsc::Receiver<()>,
    handle: JoinHandle<()>,
}

struct PfInner {
    /// 新表就位前原地保留（否则状态面出现「空表」第三态）。
    states: Vec<Arc<PfState>>,
    lns: Vec<PfListener>,
}

/// `install` 阶段 3 的**暂存守卫**（代码门 r27 P3）：已 spawn 但尚未换入 `inner` 的
/// 监听器不在 `stop_all` 的视野里——本守卫在 drop（含 panic 展开 / 任何早退）时对
/// 未提交条目置停止位，线程 ≤1 拍自退、fd 即关 ⇒ 孤儿窗口收敛为 0。
struct StagedLns {
    lns: Vec<PfListener>,
    committed: bool,
}

impl Drop for StagedLns {
    fn drop(&mut self) {
        if !self.committed {
            for l in &self.lns {
                l.stop.store(true, Ordering::Release);
            }
        }
    }
}

/// portfwd 运行时（规则替换 / 真监听 / 真计数）。
pub struct PfRuntime {
    /// install / stop_all 串行（长动作——bind/等 ack/spawn——全在状态锁外）。
    install: Mutex<()>,
    /// 短临界区：单次换入 / 读状态。
    inner: Mutex<PfInner>,
    ctx: Arc<PfContext>,
    wait_ack: AckWaitFn,
    /// 【测试缝】换入前钩子（确定性直喂「读侧只见全旧/全新」，替代不可信的并发采样）。
    #[cfg(test)]
    swap_seam: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl PfRuntime {
    pub fn new(setup: PfSetup) -> Self {
        PfRuntime {
            install: Mutex::new(()),
            inner: Mutex::new(PfInner {
                states: Vec::new(),
                lns: Vec::new(),
            }),
            ctx: Arc::new(PfContext {
                counters: Arc::new(PfCounters::default()),
                dial: setup.dial,
                logf: setup.logf,
                budget: setup.budget,
                limits: setup.limits,
                gen_stop: setup.stop,
                #[cfg(test)]
                conn_spawns: AtomicU64::new(0),
            }),
            wait_ack: Arc::new(wait_ack_default),
            #[cfg(test)]
            swap_seam: None,
        }
    }

    /// 【测试缝】注入 ack 等待（须在 `Arc` 共享前调用）。
    #[cfg(test)]
    pub(crate) fn inject_wait_ack(&mut self, f: AckWaitFn) {
        self.wait_ack = f;
    }

    /// 【测试缝】注入换入前钩子；`None` = 关闭。
    #[cfg(test)]
    pub(crate) fn inject_swap_seam(&mut self, f: Option<Arc<dyn Fn() + Send + Sync>>) {
        self.swap_seam = f;
    }

    /// 计数（`runner_of`/`stats` 行源）。
    pub fn counters(&self) -> &PfCounters {
        &self.ctx.counters
    }

    /// 当前整表状态快照（`portForwards[]` 唯一数据源；短锁克隆 + 出锁 snapshot）。
    pub fn snapshot_states(&self) -> Vec<super::tun_status::PfStateIn> {
        let states = lock_unpoison(&self.inner).states.clone();
        states.iter().map(|s| s.snapshot()).collect()
    }

    /// 在册监听器数（测试断言「破损配置未 bind」的直读面）。
    #[cfg(test)]
    pub(crate) fn lns_len(&self) -> usize {
        lock_unpoison(&self.inner).lns.len()
    }

    /// 【测试缝】conn 线程 spawn 尝试计数（阀在 spawn 之前的直接证据）。
    #[cfg(test)]
    pub(crate) fn conn_thread_spawns(&self) -> u64 {
        self.ctx.conn_spawns.load(Ordering::Relaxed)
    }

    /// 整表替换（attach 首启 / NAPI 热替换）。状态面对外只看到「全旧」或「全新」：
    /// 旧 `states` 原地保留到**单次换入**；旧 `lns` 只在短锁内 take，等 ack / bind /
    /// spawn 全在锁外（Go 持 `pfMu` 的等价语义）。
    pub fn install(&self, rules: &[PortForwardRule]) {
        let _serial = lock_unpoison(&self.install);
        let logf = Arc::clone(&self.ctx.logf);
        // ---- 阶段 1：短锁只 take 旧 lns（states 原地保留——不许出现空表第三态）----
        let old = {
            let mut inner = lock_unpoison(&self.inner);
            std::mem::take(&mut inner.lns)
        };
        let n_old = old.len();
        let reuse: BTreeSet<u16> = rules.iter().map(|r| r.listen).collect();
        let mut awaiting: Vec<PfListener> = Vec::new();
        for l in old {
            l.stop.store(true, Ordering::Release);
            if reuse.contains(&l.listen) {
                // 端口被新表复用：必须确认 fd 已关才能重绑；其余 detach 自退（≤1 拍）
                awaiting.push(l);
            }
        }
        if n_old > 0 {
            (logf)(&format!("port-forward: 已停止全部监听器（{n_old} 个）"));
        }
        // ---- 阶段 2：等 ack（**共享**预算）----
        let mut unfreed: BTreeSet<u16> = BTreeSet::new();
        if !awaiting.is_empty() {
            let deadline = Instant::now() + ACK_BUDGET_INSTALL;
            for l in awaiting {
                let left = deadline.saturating_duration_since(Instant::now());
                let outcome = if left.is_zero() {
                    AckOutcome::Timeout
                } else {
                    (self.wait_ack)(&l.acked, left)
                };
                match outcome {
                    AckOutcome::Acked | AckOutcome::Disconnected => {
                        let _ = l.handle.join();
                    }
                    AckOutcome::Timeout => {
                        unfreed.insert(l.listen);
                        (logf)(&format!(
                            "port-forward: 旧监听器 {} 未在预算内退出——已在后台自退",
                            l.listen
                        ));
                        // handle 随 l drop ⇒ detach（线程仍在 ≤1 拍内自退）
                    }
                }
            }
        }
        // ---- 阶段 3：逐条 bind + spawn（失败/破损逐条 failed，不影响其余）----
        // 暂存守卫：panic 展开（或本函数任何早退）时对**尚未换入** `inner` 的监听器
        // 置停止位——它们不在 `stop_all` 的视野里（代码门 r27 P3）。
        let mut staged = StagedLns {
            lns: Vec::new(),
            committed: false,
        };
        let mut states: Vec<Arc<PfState>> = Vec::with_capacity(rules.len());
        for (idx, r) in rules.iter().enumerate() {
            let (st, ln) = self.install_one(r, idx, &unfreed);
            states.push(st);
            if let Some(l) = ln {
                staged.lns.push(l);
            }
        }
        // ---- 阶段 4：单次换入（同一临界区写两字段 ⇒ 读侧无半表）----
        #[cfg(test)]
        if let Some(seam) = &self.swap_seam {
            seam();
        }
        let mut inner = lock_unpoison(&self.inner);
        inner.lns = std::mem::take(&mut staged.lns);
        staged.committed = true;
        inner.states = states;
    }

    /// 装一条映射：防御面 → bind（含「旧监听器未在预算内释放」的 3×50ms 有界重试）
    /// → spawn accept 线程。返回状态（必产）+ 成功的监听器（spawn 成功才有）。
    fn install_one(
        &self,
        r: &PortForwardRule,
        idx: usize,
        unfreed: &BTreeSet<u16>,
    ) -> (Arc<PfState>, Option<PfListener>) {
        let logf = Arc::clone(&self.ctx.logf);
        let target = pf_target_text(r);
        if let Some(err) = defensive_err(r, idx, self.ctx.limits.max_rules) {
            (logf)(&format!("port-forward: {err}"));
            return (Arc::new(PfState::failed_with(r.listen, target, err, None)), None);
        }
        let dst = match r.dial_target() {
            Ok(d) => d,
            // 防御面已拦下同名形态（不达此分支）；真到了也如实 failed，不静默
            Err(e) => {
                let err = format!("{e}——本条未建立监听");
                (logf)(&format!("port-forward: {err}"));
                return (Arc::new(PfState::failed_with(r.listen, target, err, None)), None);
            }
        };
        let mut bind = pf_bind(r.listen);
        if bind.is_err() && unfreed.contains(&r.listen) {
            // 旧监听器未在预算内释放的窄窗：有界重试 bind（**不静默**——不许把内部
            // 竞态伪装成用户可见的「端口被占用」；仍失败时要双记行）
            for _ in 0..BIND_RETRY_TIMES {
                std::thread::sleep(BIND_RETRY_GAP);
                bind = pf_bind(r.listen);
                if bind.is_ok() {
                    break;
                }
            }
        }
        match bind {
            Ok(ln) => {
                let st = Arc::new(PfState::listening(r.listen, target.clone()));
                match self.spawn_listener(r.listen, ln, Arc::clone(&st), dst) {
                    Ok(l) => {
                        (logf)(&format!(
                            "port-forward: 127.0.0.1:{} -> {target} 监听中",
                            r.listen
                        ));
                        (st, Some(l))
                    }
                    Err(e) => {
                        // spawn 失败 ⇒ 监听器随 spawn 错误丢弃（fd 即关）+ 该条 failed
                        //（不静默、不谎报 listening）
                        let err = format!("监听线程启动失败（{e}）——该条映射不可用，不影响隧道");
                        (logf)(&format!("port-forward: 127.0.0.1:{} {err}", r.listen));
                        (Arc::new(PfState::failed_with(r.listen, target, err, None)), None)
                    }
                }
            }
            Err(e) => {
                if unfreed.contains(&r.listen) {
                    (logf)(&format!(
                        "port-forward: {} 旧监听器未在预算内释放——已重试 bind 仍失败",
                        r.listen
                    ));
                }
                (logf)(&format!(
                    "port-forward: 监听 127.0.0.1:{} 失败（{e}）——该条映射不可用，不影响隧道",
                    r.listen
                ));
                (Arc::new(PfState::failed(r.listen, target, e.to_string())), None)
            }
        }
    }

    /// 世代收工（Go `stopPortForwards` = `setPortForwards(nil)`）：停全部监听器 +
    /// 清状态（停止之后确实不在监听，状态里不许残留 `listening`）。**幂等**。
    ///
    /// 顺序：先停/等 ack（窗口内状态仍如实反映「监听器正在关」），ack 之后才清 states
    /// ——不留「states 空而 fd 未关」的第三种谎报。
    pub fn stop_all(&self) {
        let _serial = lock_unpoison(&self.install);
        let old = {
            let mut inner = lock_unpoison(&self.inner);
            std::mem::take(&mut inner.lns)
        };
        if old.is_empty() {
            lock_unpoison(&self.inner).states.clear();
            return;
        }
        let n = old.len();
        for l in &old {
            l.stop.store(true, Ordering::Release);
        }
        (self.ctx.logf)(&format!("port-forward: 已停止全部监听器（{n} 个）"));
        let deadline = Instant::now() + ACK_BUDGET_STOP;
        for l in old {
            let left = deadline.saturating_duration_since(Instant::now());
            let outcome = if left.is_zero() {
                AckOutcome::Timeout
            } else {
                (self.wait_ack)(&l.acked, left)
            };
            if outcome == AckOutcome::Timeout {
                (self.ctx.logf)(&format!(
                    "port-forward: 旧监听器 {} 未在预算内退出——已在后台自退",
                    l.listen
                ));
            } else {
                let _ = l.handle.join();
            }
        }
        lock_unpoison(&self.inner).states.clear();
    }

    /// bind + spawn 一条 accept 线程（fd 单属该线程；退出即投 ack——在 fd 关闭之后）。
    fn spawn_listener(
        &self,
        listen: u16,
        ln: TcpListener,
        st: Arc<PfState>,
        dst: PfDialTarget,
    ) -> io::Result<PfListener> {
        let (ack_tx, acked) = mpsc::channel::<()>();
        let stop = Arc::new(AtomicBool::new(false));
        let ctx = Arc::clone(&self.ctx);
        let own_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("homeway-pf-accept".into())
            .stack_size(PF_THREAD_STACK)
            .spawn(move || pf_accept_thread(ctx, listen, ln, st, own_stop, ack_tx, dst))?;
        Ok(PfListener {
            listen,
            stop,
            acked,
            handle,
        })
    }
}

/// 装配期防御面（tunConfig 旁路可达的破损配置——逐条、不影响其余条目）：
/// `listen` 非法 / 超条数 / `target_ip` 非法 一律**不 bind** + `failed` 空码 + 精确 err。
///
/// 可达性：表单路径拦得住（NAPI 门 `-2`），但**手改/损坏的持久化记录可达**
/// （`parsePortForwards` 只丢 id/端口为 0 的条目、`HostStore.updateForwards` 不校验）。
fn defensive_err(r: &PortForwardRule, idx: usize, max_rules: usize) -> Option<String> {
    if r.listen == 0 || r.listen < MIN_PORT {
        // Go 对 listen=0 会绑随机端口（状态面与实况不符）⇒ 偏离 Go 的加固：拒绝 + 精确 err
        return Some(format!(
            "监听端口 {} 不在 1024–65535（本机未建立监听）",
            r.listen
        ));
    }
    if idx >= max_rules {
        return Some(format!("映射数超过上限（{max_rules}）——本条未建立监听"));
    }
    if !r.target_ip.is_empty() && r.target_ip.parse::<Ipv4Addr>().is_err() {
        // 不拦 ⇒ bind 成功、状态 listening，而每次连接在 dial_target 才失败
        // = 复现「监听中但连不上」
        return Some(format!(
            "目标地址非法：{:?}——本条未建立监听",
            r.target_ip
        ));
    }
    None
}

/// bind `127.0.0.1:{listen}`（F6 语义对齐 Go `net.Listen`）：`SOCK_CLOEXEC` +
/// `SO_REUSEADDR` + backlog 128 + 非阻塞；**单次尝试不重试**（重试语义在调用方）。
/// 地址**硬编码回环**（无配置面、无 `0.0.0.0` 退路——局域网不可达由绑定地址保证）。
pub(crate) fn pf_bind(listen: u16) -> io::Result<TcpListener> {
    use std::os::fd::AsRawFd as _;
    let fd = crate::sysfd::socket_cloexec(libc::AF_INET, libc::SOCK_STREAM, 0)?;
    let raw = fd.as_raw_fd();
    let one: libc::c_int = 1;
    if unsafe {
        libc::setsockopt(
            raw,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &one as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let mut sa: libc::sockaddr_in = unsafe { std::mem::zeroed() };
    #[cfg(target_os = "macos")]
    {
        sa.sin_len = std::mem::size_of::<libc::sockaddr_in>() as u8;
    }
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = listen.to_be();
    sa.sin_addr = libc::in_addr {
        s_addr: u32::from(Ipv4Addr::LOCALHOST).to_be(),
    };
    if unsafe {
        libc::bind(
            raw,
            &sa as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::listen(raw, LISTEN_BACKLOG) } != 0 {
        return Err(io::Error::last_os_error());
    }
    let ln = TcpListener::from(fd);
    ln.set_nonblocking(true)?;
    Ok(ln)
}

/// `poll(2)` 等监听 fd 可读（≤`timeout_ms`）；到点返回 `Ok(())`（由 `accept` 裁决）。
fn poll_readable(fd: std::os::fd::RawFd, timeout_ms: i32) -> io::Result<()> {
    let mut pfd = libc::pollfd {
        fd,
        events: libc::POLLIN,
        revents: 0,
    };
    let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
    if r < 0 {
        let e = io::Error::last_os_error();
        if e.kind() == io::ErrorKind::Interrupted {
            return Ok(()); // EINTR：下一轮
        }
        return Err(e);
    }
    Ok(())
}

/// accept 线程体（fd 单属本线程；退出 ack 由 [`ListenerExit`] 在 **fd 关闭之后**投出）。
fn pf_accept_thread(
    ctx: Arc<PfContext>,
    listen: u16,
    ln: TcpListener,
    st: Arc<PfState>,
    own_stop: Arc<AtomicBool>,
    ack: mpsc::Sender<()>,
    dst: PfDialTarget,
) {
    use std::os::fd::AsRawFd as _;
    let exit = ListenerExit {
        ln: Some(ln),
        ack: Some(ack),
        state: Arc::clone(&st),
    };
    let ln = exit.ln.as_ref().expect("监听器在场");
    let mut accept = move || -> io::Result<TcpStream> {
        poll_readable(ln.as_raw_fd(), ACCEPT_POLL_MS)?;
        let (c, _) = ln.accept()?;
        // BSD/macOS：accepted socket **继承**监听口的 `O_NONBLOCK`（darwin 实测）⇒
        // 必须显式转回阻塞，否则泵一读就 WouldBlock 收口（桥宿主 UDS 面同款整改）
        c.set_nonblocking(false)?;
        Ok(c)
    };
    accept_serve(&ctx, listen, &st, &own_stop, &dst, &mut accept);
}

/// accept 线程的退出守卫：drop 即「关 fd → 投 ack」（fd 已关 = 端口释放的可观测证据）；
/// panic 展开路径同样保证「ack 在 fd 之后」，并把该条状态转 `failed`——accept 线程异常
/// 退出与致命错误同族：端口已释放而状态还报 `listening` 就是谎报（代码门 r27 P2）。
struct ListenerExit {
    ln: Option<TcpListener>,
    ack: Option<mpsc::Sender<()>>,
    state: Arc<PfState>,
}

impl Drop for ListenerExit {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.state.mark_late_failed(
                "accept 线程异常退出——该条映射不可用（端口已释放）".into(),
            );
        }
        drop(self.ln.take());
        if let Some(tx) = self.ack.take() {
            let _ = tx.send(());
        }
    }
}

/// 可停 accept 循环（错误分类走共享件 `files_server::classify_accept_err`；`accept`
/// 可注入 ⇒ 「一次致命错误后状态转 failed」与「瞬态退避不退出」可测）。
fn accept_serve<A>(
    ctx: &Arc<PfContext>,
    listen: u16,
    st: &Arc<PfState>,
    own_stop: &AtomicBool,
    dst: &PfDialTarget,
    accept: &mut A,
) where
    A: FnMut() -> io::Result<TcpStream>,
{
    use crate::files_server::{classify_accept_err, AcceptAction};
    let mut backoff = Duration::from_millis(200);
    let mut transient: u64 = 0;
    loop {
        if ctx.gen_stop.load(Ordering::Acquire) || own_stop.load(Ordering::Acquire) {
            return;
        }
        match accept() {
            Ok(conn) => {
                backoff = Duration::from_millis(200);
                transient = 0;
                admit_conn(ctx, listen, st, conn, dst);
            }
            Err(e) => match classify_accept_err(&e) {
                // 非阻塞空转：`poll` 已等过一拍（这里再让 1ms 防「poll 假就绪」自旋）
                AcceptAction::Retry => std::thread::sleep(Duration::from_millis(1)),
                AcceptAction::Backoff => {
                    transient += 1;
                    if transient <= 3 || transient.is_multiple_of(100) {
                        (ctx.logf)(&format!(
                            "port-forward: accept 瞬态错误（{e}）——退避重试（累计 {transient} 次）"
                        ));
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(1));
                }
                AcceptAction::Fatal => {
                    // Go 留 `listening` 且 fd 不关；Rust 的 fd 单属本线程 ⇒ 退工即释放
                    // 端口 ⇒ 必须转 failed（否则又是「监听中但连不上」——登记为偏离 Go）
                    let text = format!("accept 致命错误（{e}）——该条映射不可用");
                    (ctx.logf)(&format!("port-forward: {text}"));
                    st.mark_late_failed(text);
                    (ctx.logf)(&format!(
                        "port-forward: 127.0.0.1:{listen} 监听已失效——该条状态转 failed（空码）"
                    ));
                    return;
                }
            },
        }
    }
}

/// 准入（**在 accept 线程内**，Go 形）：阀 → RAII `FlowGuard` → spawn conn 线程。
/// 阀先于线程创建 ⇒ 它同时是「在册流」与「线程创建」的上界（超限连接不进线程）。
fn admit_conn(
    ctx: &Arc<PfContext>,
    listen: u16,
    st: &Arc<PfState>,
    conn: TcpStream,
    dst: &PfDialTarget,
) {
    let n = ctx.counters.flows.fetch_add(1, Ordering::AcqRel) + 1;
    if n > ctx.limits.max_flows {
        ctx.counters.flows.fetch_sub(1, Ordering::AcqRel);
        let r = ctx.counters.flow_rejected.fetch_add(1, Ordering::Relaxed) + 1;
        if r <= 5 || r.is_multiple_of(50) {
            (ctx.logf)(&format!(
                "port-forward {listen}: 并发流已达上限 {}，拒绝（累计拒绝 {r}）",
                ctx.limits.max_flows
            ));
        }
        drop(conn); // 关连接（不回 RST、不计 fails）
        return;
    }
    // RAII：spawn 失败 ⇒ guard 在本线程 drop ⇒ conns/flows 自动回退（结构保证）
    let guard = FlowGuard::admit(&ctx.counters, st);
    let ctx2 = Arc::clone(ctx);
    let st2 = Arc::clone(st);
    let dst = *dst;
    #[cfg(test)]
    ctx.conn_spawns.fetch_add(1, Ordering::Relaxed);
    let spawned = std::thread::Builder::new()
        .name("homeway-pf-conn".into())
        .stack_size(PF_THREAD_STACK)
        .spawn(move || pf_conn_thread(ctx2, listen, st2, conn, guard, dst));
    if let Err(e) = spawned {
        (ctx.logf)(&format!(
            "port-forward: {listen} 连接处理线程启动失败（{e}）——该连接收口"
        ));
        // conn 随闭包丢弃关闭；guard 已随闭包丢弃 ⇒ 计数回退
    }
}

/// 准入守卫（RAII 承担资源不变量）：`conns -= 1` 与 `flows -= 1` 在最后一枚持有者
/// 结束时自动发生（泵线程各持一份 `Arc<FlowGuard>`——两向都收工才回退；`accepted`
/// 只增）。
struct FlowGuard {
    counters: Arc<PfCounters>,
    state: Arc<PfState>,
}

impl FlowGuard {
    /// 准入（`flows` 的 +1 已由阀判定完成；本件负责三条计数的**回退**与 accepted/conns 的 +1）。
    fn admit(counters: &Arc<PfCounters>, state: &Arc<PfState>) -> Self {
        state.conns.fetch_add(1, Ordering::Relaxed);
        counters.accepted.fetch_add(1, Ordering::Relaxed);
        FlowGuard {
            counters: Arc::clone(counters),
            state: Arc::clone(state),
        }
    }
}

impl Drop for FlowGuard {
    fn drop(&mut self) {
        self.state.conns.fetch_sub(1, Ordering::Relaxed);
        self.counters.flows.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 一条入站连接：裸拨（F2/D11——**不复用 `healing_dial`**，常态拒绝不许触发恢复阶梯）
/// → 成功则双向泵（复用 `bridge_host::pump`，EOF 零日志——对齐 Go `pkg/netpipe`）；
/// 失败 ⇒ `fails += 1` + **RST 收口**（`SO_LINGER(0)`；优雅 FIN 会让应用把「连上后
/// 立刻 EOF」当成响应结束而静默挂住）。
fn pf_conn_thread(
    ctx: Arc<PfContext>,
    listen: u16,
    st: Arc<PfState>,
    conn: TcpStream,
    guard: FlowGuard,
    dst: PfDialTarget,
) {
    match (ctx.dial)(dst.resolve(), ctx.budget) {
        Ok(remote) => {
            let halves = remote.into_halves();
            let local_r = conn.try_clone().ok();
            match (halves, local_r) {
                (Ok((mut remote_r, mut remote_w)), Some(mut local_r)) => {
                    let mut local_w = conn;
                    let guard = Arc::new(guard);
                    let g1 = Arc::clone(&guard);
                    let g2 = Arc::clone(&guard);
                    let logf1 = Arc::clone(&ctx.logf);
                    let logf2 = Arc::clone(&ctx.logf);
                    drop(guard);
                    let up = std::thread::Builder::new()
                        .name("homeway-pf-pump".into())
                        .stack_size(PF_THREAD_STACK)
                        .spawn(move || {
                            let _g = g1;
                            super::bridge_host::pump(
                                &mut local_r,
                                &mut *remote_w,
                                &logf1,
                                "port-forward",
                                "up",
                                false,
                            );
                        });
                    if let Err(e) = up {
                        (ctx.logf)(&format!(
                            "port-forward: {listen} 泵线程（up）启动失败（{e}）——该方向收口"
                        ));
                    }
                    let down = std::thread::Builder::new()
                        .name("homeway-pf-pump".into())
                        .stack_size(PF_THREAD_STACK)
                        .spawn(move || {
                            let _g = g2;
                            super::bridge_host::pump(
                                &mut *remote_r,
                                &mut local_w,
                                &logf2,
                                "port-forward",
                                "down",
                                false,
                            );
                        });
                    if let Err(e) = down {
                        (ctx.logf)(&format!(
                            "port-forward: {listen} 泵线程（down）启动失败（{e}）——该方向收口"
                        ));
                    }
                }
                (Err(_), _) => {
                    (ctx.logf)(&format!("port-forward: {listen} 转发流拆半失败——连接收口"));
                }
                (_, None) => {
                    (ctx.logf)(&format!("port-forward: {listen} 本地 fd 复制失败——连接收口"));
                }
            }
        }
        Err(e) => {
            let n = ctx.counters.fails.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 5 || n.is_multiple_of(20) {
                (ctx.logf)(&format!(
                    "port-forward: {listen} -> {} 拨号失败 #{n}: {e}",
                    st.target
                ));
            }
            crate::sysfd::rst_close_tcp(&conn);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};
    use std::net::Shutdown;
    use std::os::unix::net::UnixStream;
    use std::time::Instant;

    fn rule(listen: u16, ip: &str, port: u16) -> PortForwardRule {
        PortForwardRule {
            listen,
            target_ip: ip.to_owned(),
            target_port: port,
        }
    }

    fn log_lines() -> (Logf, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let l2 = Arc::clone(&lines);
        (
            Arc::new(move |m: &str| {
                l2.lock().unwrap().push(m.to_owned());
            }),
            lines,
        )
    }

    /// 探测空闲回环端口（**不钉固定端口**——同树并发跑测试会互撞）。
    fn free_port() -> u16 {
        let l = TcpListener::bind(("127.0.0.1", 0)).expect("探测端口可绑");
        l.local_addr().unwrap().port()
    }

    fn stub_dial_err() -> PfDialFn {
        Arc::new(|_dst, _b| Err(io::Error::new(io::ErrorKind::ConnectionRefused, "桩拒绝")))
    }

    fn runtime_with(dial: PfDialFn, limits: PfLimits, logf: Logf) -> Arc<PfRuntime> {
        Arc::new(PfRuntime::new(PfSetup {
            dial,
            stop: Arc::new(AtomicBool::new(false)),
            logf,
            budget: Duration::from_secs(5),
            limits,
        }))
    }

    fn wait_until(what: &str, mut f: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if f() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("等待超时：{what}");
    }

    fn has_line(lines: &Arc<Mutex<Vec<String>>>, needle: &str) -> bool {
        lines.lock().unwrap().iter().any(|l| l.contains(needle))
    }

    /// NAPI 整表校验的六条拒绝 + 通过形态（Go 循环体逐条对齐 + 条数上限）。
    #[test]
    fn validate_table_rejections() {
        assert_eq!(
            validate_table(&[rule(0, "", 80)]),
            Err(TableErr::ZeroPort { listen: 0, target_port: 80 })
        );
        assert_eq!(
            validate_table(&[rule(18080, "", 0)]),
            Err(TableErr::ZeroPort { listen: 18080, target_port: 0 })
        );
        assert_eq!(
            validate_table(&[rule(80, "", 8080)]),
            Err(TableErr::ListenRange(80))
        );
        assert_eq!(
            validate_table(&[rule(18080, "fe80::1", 80)]),
            Err(TableErr::BadTarget("fe80::1".into()))
        );
        assert_eq!(
            validate_table(&[rule(18080, "example.com", 80)]),
            Err(TableErr::BadTarget("example.com".into()))
        );
        assert_eq!(
            validate_table(&[rule(18080, "", 80), rule(18080, "", 81)]),
            Err(TableErr::DupListen(18080))
        );
        // 条数上限（> 8 ⇒ NAPI -2）
        let many: Vec<PortForwardRule> = (0..9).map(|i| rule(18080 + i, "", 80)).collect();
        assert_eq!(
            validate_table(&many),
            Err(TableErr::TooMany { n: 9, max: MAX_PF_RULES })
        );
        // 通过：出口自己 / IPv4 字面量 / 目标 <1024 合法（拨号不 bind——spec 只约束监听端口）
        assert!(validate_table(&[rule(18080, "", 22)]).is_ok());
        assert!(validate_table(&[rule(18080, "10.1.2.3", 80)]).is_ok());
        assert!(validate_table(&[]).is_ok());
        let eight: Vec<PortForwardRule> = (0..8).map(|i| rule(18080 + i, "", 80)).collect();
        assert!(validate_table(&eight).is_ok(), "8 条 = 上限内");
    }

    /// 目标文案三形态（FIX-46：port 0 不再原样呈现）。
    #[test]
    fn target_text_forms() {
        assert_eq!(pf_target_text(&rule(18080, "", 0)), "主机（同端口）");
        assert_eq!(pf_target_text(&rule(18080, "", 8080)), "主机:8080");
        assert_eq!(pf_target_text(&rule(18080, "1.2.3.4", 0)), "1.2.3.4:18080");
        assert_eq!(pf_target_text(&rule(18080, "1.2.3.4", 9999)), "1.2.3.4:9999");
    }

    /// 拨号目标派生（五形态）：空 IP / 环回 / 未指定 ⇒ `ExitPort`；其余 ⇒ `Remote`；
    /// `target_port=0` 在 IP 分支折 `listen`；非法 IP ⇒ 装配期拒。
    #[test]
    fn dial_target_semantics() {
        assert_eq!(rule(18080, "", 8080).dial_target().unwrap(), PfDialTarget::ExitPort(8080));
        assert_eq!(
            rule(18080, "", 0).dial_target().unwrap(),
            PfDialTarget::ExitPort(0),
            "空 IP + port 0 原样（Go pfDial 原样拨 TargetPort；NAPI 门拒 0 ⇒ 仅旁路可达）"
        );
        assert_eq!(
            rule(18080, "127.0.0.1", 8080).dial_target().unwrap(),
            PfDialTarget::ExitPort(8080),
            "环回 ⇒ 出口本机（Go 经出口过境重拨同效；本栈显式拒环回）"
        );
        assert_eq!(
            rule(18080, "127.9.8.7", 0).dial_target().unwrap(),
            PfDialTarget::ExitPort(18080),
            "环回 + port 0 ⇒ 同监听端口"
        );
        assert_eq!(
            rule(18080, "0.0.0.0", 8080).dial_target().unwrap(),
            PfDialTarget::ExitPort(8080),
            "未指定（0.0.0.0）⇒ 出口本机"
        );
        assert_eq!(
            rule(18080, "1.2.3.4", 0).dial_target().unwrap(),
            PfDialTarget::Remote("1.2.3.4:18080".parse().unwrap())
        );
        assert_eq!(
            rule(18080, "10.1.2.3", 9999).dial_target().unwrap(),
            PfDialTarget::Remote("10.1.2.3:9999".parse().unwrap())
        );
        assert_eq!(
            rule(18080, "example.com", 9999).dial_target(),
            Err(TableErr::BadTarget("example.com".into()))
        );
        // 解析（M4 §1.3 裁决 D-1）：`ExitPort` 落到**出口回环**——wire 上出现 `127.0.0.1:p`
        // 就是「出口本机」（WG 腿的 `SERVER_TUNNEL_IP` 替换在 `tun_exec::wg_dial_addr` 内）
        assert_eq!(
            PfDialTarget::ExitPort(8080).resolve(),
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 8080)
        );
        assert_eq!(
            PfDialTarget::ExitPort(0).resolve(),
            SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
            "端口 0 原样落 wire（出口 dial 腿的 A8 判定表第 11 类拒它）"
        );
        assert_eq!(
            PfDialTarget::Remote("10.1.2.3:9".parse().unwrap()).resolve(),
            "10.1.2.3:9".parse::<SocketAddrV4>().unwrap(),
            "Remote 原样（不经任何归一）"
        );
    }

    /// 失败态的 code = bind_failed（真 bind 失败）；`failed_with(None)` = 空码（不误归因）；
    /// 迟到失败（accept 致命）覆盖 state/err/code 三元组。
    #[test]
    fn state_codes_and_late_fail() {
        let ok = PfState::listening(18080, "主机:8080".into());
        assert_eq!(ok.snapshot().code, "");
        assert_eq!(ok.snapshot().state, "listening");
        let fail = PfState::failed(18081, "主机:80".into(), "Address already in use".into());
        let snap = fail.snapshot();
        assert_eq!(snap.code, "bind_failed");
        assert_eq!(snap.state, "failed");
        assert_eq!(snap.err, "Address already in use");

        let defensive =
            PfState::failed_with(18082, "主机:80".into(), "监听端口 80 不在 1024–65535（本机未建立监听）".into(), None);
        let snap = defensive.snapshot();
        assert_eq!(snap.state, "failed");
        assert_eq!(snap.code, "", "防御面空码（不假归因「端口被占用」）");
        assert!(snap.err.contains("本机未建立监听"));

        let late = PfState::listening(18083, "主机:80".into());
        assert_eq!(late.snapshot().state, "listening");
        late.mark_late_failed("accept 致命错误（Bad file descriptor）——该条映射不可用".into());
        let snap = late.snapshot();
        assert_eq!(snap.state, "failed", "迟到失败 ⇒ 不得再报 listening");
        assert_eq!(snap.code, "");
        assert!(snap.err.contains("accept 致命错误"));
        late.mark_late_failed("第二条原因".into());
        assert!(late.snapshot().err.contains("第一条原因") || late.snapshot().err.contains("accept"), "幂等：首个原因保留");
    }

    /// #1 真 bind：成功条 `listening`（端口真可连）、占用条 `failed(bind_failed)`。
    #[test]
    fn install_reports_per_entry_states() {
        let (logf, lines) = log_lines();
        let rt = runtime_with(stub_dial_err(), PfLimits::default(), logf);
        let ok_port = free_port();
        // 占住另一个端口（同机真占用 ⇒ EADDRINUSE）
        let squatter = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let busy_port = squatter.local_addr().unwrap().port();
        rt.install(&[
            rule(ok_port, "", 8080),
            rule(busy_port, "10.9.8.7", 9090),
        ]);
        let states = rt.snapshot_states();
        assert_eq!(states.len(), 2);
        assert_eq!(states[0].listen, ok_port);
        assert_eq!(states[0].state, "listening", "真监听");
        assert_eq!(states[0].code, "");
        assert_eq!(states[0].err, "");
        assert_eq!(states[0].target, "主机:8080");
        // 端口真可连（accept 面活着）
        let c = TcpStream::connect(("127.0.0.1", ok_port)).expect("监听口可连");
        drop(c);
        assert_eq!(states[1].listen, busy_port);
        assert_eq!(states[1].state, "failed");
        assert_eq!(states[1].code, "bind_failed", "真 bind 失败 = 真值");
        assert!(states[1].err.to_lowercase().contains("address already in use"), "{}", states[1].err);
        assert_eq!(states[1].target, "10.9.8.7:9090");
        // Go 行文（成功条 / 失败条）
        assert!(has_line(&lines, &format!("port-forward: 127.0.0.1:{ok_port} -> 主机:8080 监听中")));
        assert!(has_line(&lines, &format!("port-forward: 监听 127.0.0.1:{busy_port} 失败（")));
        assert!(has_line(&lines, "——该条映射不可用，不影响隧道"));
        // 收口：端口释放
        rt.stop_all();
        assert!(rt.snapshot_states().is_empty(), "收工后空表");
        let again = TcpListener::bind(("127.0.0.1", ok_port)).expect("端口已释放");
        drop(again);
        drop(squatter);
    }

    /// #2 原子性（确定性缝）：换入前读侧只见「全旧」，换入后「全新」——无半表/空表。
    #[test]
    fn install_atomic_single_swap() {
        let (logf, _lines) = log_lines();
        let p1 = free_port();
        let p2 = free_port();
        let p3 = free_port();
        let calls = Arc::new(AtomicU64::new(0));
        let (entered_tx, entered_rx) = mpsc::channel::<()>();
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let mut rt = PfRuntime::new(PfSetup {
            dial: stub_dial_err(),
            stop: Arc::new(AtomicBool::new(false)),
            logf: logf.clone(),
            budget: Duration::from_secs(5),
            limits: PfLimits::default(),
        });
        let c2 = Arc::clone(&calls);
        let r2 = Arc::clone(&release_rx);
        rt.inject_swap_seam(Some(Arc::new(move || {
            // 只在第二次 install（热替换）阻塞——直喂「换入前 = 全旧」
            if c2.fetch_add(1, Ordering::SeqCst) == 1 {
                let _ = entered_tx.send(());
                let _ = r2.lock().unwrap().recv();
            }
        })));
        let rt = Arc::new(rt);
        rt.install(&[rule(p1, "", 8080), rule(p2, "", 8081)]);
        let old = rt.snapshot_states();
        assert_eq!(old.len(), 2);
        let rt2 = Arc::clone(&rt);
        let h = std::thread::spawn(move || {
            rt2.install(&[rule(p1, "10.0.0.9", 9090), rule(p3, "", 8082)]);
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).expect("换入缝被调用");
        let mid = rt.snapshot_states();
        assert_eq!(mid.len(), 2, "换入窗口内**不得**空表（第三态）");
        assert_eq!(mid[0].listen, p1);
        assert_eq!(mid[1].listen, p2, "换入前 = 全旧");
        assert_eq!(mid[1].state, "listening");
        release_tx.send(()).unwrap();
        h.join().unwrap();
        let new = rt.snapshot_states();
        assert_eq!(new.len(), 2);
        assert_eq!(new[0].target, "10.0.0.9:9090", "换入后 = 全新");
        assert_eq!(new[1].listen, p3);
        rt.stop_all();
    }

    /// #3 热替换释放被复用的端口（ack 生效）：同端口替换成功 + 收工后端口可外部重绑。
    #[test]
    fn hot_replace_frees_reused_port() {
        let (logf, _lines) = log_lines();
        let rt = runtime_with(stub_dial_err(), PfLimits::default(), logf);
        let p = free_port();
        rt.install(&[rule(p, "", 8080)]);
        assert_eq!(rt.snapshot_states()[0].state, "listening");
        assert!(
            TcpListener::bind(("127.0.0.1", p)).is_err(),
            "监听期间外部不得重绑同端口"
        );
        // 同端口热替换（旧监听器 ack ⇒ fd 已关 ⇒ 重绑成功）
        rt.install(&[rule(p, "", 8081)]);
        let st = rt.snapshot_states();
        assert_eq!(st.len(), 1);
        assert_eq!(st[0].state, "listening");
        assert_eq!(st[0].target, "主机:8081");
        let c = TcpStream::connect(("127.0.0.1", p)).expect("替换后监听口仍可连");
        drop(c);
        // 收工之后端口回到外部可绑
        rt.stop_all();
        assert!(rt.snapshot_states().is_empty());
        let l = TcpListener::bind(("127.0.0.1", p)).expect("收工后端口可重绑");
        drop(l);
    }

    /// #3b ack 超时（注入「永不 ack」+ 真实占用端口）：3×50ms 重试 + **双记行**
    /// （Go 行文 + 「未在预算内释放」标注行——不许把内部竞态伪装成「端口被占用」）。
    #[test]
    fn install_ack_timeout_retries_then_marks() {
        let (logf, lines) = log_lines();
        let p = free_port();
        let mut rt = PfRuntime::new(PfSetup {
            dial: stub_dial_err(),
            stop: Arc::new(AtomicBool::new(false)),
            logf: logf.clone(),
            budget: Duration::from_secs(5),
            limits: PfLimits::default(),
        });
        // 注入「永不 ack」：直接判超时（不真等 400ms）
        rt.inject_wait_ack(Arc::new(|_rx, _b| AckOutcome::Timeout));
        let rt = Arc::new(rt);
        // 伪造一个「未在预算内退出」的旧监听器（stop 位被忽略 + 永不 ack）
        let stale_stop = Arc::new(AtomicBool::new(false));
        let (hold_tx, hold_rx) = mpsc::channel::<()>();
        let fake = PfListener {
            listen: p,
            stop: Arc::clone(&stale_stop),
            acked: hold_rx,
            handle: std::thread::spawn(|| std::thread::sleep(Duration::from_millis(400))),
        };
        {
            let mut inner = lock_unpoison(&rt.inner);
            inner.lns.push(fake);
            inner.states.push(Arc::new(PfState::listening(p, "主机（同端口）".into())));
        }
        // 真实占住端口（= 旧监听器确实没释放 fd 的等价形态）
        let squatter = TcpListener::bind(("127.0.0.1", p)).unwrap();
        let t0 = Instant::now();
        rt.install(&[rule(p, "", 8080)]);
        let elapsed = t0.elapsed();
        assert!(
            elapsed >= BIND_RETRY_GAP * BIND_RETRY_TIMES,
            "必须走完 3×50ms 重试（实耗 {elapsed:?}）"
        );
        assert!(stale_stop.load(Ordering::Acquire), "旧监听器已置停止位");
        let st = rt.snapshot_states();
        assert_eq!(st[0].state, "failed");
        assert_eq!(st[0].code, "bind_failed");
        assert!(
            has_line(&lines, &format!("port-forward: {p} 旧监听器未在预算内释放——已重试 bind 仍失败")),
            "重试仍失败要有标注行（双记行之一）"
        );
        assert!(
            has_line(&lines, &format!("port-forward: 监听 127.0.0.1:{p} 失败（")),
            "Go 行文（双记行之二）"
        );
        assert!(
            has_line(&lines, &format!("port-forward: 旧监听器 {p} 未在预算内退出——已在后台自退")),
            "detach 记行"
        );
        drop(hold_tx);
        drop(squatter);
        rt.stop_all();
    }

    /// #5 收工：states 清空、端口可重绑、线程在预算内退（ack 收到）。
    #[test]
    fn stop_all_clears_states_and_frees_ports() {
        let (logf, lines) = log_lines();
        let rt = runtime_with(stub_dial_err(), PfLimits::default(), logf);
        let p1 = free_port();
        let p2 = free_port();
        rt.install(&[rule(p1, "", 8080), rule(p2, "10.0.0.9", 8081)]);
        assert_eq!(rt.snapshot_states().len(), 2);
        rt.stop_all();
        assert!(rt.snapshot_states().is_empty(), "停止后状态清空（Go stopPortForwards 语义）");
        let l1 = TcpListener::bind(("127.0.0.1", p1)).expect("端口 1 释放");
        let l2 = TcpListener::bind(("127.0.0.1", p2)).expect("端口 2 释放");
        drop((l1, l2));
        assert!(
            has_line(&lines, "port-forward: 已停止全部监听器（2 个）"),
            "Go 行文"
        );
        // 幂等：再调不 panic、不重复记行
        let n_before = lines.lock().unwrap().len();
        rt.stop_all();
        assert_eq!(lines.lock().unwrap().len(), n_before, "幂等（无重复行）");
        assert!(rt.snapshot_states().is_empty());
    }

    /// #6 并发阀：`max_flows=2` ⇒ 第 3 连接被拒（且**未到拨号**= 未 spawn conn 线程）、
    /// `flow_rejected==1`、行文含「并发流已达上限 … 累计拒绝」、释放后 flows 归零。
    #[test]
    fn flow_valve_rejects_over_limit() {
        let (logf, lines) = log_lines();
        let entered = Arc::new(AtomicU64::new(0));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let e2 = Arc::clone(&entered);
        let r2 = Arc::clone(&release_rx);
        let dial: PfDialFn = Arc::new(move |_dst, _b| {
            e2.fetch_add(1, Ordering::AcqRel);
            let _ = r2.lock().unwrap().recv(); // 阻塞到测试放行
            Err(io::Error::new(io::ErrorKind::ConnectionRefused, "桩"))
        });
        let rt = runtime_with(
            dial,
            PfLimits {
                max_rules: 8,
                max_flows: 2,
            },
            logf,
        );
        let p = free_port();
        rt.install(&[rule(p, "", 8080)]);
        // 两条占满阀（拨号阻塞在途）
        let c1 = TcpStream::connect(("127.0.0.1", p)).unwrap();
        let c2 = TcpStream::connect(("127.0.0.1", p)).unwrap();
        wait_until("两条连接进入拨号", || entered.load(Ordering::Acquire) == 2);
        let st = rt.snapshot_states();
        assert_eq!(st[0].conns, 2, "conns 真值");
        // 第 3 连接：被阀拒绝（关连接、不 spawn、不计 fails）
        let mut c3 = TcpStream::connect(("127.0.0.1", p)).unwrap();
        c3.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 8];
        let r = c3.read(&mut buf);
        assert!(
            matches!(r, Ok(0)) || r.is_err(),
            "超限连接被关（非 RST 语义允许 EOF）: {r:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            entered.load(Ordering::Acquire),
            2,
            "超限连接不得进 conn 线程（阀在 spawn 之前）"
        );
        assert_eq!(
            rt.conn_thread_spawns(),
            2,
            "阀在 spawn 之前：第 3 条连接不得创建 conn 线程（spawn 计数缝）"
        );
        assert_eq!(rt.counters().flow_rejected(), 1);
        assert_eq!(rt.counters().accepted(), 2);
        assert!(
            has_line(&lines, "并发流已达上限 2，拒绝（累计拒绝 1）"),
            "Go 行文逐字（含「累计拒绝」）"
        );
        // 放行 ⇒ flows/conns 归零（拨号失败收口；两条在途拨号各取一枚——mpsc 单消费者）
        release_tx.send(()).unwrap();
        release_tx.send(()).unwrap();
        wait_until("flows 归零", || rt.counters().flows() == 0);
        wait_until("conns 归零", || rt.snapshot_states()[0].conns == 0);
        assert_eq!(rt.counters().fails(), 2, "两次拨号失败");
        drop((c1, c2, c3));
        rt.stop_all();
    }

    /// #7 拨号失败：对端 `read` = ECONNRESET（非 EOF）+ `accepted/fails` 真值 +
    /// `conns/flows` 归零 + 节流行文。
    #[test]
    fn dial_failure_rst_and_counters() {
        let (logf, lines) = log_lines();
        let rt = runtime_with(stub_dial_err(), PfLimits::default(), logf);
        let p = free_port();
        rt.install(&[rule(p, "", 8080)]);
        let mut c = TcpStream::connect(("127.0.0.1", p)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 8];
        match c.read(&mut buf) {
            Err(e) => assert_eq!(
                e.kind(),
                io::ErrorKind::ConnectionReset,
                "RST 收口（非优雅 FIN）: {e:?}"
            ),
            Ok(n) => panic!("拨号失败后不得回数据（读到 {n} 字节）"),
        }
        wait_until("计数归零", || {
            rt.counters().flows() == 0 && rt.snapshot_states()[0].conns == 0
        });
        assert_eq!(rt.counters().accepted(), 1);
        assert_eq!(rt.counters().fails(), 1);
        assert!(has_line(&lines, &format!("port-forward: {p} -> 主机:8080 拨号失败 #1: ")));
        drop(c);
        rt.stop_all();
    }

    /// 入站连接经泵转发到目标（**真 socket 往返**）：客户端 ↔（127.0.0.1 监听口）↔
    /// 拨号缝（UnixStream 对）↔ 测试侧「目标」；含半关闭双向传播与计数归零。
    #[test]
    fn inbound_roundtrip_through_pump() {
        let (logf, lines) = log_lines();
        let (tx, rx) = mpsc::channel::<UnixStream>();
        let dial: PfDialFn = Arc::new(move |_dst, _b| {
            let (a, b) = UnixStream::pair()?;
            let _ = tx.send(b);
            Ok(Box::new(a) as Box<dyn BridgeStream>)
        });
        let rt = runtime_with(dial, PfLimits::default(), logf);
        let p = free_port();
        rt.install(&[rule(p, "", 8080)]);
        let mut c = TcpStream::connect(("127.0.0.1", p)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut target = rx.recv_timeout(Duration::from_secs(3)).expect("拨号缝被调用");
        target
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        // 上行：客户端 → 目标
        c.write_all(b"ping-1").unwrap();
        let mut buf = [0u8; 6];
        target.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"ping-1");
        // 下行：目标 → 客户端
        target.write_all(b"pong-1").unwrap();
        let mut buf2 = [0u8; 6];
        c.read_exact(&mut buf2).unwrap();
        assert_eq!(&buf2, b"pong-1");
        // 半关闭：客户端关写端 ⇒ 上行 EOF ⇒ 对侧写端被 shutdown（目标读到 EOF）
        c.shutdown(Shutdown::Write).unwrap();
        let mut sink = [0u8; 8];
        assert_eq!(target.read(&mut sink).unwrap(), 0, "上行 EOF 传播到目标");
        // 目标关 ⇒ 下行 EOF 传播回客户端（darwin 上 UDS 的 shutdown 在「对端已 shutdown(WR)」
        // 形态可能回 ENOTCONN——尽力而为，drop 亦触发 EOF）
        let _ = target.shutdown(Shutdown::Both);
        drop(target);
        let n = c.read(&mut sink).unwrap_or(0);
        assert_eq!(n, 0, "下行 EOF 传播到客户端");
        wait_until("计数归零", || {
            rt.counters().flows() == 0 && rt.snapshot_states()[0].conns == 0
        });
        assert_eq!(rt.counters().accepted(), 1);
        assert_eq!(rt.counters().fails(), 0);
        assert!(
            !has_line(&lines, "port-forward[up] EOF") && !has_line(&lines, "port-forward[down] EOF"),
            "portfwd 侧 EOF 零日志（对齐 Go netpipe——浏览器关连接是常态）"
        );
        drop(c);
        rt.stop_all();
    }

    /// #4 热替换不打断已建立连接（D9：Go `ln.Close()` 不影响已 accept 的连接）。
    #[test]
    fn hot_replace_keeps_established_conns() {
        let (logf, _lines) = log_lines();
        let (tx, rx) = mpsc::channel::<UnixStream>();
        let dial: PfDialFn = Arc::new(move |_dst, _b| {
            let (a, b) = UnixStream::pair()?;
            let _ = tx.send(b);
            Ok(Box::new(a) as Box<dyn BridgeStream>)
        });
        let rt = runtime_with(dial, PfLimits::default(), logf);
        let p = free_port();
        rt.install(&[rule(p, "", 8080)]);
        let mut c = TcpStream::connect(("127.0.0.1", p)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut target = rx.recv_timeout(Duration::from_secs(3)).unwrap();
        target.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        c.write_all(b"before").unwrap();
        let mut buf = [0u8; 6];
        target.read_exact(&mut buf).unwrap();
        // 热替换（同端口，新目标）
        rt.install(&[rule(p, "", 8081)]);
        // 原连接双向仍通
        c.write_all(b"after!").unwrap();
        target.read_exact(&mut buf).unwrap();
        assert_eq!(&buf, b"after!");
        target.write_all(b"back!!").unwrap();
        let mut buf2 = [0u8; 6];
        c.read_exact(&mut buf2).unwrap();
        assert_eq!(&buf2, b"back!!");
        drop((c, target));
        rt.stop_all();
    }

    /// #9 旁路破损配置：`listen` 非法 / 超条数 / `target_ip` 非法 ⇒ **均不 bind**、
    /// `failed` + **空码** + 精确 err；其余条目照常监听。
    #[test]
    fn bypass_config_entries_are_failed_not_bound() {
        let (logf, lines) = log_lines();
        let rt = runtime_with(stub_dial_err(), PfLimits::default(), logf);
        let good_ports: Vec<u16> = (0..9).map(|_| free_port()).collect();
        let mut rules = vec![
            rule(0, "", 8080),
            rule(80, "", 8080),
            rule(good_ports[0], "example.com", 80),
            rule(good_ports[1], "::1", 80),
        ];
        rules.extend(good_ports.iter().map(|p| rule(*p, "", 8080)));
        assert_eq!(rules.len(), 13);
        rt.install(&rules);
        let st = rt.snapshot_states();
        assert_eq!(st.len(), 13, "每条都有状态（破损条目不受影响地逐条 failed）");
        // ① listen 非法
        assert_eq!(st[0].state, "failed");
        assert_eq!(st[0].code, "", "空码（不许假归因 bind_failed）");
        assert_eq!(st[0].err, "监听端口 0 不在 1024–65535（本机未建立监听）");
        assert_eq!(st[1].state, "failed");
        assert_eq!(st[1].err, "监听端口 80 不在 1024–65535（本机未建立监听）");
        // ② target_ip 非法（域名 / IPv6 字面量）
        assert_eq!(st[2].state, "failed");
        assert_eq!(st[2].code, "");
        assert_eq!(st[2].err, r#"目标地址非法："example.com"——本条未建立监听"#);
        assert_eq!(st[3].err, r#"目标地址非法："::1"——本条未建立监听"#);
        // ③ 前 4 条之后的 4 条合法（idx 4..8 在限内）
        for s in &st[4..8] {
            assert_eq!(s.state, "listening", "限内条目照常监听");
        }
        // ④ 超条数（idx >= 8）
        for s in &st[8..13] {
            assert_eq!(s.state, "failed");
            assert_eq!(s.code, "");
            assert!(s.err.contains("映射数超过上限（8）"), "{}", s.err);
        }
        assert_eq!(rt.lns_len(), 4, "只有限内合法条目真 bind（破损条目一条都没绑）");
        assert!(has_line(&lines, "监听端口 80 不在 1024–65535（本机未建立监听）"));
        assert!(has_line(&lines, "目标地址非法"));
        assert!(has_line(&lines, "映射数超过上限（8）——本条未建立监听"));
        rt.stop_all();
    }

    /// #10 `pf_bind` 语义：`SO_REUSEADDR` 位已设（getsockopt 回读）+ 连续两轮同端口可绑。
    #[test]
    fn bind_uses_reuseaddr_and_single_attempt() {
        use std::os::fd::AsRawFd as _;
        let p = free_port();
        let ln = pf_bind(p).expect("首次 bind");
        let mut v: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        assert_eq!(
            unsafe {
                libc::getsockopt(
                    ln.as_raw_fd(),
                    libc::SOL_SOCKET,
                    libc::SO_REUSEADDR,
                    &mut v as *mut _ as *mut libc::c_void,
                    &mut len,
                )
            },
            0
        );
        assert_ne!(
            v, 0,
            "SO_REUSEADDR 已设（Go net.Listen 默认同义；darwin 回读 = 位值 4，linux = 1）"
        );
        // 地址硬编码回环
        assert_eq!(ln.local_addr().unwrap().ip().to_string(), "127.0.0.1");
        // 占用期间第二次 bind 真失败（单次尝试、不重试）
        let e = pf_bind(p).expect_err("占用中 bind 必败");
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
        drop(ln);
        // 释放后可重绑（SO_REUSEADDR：TIME_WAIT 不挡）
        let ln2 = pf_bind(p).expect("第二轮 bind");
        drop(ln2);
    }

    /// #11b accept 致命错误（可注入）⇒ 记行 + 该条状态转 `failed`（空码 + 精确 err）。
    #[test]
    fn accept_fatal_marks_entry_failed() {
        let (logf, lines) = log_lines();
        let ctx = Arc::new(PfContext {
            counters: Arc::new(PfCounters::default()),
            dial: stub_dial_err(),
            logf,
            budget: Duration::from_secs(5),
            limits: PfLimits::default(),
            gen_stop: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            conn_spawns: AtomicU64::new(0),
        });
        let p = free_port();
        let st = Arc::new(PfState::listening(p, "主机（同端口）".into()));
        let mut accept =
            || Err::<TcpStream, io::Error>(io::Error::from_raw_os_error(libc::EBADF));
        accept_serve(
            &ctx,
            p,
            &st,
            &AtomicBool::new(false),
            &PfDialTarget::ExitPort(8080),
            &mut accept,
        );
        let snap = st.snapshot();
        assert_eq!(snap.state, "failed", "致命错误后不得再报 listening（fd 已释放）");
        assert_eq!(snap.code, "", "空码兜底（不是 bind 失败）");
        assert!(snap.err.contains("accept 致命错误"), "{}", snap.err);
        assert!(has_line(&lines, "accept 致命错误（") || has_line(&lines, "accept 致命错误"));
        assert!(has_line(&lines, "监听已失效——该条状态转 failed"));
    }

    /// accept 瞬态错误（EMFILE）⇒ 退避重试、**不退出**（下一条连接照常受理）。
    #[test]
    fn accept_transient_backs_off_and_continues() {
        let (logf, lines) = log_lines();
        let ctx = Arc::new(PfContext {
            counters: Arc::new(PfCounters::default()),
            dial: stub_dial_err(),
            logf,
            budget: Duration::from_secs(5),
            limits: PfLimits::default(),
            gen_stop: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            conn_spawns: AtomicU64::new(0),
        });
        let p = free_port();
        let st = Arc::new(PfState::listening(p, "主机（同端口）".into()));
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let mut n = 0;
        let mut accept = move || -> io::Result<TcpStream> {
            n += 1;
            match n {
                1 => Err(io::Error::from_raw_os_error(libc::EMFILE)),
                _ => {
                    stop2.store(true, Ordering::Release); // 第二拍起收工（不真接连接）
                    Err(io::Error::from(io::ErrorKind::WouldBlock))
                }
            }
        };
        accept_serve(
            &ctx,
            p,
            &st,
            &stop,
            &PfDialTarget::ExitPort(8080),
            &mut accept,
        );
        assert_eq!(st.snapshot().state, "listening", "瞬态错误不改变映射状态");
        assert!(has_line(&lines, "accept 瞬态错误"));
    }

    /// F1-2 锁纪律：持锁线程 panic（毒锁）后 `stop_all` 仍能收工（`lock_unpoison`
    /// 不 panic——`Finish::drop` 可能取到毒锁），且端口真被释放。
    #[test]
    fn poisoned_lock_still_stops_all() {
        let (logf, _lines) = log_lines();
        let rt = runtime_with(stub_dial_err(), PfLimits::default(), logf);
        let p = free_port();
        rt.install(&[rule(p, "", 8080)]);
        assert_eq!(rt.snapshot_states()[0].state, "listening");
        // 毒化 inner 锁（持锁线程 panic 的等价形态）
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // 静音预期的注入 panic
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _g = lock_unpoison(&rt.inner);
            panic!("毒锁注入");
        }));
        std::panic::set_hook(prev);
        assert!(poisoned.is_err(), "前置：锁已毒化");
        assert!(rt.inner.is_poisoned(), "前置：真毒锁");
        rt.stop_all(); // 不得 panic
        assert!(rt.snapshot_states().is_empty());
        let l = TcpListener::bind(("127.0.0.1", p)).expect("毒锁后端口仍被释放");
        drop(l);
    }

    /// P2（代码门 r27）：accept 线程**异常退出**（panic 展开）⇒ `ListenerExit` 在
    /// 关 fd 的同时把该条状态转 `failed`（空码）——「端口已释放而状态还报 listening」
    /// 的最后一处谎报面收口。
    #[test]
    fn listener_exit_marks_late_fail_on_panic() {
        let p = free_port();
        let st = Arc::new(PfState::listening(p, "主机（同端口）".into()));
        let ln = pf_bind(p).expect("bind");
        let (tx, _rx) = mpsc::channel::<()>();
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(|_| {})); // 静音预期的注入 panic
        let st2 = Arc::clone(&st);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _g = ListenerExit {
                ln: Some(ln),
                ack: Some(tx),
                state: st2,
            };
            panic!("accept 线程异常退出注入");
        }));
        std::panic::set_hook(prev);
        assert!(caught.is_err(), "前置：线程「异常退出」");
        let snap = st.snapshot();
        assert_eq!(snap.state, "failed", "异常退出后不得再报 listening");
        assert_eq!(snap.code, "", "空码兜底（不是 bind 失败）");
        assert!(snap.err.contains("accept 线程异常退出"), "{}", snap.err);
        // fd 与 ack 顺序：端口已释放（fd 已关）
        let again = pf_bind(p).expect("异常退出后端口已释放");
        drop(again);
    }

    /// 状态构造函数（spawn 失败 / 防御面 / 迟到失败）——断言的纯函数缝（spawn 失败
    /// 不可注入，按 Q-F 口径抽构造面直喂；生产路径由 install 的对应分支调用）。
    #[test]
    fn defensive_state_constructors() {
        let s = PfState::failed_with(
            18080,
            "主机（同端口）".into(),
            "监听线程启动失败（Resource temporarily unavailable）——该条映射不可用，不影响隧道".into(),
            None,
        );
        let snap = s.snapshot();
        assert_eq!(snap.state, "failed");
        assert_eq!(snap.code, "", "线程资源失败 ≠ 端口被占用");
        assert!(snap.err.contains("监听线程启动失败"));
        assert_eq!(
            defensive_err(&rule(1500, "", 80), 0, MAX_PF_RULES),
            None,
            "合法条目无防御错误"
        );
        assert!(defensive_err(&rule(1023, "", 80), 0, MAX_PF_RULES).is_some());
        assert!(defensive_err(&rule(18080, "", 80), 8, MAX_PF_RULES).is_some());
        assert!(defensive_err(&rule(18080, "1.2.3.4", 80), 0, MAX_PF_RULES).is_none());
    }
}
