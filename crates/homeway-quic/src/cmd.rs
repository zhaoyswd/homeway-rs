//! 岛的命令通道协议：**全 std 类型**（本文件禁止出现异步栈名字——隔离门层 3 断言）。
//!
//! 形态对齐今日 `wgcore` 的单驱动线程（设计 §3.2/§3.3）：投递不阻塞（unbounded，
//! 无需回执）；带 `reply` 的命令由岛在事件到点时应答；状态走 `Arc<Mutex<…>>` 轮询。

use std::net::SocketAddrV4;
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

use crate::stream::{StreamErr, StreamId, StreamTag, StreamWriteOut};

/// 同步面 → 岛。
///
/// **M0 三条成员**（骨架真落地、真被测）+ **M1 S2a 增补**（逐条照设计 §2.1 表，
/// 成员各自写明「带 reply 与否 + 理由」）。**新增成员必须照此写理由**。
///
/// `#[non_exhaustive]`（AGENTS 工程原则 1：协议帧类型；本枚举是**明确会长的跨 crate 消费面**
/// ——消费侧必须留通配臂，后续加成员不会硬断下游）。
#[non_exhaustive]
pub enum Cmd {
    /// L3 直通 attach（M1 起语义 = `wgcore::attach_tun`：**fd 所有权在扩展，岛从不 close**）。
    TunAttach {
        fd: i32,
        mtu: u32,
        reply: IslandReply<()>,
    },
    /// 应用出站包（热路径，**无 reply**——与 `wgcore::Cmd::TunPacket` 同形；队列上限与
    /// 丢弃计数口径见设计 §6.4 矩阵）。
    TunPacket(Box<[u8]>),
    /// 收工（幂等；重入无害）。
    Stop,
    /// 替换不健康回调（设计 §2.1：`mark_unhealthy_if_current(gen, …)` 家族信号）。
    /// **无 reply**：与构造期的 `on_unhealthy` 同一通话面——回调在岛线程内执行，
    /// 丢一次无害（下次信号/世代重建再装）。
    SetOnUnhealthy { h: OnUnhealthy },
    /// 安装事件回调（岛 → 同步面的分类计数事件；M0 声明面预留的 `SetOnEvent` 位）。
    /// **无 reply**：同上（安装是单向状态；`SetOnUnhealthy` 同形）。
    SetOnEvent { h: OnEvent },
    /// 更新候选清单（设计 §2.1：与 `wgcore::Cmd::SetCandidates` 同形——高频、无回执、
    /// 丢一次无害）。`Connect` 的 `cands` 为空时用它。
    SetCandidates { cands: Vec<Candidate> },
    /// 赛跑接上（设计 §2.1/§2.2）。**带 reply**：赛跑结果是同步面要等的一次性结论
    /// （via/ep/rtt 入状态面），失败必须可归因（[`IslandErr::NoCandidate`]）。
    /// `budget` = **整轮预算**（不是每候选）：到点未完成者按 drop 收。
    Connect {
        cands: Vec<Candidate>,
        budget: Duration,
        reply: IslandReply<RaceOutcome>,
    },
    /// 换本地 socket（迁移；设计 §2.1/§2.3）。**带 reply**：迁移是用户可见事件
    /// （C 系列行 + 状态面），失败必须可回报（否则同步面只能靠超时猜）。
    /// `local = None` ⇒ 通配重绑（`0.0.0.0:0`）。
    ///
    /// ⚠️ `Endpoint::rebind` 是 **endpoint 粒度**（影响该端点全部连接），旧 socket 只
    /// 续收片刻，**对端不可达没有专用错误** ⇒ 保持检测与回落判据在岛内另算
    /// （[`IslandSnapshot::migration_unconfirmed`]，节拍 = `IslandConfig::patrol`）。
    Rebind {
        local: Option<SocketAddrV4>,
        reply: IslandReply<SocketAddrV4>,
    },
    /// 巡检判活（替代 `path_probe` 的连接级判据；设计 §2.5）。**带 reply**：判活要结论；
    /// `budget` = 本次判活预算（由调用方给——巡检节拍/threshold 沿用既有常数）。
    Probe {
        budget: Duration,
        reply: IslandReply<Duration>,
    },
    /// 数据面丢弃上报（设计 §2.1 的 `DatagramDropped` 事件位）：TUN 读线程/回程线程把
    /// 自己的丢弃分类计数报进岛，岛据此入 `IslandSnapshot::drops`（N-c 行 + 状态 JSON
    /// 同源）并回调 [`OnEvent`]。**无 reply**（热路径；计数丢一次只是少记，不静默面靠
    /// 行 + 快照兜底）。
    DatagramDropped { reason: DropReason, n: u64 },
    /// TUN fd 判死（S2-4；**形态镜像 `wgcore::Cmd::TunFdDead`**，故同为 `String` 载荷 +
    /// 无 reply）。岛侧处置 = 记行 + 不健康分类 **`fd`**（既有取值集
    /// `{patrol,fd,panic,stop}` 的 `fd` 位；与 `wgcore` 的 `TunFdDead → markUnhealthy("fd")`
    /// 逐字同义）。
    ///
    /// 为什么需要它（超出设计 §2.1 六成员的**最小增补**）：QUIC 档的 L3 承载在岛上，
    /// fd 死了若只记行、不分类，App 侧就没有任何重建信号（等价面在 WG 档由 `wgcore`
    /// 提供）⇒「功能等价全局代理」不成立。
    TunFdDead { msg: String },

    // ---------- M3 S1：服务流族（STREAM tag；设计 §1.4/§1.5/§1.6） ----------
    /// 开一条服务流（§1.1：一条 bidi 流 = 一个服务会话）。
    ///
    /// **首字节 tag 由岛写**（不交给调用方）——tag 常量是 [`StreamTag`] 的单源，调用侧
    /// 拼字节就等于开了第二个真相面（协议面错位是静默的）。
    ///
    /// **带 reply**：开流是调用方要等的结论；失败必须可归因（额度耗尽/未建连/对端错配）。
    /// 岛侧**先查自记账容量**再 `open_bi`（§1.6：额度耗尽 = 快速失败，不等 5s 超时）。
    StreamOpen {
        tag: StreamTag,
        reply: StreamReply<StreamId>,
    },
    /// 服务流写（§1.4：`n` 由**非阻塞**判定给出——有界待发队列余量；**命令循环绝不
    /// `await` 到写满**）。
    ///
    /// **带 reply**：回执本身就是背压信号（[`StreamWriteOut`] 与 `wgcore::WriteOut`
    /// 逐字段同形；`n=0` 走仓内既有的 Ok(0) 分级退避环，**不得**按 `io::Write` 的
    /// 「`Ok(0)` = 通道关」处理）。
    StreamWrite {
        id: StreamId,
        data: Vec<u8>,
        reply: StreamReply<StreamWriteOut>,
    },
    /// 服务流读（§1.5：**无限期挂起**——files 空闲 5min、term 腿可挂数小时，**不得**套
    /// `QUIC_RPC_BUDGET`；取消只经 [`Cmd::StreamClose`]）。
    ///
    /// **带 reply**：每次读返回一块给本条命令的回执（**按需拉取**：岛侧不主动排空
    /// `RecvStream` 进无界通道，§1.4-①）。对端 FIN ⇒ `Err(StreamErr::Closed)`（= 今天的
    /// EOF 语义）。
    StreamRead {
        id: StreamId,
        reply: StreamReply<Vec<u8>>,
    },
    /// 半关写半边（FIN；对端仍可发——与 `wgcore::Cmd::Shutdown` 同义，§1.3/§1.5）。
    ///
    /// **带 reply**：入队即回执（FIN 的实际发出在写者任务里，与 `wgcore` 的
    /// 「`sock.close()` 后即答」同形；有界变体由调用方在回执等待上夹）。
    StreamShutdown {
        id: StreamId,
        reply: StreamReply<()>,
    },
    /// 关流/复位（abort；**同时取消在途读**——§1.5 的取消面），并从在册表摘除。
    ///
    /// **带 reply**：与 `wgcore::Cmd::Close` 同形（摘表 + 复位即答）。
    StreamClose {
        id: StreamId,
        reply: StreamReply<()>,
    },
}

/// 岛 → 同步面的单次回执口（每命令一条）。
///
/// **取回纪律**（设计 §3.6-2①，消费侧照此）：`rx.recv().map_err(|_| IslandErr::EngineGone)`
/// ——岛线程 panic 时栈上的 reply sender 被 drop ⇒ `RecvError` ⇒ 立刻归错；**禁止 `unwrap()`**
/// （否则一次岛死会把同步面挂死）。
pub type IslandReply<T> = Sender<Result<T, IslandErr>>;

/// **服务流**命令的回执口（M3 §1.6：流面错误是 typed [`StreamErr`]，不挤进 [`IslandErr`]）。
///
/// 「岛已收工」不在此枚举里：岛退出/任务被 abort 时 reply sender 随栈 drop ⇒ 调用方
/// `rx.recv()` 得 `RecvError` ⇒ 与 [`IslandReply`] 同一条纪律归 `EngineGone`。
pub type StreamReply<T> = Sender<Result<T, StreamErr>>;

/// 岛侧错误（**不得**携带非 std 载荷；`source()` **不得**返回异步栈错误——本枚举即封印面）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum IslandErr {
    /// 岛已收工（通道已断）。岛线程 panic / 退出一律映射到这里 ⇒ 同步面绝不挂死。
    #[error("岛已收工（通道已断）")]
    EngineGone,
    /// 隧道面已附加（重复 attach 拒绝）。
    #[error("隧道面已附加")]
    TunAlreadyAttached,
    /// 全候选失败（设计 §2.2 的失败面）：预算内没有任何候选完成握手。
    #[error("无候选可用（全候选未在预算内完成握手）")]
    NoCandidate,
    /// 已有赛跑在途：赛跑结论是一次性的，调用方等前一轮回执再发。
    #[error("已有赛跑在途（等前一轮回执再发）")]
    RaceInFlight,
    /// 岛未持有连接（该命令要求已建连）。
    #[error("岛未持有连接")]
    NotConnected,
    /// 连接已断（对端关闭 / 空闲回收）。
    #[error("连接已断")]
    ConnectionLost,
    /// 登记窗内连接关闭：出口拒绝（坏 MAC/表满/吊销）或链路断——客户端侧不可区分，
    /// 出口侧归因行在出口日志。
    ///
    /// **M3 §4 起**：出口带**准入关闭码**（`0x11–0x14`）的拒绝走 [`Self::AdmissionRejected`]；
    /// 本变体只剩「码不在白名单/无关闭原因/本地关」的形态（归因面**不扩**：未知码不给
    /// 可行动文案——不给未认证对端额外 Oracle）。
    #[error("登记失败（连接在登记窗内关闭）")]
    RegistrationFailed,
    /// **准入被拒**（M3 §4；**只在未绑定态映射**——设计门 F4）：出口以
    /// `CONNECTION_CLOSE(code)` 拒绝，`code` ∈ `{0x11 凭证不被接受, 0x12 资源暂不可用,
    /// 0x13 准入数据非法, 0x14 准入超时}`（取值/短语单源 = [`crate::admit_close`]）。
    ///
    /// 为什么不再是「一律 RegistrationFailed」：M2 真机发现①——三种拒绝原因在设备侧
    /// 不可见，WG 回落又让黑洞期无观测 ⇒ 用户看到的是「莫名走了 WG」。
    #[error("准入被拒（code=0x{code:02x}）")]
    AdmissionRejected {
        /// 出口写进 `CONNECTION_CLOSE` 的应用码（白名单内）。
        code: u64,
    },
    /// **会话已关闭**（准入后：被替换/设备被摘除/对端主动关；M3 §4 的独立分支）。
    ///
    /// 与 [`Self::AdmissionRejected`] 严格分开：否则「会话中被吊销/被替换」会被误报成
    /// 「准入被拒」（设计门 F4）。
    #[error("会话已关闭（{reason}）")]
    SessionClosed {
        /// 出口的关闭短语（`replaced by newer registration` / `device removed` 等；
        /// 空 = 用码值兜底）。
        reason: String,
    },
    /// 探活在预算内未获对端证据——**不误报活**（无证据即失败）。
    #[error("探活预算内无对端证据")]
    ProbeNoResponse,
    /// 重绑本地 socket 失败（绑定/非阻塞/换绑）。
    #[error("重绑本地 socket 失败（{0}）")]
    Rebind(#[source] std::io::Error),
    /// TUN 面装配失败（数据面的读/写线程起不来；`attach` 失败即无数据面——
    /// 与「半起态不留」同一条纪律）。
    #[error("TUN 面装配失败（{0}）")]
    TunAttach(#[source] std::io::Error),
}

/// 候选的承载类别（设计 §2.1 的 `Via`：候选 = 地址 + 承载类别；中继候选必须带 label
/// 才能组信封帧 `[0xAA][label8]‖[0xBB][5]‖pkt`）。
///
/// **包封/剥壳的落地 = S2-7**：S2a 只把类别带进赛跑/快照/判据行，两类候选都发起
/// `connect`（中继候选在 S2a 没有信封 socket，其连接不会完成——S2-7 换 socket 后补齐）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum Via {
    /// 直连（裸 QUIC 包）。
    Direct,
    /// 中继（`label = sha256(peerId)[:8]`，信封帧用）。
    Relay { label: [u8; 8] },
}

impl Via {
    /// 判据行取值（与既有 C 系列同词：`直连`/`中继`）——`pub` 因为世代层的交接行
    /// （`quic: 岛已建连…`）也要用同一份词表。
    pub fn text(self) -> &'static str {
        match self {
            Via::Direct => "直连",
            Via::Relay { .. } => "中继",
        }
    }

    /// 中继判别（判据行 `中继=true/false` 字段）。
    pub(crate) fn is_relay(self) -> bool {
        matches!(self, Via::Relay { .. })
    }

    /// link/JSON 面的 via 词表（`direct|relay`——与既有三态词表的非 `none` 两值同形）。
    ///
    /// 为什么映射在本 crate 内：本枚举 `#[non_exhaustive]`，**跨 crate 匹配必须带通配臂**
    /// （会给未来新增成员留一个静默归错的口子）；这里做同 crate 穷尽匹配 ⇒ 加成员时
    /// 编译期就在本处被点名。
    pub fn link_text(self) -> &'static str {
        match self {
            Via::Direct => "direct",
            Via::Relay { .. } => "relay",
        }
    }
}

/// 一个候选端点（设计 §2.1；`homeway-core` 侧在边界转换成此形态）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct Candidate {
    /// 端点地址（QUIC 端口；中继候选 = 该中继的 QUIC 形）。
    pub addr: SocketAddrV4,
    /// 承载类别（决定上行形态，见 [`Via`]）。
    pub via: Via,
}

/// 赛跑结算（设计 §2.2 的 C5'：胜者 + 候选数 + 耗时 + 完成/未完成清单）。
///
/// 「完成」清单保留**端点**而不是布尔：排障要看得见"哪个候选没起来"（r12 专2-3）。
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RaceOutcome {
    /// 胜出候选（首个完成握手者）。
    pub winner: SocketAddrV4,
    /// 胜者承载类别。
    pub via: Via,
    /// 胜者握手完成时的 RTT（ms）。
    pub rtt_ms: u64,
    /// 本轮完成握手的候选（含胜者；并行候选在胜出瞬间已完成的也会列在这里并被关闭）。
    pub completed: Vec<SocketAddrV4>,
    /// 未完成/失败的候选（含被 drop 的在途握手）。
    pub unfinished: Vec<SocketAddrV4>,
    /// 本轮耗时（ms）。
    pub elapsed_ms: u64,
}

/// 丢弃归类（设计 §2.4/§6.4 四类；**enum 而非裸计数**——AGENTS 原则 1）。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[non_exhaustive]
pub enum DropReason {
    /// 超限（内层包 > `max_datagram_size()`，含 MTU 变小后自有队列的超限包）。
    TooLarge,
    /// 发送缓冲满（per-conn 预检不过 ∨ 出站队列满）。
    SendBufferFull,
    /// 回程队列满（岛 → TUN 写线程的有界队列）。
    ReturnQueueFull,
    /// 未登记（登记前丢弃：连接未就绪/未登记/已断；含 TUN 读线程投递不进面）。
    Unregistered,
}

impl DropReason {
    /// N-c 行的字段名与顺序（`超限/发送缓冲满/回程队列满/未登记`）。
    pub(crate) fn field(self) -> usize {
        match self {
            DropReason::TooLarge => 0,
            DropReason::SendBufferFull => 1,
            DropReason::ReturnQueueFull => 2,
            DropReason::Unregistered => 3,
        }
    }

    pub(crate) fn text(self) -> &'static str {
        match self {
            DropReason::TooLarge => "超限",
            DropReason::SendBufferFull => "发送缓冲满",
            DropReason::ReturnQueueFull => "回程队列满",
            DropReason::Unregistered => "未登记",
        }
    }
}

/// 四类丢弃计数（设计 §2.4；N-c 行与状态 JSON 同源）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Drops {
    pub too_large: u64,
    pub send_buffer_full: u64,
    pub return_queue_full: u64,
    pub unregistered: u64,
}

impl Drops {
    /// 记一次/多次丢弃，返回**该类**的累计值（节流记行用它）。
    pub(crate) fn bump(&mut self, reason: DropReason, n: u64) -> u64 {
        let slot = match reason.field() {
            0 => &mut self.too_large,
            1 => &mut self.send_buffer_full,
            2 => &mut self.return_queue_full,
            _ => &mut self.unregistered,
        };
        *slot = slot.saturating_add(n);
        *slot
    }
}

/// 岛 → 同步面的事件（**分类计数事件**；`#[non_exhaustive]`：会随观测面生长）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum IslandEvent {
    /// 丢弃计数变化（`n` = 本次增量；累计值见 [`IslandSnapshot::drops`]）。
    DatagramDropped { reason: DropReason, n: u64 },
    /// 服务流已开（M3 §8.2-11 的 C19 族输入；`tag` = 服务类别）。
    StreamOpened { tag: StreamTag },
    /// 服务流已关（FIN/复位/本端 `close`；同上）。
    StreamClosed { tag: StreamTag },
}

/// 事件回调（**岛线程内**执行；只允许内存操作/通道投递——同 [`OnUnhealthy`] 纪律）。
pub type OnEvent = Arc<dyn Fn(IslandEvent) + Send + Sync + 'static>;

/// 岛侧状态快照（同步面轮询；**无阻塞**）。
///
/// M1 S2a 起为设计 §2.1 的增补形态（`via/ep/rtt/mirrors/packets_in,out/drops/mtu`），
/// 另加设计 §10 的 S2-4/S2-5/S2-7 三条判据位：`local`/`connections`（M0 §8.1 残余项
/// 「detach 后老世代 UDP 源端口/连接数可观测」）、`relay_tx`/`rx_ignored`（S2-7 的
/// 「非 kind=5 帧忽略」与中继包封读数）。
#[derive(Clone, Debug, Default)]
pub struct IslandSnapshot {
    /// 隧道面是否已 attach。
    pub attached: bool,
    /// `TunPacket` 投递计数（**收到的**包数；是否发送/丢弃见 `drops`）。
    pub packets_in: u64,
    /// 回程包计数（岛 → TUN；写线程成功写入 TUN 的包数，S2-4 起真计数）。
    pub packets_out: u64,
    /// 当前路径承载类别（`None` = 未建连）。
    pub via: Option<Via>,
    /// 当前路径端点（胜出候选）。
    pub ep: Option<SocketAddrV4>,
    /// 当前路径 RTT（ms；未建连 = 0）。
    pub rtt_ms: u64,
    /// 赛跑投出的候选总数（累计；= C5' 的「候选 N 个」输入）。
    pub mirrors: u64,
    /// 最近一次 `SetCandidates`/`Connect` 登记的候选条数。
    pub candidates: usize,
    /// `max_datagram_size()` 现值（未建连 = `None`）。
    pub mtu: Option<u32>,
    /// `current_mtu` 现值（DPLPMTUD 面；未建连 = 0）。
    pub current_mtu: u16,
    /// 四类丢弃计数（N-c 行与状态 JSON 同源）。
    pub drops: Drops,
    /// 迁移**完成**次数（重绑后收到对端回包 = 路径确认；设计 §2.3 的 N-b 行）。
    pub migrations: u64,
    /// 迁移**未确认**位（重绑后一个巡检节拍内无回包 ⇒ 置位）。回落「重连/重赛跑」的
    /// **动作**在世代层（岛只出判据：本字段 + `patrol` 分类回调；设计 §2.3）。
    pub migration_unconfirmed: bool,
    /// 当前本地地址（**UDP 源端口**；未就绪 = `None`）。detach 后仍可读 ⇒ M0 §8.1 残余项
    /// 「老世代 UDP 源端口可观测」的落点。
    pub local: Option<SocketAddrV4>,
    /// 在用连接数（0/1；岛单连接结构）。detach 后仍可读 ⇒ 同上「连接数可观测」。
    pub connections: u64,
    /// 丢失包计数（`PathStats::lost_packets`；设计 §12-④ 的 A/B 判据读数）。
    pub lost_packets: u64,
    /// 拥塞事件计数（`PathStats::congestion_events`；与 `lost_packets` 配对分辨
    /// 「限速器静默丢被 QUIC 当拥塞」与链路真丢）。
    pub congestion_events: u64,
    /// 上行包封次数（中继路径；S2-7 的读数面）。
    pub relay_tx: u64,
    /// 收到的腿帧中**非 kind=5** 的条数（忽略面；S2-7 判据的可观测位）。
    pub rx_ignored: u64,
    /// **已占用（未确认）的 DATAGRAM 发送缓冲字节数**（M1 交下项 N8① / M2 §9.1.1 的
    /// 「黑洞期已入缓冲的 1 MiB」可观测面）：无连接 = 0，满 = 每连接上限
    /// （`exit::transport::DATAGRAM_BUFFER` = 1 MiB）。瞬时量（不是累计计数）——
    /// TUN 上行的每包路径与巡检拍都会刷新它。
    pub send_buffer_used: u64,

    // ---------- M3 S1：服务流面（设计 §8.2-12 的 `quic` 段子集；出口侧计数在 S2 落） ----------
    /// 累计**开流成功**条数。
    pub streams_open: u64,
    /// **当前在册**服务流条数（瞬时量；关流/连接死即减）。
    pub streams_active: u64,
    /// 岛内**拒开**计数（额度耗尽/未建连/对端错配超时——`StreamErr::{Busy,ConnectionLost}`）。
    pub streams_refused: u64,
    /// 经 QUIC 流**发出**的应用字节（写者任务成功写出的量）。
    pub stream_bytes_out: u64,
    /// 从 QUIC 流**读回**的应用字节（读任务交付的量）。
    pub stream_bytes_in: u64,
    /// 背压事件计数（`StreamWriteOut{n:0}` 回执次数；§1.4 的 Ok(0) 面）。
    pub stream_backpressure_events: u64,
    /// `try_send` 的**非 `WouldBlock`** 错误计数（§3.1-N5；本机发送面信号）。
    pub sock_send_errs: u64,
    /// 其中命中 **errno 白名单**的计数（`{ENETUNREACH,EHOSTUNREACH,EADDRNOTAVAIL,
    /// ENETDOWN,EINVAL}` ⇒ M/R 判别里的「M（本机发送面错误）」证据；`rebind` 清零）。
    pub sock_send_errs_local: u64,
    /// **新鲜的**白名单命中（落在 `ProbeTuning::send_err_fresh` 窗内）⇒ **M（Rebind）**
    /// 动作的判据位（§3.1 的 M/R 判别：本机发送面报错 ⇒ M；无错 ⇒ R）。
    pub sock_send_err_local_fresh: bool,
    /// 末次**白名单**错误的 errno（N5 的「末次错误 kind」；`None` = 无/已随 rebind 清零）。
    pub sock_send_err_last_errno: Option<i32>,
    /// 末次**白名单**错误距本快照的毫秒数（`None` = 无/已随 rebind 清零）。
    ///
    /// 新鲜度窗由消费侧（S4）按 `ProbeTuning::send_err_fresh` 判——本字段只出「多久以前」，
    /// 不让快照面替阶梯做判定。
    pub sock_send_err_age_ms: Option<u64>,

    // ---------- M3 S4：快探阶梯（§3.1/§3.2；`quic` JSON 段的四个新键） ----------
    /// 累计**成功**快探次数（含复探/动作确认；e2e 的 `T_recv` 观测位）。
    pub ladder_probe_ok: u64,
    /// 当前连续失败次数（快探链；成功即清零）。
    pub ladder_fail_streak: u32,
    /// 当前连续抖动次数（§3.1-2：复探成功计数；≥ 阈值 ⇒ 升格为失败）。
    pub ladder_jitter_streak: u32,
    /// 最近一次动作（`""`/`migrate`/`reconnect`/`rebuild`；排障读数）。
    pub ladder_action: String,

    // ---------- M3 S5：准入失败归因（设计 §4；`quic` JSON 段的两个新键） ----------
    /// 末次**准入被拒**的应用码（`None` = 未发生过）。
    ///
    /// 单向递增语义：一旦记录即保持（世代内最后一次拒绝的归因）——回落 WG 的世代里它是
    /// App 回答「为什么走了 WG」的唯一依据（设计 §4）。
    pub admit_reject_code: Option<u64>,
    /// 上者的稳定短语（与 [`crate::admit_close::text`] 同源；`None` = 未发生/未知码）。
    pub admit_reject_text: Option<String>,
}

/// 日志落点（与同步面同形：`Arc<dyn Fn(&str) + Send + Sync>`；域前缀由调用方自带）。
pub type Logf = Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// 不健康回调（**岛线程内**执行；只允许内存操作/通道投递——M1 起 =
/// `mark_unhealthy_if_current(gen, reason)`；取值集 `{patrol, fd, panic, stop}` 是判据语义）。
pub type OnUnhealthy = Arc<dyn Fn(&str) + Send + Sync + 'static>;

