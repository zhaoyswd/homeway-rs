//! 岛的命令通道协议：**全 std 类型**（本文件禁止出现异步栈名字——隔离门层 3 断言）。
//!
//! 形态对齐今日 `wgcore` 的单驱动线程（设计 §3.2/§3.3）：投递不阻塞（unbounded，
//! 无需回执）；带 `reply` 的命令由岛在事件到点时应答；状态走 `Arc<Mutex<…>>` 轮询。

use std::sync::mpsc::Sender;
use std::sync::Arc;

/// 同步面 → 岛。
///
/// **M0 只有三条成员**（骨架真落地、真被测；不带未构造变体——那会触发 `dead_code`）。
/// M1–M4 的命令面（`Connect`/`Migrate`/`SetCandidates`/`SetOnUnhealthy`/`SetOnEvent`/
/// `StreamOpen`/… ）由各期设计门定稿后增补；新增成员**必须**写明「带 reply 与否 + 理由」。
///
/// `#[non_exhaustive]`（AGENTS 工程原则 1：协议帧类型；本枚举是**明确会长的跨 crate 消费面**
/// ——消费侧必须留通配臂，M1 加成员不会硬断下游）。
#[non_exhaustive]
pub enum Cmd {
    /// L3 直通 attach（M1 起语义 = `wgcore::attach_tun`：**fd 所有权在扩展，岛从不 close**）。
    TunAttach {
        fd: i32,
        mtu: u32,
        reply: IslandReply<()>,
    },
    /// 应用出站包（热路径，**无 reply**——与 `wgcore::Cmd::TunPacket` 同形；
    /// 队列上限与丢弃计数是 M1 项，见设计 §8.2 R-H）。
    TunPacket(Box<[u8]>),
    /// 收工（幂等；重入无害）。
    Stop,
}

/// 岛 → 同步面的单次回执口（每命令一条）。
///
/// **取回纪律**（设计 §3.6-2①，M1 消费侧照此）：`rx.recv().map_err(|_| IslandErr::EngineGone)`
/// ——岛线程 panic 时栈上的 reply sender 被 drop ⇒ `RecvError` ⇒ 立刻归错；**禁止 `unwrap()`**
/// （否则一次岛死会把同步面挂死）。
pub type IslandReply<T> = Sender<Result<T, IslandErr>>;

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
}

/// 岛侧状态快照（同步面轮询；**无阻塞**）。M1 起增补字段/方法。
#[derive(Clone, Debug, Default)]
pub struct IslandSnapshot {
    /// 隧道面是否已 attach。
    pub attached: bool,
    /// `TunPacket` 投递计数（丢弃/超限计数是 M1 项）。
    pub packets_in: u64,
}

/// 日志落点（与同步面同形：`Arc<dyn Fn(&str) + Send + Sync>`；域前缀由调用方自带）。
pub type Logf = Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// 不健康回调（**岛线程内**执行；只允许内存操作/通道投递——M1 起 =
/// `mark_unhealthy_if_current(gen, reason)`；取值集 `{patrol, fd, panic, stop}` 是判据语义）。
pub type OnUnhealthy = Arc<dyn Fn(&str) + Send + Sync + 'static>;
