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

/// 拥塞控制算法选择（R8-2 归因插桩）：默认 CUBIC；环境变量 `HOMEWAY_CC=reno|none`
/// 供真机 A/B 消融（Go gVisor 默认 Reno——`reno` 臂消除「算法差 vs 发送路径差」的
/// 混杂；`none` 臂复现无 CC 塌陷形态，R8-1 评审 F4 的可复现消融义务）。
/// 不设/值不识别 ⇒ 恒 CUBIC——产品行为不变；解析一次（OnceLock），非法值静默回落。
pub(crate) fn cc_choice() -> smoltcp::socket::tcp::CongestionControl {
    use smoltcp::socket::tcp::CongestionControl;
    static CHOICE: std::sync::OnceLock<CongestionControl> = std::sync::OnceLock::new();
    *CHOICE.get_or_init(|| match std::env::var("HOMEWAY_CC").as_deref() {
        Ok("reno") => CongestionControl::Reno,
        Ok("none") => CongestionControl::None,
        _ => CongestionControl::Cubic,
    })
}

pub mod facade;
pub mod artifact;
pub mod daemon;
pub mod files;
pub mod files_server;
pub mod go_fmt;
pub mod identity;
pub mod logfile;
pub mod nodestate;
pub mod probe;
pub mod psk;
pub mod relay;
pub mod relaywire;
pub mod server;
pub mod session;
pub mod session_lock;
pub mod speedtest;
pub mod speedtest_server;
pub mod status_json;
pub mod term;
pub mod token;
pub mod tunnel_addr;
pub mod udpbatch;
pub mod wgcore;
pub mod wtransport;

/// 端口转发失败码（portfwd/err 词表——tier 台账 422 单元之一；Display = 线上词面）。
/// bind_failed = 本地监听建不起来（映射不可用但隧道不受影响）；dial_failed /
/// invalid_target 为登记保留值（本核形态尚不产出——词汇门允许缺席表在册）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortfwdErr {
    BindFailed,
}

impl PortfwdErr {
    /// 线上词面（napi-payload `code=` 同串——Go portfwd 包同源）。
    pub fn as_str(self) -> &'static str {
        match self {
            PortfwdErr::BindFailed => "bind_failed",
        }
    }
}

impl std::fmt::Display for PortfwdErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}
