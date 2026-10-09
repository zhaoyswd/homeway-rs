//! 抗放大与握手限流（M2 设计 §3.1–§3.3）：每源滑动窗闸 + 证明失败闸 + Retry 触发判定。
//!
//! **纯 std**（设计门 r14 F26 / S2-4 定死）：只用 `std::time::Instant`、`VecDeque`、
//! `HashMap`。因此本文件**不进** `tools/check-quic-isolation.sh` 的 `ASYNC_FILES` 清单
//! （该清单 = 异步面白名单，本文件是同步小件）。
//!
//! 结构（§3.2-④ 定死）：插入序 `VecDeque` + `HashMap` 索引 ⇒ 记一次尝试 O(1) 摊还、
//! 淘汰取队首（**不扫描挑最旧**——洪泛下那会变成 CPU 放大器）；表 ≤ [`SRC_TABLE_CAP`] 条
//! （≈40–64KB）。
//!
//! ## 记账语义（§3.2-④ / §3.3-1 要求「窗口语义与『被拒是否计数』必须写死」）
//!
//! - **窗口 = 滑动窗**（一条按时间排的时间戳序列；不进窗 = 不计数）；
//! - **过闸即记账**（乐观计入）：每次尝试在过闸时就进窗；
//! - **完成即销账**（[`SrcGate::completed`]）：握手被采纳的尝试销掉窗内**最早一条** ⇒
//!   窗内计数 = 「尝试数 − 完成数」= **未完成/被拒的尝试数**（§3.1-② 的口径；r14 F10
//!   的「正常赛跑会完成 ⇒ 不计数」正是这条减法，不是「不记」）；
//! - **被拒的尝试仍计数**：拒后仍不断供 ⇒ 洪泛期持续被拒（§3.3-1 的
//!   `flood_refused = K − F` 就是这条算术）。
//!
//! ## 与「手」的对应
//!
//! | 件 | 设计条 | 消费者 |
//! |---|---|---|
//! | [`SrcGate`] | §3.2-④ 每源准入速率（键 = v4 /32、v6 /64） | 出口主循环「先闸后 Retry」的第一道 |
//! | [`ProofGate`] | §3.2-⑥ 证明失败闸（按 devTag，**排除引擎裁决拒绝**） | 出口准入面（Hello 之后、写 Challenge 之前） |
//! | [`retry_due`] | §3.1 三条触发条件 | 出口主循环（`Incoming::retry()` 之前） |

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

// ---------- 值域与缺省（设计 §3.2 表；配置面与校验层引用同一组常量） ----------

/// 每源闸表上限（§3.2-④ 定死：≤1024 条 ≈40–64KB）。
pub const SRC_TABLE_CAP: usize = 1024;
/// 证明失败闸表上限（同 [`SRC_TABLE_CAP`] 量级；键 = devTag 8B）。
pub const PROOF_TABLE_CAP: usize = 1024;

/// `retry_token_lifetime` 缺省（**收自 quinn 缺省 15s**，§3.1 登记）。
pub const RETRY_TOKEN_LIFETIME_DEFAULT: Duration = Duration::from_secs(5);
/// `retry_token_lifetime` 值域 `1s..=60s`。
pub const RETRY_TOKEN_LIFETIME_MIN: Duration = Duration::from_secs(1);
/// `retry_token_lifetime` 值域上界（`1s..=60s`）。
pub const RETRY_TOKEN_LIFETIME_MAX: Duration = Duration::from_secs(60);

/// `per_src_fails` 缺省（**M2 §14-1 裁决：`10` → `16`**；窗保持 10s）。
///
/// 依据（S3 实测 + 主会话裁定）：3 候选同址赛跑在「客户端 abort 未完成候选」形态下，
/// 单轮消耗 `(N−1)` 次失败预算（N=3 ⇒ 2 次/轮）⇒ 旧值 10 在第 4 轮就触发闸（实测
/// `flood_refused=5` + 岛侧 `NoCandidate`）。**不放宽计数集**（攻击者同样能 abort ⇒
/// 豁免 = 逃逸面），改为抬高预算：16 允许约 5 次 3 候选赛跑/窗（覆盖断网抖动期的恢复
/// 节奏），同时仍把攻击者束在 **≤16 次握手/10s/源**（有界性不变）。值可配，**待真机标定**。
pub const PER_SRC_FAILS_DEFAULT: u32 = 16;
/// `per_src_fails` 值域下界（`1..=1000`）。
pub const PER_SRC_FAILS_MIN: u32 = 1;
/// `per_src_fails` 值域上界（`1..=1000`）。
pub const PER_SRC_FAILS_MAX: u32 = 1000;

/// `per_src_window` 缺省（§3.2-④：`10s`）。
pub const PER_SRC_WINDOW_DEFAULT: Duration = Duration::from_secs(10);
/// `per_src_window` 值域下界（`1s..=1h`）。
pub const PER_SRC_WINDOW_MIN: Duration = Duration::from_secs(1);
/// `per_src_window` 值域上界（`1s..=1h`）。
pub const PER_SRC_WINDOW_MAX: Duration = Duration::from_secs(3600);

/// `nonce_ttl` 值域下界（`1s..=30s`；缺省 = `super::DEFAULT_NONCE_TTL`）。
pub const NONCE_TTL_MIN: Duration = Duration::from_secs(1);
/// `nonce_ttl` 值域上界（`1s..=30s`）。
pub const NONCE_TTL_MAX: Duration = Duration::from_secs(30);

/// `admit_deadline` 值域下界（`1s..=60s`；缺省 = `super::DEFAULT_ADMIT_DEADLINE`）。
pub const ADMIT_DEADLINE_MIN: Duration = Duration::from_secs(1);
/// `admit_deadline` 值域上界（`1s..=60s`）。
pub const ADMIT_DEADLINE_MAX: Duration = Duration::from_secs(60);

/// `proof_fail_threshold` 缺省（§3.2-⑥：`10`；`0` = 关闭该闸）。
pub const PROOF_FAIL_THRESHOLD_DEFAULT: u32 = 10;
/// `proof_fail_threshold` 值域上界（`0..=1000`；0 = 关闭）。
pub const PROOF_FAIL_THRESHOLD_MAX: u32 = 1000;
/// 证明失败闸的**统计窗**（§3.2-⑥ 定值：60s 内的 nonce/MAC 类失败）。
pub const PROOF_FAIL_WINDOW: Duration = Duration::from_secs(60);
/// 证明失败闸的**冷却时长**（§3.2-⑥ 只写了「冷却期内不再发 Challenge」，长度未给值 ⇒
/// 实施期取与统计窗同档的 60s；越界面由 [`PROOF_FAIL_THRESHOLD`] 的「0 = 关闭」承担）。
pub const PROOF_FAIL_COOLDOWN: Duration = Duration::from_secs(60);

/// Retry 触发条件②的阈值（§3.1-②：同源「未完成/被拒」≥ 本值 / 窗）。
pub const RETRY_AFTER_FAILS: u32 = 5;
/// 触发条件①的分母系数（§3.1-①：未认证在途 ≥ `handshake_cap / 本值`）。
pub const RETRY_INFLIGHT_DIVISOR: usize = 2;

/// `serve.quic_admit` 段的**已解析值**（§3.2 六行七键的载体；配置面 → 出口面的唯一搬运
/// 形态——纯 std，同步面（`homeway-core`）读得懂，且值域校验只有这一处真源）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmitLimits {
    /// Retry token 有效期（`1s..=60s`；缺省 5s）。
    pub retry_token_lifetime: Duration,
    /// 每源滑动窗上限（`1..=1000`；缺省 16 = [`PER_SRC_FAILS_DEFAULT`]，M2 §14-1 裁决）。
    pub per_src_fails: u32,
    /// 每源滑动窗窗长（`1s..=1h`；缺省 10s）。
    pub per_src_window: Duration,
    /// nonce 有效期（`1s..=30s`；缺省 5s = `super::DEFAULT_NONCE_TTL`）。
    pub nonce_ttl: Duration,
    /// 准入总期限（`1s..=60s`；缺省 10s = `super::DEFAULT_ADMIT_DEADLINE`）。
    pub admit_deadline: Duration,
    /// 证明失败闸阈值（`0..=1000`；缺省 10；`0` = 关闭）。
    pub proof_fail_threshold: u32,
    /// Retry 策略（缺省 `pressure`）。
    pub retry_policy: RetryPolicy,
}

impl Default for AdmitLimits {
    /// 设计定值（§3.2/§3.1；`ExitQuicConfig::new` 与此逐值相同——缺省不改行为）。
    fn default() -> Self {
        Self {
            retry_token_lifetime: RETRY_TOKEN_LIFETIME_DEFAULT,
            per_src_fails: PER_SRC_FAILS_DEFAULT,
            per_src_window: PER_SRC_WINDOW_DEFAULT,
            nonce_ttl: super::DEFAULT_NONCE_TTL,
            admit_deadline: super::DEFAULT_ADMIT_DEADLINE,
            proof_fail_threshold: PROOF_FAIL_THRESHOLD_DEFAULT,
            retry_policy: RetryPolicy::Pressure,
        }
    }
}

impl AdmitLimits {
    /// 值域校验（§3.2 表「越界处置 = 拒启」的判据；`Err` 文案由配置层直接拼进拒启消息）。
    ///
    /// 字段顺序 = §3.2 表的行序（报错取第一个越界项，够定位；不必一次报全）。
    pub fn validate(&self) -> Result<(), String> {
        let dur = |name: &str, v: Duration, lo: Duration, hi: Duration| -> Result<(), String> {
            if v < lo || v > hi {
                return Err(format!(
                    "serve.quic_admit.{name}：{} 非法（合法值域 {}–{}）",
                    fmt_dur(v),
                    fmt_dur(lo),
                    fmt_dur(hi)
                ));
            }
            Ok(())
        };
        dur(
            "retry_token_lifetime",
            self.retry_token_lifetime,
            RETRY_TOKEN_LIFETIME_MIN,
            RETRY_TOKEN_LIFETIME_MAX,
        )?;
        if !(PER_SRC_FAILS_MIN..=PER_SRC_FAILS_MAX).contains(&self.per_src_fails) {
            return Err(format!(
                "serve.quic_admit.per_src_fails：{} 非法（合法值域 {}–{}）",
                self.per_src_fails, PER_SRC_FAILS_MIN, PER_SRC_FAILS_MAX
            ));
        }
        dur(
            "per_src_window",
            self.per_src_window,
            PER_SRC_WINDOW_MIN,
            PER_SRC_WINDOW_MAX,
        )?;
        dur("nonce_ttl", self.nonce_ttl, NONCE_TTL_MIN, NONCE_TTL_MAX)?;
        dur(
            "admit_deadline",
            self.admit_deadline,
            ADMIT_DEADLINE_MIN,
            ADMIT_DEADLINE_MAX,
        )?;
        if self.proof_fail_threshold > PROOF_FAIL_THRESHOLD_MAX {
            return Err(format!(
                "serve.quic_admit.proof_fail_threshold：{} 非法（合法值域 0–{}；0 = 关闭该闸）",
                self.proof_fail_threshold, PROOF_FAIL_THRESHOLD_MAX
            ));
        }
        Ok(())
    }
}

/// 秒级时长文案（校验错误消息用；配置面自己的 `Go` 时长串解析在 CLI 侧）。
fn fmt_dur(d: Duration) -> String {
    if d.as_secs() >= 3600 && d.as_secs().is_multiple_of(3600) {
        format!("{}h", d.as_secs() / 3600)
    } else {
        format!("{}s", d.as_secs())
    }
}

/// 每源键（§3.2-④ 定死：**v4 /32、v6 /64 前缀聚合**——同址多端口算同源）。
///
/// **v4-mapped 归一（S5 D1 修复，2026-10-09）**：出口 QUIC socket 是**双栈**
/// （`bind_dual_stack` 绑 `[::]`）⇒ IPv4 对端在 socket 面以 `::ffff:a.b.c.d` 出现。
/// 若不归一，所有 IPv4 源会塌进同一个 `::/64` 桶（S5 实测：源 A 打满 16/10s 后，
/// 异 /32 的源 B **第 1 次**尝试即被拒）——即 §3.2-④ 的「v4 /32」在双栈出口上不生效。
/// 归一的定义域**只有** RFC 4291 的 v4-mapped（`::ffff:0:0/96`）：其余 IPv6
/// （含 `::1`、ULA、GUA）保持 /64 聚合。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum SrcKey {
    /// IPv4 /32（整地址）。
    V4([u8; 4]),
    /// IPv6 /64（前 8 字节）。
    V6([u8; 8]),
}

impl SrcKey {
    /// 地址 → 前缀键（聚合口径见类型文档；**v4-mapped 先归一为 `V4`**）。
    pub fn of(peer: SocketAddr) -> Self {
        match peer.ip() {
            IpAddr::V4(v4) => Self::V4(v4.octets()),
            IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
                Some(v4) => Self::V4(v4.octets()),
                None => {
                    let mut b = [0u8; 8];
                    b.copy_from_slice(&v6.octets()[..8]);
                    Self::V6(b)
                }
            },
        }
    }

    /// 展示文案（判据行里要看得见**聚合后的源前缀**：`a.b.c.d/32` / `x::/64`）。
    pub fn text(self) -> String {
        match self {
            Self::V4(o) => format!("{}/32", std::net::Ipv4Addr::from(o)),
            Self::V6(b) => {
                let mut full = [0u8; 16];
                full[..8].copy_from_slice(&b);
                format!("{}/64", std::net::Ipv6Addr::from(full))
            }
        }
    }
}

/// 过闸结论 + 当时的窗内计数（计数进「握手洪泛拒绝」行）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct GateOutcome {
    /// 放行 / 拒。
    pub action: SrcAction,
    /// 记账后该源在窗内的计数（含本次）。
    pub in_window: u32,
}

/// 过闸动作。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SrcAction {
    /// 放行（窗内计数未达上限）。
    Allow,
    /// 拒（窗内计数已达上限）——调用方 `refuse()` + 计数 + 记行。
    Refuse,
}

/// 每源准入速率闸（§3.2-④；记账语义见模块头）。
pub(crate) struct SrcGate {
    limit: u32,
    window: Duration,
    cap: usize,
    /// 插入序（每个在场键一条；淘汰从队首取）。
    order: VecDeque<SrcKey>,
    /// 每源的窗内时间戳序列（队首 = 最早）。
    attempts: HashMap<SrcKey, VecDeque<Instant>>,
    /// 最近一次闸拒绝（触发条件③的输入）。
    last_refuse: Option<Instant>,
}

impl SrcGate {
    /// 建闸（`limit` = 窗内上限、`window` = 窗长；表上限取 [`SRC_TABLE_CAP`]）。
    pub fn new(limit: u32, window: Duration) -> Self {
        Self::with_cap(limit, window, SRC_TABLE_CAP)
    }

    /// 建闸（显式表上限；测试用）。
    pub fn with_cap(limit: u32, window: Duration, cap: usize) -> Self {
        Self {
            limit,
            window,
            cap,
            order: VecDeque::new(),
            attempts: HashMap::new(),
            last_refuse: None,
        }
    }

    /// 一次建连尝试过闸：窗内计数 ≥ 上限 ⇒ [`SrcAction::Refuse`]（**仍记账**）。
    pub fn attempt(&mut self, now: Instant, peer: SocketAddr) -> GateOutcome {
        let key = SrcKey::of(peer);
        let before = self.prune(now, key);
        let action = if before >= self.limit {
            SrcAction::Refuse
        } else {
            SrcAction::Allow
        };
        if !self.attempts.contains_key(&key) {
            self.order.push_back(key);
        }
        self.attempts.entry(key).or_default().push_back(now);
        if action == SrcAction::Refuse {
            self.last_refuse = Some(now);
        }
        self.enforce_cap();
        GateOutcome {
            action,
            in_window: before + 1,
        }
    }

    /// 一次尝试**完成**（握手被采纳）：销窗内最早一条 ⇒ 正常完成的尝试不计数（r14 F10）。
    pub fn completed(&mut self, peer: SocketAddr) {
        if let Some(dq) = self.attempts.get_mut(&SrcKey::of(peer)) {
            dq.pop_front();
        }
    }

    /// 该源窗内「未完成/被拒」计数（Retry 触发条件②的输入；顺带剪枝）。
    pub fn pending(&mut self, now: Instant, peer: SocketAddr) -> u32 {
        self.prune(now, SrcKey::of(peer))
    }

    /// 最近一个窗口内是否发生过闸拒绝（触发条件③：攻击迹象）。
    pub fn refused_recently(&self, now: Instant) -> bool {
        self.last_refuse
            .is_some_and(|t| now.duration_since(t) <= self.window)
    }

    /// 在场键数（**表上限的读面**；测试用——产品路径不读它，故不进快照）。
    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.attempts.len()
    }

    /// 剪掉过期时间戳并返回剩余计数。
    fn prune(&mut self, now: Instant, key: SrcKey) -> u32 {
        let Some(dq) = self.attempts.get_mut(&key) else {
            return 0;
        };
        while let Some(&front) = dq.front() {
            if now.duration_since(front) > self.window {
                dq.pop_front();
            } else {
                break;
            }
        }
        dq.len() as u32
    }

    /// 表上限：超限即按插入序丢最旧键（O(1)；**不扫描挑最旧**）。
    fn enforce_cap(&mut self) {
        while self.order.len() > self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.attempts.remove(&oldest);
            }
        }
    }
}

/// 证明失败闸（§3.2-⑥）：同 devTag 在 [`PROOF_FAIL_WINDOW`] 内 **nonce/MAC 类**失败
/// ≥ 阈值 ⇒ 冷却 [`PROOF_FAIL_COOLDOWN`]（冷却期不再发 Challenge）。
///
/// **计数集排除引擎裁决拒绝**（r14 F12 订正）：表满/冲突/吊销/窗超都是合法设备的可用性
/// 故障，把它们算进冷却会把一次可用性故障放大成更长的锁死 ⇒ 计数点只有两处
/// （nonce 类 / MAC 类，见 `exit/conn.rs`），引擎裁决拒绝**不进本闸**。
pub(crate) struct ProofGate {
    threshold: u32,
    cap: usize,
    order: VecDeque<[u8; 8]>,
    fails: HashMap<[u8; 8], VecDeque<Instant>>,
    cooling: HashMap<[u8; 8], Instant>,
}

impl ProofGate {
    /// 建闸（`threshold = 0` ⇒ 本闸关闭，`note_fail` 恒返 `None`、`is_cooling` 恒 `false`）。
    pub fn new(threshold: u32) -> Self {
        Self::with_cap(threshold, PROOF_TABLE_CAP)
    }

    /// 建闸（显式表上限；测试用）。
    pub fn with_cap(threshold: u32, cap: usize) -> Self {
        Self {
            threshold,
            cap,
            order: VecDeque::new(),
            fails: HashMap::new(),
            cooling: HashMap::new(),
        }
    }

    /// 本闸是否启用（`proof_fail_threshold != 0`）。
    pub fn enabled(&self) -> bool {
        self.threshold > 0
    }

    /// 记一次 nonce/MAC 类失败。返回 `Some(窗内失败次数)` = **本次跨过阈值、新进入冷却**
    /// （调用方据此打「证明失败闸」行 + 计数）；冷却期内再失败返回 `None`。
    pub fn note_fail(&mut self, now: Instant, dev: [u8; 8]) -> Option<u32> {
        if !self.enabled() {
            return None;
        }
        let window = PROOF_FAIL_WINDOW;
        let was_cooling = self.cooling.get(&dev).is_some_and(|until| *until > now);
        let (count, first) = {
            let dq = self.fails.entry(dev).or_insert_with(|| {
                VecDeque::new()
            });
            let newly_inserted = dq.is_empty() && !self.order.contains(&dev);
            while let Some(&front) = dq.front() {
                if now.duration_since(front) > window {
                    dq.pop_front();
                } else {
                    break;
                }
            }
            dq.push_back(now);
            (dq.len() as u32, newly_inserted)
        };
        if first {
            self.order.push_back(dev);
        }
        self.enforce_cap();
        if count >= self.threshold {
            self.cooling.insert(dev, now + PROOF_FAIL_COOLDOWN);
            if !was_cooling {
                return Some(count);
            }
        }
        None
    }

    /// 该 devTag 是否处于冷却（冷却期 ⇒ 拒 Hello，不再发 Challenge）。
    pub fn is_cooling(&mut self, now: Instant, dev: [u8; 8]) -> bool {
        if !self.enabled() {
            return false;
        }
        match self.cooling.get(&dev).copied() {
            Some(until) if until > now => true,
            Some(_) => {
                self.cooling.remove(&dev);
                false
            }
            None => false,
        }
    }

    /// 在场键数（**表上限的读面**；测试用——产品路径不读它，故不进快照）。
    #[cfg(test)]
    pub fn tracked(&self) -> usize {
        self.fails.len()
    }

    /// 表上限：超限即按插入序丢最旧键（同时清其冷却位）。
    fn enforce_cap(&mut self) {
        while self.order.len() > self.cap {
            if let Some(oldest) = self.order.pop_front() {
                self.fails.remove(&oldest);
                self.cooling.remove(&oldest);
            }
        }
    }
}

/// Retry 策略（§3.2 表：`pressure`（缺省）| `always` | `never`）。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum RetryPolicy {
    /// 压力触发（§3.1 推荐档）：三条触发条件任一命中才 Retry。
    #[default]
    Pressure,
    /// 恒 Retry（**常态每次重连 +1 RTT**，§3.1 登记的代价；排障/测试用）。
    Always,
    /// 从不 Retry（只靠 quinn 内建 3× 限与三闸）。
    Never,
}

impl RetryPolicy {
    /// 配置/ env 的取值（枚举真源；`serve.quic_admit.retry_policy` 与
    /// `HOMEWAY_QUIC_ADMIT_RETRY` 共用）。
    pub const VALUES: [&'static str; 3] = ["pressure", "always", "never"];

    /// 解析（非法值 ⇒ `None`，由调用侧决定「拒启」还是「记行 + 缺省」）。
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "pressure" => Some(Self::Pressure),
            "always" => Some(Self::Always),
            "never" => Some(Self::Never),
            _ => None,
        }
    }

    /// 文本（记行/配置回写用；与 [`Self::parse`] 互逆）。
    pub const fn text(self) -> &'static str {
        match self {
            Self::Pressure => "pressure",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
}

/// Retry 触发判定（§3.1；**先闸后 Retry** 的第二步）。
///
/// - `Pressure` 档 = 三条任一：① 未认证在途 ≥ `inflight_cap / RETRY_INFLIGHT_DIVISOR`；
///   ② 同源「未完成/被拒」≥ [`RETRY_AFTER_FAILS`]；③ 最近窗内有闸拒绝（攻击迹象）。
/// - `Always` 档 = 恒真；`Never` 档 = 恒假。
///
/// 调用侧还必须叠 `!remote_address_validated() && may_retry()` 守卫（r14 F19）。
pub fn retry_due(
    policy: RetryPolicy,
    inflight_unauthenticated: usize,
    inflight_capacity: usize,
    src_pending: u32,
    gate_refused_recently: bool,
) -> bool {
    match policy {
        RetryPolicy::Never => false,
        RetryPolicy::Always => true,
        RetryPolicy::Pressure => {
            let threshold = (inflight_capacity / RETRY_INFLIGHT_DIVISOR).max(1);
            inflight_unauthenticated >= threshold
                || src_pending >= RETRY_AFTER_FAILS
                || gate_refused_recently
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v4(tail: u8, port: u16) -> SocketAddr {
        SocketAddr::from(([127, 0, 0, tail], port))
    }

    fn v6(head: u8, tail: u16, port: u16) -> SocketAddr {
        let ip = std::net::Ipv6Addr::new(
            ((head as u16) << 8) | 0x20,
            0x1234,
            0x5678,
            0x9abc,
            tail,
            0,
            0,
            1,
        );
        SocketAddr::new(IpAddr::V6(ip), port)
    }

    /// IPv4 的 `::ffff:a.b.c.d` 形态（**双栈 socket 面 IPv4 对端的真实表示**，S5 D1）。
    fn v4_mapped(octets: [u8; 4], port: u16) -> SocketAddr {
        SocketAddr::new(
            IpAddr::V6(std::net::Ipv4Addr::from(octets).to_ipv6_mapped()),
            port,
        )
    }

    /// **判据（S3-1）**：闸边界——窗内第 `F+1` 次起全部被拒；被拒的尝试仍计数
    /// （§3.3-1 的 `flood_refused = K − F` 算术）。
    #[test]
    fn gate_refuses_from_f_plus_one_and_counts_refused() {
        let t0 = Instant::now();
        let mut g = SrcGate::new(3, Duration::from_secs(10));
        let peer = v4(1, 40000);
        let mut refused = 0;
        for i in 1..=8u32 {
            let out = g.attempt(t0 + Duration::from_millis(i as u64), peer);
            let want = if i <= 3 { SrcAction::Allow } else { SrcAction::Refuse };
            assert_eq!(out.action, want, "第 {i} 次的过闸结论");
            assert_eq!(out.in_window, i, "窗内计数含本次：第 {i} 次");
            if out.action == SrcAction::Refuse {
                refused += 1;
            }
        }
        assert_eq!(refused, 8 - 3, "第 F+1..K 次全拒（K−F = 5）");
    }

    /// **判据（S3-1）**：窗口推进——过期的时间戳滚出窗外（滑窗语义，写死在此）；
    /// 窗内计数回落后重新放行。
    #[test]
    fn gate_window_slides_and_reallows() {
        let t0 = Instant::now();
        let w = Duration::from_secs(10);
        let mut g = SrcGate::new(2, w);
        let peer = v4(2, 40000);
        assert_eq!(g.attempt(t0, peer).action, SrcAction::Allow);
        assert_eq!(
            g.attempt(t0 + Duration::from_secs(1), peer).action,
            SrcAction::Allow
        );
        assert_eq!(
            g.attempt(t0 + Duration::from_secs(2), peer).action,
            SrcAction::Refuse,
            "窗内第 3 次超上限"
        );
        // 恰好 +W：最早一条（t0）过期 ⇒ 计数 2 ⇒ 仍拒（第 2 条与第 3 条过期前）
        assert_eq!(
            g.attempt(t0 + w, peer).action,
            SrcAction::Refuse,
            "滑窗：t0 滚出但 t0+1s/t0+2s 仍在窗内"
        );
        // 再往后：t0+2s 也滚出（t0+10 那次拒绝的记账仍在窗内）⇒ 计数回落到 1 ⇒ 放行
        assert_eq!(
            g.attempt(t0 + w + Duration::from_secs(3), peer).action,
            SrcAction::Allow,
            "旧时间戳陆续滚出 ⇒ 重新放行"
        );
        // 严格边界（用只读计数面断言，避免「被拒也记账」干扰）：恰好等于窗长时
        // **仍算窗内**（过期判据是严格大于）。
        let mut h = SrcGate::new(1, w);
        assert_eq!(h.attempt(t0, peer).action, SrcAction::Allow);
        assert_eq!(h.pending(t0 + w, peer), 1, "恰好 W 仍在窗内");
        assert_eq!(
            h.pending(t0 + w + Duration::from_millis(1), peer),
            0,
            "超 W ⇒ 过期（取 1ms 而不是 1ns：`Instant` 加法在部分平台按时钟粒度取整）"
        );
    }

    /// **判据（S3-1）**：`v6 /64` 前缀聚合——同 /64 的不同接口/端口算同源，
    /// 换 /64 前缀（第 2 组 u16 变化）算异源；v4 按 /32。
    #[test]
    fn gate_aggregates_v6_by_64_and_v4_by_32() {
        let t0 = Instant::now();
        let mut g = SrcGate::new(2, Duration::from_secs(10));
        // 同 /64（仅末 64 位与端口不同）⇒ 同键
        assert_eq!(g.attempt(t0, v6(1, 0x0001, 1000)).action, SrcAction::Allow);
        assert_eq!(g.attempt(t0, v6(1, 0xffff, 2000)).action, SrcAction::Allow);
        assert_eq!(
            g.attempt(t0, v6(1, 0x1234, 3000)).action,
            SrcAction::Refuse,
            "同 /64 的不同主机/端口必须聚合到同一键"
        );
        assert_eq!(g.tracked(), 1, "同 /64 只占一条表项");
        // 换 /64 前缀（第 2 组 u16 变）⇒ 异键 ⇒ 不共享计数
        assert_eq!(g.attempt(t0, v6(2, 0x0001, 1000)).action, SrcAction::Allow);
        assert_eq!(g.tracked(), 2);
        // v4：同 /32 的不同端口聚合
        let mut h = SrcGate::new(2, Duration::from_secs(10));
        assert_eq!(h.attempt(t0, v4(9, 1111)).action, SrcAction::Allow);
        assert_eq!(h.attempt(t0, v4(9, 2222)).action, SrcAction::Allow);
        assert_eq!(
            h.attempt(t0, v4(9, 3333)).action,
            SrcAction::Refuse,
            "v4 按 /32 聚合（端口不进键）"
        );
        assert_ne!(SrcKey::of(v4(9, 1)), SrcKey::of(v4(10, 1)), "不同 /32 = 异键");
    }

    /// **判据（S5 D1 修复，2026-10-09）**：双栈出口（`[::]` 绑定）下 IPv4 对端以
    /// **v4-mapped**（`::ffff:a.b.c.d`）出现 ⇒ 键必须先归一为 `V4(a.b.c.d/32)`：
    /// **两个不同 IPv4 源各自独立计桶**（源 A 用满后源 B 仍可准入），且与 v4-only
    /// socket 下同址的键一致；真正的 IPv6（`::1` 等）不受影响、仍按 /64 聚合。
    #[test]
    fn v4_mapped_sources_get_independent_v4_buckets() {
        let t0 = Instant::now();
        // 形态 = S5 实测行文里的对端：`[::ffff:192.168.3.12]:61979`
        let a = v4_mapped([192, 168, 3, 12], 61979);
        let b = v4_mapped([127, 0, 0, 1], 40000);
        // 归一落点：v4-mapped 与同址的 v4 形态同键（同 /32 = 同桶，不论表示）
        assert_eq!(
            SrcKey::of(a),
            SrcKey::of(SocketAddr::from(([192, 168, 3, 12], 1))),
            "v4-mapped 与 v4 同址必须同键（端口不进键）"
        );
        assert_eq!(SrcKey::of(b), SrcKey::of(v4(1, 1)), "127.0.0.1 两种形态同键");
        // 异 /32 ⇒ 异键。**这是 D1 的证伪点**：修复前两者都落 `V6([0; 8])` ⇒ 同键「::/64」
        assert_ne!(
            SrcKey::of(a),
            SrcKey::of(b),
            "不同 IPv4 源必须是不同桶（D1 根因：未归一时键恒为 ::/64）"
        );
        assert_eq!(SrcKey::of(a).text(), "192.168.3.12/32", "行文键 = 归一后的 /32");
        // 行为面：F=2；源 A 打满（被拒那条第 3 次仍记账）⇒ 源 B 第 1 次仍被放行
        let mut g = SrcGate::new(2, Duration::from_secs(10));
        assert_eq!(g.attempt(t0, a).action, SrcAction::Allow);
        assert_eq!(g.attempt(t0, a).action, SrcAction::Allow);
        assert_eq!(g.attempt(t0, a).action, SrcAction::Refuse, "源 A 打满 2/10s");
        let out = g.attempt(t0, b);
        assert_eq!(
            out.action,
            SrcAction::Allow,
            "源 B 独立计桶 ⇒ 第 1 次必放行（S5 实测：修复前此处被拒）"
        );
        assert_eq!(out.in_window, 1, "源 B 的窗内计数与源 A 无关");
        assert_eq!(g.pending(t0, a), 3, "源 A 三条（含被拒那条仍记账）");
        assert_eq!(g.pending(t0, b), 1, "源 B 一条");
        assert_eq!(g.tracked(), 2, "两个源各占一条表项（修复前只有一条）");
        // 真 IPv6 不被归一：`::1` 仍走 /64 键，且与 v4-mapped 异键
        let loopback = SocketAddr::new(IpAddr::V6(std::net::Ipv6Addr::LOCALHOST), 1000);
        assert!(
            matches!(SrcKey::of(loopback), SrcKey::V6(_)),
            "非 v4-mapped 的 IPv6 保持 /64 聚合"
        );
        assert_ne!(SrcKey::of(loopback), SrcKey::of(b), "`::1` 不是 127.0.0.1");
        assert_eq!(SrcKey::of(loopback).text(), "::/64");
    }

    /// **判据（S3-1 / r14 F10）**：正常完成的尝试**不计数**——完成销窗内最早一条；
    /// 因故未完成（引擎拒绝/对端放弃/期限到点）的留在窗内。
    #[test]
    fn completed_attempts_are_released_not_counted() {
        let t0 = Instant::now();
        let mut g = SrcGate::new(2, Duration::from_secs(10));
        let peer = v4(3, 40000);
        // 连续 5 轮「尝试 → 完成」：任何时刻窗内计数都不该累到上限
        for i in 0..5u32 {
            let out = g.attempt(t0 + Duration::from_secs(u64::from(i)), peer);
            assert_eq!(out.action, SrcAction::Allow, "第 {i} 轮：正常赛跑不得被闸误伤");
            assert_eq!(out.in_window, 1, "完成销账后窗内只剩本次");
            g.completed(peer);
        }
        assert_eq!(g.pending(t0 + Duration::from_secs(4), peer), 0, "全部销完");
        // 未完成的（不销账）累积 ⇒ 第 3 次起拒
        let mut h = SrcGate::new(2, Duration::from_secs(10));
        for i in 0..2 {
            assert_eq!(h.attempt(t0, peer).action, SrcAction::Allow, "第 {i} 次");
        }
        assert_eq!(h.attempt(t0, peer).action, SrcAction::Refuse, "第 3 次超上限");
        assert_eq!(h.pending(t0, peer), 3, "三条都记账（含被拒的那条）");
        h.completed(peer);
        assert_eq!(h.pending(t0, peer), 2, "完成销最早一条");
        assert_eq!(
            h.attempt(t0, peer).action,
            SrcAction::Refuse,
            "销一条后计数 = 上限（2）⇒ 仍拒"
        );
        h.completed(peer);
        h.completed(peer);
        assert_eq!(h.pending(t0, peer), 1, "四条记账销三条 ⇒ 剩一条");
        assert_eq!(
            h.attempt(t0, peer).action,
            SrcAction::Allow,
            "计数 1 < 上限 2 ⇒ 放行（计数 = 尝试 − 完成）"
        );
    }

    /// **判据（S3-1）**：索引淘汰——表 ≤ 上限（插入序丢最旧，O(1)）；被淘汰的同源
    /// 计数从零开始；淘汰不影响他源。
    #[test]
    fn gate_table_is_capped_and_evicts_oldest() {
        let t0 = Instant::now();
        let mut g = SrcGate::with_cap(1, Duration::from_secs(10), 4);
        for i in 0..4u8 {
            assert_eq!(g.attempt(t0, v4(i, 1000)).action, SrcAction::Allow);
        }
        assert_eq!(g.tracked(), 4, "上限内");
        // 上限内他源不受影响：tail=1 已有一条 ⇒ 第 2 次被拒
        assert_eq!(g.attempt(t0, v4(1, 1234)).action, SrcAction::Refuse);
        // 第 5 个源：超上限 ⇒ 丢最旧（tail=0）
        assert_eq!(g.attempt(t0, v4(4, 1000)).action, SrcAction::Allow);
        assert_eq!(g.tracked(), 4, "表恒 ≤ 上限");
        // 被淘汰源重新出现 ⇒ 计数从零（键被丢弃后重建）
        let again = g.attempt(t0, v4(0, 1234));
        assert_eq!(again.action, SrcAction::Allow);
        assert_eq!(again.in_window, 1, "重建后窗内只有本次");
    }

    /// **判据（S3-2 触发③）**：`refused_recently` 只在窗内为真（窗外的旧拒绝不触发）。
    #[test]
    fn refused_recently_is_windowed() {
        let t0 = Instant::now();
        let w = Duration::from_secs(10);
        let mut g = SrcGate::new(1, w);
        let peer = v4(5, 1000);
        assert!(!g.refused_recently(t0), "从未拒绝 ⇒ 无攻击迹象");
        assert_eq!(g.attempt(t0, peer).action, SrcAction::Allow);
        let t_refuse = t0 + Duration::from_secs(1);
        assert_eq!(g.attempt(t_refuse, peer).action, SrcAction::Refuse);
        assert!(g.refused_recently(t_refuse), "刚拒绝过 ⇒ 真");
        assert!(g.refused_recently(t_refuse + w), "恰好一个窗内 ⇒ 仍真");
        assert!(
            !g.refused_recently(t_refuse + w + Duration::from_nanos(1)),
            "窗外 ⇒ 假"
        );
    }

    /// **判据（S3-1 / S3-2）**：`retry_due` 三条触发条件逐条成立、`never`/`always` 档为常量。
    #[test]
    fn retry_due_three_conditions_and_policies() {
        // ① 未认证在途 ≥ cap/2（cap=64 ⇒ 32）
        assert!(!retry_due(RetryPolicy::Pressure, 31, 64, 0, false));
        assert!(retry_due(RetryPolicy::Pressure, 32, 64, 0, false));
        // ② 同源「未完成/被拒」≥ 5
        assert!(!retry_due(RetryPolicy::Pressure, 0, 64, 4, false));
        assert!(retry_due(RetryPolicy::Pressure, 0, 64, 5, false));
        // ③ 最近窗内有闸拒绝
        assert!(retry_due(RetryPolicy::Pressure, 0, 64, 0, true));
        // 常态（三条全不命中）⇒ 不 Retry（r14 F10：不得让常态重连吃 +1 RTT）
        assert!(!retry_due(RetryPolicy::Pressure, 3, 64, 4, false));
        // 档位
        assert!(retry_due(RetryPolicy::Always, 0, 64, 0, false));
        assert!(!retry_due(RetryPolicy::Never, 999, 64, 99, true));
        // cap 很小也不得出现 0 阈值（`max(1)` 兜底：cap=1 ⇒ 阈值 1）
        assert!(retry_due(RetryPolicy::Pressure, 1, 1, 0, false));
        assert!(!retry_due(RetryPolicy::Pressure, 0, 1, 0, false));
    }

    /// 策略解析/回写互逆 + 非法取值（配置面与 env 面共用同一条真源）。
    #[test]
    fn retry_policy_parse_round_trip() {
        for v in RetryPolicy::VALUES {
            let p = RetryPolicy::parse(v).expect("合法取值");
            assert_eq!(p.text(), v);
        }
        assert_eq!(RetryPolicy::parse("Pressure"), None, "大小写敏感");
        assert_eq!(RetryPolicy::parse(""), None);
        assert_eq!(RetryPolicy::parse("恒"), None);
        assert_eq!(RetryPolicy::default(), RetryPolicy::Pressure, "缺省档");
    }

    /// **判据（S3-3 判据 ⑥ / r14 F12）**：证明失败闸——阈值跨过后进冷却；冷却期内在
    /// 同 devTag 上继续失败不重复报「进入冷却」；冷却到期后重新可触发；
    /// **`threshold=0` ⇒ 整闸关闭**（不冷却、不记账）。
    #[test]
    fn proof_gate_cools_after_threshold_and_respects_off_switch() {
        let t0 = Instant::now();
        let dev = [0xABu8; 8];
        let mut g = ProofGate::new(3);
        assert!(g.enabled());
        assert!(!g.is_cooling(t0, dev), "未达阈值不冷却");
        assert_eq!(g.note_fail(t0, dev), None, "第 1 次失败未跨阈值");
        assert_eq!(g.note_fail(t0, dev), None, "第 2 次");
        assert_eq!(g.note_fail(t0, dev), Some(3), "第 3 次跨阈值 ⇒ 进冷却");
        assert!(g.is_cooling(t0, dev));
        assert_eq!(g.note_fail(t0, dev), None, "冷却期内再失败不重复报");
        // 冷却到期（60s）⇒ 不再冷却；窗内失败也已滚出 ⇒ 需重新攒
        let after = t0 + PROOF_FAIL_COOLDOWN + Duration::from_secs(1);
        assert!(!g.is_cooling(after, dev), "冷却到期");
        assert_eq!(g.note_fail(after, dev), None, "窗内计数已滚出 ⇒ 重新攒");
        // 关闭档：0 = 关
        let mut off = ProofGate::new(0);
        assert!(!off.enabled());
        for _ in 0..50 {
            assert_eq!(off.note_fail(t0, dev), None);
        }
        assert!(!off.is_cooling(t0, dev), "关闭档恒不冷却");
    }

    /// 证明失败闸的表上限：不同 devTag 大量冲击 ⇒ 表 ≤ 上限（冷却位随键一起淘汰）。
    #[test]
    fn proof_gate_table_is_capped() {
        let t0 = Instant::now();
        let mut g = ProofGate::with_cap(1, 4);
        for i in 0..8u8 {
            let mut dev = [0u8; 8];
            dev[0] = i;
            g.note_fail(t0, dev);
        }
        assert_eq!(g.tracked(), 4, "表恒 ≤ 上限");
    }
}
