//! 出口 QUIC 面（M1 设计 §1.1/§1.2/§1.7）：**独立 UDP 端口** + 一枚专用线程 +
//! `current_thread` runtime。
//!
//! 为什么独立端口（§1.1）：`quinn::Endpoint` 独占一枚 UDP socket，而 `serve.listen` 的
//! socket 由 `ServerBind` 独占（poll 集 + 腿 fd 同构）；两栈共用 fd 需自写 demux，纯增
//! 复杂度。M1 是**双栈并存期**（WG 路径原样可用），单一端口会把两条栈的生命周期耦合。
//!
//! socket 的**绑定与退让在调用侧**（`homeway-core` 复用 WG 的 `+1…+9 → 随机` 退让，
//! §1.1）：本模块只把已绑好的 socket 交给 quinn 并起服务面——所以「实际端口」的权威
//! 来源是 [`ExitQuic::local_addr`]（退让后真实端口，进 token/UPnP/status 用）。
//!
//! 线程模型（§1.7）：与客户端岛同构——**专用线程 + `current_thread` runtime**
//! （不启 `rt-multi-thread`：单线程是结构不变量），线程名 [`EXIT_THREAD`]。收工：
//! [`ExitQuic::stop_within`] 有界预算内退出（到点 detach + 收割线程 `hw-quic-exit-reap`）。
//!
//! 本棒（S1a）范围 = 端点 + TransportConfig + 出口 RPK 身份 + E-q1 就绪行；
//! **S1b** 补准入（`hr-reg4` 连接绑定 → `table.register`）、数据面（DATAGRAM ⇄ 引擎
//! `intercept.on_plain`）与出站分流；中继承载（`kind=5` + 自定义 `AsyncUdpSocket`）、
//! UPnP 与 token 端点类是 S1c。
//!
//! 隔离：本目录（`src/exit/**`）属**异步面**——`tools/check-quic-isolation.sh` 第 ② 条
//! 的白名单与 `driver.rs` 同款（其余 `src/` 文件零异步栈名字）。

pub(crate) mod admit;
mod bridge;
mod conn;
// M4 S1：dial 腿（tag=4 的真拨号；M4-design §1/§3）。**同批入隔离门 `ASYNC_FILES`**。
mod dial;
mod intake;
mod pump;
pub(crate) mod rpk;
mod serve;
mod socket;
pub(crate) mod transport;

#[cfg(test)]
mod tests;

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::RawFd;
use std::os::unix::net::UnixStream;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use quinn::{Connection, Endpoint, EndpointConfig};
use tokio::sync::mpsc::{self as tmpsc, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;

use crate::cmd::Logf;
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::sync_util::{lock_unpoison, log_spawn_failed, ExitSignal};
use crate::tuning::StreamLimits;

use bridge::{DropKind, ExitBridge, Outbound, OUTBOUND_QUEUE_MAX};
use socket::{ExitSock, LegTable};

/// 两向边界的公面（引擎消费面）：入站事件 + 准入请求/裁决 + 出站投递结果。
pub use admit::RetryPolicy;
pub use bridge::{
    EngineRejectClass, ExitInbound, ExitSend, Reg4Request, Reg4Verdict, RejectWhy,
};
/// 服务入口（M3 S2，§2.2 方案 B′）：服务侧受理面 `ServiceIntake`（**纯 std**，`homeway-core`
/// 的三服务 accept 循环直接吃它）+ 出口侧入队句柄 `ServiceIntakeTx`。
pub use intake::{IntakeFull, ServiceIntake, ServiceIntakeTx, ServiceIntakes};
/// 腿帧 kind=5（QUIC 载荷）的线字节：真源 = `homeway-core` 的
/// `wtransport::frame::FrameKind::Quic`；本 crate 是叶子、按字节复刻，跨 crate 一致性由
/// `homeway-core` 侧的断言钉住（见 `socket` 模块头）。
pub use socket::FRAME_KIND_QUIC;

/// 出口面线程内共享上下文（端点主循环 + 每连接任务）。
pub(crate) struct FaceCtx {
    pub(crate) stats: Arc<ExitStats>,
    pub(crate) bridge: Arc<ExitBridge>,
    pub(crate) logf: Logf,
    /// 准入总期限（连接被采纳起；§1.3 的 `ADMIT_DEADLINE`）。
    pub(crate) admit_deadline: Duration,
    /// nonce 有效期（Challenge 起；§1.3 的 `NONCE_TTL`）。
    pub(crate) nonce_ttl: Duration,
    /// 连接总数上限（`2 × max_devices`；挑战行的「在途未认证 n/cap」分母）。
    pub(crate) conn_cap: usize,
    /// 证明失败闸（M2 §3.2-⑥；主循环建、每连接任务读写——任务与主循环同线程，
    /// `Mutex` 只为通过 `Arc` 共享）。
    pub(crate) proof_gate: Arc<Mutex<admit::ProofGate>>,
    /// 每 tag 的服务入口入队句柄（M3 S2；`None` = 该服务本期不可用 ⇒ 0x22）。
    pub(crate) intakes: Arc<ServiceIntakes>,
}

/// QUIC 面线程名（与岛 `homeway-quic` 区分：这是**出口侧**的那一枚）。
pub(crate) const EXIT_THREAD: &str = "homeway-quic-exit";
/// 到点收割线程名（镜像 `hw-quic-reap`）。
pub(crate) const REAP_THREAD: &str = "hw-quic-exit-reap";
/// 驱动循环回看节拍（stop 位 + 连接巡检；无事件时周期性回看，不做忙等）。
const TICK: Duration = Duration::from_millis(250);

/// 并发握手上限（M1 设计 §9.3 Q-O；**保守资源上限**——M2 才有抗放大/Retry/限流）。
pub const DEFAULT_HANDSHAKE_CAP: usize = 64;
/// 握手期限（同上：超时不完成即弃——拦「占着槽位不完成」的形态，不替代 M2 的防放大。
/// 副作用登记：期限内未完成的握手丢掉后，对端重传 Initial 会再触发一轮 ⇒ 单源可反复
/// 触发（**上限仍受并发握手闸约束**），M2 的 Retry/限流面承接）。
pub const DEFAULT_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
/// **准入总期限**（M2 设计 §1.3 / 设计门 r14 F3）：连接被采纳起 10s 内未走完 `hr-reg4`
/// 四帧 ⇒ 拒 + 关连接。
///
/// 为什么必须有这一条：`max_idle_timeout=30s` + `keep_alive_interval=10s`（`transport.rs`）
/// 的组合下，只要对端 ACK 服务端的 PING，**「握手完成但不发 Hello」的连接永不自然过期**；
/// 而握手闸只管 `Connecting` 阶段、`pending` 只在收到 Hello 后才存在 ⇒ 单攻击者用
/// `conn_cap` 条这种连接即可长期占满连接槽。本期限把「未认证状态」的生命周期变成有界的。
pub const DEFAULT_ADMIT_DEADLINE: Duration = Duration::from_secs(10);
/// **nonce 有效期**（M2 设计 §1.3）：出口发出 Challenge 起 5s 内未收到 Proof ⇒ 弃连接。
///
/// 计量用**服务端 `Instant`** ⇒ 与客户端时钟解耦（时钟偏移容忍不新增面）。它只需覆盖
/// 「收 Challenge → 回 Proof」的 1 RTT（客户端总预算是它的数倍 ⇒ 不冲突）。
pub const DEFAULT_NONCE_TTL: Duration = Duration::from_secs(5);
/// 记行节流（仓内既有口径「首 3 + 每 100」——`relay/mod.rs` 的 `reject_log_due` 同款）。
pub(crate) fn log_due(n: u64) -> bool {
    n <= 3 || n.is_multiple_of(100)
}

/// 出口 QUIC 面配置（**全 std 类型 + 本岛 newtype**：同步面可见面，不夹带异步栈类型）。
pub struct ExitQuicConfig {
    /// 出口 RPK 私钥种子（**出口身份**：`HKDF(后端静态私钥, "homeway/quic-rpk")`，
    /// 由 `homeway-core` 的装配点派生——见 M1 设计 §1.3/§12-②）。
    pub rpk_seed: Ed25519Seed,
    /// 设备表上限（连接总数上限 = `2 × max_devices`，§9.3 Q-O）。
    pub max_devices: usize,
    /// 并发握手上限（缺省 [`DEFAULT_HANDSHAKE_CAP`]；测试调小以钉死拒绝路径）。
    pub handshake_cap: usize,
    /// 握手期限（缺省 [`DEFAULT_HANDSHAKE_DEADLINE`]；同上）。
    pub handshake_deadline: Duration,
    /// 准入总期限（缺省 [`DEFAULT_ADMIT_DEADLINE`]；M2 §1.3 的双期限之一，测试调短以钉死回收路径）。
    pub admit_deadline: Duration,
    /// nonce 有效期（缺省 [`DEFAULT_NONCE_TTL`]；M2 §1.3 的双期限之二，同上）。
    pub nonce_ttl: Duration,
    /// Retry token 有效期（缺省 [`admit::RETRY_TOKEN_LIFETIME_DEFAULT`] = 5s，**收自 quinn
    /// 缺省 15s**，M2 §3.1 登记；值域 `1s..=60s`）。
    pub retry_token_lifetime: Duration,
    /// 每源滑动窗上限（缺省 [`admit::PER_SRC_FAILS_DEFAULT`] = 16，M2 §14-1 裁决；值域 `1..=1000`）。
    pub per_src_fails: u32,
    /// 每源滑动窗窗长（缺省 [`admit::PER_SRC_WINDOW_DEFAULT`] = 10s；值域 `1s..=1h`）。
    pub per_src_window: Duration,
    /// 证明失败闸阈值（缺省 [`admit::PROOF_FAIL_THRESHOLD_DEFAULT`] = 10；`0` = 关闭该闸）。
    pub proof_fail_threshold: u32,
    /// Retry 策略（缺省 [`RetryPolicy::Pressure`] = 压力触发，M2 §3.1 推荐档）。
    pub retry_policy: RetryPolicy,
    /// **流面限制**（M3 §1.7 / §15-3）：并发上限 / 每流接收窗 / 连接级发送窗。
    ///
    /// 与客户端**同一份值域**（`crate::tuning::StreamLimits`）：quinn 的 TP 是「本端接收
    /// 对方开流的上限」⇒ 出口广告 `64` 才允许一条设备连接开 64 条服务流；两端值不同 =
    /// 岛侧自记账与对端信用不一致（§1.7 的账全错）。
    pub streams: StreamLimits,
    /// **服务入口**（M3 S2，§2.2 方案 B′）：tag 1/2/3 的入队句柄；未装配的服务 ⇒ `0x22`。
    /// 缺省全空 = S1 的「QUIC 档不提供这四个服务」形态（既有测试零改）。
    pub intakes: ServiceIntakes,
}

impl ExitQuicConfig {
    /// 生产缺省（seed 与设备表上限由调用侧给；Q-O 的两个上限、M2 的双期限与 §3 的抗放大
    /// 初值全取设计定值——`serve.quic_admit` 的显式配置覆盖在 `homeway-core` 装配点叠加）。
    pub fn new(rpk_seed: Ed25519Seed, max_devices: usize) -> Self {
        Self {
            rpk_seed,
            max_devices,
            handshake_cap: DEFAULT_HANDSHAKE_CAP,
            handshake_deadline: DEFAULT_HANDSHAKE_DEADLINE,
            admit_deadline: DEFAULT_ADMIT_DEADLINE,
            nonce_ttl: DEFAULT_NONCE_TTL,
            retry_token_lifetime: admit::RETRY_TOKEN_LIFETIME_DEFAULT,
            per_src_fails: admit::PER_SRC_FAILS_DEFAULT,
            per_src_window: admit::PER_SRC_WINDOW_DEFAULT,
            proof_fail_threshold: admit::PROOF_FAIL_THRESHOLD_DEFAULT,
            retry_policy: RetryPolicy::Pressure,
            streams: StreamLimits::design(),
            intakes: ServiceIntakes::default(),
        }
    }

    /// 装配服务入口（M3 S2；`homeway-core` 的引擎装配点在起本面之前把三个 intake 的
    /// 入队句柄挂进来——服务侧 intake 同时交给各服务的 `serve_stoppable`）。
    pub fn with_intakes(mut self, intakes: ServiceIntakes) -> Self {
        self.intakes = intakes;
        self
    }

    /// 连接总数上限（`2 × max_devices`，§9.3 Q-O；饱和乘防溢出）。
    fn conn_cap(&self) -> usize {
        self.max_devices.saturating_mul(2)
    }

    /// 叠加 `serve.quic_admit` 的显式配置（M2 §3.2；缺省 = [`admit::AdmitLimits::default`]，
    /// 与 [`Self::new`] 逐值相同 ⇒ 缺省不改行为）。
    pub fn with_admit(mut self, limits: admit::AdmitLimits) -> Self {
        self.retry_token_lifetime = limits.retry_token_lifetime;
        self.per_src_fails = limits.per_src_fails;
        self.per_src_window = limits.per_src_window;
        self.nonce_ttl = limits.nonce_ttl;
        self.admit_deadline = limits.admit_deadline;
        self.proof_fail_threshold = limits.proof_fail_threshold;
        self.retry_policy = limits.retry_policy;
        self
    }
}

/// 起点失败（类型化；`#[non_exhaustive]` 防下游穷举）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExitQuicErr {
    /// 端点/socket 起不来（quinn 的 `Endpoint::new`，或 runtime 建不起来）。
    #[error("QUIC 端点起不来（{0}）")]
    Endpoint(#[from] io::Error),
    /// 服务端身份（RPK）构建失败。
    #[error("QUIC 服务端身份构建失败（{0}）")]
    Identity(#[from] crate::rpk::RpkErr),
}

/// 出口 QUIC 面的运行读数（同步面轮询；无阻塞）。
#[derive(Clone, Debug, Default)]
pub struct ExitQuicSnapshot {
    /// 当前存活连接数（已采纳、未结束）。
    pub connections: u64,
    /// 累计采纳连接数。
    pub admitted: u64,
    /// 观测到的路径变更次数（`remote_address()` 变化——S1b 的 E-q2 行前身）。
    pub path_changes: u64,
    /// 握手失败次数（对端放弃 / 协议错；期限到点单列 [`Self::handshake_timeouts`]）。
    pub handshake_failed: u64,
    /// [`Self::handshake_failed`] 的**子集**：对端在握手期**主动关闭**的条数（M2 §14-1④ 的
    /// 归因面——「出口侧能否区分『对端主动关闭』与『对端静默』」：**能**，以 quinn 的
    /// `ConnectionError` 变体为据）。
    ///
    /// 口径写死在这里：`ConnectionClosed`（收到对端 CONNECTION_CLOSE 帧——含 TLS alert 类
    /// crypto 错误）与 `ApplicationClosed`（对端应用层关闭）计入本计数；`TimedOut`（静默到
    /// idle 超时）/`Reset`/`VersionMismatch`/`TransportError`/`LocallyClosed` **不计**。
    /// 注记（如实）：`TransportError` 一档既可能来自「对端发了坏帧」也可能来自「本地 TLS 栈
    /// 因对端 alert 报错」⇒ 从变体**无法**反推对端是否发过帧，故该档统一归「非主动关闭」。
    ///
    /// **计数集不放宽**（M2 §14-1① 裁定）：本字段只作**归因**，每源闸/证明失败闸的输入集
    /// 不变（对端主动关闭同样计入「未完成」——攻击者也能主动 abort）。
    pub handshake_peer_closed: u64,
    /// 当前在途握手数（已收到 Initial、尚未完成）。
    pub handshakes_in_flight: u64,
    /// 因**并发握手超限**拒绝的次数（§9.3 Q-O）。
    pub handshake_refused: u64,
    /// 因**连接总数超限**拒绝的次数（§9.3 Q-O）。
    pub conn_refused: u64,
    /// 因**握手期限**到点放弃的次数（§9.3 Q-O）。
    pub handshake_timeouts: u64,
    /// 准入通过（绑定成功）的连接/刷新次数（S1-3）。
    pub regs_accepted: u64,
    /// 准入被拒次数（**含刷新帧**：帧非法/版本不符/再准入/nonce 类/MAC 类/引擎拒绝/未绑定刷新）。
    pub regs_rejected: u64,
    /// 已发出的 Challenge（`C4`）帧数（M2 §1.6；挑战行的计数源）。
    pub challenges_issued: u64,
    /// **未发挑战**的准入拒绝数（帧非法/版本不符/已绑定再准入/未绑定刷新/重复 Hello——
    /// 设计 §1.1 的「更便宜的拒绝面」在本期的可观测落点）。
    pub challenges_refused: u64,
    /// Proof 阶段的拒绝数（nonce 缺失/过期/已消费、MAC 不符、引擎裁决拒绝、刷新身份不符）。
    pub proof_rejected: u64,
    /// pending 过期（Challenge 已发、`NONCE_TTL` 内未收到 Proof ⇒ 弃连接）。
    pub pending_expired: u64,
    /// 准入总期限到点（连接被采纳起 `ADMIT_DEADLINE` 内未走完四帧 ⇒ 弃连接——含
    /// 「握手完成但不发 Hello」，设计门 r14 F3）。
    pub admit_timeouts: u64,
    /// 已发出的 **Retry**（地址校验挑战）包数（M2 §3.1 观测面；生产者 = S3）。
    pub retry_sent: u64,
    /// 每源闸拒绝的连接尝试数（M2 §3.2-④/§3.3；生产者 = S3——「重连洪泛有界」的主计数）。
    ///
    /// 口径（设计门 r14 F18）：与 quinn `EndpointStats::refused_handshakes` **不同源**——
    /// 后者只含应用层 `refuse()`（本计数亦然），**不含** <1200B 短包与端点饱和的静默丢。
    pub flood_refused: u64,
    /// 证明失败闸**进入冷却**的次数（M2 §3.2-⑥；行「证明失败闸」的节流计数源）。
    pub proof_cooldowns: u64,
    /// 丢弃：超限（内层包 > `max_datagram_size()`）。
    pub drop_too_large: u64,
    /// 丢弃：发送缓冲满（per-conn 预检不过 ∨ 引擎→面出站队列满）。
    pub drop_send_buffer_full: u64,
    /// 丢弃：未登记（未登记连接的数据报 / 入境队列满 / 出站目标无绑定）。
    pub drop_unregistered: u64,
    /// 丢弃：源校验拒（复刻 `device.rs` 的 `src_allowed`）。
    pub drop_src_rejected: u64,
    // ---------- M3 S1/S2：服务流面（出口侧；§8.2-7 的 E-q5 行与 §8.2-12 的 `quic` 段） ----------
    /// 已受理的服务流条数（probe 回显 + tag 1/2/3 的**入队**即计；`0x23` 拒的不计）。
    pub streams_open: u64,
    /// 服务流拒绝条数（`0x21` 未知 tag / `0x22` 服务不可用 / `0x23` 入口队列满 /
    /// `0x24` 未绑定 / `0x27` tag 读取超时）。
    pub stream_refused: u64,
    /// 已收工的服务流条数（泵/回显两侧任一收线即计——收工明细见 E-q5 行）。
    pub streams_closed: u64,
    /// 服务流搬运的应用字节（**上行**：客户端 → 出口侧读到的量）。
    pub stream_bytes_in: u64,
    /// 服务流搬运的应用字节（**下行**：出口 → 客户端的量）。
    pub stream_bytes_out: u64,
}

/// 计量面（原子直读——**反应式**，不必等巡检拍；`snapshot()` 由它组装）。
#[derive(Default)]
pub(crate) struct ExitStats {
    connections: AtomicU64,
    admitted: AtomicU64,
    path_changes: AtomicU64,
    handshake_failed: AtomicU64,
    handshake_peer_closed: AtomicU64,
    handshakes_in_flight: AtomicU64,
    handshake_refused: AtomicU64,
    conn_refused: AtomicU64,
    handshake_timeouts: AtomicU64,
    regs_accepted: AtomicU64,
    regs_rejected: AtomicU64,
    challenges_issued: AtomicU64,
    challenges_refused: AtomicU64,
    proof_rejected: AtomicU64,
    pending_expired: AtomicU64,
    admit_timeouts: AtomicU64,
    retry_sent: AtomicU64,
    flood_refused: AtomicU64,
    proof_cooldowns: AtomicU64,
    drop_too_large: AtomicU64,
    drop_send_buffer_full: AtomicU64,
    drop_unregistered: AtomicU64,
    drop_src_rejected: AtomicU64,
    streams_open: AtomicU64,
    stream_refused: AtomicU64,
    streams_closed: AtomicU64,
    stream_bytes_in: AtomicU64,
    stream_bytes_out: AtomicU64,
}

impl ExitStats {
    fn snapshot(&self) -> ExitQuicSnapshot {
        ExitQuicSnapshot {
            connections: self.connections.load(Ordering::SeqCst),
            admitted: self.admitted.load(Ordering::SeqCst),
            path_changes: self.path_changes.load(Ordering::SeqCst),
            handshake_failed: self.handshake_failed.load(Ordering::SeqCst),
            handshake_peer_closed: self.handshake_peer_closed.load(Ordering::SeqCst),
            handshakes_in_flight: self.handshakes_in_flight.load(Ordering::SeqCst),
            handshake_refused: self.handshake_refused.load(Ordering::SeqCst),
            conn_refused: self.conn_refused.load(Ordering::SeqCst),
            handshake_timeouts: self.handshake_timeouts.load(Ordering::SeqCst),
            regs_accepted: self.regs_accepted.load(Ordering::SeqCst),
            regs_rejected: self.regs_rejected.load(Ordering::SeqCst),
            challenges_issued: self.challenges_issued.load(Ordering::SeqCst),
            challenges_refused: self.challenges_refused.load(Ordering::SeqCst),
            proof_rejected: self.proof_rejected.load(Ordering::SeqCst),
            pending_expired: self.pending_expired.load(Ordering::SeqCst),
            admit_timeouts: self.admit_timeouts.load(Ordering::SeqCst),
            retry_sent: self.retry_sent.load(Ordering::SeqCst),
            flood_refused: self.flood_refused.load(Ordering::SeqCst),
            proof_cooldowns: self.proof_cooldowns.load(Ordering::SeqCst),
            drop_too_large: self.drop_too_large.load(Ordering::SeqCst),
            drop_send_buffer_full: self.drop_send_buffer_full.load(Ordering::SeqCst),
            drop_unregistered: self.drop_unregistered.load(Ordering::SeqCst),
            drop_src_rejected: self.drop_src_rejected.load(Ordering::SeqCst),
            streams_open: self.streams_open.load(Ordering::SeqCst),
            stream_refused: self.stream_refused.load(Ordering::SeqCst),
            streams_closed: self.streams_closed.load(Ordering::SeqCst),
            stream_bytes_in: self.stream_bytes_in.load(Ordering::SeqCst),
            stream_bytes_out: self.stream_bytes_out.load(Ordering::SeqCst),
        }
    }
}

/// 出口 QUIC 面的**只读计数句柄**（M3 S2）：`Clone` + `Send` + `Sync`，同步面（`homeway-core`
/// 的 `serve status --json` 段）直读快照——不占出口线程、不碰面句柄的生命周期（面的收工
/// 与句柄 drop 无关）。
#[derive(Clone)]
pub struct ExitStatsHandle {
    stats: Arc<ExitStats>,
}

impl ExitStatsHandle {
    /// 运行读数（原子直读；与 [`ExitQuic::snapshot`] 同源同形）。
    pub fn snapshot(&self) -> ExitQuicSnapshot {
        self.stats.snapshot()
    }
}

/// 出口 QUIC 面句柄（同步面）：地址/公开身份/读数/两向投递/收工（幂等）。
pub struct ExitQuic {
    local_addr: SocketAddr,
    rpk_public_key: RpkPublicKey,
    stop_tx: UnboundedSender<()>,
    exit: Arc<ExitSignal>,
    handle: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<ExitStats>,
    bridge: Arc<ExitBridge>,
    /// 中继腿表（发送侧路由；M1 §1.6）：引擎线程写、QUIC 线程读。
    legs: Arc<LegTable>,
    /// 引擎 → QUIC 面的注入队列（腿上的 kind=5 载荷；引擎线程 `try_send` 非阻塞）。
    inject_tx: socket::InjectTx,
    logf: Logf,
}

impl ExitQuic {
    /// 起出口 QUIC 面。
    ///
    /// `socket` = **已绑定**的 UDP socket（退让语义在调用侧：复用 WG 的
    /// `+1…+9 → 随机`，M1 设计 §1.1）；交给 quinn 后本模块不再碰它。
    pub fn start(
        socket: UdpSocket,
        cfg: ExitQuicConfig,
        logf: Logf,
    ) -> Result<ExitQuic, ExitQuicErr> {
        // 端点与 runtime 都建在**专用线程内**（`tokio::net::UdpSocket::from_std` 需要
        // runtime 上下文）；起点失败的两种情形（runtime 建不出来 / 端点或身份建不出来）
        // 都必须在 `start` 返回前定音——不留「起了但死的面」。
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(SocketAddr, RpkPublicKey), ExitQuicErr>>();
        let (stop_tx, stop_rx) = tmpsc::unbounded_channel::<()>();
        let exit = Arc::new(ExitSignal::new());
        let stats = Arc::new(ExitStats::default());
        // ---- 两向边界（M1 §1.7）：唤醒 self-pipe + 出站队列（引擎侧 try_send 非阻塞）----
        let (wake_tx, wake_rx) = UnixStream::pair().map_err(ExitQuicErr::Endpoint)?;
        wake_tx.set_nonblocking(true).map_err(ExitQuicErr::Endpoint)?;
        wake_rx.set_nonblocking(true).map_err(ExitQuicErr::Endpoint)?;
        let (out_tx, out_rx) = tmpsc::channel::<Outbound>(OUTBOUND_QUEUE_MAX);
        let bridge = Arc::new(ExitBridge::new(
            Arc::clone(&stats),
            Arc::clone(&logf),
            wake_tx,
            wake_rx,
            out_tx,
        ));
        // ---- 中继承载（M1 §1.6）：腿表 + 引擎注入队列 ----
        // 注入队列与「面 → 引擎」的入站队列同上限（8192 条，§6.4）：满 ⇒ 丢 + 计数。
        let legs = Arc::new(LegTable::default());
        let (inject_tx, inject_rx) = tmpsc::channel::<socket::InjPkt>(bridge::INBOUND_QUEUE_MAX);

        let handle = thread::Builder::new()
            .name(EXIT_THREAD.into())
            .spawn({
                let logf = Arc::clone(&logf);
                let exit = Arc::clone(&exit);
                let stats = Arc::clone(&stats);
                let bridge = Arc::clone(&bridge);
                let legs = Arc::clone(&legs);
                move || {
                    thread_body(
                        socket, cfg, logf, ready_tx, stop_rx, exit, stats, bridge, legs, out_rx,
                        inject_rx,
                    )
                }
            })
            .map_err(|e| {
                log_spawn_failed(&logf, EXIT_THREAD, &e, "出口 QUIC 面缺席（WG 面不受影响）");
                ExitQuicErr::Endpoint(e)
            })?;

        match ready_rx.recv() {
            Ok(Ok((local_addr, rpk_public_key))) => Ok(ExitQuic {
                local_addr,
                rpk_public_key,
                stop_tx,
                exit,
                handle: Mutex::new(Some(handle)),
                stats,
                bridge,
                legs,
                inject_tx,
                logf,
            }),
            Ok(Err(e)) => {
                let _ = handle.join(); // 起点失败即退；join 掉不让线程悬空
                Err(e)
            }
            Err(_) => {
                // 线程在报告起点前就没了（panic 展开）：join 取回证据
                let _ = handle.join();
                Err(ExitQuicErr::Endpoint(io::Error::other(
                    "QUIC 面线程在就绪前退出（见其 panic 记行）",
                )))
            }
        }
    }

    /// 实际监听地址（**退让后的真实端口**——token/UPnP/status 的唯一来源，§1.1）。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 出口 RPK 公钥（进 token 的 32B；M1 设计 §12-②）。
    pub fn rpk_public_key(&self) -> RpkPublicKey {
        self.rpk_public_key
    }

    /// 运行读数（轮询；无阻塞——原子直读，不含锁等待）。
    pub fn snapshot(&self) -> ExitQuicSnapshot {
        self.stats.snapshot()
    }

    /// 只读计数句柄（M3 S2：`serve status --json` 的 `quic` 段在**装配点**留一份，
    /// 驱动线程仍独占面句柄；句柄 drop 不影响面的生命周期）。
    pub fn stats_handle(&self) -> ExitStatsHandle {
        ExitStatsHandle {
            stats: Arc::clone(&self.stats),
        }
    }

    /// 唤醒 fd（**引擎 poll 集**用它：入站队列非空时此 fd 可读；§1.4）。
    ///
    /// 所有权在句柄（引擎**不得 close**；句柄 drop 后该 fd 失效——引擎须同生命周期持有）。
    pub fn wake_fd(&self) -> RawFd {
        self.bridge.wake_fd()
    }

    /// 排空唤醒字节 + 入站事件（引擎 poll 醒来后调；无事件时是「读空管道 + 空队列」的
    /// 常量开销）。回调在队列锁外执行（引擎侧工作不攥着锁）。
    pub fn drain_inbound(&self, mut f: impl FnMut(ExitInbound)) {
        self.bridge.drain_wake();
        for item in self.bridge.take_inbound() {
            f(item);
        }
    }

    /// 引擎出站面（M1 §1.5）：把内层明文包交给该设备的 QUIC 连接。
    ///
    /// - [`ExitSend::Handled`]：QUIC 面承接（入队；队列满 = 已丢 + 计数）；
    /// - [`ExitSend::Unbound`]：该设备无绑定（或面已死）⇒ 调用方按 WG 原样走。
    pub fn send_to_pub(&self, pubkey: &[u8; 32], pkt: &[u8]) -> ExitSend {
        if self.exit.is_exited() {
            return ExitSend::Unbound; // 面已死：出站回落 WG（不黑洞）
        }
        self.bridge.send_to_pub(pubkey, pkt)
    }

    /// 摘某设备的绑定并关闭其连接（设备被摘除/身份轮换的 WG 侧路径；§1.3 撤销/轮换）。
    pub fn unbind_pub(&self, pubkey: &[u8; 32]) {
        self.bridge.unbind_pub(pubkey);
    }

    // ---- 中继承载（M1 §1.6）：腿表 + 注入队列的同步面 ============================

    /// 登记一条中继腿的**发送句柄**（`sock` = 该腿 socket 的 `try_clone`；引擎线程调）。
    /// 同远端重复登记 = 替换句柄并撤「最近摘除」标记（`register_leg` 的重建语义）。
    pub fn leg_open(&self, remote: SocketAddr, sock: UdpSocket) {
        self.legs.open(remote, sock);
    }

    /// 摘除一条腿（保留「最近摘除」窗：窗内对该远端的发送**丢**而不回落直连）。
    pub fn leg_close(&self, remote: SocketAddr) {
        self.legs.close(remote);
    }

    /// 当前腿远端集（引擎侧差分的读面）。
    pub fn leg_remotes(&self) -> Vec<SocketAddr> {
        self.legs.remotes()
    }

    /// 该腿远端是否已登记（引擎侧差分的读面）。
    pub fn has_leg(&self, remote: &SocketAddr) -> bool {
        self.legs.has(remote)
    }

    /// 注入一条腿上的 QUIC 报文（引擎线程；`src` = 该腿的远端地址）。返回 `false` =
    /// 面已收工（调用方不必再注入）；队列满 = 已丢 + 计数（§6.4），仍返回 `true`。
    pub fn inject_leg(&self, src: SocketAddr, payload: Vec<u8>) -> bool {
        if self.exit.is_exited() {
            return false;
        }
        ExitSock::inject(&self.inject_tx, &self.bridge, src, payload)
    }

    /// QUIC 面线程是否已退出。
    pub fn is_finished(&self) -> bool {
        self.exit.is_exited()
    }

    /// 有界收工：预算内退出返回 `true`；到点 detach 交收割线程并返回 `false`
    /// （返回值语义与 `Island::stop_within` 逐条同构：重入即 `true`）。
    pub fn stop_within(&self, deadline: Instant) -> bool {
        let _ = self.stop_tx.send(()); // 幂等：重复发送无害（接收端已退出）
        let h = match lock_unpoison(&self.handle).take() {
            None => return true, // 重入/已 detach：收工请求已发过
            Some(h) => h,
        };
        if self.exit.wait_exit(deadline) {
            join_and_log(h, &self.logf);
            return true;
        }
        let logf = Arc::clone(&self.logf);
        let spawned = thread::Builder::new()
            .name(REAP_THREAD.into())
            .spawn(move || {
                (logf)(&format!(
                    "{REAP_THREAD}: 到点 detach—— QUIC 面线程未在收工预算内退出，由本线程等待其自行退出"
                ));
                join_and_log(h, &logf);
            });
        if let Err(e) = spawned {
            log_spawn_failed(&self.logf, REAP_THREAD, &e, "QUIC 面线程的 join 侧记录缺席");
        }
        false
    }

    /// 无界收工（`Drop` 兜底；世代收工链一律走 [`Self::stop_within`]）。
    pub fn stop(&self) {
        let _ = self.stop_tx.send(());
        if let Some(h) = lock_unpoison(&self.handle).take() {
            join_and_log(h, &self.logf);
        }
    }
}

impl Drop for ExitQuic {
    fn drop(&mut self) {
        // 兜底：已 detach（stop 位已置）时不再等待——不挂死调用方。
        if !self.exit.is_exited() {
            self.stop();
        }
    }
}

/// join 收口：QUIC 面线程 panic 时记行（与岛的 `join_and_classify` 同分工；出口面的
/// 不健康分类（`mark_unhealthy_if_current(gen, …)`）与引擎巡检同批接——S1b）。
fn join_and_log(h: JoinHandle<()>, logf: &Logf) {
    if h.join().is_err() {
        (*logf)(&format!("quic: {EXIT_THREAD} 线程 panic —— 本世代 QUIC 面已死"));
    }
}

/// QUIC 面线程体：`catch_unwind` ⇒ 记行 ⇒ 置退出位（不复用该线程）。
#[allow(clippy::too_many_arguments)] // 线程起点一次性移交全部状态（同 `driver_loop` 的口径）
fn thread_body(
    socket: UdpSocket,
    cfg: ExitQuicConfig,
    logf: Logf,
    ready_tx: Sender<Result<(SocketAddr, RpkPublicKey), ExitQuicErr>>,
    stop_rx: UnboundedReceiver<()>,
    exit: Arc<ExitSignal>,
    stats: Arc<ExitStats>,
    bridge: Arc<ExitBridge>,
    legs: Arc<LegTable>,
    out_rx: tmpsc::Receiver<Outbound>,
    inject_rx: socket::InjectRx,
) {
    let res = catch_unwind(AssertUnwindSafe(|| {
        run_exit(socket, cfg, &logf, ready_tx, stop_rx, &stats, &bridge, &legs, out_rx, inject_rx)
    }));
    if let Err(payload) = res {
        let msg = crate::driver::panic_msg(payload.as_ref());
        (*logf)(&format!(
            "quic: 出口 QUIC 面 panic（{msg}）—— 判不健康，线程退出（不复用该线程）"
        ));
    }
    exit.mark_exited();
}

/// 一个存活连接 + 最近观测到的远端地址（路径变更观测面：`remote_address()` 变化）。
struct LiveConn {
    conn: Connection,
    conn_id: u64,
    remote: SocketAddr,
}

/// 在途握手的结果（三态：采纳 / 失败 / 期限到点——各自的计数与记行口径不同）。
///
/// `Failed` 携带**归因位**（M2 §14-1④）：对端在握手期是否**主动关闭**（收到了关闭帧）。
/// 该位只进 [`ExitQuicSnapshot::handshake_peer_closed`]（归因面），**不改**任何闸的输入集。
enum HandshakeOutcome {
    Accepted(SocketAddr, Connection),
    Failed(SocketAddr, FailureKind),
    Deadline(SocketAddr),
}

/// 握手失败的归因位（口径见 [`ExitQuicSnapshot::handshake_peer_closed`] 的文档）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FailureKind {
    /// 对端**主动关闭**（`ConnectionClosed`/`ApplicationClosed`）。
    PeerClosed,
    /// 静默或其他（`TimedOut`/`Reset`/`VersionMismatch`/`TransportError`/`LocallyClosed`）。
    Other,
}

/// quinn 的握手错误 → 归因位（变体映射写死在此，随上游变体增删只改这一处）。
fn failure_kind(e: &quinn::ConnectionError) -> FailureKind {
    match e {
        quinn::ConnectionError::ConnectionClosed(_) | quinn::ConnectionError::ApplicationClosed(_) => {
            FailureKind::PeerClosed
        }
        _ => FailureKind::Other,
    }
}

/// 端点到点前的装配与主循环（**全在专用线程的 `current_thread` runtime 内**）。
#[allow(clippy::too_many_arguments)] // 线程起点单参化无收益（同 `thread_body`）
fn run_exit(
    socket: UdpSocket,
    cfg: ExitQuicConfig,
    logf: &Logf,
    ready_tx: Sender<Result<(SocketAddr, RpkPublicKey), ExitQuicErr>>,
    mut stop_rx: UnboundedReceiver<()>,
    stats: &Arc<ExitStats>,
    bridge: &Arc<ExitBridge>,
    legs: &Arc<LegTable>,
    mut out_rx: tmpsc::Receiver<Outbound>,
    inject_rx: socket::InjectRx,
) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
            return;
        }
    };
    rt.block_on(async move {
        // ---- 服务端身份（出口 RPK：RFC 7250；私钥种子 → PKCS#8 → SPKI 出示）----
        // `retry_token_lifetime` 随服务端配置下发（§3.1：收自 quinn 缺省 15s 到 5s）。
        // M3 §15-3：流面限制随 `TransportConfig` 下发（两端同一份值域）。
        let (server_cfg, rpk_public_key) = match rpk::server_config(
            &cfg.rpk_seed,
            cfg.retry_token_lifetime,
            cfg.streams,
        ) {
            Ok(v) => v,
            Err(e) => {
                let _ = ready_tx.send(Err(ExitQuicErr::Identity(e)));
                return;
            }
        };
        // ---- 自定义 socket（M1 §1.6）：直连端口 socket + 腿表 + 引擎注入队列 ----
        // `UdpSocket::from_std` 需要 runtime 上下文（本函数在 `rt.block_on` 内）。
        let local_addr = match socket.local_addr() {
            Ok(a) => a,
            Err(e) => {
                let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
                return;
            }
        };
        // 非阻塞是 `tokio::net::UdpSocket::from_std` 的前置（调用侧通常已设——引擎装配
        // 时就设过；这里幂等地再保一次，让「直接把 std socket 交给面」的调用形态也成立）。
        if let Err(e) = socket.set_nonblocking(true) {
            let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
            return;
        }
        let direct = match tokio::net::UdpSocket::from_std(socket) {
            Ok(s) => s,
            Err(e) => {
                let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
                return;
            }
        };
        let abs = Arc::new(ExitSock::new(
            direct,
            local_addr,
            Arc::clone(legs),
            inject_rx,
            Arc::clone(bridge),
        ));
        // ---- 端点（抽象 socket：直连 + 腿两条物理路径，见 `socket` 模块头）----
        let endpoint = match Endpoint::new_with_abstract_socket(
            EndpointConfig::default(),
            Some(server_cfg),
            abs,
            Arc::new(quinn::TokioRuntime),
        ) {
            Ok(ep) => ep,
            Err(e) => {
                let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
                return;
            }
        };
        // ---- E-q1（判据行：出口 QUIC 端点起后一行；形态见 M1 设计 §3.4）----
        (*logf)(&format!(
            "quic: 端点就绪（{local_addr}，migration={}，initial_mtu={}，datagram 缓冲 {}B）",
            transport::MIGRATION,
            transport::INITIAL_MTU,
            transport::DATAGRAM_BUFFER
        ));
        // ---- 抗放大面生效值（§3.2；`always` 档的代价记行 = §5-11 登记项）----
        (*logf)(&format!(
            "quic: 流面参数（bidi={} uni={} recv_window={}B conn_recv_window={}B send_window={}B）",
            cfg.streams.max_bidi,
            cfg.streams.max_uni,
            cfg.streams.recv_window,
            cfg.streams.conn_recv_window,
            cfg.streams.send_window
        ));
        (*logf)(&format!(
            "quic: 抗放大面（retry={}；retry_token_lifetime={:?}；每源 {}/{:?}；证明失败闸 {}）",
            cfg.retry_policy.text(),
            cfg.retry_token_lifetime,
            cfg.per_src_fails,
            cfg.per_src_window,
            if cfg.proof_fail_threshold == 0 {
                "关闭".to_owned()
            } else {
                cfg.proof_fail_threshold.to_string()
            }
        ));
        if cfg.retry_policy == RetryPolicy::Always {
            (*logf)(
                "⚠️ quic: retry_policy=always —— 常态每次建连/重连 +1 RTT（真机 LTE ≈50ms；§3.1 登记的代价，仅排障用）",
            );
        }
        if ready_tx.send(Ok((local_addr, rpk_public_key))).is_err() {
            return; // 调用侧已放弃（start 失败路径）——直接收摊
        }

        let mut conns: Vec<LiveConn> = Vec::new();
        // 在途握手（并发上限与期限都挂在这张表上；`JoinSet` 保证收工时全部 abort）
        let mut handshakes: JoinSet<HandshakeOutcome> = JoinSet::new();
        // 每连接任务（控制流登记 + 数据报收包；`JoinSet` 同款收工 abort）
        let mut tasks: JoinSet<()> = JoinSet::new();
        let mut next_conn_id: u64 = 1;
        let conn_cap = cfg.conn_cap();
        // ---- 抗放大闸表（M2 §3.2-④/§3.2-⑥；纯 std，主循环线程独占 `src_gate`）----
        let mut src_gate = admit::SrcGate::new(cfg.per_src_fails, cfg.per_src_window);
        let proof_gate = Arc::new(Mutex::new(admit::ProofGate::new(cfg.proof_fail_threshold)));
        let ctx = Arc::new(FaceCtx {
            stats: Arc::clone(stats),
            bridge: Arc::clone(bridge),
            logf: Arc::clone(logf),
            admit_deadline: cfg.admit_deadline,
            nonce_ttl: cfg.nonce_ttl,
            conn_cap,
            proof_gate: Arc::clone(&proof_gate),
            intakes: Arc::new(cfg.intakes.clone()),
        });
        loop {
            tokio::select! {
                _ = stop_rx.recv() => break,
                inc = endpoint.accept() => match inc {
                    Some(incoming) => {
                        let peer = incoming.remote_address();
                        let now = Instant::now();
                        // ============== 抗放大（M2 §3.1/§3.2）：先闸后 Retry ==============
                        // ① 每源滑动窗闸（§3.2-④；键 = v4 /32、v6 /64 前缀）。顺序裁决
                        //    （§3.1）：**先闸（廉价、本地）后 Retry**——闸拒绝 = `refuse()`
                        //    发 CONNECTION_REFUSED，同样不给攻击者建立状态。
                        let gate_out = src_gate.attempt(now, peer);
                        if gate_out.action == admit::SrcAction::Refuse {
                            incoming.refuse();
                            let n = stats.flood_refused.fetch_add(1, Ordering::SeqCst) + 1;
                            if log_due(n) {
                                (*logf)(&format!(
                                    "quic: 握手洪泛拒绝（{src:?} 在 {win:?} 内第 {k} 次尝试——已拒；第 {n} 次）",
                                    src = admit::SrcKey::of(peer).text(),
                                    win = cfg.per_src_window,
                                    k = gate_out.in_window
                                ));
                            }
                        } else {
                            // ② Retry 决策（§3.1 三条；`never` 档恒假、`always` 档恒真）：
                            //    ① 未认证在途 ≥ handshake_cap/2 ② 同源「未完成/被拒」≥5
                            //    ③ 窗内有闸拒绝。三条都按「未完成/被拒」计数 ⇒ 常态赛跑不吃
                            //    +1 RTT（r14 F10）。
                            let inflight = conns.len().saturating_sub(bridge.bound_count());
                            let due = admit::retry_due(
                                cfg.retry_policy,
                                inflight,
                                cfg.handshake_cap,
                                src_gate.pending(now, peer),
                                src_gate.refused_recently(now),
                            );
                            // 守卫（r14 F19）：用 `!validated() && may_retry()` 避免进 `Err`；
                            // 真进了 `Err` 也**必须** `into_incoming()` 取回——直接丢错误值会连带
                            // drop `Incoming` ⇒ 隐式 `refuse()`（假拒绝）。
                            let incoming = if due
                                && !incoming.remote_address_validated()
                                && incoming.may_retry()
                            {
                                match incoming.retry() {
                                    Ok(()) => {
                                        let n = stats.retry_sent.fetch_add(1, Ordering::SeqCst) + 1;
                                        if log_due(n) {
                                            (*logf)(&format!(
                                                "quic: 地址校验挑战（{peer}；在途未认证 {inflight}/{conn_cap}；第 {n} 次）"
                                            ));
                                        }
                                        None
                                    }
                                    Err(e) => Some(e.into_incoming()),
                                }
                            } else {
                                Some(incoming)
                            };
                            if let Some(incoming) = incoming {
                                // 闸①：连接总数（**口径收紧一档**：存活连接 + 在途握手一起算——
                                // 否则「在途握手转正」会把上限撑破；§9.3 Q-O 的原义只写了连接总数）
                                let held = (conns.len() + handshakes.len()) as u64;
                                if held >= conn_cap as u64 {
                                    incoming.refuse();
                                    let n = stats.conn_refused.fetch_add(1, Ordering::SeqCst) + 1;
                                    if log_due(n) {
                                        (*logf)(&format!(
                                            "quic: 拒新连接（连接总数 {held}/{conn_cap} 超限，来自 {peer}；第 {n} 次）"
                                        ));
                                    }
                                } else if handshakes.len() >= cfg.handshake_cap {
                                    // 闸②：并发握手上限（Q-O 的 64）
                                    incoming.refuse();
                                    let n = stats.handshake_refused.fetch_add(1, Ordering::SeqCst) + 1;
                                    if log_due(n) {
                                        (*logf)(&format!(
                                            "quic: 拒新连接（并发握手 {}/{cap} 超限，来自 {peer}；第 {n} 次）",
                                            handshakes.len(),
                                            cap = cfg.handshake_cap
                                        ));
                                    }
                                } else {
                                    // 闸③：握手期限（到点即弃；丢 `Connecting` = quinn 侧关连接）
                                    let deadline = cfg.handshake_deadline;
                                    stats.handshakes_in_flight.fetch_add(1, Ordering::SeqCst);
                                    handshakes.spawn(async move {
                                        match incoming.accept() {
                                            Ok(connecting) => match tokio::time::timeout(deadline, connecting).await {
                                                Ok(Ok(conn)) => HandshakeOutcome::Accepted(peer, conn),
                                                Ok(Err(e)) => HandshakeOutcome::Failed(peer, failure_kind(&e)),
                                                Err(_) => HandshakeOutcome::Deadline(peer),
                                            },
                                            Err(e) => HandshakeOutcome::Failed(peer, failure_kind(&e)),
                                        }
                                    });
                                }
                            }
                        }
                    }
                    None => break,
                },
                Some(joined) = handshakes.join_next(), if !handshakes.is_empty() => {
                    stats.handshakes_in_flight.fetch_sub(1, Ordering::SeqCst);
                    match joined {
                        Ok(HandshakeOutcome::Accepted(peer, conn)) => {
                            // 闸④ 销账（§3.1-② / r14 F10）：**握手被采纳的尝试不计数** ⇒
                            // 多候选正常赛跑（其余候选完成/被关闭）不会攒满每源闸。
                            src_gate.completed(peer);
                            stats.admitted.fetch_add(1, Ordering::SeqCst);
                            let conn_id = next_conn_id;
                            next_conn_id += 1;
                            // 每连接两枚任务（§1.3 控制流登记 / §1.4 数据报收包）。
                            // **服务流受理不在这里起**：首条 bidi 流被准入协议固定为控制流
                            // （`conn::admit` 的 `accept_bi`）⇒ 两个 `accept_bi` 并发会抢流；
                            // 受理循环由控制流任务在**准入通过之后**随刷新循环一起跑
                            // （`conn::control` 的 `join!`，见该函数注释）。
                            tasks.spawn(conn::control(conn.clone(), conn_id, Arc::clone(&ctx)));
                            tasks.spawn(conn::datagrams(conn.clone(), conn_id, Arc::clone(&ctx)));
                            conns.push(LiveConn { remote: conn.remote_address(), conn, conn_id });
                            stats.connections.store(conns.len() as u64, Ordering::SeqCst);
                        }
                        // 握手失败（错 RPK / 对端放弃）：明细记行与四类丢弃计数归 E-q3——
                        // 握手面失败留在本总计数（客户端钉定判据的证据面）。
                        // 闸④：**未完成**的尝试留在窗内（不销账）——它们正是触发②/③的输入。
                        Ok(HandshakeOutcome::Failed(_peer, kind)) => {
                            stats.handshake_failed.fetch_add(1, Ordering::SeqCst);
                            // M2 §14-1④：归因位（对端主动关闭 vs 静默）——**只归因，不豁免**
                            if kind == FailureKind::PeerClosed {
                                stats.handshake_peer_closed.fetch_add(1, Ordering::SeqCst);
                            }
                        }
                        Ok(HandshakeOutcome::Deadline(peer)) => {
                            let n = stats.handshake_timeouts.fetch_add(1, Ordering::SeqCst) + 1;
                            if log_due(n) {
                                (*logf)(&format!(
                                    "quic: 握手期限（{peer} 未在 {deadline:?} 内完成，已弃；第 {n} 次）",
                                    deadline = cfg.handshake_deadline
                                ));
                            }
                        }
                        Err(_join_err) => {} // 任务被 abort（收工路径）——不计
                    }
                }
                Some(out) = out_rx.recv() => {
                    // 引擎 → 出口面：内层包交给对应连接（§1.5）
                    match bridge.conn_of_pub(&out.pubkey) {
                        Some(conn) => {
                            let _ = conn::send_datagram_checked(&conn, out.pkt, &ctx);
                        }
                        // 绑定在「引擎判有绑定」与「面侧取连接」之间消失（设备摘除/连接死）
                        None => bridge.note_drop(DropKind::Unregistered, "出站目标无绑定"),
                    }
                }
                _ = tokio::time::sleep(TICK) => {}
            }
            // 巡检（每拍）：清死连接（摘绑定）+ 观测路径变更（E-q2 行）
            conns.retain(|c| {
                let alive = c.conn.close_reason().is_none();
                if !alive {
                    bridge.unbind_conn(c.conn_id);
                }
                alive
            });
            stats.connections.store(conns.len() as u64, Ordering::SeqCst);
            for c in conns.iter_mut() {
                let now = c.conn.remote_address();
                if now != c.remote {
                    let from = c.remote;
                    c.remote = now;
                    stats.path_changes.fetch_add(1, Ordering::SeqCst);
                    match bridge.dev_of_conn(c.conn_id) {
                        Some(dev) => (*logf)(&format!(
                            "quic: 路径变更 dev={} {from} → {now}",
                            bridge::dev_short(&dev)
                        )),
                        None => (*logf)(&format!("quic: 路径变更（未登记连接）{from} → {now}")),
                    }
                }
            }
        }
        // ---- 收工：先在途握手/每连接任务全弃（JoinSet drop 即 abort）→ 关端点 → 丢句柄 ----
        drop(tasks);
        drop(handshakes);
        endpoint.close(0u32.into(), b"exit stopping");
        drop(conns);
        drop(endpoint);
    });
}
