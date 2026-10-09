//! 宿主面域名解析：分档令牌池 + 有界解析（`host:port` 切分）。
//!
//! M5 C3 迁址：原住址 `wtransport::domain_eps`（WG 档的「token 端点 → 建连候选」面，
//! 随 S2 整件退役）。其中**与承载无关的两件**被保留面继续消费，故迁到 crate 根：
//!
//! 1. [`lookup_host`]（+ [`ResolverLimiter`]/[`ResolveLane`]）——CLI host 面的
//!    `--host <域名>` 拨号（`facade::host_session`）与 daemon `host reach` 的域名目标
//!    解析（`daemon/hosts.rs`）；两者都不经 WG，QUIC 岛同样要把域名解析成 `IP:port`
//!    才能开流/探测。
//! 2. [`split_host_port_pub`]——`host:port` 切分（同上两处共用）。
//!
//! `v4-mapped` 归一契约（原模块的 M2 代码门 G3 结论）随迁：`lookup_host` 返回的列表
//! **v4 在前**，且 `[::ffff:a.b.c.d]` 一律映射回纯 v4（下游地址比较/键的基准）。
//!
//! 为什么**不是** Go 直译：原实现里「候选展开 + 重解析编排」是 WG 专属语义（随
//! `wtransport` 删除），本模块只留**纯解析工具**——没有 `Candidate`/`Bind` 概念，
//! 没有回调三件套（`ApplyStaticFn`/`IsRelayFn`/`SoftRearmFn`）。

use std::net::IpAddr;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// 宿主面解析的**同步**预算（Go `ProbeCandidates` 的 3s 同值；daemon `host reach`
/// 的 3.5s spec MUST 由调用方自扣——本常量只给「建会话/探测拍」这一档）。
pub const PROBE_SYNC_BUDGET: Duration = Duration::from_secs(3);

/// 域名解析的**档位**（Q-F F8e 分档令牌池）：关键路径（建会话 / daemon `host reach`
/// 用户可见结论）与后台刷新（巡检）分池——避免后台刷新把用户可见路径饿死。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResolveLane {
    /// 建会话 + daemon `host reach`（预算 3.5s 是 spec MUST）。
    Critical,
    /// 后台刷新（巡检，可等）。
    Background,
}

/// 各档额度（合计 8）：域名条目典型 ≤2–4 ⇒ 关键路径必须**永远拿得到**额度。
const CRITICAL_QUOTA: usize = 4;
const BACKGROUND_QUOTA: usize = 4;

/// 在飞解析上限（分档令牌池；Q-F F8e）。
///
/// 形态：**类型**（可注入/可替换）+ 进程级单例（[`process_limiter`]）。`acquire` 阻塞
/// 等待上限 = **调用方预算**（拿不到 ⇒ `TimedOut` + 明确文案）；**获取耗时计进调用方
/// 预算**（调用方在 [`lookup_host`] 里扣减后再喂 worker 期限，否则总耗时 = 2× 预算）。
///
/// 残余（如实登记）：DNS 黑洞下 critical 档 4 枚卡死线程可被占满 ⇒ 退化为「**有界地
/// 失败**」（该轮解析超时），不是「不会失效」。
#[derive(Debug, Default)]
pub struct ResolverLimiter {
    /// Arc 形态：**名额守卫必须能 move 进 worker 线程**（额度 = 在飞解析）⇒ [`Permit`]
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

/// 域名 → IPv4/IPv6 地址列表（系统解析器，`budget` 预算——被 DNS 卡住的上界；
/// Go `net.DefaultResolver` 等价）。返回顺序 **v4 在前**（同网段/兼容性更好的先试）。
///
/// Q-F F8e：入口先取分档令牌（阻塞上限 = 调用方预算）；**获取耗时计进同一预算**
/// （worker 期限按剩余重设——否则总耗时 = 2× 预算）。
///
/// **额度归属 = worker（在飞解析）**：令牌随 worker 闭包走，调用方超时返回**不**归还
/// 额度——黑洞下卡在 `getaddrinfo` 里的线程仍占额度 ⇒ 该档位到上限后**有界地失败**
/// （第 N+1 个调用等到预算耗尽即回 TimedOut）。worker 正常返回 ⇒ 额度随闭包结束归还。
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

/// `lookup_host` 的可注入主体（Q-F F8e 测试缝）：池与解析器都可替换 ⇒ 单测能真钉
/// 「额度随 worker 生命周期」（调用方超时后仍占额度）。
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
        // 归因区分两态：零预算进入（daemon reach 的 `saturating_sub` 可能给到 0）
        // ≠ 等待把预算耗尽
        let why = if budget.is_zero() {
            "域名解析预算为零（调用方未给预算）".to_owned()
        } else {
            format!("域名解析预算已被并发等待耗尽（预算 {budget:?}）")
        };
        return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, why));
    }
    // getaddrinfo 无原生超时——worker 线程跑真解析、按期限等（超时 detach 线程：
    // 它在 getaddrinfo 里自生自灭，进程退出由 OS 收；**额度由它继续持有**直到真返回）。
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
                        // v4-mapped v6：按 Go `Unmap` 口径归 v4 语义面（这里映射回 v4）。
                        IpAddr::V6(mapped) => {
                            v4.push(IpAddr::V4(mapped.to_ipv4_mapped().expect("已判 mapped")))
                        }
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

/// `host:port` 拆分（端口 1-65535；`[v6]:port` 形态由 `SocketAddr` 解析先行兜住，
/// 这里只剩域名形态）。`None` = 端口非法。
pub fn split_host_port(s: &str) -> Option<(String, u16)> {
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
            held.push(
                lim.acquire(ResolveLane::Critical, Duration::from_millis(10))
                    .unwrap(),
            );
        }
        assert_eq!(lim.inflight(ResolveLane::Critical), CRITICAL_QUOTA);
        // critical 占满：background 档不受影响（保留额度——两边互不饿死）
        let bg = lim
            .acquire(ResolveLane::Background, Duration::from_millis(10))
            .unwrap();
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
        assert!(lim
            .acquire(ResolveLane::Critical, Duration::from_millis(200))
            .is_ok());
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
                c2.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = rx2.lock().unwrap().recv();
                Ok(vec![])
            },
        )
        .expect_err("解析不返回 ⇒ 调用方按预算 TimedOut");
        assert_eq!(e.kind(), std::io::ErrorKind::TimedOut, "{e}");
        assert!(t0.elapsed() < Duration::from_secs(2), "按预算收敛");
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "worker 真跑过");
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
            held.push(
                lim.acquire(ResolveLane::Critical, Duration::from_millis(10))
                    .unwrap(),
            );
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
}
