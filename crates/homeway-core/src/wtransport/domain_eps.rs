//! token 端点 → 建连候选（IP 字面量 / 域名两种形态）+ 域名重解析编排。
//! 语义真源 `baseline:clientcore/hostsession/session_endpoints.go`（splitTokenEndpoints /
//! resolveCandidates / domainEndpointPorts）+ `wgcore/transport.go`（refreshDomain*
//! 家族——判据行逐串对齐）。
//!
//! 为什么需要：token 的端点在协议里允许写域名（Endpoint.Addr 按 `host:port` 放行），
//! 但建连候选必须是 `IP:port`（Bind 直接用它做 UDP 目标）。只 ParseAddrPort 会把
//! 带域名的 token 整体判成「没有任何可用端点」（真机 2026-09-19 实测：某出口用
//! 域名签发，手机侧起不来隧道）。
//!
//! 语义：域名在**每次建会话时**解析一次（A + AAAA：隧道内层只承载 IPv4，但外层
//! WG socket 可以是 IPv6——出口若公布域名或 v6 地址，客户端要把 AAAA 也当候选）；
//! 解析出的每个地址都作为独立候选参与赛跑。解析失败只记一行、跳过该端点（其它
//! 端点照常）；全部失败才算「没有可用端点」。
//!
//! 重解析（`DomainRefresher`）：Rearm/RearmSoft 时**并发**另跑（恢复阶梯的动作
//! 预算是 2s 有界，塞进 5s DNS 预算会把蜂窝下常态 DNS 空等打成 rc=-3 误升整套
//! 重建——Go endpoint-freshness D5 同理）；解析结果到达后补投两分支：赛跑未结算
//! → 候选重投即可；已结算且落中继 → 经节流的软赛跑让新候选立刻获得一次赛跑机会。

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::token::{EndpointKind, EndpointRef};
use crate::wtransport::bind::{same_candidates, Candidate};
use crate::Logf;

/// 候选应用回调（新候选集已并入静态面并重投给 Bind）。
pub type ApplyStaticFn = Arc<dyn Fn(&[Candidate]) + Send + Sync>;
/// 当前采纳是否中继（Some(addr) = 中继采纳地址；None = 非/未采纳——不补投）。
pub type IsRelayFn = Arc<dyn Fn() -> Option<SocketAddr> + Send + Sync>;
/// 软赛跑触发（RearmSoft 同义）。
pub type SoftRearmFn = Arc<dyn Fn() + Send + Sync>;

/// 域名建会话解析预算（Go lookupIPv4 的 5s 同值）。
pub const RESOLVE_BUDGET: Duration = Duration::from_secs(5);
/// 重解析异步预算（Go refreshDomainAsync 的 5s 同值）。
const REFRESH_BUDGET: Duration = Duration::from_secs(5);
/// 旁路探测拍的同步重解析预算（Go ProbeCandidates 的 3s 同值）。
pub const PROBE_SYNC_BUDGET: Duration = Duration::from_secs(3);
/// 「赛跑已结算 + 中继获胜」时补投软赛跑的节流（Go 15s 同值）。
const SOFT_REARM_THROTTLE: Duration = Duration::from_secs(15);

/// 域名形态的 token 端点（重解析输入）。Relay 必须随端口一起携带（FIX-14 同义）：
/// token 里写中继域名时，重解析产出的候选若不标 Relay，会按直连裸发——中继只认
/// 腿帧路由头，该腿静默失效。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DomainEndpoint {
    pub host: String,
    pub port: u16,
    pub relay: bool,
}

/// 域名解析的**档位**（Q-F F8e 分档令牌池）：关键路径（建会话 / daemon `host reach`
/// 用户可见结论）与后台刷新（巡检）分池——避免后台刷新把用户可见路径饿死。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveLane {
    /// 建会话 `split_and_resolve` + daemon `host reach`（预算 3.5s 是 spec MUST）。
    Critical,
    /// 巡检 `refresh_sync`/`refresh_async`（后台，可等）。
    Background,
}

/// 各档额度（合计 8）：域名条目典型 ≤2–4 ⇒ 关键路径必须**永远拿得到**额度。
const CRITICAL_QUOTA: usize = 4;
const BACKGROUND_QUOTA: usize = 4;

/// 在飞解析上限（分档令牌池；Q-F F8e）。
///
/// 形态：**类型**（可注入/可替换）+ 进程级单例（[`process_limiter`]）。`acquire` 阻塞
/// 等待上限 = **调用方预算**（拿不到 ⇒ 既有 `TimedOut` 形态 + 明确文案）；**获取耗时
/// 计进调用方预算**（调用方在 `lookup_host` 里扣减后再喂 worker 期限，否则总耗时
/// = 2× 预算——设计门 D4）。
///
/// 残余（如实登记）：DNS 黑洞下 critical 档 4 枚卡死线程可被占满 ⇒ 退化为「**有界地
/// 失败**」（该轮解析超时），不是「不会失效」。
#[derive(Debug, Default)]
pub struct ResolverLimiter {
    /// Arc 形态：**名额守卫必须能 move 进 worker 线程**（额度 = 在飞解析）⇒ `Permit`
    /// 不借外层（无生命周期参数）；`ResolverLimiter` 自身也可廉价克隆/注入。
    inner: Arc<LimiterInner>,
}

#[derive(Default, Debug)]
struct LimiterInner {
    state: Mutex<LaneState>,
    cv: Condvar,
}

#[derive(Default, Debug)]
struct LaneState {
    critical: usize,
    background: usize,
}

impl ResolverLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    fn quota(lane: ResolveLane) -> usize {
        match lane {
            ResolveLane::Critical => CRITICAL_QUOTA,
            ResolveLane::Background => BACKGROUND_QUOTA,
        }
    }

    fn field(state: &mut LaneState, lane: ResolveLane) -> &mut usize {
        match lane {
            ResolveLane::Critical => &mut state.critical,
            ResolveLane::Background => &mut state.background,
        }
    }

    /// 当前在飞数（诊断/测试用）。
    pub fn inflight(&self, lane: ResolveLane) -> usize {
        let mut st = crate::syncutil::lock_unpoison(&self.inner.state);
        *Self::field(&mut st, lane)
    }

    /// 阻塞获取一个解析名额（等待上限 = `budget`；拿不到 ⇒ `TimedOut`）。
    /// 返回的 [`Permit`] 可 move 进 worker 线程（额度 = 在飞解析）。
    pub fn acquire(&self, lane: ResolveLane, budget: Duration) -> std::io::Result<Permit> {
        let quota = Self::quota(lane);
        let deadline = Instant::now() + budget;
        let mut st = crate::syncutil::lock_unpoison(&self.inner.state);
        loop {
            let used = *Self::field(&mut st, lane);
            if used < quota {
                *Self::field(&mut st, lane) = used + 1;
                return Ok(Permit {
                    limiter: Arc::clone(&self.inner),
                    lane,
                });
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("域名解析并发已达上限（{quota}），本次等待超时"),
                ));
            }
            let (g, _to) = self
                .inner
                .cv
                .wait_timeout(st, left)
                .unwrap_or_else(|e| e.into_inner());
            st = g;
        }
    }
}

/// 名额守卫（Drop 归还 + 唤醒等待者；`'static` 可 move 进 worker 线程）。
#[derive(Debug)]
pub struct Permit {
    limiter: Arc<LimiterInner>,
    lane: ResolveLane,
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut st = crate::syncutil::lock_unpoison(&self.limiter.state);
        let slot = ResolverLimiter::field(&mut st, self.lane);
        *slot = slot.saturating_sub(1);
        drop(st);
        self.limiter.cv.notify_all();
    }
}

/// 进程级单例（缺省归属；池本身是类型 ⇒ 可注入替换）。
pub fn process_limiter() -> &'static ResolverLimiter {
    static LIMITER: std::sync::OnceLock<ResolverLimiter> = std::sync::OnceLock::new();
    LIMITER.get_or_init(ResolverLimiter::new)
}

/// 【test-seams】复位进程级池（集成测试看不到 `cfg(test)`——设计门 D4 口径）。
#[cfg(feature = "test-seams")]
pub fn debug_reset_process_limiter() {
    let l = process_limiter();
    let mut st = crate::syncutil::lock_unpoison(&l.inner.state);
    *st = LaneState::default();
    drop(st);
    l.inner.cv.notify_all();
}

/// 域名 → IPv4/IPv6 地址列表（系统解析器，`budget` 预算——建会话被 DNS 卡住的上界；
/// Go net.DefaultResolver 等价）。返回顺序 **v4 在前**（同网段/兼容性更好的先试）。
///
/// Q-F F8e：入口先取分档令牌（阻塞上限 = 调用方预算）；**获取耗时计进同一预算**
/// （worker 期限按剩余重设——否则总耗时 = 2× 预算）。
///
/// **额度归属 = worker（在飞解析）**：令牌随 worker 闭包走，调用方超时返回**不**归还
/// 额度——黑洞下卡在 `getaddrinfo` 里的线程仍占额度 ⇒ 该档位到上限后**有界地失败**
/// （第 N+1 个调用等到预算耗尽即回 TimedOut），这才是「在飞解析上限」（审计条目 14
/// 的根因：超时 detach 线程无上界）。worker 正常返回 ⇒ 额度随闭包结束归还。
pub fn lookup_host(host: &str, budget: Duration, lane: ResolveLane) -> std::io::Result<Vec<IpAddr>> {
    lookup_host_with(process_limiter(), host, budget, lane, resolve_system)
}

/// 真解析（系统解析器；port 0：只为触发名字解析，端口不参与）。
fn resolve_system(host: &str) -> std::io::Result<Vec<IpAddr>> {
    use std::net::ToSocketAddrs as _;
    (host, 0)
        .to_socket_addrs()
        .map(|it| it.map(|a| a.ip()).collect::<Vec<_>>())
}

/// `lookup_host` 的可注入主体（Q-F F8e 测试缝，代码门 r15 新增 2）：池与解析器
/// 都可替换 ⇒ 单测能真钉「额度随 worker 生命周期」（调用方超时后仍占额度）。
fn lookup_host_with<R>(
    limiter: &ResolverLimiter,
    host: &str,
    budget: Duration,
    lane: ResolveLane,
    resolve: R,
) -> std::io::Result<Vec<IpAddr>>
where
    R: Fn(&str) -> std::io::Result<Vec<IpAddr>> + Send + 'static,
{
    let t0 = Instant::now();
    let permit = limiter.acquire(lane, budget)?;
    let remaining = budget.saturating_sub(t0.elapsed());
    if remaining.is_zero() {
        // 归因区分两态（代码门 r15 新增 6）：零预算进入（daemon reach 的
        // `saturating_sub` 可能给到 0）≠ 等待把预算耗尽
        let why = if budget.is_zero() {
            "域名解析预算为零（调用方未给预算）".to_owned()
        } else {
            format!("域名解析预算已被并发等待耗尽（预算 {budget:?}）")
        };
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, why));
    }
    // getaddrinfo 无原生超时——worker 线程跑真解析、按期限等（dnsq 同款壳；
    // 超时 detach 线程：它在 getaddrinfo 里自生自灭，进程退出由 OS 收；**额度
    // 由它继续持有**直到真返回——见上）。
    let (tx, rx) = std::sync::mpsc::channel::<std::io::Result<Vec<IpAddr>>>();
    let host_owned = host.to_owned();
    let th = std::thread::Builder::new()
        .name("hw-domlook".to_owned())
        .stack_size(256 * 1024)
        .spawn(move || {
            // 额度随 worker 生命周期（超时 detach 的 worker 仍占额度——有界在飞上限）
            let _permit = permit;
            let r = resolve(&host_owned);
            let _ = tx.send(r);
        })
        .map_err(|e| std::io::Error::other(format!("解析线程建立失败：{e}")))?;
    match rx.recv_timeout(remaining) {
        Ok(r) => {
            let _ = th.join();
            r.map(|ips| {
                let mut v4 = Vec::new();
                let mut v6 = Vec::new();
                for ip in ips {
                    match ip {
                        IpAddr::V4(_) => v4.push(ip),
                        IpAddr::V6(v6a) if v6a.to_ipv4_mapped().is_none() => v6.push(ip),
                        // v4-mapped v6：按 Go Unmap 口径归 v4 语义面（这里映射回 v4）。
                        IpAddr::V6(mapped) => v4.push(IpAddr::V4(mapped.to_ipv4_mapped().unwrap())),
                    }
                }
                v4.extend(v6);
                v4
            })
        }
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("域名解析超时（预算 {budget:?}）"),
        )),
    }
}

/// 建会话时的端点展开产物。
pub struct EndpointInputs {
    /// 全部候选（IP 字面量 + 域名首解析产物，按 token 序）。
    pub candidates: Vec<Candidate>,
    /// 域名条目原文（重解析输入；空 = 无域名面）。
    pub domains: Vec<DomainEndpoint>,
    /// **静态基座**（仅 IP 字面量——Go `staticBase` 同义：永不变、重建世代沿用）。
    /// 静态面 = static_base + domain_initial / 最新域名解析产物（重解析只替换域名组
    /// ——评审 4.2/4.3：按条目类型分组而非地址反推，字面量永不被域名解析结果挤出，
    /// 域名 IP 漂移后旧地址随组替换退场）。
    pub static_base: Vec<Candidate>,
    /// 域名首解析产物（与 static_base 分组各自独立去重——Go staticBase/domainCands
    /// 分离同构）。
    pub domain_initial: Vec<Candidate>,
}

/// 把 token 端点展开成候选（`resolveCandidates` + `domainEndpointPorts` 合一）。
/// 判据行同串：
/// - 「token 端点 %q 域名解析失败（跳过）：%v」
/// - 「token 端点 %q 端口非法（跳过）」（域名字面量端口非 1-65535）
/// - 「token 端点 %s 解析为 %d 个地址（%s）」
///
/// **入参须已按 transport 过滤**（M1 §2.1 末段「两族候选不得互相投喂」）：WG 面的调用方
/// 传 `token::wg_endpoint_refs(...)`（滤掉 QUIC 类端点——QUIC 端口与 WG 端口不同，§1.1，
/// 当 WG 候选只会产出「握手超时」噪声与赛跑结算失真）；QUIC 档的候选由 S2 的岛按
/// `kind==Quic` 取。过滤放在 `token` 侧（本文件属 `wtransport/**`，M1 S1c 的红线面）。
pub fn split_and_resolve(eps: &[EndpointRef<'_>], logf: &Logf) -> EndpointInputs {
    let mut out = Vec::with_capacity(eps.len());
    let mut seen: Vec<SocketAddr> = Vec::new(); // 全局去重（token 内跨组同址：先到先得）
    let mut seen_static: Vec<SocketAddr> = Vec::new();
    let mut seen_domain: Vec<SocketAddr> = Vec::new();
    let mut domains = Vec::new();
    let mut static_base = Vec::new();
    let mut domain_initial = Vec::new();
    let mut add = |ap: SocketAddr, relay: bool, into_domain: bool| {
        if !seen.contains(&ap) {
            seen.push(ap);
            let group_seen = if into_domain { &mut seen_domain } else { &mut seen_static };
            let c = Candidate { addr: ap, relay };
            if group_seen.contains(&ap) {
                // 全局未见但组内已见（跨组同址的第二次出现）：只进 out（两个组
                // 各自保留一份——静态组保字面量、域名组保解析产物）。
                out.push(c);
            } else {
                group_seen.push(ap);
                match into_domain {
                    true => domain_initial.push(c),
                    false => static_base.push(c),
                }
                out.push(c);
            }
        }
    };
    for ep in eps {
        let relay = ep.kind == EndpointKind::Relay;
        if let Ok(ap) = ep.addr.parse::<SocketAddr>() {
            add(ap, relay, false);
            continue;
        }
        let (host, port) = match split_host_port(ep.addr) {
            Some(v) => v,
            None => {
                (logf)(&format!("token 端点 {:?} 端口非法（跳过）", ep.addr));
                continue;
            }
        };
        // 域名条目**先入表再解析**（评审 4.1：解析失败也必须保留原文——Go
        // `domainEndpointPorts` 只依赖 SplitHostPort 成功，与解析结果无关；
        // DDNS 动态 IP + 建会话时 DNS 抖一下的场景靠重解析自愈，丢条目 = 这一代
        // 会话永不再解析该域名）。
        domains.push(DomainEndpoint { host: host.clone(), port, relay });
        match lookup_host(&host, RESOLVE_BUDGET, ResolveLane::Critical) {
            Err(e) => {
                (logf)(&format!("token 端点 {:?} 域名解析失败（跳过）：{e}", ep.addr));
                continue;
            }
            Ok(ips) if ips.is_empty() => {
                (logf)(&format!("token 端点 {:?} 域名解析失败（跳过）：无地址", ep.addr));
                continue;
            }
            Ok(ips) => {
                for ip in &ips {
                    add(SocketAddr::new(*ip, port), relay, true);
                }
                (logf)(&format!(
                    "token 端点 {} 解析为 {} 个地址（{:?}）",
                    ep.addr,
                    ips.len(),
                    ips
                ));
            }
        }
    }
    EndpointInputs { candidates: out, domains, static_base, domain_initial }
}

/// 重解析一拍：解析全部域名条目 → 候选集（`refreshDomainLocked` 的解析段）。
/// 任一条目失败 → None（**退回上次解析结果**——Go 同语义：本函数直接返回不改）；
/// 全部成功但零候选 → None。判据行同串：
/// - 「域名重解析 %s 失败（退回上次解析结果）：%v」
pub fn resolve_domains(
    eps: &[DomainEndpoint],
    budget: Duration,
    logf: &Logf,
) -> Option<Vec<Candidate>> {
    let mut fresh = Vec::new();
    for de in eps {
        match lookup_host(&de.host, budget, ResolveLane::Background) {
            Err(e) => {
                (logf)(&format!("域名重解析 {} 失败（退回上次解析结果）：{e}", de.host));
                return None;
            }
            Ok(ips) => {
                for ip in ips {
                    fresh.push(Candidate {
                        addr: SocketAddr::new(ip, de.port),
                        relay: de.relay,
                    });
                }
            }
        }
    }
    if fresh.is_empty() {
        return None;
    }
    Some(fresh)
}

/// 域名重解析编排器（每会话一个；Rearm/RearmSoft/旁路探测拍触发）。
/// 回调面由消费层注入（两消费层 session/facade 结构不同，不做共享依赖）：
/// - `apply_static`：新候选集已并入静态面并重投给 Bind（SetCandidates 同义）；
/// - `is_relay`：当前采纳是否中继（Some(addr) = 中继；None/非中继 = 不补投）；
/// - `soft_rearm`：软赛跑（RearmSoft 同义）。
pub struct DomainRefresher {
    inner: Arc<RefInner>,
}

struct RefInner {
    eps: Vec<DomainEndpoint>,
    inflight: AtomicBool,
    last_soft_rearm: Mutex<Option<Instant>>,
    last_cands: Mutex<Vec<Candidate>>,
    logf: Logf,
    apply_static: ApplyStaticFn,
    is_relay: IsRelayFn,
    soft_rearm: SoftRearmFn,
}

impl DomainRefresher {
    pub fn new(
        eps: Vec<DomainEndpoint>,
        initial: Vec<Candidate>,
        logf: Logf,
        apply_static: ApplyStaticFn,
        is_relay: IsRelayFn,
        soft_rearm: SoftRearmFn,
    ) -> Self {
        Self {
            inner: Arc::new(RefInner {
                eps,
                inflight: AtomicBool::new(false),
                last_soft_rearm: Mutex::new(None),
                last_cands: Mutex::new(initial),
                logf,
                apply_static,
                is_relay,
                soft_rearm,
            }),
        }
    }

    /// 域名面是否为空（无域名条目 = 调用方可整个跳过）。
    pub fn is_empty(&self) -> bool {
        self.inner.eps.is_empty()
    }

    /// 同步重解析一拍（旁路探测拍用——Go ProbeCandidates 的 3s 等待有界形态）。
    /// 返回 fresh 候选（失败/零候选 = None，退回上次结果由调用方保持现状）；
    /// **不 apply**（是否入静态面由调用方决定）。
    pub fn refresh_sync(&self, budget: Duration) -> Option<Vec<Candidate>> {
        let fresh = resolve_domains(&self.inner.eps, budget, &self.inner.logf)?;
        let mut last = self.inner.last_cands.lock().expect("重解析锁中毒");
        if !same_candidates(&fresh, &last) {
            *last = fresh.clone();
        }
        Some(fresh)
    }

    /// 异步重解析（Rearm/RearmSoft 触发；单飞防重入——Go refreshDomainAsync 同义）。
    pub fn refresh_async(self: &Arc<Self>) {
        if self.inner.eps.is_empty() || !self.set_inflight() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        match std::thread::Builder::new()
            .name("hw-domrefresh".to_owned())
            .stack_size(256 * 1024)
            .spawn(move || {
                let _guard = InflightGuard(&inner);
                let Some(fresh) = resolve_domains(&inner.eps, REFRESH_BUDGET, &inner.logf)
                else {
                    return; // 失败已记行；退回上次结果
                };
                let changed = {
                    let mut last = inner.last_cands.lock().expect("重解析锁中毒");
                    let changed = !same_candidates(&fresh, &last);
                    if changed {
                        *last = fresh.clone();
                    }
                    changed
                };
                (inner.apply_static)(&fresh);
                if !changed {
                    return;
                }
                (inner.logf)(&format!(
                    "域名重解析：{} 条候选已刷新（{}）",
                    fresh.len(),
                    describe_candidates(&fresh)
                ));
                // 补投两分支（评审 B-3）：已结算且落中继 → 经节流的软赛跑让新候选
                // 立刻获得一次赛跑机会（否则要等 RELAY-UPGRADE 5 拍 × 60s）。
                if let Some(addr) = (inner.is_relay)() {
                    let due = {
                        let mut l = inner.last_soft_rearm.lock().expect("软赛跑节流锁中毒");
                        let due = l.map(|t| t.elapsed() >= SOFT_REARM_THROTTLE).unwrap_or(true);
                        if due {
                            *l = Some(Instant::now());
                        }
                        due
                    };
                    if due {
                        (inner.logf)(&format!(
                            "域名重解析晚于赛跑结算（当前中继 {addr}）→ 节流软赛跑补投新候选"
                        ));
                        (inner.soft_rearm)();
                    }
                }
            }) {
            Ok(_) => {}
            Err(e) => {
                // 评审 4.7 整改：spawn 失败必须复位单飞位——否则一次线程建立失败后
                // 本会话所有重解析静默关闭（三条触发全失效且无日志）。
                self.inner.inflight.store(false, Ordering::SeqCst);
                (self.inner.logf)(&format!("域名重解析线程建立失败（{e}）——本轮跳过"));
            }
        }
    }

    fn set_inflight(&self) -> bool {
        self.inner
            .inflight
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }
}

struct InflightGuard<'a>(&'a RefInner);

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.0.inflight.store(false, Ordering::SeqCst);
    }
}

/// 候选描述串（link 行 `ep=…` 的候选简述同族形态）。
fn describe_candidates(c: &[Candidate]) -> String {
    c.iter()
        .map(|c| {
            if c.relay {
                format!("{}（中继）", c.addr)
            } else {
                c.addr.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("、")
}

/// `host:port` 拆分（端口 1-65535;`[v6]:port` 形态由 SocketAddr 解析先行兜住,
/// 这里只剩域名形态）。None = 端口非法。（hosts 的 reach 探测共用——pub 别名。）
pub fn split_host_port_pub(s: &str) -> Option<(String, u16)> {
    split_host_port(s)
}

fn split_host_port(s: &str) -> Option<(String, u16)> {
    let idx = s.rfind(':')?;
    let port: u16 = s[idx + 1..].parse().ok()?;
    if port == 0 {
        return None;
    }
    Some((s[..idx].to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem_logf() -> (Logf, Arc<Mutex<Vec<String>>>) {
        let v = Arc::new(Mutex::new(Vec::new()));
        let v2 = Arc::clone(&v);
        let f: Logf = Arc::new(move |s: &str| v2.lock().unwrap().push(s.to_owned()));
        (f, v)
    }

    #[test]
    fn host_port_split() {
        assert_eq!(
            split_host_port("home.example.com:41641"),
            Some(("home.example.com".into(), 41641))
        );
        assert_eq!(split_host_port("a.b:0"), None, "端口 0 非法");
        assert_eq!(split_host_port("a.b:99999"), None);
        assert_eq!(split_host_port("noport"), None);
    }

    /// 字面量分流 + 同址去重 + relay 位（同址先到先得——去重键 = 地址）。
    #[test]
    fn split_static_and_domain_fallback() {
        let (logf, _lines) = mem_logf();
        let eps = vec![
            EndpointRef::new("1.2.3.4:41641", EndpointKind::Direct),
            EndpointRef::new("1.2.3.4:41641", EndpointKind::Relay), // 同址去重（先到先得）
            EndpointRef::new("5.6.7.8:41641", EndpointKind::Relay),
        ];
        let got = split_and_resolve(&eps, &logf);
        assert_eq!(got.candidates.len(), 2, "同址去重");
        assert!(got.candidates[0].addr.to_string().starts_with("1.2.3.4"));
        assert!(!got.candidates[0].relay, "先到的直连形态保留");
        assert!(got.candidates[1].relay);
        assert!(got.domains.is_empty());
    }

    /// 域名端点（localhost）展开 + 首解析记行 + 域名条目保留。
    #[test]
    fn split_resolves_domain_endpoint() {
        let (logf, lines) = mem_logf();
        let eps = vec![
            EndpointRef::new("1.2.3.4:41641", EndpointKind::Direct),
            EndpointRef::new("localhost:41641", EndpointKind::Relay),
        ];
        let got = split_and_resolve(&eps, &logf);
        assert_eq!(got.domains.len(), 1);
        assert_eq!(got.domains[0].host, "localhost");
        assert!(got.domains[0].relay);
        assert!(got
            .candidates
            .iter()
            .any(|c| c.relay && c.addr.port() == 41641));
        let logged = lines.lock().unwrap();
        assert!(
            logged.iter().any(|l| l.starts_with("token 端点 localhost:41641 解析为")),
            "首解析记行（实收 {logged:?}）"
        );
    }

    /// 本地域名解析真跑（localhost：系统解析器恒可解析）。
    #[test]
    fn lookup_localhost() {
        let ips = lookup_host("localhost", Duration::from_secs(5), ResolveLane::Critical).unwrap();
        assert!(!ips.is_empty());
        assert!(ips.iter().any(|a| a.is_loopback()));
    }

    /// F8e：分档令牌池——额度独立（critical 占满不影响 background）、超限等待
    /// **按调用方预算**返回 TimedOut、Drop 归还后可再取。
    #[test]
    fn resolver_limiter_lanes_and_budget() {
        let lim = ResolverLimiter::new();
        // 占满 critical 档
        let mut held = Vec::new();
        for _ in 0..CRITICAL_QUOTA {
            held.push(lim.acquire(ResolveLane::Critical, Duration::from_millis(10)).unwrap());
        }
        assert_eq!(lim.inflight(ResolveLane::Critical), CRITICAL_QUOTA);
        // critical 占满：background 档不受影响（保留额度——两边互不饿死）
        let bg = lim.acquire(ResolveLane::Background, Duration::from_millis(10)).unwrap();
        assert_eq!(lim.inflight(ResolveLane::Background), 1);
        // 超限：等待上限 = 调用方预算（不得无限等）
        let t0 = Instant::now();
        let e = lim
            .acquire(ResolveLane::Critical, Duration::from_millis(120))
            .expect_err("额度满 ⇒ TimedOut");
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut);
        assert!(t0.elapsed() >= Duration::from_millis(100), "等到预算耗尽");
        assert!(t0.elapsed() < Duration::from_secs(2), "不得超出预算久等");
        assert!(e.to_string().contains("域名解析并发已达上限（4）"), "{e}");
        // 归还后可再取
        held.pop();
        assert!(lim.acquire(ResolveLane::Critical, Duration::from_millis(200)).is_ok());
        drop(bg);
        assert_eq!(lim.inflight(ResolveLane::Background), 0);
    }

    /// F8e：额度归属 = **worker（在飞解析）**——调用方超时返回**不**归还额度；
    /// 阻塞的解析 worker 继续占额度 ⇒ 该档到上限后有界地失败（第 N+1 个调用按预算
    /// TimedOut）；worker 真结束后额度归还。本测走**真 `lookup_host_with`**（可注入
    /// 池 + 可注入解析器），把「Permit 放回调用方」的旧形态直接钉红。
    #[test]
    fn permit_is_held_by_worker_not_caller() {
        let lim = ResolverLimiter::new();
        // 解析器：挂到测试放行（模拟黑洞 DNS——getaddrinfo 不返回）
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let rx2 = Arc::clone(&release_rx);
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = Arc::clone(&calls);
        let t0 = Instant::now();
        // 调用方预算 100ms：解析器永不按预算返回 ⇒ 调用方 TimedOut
        let e = lookup_host_with(
            &lim,
            "blackhole.invalid",
            Duration::from_millis(100),
            ResolveLane::Critical,
            move |_h: &str| {
                c2.fetch_add(1, Ordering::SeqCst);
                let _ = rx2.lock().unwrap().recv();
                Ok(vec![])
            },
        )
        .expect_err("解析不返回 ⇒ 调用方按预算 TimedOut");
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut, "{e}");
        assert!(t0.elapsed() < Duration::from_secs(2), "按预算收敛");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "worker 真跑过");
        // **关键断言**：调用方已返回，额度仍被 detach 的 worker 占着（旧形态此处必 0）
        assert_eq!(
            lim.inflight(ResolveLane::Critical),
            1,
            "额度随 worker（调用方超时不归还）"
        );
        // 放行 worker：额度随闭包结束归还
        let _ = release_tx.send(());
        let deadline = Instant::now() + Duration::from_secs(2);
        while lim.inflight(ResolveLane::Critical) != 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(lim.inflight(ResolveLane::Critical), 0, "worker 结束 ⇒ 归还");
    }

    /// F8e：额度占满时 `acquire` 的等待按预算收敛（`lookup_host` 的扣时契约同源——
    /// 获取耗时计进调用方预算，总耗时不得 ≈2× 预算）。
    #[test]
    fn resolver_acquire_wait_converges_within_budget() {
        let lim = ResolverLimiter::new();
        let mut held = Vec::new();
        for _ in 0..CRITICAL_QUOTA {
            held.push(lim.acquire(ResolveLane::Critical, Duration::from_millis(10)).unwrap());
        }
        let t0 = Instant::now();
        let e = lim
            .acquire(ResolveLane::Critical, Duration::from_millis(80))
            .unwrap_err();
        assert!(e.to_string().contains("并发已达上限"), "{e}");
        assert!(
            t0.elapsed() < Duration::from_millis(400),
            "获取等待必须按预算收敛（实耗 {:?}）",
            t0.elapsed()
        );
        drop(held);
        // 额度空出来后真解析可跑（localhost；进程单例池此时必有名额）
        assert!(lookup_host("localhost", Duration::from_secs(5), ResolveLane::Background).is_ok());
    }

    #[test]
    fn candidates_equality_relay_bits() {
        let a = vec![Candidate { addr: "1.2.3.4:1".parse().unwrap(), relay: false }];
        let b = vec![Candidate { addr: "1.2.3.4:1".parse().unwrap(), relay: false }];
        let c = vec![Candidate { addr: "1.2.3.4:1".parse().unwrap(), relay: true }];
        assert!(same_candidates(&a, &b));
        assert!(!same_candidates(&a, &c), "Relay 位参与比较");
    }

    /// F11：多重集反例——`[A,A,B]` vs `[A,B,B]` 必须判「变了」（旧的非多重集实现会误判
    /// 相等 ⇒ 跳过中继档的软赛跑补投并污染 last_cands）。
    #[test]
    fn same_candidates_multiset_counterexample() {
        let a: SocketAddr = "1.2.3.4:1".parse().unwrap();
        let b: SocketAddr = "1.2.3.4:2".parse().unwrap();
        let ca = Candidate { addr: a, relay: false };
        let cb = Candidate { addr: b, relay: false };
        assert!(!same_candidates(&[ca, ca, cb], &[ca, cb, cb]), "多重集必须不等");
        assert!(same_candidates(&[ca, cb], &[cb, ca]), "同集忽略顺序");
    }

    /// 重解析编排全链：候选变化 → apply 回调 + 记行；无变化 → 静默；中继采纳 →
    /// 节流软赛跑补投（首投 due、15s 内第二投节流掉）。
    #[test]
    fn refresher_apply_and_throttle() {
        use std::sync::atomic::AtomicUsize;
        let applies = Arc::new(AtomicUsize::new(0));
        let softs = Arc::new(AtomicUsize::new(0));
        let is_relay_hit = Arc::new(AtomicBool::new(true));
        let eps = vec![DomainEndpoint { host: "localhost".into(), port: 41641, relay: false }];
        let (logf, lines) = mem_logf();
        let a2 = Arc::clone(&applies);
        let s2 = Arc::clone(&softs);
        let ir = Arc::clone(&is_relay_hit);
        let r = Arc::new(DomainRefresher::new(
            eps,
            vec![Candidate { addr: "1.2.3.4:41641".parse().unwrap(), relay: false }],
            logf,
            Arc::new(move |_c: &[Candidate]| {
                a2.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(move || {
                (ir.load(Ordering::SeqCst))
                    .then(|| "9.9.9.9:41741".parse::<SocketAddr>().unwrap())
            }),
            Arc::new(move || {
                s2.fetch_add(1, Ordering::SeqCst);
            }),
        ));
        r.refresh_async();
        // 等解析线程收尾（localhost 解析毫秒级；给 2s 余量）。
        for _ in 0..100 {
            if applies.load(Ordering::SeqCst) >= 1 {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(applies.load(Ordering::SeqCst), 1, "候选变化 → apply 一次");
        assert_eq!(softs.load(Ordering::SeqCst), 1, "中继采纳 → 软赛跑补投一次");
        // 15s 节流内第二发（候选不变也不 apply；即便变也节流软赛跑——这里候选不变）。
        std::thread::sleep(Duration::from_millis(50));
        r.refresh_async();
        std::thread::sleep(Duration::from_millis(300));
        let lines = lines.lock().unwrap();
        assert!(
            lines.iter().any(|l| l.starts_with("域名重解析：")),
            "刷新记行（实收 {lines:?}）"
        );
    }
}
