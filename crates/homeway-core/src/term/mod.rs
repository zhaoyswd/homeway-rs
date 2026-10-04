//! term — 出口侧终端服务（R6）。
//!
//! 分层（设计见 `docs/reviews/R6-design.md`）：
//! - [`vt`]：会话屏态的仿真底座（alacritty_terminal 0.26 适配层——坐标/模式位/脏行
//!   读取与 cell 归一化都在这一处换算，上层只见 Go `pkg/term/vt` 同形的视口模型）；
//! - [`responder`]：自建应答器（DA/DSR/DECRQM/kitty/OSC 颜色查询的 ghostty 同形应答；
//!   行为真源 = fixtures/vectors/term_responder.json）；
//! - [`keyenc`]：自建键/鼠标/焦点/粘贴编码器（行为真源 =
//!   fixtures/vectors/term_{keyenc,mouseenc}.json）；
//! - [`frames`]：term 帧协议编解码（op 表/词表面/HELLO 尾随块/INPUT 上行；
//!   判据 = fixtures/term/frames.v1.jsonl 冻结契约）；
//! - [`session`]：会话注册表与腿接入语义（多腿注册序/ENDED 词表应用/活动选举；
//!   纯状态机——PTY/泵/写者接线在 6f）；
//! - 其余模块（codec/scan/manifest/leg/surface/agent）按拆步 6e–6f 陆续就位。
//!
//! 行为对齐基线 = baseline 克隆 `pkg/term/`（wire 字节与判据行逐一对齐）。

pub mod frames;
pub mod session;
pub mod keyenc;
pub mod responder;
pub mod vt;
