//! homeway-core —— homeway（Go，`github.com/zhaoyswd/homeway`）的 Rust 平行实现。
//!
//! 对齐纪律（见仓 AGENTS「工程原则」）：**形随手惯用，行随对齐**——形态按 Rust 习惯
//! （newtype 承担不变量、错误走类型化 enum、解析借用零拷贝），行为字节对齐 baseline
//! 克隆（基线 hash 与判据见 `docs/BASELINE.md` / `docs/INTEROP-CRITERIA.md`）。
//!
//! 模块划分按 Rust 惯例，不映射 Go 包结构 1:1；各期落位：
//! - `token`：hmw1 凭证（R0，pkg/proto/token.go 语义）
//! - 后续：`identity`（R1）、`wtransport`/`wgcore`（R1–R2）、`intercept`（R3）…

/// 日志面（跨线程共享的判据行输出；Session 在其上加前缀）。
pub type Logf = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

pub mod facade;
pub mod files;
pub mod files_server;
pub mod go_fmt;
pub mod identity;
pub mod probe;
pub mod psk;
pub mod relaywire;
pub mod server;
pub mod session;
pub mod speedtest;
pub mod speedtest_server;
pub mod status_json;
pub mod token;
pub mod tunnel_addr;
pub mod wgcore;
pub mod wtransport;
