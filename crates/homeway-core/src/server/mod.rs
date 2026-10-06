//! server：R3 出口侧模块（多 peer WG device / 腿帧收发面 / 设备表 / 台账 / 拦截层）。
//!
//! 对应 Go 侧 `internal/server/` + `pkg/servercore/` + `pkg/intercept/` + `pkg/dns/` 的 Rust 平移；
//! 模块划分按 Rust 惯例（`bind.rs` 对应 `servercore.ServerBind`——与客户端
//! `wtransport::bind`（对应 `wtransport.Bind`）同名是「同一协议两端各一个 Bind」的
//! 对称结构，全路径区分）。

pub mod bind;
pub mod bindwatch;
pub mod ddnscheck;
pub mod device;
pub mod dnsproxy;
pub mod engine;
pub mod egress;
pub mod upnp;
pub mod state;
pub mod table;
pub mod intercept;
pub mod relayleg;
pub mod txring;
