//! term — 出口侧终端服务（R6）。
//!
//! 分层（设计见 `docs/reviews/R6-design.md`）：
//! - [`vt`]：会话屏态的仿真底座（alacritty_terminal 0.26 适配层——坐标/模式位/脏行
//!   读取与 cell 归一化都在这一处换算，上层只见 Go `pkg/term/vt` 同形的视口模型）；
//! - [`responder`]：自建应答器（DA/DSR/DECRQM/kitty/OSC 颜色查询的 ghostty 同形应答；
//!   行为真源 = fixtures/vectors/term_responder.json）；
//! - 其余模块（frames/codec/keyenc/scan/manifest/session/leg/surface/agent）
//!   按拆步 6c–6f 陆续就位。
//!
//! 行为对齐基线 = baseline 克隆 `pkg/term/`（wire 字节与判据行逐一对齐）。

pub mod responder;
pub mod vt;
