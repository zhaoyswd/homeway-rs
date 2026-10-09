//! homeway-core —— homeway（Go，`github.com/zhaoyswd/homeway`）的 Rust 平行实现。
//!
//! 对齐纪律（见仓 AGENTS「工程原则」）：**形随手惯用，行随对齐**——形态按 Rust 习惯
//! （newtype 承担不变量、错误走类型化 enum、解析借用零拷贝），行为字节对齐 baseline
//! 克隆（基线 hash 与判据见 `docs/BASELINE.md` / `docs/INTEROP-CRITERIA.md`）。
//!
//! 模块划分按 Rust 惯例，不映射 Go 包结构 1:1；各期落位：
//! - `token`：hmw2 凭证（R0，pkg/proto/token.go 语义）
//! - `identity`（R1）、`intercept`（R3）…
//!
//! **M5 C3（WG 面删除）**：`wtransport`/`wgcore`/`session` 三模块（WG 档的候选赛跑、
//! 栈 B 引擎、服务会话）已整件退役——QUIC 岛单承载（`crate::facade::host_session`
//! 承接服务会话，`crate::stackb`/`crate::legframe`/`crate::reg2` 为其保留件）。

/// 日志面（跨线程共享的判据行输出；Session 在其上加前缀）。
pub type Logf = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// 进程级 env 开关缓存（Q-I F4：热路径 getenv 归零）。
pub(crate) mod envflag;

/// 同步与线程卫生的单源小件（Q-F F6/F7：lock_unpoison / join_bounded / spawn 失败
/// 记行——facade 经 `facade::tun_shared` 重导出，facade 内既有调用点零改动）。
pub(crate) mod syncutil;

pub mod facade;
pub mod artifact;
pub mod daemon;
pub mod files;
pub mod files_server;
pub mod go_fmt;
pub mod identity;
/// 宿主面域名解析（分档令牌池 + 有界解析 + `host:port` 切分）。
///
/// M5 C3 迁址（设计 §1.1-D4 的保留面）：原 `wtransport::domain_eps` 的**与承载无关**
/// 两件——CLI host 面拨号（`facade::host_session`）与 daemon `host reach`
/// （`daemon/hosts.rs`）共用的解析闸与拆分函数；WG 档的候选展开/重解析编排随
/// `wtransport` 删除（岛候选恒来自 token，无「学习/重解析」概念）。
pub mod hostdns;
/// 腿帧线格式（`[0xAA][peerId]` 前导 + `[0xBB][type][payload]` 封装）。
///
/// M5 S0 迁址（设计 §1.2-M1）→ M5 C3 随 `wtransport` 退役而定居 crate 根：中继
/// （红线面）与 `relaywire` 直接用它在用（QUIC 载荷 kind=5 与 `homeway-quic` 的
/// `FRAME_KIND_QUIC` 按字节复刻互锚）。
pub mod legframe;
pub mod logfile;
pub mod nodestate;
pub mod probe;
pub mod psk;
/// 注册报文 v2（`"H2" ‖ pubkey ‖ devTag ‖ ts ‖ mac`，MAC 标签 `hr-reg2`）的编码面。
///
/// M5 S0 迁址（设计 §1.2-M3）→ M5 C3 随 `wtransport` 退役而定居 crate 根：出口
/// `admit_reg4` 用它**重建 v2 报文**再喂 `table.register`（时间窗/吊销/淘汰语义逐字
/// 不变），`server/table` 测试交叉验同源。
pub mod reg2;
pub mod relay;
pub mod relaywire;
pub mod server;
pub mod session_lock;
pub mod speedtest;
pub mod speedtest_server;
/// 栈 B（smoltcp 用户态栈）：`TunDevice`（`phy::Device`）+ `StackB` + `MTU`。
///
/// M5 S0 迁址（设计 §1.2-M2）→ M5 C3 后**唯一消费者 = 出口 intercept**（生产面
/// `TunDevice` + E2E 测试泵 `StackB`）：客户端生产消费者已随 WG 面退役。
pub mod stackb;
pub mod status_json;
/// 平台系统事实单源（fd 标志 / `sockaddr_un` 上限）——Q-G F1/F4；CLI crate 复用。
pub mod sysfd;
pub mod term;
pub mod token;
pub mod tunnel_addr;
pub mod udpbatch;

/// 构建标记（探针应答 `build` 字段 / 出口能力行的单一真源）。中继与出口共用——
/// 中继此前 `Config.build` 全仓无赋值 ⇒ 恒 `"relay-dev"`（F9：探针应答上报真实构建）。
pub const BUILD_STR: &str = "homeway-rs-dev";

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
