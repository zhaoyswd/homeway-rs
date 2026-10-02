//! wtransport：传输层（腿帧线格式 + 直连 Bind + reg 报文 + 端点学习缓存）。
//!
//! R1 范围 = Go `clientcore/internal/wtransport` 的**直连子集**（设计文档 §5 1b）：
//! 帧编解码（frame）、注册报文 encode（reg）、候选镜像/采纳/搭车（bind）、
//! 端点缓存内存版（endpoint_cache）。**不做**：漫游/换网（Rebind 家族）、中继腿、
//! 恢复阶梯档位（R2 期；注意与 roadmap「R1 期」撞名——恢复阶梯整体属 R2）。

pub mod bind;
pub mod endpoint_cache;
pub mod frame;
pub mod reg;

pub use bind::{Bind, Candidate, RegCtx, Status, Via};
