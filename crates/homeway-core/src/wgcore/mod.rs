//! wgcore：WG 数据面引擎（boringtun noise + 栈 B + wtransport Bind 的装配与驱动）。
//!
//! 对齐 Go 侧 `clientcore/internal/wgcore`（core.go + hub.go）的 R1 子集（设计文档
//! §1/§3/§4）：
//! - **单驱动线程**独占 Tunn + Interface + UDP socket（无锁热路径）；主线程经
//!   unbounded 命令通道投递请求、经各自 reply 通道收结果（三条死锁纪律：命令通道
//!   unbounded / WG 线程不阻塞在 channel / 不持锁跨等待）；
//! - **三唤醒源**：`poll(2)` on {UDP fd, self-pipe}，超时 = `min(poll_delay, 250ms)`
//!   （延迟 ACK 10ms 等栈定时器不迟到）；
//! - reg 搭车收口 = `Bind::send_wg`（四来源全覆盖）；expired → **一次性**重建 Tunn
//!   （新随机 index 前缀 <2^24）+ 补注册，**保采纳**（= Go 恢复阶梯 R1 档「补注册 +
//!   丢会话保采纳」的最小兜底——比设计文档 v2 登记的「丢采纳」更贴 Go，按实现修正；
//!   阶梯档位/节拍整体属 R2）；
//! - 静默丢包类 `WireGuardError` 计数继续，`ConnectionExpired` 才重建（评审 ②-9）；
//! - `decapsulate` 返回 `WriteToNetwork` 后以**空数据报重调**到 Done（冲掉握手期排队
//!   的内层包与握手响应 keepalive——评审 ③-7）。

use std::collections::HashMap;
use std::io::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::StaticSecret;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::time::Instant as SmolInstant;

use crate::identity::Identity;
use crate::psk::Psk;
use crate::token::{PeerId, Secret};
use crate::tunnel_addr;
use crate::wtransport::{Bind, Candidate, RegCtx, Via};

pub mod stackb;

use self::stackb::{DialError, StackB};

/// TUN 面错误回调类型（fd 读写失败 → facade markUnhealthy；driver 线程执行）。
pub type OnTunError = Box<dyn Fn(&str) + Send>;

/// 出口隧道 IP 的契约常量（**单一真源已迁至 `crate::tunnel_addr`**——F11 撞车守卫与
/// 两个派生函数同域判定；此处保留再导出以免大范围改引用点）。
pub use crate::tunnel_addr::SERVER_TUNNEL_IP;
/// poll 等待上限（smoltcp poll_delay 的封顶；延迟 ACK 10ms 一类栈定时器的到点保障）。
const POLL_CAP: i32 = 250;
/// TUN fd 的 poll 单片时长（Q-G F2）——与 Go `tunfd_unix.go` 的 poll 节拍同值。
/// 触底语义：片到无事件 = `Ready::default()`（**不判死**——超时是健康形态）。
pub(crate) const POLL_SLICE: Duration = Duration::from_millis(500);
/// TUN fd 判死前的**确认拍**（Q-G F2；`n==0` 分支）。取值依据是「同一观察重复一次」
/// 而非精确时延：HUP 下 poll 立返，不真睡就复 poll 等于没确认；50ms 足以让
/// 「EOF 与读空交错」的瞬时形态自证（真失效的 fd 50ms 后仍报 HUP/ERR/NVAL）。
pub(crate) const DEAD_CONFIRM_DELAY: Duration = Duration::from_millis(50);
/// 隧道域**世代收尾**的 `Client` 停止预算（Q-G F5）：与 Q-F 的 `EXIT_RPC_BUDGET`
/// 同值（收工链既有量级）。显式 5 处 `stop_within` 用它；`Drop for Client` 兜底
/// 保持无界（设计 §3-D3）。
pub(crate) const CLIENT_CLOSE_BUDGET: Duration = Duration::from_secs(2);
/// 8s 密集 ACK 时钟：有界 drain 的**栈字节阈值**（D-3；设计 docs/reviews/R8.md
/// §十二）。背景：smoltcp 0.14 的 ACK 策略已内建 RFC5681/Linux 风格（未确认 >
/// 1×remote_mss ⇒ 立即 ACK；10ms delayed 兜底）且「每 poll 每 socket 至多一个
/// 累积 ACK」（Interface::poll 先排空全部 ingress 再 egress——评审 r1-4.1 已核）；
/// 稀疏 ACK 时钟（实测 18 段/ACK）的成因是驱动循环「一次收光 → 一次 poll」的
/// **批到达形态**。改法 = drain_udp 每累计 2×1280B（2×MSS：>1×MSS 起即产立即
/// ACK）的**进 stack B 字节**就返回——驱动循环的 poll 因 UDP fd 仍可读立即返回
/// → pump_once 的既有栈 poll 产 ACK ⇒ ACK 密度 ≈ 每 2 满段一个（Go/gVisor 接收
/// 端同档节奏；pump_once 保持唯一栈驱动点，8n① 三段计时口径不动——评审 r1-4.2
/// 采納的有界 drain 形态）。transit/TUN 包不计（不经 stack B，零额外 poll 成本
/// ——评审 r1-3.3）；小包按字节累积自然合并；批尾不足阈值由 pump_once 兜底，
/// 奇尾段走 smoltcp 10ms delayed-ACK（驱动 poll 超时已尊重 poll_delay——不变）。
/// **适用域**：只覆盖核心自连（stack B 收端 = speedtest/files/term/探测路径）；
/// 经 TUN 的应用流量由 OHOS 内核回 ACK，本改动对其零作用。
/// env `HOMEWAY_ACK_POLL_CHUNK`（字节阈值覆盖；CLI/harness 消融缝——手机上 env
/// 不可设，真机 off 臂 = 装 baseline 核对照；大值 ≈ 旧行为〔单 poll〕）。
const ACK_DRAIN_BYTES: usize = 2 * stackb::MTU; // 2×MTU（MSS=MTU-40 ⇒ 2 满段 > 1×MSS 即触发立即 ACK；抬 MTU 时随动——r2-3.4）

/// ACK drain 阈值（env 覆盖的缓存读——热路径不重复走 env 解析）。
fn ack_drain_bytes() -> usize {
    static CHUNK: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CHUNK.get_or_init(|| {
        std::env::var("HOMEWAY_ACK_POLL_CHUNK")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(ACK_DRAIN_BYTES)
    })
}
/// UDP 载荷上限门（Q-K F5-b，纯函数面）：`20 + 8 + len > 65535` ⇒ 结构性不可发
/// （IPv4 `total_len` 是 u16），必须在**调用侧可见地失败**——此前该形态是
/// 「`send_slice` 返 Ok 但报文被 smoltcp 的分片缓冲静默丢」。区间 `(1253, 65507]`
/// **不报错**（那是正常可发：F5-a 起由栈真分片）。
fn udp_payload_gate(len: usize) -> Result<(), ConnErr> {
    if 20 + 8 + len > 65535 {
        Err(ConnErr::DatagramTooLarge(len))
    } else {
        Ok(())
    }
}

/// WG 网络包缓冲上界（握手 148 / 数据 = 明文 + 32B 开销）。
const WG_BUF: usize = 65536 + 148;
/// 连接的建立期限（engine 侧；主线程另有自己的 RPC 超时）。
#[cfg(not(test))]
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
#[cfg(test)]
const CONNECT_DEADLINE: Duration = Duration::from_millis(400);

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnErr {
    #[error("连接被拒（对端 RST）")]
    Refused,
    #[error("连接超时")]
    Timeout,
    #[error("连接已关闭")]
    Closed,
    #[error("通道已断（引擎收工）")]
    EngineGone,
    /// UDP 数据报超过 IPv4 可承载上限（载荷 > 65507 = 65535 − IP 20 − UDP 8；Q-K F5-b）。
    /// 此前该形态是「send_slice 返 Ok 但报文被静默丢」——**结构性不可发**必须在调用侧可见。
    #[error("UDP 数据报过大（载荷 {0} 字节 > 65507）")]
    DatagramTooLarge(usize),
    #[error(transparent)]
    Dial(#[from] DialError),
}

/// 主线程 → 驱动线程的命令（全部非阻塞投递；带 reply 的由驱动线程在事件到点时应答）。
/// `Client::write` 的回执（R8-3 F12）：`n` = 本次接纳字节数；`back` = 零接纳时
/// **原样带回**的载荷（调用方重试直接复用，消背压期的整段重拷；部分接纳时
/// 余量由调用方按 io::Write 契约自行切片）。
pub struct WriteOut {
    pub n: usize,
    pub back: Option<Vec<u8>>,
}

pub enum Cmd {
    Connect {
        id: u64,
        dst: SocketAddrV4,
        /// 建立期限（引擎侧到点 abort——探测预算必须下沉引擎，caller 弃等会留残留 SYN
        /// 污染阶梯归因；R2 评审中-13）。
        deadline: Duration,
        reply: Sender<Result<(), ConnErr>>,
    },
    Write {
        id: u64,
        data: Vec<u8>,
        reply: Sender<Result<WriteOut, ConnErr>>,
    },
    Read {
        id: u64,
        reply: Sender<Result<Vec<u8>, ConnErr>>,
    },
    /// 半关（FIN；对端仍可发）。
    Shutdown {
        id: u64,
        reply: Sender<Result<(), ConnErr>>,
    },
    Close {
        id: u64,
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 补注册（回执 = 是否真发出：false = bind 已收工或无采纳地址）。
    RefreshReg {
        reply: Option<Sender<bool>>,
    },
    /// 清采纳、重启赛跑（Go Rearm 家族的硬赛跑形态；候选重投由调用方经 SetCandidates 跟进）。
    Rearm {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 软赛跑（Go RearmSoft：中继立即参与——升直连/hint 打洞用，不停在用路径）。
    RearmSoft {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 装 hint 回调（回调在**驱动线程**执行——只允许内存操作/通道投递，严禁 RPC 回引擎）。
    /// 【测试缝】中继锁定（relay-lock 注入——见 wtransport::Bind::relay_only）。
    SetRelayOnly,
    SetOnHint {
        h: crate::wtransport::bind::OnHint,
    },
    /// 丢弃本地 WG 会话（Go ResetPeerSession 同义：peer 移除再写回 ≙ 重建 Tunn；
    /// **保采纳**）——下一发出站包全新握手。
    ResetPeerSession {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 换本地 UDP socket（Go Rebind 同义：不换 Identity、不动采纳）。
    Rebind {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 更新候选集（学习缓存刷新后）。
    SetCandidates {
        cands: Vec<Candidate>,
    },
    /// UDP 拨号面（R3-3f：E12 出口 UDP 判据采样 + DNS 代答实测——Go 栈内 udp 的
    /// 最小客户端面）：Open = 栈内 bind（ephemeral 端口自管）；Send/Recv/Close。
    UdpOpen {
        id: u64,
        reply: Sender<Result<u16, ConnErr>>,
    },
    UdpSend {
        id: u64,
        dst: SocketAddrV4,
        data: Vec<u8>,
        reply: Sender<Result<(), ConnErr>>,
    },
    UdpRecv {
        id: u64,
        reply: Sender<Result<(Vec<u8>, SocketAddrV4), ConnErr>>,
    },
    UdpClose {
        id: u64,
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 测试缝：把当前 UDP socket 置为已关形态（模拟冻结唤醒后 OS 作废 socket——
    /// R2 阶梯 R2 档注入；Go 集成测试注入假 transport 同义）。
    #[cfg(feature = "test-seams")]
    DebugPoisonSocket,
    /// L3 直通：注册应用 TUN 源（Go hub.AttachTUN——只允许一次；fd 所有权在扩展，
    /// 引擎只裸读写、**从不 close**，失效经错误回调暴露——坑 50）。
    TunAttach {
        fd: i32,
        mtu: u32,
        reply: Sender<Result<(), ConnErr>>,
    },
    /// TUN 读线程投来的应用出站包（driver encap 发送——Go hub 的 outbound 通道面）。
    TunPacket(Vec<u8>),
    /// 装 TUN 面错误回调（fd 读写失败 → facade 挂 markUnhealthy；attach 前后都可调，
    /// driver 线程执行——回调只允许原子/内存操作，Go SetOnTunError 同义）。
    SetOnTunError {
        f: OnTunError,
    },
    /// TUN 读线程报 fd 失效（driver 卸源 + 触发错误回调——与写失败同收口）。
    TunFdDead {
        msg: String,
    },
    Stop,
}

/// 引擎状态快照（主线程轮询；驱动线程独占写）。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub via: Via,
    pub ep: Option<SocketAddr>,
    pub mirrored: u64,
    pub rx: u64,
    pub tx: u64,
    /// 最近一次**采纳路径**本地类发送错误时刻（巡检失败拍的噪声判定数据源；
    /// Go Bind.lastLocalSendErrAt 同义——镜像候选的本地错误不刷此位）。
    pub last_local_send_err: Option<Instant>,
    /// 全候选发送统计的**生命周期累计**（(尝试, 本地失败)；Bind 计数器单调，快照每
    /// 拍重写，Client::swap_send_stats 以差分等价 Go 的 swap-reset——复核 r3-F5 更正
    /// 注释：此前误写「消费后置 None」）。
    pub bind_stats: Option<(i64, i64)>,
    /// 本地发送错误累计（(采纳路径, 全部)——tunStatusJSON demand.localErr* 源；
    /// 拍板①：Go adoptedLocalErrCount/localErrCount 的累计面）。
    pub local_err: (u64, u64),
}

pub struct CoreConfig {
    pub peer_id: PeerId,
    pub secret: Secret,
    pub identity: Identity,
    pub candidates: Vec<Candidate>,
    pub logf: Arc<dyn Fn(&str) + Send + Sync>,
}

type UdpRecvReply = Result<(Vec<u8>, SocketAddrV4), ConnErr>;

/// TUN 面的共享计数（fd 字节对表 + 需求信号源；driver 与读线程两写、主线程读）。
#[derive(Debug, Default)]
pub struct TunCounters {
    /// 读（应用出站方向）累计字节——与设备 `vpn-tun` 的 TX 对表。
    pub read_bytes: AtomicU64,
    /// 写（应用入站方向）累计字节——与设备 `vpn-tun` 的 RX 对表。
    pub write_bytes: AtomicU64,
    /// App 出站包计数（demand-driven-recovery D1：巡检拍「取走清零」消费）。
    pub out_pkts: std::sync::atomic::AtomicI64,
    /// 最近出站包时刻（unix nano；0 = 本世代从未有过应用出站——tunStatusJSON 的
    /// demand.outboundAt 面）。
    pub last_outbound_ns: AtomicI64,
    /// 同一时刻的**单调相对读数**（自进程起点的 ns；0 = 从未）——下推器（D4）的
    /// 时基。评审 r2-H1：单调面与 unix 面分离（曾用 unix ns 换算 Instant，减出
    /// 56 年前的时刻 ⇒ should_push 恒 false）。
    pub last_outbound_mono_ns: AtomicI64,
}

/// 进程单调起点（last_outbound_mono_ns 的基准；懒初始化）。
fn process_mono_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// 引擎内的应用 TUN 源（L3 直通；Go hub 的 appTun 面收窄——栈 B 本就在 Engine 里，
/// hub 的「双源喂 device」= decap 分流 + TunPacket 命令两个动作）。
struct AppTun {
    fd: i32,
    /// 读线程停止位（Engine stop/重复 attach 时置位；线程分离不 join——阻塞在
    /// read 上，退出靠 fd 失效〔扩展 destroy〕或错误返回）。
    stop: Arc<AtomicBool>,
    counters: Arc<TunCounters>,
    /// fd 读写失败回调（facade 挂 markUnhealthy；driver 线程执行——只允许原子/内存操作）。
    on_error: Option<OnTunError>,
}

struct UdpConn {
    handle: SocketHandle,
    wait_recv: Option<Sender<UdpRecvReply>>,
}

struct Conn {
    handle: SocketHandle,
    syn_sent: bool,
    local_aborted: bool,
    established: bool,
    deadline: Instant,
    wait_est: Option<Sender<Result<(), ConnErr>>>,
    wait_read: Option<Sender<Result<Vec<u8>, ConnErr>>>,
}

fn make_tunn(identity_key: &StaticSecret, secret: &Secret, peer_pub: &[u8; 32]) -> Tunn {
    Tunn::new(
        identity_key.clone(),
        boringtun::x25519::PublicKey::from(*peer_pub),
        // Secret 非 Copy（F8d）：PSK 派生面按引用克隆一份（派生后即被 Psk 吸收，
        // 原 Secret 的副本随本次调用结束被 Drop 擦除）
        Some(*Psk::from(secret.clone()).as_bytes()),
        None, // persistent_keepalive：对齐 Go 客户端（不设；保活 = probe 拍）
        rand_index(),
        None, // rate_limiter 勿改——Some 会让客户端对出口握手应答回 cookie（§2 勿改清单）
    )
    .expect("参数恒合法（dalek 钥/PSK 构造期已验）")
}

fn rand_index() -> u32 {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("系统随机源不可用");
    u32::from_le_bytes(b) & 0x00ff_ffff // < 2^24：Tunn 内部 <<8 丢高位（评审 ②-2）
}

/// 引擎主体（驱动线程内独占）。
struct Engine {
    bind: Bind,
    tunn: Tunn,
    stack: StackB,
    conns: HashMap<u64, Conn>,
    /// UDP 面：id → 栈内 socket + 待决读。
    udp: HashMap<u64, UdpConn>,
    wg_buf: Vec<u8>,
    /// Q-I F5-1：`resolve_udp` 的复用读缓冲（构造期一次分配；此前每待决 id 每拍
    /// `vec![0u8; 65535]`——挂起无包时也发生）。交付时 `[..n].to_vec()`（内层包小，
    /// n 字节拷贝 ≪ 省下的 64KB alloc+memset；不复用交付 = 防别名）。
    udp_rx_buf: Vec<u8>,
    cmd_rx: mpsc::Receiver<Cmd>,
    snapshot: Arc<Mutex<Snapshot>>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    identity_key: StaticSecret,
    secret: Secret,
    peer_id: PeerId,
    /// expired 重建执行位（一次性；避免每拍重建——update_timers 过期后每 tick 回错）。
    expired_pending: bool,
    silent_drops: u64,
    /// 栈 B 收队列溢出丢弃的节流记行（R8-2 归因插桩）：已记行数 + 上次记行时的
    /// 累计丢弃——**首 3 次 + 此后每 1000 包一行**（仓内计数记行同惯例，评审 r1-F10：
    /// 纯时间节流在持续溢出形态下 1 行/秒无限刷）。
    rx_drop_logs: u32,
    last_rx_drop_logged_at: u64,
    time0: Instant,
    /// L3 直通的应用 TUN 源（None = 未 attach——transit 回包丢弃，Go hub 同义）。
    app_tun: Option<AppTun>,
    /// TUN 计数（attach 前也可读——全零；detach/收尾后保留末值）。
    tun_counters: Arc<TunCounters>,
    /// 挂起的 TUN 错误回调（attach 前设置则存这里，attach 时搬进 AppTun）。
    on_tun_error: Option<OnTunError>,
    /// 投递通道（读线程投 TunPacket 用；Client::start 注入克隆）。
    cmd_tx: Option<Sender<Cmd>>,
    /// 装载的 TUN 读线程唤醒面（我-4：读线程投 TunPacket 后写同一 wake 管道——
    /// driver 的 poll(2) 最多 250ms 才醒，上行每串包白等一拍；与 Client::send 共用
    /// 同一 fd 与互斥位，stop 关闭后写入自然 no-op）。
    wake: Arc<Mutex<Option<i32>>>,
    /// R8-4 8n 归因插桩：热路径分段计时（累计 ns）+ 收发计数。分段 = drain_udp
    /// （UDP 收 syscall + boringtun decap）/ iface.poll（smoltcp ingress + ACK 产出）/
    /// encap（TX 出队 + encap + sendto）。每秒观测行消费清零；只在收向有流量时打
    /// （空闲静默——与出口 cc 行同惯例）。
    hp_drain_ns: u128,
    hp_poll_ns: u128,
    hp_encap_ns: u128,
    hp_rx_pkts: u64,
    hp_rx_bytes: u64,
    hp_tx_pkts: u64,
    hp_tx_bytes: u64,
    hp_last_line: Option<Instant>,
}

impl Engine {
    fn now_smol(&self) -> SmolInstant {
        SmolInstant::from_millis(self.time0.elapsed().as_millis() as i64)
    }

    /// 重建 Tunn（**唯一重建点**——expired 兜底与阶梯 ResetPeerSession 共用）：
    /// 同身份/同 secret/同 peer 的全新会话状态（新随机 index 前缀），**保采纳**
    /// （采纳/reg 都在 Bind，不受影响）。Go「peer 移除再写回」（core.go:228-246）的
    /// 单体等价物；排队内层包随旧 Tunn 丢弃（= Go flushStagedPackets）。
    fn rebuild_tunn(&mut self) {
        self.tunn = make_tunn(&self.identity_key, &self.secret, self.peer_id.as_bytes());
    }

    /// ConnectionExpired 的**一次性**重建（引擎兜底：boringtun 特有义务，wireguard-go
    /// 自管 rekey、Go 侧无对应物；**不产 RECOVER 行**，打自己的行）。置位防每拍重建
    /// （update_timers 过期后每 tick 回错）；明文包到达复位。
    fn rebuild_tunn_once(&mut self) {
        if self.expired_pending {
            return;
        }
        self.expired_pending = true;
        self.rebuild_tunn();
        if self.bind.adopted().is_some() {
            (self.logf)("wgcore: 会话过期已重建（丢会话保采纳）—— 补注册");
            self.bind.refresh_reg();
        } else {
            // 未采纳（无路径）：RREG 需要 adopted 会静默 no-op——改走 rearm 重武装 reg
            // 并主动触发一次出站（空载荷 encapsulate 无会话 ⇒ 产握手 init + 搭 reg，
            // Go 恢复阶梯 R1 档在「无路径」时刻的同义动作；评审中-1）
            (self.logf)("wgcore: 会话过期已重建（无采纳路径）—— 重赛跑 + 补注册");
            self.bind.rearm();
            self.encap_send(&[]);
        }
    }

    /// 单轮驱动：命令 → UDP 批量收 → 栈 poll → TX 出队封装 → 定时器 → 待决结算。
    /// 返回 false = 收到 Stop。
    ///
    /// R8-8b 注记：曾在此处做「上行 bulk 批化收集 + send_wg_batch/sendmmsg 批量发送」，
    /// 真机 A/B 实测（FMR0224116011480）**两侧皆劣化**（down 47→15MB/流、up 86→25MB/流
    /// ——OHOS 的 sendmmsg 路径在批量形态下反而拖慢收发节奏）已整体移除；上行 bulk
    /// 的真修复 = SessionWriteHalf 的背压语义（Ok(0) 展开为有界等待，见 tun_exec.rs）。
    fn pump_once(&mut self, udp_buf: &mut [u8]) -> bool {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            if !self.handle_cmd(cmd) {
                return false;
            }
        }
        self.bind.tick_unlock();
        let hp_t0 = Instant::now();
        self.drain_udp(udp_buf);
        let hp_t1 = Instant::now();
        // R8-2 归因插桩：栈 B 收队列溢出丢弃（首 3 次 + 每 1000 包一行）。
        // 下行 bulk 的出口突发（拦截栈单拍可产上千包）超 QUEUE_CAP 时，多余内层包
        // 在这里静默丢 ⇒ TCP 层大规模重传——本行即该丢失面的判据。
        {
            let dropped = self.stack.device.rx_dropped();
            if dropped > 0
                && (self.rx_drop_logs < 3 || dropped - self.last_rx_drop_logged_at >= 1000)
            {
                self.rx_drop_logs += 1;
                self.last_rx_drop_logged_at = dropped;
                (self.logf)(&format!(
                    "wgcore: 栈B 收队列溢出（QUEUE_CAP={}）累计丢弃 {dropped} 包——出口下行突发超队列容量",
                    crate::wgcore::stackb::QUEUE_CAP
                ));
            }
        }
        let now = self.now_smol();
        self.stack
            .iface
            .poll(now, &mut self.stack.device, &mut self.stack.sockets);
        let hp_t2 = Instant::now();
        let mut tx: Vec<Vec<u8>> = Vec::new();
        self.stack.device.drain_tx(&mut tx);
        for pkt in &tx {
            self.hp_tx_pkts += 1;
            self.hp_tx_bytes += pkt.len() as u64;
            self.encap_send(pkt);
        }
        let hp_t3 = Instant::now();
        self.hp_drain_ns += hp_t1.saturating_duration_since(hp_t0).as_nanos();
        self.hp_poll_ns += hp_t2.saturating_duration_since(hp_t1).as_nanos();
        self.hp_encap_ns += hp_t3.saturating_duration_since(hp_t2).as_nanos();
        // 8n 归因观测行（每秒；收向有流量才打）：判别 = 三段耗时占比定位手机核
        // 每字节成本的去处（decap/栈 poll/上行 encap）+ 收包速率与均包长。
        // 首拍只落时间基（None → 起算点），满 1s 才报。
        let due = match self.hp_last_line {
            None => {
                self.hp_last_line = Some(Instant::now());
                false
            }
            Some(t) => t.elapsed() >= Duration::from_secs(1),
        };
        if due {
            if self.hp_rx_bytes > 0 {
                let ms = |ns: u128| (ns / 1_000_000).min(9999) as u64;
                (self.logf)(&format!(
                    "wgcore: 热路径1s 收={}包/{}KB(均{}B) drain={}ms poll={}ms encap={}ms 出={}包/{}KB",
                    self.hp_rx_pkts,
                    self.hp_rx_bytes / 1024,
                    self.hp_rx_bytes / self.hp_rx_pkts.max(1),
                    ms(self.hp_drain_ns),
                    ms(self.hp_poll_ns),
                    ms(self.hp_encap_ns),
                    self.hp_tx_pkts,
                    self.hp_tx_bytes / 1024,
                ));
            }
            self.hp_last_line = Some(Instant::now());
            self.hp_drain_ns = 0;
            self.hp_poll_ns = 0;
            self.hp_encap_ns = 0;
            self.hp_rx_pkts = 0;
            self.hp_rx_bytes = 0;
            self.hp_tx_pkts = 0;
            self.hp_tx_bytes = 0;
        }
        self.timer_tick();
        self.resolve_pending();
        self.resolve_udp();
        self.update_snapshot();
        true
    }

    /// UDP 收包排空（8s：**有界**——每累计 `ACK_DRAIN_BYTES` 的进 stack B 字节返回
    /// 一次，驱动循环的下一轮 poll 因 UDP fd 仍可读立即返回、pump_once 产 ACK；
    /// transit/TUN 包不计，非栈流量形态与旧全量排空逐字节一致）。
    fn drain_udp(&mut self, buf: &mut [u8]) {
        let threshold = ack_drain_bytes();
        let mut stack_bytes = 0usize;
        loop {
            match self.bind.recv_from(buf) {
                Ok(Some(n)) => {
                    let src_ip = self.bind.adopted().map(|a| match a {
                        SocketAddr::V4(v4) => IpAddr::V4(*v4.ip()),
                        SocketAddr::V6(v6) => IpAddr::V6(*v6.ip()),
                    });
                    stack_bytes += self.decapsulate_in(src_ip, &buf[..n]);
                    if stack_bytes >= threshold {
                        return; // 8s：栈字节满一拍即返回（ACK 在 pump_once 的 poll 产出）
                    }
                }
                Ok(None) => continue,
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    return;
                }
                Err(_) => {
                    self.silent_drops += 1;
                    return;
                }
            }
        }
    }

    /// decapsulate + 空数据报重调协议（WriteToNetwork 后以空输入重调到 Done）。
    /// 返回**进 stack B 的字节数**（transit/TUN/其它 = 0——8s 有界 drain 的计数面）。
    fn decapsulate_in(&mut self, src_ip: Option<IpAddr>, datagram: &[u8]) -> usize {
        let src_ip = src_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        // 热路径：容量恒足（构造时 WG_BUF 一次分配）；boringtun 只写前缀——
        // 不 clear/resize（每包 65KB memset 纯浪费，中-10①）。长度由返回值给。
        // Q-I F2：空数据报重调循环内的同款 clear+resize 也已删（此前注释-实现不符）；
        // 长度不变量在入口兜底（debug 拦「未来有人把缓冲改短」）。
        debug_assert_eq!(self.wg_buf.len(), WG_BUF, "wg_buf 长度契约（构造期一次分配）");
        match self
            .tunn
            .decapsulate(Some(src_ip), datagram, &mut self.wg_buf)
        {
            TunnResult::WriteToNetwork(w) => {
                self.bind.send_wg(w);
                loop {
                    match self.tunn.decapsulate(Some(src_ip), &[], &mut self.wg_buf) {
                        TunnResult::WriteToNetwork(w2) => self.bind.send_wg(w2),
                        TunnResult::Err(WireGuardError::ConnectionExpired) => {
                            self.rebuild_tunn_once();
                            break;
                        }
                        TunnResult::Err(_) => {
                            self.silent_drops += 1;
                            break;
                        }
                        _ => break,
                    }
                }
                0
            }
            TunnResult::WriteToTunnelV4(pkt, _) => {
                self.expired_pending = false; // 明文包到达 = 会话活着
                // 8n 归因插桩：收向明文包计数（inject 与 TUN 投递两条路都算）。
                self.hp_rx_pkts += 1;
                self.hp_rx_bytes += pkt.len() as u64;
                                              // L3 直通分流（Go hub.Write）：dst == B 的本地地址（派生隧道 IP）→ 栈 B
                                              // （核心自连回程），其余 → 真 TUN fd（内核投给应用）。
                if pkt.len() >= 20
                    && pkt[0] >> 4 == 4
                    && pkt[16..20] == self.stack.tunnel_ip.octets()
                {
                    self.stack.inject(pkt);
                    return pkt.len();
                } else if let Some(tun) = self.app_tun.as_ref() {
                    match write_fd_all(tun.fd, pkt) {
                        Ok(()) => {
                            tun.counters
                                .write_bytes
                                .fetch_add(pkt.len() as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let msg = format!("tun fd 写入失败：{e}");
                            (self.logf)(&msg);
                            // 卸源 + 置读线程停止位（评审 r2-我-5/评审者「不认同②」：
                            // 停止位在 AppTun 里——直接 `= None` 会把引擎侧唯一能置位
                            // 的句柄一起丢掉。先置 stop 再卸源；错误回调同拍发出
                            // 〔markUnhealthy_if_current("fd") → 扩展重建〕）。
                            if let Some(tun) = self.app_tun.take() {
                                tun.stop.store(true, Ordering::SeqCst);
                                if let Some(cb) = tun.on_error.as_ref() {
                                    cb(&msg);
                                }
                            }
                        }
                    }
                }
                // 未 attach：transit 回包不该出现（还没有应用流量）；丢弃（Go 同义）
                0
            }
            TunnResult::WriteToTunnelV6(_, _) => 0, // 内层只承载 IPv4（D4）
            TunnResult::Done => 0,
            TunnResult::Err(WireGuardError::ConnectionExpired) => {
                self.rebuild_tunn_once();
                0
            }
            TunnResult::Err(_) => {
                self.silent_drops += 1; // 静默丢包类：计数继续（评审 ②-9）
                0
            }
        }
    }

    fn encap_send(&mut self, pkt: &[u8]) {
        // 热路径：容量恒足（构造时 WG_BUF 一次分配）；boringtun 只写前缀——
        // 不 clear/resize（每包 65KB memset 纯浪费，中-10①）。长度由返回值给。
        match self.tunn.encapsulate(pkt, &mut self.wg_buf) {
            TunnResult::WriteToNetwork(w) => self.bind.send_wg(w),
            TunnResult::Err(WireGuardError::ConnectionExpired) => self.rebuild_tunn_once(),
            TunnResult::Err(_) => {
                self.silent_drops += 1;
            }
            _ => {}
        }
    }

    fn timer_tick(&mut self) {
        // 热路径：容量恒足（构造时 WG_BUF 一次分配）；boringtun 只写前缀——
        // 不 clear/resize（每包 65KB memset 纯浪费，中-10①）。长度由返回值给。
        match self.tunn.update_timers(&mut self.wg_buf) {
            TunnResult::WriteToNetwork(w) => self.bind.send_wg(w),
            TunnResult::Err(WireGuardError::ConnectionExpired) => self.rebuild_tunn_once(),
            TunnResult::Err(_) => {
                self.silent_drops += 1;
            }
            _ => {}
        }
    }

    /// UDP 面：栈内 bind（ephemeral；回环拒绝——环回不进隧道与 TCP 同口径）。
    fn start_udp(&mut self, id: u64) -> Result<u16, ConnErr> {
        use smoltcp::socket::udp;
        let port = self.stack.alloc_udp_port();
        let rx_meta: Vec<udp::PacketMetadata> =
            (0..16).map(|_| udp::PacketMetadata::EMPTY).collect();
        let tx_meta: Vec<udp::PacketMetadata> =
            (0..16).map(|_| udp::PacketMetadata::EMPTY).collect();
        let mut sock = udp::Socket::new(
            udp::PacketBuffer::new(rx_meta, vec![0u8; 64 * 1024]),
            udp::PacketBuffer::new(tx_meta, vec![0u8; 64 * 1024]),
        );
        let ep = smoltcp::wire::IpEndpoint::new(self.stack.tunnel_ip.into(), port);
        sock.bind(ep)
            .map_err(|e| ConnErr::Dial(DialError::Stack(format!("{e:?}"))))?;
        let h = self.stack.sockets.add(sock);
        self.udp.insert(
            id,
            UdpConn {
                handle: h,
                wait_recv: None,
            },
        );
        Ok(port)
    }

    fn udp_send(&mut self, id: u64, dst: SocketAddrV4, data: &[u8]) -> Result<(), ConnErr> {
        if dst.ip().is_loopback() {
            return Err(ConnErr::Dial(DialError::LoopbackRejected));
        }
        udp_payload_gate(data.len())?;
        let Some(u) = self.udp.get(&id) else {
            return Err(ConnErr::Closed);
        };
        let ep = smoltcp::wire::IpEndpoint::new((*dst.ip()).into(), dst.port());
        let sock = self
            .stack
            .sockets
            .get_mut::<smoltcp::socket::udp::Socket>(u.handle);
        sock.send_slice(data, ep).map_err(|_| ConnErr::Closed)?;
        Ok(())
    }

    /// UDP 读结算（resolve_pending 的伙伴面）。
    fn resolve_udp(&mut self) {
        let ids: Vec<u64> = self.udp.keys().copied().collect();
        for id in ids {
            let Some(u) = self.udp.get_mut(&id) else {
                continue;
            };
            if u.wait_recv.is_none() {
                continue;
            }
            let handle = u.handle;
            // Q-I F5-1：复用缓冲（构造期一次分配）——此前每拍 `vec![0u8; 65535]`
            // （有 `UdpRecv` 挂起但无包时也发生）。
            let got = self
                .stack
                .sockets
                .get_mut::<smoltcp::socket::udp::Socket>(handle)
                .recv_slice(&mut self.udp_rx_buf);
            match got {
                Ok((n, meta)) if n > 0 => {
                    let from = match meta.endpoint.addr {
                        smoltcp::wire::IpAddress::Ipv4(a) => a, // 0.14：core::net::Ipv4Addr 直存（R8-8a）
                        _ => continue,
                    };
                    // 交付走 owned 副本（复用缓冲不得跨调用泄漏——防别名）
                    let pkt = self.udp_rx_buf[..n].to_vec();
                    let tx = self.udp.get_mut(&id).and_then(|u| u.wait_recv.take());
                    if let Some(tx) = tx {
                        let _ = tx.send(Ok((pkt, SocketAddrV4::new(from, meta.endpoint.port))));
                    }
                }
                _ => {} // 无包：继续等
            }
        }
    }

    /// 返回 false = 收到 Stop。
    fn handle_cmd(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Connect {
                id,
                dst,
                deadline,
                reply,
            } => match self.start_conn(id, dst, deadline) {
                Ok(()) => {
                    if let Some(c) = self.conns.get_mut(&id) {
                        c.wait_est = Some(reply);
                    }
                }
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            },
            Cmd::Write { id, data, reply } => {
                let r = match self.conns.get(&id) {
                    Some(c) => {
                        let sock = self.stack.sockets.get_mut::<TcpSocket>(c.handle);
                        let r = sock.send_slice(&data).map_err(|_| ConnErr::Closed);
                        if r.is_err() && crate::envflag::wg_debug() {
                            eprintln!(
                                "[wr-debug] write 失败 id={id} state={:?} may_send={} local={:?}",
                                sock.state(),
                                sock.may_send(),
                                sock.local_endpoint()
                            );
                        }
                        // R8-3 F12：零接纳（背压 Ok(0)）把数据原 Vec 带回——调用方
                        // 重试环不再每 2ms 重拷整段（登记的「每次重拷」面）。
                        r.map(|n| WriteOut {
                            n,
                            back: if n == 0 { Some(data) } else { None },
                        })
                    }
                    None => Err(ConnErr::Closed),
                };
                let _ = reply.send(r);
            }
            Cmd::Read { id, reply } => match self.conns.get_mut(&id) {
                Some(c) => c.wait_read = Some(reply),
                None => {
                    let _ = reply.send(Err(ConnErr::Closed));
                }
            },
            Cmd::Shutdown { id, reply } => {
                let r = match self.conns.get(&id) {
                    Some(c) => {
                        self.stack.sockets.get_mut::<TcpSocket>(c.handle).close();
                        Ok(())
                    }
                    None => Err(ConnErr::Closed),
                };
                let _ = reply.send(r);
            }
            Cmd::Close { id, reply } => {
                let r = match self.conns.get_mut(&id) {
                    Some(c) => {
                        self.stack.sockets.get_mut::<TcpSocket>(c.handle).abort();
                        c.local_aborted = true;
                        Ok(())
                    }
                    None => Err(ConnErr::Closed),
                };
                let _ = reply.send(r);
            }
            Cmd::UdpOpen { id, reply } => {
                let r = self.start_udp(id);
                let _ = reply.send(r);
            }
            Cmd::UdpSend {
                id,
                dst,
                data,
                reply,
            } => {
                let r = self.udp_send(id, dst, &data);
                let _ = reply.send(r);
            }
            Cmd::UdpRecv { id, reply } => match self.udp.get_mut(&id) {
                Some(u) => u.wait_recv = Some(reply),
                None => {
                    let _ = reply.send(Err(ConnErr::Closed));
                }
            },
            Cmd::UdpClose { id, reply } => {
                if let Some(u) = self.udp.remove(&id) {
                    self.stack.sockets.remove(u.handle);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::RefreshReg { reply } => {
                let sent = self.bind.refresh_reg();
                if let Some(tx) = reply {
                    let _ = tx.send(sent);
                }
            }
            Cmd::RearmSoft { reply } => {
                self.bind.rearm_soft();
                if self.bind.adopted().is_none() {
                    self.encap_send(&[]);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::SetRelayOnly => self.bind.set_relay_only(),
            Cmd::SetOnHint { h } => {
                self.bind.set_on_hint(h);
            }
            Cmd::Rearm { reply } => {
                self.bind.rearm();
                // 无采纳路径的出站触发：空载荷 encapsulate 无会话 ⇒ 产握手 init + 搭 reg
                // （评审中-1 的 rearm 语义；有采纳时 rearmed reg 由下一出站包搭车/独立补发）
                if self.bind.adopted().is_none() {
                    self.encap_send(&[]);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::ResetPeerSession { reply } => {
                // 阶梯 R1 档动作：重建会话（保采纳）+ 清 expired 位（重建即消化过期事实，
                // 引擎兜底路径复位）+ 判据行同串（core.go:244）。
                self.rebuild_tunn();
                self.expired_pending = false;
                (self.logf)("wgcore: 已丢弃本地会话（peer 移除并写回）—— 下一发出站包将全新握手");
                let _ = reply.send(Ok(()));
            }
            Cmd::Rebind { reply } => {
                let r = self
                    .bind
                    .rebind()
                    .map(|_| ())
                    .map_err(|e| ConnErr::Dial(DialError::Stack(e.to_string())));
                let _ = reply.send(r);
            }
            Cmd::SetCandidates { cands } => {
                self.bind.set_candidates(cands);
            }
            #[cfg(feature = "test-seams")]
            Cmd::DebugPoisonSocket => {
                self.bind.poison_socket_for_test();
            }
            Cmd::TunAttach { fd, mtu, reply } => {
                let r = self.attach_tun(fd, mtu);
                let _ = reply.send(r);
            }
            Cmd::TunPacket(pkt) => {
                // 应用出站包：encap 后经 bind 发出（Go hub 的 outbound → device.Read 面）
                self.encap_send(&pkt);
            }
            Cmd::SetOnTunError { f } => {
                if let Some(tun) = self.app_tun.as_mut() {
                    tun.on_error = Some(f);
                } else {
                    self.on_tun_error = Some(f);
                }
            }
            Cmd::TunFdDead { msg } => {
                if let Some(tun) = self.app_tun.take() {
                    if let Some(cb) = tun.on_error.as_ref() {
                        cb(&msg);
                    }
                }
            }
            Cmd::Stop => return false,
        }
        true
    }

    /// L3 直通 attach（Go hub.AttachTUN）：注册应用 TUN 源 + 起读线程。fd 所有权在
    /// 扩展（坑 50）——引擎裸读写、从不 close；读线程退出靠 fd 失效（错误回调）或
    /// 引擎收工（stop 位 + fd 随扩展 destroy 报错）。重复 attach = 拒绝（保会话不断）。
    fn attach_tun(&mut self, fd: i32, mtu: u32) -> Result<(), ConnErr> {
        if self.app_tun.is_some() {
            return Err(ConnErr::Closed);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::clone(&self.tun_counters);
        let on_error = self.on_tun_error.take();
        (self.logf)(&format!(
            "wgcore: 应用面就绪（TUN fd={fd} 已接上，mtu={mtu}，transit 直通）"
        ));
        self.app_tun = Some(AppTun {
            fd,
            stop: Arc::clone(&stop),
            counters: Arc::clone(&counters),
            on_error,
        });
        let cmd_tx = self.cmd_tx_clone();
        let logf = Arc::clone(&self.logf);
        let wake = Arc::clone(&self.wake);
        let _ = thread::Builder::new()
            .name("homeway-tun-read".into())
            .spawn(move || tun_read_loop(fd, cmd_tx, wake, stop, counters, logf));
        Ok(())
    }

    fn cmd_tx_clone(&self) -> Sender<Cmd> {
        // Engine 的投递通道（构造期由 Client::start 注入的克隆；读线程投 TunPacket 用）
        self.cmd_tx.clone().expect("cmd_tx 由 Client::start 注入")
    }

    fn start_conn(
        &mut self,
        id: u64,
        dst: SocketAddrV4,
        deadline: Duration,
    ) -> Result<(), ConnErr> {
        let handle = self.stack.connect(dst)?;
        self.conns.insert(
            id,
            Conn {
                handle,
                syn_sent: false,
                local_aborted: false,
                established: false,
                deadline: Instant::now() + deadline,
                wait_est: None,
                wait_read: None,
            },
        );
        Ok(())
    }

    /// 每轮结算：建立/超时/refused/读数据/EOF + Closed 槽位回收。
    fn resolve_pending(&mut self) {
        let now = Instant::now();
        let ids: Vec<u64> = self.conns.keys().copied().collect();
        type EstReply = (Sender<Result<(), ConnErr>>, Result<(), ConnErr>);
        type ReadReply = (Sender<Result<Vec<u8>, ConnErr>>, Result<Vec<u8>, ConnErr>);
        let mut est: Vec<EstReply> = Vec::new();
        let mut reads: Vec<ReadReply> = Vec::new();
        let mut reap: Vec<SocketHandle> = Vec::new();

        for id in ids {
            let Some(c) = self.conns.get_mut(&id) else {
                continue;
            };
            let (state, can_recv, may_recv, is_active) = {
                let s = self.stack.sockets.get_mut::<TcpSocket>(c.handle);
                (s.state(), s.can_recv(), s.may_recv(), s.is_active())
            };
            if state == tcp::State::SynSent || state == tcp::State::SynReceived {
                c.syn_sent = true;
            }
            // 建立
            if !c.established && state == tcp::State::Established {
                c.established = true;
                if let Some(tx) = c.wait_est.take() {
                    est.push((tx, Ok(())));
                }
            }
            // 超时（未建立且过期限）：本地 abort（断 SYN 重传）+ Timeout
            if !c.established && now > c.deadline {
                self.stack.sockets.get_mut::<TcpSocket>(c.handle).abort();
                c.local_aborted = true;
                if let Some(tx) = c.wait_est.take() {
                    est.push((tx, Err(ConnErr::Timeout)));
                }
            }
            // refused：SynSent→Closed 且非本地 abort/超时打断 = 对端 RST（会话活着）
            if !c.established
                && c.syn_sent
                && state == tcp::State::Closed
                && !c.local_aborted
                && c.wait_est.is_some()
            {
                let tx = c.wait_est.take().unwrap();
                est.push((tx, Err(ConnErr::Refused)));
            }
            // 读结算
            if c.wait_read.is_some() {
                if can_recv {
                    let mut buf = vec![0u8; 64 * 1024];
                    let n = self
                        .stack
                        .sockets
                        .get_mut::<TcpSocket>(c.handle)
                        .recv_slice(&mut buf)
                        .unwrap_or(0);
                    if n > 0 {
                        buf.truncate(n);
                        reads.push((c.wait_read.take().unwrap(), Ok(buf)));
                    }
                } else if !may_recv || !is_active {
                    // EOF：对端已关写半（FIN ⇒ may_recv=false，socket 停在 CloseWait 但
                    // is_active 仍真——只看 is_active 会把 EOF 判成永久等待）或整连接已关
                    reads.push((c.wait_read.take().unwrap(), Err(ConnErr::Closed)));
                }
            }
            // 对端半关、缓冲已排空且无待决读：本地补 FIN 推进到 Closed（槽位回收前提；
            // smoltcp 语义——CloseWait 不会自发迁移，应用层看到 EOF 后须 close。
            // **必须等缓冲排空**：数据+FIN 先于读命令到达时，缓冲里的数据尚未消费）
            if !may_recv && !can_recv && state == tcp::State::CloseWait && c.wait_read.is_none() {
                self.stack.sockets.get_mut::<TcpSocket>(c.handle).close();
            }
            // 彻底关、缓冲排空且无待决：回收槽位（TIME_WAIT 由 poll 推进至 Closed 后
            // 回收，评审 ③-8；can_recv 兜「Closed 但缓冲还有数据」的窗口）
            if state == tcp::State::Closed
                && !can_recv
                && c.wait_est.is_none()
                && c.wait_read.is_none()
            {
                reap.push(c.handle);
            }
        }

        for (tx, r) in est {
            let _ = tx.send(r);
        }
        for (tx, r) in reads {
            let _ = tx.send(r);
        }
        if !reap.is_empty() {
            for h in &reap {
                self.stack.sockets.remove(*h);
            }
            self.conns.retain(|_, c| !reap.contains(&c.handle));
        }
    }

    fn update_snapshot(&self) {
        let st = self.bind.status();
        let (rx, tx) = self.bind.rx_tx();
        let (tries, fails) = self.bind.send_stats_pending();
        let local_err = self.bind.local_err_counters();
        let mut s = crate::syncutil::lock_unpoison(&self.snapshot);
        s.via = st.via;
        s.ep = st.ep;
        s.mirrored = st.mirrored;
        s.rx = rx;
        s.tx = tx;
        s.last_local_send_err = self.bind.last_local_send_err_at();
        s.bind_stats = Some((tries, fails));
        s.local_err = local_err;
    }
}

/// 客户端句柄（主线程/拨号线程共享面）：命令投递 + 状态轮询 + 收工（幂等，`&self`——
/// 经 `Arc<Client>` 共享时也能收口）。
pub struct Client {
    cmd_tx: mpsc::Sender<Cmd>,
    /// wake 管道写端（与引擎内 TUN 读线程共享同一把锁位——我-4；stop 取出后关闭）。
    wake_wr: Arc<Mutex<Option<i32>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
    snapshot: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
    pub tunnel_ip: Ipv4Addr,
    /// TUN 面计数（L3 直通 attach 后与设备 vpn-tun 对表；需求信号源）。
    tun_counters: Arc<TunCounters>,
    /// 发送统计的 swap 基线（拍板①：差分等价 Go 的 swap-reset）。
    last_send_swap: Mutex<(i64, i64)>,
}

impl Client {
    /// 装配 + 起驱动线程（C2 判据行在此打出）。
    pub fn start(cfg: CoreConfig) -> io::Result<Self> {
        let pubkey = cfg.identity.public_key();
        let tunnel_ip = tunnel_addr::derive_tunnel_ip(&cfg.secret, &pubkey);
        (cfg.logf)(&format!(
            "wgcore: 隧道侧就绪（L3 直通；隧道地址 {tunnel_ip}，后端隧道 IP {SERVER_TUNNEL_IP}，核心自连经 B 拨隧道 IP）"
        ));

        let bind = Bind::open(
            &cfg.candidates,
            Some(RegCtx {
                // Secret 非 Copy（F8d）：持有者各自持一份（Drop 各自擦除）
                secret: cfg.secret.clone(),
                pubkey,
                dev_tag: *cfg.identity.dev_tag().as_bytes(),
            }),
            Some(Duration::ZERO), // 直连优先窗口取缺省 2s（Go directFirst 0→2s 同义）
            cfg.peer_id.as_bytes(),
            Arc::clone(&cfg.logf),
        )?;

        // F1：wake 管道走 sysfd 单源（linux/OHOS 原子 `pipe2(O_CLOEXEC)`；darwin 建后
        // 立即补）——daemon 自 exec 是真实继承面（std `Command` 会继承未设 CLOEXEC 的
        // fd，已实测）。生命周期仍是既有 i32 形状（driver 关读端、stop 关写端）。
        let (wake_r, wake_w) = crate::sysfd::pipe_cloexec()?;
        let (wake_r, wake_w) = (
            std::os::fd::IntoRawFd::into_raw_fd(wake_r),
            std::os::fd::IntoRawFd::into_raw_fd(wake_w),
        );
        unsafe {
            libc::fcntl(wake_r, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(wake_w, libc::F_SETFL, libc::O_NONBLOCK);
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let next_id = Arc::new(AtomicU64::new(1));
        // wake 写端共享面（Client::send 与 TUN 读线程共用——我-4：读线程投包后
        // 即写管道唤醒 driver，不等 poll 超时拍）
        let wake_wr_shared = Arc::new(Mutex::new(Some(wake_w)));

        let identity_key = cfg.identity.private_key().clone();
        let secret = cfg.secret;
        let tunn = make_tunn(&identity_key, &secret, cfg.peer_id.as_bytes());
        let tun_counters = Arc::new(TunCounters::default());
        let engine = Engine {
            bind,
            tunn,
            stack: StackB::new(tunnel_ip, SERVER_TUNNEL_IP, SmolInstant::from_millis(0)),
            conns: HashMap::new(),
            udp: HashMap::new(),
            wg_buf: vec![0u8; WG_BUF],
            udp_rx_buf: vec![0u8; 65535],
            cmd_rx,
            snapshot: Arc::clone(&snapshot),
            logf: cfg.logf,
            identity_key,
            secret,
            peer_id: cfg.peer_id,
            expired_pending: false,
            silent_drops: 0,
            rx_drop_logs: 0,
            last_rx_drop_logged_at: 0,
            time0: Instant::now(),
            app_tun: None,
            tun_counters: Arc::clone(&tun_counters),
            on_tun_error: None,
            cmd_tx: Some(cmd_tx.clone()),
            wake: Arc::clone(&wake_wr_shared),
            hp_drain_ns: 0,
            hp_poll_ns: 0,
            hp_encap_ns: 0,
            hp_rx_pkts: 0,
            hp_rx_bytes: 0,
            hp_tx_pkts: 0,
            hp_tx_bytes: 0,
            hp_last_line: None,
        };

        let stop2 = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("homeway-wg".into())
            .spawn(move || driver(engine, wake_r, stop2))?;

        Ok(Self {
            cmd_tx,
            wake_wr: wake_wr_shared,
            handle: Mutex::new(Some(handle)),
            snapshot,
            stop,
            next_id,
            tunnel_ip,
            tun_counters,
            last_send_swap: Mutex::new((0, 0)),
        })
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, cmd: Cmd) {
        if self.cmd_tx.send(cmd).is_ok() {
            if let Some(fd) = *crate::syncutil::lock_unpoison(&self.wake_wr) {
                unsafe {
                    libc::write(fd, b"x".as_ptr().cast(), 1);
                }
            }
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        crate::syncutil::lock_unpoison(&self.snapshot).clone()
    }

    /// 建连（阻塞到 Established / refused / 超时；默认期限）。
    pub fn connect(&self, dst: SocketAddrV4) -> Result<u64, ConnErr> {
        self.connect_deadline(dst, CONNECT_DEADLINE)
    }

    /// 建连（显式期限——阶梯探测/PathProbe 预算用；引擎侧到点 abort）。
    pub fn connect_deadline(&self, dst: SocketAddrV4, deadline: Duration) -> Result<u64, ConnErr> {
        let id = self.alloc_id();
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Connect {
            id,
            dst,
            deadline,
            reply: tx,
        });
        rx.recv().map_err(|_| ConnErr::EngineGone)??;
        Ok(id)
    }

    /// PathProbe：拨出口必然拒绝的端口（主入口恒 :1），拿到 RST = 隧道通、出口在、
    /// 拦截层可用（C8 `判据=wg` 的依据；超时/不可达才算死）。预算下沉引擎。
    /// UDP 面（R3-3f 测试判据用）：栈内 bind，返回 (id, 本地端口)。
    pub fn udp_open(&self) -> Result<(u64, u16), ConnErr> {
        let id = self.alloc_id();
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpOpen { id, reply: tx });
        rx.recv()
            .map_err(|_| ConnErr::EngineGone)?
            .map(|port| (id, port))
    }

    pub fn udp_send(&self, id: u64, dst: SocketAddrV4, data: Vec<u8>) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpSend {
            id,
            dst,
            data,
            reply: tx,
        });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 阻塞收一包（无超时——调用方自管整体预算；会话收工 = Closed）。
    pub fn udp_recv(&self, id: u64) -> Result<(Vec<u8>, SocketAddrV4), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpRecv { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    pub fn udp_close(&self, id: u64) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpClose { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// PathProbe：拨出口必然拒绝的端口（主入口恒 :1），拿到 RST = 隧道通、出口在、
    /// 拦截层可用（C8 `判据=wg` 的依据；超时/不可达才算死）。预算下沉引擎 + **外层
    /// 硬超时**（评审 r2-M4：引擎 driver 线程卡死时 rx.recv() 永不返回 ⇒ 暖机永久
    /// preparing/巡检线程永久卡死——Go 暖机 select+time.After(warmTimeout)、巡检
    /// time.After(perTry+2s) 的双保险同义：预算 + 2s 宽限后按超时收）。
    pub fn path_probe(&self, timeout: Duration) -> Result<(), ConnErr> {
        let id = self.alloc_id();
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Connect {
            id,
            dst: SocketAddrV4::new(SERVER_TUNNEL_IP, 1),
            deadline: timeout,
            reply: tx,
        });
        const SLACK: Duration = Duration::from_secs(2);
        match rx.recv_timeout(timeout + SLACK) {
            Ok(Ok(())) => {
                // :1 真有服务（连接成功）也说明会话活着——关掉探针连接防引擎内槽位滞留
                let _ = self.close(id);
                Ok(())
            }
            Ok(Err(ConnErr::Refused)) => Ok(()),
            Ok(Err(other)) => Err(other),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ConnErr::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ConnErr::EngineGone),
        }
    }

    /// 写通道的回执等待上界（R8-3 F12：引擎线程卡死时 `rx.recv()` 永不返回 ⇒
    /// SessionWriteHalf 的外层 10s 无进展界永远走不到——每笔写自身必须有界。
    /// 正常引擎回执是即时的（背压在引擎侧以 Ok(0) 即答，不挂本通道）——10s 只
    /// 兜「引擎死了」的极端形态，与外层无进展界同刻度）。
    const WRITE_REPLY_BOUND: Duration = Duration::from_secs(10);

    pub fn write(&self, id: u64, data: Vec<u8>) -> Result<WriteOut, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Write {
            id,
            data,
            reply: tx,
        });
        match rx.recv_timeout(Self::WRITE_REPLY_BOUND) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ConnErr::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ConnErr::EngineGone),
        }
    }

    pub fn read(&self, id: u64) -> Result<Vec<u8>, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Read { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    pub fn shutdown(&self, id: u64) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Shutdown { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 有界关流（Q-F F3b/N1）：`SessionWriteHalf::close_write` 与 `SharedConn::drop`
    /// 走在**必须退出**的收工链上——引擎卡死时无界 `rx.recv()` 会把它们钉住。
    pub fn shutdown_bounded(&self, id: u64, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Shutdown { id, reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    pub fn close(&self, id: u64) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Close { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 有界关流（同 `shutdown_bounded`；`SharedConn::drop` 面）。
    pub fn close_bounded(&self, id: u64, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Close { id, reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    /// 补注册（fire-and-forget：patrol 暖机/周期刷新用）。
    pub fn refresh_reg(&self) {
        self.send(Cmd::RefreshReg { reply: None });
    }

    /// 补注册并回执（阶梯 R1 档要区分「发出/未发出」）。
    pub fn refresh_reg_result(&self) -> Result<bool, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RefreshReg { reply: Some(tx) });
        rx.recv().map_err(|_| ConnErr::EngineGone)
    }

    /// 有界补注册（Q-F F3b/N1）：调用点全在必须退出的线程（巡检拍）——引擎卡死
    /// 时无界等待会让 STOp 预算在结构上不可保。
    pub fn refresh_reg_result_bounded(&self, d: Duration) -> Result<bool, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RefreshReg { reply: Some(tx) });
        match rx.recv_timeout(d) {
            Ok(v) => Ok(v),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ConnErr::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ConnErr::EngineGone),
        }
    }

    /// 全候选发送统计的「取走清零」面（拍板①：Go SwapSendStats——巡检拍头消费；
    /// 返回 (尝试数, 本地失败数) = 本次快照累计 − 上次取走值（Bind 计数器随引擎
    /// 生命周期单调，差分等价 Go 的 swap-reset）。
    pub fn swap_send_stats(&self) -> (i64, i64) {
        let cur = crate::syncutil::lock_unpoison(&self.snapshot)
            .bind_stats
            .unwrap_or((0, 0));
        let mut last = crate::syncutil::lock_unpoison(&self.last_send_swap);
        let delta = (cur.0 - last.0, cur.1 - last.1);
        *last = cur;
        delta
    }

    /// 本地发送错误累计（tunStatusJSON demand.localErr* 两键源；Go
    /// adoptedLocalErrCount/localErrCount——拍板①补全）。
    pub fn local_err_counters(&self) -> (u64, u64) {
        crate::syncutil::lock_unpoison(&self.snapshot).local_err
    }

    /// 清采纳、重启赛跑（阶梯 R3 档动作）。
    pub fn rearm(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rearm { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 软赛跑（中继立即参与；升直连/hint 打洞用）。
    pub fn rearm_soft(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RearmSoft { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 有界软赛跑（Q-F F3b/N1）：调用点全在必须退出的线程（hint 处理 / 巡检 /
    /// 域名刷新回调）——引擎卡死时无界等待会长期占住这些线程（域名刷新的单飞位
    /// 被占则本会话所有重解析静默停摆）。
    pub fn rearm_soft_bounded(&self, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RearmSoft { reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    /// 装 hint 回调（回调在驱动线程执行——只做内存操作/通道投递）。
    pub fn set_on_hint(&self, h: crate::wtransport::bind::OnHint) {
        self.send(Cmd::SetOnHint { h });
    }

    /// 【测试缝】中继锁定（relay-lock 注入）——须在首包发出前设置（连接前的装配窗口）。
    pub fn set_relay_only(&self) {
        self.send(Cmd::SetRelayOnly);
    }

    /// 档位动作的**有界** RPC（阶梯动作预算 2s：引擎卡住时按超时收轮，不让阶梯
    /// 无限等——Go runBoundedAction 的 Rust 对应；引擎正常时节拍 ≤250ms 即回）。
    pub fn reset_peer_session_bounded(&self, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::ResetPeerSession { reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    pub fn rebind_bounded(&self, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rebind { reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    pub fn rearm_bounded(&self, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rearm { reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    pub fn refresh_reg_bounded(&self, d: Duration) -> Result<bool, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RefreshReg { reply: Some(tx) });
        match rx.recv_timeout(d) {
            Ok(v) => Ok(v),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ConnErr::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ConnErr::EngineGone),
        }
    }

    /// 丢弃本地 WG 会话（阶梯 R1 档动作；保采纳）。
    pub fn reset_peer_session(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::ResetPeerSession { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 换本地 socket（阶梯 R2 档动作；保采纳）。
    pub fn rebind(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rebind { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 更新候选集（学习缓存刷新后；R2 形态：直连地址集）。
    pub fn set_candidates(&self, cands: Vec<Candidate>) {
        self.send(Cmd::SetCandidates { cands });
    }

    /// 测试缝：模拟冻结唤醒后 OS 作废 socket（阶梯 R2 档注入）。
    #[cfg(feature = "test-seams")]
    pub fn debug_poison_socket(&self) {
        self.send(Cmd::DebugPoisonSocket);
    }

    // ---- L3 直通面（Go hub.AttachTUN/FdStats/SwapOutboundPackets/LastOutboundAt）----

    /// 注册应用 TUN 源（两阶段启动的第二阶段；只允许一次——Err(Closed) = 已 attach）。
    /// fd 所有权在扩展：引擎裸读写、从不 close。
    pub fn attach_fd(&self, fd: i32, mtu: u32) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::TunAttach { fd, mtu, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 装 TUN 面错误回调（fd 读写失败 → 挂 markUnhealthy 的面）。
    pub fn set_on_tun_error(&self, f: Box<dyn Fn(&str) + Send>) {
        self.send(Cmd::SetOnTunError { f });
    }

    /// TUN fd 计数（read = 上行 / write = 下行；attach 前全零）。
    pub fn tun_stats(&self) -> (u64, u64) {
        let c = &self.tun_counters;
        (
            c.read_bytes.load(Ordering::Relaxed),
            c.write_bytes.load(Ordering::Relaxed),
        )
    }

    /// 取走并清零「自上次调用以来的 App 出站包数」（巡检拍消费：>0 = 本拍有真实需求）。
    pub fn swap_out_pkts(&self) -> i64 {
        self.tun_counters.out_pkts.swap(0, Ordering::Relaxed)
    }

    /// 最近一次 App 出站包的时刻（单调 Instant；None = 从未——Go LastOutboundAt
    /// 的零值形态）。**必须**用 `last_outbound_mono_ns`（单调相对读数）换算——
    /// 评审 r2-H1：曾用 unix epoch ns 换算，Instant 减出 56 年前 ⇒ D4 恒不触发。
    pub fn last_outbound_at(&self) -> Option<Instant> {
        let mono = self
            .tun_counters
            .last_outbound_mono_ns
            .load(Ordering::Relaxed);
        (mono != 0).then(|| process_mono_start() + Duration::from_nanos(mono as u64))
    }

    /// 最近出站的 unix 毫秒（tunStatusJSON demand.outboundAt 源；0 = 从未）。
    pub fn last_outbound_unix_ms(&self) -> i64 {
        self.tun_counters.last_outbound_ns.load(Ordering::Relaxed) / 1_000_000
    }

    /// 收工（幂等；Drop 同义——不显式 stop 也能停线程关 fd，评审中-11）。
    /// **无界 join**（跟随引擎自然退出）——收工预算面走 [`Self::stop_within`]。
    /// 调用面（Q-G F5 起）：`Session::stop` 五段预算（Q-F F6-4 段 5）+ 隧道域五处
    /// 显式收尾（`request_stop`/`Finish::drop`/`gen_loop` 装配窗/`rebuild_session`
    /// 的 new·old）一律走有界版；**本无界版只留 `Drop for Client` 兜底**（设计 §3-D3）。
    /// 锁一律 `lock_unpoison`（Drop → stop 链上零 panic 面）。
    pub fn stop(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return; // 已收工（幂等，防 double-close fd）
        }
        self.send(Cmd::Stop);
        if let Some(h) = crate::syncutil::lock_unpoison(&self.handle).take() {
            let _ = h.join();
        }
        if let Some(fd) = crate::syncutil::lock_unpoison(&self.wake_wr).take() {
            unsafe {
                libc::close(fd);
            }
        }
    }

    /// 有界收工（Q-F F6-4 段 5）：到点放弃 join，把「JoinHandle + wake 写端」交给
    /// 一枚**收割线程**（`hw-engine-reap`）收口——`wake_wr` 只在引擎线程确认退出
    /// 后关闭（提前关会让驱动 `poll` 立刻 POLLHUP 忙转——`driver` 只判 POLLIN）。
    ///
    /// 不变式：`wake_wr` 不得留在 Option 里无人关（要么本次关、要么收割线程关；
    /// `Option::take` 天然防 double-close）。返回 `false` = 到点 detach（引擎线程
    /// 可能存活到自行退出，持 UDP fd/缓冲——残余登记见设计 §7-6）。
    pub fn stop_within(&self, deadline: Instant) -> bool {
        if self.stop.swap(true, Ordering::SeqCst) {
            return true; // 重入：已有人收过工（不重复等待/不重复关 fd）
        }
        self.send(Cmd::Stop);
        let h = crate::syncutil::lock_unpoison(&self.handle).take();
        let h = match h {
            None => return true, // 无句柄（理论窗口）：收工请求已发，视同已收
            Some(h) => h,
        };
        if crate::syncutil::wait_finished(&h, deadline) {
            let _ = h.join();
            if let Some(fd) = crate::syncutil::lock_unpoison(&self.wake_wr).take() {
                unsafe {
                    libc::close(fd);
                }
            }
            return true;
        }
        // 到点：wake 写端随 JoinHandle 交收割线程（本线程不再持有）
        let fd = crate::syncutil::lock_unpoison(&self.wake_wr).take();
        let spawned = std::thread::Builder::new()
            .name("hw-engine-reap".into())
            .spawn(move || {
                let _ = h.join();
                if let Some(fd) = fd {
                    unsafe {
                        libc::close(fd);
                    }
                }
            });
        if spawned.is_err() {
            // 极端形态（线程资源耗尽）：收割线程起不来 ⇒ 放弃 close（fd 泄漏一枚，
            // 保持打开——提前 close 会让尚在运行的驱动 poll 忙转）。JoinHandle
            // 随 drop 分离，引擎线程自行退出。
        }
        false
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 驱动循环：poll(2) 三唤醒源（UDP fd / self-pipe / 定时）。**UDP fd 每轮重取**
/// （Rebind 换 socket 后自动跟随——Go 接收循环「陈旧 socket 换新重试」的等价物）。
fn driver(mut engine: Engine, wake_r: i32, stop: Arc<AtomicBool>) {
    let mut udp_buf = Box::new([0u8; 65536]);
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let udp_fd = engine.bind.socket().as_raw_fd();
        let timeout = {
            let now = engine.now_smol();
            let delay = engine
                .stack
                .iface
                .poll_delay(now, &engine.stack.sockets)
                .unwrap_or(smoltcp::time::Duration::from_millis(POLL_CAP as u64))
                .min(smoltcp::time::Duration::from_millis(POLL_CAP as u64));
            let mut ms = delay.total_millis().clamp(1, POLL_CAP as u64);
            // 读退避余量参与超时（持续 POLLERR 形态不空转——评审低-20）
            if let Some(r) = engine.bind.recv_backoff_remain() {
                ms = ms.min(r.as_millis().max(1) as u64);
            }
            ms as i32
        };
        let mut fds = [
            libc::pollfd {
                fd: udp_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_r,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        unsafe {
            libc::poll(fds.as_mut_ptr(), 2, timeout);
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut b = [0u8; 64];
            unsafe { while libc::read(wake_r, b.as_mut_ptr().cast(), 64) > 0 {} }
        }
        if !engine.pump_once(&mut udp_buf[..]) {
            break;
        }
    }
    // TUN 读线程收口（stop 位；线程若还阻塞在 read 上则等 fd 失效自然退出——
    // fd 所有权在扩展，引擎不 close）
    if let Some(tun) = engine.app_tun.take() {
        tun.stop.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::close(wake_r);
    }
    let _ = io::stdout().flush();
}

/// 单 fd poll 的 **revents 派生就绪掩码**（Q-G F2）——`poll_fd` 的返回形态。
///
/// 三态语义（设计 §2-F2）：
/// - `readable`/`writable` = 请求的位就绪；
/// - `hup`/`err`/`nval` = **异常位**（判死只看这三个——darwin 上管道 EOF 恒带
///   `POLLIN|POLLHUP` 同置，判死**不得**依赖「readable 为假」，实测见设计 §0.4）；
/// - 全 false = **超时**（健康形态，不判死）。
#[derive(Default, Clone, Copy, Debug)]
struct Ready {
    readable: bool,
    writable: bool,
    hup: bool,
    err: bool,
    nval: bool,
}

/// TUN fd 全量写（部分写回补——包写入原子性由内核 tun 语义保证，这里兜短写）。
/// EAGAIN：OHOS 的 VPN fd 是**非阻塞**的（Go tunfd_unix.go 同款真机实证）——
/// poll(POLLOUT) 等可写再续写。POLLOUT 等待有**总预算**（评审 r2-我-5：此前在
/// 唯一 driver 线程里无界重试——fd 长期不可写时 Cmd::Stop 处理不了、Client::stop
/// 的 join 挂死；超预算按超时错误收 ⇒ 卸源 + 停止位 + 错误回调）。
///
/// Q-G F2：预算检查**写死在循环顶**（不能只靠 `poll_fd` 的内部期限——`poll_fd`
/// 在片到即返 `Ok`，缺了循环顶检查就会退化成无睡眠死循环）；`hup|err|nval` 就绪
/// 即**立即出线**（不再烧满 5s 预算——理由不是 SIGPIPE：本函数唯一调用点写的是
/// tun 字符设备，SIGPIPE 只在写「无读者的 pipe/socket」时产生）。
fn write_fd_all(fd: i32, mut buf: &[u8]) -> io::Result<()> {
    const WRITE_BUDGET: Duration = Duration::from_secs(5);
    let deadline = Instant::now() + WRITE_BUDGET;
    while !buf.is_empty() {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "tun fd 写等待超预算",
            ));
        }
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    let r = poll_fd(fd, libc::POLLOUT, deadline)?;
                    if r.hup || r.err || r.nval {
                        return Err(io::Error::new(
                            io::ErrorKind::BrokenPipe,
                            "tun fd 已失效（POLLHUP/POLLERR/POLLNVAL）——立即出线",
                        ));
                    }
                    continue; // writable 或超时 ⇒ 回循环顶复检期限
                }
                _ => return Err(e),
            }
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// 单 fd poll 等待（片长 `POLL_SLICE`；EINTR 内部重试；到 `deadline` 返 `TimedOut`；
/// 片到无事件 = `Ready::default()`）。`r > 0` 但只有未知位（`POLLPRI`/`POLLRDHUP`
/// 等）⇒ 保守按 `err` 处置（Q-G F2：比忙转安全）。
fn poll_fd(fd: i32, events: libc::c_short, deadline: Instant) -> io::Result<Ready> {
    loop {
        let now = Instant::now();
        if now >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "tun fd 等待超预算",
            ));
        }
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let slice = POLL_SLICE.min(deadline.saturating_duration_since(now));
        let ms = slice.as_millis().min(i32::MAX as u128) as i32;
        let r = unsafe { libc::poll(&mut pfd, 1, ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue; // EINTR 内部重试（调用方不感知——读/写两侧都无此分支）
            }
            return Err(e);
        }
        if r == 0 {
            return Ok(Ready::default()); // 片到无事件：超时，不判死
        }
        let rev = pfd.revents;
        let mut ready = Ready {
            readable: rev & libc::POLLIN != 0,
            writable: rev & libc::POLLOUT != 0,
            hup: rev & libc::POLLHUP != 0,
            err: rev & libc::POLLERR != 0,
            nval: rev & libc::POLLNVAL != 0,
        };
        if !ready.readable && !ready.writable && !ready.hup && !ready.err && !ready.nval {
            ready.err = true; // 只认出未知位 ⇒ 保守异常
        }
        return Ok(ready);
    }
}

/// TUN 读循环（应用出站方向）：裸 read → 计数 → 投 TunPacket 给 driver encap。
/// **OHOS 的 VPN fd 是非阻塞的**（Go tunfd_unix.go 真机实证——裸读立即返回
/// EAGAIN，被当错误上报会当场把健康隧道判死）：EAGAIN → poll(POLLIN, 一片) →
/// 再读；stop 位在 poll 片界检查（引擎收工后 ≤~500ms 内退出）。
/// 投包后**写 wake 管道**（我-4：driver 的 poll 超时上限 250ms——不写管道时上行
/// 每串包最长排队一拍；Go 是 channel 直接唤醒 wireguard-go 读循环，无此延迟）。
///
/// Q-G F2 语义（v3）：
/// - **判死只看 `hup||err||nval`**（不看 readable——darwin 上 EOF 恒带 `POLLIN`）；
/// - EAGAIN 分支**可读优先**（`readable` ⇒ 立即回读，不丢最后一包；HUP/ERR 不
///   在这里判死，落到下一轮 `read` 由既有两条路径定性）；
/// - `n==0` 分支判死前**确认一拍**（真睡眠 `DEAD_CONFIRM_DELAY` 后复 poll）；
/// - 超时（片到无事件）一律 continue（健康形态）。
///
/// 退出路径：fd 失效报错（EBADF/EINVAL = 扩展 destroy）/ stop 位 / channel 断
/// （driver 已死）。
fn tun_read_loop(
    fd: i32,
    cmd_tx: Sender<Cmd>,
    wake: Arc<Mutex<Option<i32>>>,
    stop: Arc<AtomicBool>,
    counters: Arc<TunCounters>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let mut buf = vec![0u8; 65535];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                }
                io::ErrorKind::WouldBlock => {
                    // 非阻塞 fd 的常态：等 POLLIN（片界检查 stop）。
                    match poll_fd(fd, libc::POLLIN, Instant::now() + POLL_SLICE) {
                        // 可读优先（含 POLLIN|POLLHUP 同置：先读，下一轮 read 返 0
                        // 才走确认拍）；hup/err/nval 也不在此判死——回循环顶再 read
                        // 一次，由 read 的返回值（数据 / 0 / errno）定性；片到无事件
                        // （`Ready::default()`）同样继续。
                        Ok(_ready) => {}
                        Err(pe) if pe.kind() == io::ErrorKind::TimedOut => {
                            // 片到（含 EINTR 重试耗尽片）：超时不判死
                        }
                        Err(pe) => {
                            logf(&format!("tun fd poll 失败：{pe}（标记隧道不健康）"));
                            let _ = cmd_tx.send(Cmd::TunFdDead {
                                msg: format!("tun fd poll 失败：{pe}"),
                            });
                            return;
                        }
                    }
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                }
                _ => {
                    logf(&format!("tun fd 读取失败：{e}（标记隧道不健康）"));
                    // 读失败即卸源（driver 侧同样处理写失败——两者都意味着 fd 已被收回）
                    let _ = cmd_tx.send(Cmd::TunFdDead {
                        msg: format!("tun fd 读取失败：{e}"),
                    });
                    return;
                }
            }
            continue;
        }
        if n == 0 {
            // n==0：EOF/空读形态。判死**只看 hup||err||nval**（darwin 上 EOF 恒带
            // POLLIN——旧「且无 readable」条件永不成立 ⇒ 热自旋），且判死前**确认
            // 一拍**（真睡眠后复 poll：HUP 下 poll 立返，不真睡等于没确认）。
            let first = poll_fd(fd, libc::POLLIN, Instant::now() + POLL_SLICE);
            let (dead, readable) = match &first {
                Ok(r) => (r.hup || r.err || r.nval, r.readable),
                Err(pe) if pe.kind() == io::ErrorKind::TimedOut => (false, false),
                Err(_) => (true, false), // poll 本身失败（EBADF/EIO…）⇒ 判死
            };
            let dead = if dead {
                std::thread::sleep(DEAD_CONFIRM_DELAY);
                match poll_fd(fd, libc::POLLIN, Instant::now() + POLL_SLICE) {
                    Ok(r) => r.hup || r.err || r.nval,
                    Err(pe) if pe.kind() == io::ErrorKind::TimedOut => false,
                    Err(_) => true,
                }
            } else {
                false
            };
            if dead {
                let msg = "tun fd 已失效（POLLHUP/POLLERR/POLLNVAL，确认一拍后仍成立）";
                logf(&format!("{msg}（标记隧道不健康）"));
                let _ = cmd_tx.send(Cmd::TunFdDead { msg: msg.to_owned() });
                return;
            }
            // **地板睡眠**（代码门③）：「read 返 0 且 poll 立返 POLLIN（可读但空）」
            // 的形态若无睡眠就是 100% CPU 热自旋（旧形态同样如此；OHOS VPN fd 的
            // 空读语义本仓不可取证 ⇒ 一行成本兜底）。超时形态（poll 已睡满片）与
            // HUP 形态不走这里。
            if readable {
                std::thread::sleep(DEAD_CONFIRM_DELAY);
            }
            if stop.load(Ordering::SeqCst) {
                return;
            }
            continue;
        }
        let mut n = n as usize;
        // packet-info 头自动探测（评审 r2-L4 补齐：OHOS 不需要 PI，但读侧保留探测
        // 以防万一——Go tunfd_unix.go 同款判定）：4 字节 PI 后面跟合法 IP 版本号才剥。
        let looks_like_pi = (buf[2] == 0x08 && buf[3] == 0x00 && buf[4] >> 4 == 4)
            || (buf[2] == 0x86 && buf[3] == 0xdd && buf[4] >> 4 == 6);
        if n >= 5 && buf[0] == 0 && buf[1] == 0 && looks_like_pi {
            buf.copy_within(4..n, 0);
            n -= 4;
        }
        let pkt: Vec<u8> = buf[..n].to_vec();
        counters.read_bytes.fetch_add(n as u64, Ordering::Relaxed);
        // 需求信号：App 出站包到达（demand-driven-recovery D1——只计 App 源）
        counters.out_pkts.fetch_add(1, Ordering::Relaxed);
        let now_unix = now_unix_nanos();
        let mono = process_mono_start().elapsed().as_nanos() as i64;
        counters.last_outbound_ns.store(now_unix, Ordering::Relaxed);
        counters
            .last_outbound_mono_ns
            .store(mono, Ordering::Relaxed);
        if cmd_tx.send(Cmd::TunPacket(pkt)).is_err() {
            return; // driver 已死
        }
        // 唤醒 driver（我-4：与 Client::send 同一管道——写入 1 字节即触发 POLLIN）
        if let Some(wfd) = *crate::syncutil::lock_unpoison(&*wake) {
            unsafe {
                libc::write(wfd, b"x".as_ptr().cast(), 1);
            }
        }
    }
}

fn now_unix_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boringtun::x25519::StaticSecret;

    // ---------- Q-G F2：poll_fd / write_fd_all / tun_read_loop 语义 ----------

    fn noop_logf() -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(|_s: &str| {})
    }

    fn pipe_pair() -> (i32, i32) {
        let (r, w) = crate::sysfd::pipe_cloexec().unwrap();
        (
            std::os::fd::IntoRawFd::into_raw_fd(r),
            std::os::fd::IntoRawFd::into_raw_fd(w),
        )
    }

    fn set_nonblocking(fd: i32) {
        unsafe {
            let fl = libc::fcntl(fd, libc::F_GETFL);
            libc::fcntl(fd, libc::F_SETFL, fl | libc::O_NONBLOCK);
        }
    }

    /// F2：`Ready` 四态掩码——超时全 false；**数据可读 ⇒ `readable`**；**空管道
    /// （可写）⇒ `writable`**（正向断言——两个位不得是装饰字段）；关写端后读端
    /// `hup`（**只断言 hup**，darwin 上 `readable` 恒真）；`nval` 用**高位号**
    /// 构造（低位号会被并行测试复用 ⇒ 假红）。
    #[test]
    fn poll_fd_ready_masks() {
        // ① 空管道（写端在）⇒ 超时 = 全 false（健康形态，不判死）
        let (r, w) = pipe_pair();
        let ready = poll_fd(r, libc::POLLIN, Instant::now() + Duration::from_millis(120)).unwrap();
        assert_eq!(
            (ready.readable, ready.writable, ready.hup, ready.err, ready.nval),
            (false, false, false, false, false),
            "空管道超时必须是全 false 掩码"
        );

        // ①b 正向位：空管道写端 ⇒ writable=true；有数据读端 ⇒ readable=true
        let ready = poll_fd(w, libc::POLLOUT, Instant::now() + Duration::from_millis(500)).unwrap();
        assert!(ready.writable, "空管道写端必须报 writable（got {ready:?}）");
        assert!(
            unsafe { libc::write(w, b"x".as_ptr().cast(), 1) } == 1,
            "写 1 字节"
        );
        let ready = poll_fd(r, libc::POLLIN, Instant::now() + Duration::from_millis(500)).unwrap();
        assert!(ready.readable, "有数据的读端必须报 readable（got {ready:?}）");

        // ② 关写端 ⇒ 读端 hup（darwin：EOF 与 POLLIN 同置——不断言 readable 假）
        unsafe { libc::close(w) };
        let ready = poll_fd(r, libc::POLLIN, Instant::now() + Duration::from_millis(500)).unwrap();
        assert!(ready.hup, "关写端后读端必须报 hup（got {ready:?}）");

        // ③ 写满管道 ⇒ 写端 writable（EAGAIN 形态）
        let (r2, w2) = pipe_pair();
        set_nonblocking(w2);
        let chunk = [0u8; 4096];
        let mut guard = 0;
        loop {
            let n = unsafe { libc::write(w2, chunk.as_ptr().cast(), chunk.len()) };
            if n < 0 {
                break; // EAGAIN：已满
            }
            guard += 1;
            assert!(guard < 10_000, "管道写不满？");
        }
        let ready = poll_fd(w2, libc::POLLOUT, Instant::now() + Duration::from_millis(200)).unwrap();
        // 写满且读端不读 ⇒ 既非 writable 也无 hup/err ⇒ 超时（全 false）是合法形态；
        // 但**不得**报 hup/err/nval（否则写侧会立即误出线）。
        assert!(
            !ready.hup && !ready.err && !ready.nval,
            "满管道写端不得报异常位（got {ready:?}）"
        );
        unsafe {
            libc::close(r2);
            libc::close(w2);
        }

        // ④ nval：dup2 到高位号（≥900）→ 关它 → poll 该号
        let (r3, w3) = pipe_pair();
        let hi = 900;
        assert!(unsafe { libc::dup2(r3, hi) } >= 0, "dup2 到高位号");
        unsafe { libc::close(hi) };
        let ready = poll_fd(hi, libc::POLLIN, Instant::now() + Duration::from_millis(200)).unwrap();
        assert!(ready.nval, "已关的高位 fd 必须报 nval（got {ready:?}）");
        unsafe {
            libc::close(r3);
            libc::close(w3);
        }
    }

    /// F2（U1 守卫用例）：**永不可写且无 HUP** 的 fd（读端不读、写端不关的满管道）
    /// ⇒ `write_fd_all` 在 `WRITE_BUDGET`（5s）内返 `TimedOut`。
    ///
    /// **定性（代码门⑧）**：本用例是**预算上界回归守卫**（拦「把 `poll_fd` 的期限
    /// 参数改成固定片长」一类改法），**不是**「无睡眠死循环」的判别用例——现状
    /// `poll_fd` 自身收 deadline，故修前也绿（如实登记 `docs/reviews/QG.md` §4）。
    #[test]
    fn write_all_deadline_returns_timedout() {
        let (r, w) = pipe_pair();
        set_nonblocking(w);
        let buf = vec![0u8; 1 << 20]; // 远大于管道容量 ⇒ 必进 EAGAIN 分支
        let t0 = Instant::now();
        let e = write_fd_all(w, &buf).expect_err("不可写且无 HUP ⇒ 必须超时出线");
        assert_eq!(e.kind(), io::ErrorKind::TimedOut, "{e}");
        let el = t0.elapsed();
        assert!(
            el >= Duration::from_secs(4) && el < Duration::from_secs(8),
            "必须在 5s 预算到点返回（实耗 {el:?}）"
        );
        unsafe {
            libc::close(r);
            libc::close(w);
        }
    }

    /// F2：写侧在**死 fd** 上必须立即出线（不得烧满 5s 预算）。
    /// 关读端的管道上内核直接给 `EPIPE`（Rust 测试进程 SIGPIPE 为忽略）⇒ 走
    /// `_ => return Err` 分支，**到不了** `POLLHUP/POLLERR-first` 那一支（内核先行）。
    /// 本用例钉的是**可观测上界**（< 1s 返 Err）——`hup|err|nval` 分支是纵深防御，
    /// **非判别用例**（代码门⑧ 已登记，见 `docs/reviews/QG.md` §4）。
    #[test]
    fn write_all_dead_fd_returns_promptly() {
        let (r, w) = pipe_pair();
        unsafe { libc::close(r) }; // 关读端 ⇒ 写必死
        let t0 = Instant::now();
        let e = write_fd_all(w, &[0u8; 8192]).expect_err("死 fd 必须返 Err");
        assert!(
            t0.elapsed() < Duration::from_secs(1),
            "必须立即出线（实耗 {:?}，err={e}）",
            t0.elapsed()
        );
        unsafe { libc::close(w) };
    }

    /// F2（N1，darwin 必须实测通过）：`n==0` + `POLLHUP` ⇒ 读线程 ≤1s 退出并上报
    /// `TunFdDead`。修前：判死条件含「且无 readable」在 darwin 上永不成立 ⇒ 热自旋
    /// 不退出（修前红）。
    #[test]
    fn tun_read_loop_n0_with_hup_reports_dead() {
        let (r, w) = pipe_pair();
        set_nonblocking(r);
        unsafe { libc::close(w) }; // 写端已关 ⇒ read 恒 0 + poll 恒 POLLIN|POLLHUP
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let wake: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(TunCounters::default());
        let h = thread::Builder::new()
            .name("tun-read-hup-test".into())
            .spawn(move || tun_read_loop(r, cmd_tx, wake, stop, counters, noop_logf()))
            .unwrap();
        let t0 = Instant::now();
        let mut dead = false;
        while t0.elapsed() < Duration::from_secs(1) {
            if let Ok(Cmd::TunFdDead { msg }) = cmd_rx.recv_timeout(Duration::from_millis(100)) {
                assert!(msg.contains("POLLHUP") || msg.contains("失效"), "{msg}");
                dead = true;
                break;
            }
        }
        assert!(dead, "n==0 + HUP 必须在 1s 内上报 TunFdDead（修前：热自旋不退出）");
        let t_join = Instant::now();
        while !h.is_finished() && t_join.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(h.is_finished(), "读线程必须退出");
        let _ = h.join();
    }

    /// F2：**可读优先**——写端关闭但缓冲区有数据 ⇒ 先把数据读出（不丢最后一包），
    /// 下一轮才判死上报。
    #[test]
    fn read_side_prefers_readable_on_hup_with_data() {
        let (r, w) = pipe_pair();
        set_nonblocking(r);
        unsafe { libc::write(w, b"last-packet".as_ptr().cast(), 11) };
        unsafe { libc::close(w) }; // 关写端：数据仍在管道里
        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let wake: Arc<Mutex<Option<i32>>> = Arc::new(Mutex::new(None));
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::new(TunCounters::default());
        let h = thread::Builder::new()
            .name("tun-read-pref-test".into())
            .spawn(move || tun_read_loop(r, cmd_tx, wake, stop, counters, noop_logf()))
            .unwrap();
        // 第一条消息必须是数据包（可读优先）
        match cmd_rx.recv_timeout(Duration::from_secs(1)) {
            Ok(Cmd::TunPacket(p)) => assert_eq!(p, b"last-packet"),
            Ok(_) => panic!("必须先读到最后一包，得非数据命令"),
            Err(e) => panic!("必须先读到最后一包，得 {e}"),
        }
        // 随后才是 TunFdDead
        let t0 = Instant::now();
        let mut dead = false;
        while t0.elapsed() < Duration::from_secs(1) {
            if let Ok(Cmd::TunFdDead { .. }) = cmd_rx.recv_timeout(Duration::from_millis(100)) {
                dead = true;
                break;
            }
        }
        assert!(dead, "排空后必须判死");
        let _ = h.join();
    }

    /// F2（U8）：`poll_fd` 层「`readable` 且无 hup」不是判死输入——用 `Ready` 掩码
    /// 直接钉判死谓词（n==0 健康形态构造不出，见设计 §2-F2.3）。
    #[test]
    fn ready_masks_dead_predicate_only_uses_hup_err_nval() {
        let readable_only = Ready { readable: true, ..Default::default() };
        let writable_only = Ready { writable: true, ..Default::default() };
        let timeout = Ready::default();
        for r in [readable_only, writable_only, timeout] {
            assert!(!(r.hup || r.err || r.nval), "非异常位不得进判死集（{r:?}）");
        }
        let dead = Ready { readable: true, hup: true, ..Default::default() };
        assert!(dead.hup || dead.err || dead.nval, "HUP 与 readable 同置仍判死");
    }

    /// Q-F F6-4 段 5（代码门新增覆盖）：到点 detach ⇒ `wake_wr` **本线程不再持有**
    /// （Option 已 take），由收割线程 `hw-engine-reap` 在引擎 join 后关闭——
    /// 提前 close 会让驱动 `poll` 立刻 POLLHUP 忙转，故只在引擎确认退出后关。
    #[test]
    fn stop_within_detaches_and_reaper_closes_wake_fd() {
        let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_s: &str| {});
        let client = Client::start(CoreConfig {
            peer_id: PeerId::from([5u8; 32]),
            secret: Secret::from([6u8; 32]),
            identity: Identity::ephemeral().expect("临时身份"),
            candidates: vec![],
            logf,
        })
        .expect("客户端可起");
        let fd = crate::syncutil::lock_unpoison(&client.wake_wr).expect("wake 写端在册");
        // 前置：引擎线程确未结束（否则 `wait_finished` 立即为真 ⇒ 走 join 分支，
        // 本测就测不到 detach——代码门 r15 新增 5）
        assert!(
            crate::syncutil::lock_unpoison(&client.handle)
                .as_ref()
                .is_some_and(|h| !h.is_finished()),
            "前置：引擎线程仍在跑（空候选形态刚起，不会立刻退出）"
        );
        // 期限已过 ⇒ 到点 detach（引擎线程可能还在退出路径上）
        assert!(
            !client.stop_within(Instant::now() - Duration::from_millis(1)),
            "到点必须返回 false（detach）"
        );
        assert!(
            crate::syncutil::lock_unpoison(&client.wake_wr).is_none(),
            "wake 写端已交收割线程（不得留在 Option 里无人关）"
        );
        // 收割线程 join 引擎后关 fd：F_GETFD 探测（EBADF = 已关）。
        // 已知限制（备案）：裸 fd 号探测存在 fd 复用（ABA）假活的理论可能——本测在
        // detach 后立即开始轮询（引擎 ≤~250ms 即退），复用窗口极小；真出现按 flake 记。
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let r = unsafe { libc::fcntl(fd, libc::F_GETFD) };
            if r < 0 {
                break; // EBADF：fd 已被收割线程关闭
            }
            assert!(
                Instant::now() < deadline,
                "收割线程应在 5s 内关闭 wake fd（引擎收工很快）"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn random_key() -> StaticSecret {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).unwrap();
        StaticSecret::from(b)
    }

    /// 双 Tunn 自环（noise 层互通钉死）：客户端发起 → 出口应答 → 排队内层包冲出 →
    /// 出口收到明文。PSK 混入与 x25519 面一并有假（派生错 = AEAD tag 失败此处即红）。
    #[test]
    fn two_tunn_handshake_then_queued_data() {
        let client_key = random_key();
        let server_key = random_key();
        let psk = *Psk::from(Secret::from([0x42; 32])).as_bytes();
        let mut client = Tunn::new(
            client_key.clone(),
            boringtun::x25519::PublicKey::from(&server_key),
            Some(psk),
            None,
            1,
            None,
        )
        .unwrap();
        let mut server = Tunn::new(
            server_key,
            boringtun::x25519::PublicKey::from(&client_key),
            Some(psk),
            None,
            2,
            None,
        )
        .unwrap();

        // 一个最小 IPv4 包当「内层明文」（首个出站包触发握手 + 排队）；
        // 总长字段必须正确——boringtun 按它截断（computed_len）后才回 WriteToTunnelV4。
        let mut inner = vec![0u8; 20 + 8];
        inner[0] = 0x45;
        let total = (inner.len() + 8) as u16;
        inner[2..4].copy_from_slice(&total.to_be_bytes());
        inner.extend_from_slice(b"payload!");

        let mut buf = [0u8; 65536];
        // ① 客户端 encapsulate：无会话 ⇒ 排队 + 握手 init
        let init = match client.encapsulate(&inner, &mut buf) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望握手 init，实得 {other:?}"),
        };
        // ② 出口 decapsulate：产握手应答
        let mut buf2 = [0u8; 65536];
        let resp = match server.decapsulate(None, &init, &mut buf2) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望握手应答，实得 {other:?}"),
        };
        // ③ 客户端收应答：keepalive + 建会话
        let mut buf3 = [0u8; 65536];
        match client.decapsulate(None, &resp, &mut buf3) {
            TunnResult::WriteToNetwork(_) => {} // 会话建立后的确认 keepalive
            other => panic!("期望 keepalive，实得 {other:?}"),
        }
        // ④ 空数据报重调：冲出排队的内层包
        let data = match client.decapsulate(None, &[], &mut buf3) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望排队数据冲出，实得 {other:?}"),
        };
        // ⑤ 出口解出明文
        let mut buf4 = [0u8; 65536];
        match server.decapsulate(None, &data, &mut buf4) {
            TunnResult::WriteToTunnelV4(pkt, _) => {
                assert_eq!(&pkt[inner.len() - 8..], b"payload!");
            }
            other => panic!("期望明文包，实得 {other:?}"),
        }
        // ⑥ 反向：出口 → 客户端一条数据
        let back = match server.encapsulate(&inner, &mut buf) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望数据包，实得 {other:?}"),
        };
        match client.decapsulate(None, &back, &mut buf4) {
            TunnResult::WriteToTunnelV4(pkt, _) => {
                assert_eq!(&pkt[inner.len() - 8..], b"payload!");
            }
            other => panic!("期望明文包，实得 {other:?}"),
        }
    }

    /// blackhole 反例（评审 ③-4 验收）：不可达出口 ⇒ probe 不得判通（Timeout，非 refused）。
    #[test]
    fn engine_probe_blackhole_times_out() {
        let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
        let identity = Identity::ephemeral().unwrap();
        let client = Client::start(CoreConfig {
            peer_id: PeerId::from([1; 32]),
            secret: Secret::from([2; 32]),
            identity,
            candidates: vec![Candidate {
                // TEST-NET-3 不可达地址：镜像包无人应答 ⇒ WG 永不建会话 ⇒ probe 超时
                addr: "203.0.113.1:41641".parse().unwrap(),
                relay: false,
            }],
            logf,
        })
        .unwrap();
        let t0 = Instant::now();
        let r = client.path_probe(Duration::from_millis(1500));
        assert!(
            matches!(r, Err(ConnErr::Timeout)),
            "blackhole 应 Timeout，实得 {r:?}"
        );
        assert!(t0.elapsed() < Duration::from_secs(3), "测试期限超支");
        client.stop();
    }

    /// Q-K T36：UDP 载荷上限门（F5-b）——`send_slice` 静默丢的区间在调用侧可见地失败：
    /// `> 65507` ⇒ `DatagramTooLarge`；`(1253, 65507]` **不报错**（F5-a 起由栈真分片）。
    #[test]
    fn udp_payload_gate_boundaries() {
        assert!(udp_payload_gate(0).is_ok());
        assert!(udp_payload_gate(1252).is_ok(), "MTU 内");
        assert!(udp_payload_gate(1253).is_ok(), ">MTU：F5-a 起可发（回归 R4 的静默丢）");
        assert!(udp_payload_gate(65500).is_ok());
        assert!(udp_payload_gate(65507).is_ok(), "上限内（20+8+65507 = 65535）");
        let e = udp_payload_gate(65508).unwrap_err();
        assert!(matches!(e, ConnErr::DatagramTooLarge(65508)), "越界可见失败：{e:?}");
        assert!(matches!(udp_payload_gate(70000), Err(ConnErr::DatagramTooLarge(_))));
        // 文案可用（错误链非空串）
        assert!(!ConnErr::DatagramTooLarge(65508).to_string().is_empty());
    }
}
