//! QUIC 岛（M0 骨架，**零接线**）：单线程 runtime 宿主 + 同步/异步边界的构造性承载。
//!
//! 真源：`docs/QUIC-ROADMAP.md`「目标架构」+ `docs/reviews/M0-design.md` §3。
//! 本批**不被任何生产路径构造**（`homeway-core` 的依赖表里有本 crate，但源码零引用；
//! 判据见设计 §3.5 的三条「行为零改动」）。
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
//! 模块边界（设计 §3.1；M1 起长出口侧 `exit/`）：[`cmd`] = 命令通道协议（全 std 类型）；
//! [`driver`] = 岛宿主（客户端侧异步面）；`exit/` = **出口 QUIC 面**（异步面：端点 +
//! TransportConfig + RPK 身份 + 准入/数据面，M1 设计 §1.1/§1.2/§1.3/§1.4/§1.7）；`rpk` =
//! RFC 7250 的**纯 std** 字节小件（种子/PKCS#8/SPKI——同步面也读得懂）；`reg3` = `hr-reg3`
//! 注册帧的**纯 std** 字节层（`hmac`/`sha2` 非异步栈）；`sync_util` = 线程卫生小件。
//! **异步栈名字只允许出现在 `driver.rs` 与 `exit/**`**（隔离门层 3 断言）。

mod cmd;
mod driver;
mod exit;
mod reg3;
mod rpk;
mod sync_util;

#[cfg(test)]
mod tests;

pub use cmd::{Cmd, IslandErr, IslandReply, IslandSnapshot, Logf, OnUnhealthy};
pub use driver::{Island, IslandTx};
pub use exit::{
    ExitInbound, ExitQuic, ExitQuicConfig, ExitQuicErr, ExitQuicSnapshot, ExitSend, Reg3Request,
    Reg3Verdict,
};
pub use reg3::Reg3Frame;
pub use rpk::{Ed25519Seed, RpkErr, RpkPublicKey};
