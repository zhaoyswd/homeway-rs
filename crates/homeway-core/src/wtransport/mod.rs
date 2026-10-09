//! wtransport：传输层 WG 面（直连 Bind + 端点学习缓存 + 域名端点解析）。
//!
//! R1 范围 = Go `clientcore/internal/wtransport` 的**直连子集**（设计文档 §5 1b）：
//! 候选镜像/采纳/搭车（bind）、端点缓存内存版（endpoint_cache）、域名端点
//! （domain_eps）。**不做**：漫游/换网（Rebind 家族）、中继腿、恢复阶梯档位
//! （R2 期；注意与 roadmap「R1 期」撞名——恢复阶梯整体属 R2）。
//!
//! M5 S0 迁址（设计 §1.2）：腿帧编解码（原 `frame`）→ `crate::legframe`、注册报文
//! （原 `reg`）→ `crate::reg2`——两者有**非 WG 消费者**（中继线格式 / 出口准入重建
//! v2 报文），随 WG 面删除即不可编译。本模块余下的 bind/endpoint_cache/domain_eps
//! 是 WG 客户端面，随 M5 删除（设计 §1.1 的 D2/D3/D4/D5）。

pub mod bind;
pub mod domain_eps;
pub mod endpoint_cache;

pub use bind::{Bind, Candidate, RegCtx, Status, Via};
