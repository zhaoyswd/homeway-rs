//! QUIC 岛（M1 起**已接线**）：单线程 runtime 宿主 + 同步/异步边界 + 出口 QUIC 面 +
//! 客户端连接面/数据面。
//!
//! 真源：`docs/QUIC-ROADMAP.md`「目标架构」+ `docs/reviews/M0-design.md` §3 +
//! `docs/reviews/M1-design.md`（M1 实现期真源，含 §12 拍板与 §12.6 订正）。
//!
//! **接线状态（M0 → M1 的变更，勿照旧读）**：`homeway-core` 在 M1 起**真构造**本岛
//! （`facade/tun_exec.rs` 的 `start_island`；出口侧由 `server/engine.rs` 起 `exit::ExitQuic`）
//! ——M0 的「零接线/不被任何生产路径构造」只描述 M0 状态，已作废。
//!
//! ## 边界契约（写死，逐条由 `tools/check-quic-isolation.sh` 断言）
//!
//! - **层 0（构造性）**：本 crate 独立成 workspace member；异步栈（传输/加密/运行时）
//!   只在本 crate 的 manifest 里声明 ⇒ 消费侧（同步面）**写不出** async 代码：命名
//!   任何异步类型都会编译失败（E0433）。
//! - **层 1（公面类型清单）**：公面只出现 std 类型 + 本岛 newtype。唯二的 std-only 别名
//!   [`Logf`] / [`OnUnhealthy`] 也只承载 `std::sync::Arc<dyn Fn(&str)>`。唯一的异步内件
//!   是 [`IslandTx`] 的**私有**字段（`driver.rs`；三条封印契约见下）。
//! - **层 2（可搬运性断言，**不检出泄漏**——如实标注）**：一切异步类型都满足
//!   `Send + 'static`，故 `Send + 'static` 断言保的是「同步面搬得动」，不是「没夹带」。
//!   真正的夹带检出靠层 3 源码门 + 层 4 评审 checklist。另：[`IslandErr`] 变体**不得**
//!   携带非 std 载荷、`source()` **不得**返回异步栈错误（M0 两变体均无载荷，趁现在冻结）。
//! - **层 3（源码门）**：`tools/check-quic-isolation.sh` 五条断言（异步名字只在
//!   `driver.rs`、岛内零阻塞 `sleep`、零 `aws-lc`、产品面零 `dangerous()` …）。
//! - **层 4（文档 + 评审）**：本期设计门/代码门 checklist 恒含「异步/同步边界」条。
//!
//! ## [`IslandTx`] 三条封印（防后续提交把边界打开）
//!
//! 1. **不实现 `Deref`**（否则消费侧可穿透到通道内件）；
//! 2. **不提供取内件的 accessor**；
//! 3. **`Clone` 返回 `IslandTx`**（不外泄内件类型）。
//!
//! ## 生命周期契约（M1 起照此对接世代，详见设计 §3.5）
//!
//! - 构造点 = `gen_loop` 内、`Client::start` 之后、attach 之前；停止点 = 世代收尾链，
//!   与 `wgcore::Client` **同址同序**（`pf.stop_all()` → `bridge.stop()` → 岛
//!   `stop_within(now + CLIENT_CLOSE_BUDGET)` → 缓存终写 → `finish_generation`）；
//! - 驱动线程名 `homeway-quic`、到点收割线程名 `hw-quic-reap`（镜像既有形态）；
//! - 岛内**禁裸 spawn 无监管任务**：M1 起一律 `JoinSet` + 收工路径显式 abort + join；
//! - panic 分工（设计 §3.6）：岛内**就地**分类（记行 + 不健康回调 `"panic"`）⇒ 线程退出；
//!   预算内收工由 `stop_within` 的 join 分支记 panic 行，到点 detach 由收割线程记。
//!
//! 模块边界（设计 §3.1；M1 起长出口侧 `exit/` 与客户端侧 `client/`）：[`cmd`] = 命令通道协议
//! （全 std 类型）；[`config`] = 构造配置（凭据/绑定/巡检节拍，全 std 类型）；[`driver`] =
//! 岛宿主（客户端侧异步面）；[`client`] = 客户端连接面（异步面：端点 + 赛跑 + 登记控制流/
//! 刷新 + 迁移保持检测，M1 设计 §2.2/§2.3/§2.6）；`exit/` = **出口 QUIC 面**（异步面：
//! 端点 + TransportConfig + RPK 身份 + 准入/数据面，M1 设计 §1.1/§1.2/§1.3/§1.4/§1.7）；
//! `rpk` = RFC 7250 的**纯 std** 字节小件（种子/PKCS#8/SPKI——同步面也读得懂）；
//! `reg4` = `hr-reg4` 客户端证明协议（四帧 + 刷新帧）的**纯 std** 字节层
//! （`hmac`/`sha2`/`getrandom` 非异步栈名字 ⇒ 不占异步面白名单）；
//! `sync_util` = 线程卫生小件；`tun` = 数据面的 **TUN fd 侧**（读线程 + 有界回程队列 +
//! 写线程——全 std + libc，**零异步栈名字** ⇒ 不占异步面白名单，M1 S2-4）。
//! **异步栈名字只允许出现在 `driver.rs`、`client/**` 与 `exit/**`**（隔离门层 3 断言；
//! 白名单同步扩了 `client/**`——见 `tools/check-quic-isolation.sh` 的注释与 commit 说明）。

mod client;
mod cmd;
mod config;
mod driver;
mod exit;
mod reg4;
mod rpk;
mod sync_util;
mod tun;

#[cfg(test)]
mod tests;

pub use cmd::{
    Candidate, Cmd, DropReason, Drops, IslandErr, IslandEvent, IslandReply, IslandSnapshot, Logf,
    OnEvent, OnUnhealthy, RaceOutcome, Via,
};
pub use config::{
    IslandConfig, IslandCredential, TokenSecret, DEFAULT_PATROL, QUIC_MTU_CAP_DEFAULT,
    QUIC_MTU_CAP_MAX, QUIC_MTU_CAP_MIN,
};
pub use driver::{Island, IslandTx};
/// `serve.quic_admit` 段的已解析形态（M2 §3.2 六行七键；**纯 std**——同步面（`homeway-core`
/// 的配置层与装配点）直接读它，值域校验只有 [`AdmitLimits::validate`] 一处真源）。
pub use exit::admit::AdmitLimits;
pub use exit::{
    ExitInbound, ExitQuic, ExitQuicConfig, ExitQuicErr, ExitQuicSnapshot, ExitSend, Reg4Request,
    Reg4Verdict, RejectWhy, RetryPolicy, FRAME_KIND_QUIC,
};
// 帧层的**构造面**（组帧/解帧真源）：出口面校验与客户端组帧共用它；同步面（`homeway-core`
// 的引擎与其测试）也用它——**不得**在消费侧另写一份标签顺序。
pub use reg4::{Nonce, ProofFrame, RefreshFrame, Reg4Frame};
pub use rpk::{Ed25519Seed, RpkErr, RpkPublicKey};
