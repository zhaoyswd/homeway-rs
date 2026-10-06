//! 拦截层（R3；语义真源 `pkg/intercept`）。
//!
//! 挂在出口隧道侧栈上，把「WG 解密后的明文 IP 包」按目的地址分流（tun2socks 同款语义，
//! smoltcp 形态 = 包级 NAT 重写——见 `nat.rs` 头注释）：
//!
//!    dst == 隧道IP → 豁免：LocalServices 命中端口转投 UDS、其余回环同端口重拨
//!    dst == 其它   → 过境：终结（栈内 TCP 状态机）+ 本机 socket 重拨
//!
//! **拨号先行**（设计 §4.1 / 评审 H2）：TCP SYN 不立即回 SYN-ACK——先建映射缓存 SYN、
//! worker 拨 upstream 成功（DialOk → Adopt）后才建栈内 socket 注入缓存（SYN-ACK 由此
//! 产生）；失败构造 RST 回客户端。三态（建立时点/失败可见性/黑洞 10s）与 Go
//! （Forwarder 先拨号后 CreateEndpoint）等价。

pub mod dnsface;
pub mod nat;
pub mod pool;

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use smoltcp::iface::{Config as IfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::socket::udp::{self, Socket as UdpSocket};
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{HardwareAddress, IpCidr, IpEndpoint, Ipv4Address};

use crate::wgcore::stackb::TunDevice;
use crate::Logf;

use self::dnsface::{DnsFaces, DnsRoute};
use self::nat::Ipv4View;
use self::pool::{PoolCmd, PoolEvent, Upstream, WorkerPool};
use crate::server::dnsproxy::{DnsProxy, DnsReply};

/// TCP 并发上限（serve 装配层覆盖包内默认 4096——FIX-63 生产值）。
pub const MAX_CONNS: usize = 1024;
/// TCP 空闲回收（生产值；豁免腿上的长会话别收太短）。
pub const TCP_IDLE: Duration = Duration::from_secs(5 * 60);
/// UDP 会话空闲回收（与手机侧空闲回收对齐）。
pub const UDP_IDLE: Duration = Duration::from_secs(60);
/// DNS :53 会话的短回收（一问一答即闲，防挤占会话表）。
pub const DNS_IDLE: Duration = Duration::from_secs(10);
/// TCP DNS 腿的每消息空闲期限（Go ServeStream 的 tcpIdle=30s——挂住不发的客户端
/// 不能无限占连接；UDP 腿仍用 DNS_IDLE=10s）。
const TCP_DNS_IDLE: Duration = Duration::from_secs(30);
/// TCP DNS 腿并发上限（Go MaxTCPConns=64——connreg 与隧道内 listener 共表；Rust
/// 拦截腿与隧道面各自 64，合计边界差异登记）。
const MAX_TCP_DNS_LEGS: usize = 64;
/// UDP 会话上限（保险阀）。
pub const MAX_UDP_SESSIONS: usize = 4096;
/// 每五元组建会话窗口的缓冲上限（超出丢最新）。
const UDP_PENDING_MAX: usize = 16;
/// 栈内 socket 接收缓冲（过境 TCP：通告窗口面——有意放宽 vs Go rcvWnd=4096，登记差异表）。
const FLOW_BUF: usize = 256 * 1024;
/// 栈内 socket 发送缓冲（吞吐面：发端每 RTT 能维持的在途字节——对端（客户端）
/// smoltcp 延迟 ACK ~25ms ⇒ 256KB 只能维持 ~80Mbps；1MB 对齐客户端通告窗
/// （Go gVisor 无此上限）【2026-10-02 实测：20MB 下载 41.7s→2.6s】。
const FLOW_TX_BUF: usize = 1024 * 1024;
/// 背压高水位（per-flow 未确认字节；双向）。
const WATERMARK: usize = 256 * 1024;
/// 建连窗口的 SYN/重复包缓存上限。
const SYN_CACHE_MAX: usize = 4;
/// 拨号失败降噪键（kind + 原始目的；源端点不进键——见 dial_fail_seen 注释）。
type DialFailKey = (&'static str, (Ipv4Addr, u16));

// ---------- 出口发送整形（R8-3 8i；设计 = docs/reviews/R8.md §九） ----------
//
// 归因背景（PERF-AB §9）：Rust 出口单 poll 把拦截栈排空的 ≤2379 包（≈3.1MB）一次
// 倾泻上线——团块在接收端/空口成组丢失（冷 WiFi 电源态尤甚，§9.5 同步悬崖）。本整形
// = **字节令牌桶 + 下拍续传**：pump 尾把本拍产物并入滞留 FIFO，按令牌从头释放；余量
// 留给后续 pump 拍（驱动线程每拍必调——poll 5ms 上界 + ACK 到达即醒；5ms 续水
// 320KB ≥ 2×突发额度 ⇒ 桶不会连续两拍枯竭）。只削峰不平率：稳态到达 < 速率时令牌
// 常满、零延迟直通；平均率仍由 ACK 时钟（栈内 CUBIC）决定——整形器不注入流量，
// 滞留深度 ≤ Σcwnd（TCP 在途记账自钳制），**不设显式上限**（上限 = 整形器丢包 =
// 伪修复禁区）。

/// 整形速率默认 200 MiB/s（D-2 8n② 修订；`HOMEWAY_TX_RATE_MBPS` 覆盖）。
/// R8-3 的 64MiB/s（层 0 天花板 94 的 0.68×）在 40MB/s 级需求下**把稳态吞吐钳进
/// 桶并注入 RTT**：滞留队列常驻 ~500KB ⇒ 排队延迟 ≈ 深度/64MiB/s ≈ 8ms ⇒ 真机
/// TCP RTT 17-27ms（Go 出口同刻 6ms）⇒ 吞吐 = 窗/RTT 同比塌到 17.5MB/s。真机
/// 梯度（同小时同机）：64/160KiB=17.5 稳、128/256 与 160/512=锯齿带（拍频-ACK 团
/// 耦合）、200/2048=30-40 稳（Go 出口同刻 41-46）；冷连 200/2048 平滑爬坡无悬崖
/// （冷/热 0.77 ≥ 0.70 门）。**R 的职责修正**：本整形器的存在意义是团块钳制
/// （冷连悬崖），不是速率限制——R 取 ~2× 层 0 天花板（均值面对 3MB/s 级极端场景
/// 仍有钳制），路径容量由 ACK 时钟（栈内 CUBIC）自管。
pub const TX_SHAPE_RATE: u64 = 200 * 1024 * 1024;
/// 突发额度默认 256 KiB = **桶容量 = 单拍放行上界**（D-2 8n③；`HOMEWAY_TX_BURST_KB`
/// 覆盖；两义同值——见 shape_slice 的不变量注释）。梯度数据：2048KB 形态消除了
/// 排队延迟但团块 1-2MB 在空口成组丢失（dup 260-657/5s → CUBIC 反复砍窗，B 稳
/// 19-40 波动）；256KB = 修复前 3.1MB 倾泻的 1/12、≈ Go 出口自然发送团的量级——
/// 团块钳制与无排队同时成立（B 热态 25-28 平稳无锯齿）。
pub const TX_SHAPE_BURST: usize = 256 * 1024;

/// pacing 模式（D-3 8r；**反过拟合约束 1：节奏自适应而非固定间隔**——固定 ~10µs
/// 对 1280B 包的极限 ~1Gbps，2.5G+ 快路径会变人为限速；固定 128MiB/s 对空口 94MB/s
/// 的「留交织窗」算式方向也反〔评审 r1-5.3〕）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaceMode {
    /// 自适应（config `pacing="adaptive"` / env `HOMEWAY_TX_PACING=on`）：
    /// pace = clamp(TX_PACE_GAIN×est, FLOOR, CEIL)，
    /// est = ACK 时钟吞吐估计（Interceptor 维护，稳态 ≈ cwnd/RTT——smoltcp 0.14
    /// 不暴露 cwnd/ssthresh〔R8-8a 已核〕，ACK 时钟测其等价量，Linux fq pacing 的
    /// delivered-rate 派生同型）；同时桶均值抬到 ≥ TX_RATE_GAIN×est（快路径不设
    /// 人为上限）。增益 1.2 使 CC 平衡点（吞吐=est）永不被节流，同时 ACK 时钟
    /// 团块（produce 率=est）贴着 pace 排空——每团真展开（2.0 档实测不 engage，
    /// 见 TX_PACE_GAIN 注释）。
    Adaptive,
    /// 固定速率（B/s）——env `HOMEWAY_TX_PACE_MBPS` / config `pace_mbps` 的诊断
    /// 梯度臂（与 adaptive 的 A/B 判别用；不抬桶均值）。
    Fixed(u64),
}

/// 自适应增益/上下限（数值依据 = §十二）：
/// - GAIN 1.2（Linux fq 稳态档：pacing_rate = cwnd/RTT × 1.2，慢启动才 ×2——慢启动
///   的松化由 est 的 max 攻击承担：爬坡窗的峰值速率被 est 锁存 ⇒ pace 随之抬升）。
///   2.0 档真机消融实测的失效机理（2026-10-06 日间带，A 臂 32 / baseline 13）：
///   稳态 produce 率 = ACK 时钟 ≈ est ≪ 2×est ⇒ 时刻表恒在过去、pacing 整体不
///   engage——ACK 时钟团块（~23KB/团）原样直通，wire 形态与 off 臂无差。1.2 档：
///   每团排空 ~1.7ms（真展开），同时 20% 余量使 CC 平衡点（吞吐=est）永不被
///   节流（无 R=64MiB/s 事故型的持续排队；est 测的就是被 pace 后的 ACK 时钟，
///   收敛点 est=g、pace=1.2g ≥ g 自洽）；
/// - FLOOR 16MiB/s：est 下界 TX_PACE_EST0 已保证间隔 ≤10µs/1280B（无慢路径滴流），
///   此地板是 est 参数调整时的防御位；
/// - CEIL 4GiB/s：2.5G 出口（312MB/s×2=625MB/s）≪ 4G——快路径不构成瓶颈。
pub const TX_PACE_GAIN: f64 = 1.2;
pub const TX_PACE_FLOOR: u64 = 16 * 1024 * 1024;
pub const TX_PACE_CEIL: u64 = 4 * 1024 * 1024 * 1024;
/// est 初值与衰减下界 8MiB/s（**注意有效 pace 下界 = max(TX_PACE_FLOOR,
/// GAIN×est 下界) = 16MiB/s**——FLOOR 是当前生效钳位；est 下界的作用是把 est 钉在
/// 贴实测的量级，防 8MiB 以下路径的滴流误判）：重启/空闲后 pace 从 16MiB/s 起步——首个 100ms
/// 估计窗内 max 攻击即跳到实测速率，期间更紧的 pacing 只作用于首个团（冷形态
/// 友好）。下界防滴流（9.6MiB/s 的 pace 下界 ≫ est<8 的任何路径需求——20Mbps
/// 蜂窝形态 pace 恒不绑定）。16MiB 档真机实测会把 est 钉在下界（日间带 goodput
/// 11-14 < 16）⇒ pace 与实测脱钩；8 档让 10MB/s 级路径的 est 贴实测。
pub const TX_PACE_EST0: f64 = 8.0 * 1024.0 * 1024.0;
/// 补账量子（单次释放的字节上界，pacing on 时生效）：`max(pace×2ms, 2×MSS)`。
/// 驱动循环迟到时时刻表积欠一次放多包——**量子把补账团块钉住**（而非 burst 的
/// 256KB：冷空口臂实测抓到 256KB 补账团击穿 192KiB 到达预算——25 到达丢失，量子
/// 收紧后归零）。2ms 档的取舍：补账排空速率 = 量子×拍频须 ≥ pace——真机拍频
/// ~20k/s（50µs pselect）余量 40×；harness 泵 ~0.6-1.5k/s（负载机实测 630/s）余量
/// ~2.5×（500µs 档在负载机上把快臂压到 0.63×off——排空能力不足的实测教训）；
/// 换来补账团块上界 pace×2ms（设备带 50-90MB/s ⇒ 100-180KB ≈ Go 出口自然团量级
/// 〔§9.10〕——常态（无迟到达）释放仍由时刻表按拍距散布，量子只封迟到尾部）。
pub const TX_PACE_QUANTUM: Duration = Duration::from_millis(2);
/// pacing on 时的桶均值抬升增益（R ≥ 4×est）：WiFi 形态 est≈45 ⇒ 4×45=180 < 200
/// ⇒ R 不变（8n③ 语义）；2.5G 形态 est≈312 ⇒ R≈1.25GB/s——桶不再是快路径瓶颈。
pub const TX_RATE_GAIN: f64 = 4.0;
/// est 的估计窗（ACK 确认字节/窗长；窗粗于团块 ⇒ 团内 ACK 速率尖峰被平均掉，
/// est 反映的是持续速率）与每窗衰减系数（max 攻击、慢衰减——无振荡面）。
pub const ACK_EST_WIN: Duration = Duration::from_millis(100);
pub const ACK_EST_DECAY: f64 = 0.95;

/// 出口发送整形参数（字节令牌桶 + 逐包时刻表；用户面形态——Adaptive 在使用点经
/// `tx_shape_eff` 解析为具体速率）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TxShape {
    /// 续水速率（B/s）。Adaptive pacing 下这只是**下界**（R_eff = max(rate, 4×est)）。
    pub rate: u64,
    /// 突发额度 = 令牌容量（B）：任意窗口 w 内线上深度 ≤ burst + rate·w。
    pub burst: usize,
    /// 逐包 pacing：None = 时间门关（= 8n③ 纯桶形态，滞留非空时驱动拍 1ms）；
    /// Some = 开（Some(PaceMode) 语义见枚举）。
    pub pace: Option<PaceMode>,
}

/// config.toml `[serve.tx_shape]` 节（D-3 反过拟合约束 3：参数 config 化；env 臂
/// 保留为测试缝，覆盖序 **env > config > 产品默认**）。deny_unknown = typo 保护。
#[derive(serde::Deserialize, Default, Clone, Copy, Debug)]
#[serde(deny_unknown_fields)]
pub struct TxShapeCfg {
    /// 桶均值钳制速率下界（MiB/s，默认 200；pacing=adaptive 时实际取
    /// max(rate, 4×est)）。适用形态注释：家宽/无线出口按默认即可；极快路径
    /// （≥1Gbps）交给 adaptive 自动抬升，无需手调。
    #[serde(default)]
    pub rate_mbps: Option<u64>,
    /// 单拍放行上界/桶容量（KiB，默认 256）。
    #[serde(default)]
    pub burst_kb: Option<usize>,
    /// 逐包 pacing 模式：`off`（默认——D-3 止损裁定，见 tx_shape_resolve 注释）
    /// | `adaptive` | `fixed`。适用形态：接收端无密集 ACK 时钟的路径（旧核手机、
    /// 经 TUN 的应用流量）与快路径（≥1Gbps）建议 `adaptive`。
    #[serde(default)]
    pub pacing: Option<PaceCfg>,
    /// pacing=fixed 时的速率（MiB/s）——诊断/梯度臂用；未给值回落 adaptive。
    #[serde(default)]
    pub pace_mbps: Option<u64>,
}

/// config 的 pacing 模式值面。
#[derive(serde::Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PaceCfg {
    Adaptive,
    Fixed,
    Off,
}

/// 整形参数解析（D-3：config 面 + env 测试缝，覆盖序 **env > config > 产品默认**）。
///
/// - 总开关 `HOMEWAY_TX_SHAPING`（**值匹配**：未设/`on`/`1`/`true` = 开，`off`/`0`/
///   `false` = 整套关——评审 r2 自补1 的惯例：presence 语义会把消融方向搞反）；
/// - pacing 门 `HOMEWAY_TX_PACING`（同值匹配惯例）单独关**时间门**（= 8n③ 纯桶
///   形态——消融对照面）；`HOMEWAY_TX_PACE_MBPS` = Fixed 梯度臂（>0 生效）；
/// - `HOMEWAY_TX_RATE_MBPS` / `HOMEWAY_TX_BURST_KB` 覆盖桶参数；
/// - config 面 = `TxShapeCfg`（[serve.tx_shape]）；
/// - Fixed 臂交叉钳制 pace ≤ rate（pace > rate 时 credit 门长期闭合 + min 等待
///   ⇒ 纯自旋——评审 r1-1.1）；
/// - 非法值静默回落默认。
pub(crate) fn tx_shape_resolve(cfg: Option<TxShapeCfg>) -> Option<TxShape> {
    match std::env::var("HOMEWAY_TX_SHAPING").as_deref() {
        Ok("off") | Ok("0") | Ok("false") => return None, // 消融臂（整套显式关）
        Ok(_) => {}                                      // on/1/true/其它 = 开
        Err(_) => {}                                     // 未设 = 产品默认开
    }
    let cfg = cfg.unwrap_or_default();
    let mut rate = TX_SHAPE_RATE;
    let mut burst = TX_SHAPE_BURST;
    if let Some(r) = cfg.rate_mbps {
        if r > 0 {
            rate = r * 1024 * 1024;
        }
    }
    if let Some(b) = cfg.burst_kb {
        if b > 0 {
            burst = b * 1024;
        }
    }
    // 默认 off（D-3 真机消融的止损裁定）：8s 密集 ACK 在位时 pacing 无增益
    //（18.1 vs 19.2 同带）且组合形态出现 2-4MB 常驻整形滞留（CUBIC 窗过冲改停
    // 整形队列——RTT 代价）；8r 单独对旧核 +32%，但整链默认受益者是 8s。8r 保留
    // 全套机制与开关面：无 8s 的接收端（旧核手机/Go 核/经 TUN 的应用流量——OHOS
    // 内核 ACK 时钟不受 8s 覆盖）与快路径由 config/env 显式开启。
    let mut pace = match cfg.pacing {
        Some(PaceCfg::Adaptive) => Some(PaceMode::Adaptive),
        // fixed 缺/非法 pace_mbps ⇒ 回落 adaptive（与 TxShapeCfg 字段注释一致——
        // 评审 r2-2.7：静默关与注释承诺相反）
        Some(PaceCfg::Fixed) => match cfg.pace_mbps {
            Some(n) if n > 0 => Some(PaceMode::Fixed(n * 1024 * 1024)),
            _ => Some(PaceMode::Adaptive),
        },
        Some(PaceCfg::Off) | None => None,
    };
    let mut pacing_explicit_off = false;
    match std::env::var("HOMEWAY_TX_PACING").as_deref() {
        Ok("off") | Ok("0") | Ok("false") => {
            pace = None;
            pacing_explicit_off = true; // 显式关最终胜出（r2-2.6：梯度缝不得掀翻消融臂）
        }
        Ok("on") | Ok("adaptive") | Ok("1") | Ok("true") => {
            pace = Some(PaceMode::Adaptive)
        }
        Ok(_) | Err(_) => {}
    }
    if !pacing_explicit_off {
        if let Ok(v) = std::env::var("HOMEWAY_TX_PACE_MBPS") {
            if let Ok(n) = v.trim().parse::<u64>() {
                if n > 0 {
                    pace = Some(PaceMode::Fixed(n * 1024 * 1024));
                }
            }
        }
    }
    if let Ok(v) = std::env::var("HOMEWAY_TX_RATE_MBPS") {
        if let Ok(n) = v.trim().parse::<u64>() {
            if n > 0 {
                rate = n * 1024 * 1024;
            }
        }
    }
    if let Ok(v) = std::env::var("HOMEWAY_TX_BURST_KB") {
        if let Ok(n) = v.trim().parse::<usize>() {
            if n > 0 {
                burst = n * 1024;
            }
        }
    }
    if let Some(PaceMode::Fixed(p)) = pace {
        if p > rate {
            pace = Some(PaceMode::Fixed(rate)); // 交叉钳制：防 credit 闭合期的纯自旋
        }
    }
    Some(TxShape { rate, burst, pace })
}

/// shape_slice 的已解析参数（`tx_shape_eff` 的产物：Adaptive 已折算成具体速率的
/// Fixed 形态——纯函数只认数值）。
#[derive(Clone, Copy)]
struct ShapeResolved {
    rate: u64,
    burst: usize,
    /// 已解析的 pacing 速率（None = 时间门关）。
    pace: Option<u64>,
}

/// shape_slice 的运行态进出（credit/时刻表/滞留字节——调用方持有、按值进出，
/// 纯函数可注入）。
#[derive(Clone, Copy)]
struct ShapeRun {
    credit: f64,
    pace_next: Instant,
    /// 并入前的队存字节（与 tx_deferred_bytes 同源的 O(1) 维护量）。
    deferred_bytes: usize,
}

/// 令牌桶释放的一拍（纯函数面——单测可注入时间；参数 = `tx_shape_eff` 解析后的
/// 具体速率值，Adaptive 不进这里）：`produce` 并入 `deferred` 尾部（FIFO 保序），
/// 先按 `dt` 续水（容量 = burst），再从头释放「桶 credit 与（若开）**pacing 时刻
/// 表**」都放行的包。返回 (本拍释放, ShapeRun 余态〔credit/时刻表/滞留字节〕)。
/// 大于 burst 的包
/// 防御性直通（内层 IP 恒 ≤ 64KB < 默认 burst；防极小 burst 配置把队列头部卡死）。
///
/// 8r 逐包时刻表（设计 docs/reviews/R8.md §十二）：桶门之上串联时间门——团内包按
/// `len/pace` 间隔散布。时刻表 **advisory**：`now` 已过的时刻全放（驱动循环迟到
/// 一次放多包补账，任意窗内平均放行率恒 = pace、不累积漂移；**单次补账上界 ≤
/// burst**〔credit 不变量自带〕⇒ pacer 最坏形态 = 8n③ 单拍形态，不会更坏）；
/// 时刻按 `pace_next += len/pace` 严格前进（**不向 now 钳**——钳了会把迟到时长
/// 折进下一包间隔，补账退化成每唤醒一包）；队列排空时时刻表重置到 now（空闲期
/// 不积累放行额度——否则空闲后的首个团会被整团一次放出）。稳态到达 < pace 时
/// 时刻恒在过去 ⇒ 直通零延迟。
fn shape_slice(
    deferred: &mut std::collections::VecDeque<Vec<u8>>,
    produce: Vec<Vec<u8>>,
    run_in: ShapeRun,
    p: ShapeResolved,
    dt: f64,
    now: Instant,
    two_mtu: usize,
) -> (Vec<Vec<u8>>, ShapeRun) {
    let ShapeResolved { rate, burst, pace } = p;
    let ShapeRun { mut credit, mut pace_next, deferred_bytes } = run_in;
    // 并入字节先记账（评审 r2-4.6：释放路径的字节维护量改增量——pacing-on 的深
    // 滞留形态〔实测 2-3MB〕下每拍 O(队深) 重扫 + 按队深预分配在 ~20k 拍频下是
    // GB/s 级 churn）。滞留字节并入侧自增、释放侧自减、余量随 ShapeRun 出——O(1)。
    let mut deferred_bytes =
        deferred_bytes + produce.iter().map(|p| p.len()).sum::<usize>();
    deferred.extend(produce);
    credit = (credit + rate as f64 * dt).min(burst as f64);
    // 补账量子（8r 改版）：单次释放 ≤ max(pace×QUANTUM, 2×MTU)——迟到的时刻表
    // 积欠跨多拍排空（每拍一团），不把 256KB 的桶上限当补账上界。P2：2×MTU 随
    // 拦截栈内层 MTU（1380 档 = 2 满段口径随动；评审 P2-r1-6）。
    let quantum = pace
        .map(|p| (p as f64 * TX_PACE_QUANTUM.as_secs_f64()).max(two_mtu as f64) as usize)
        .unwrap_or(usize::MAX);
    let mut released = 0usize;
    // 容量按量子上界估（不按队深）：本拍释放至多 quantum/最小包 个
    let mut out = Vec::with_capacity((quantum / 256).min(deferred.len()));
    // burst 语义（评审 1.1 认账修订）：**桶容量 = 单拍放行上界（同值）**——不变量
    // 由 credit 逐包扣减自带：每拍开始 credit ≤ burst、每放一包 credit -= fl ⇒ 单拍
    // 释放总量恒 ≤ burst（曾手写的 beat 计数与该不变量同值，是死逻辑，已删）。**调大
    // burst 等于同时放开累积额度与单拍倾泻**（真机梯度：桶 2MB 时团块 1-2MB 在空口
    // 成组丢失、dup 260-657/5s、CUBIC 反复砍窗）——256KiB = 修复前 3.1MB 倾泻的
    // 1/12 ≈ Go 出口的自然发送团量级。pacing on 时时间门在排空期更紧（串联门，
    // 释放率 = min(桶门, 时间门)——非并联防御），桶继续钳均值与极端场景。
    while let Some(front) = deferred.front() {
        let fl = front.len() as f64;
        if fl > burst as f64 {
            // 防死锁直通 ≠ 参与记账（评审 r2-1.2 + 8r 同口径）：极小 burst 配置下直通
            // 分支不扣 credit、不查时间门、不推进时刻表——扣了会打成负值/卡时刻表，
            // 后续包要等续水补回才放行。
        } else {
            if credit < fl {
                break;
            }
            if let Some(pace) = pace {
                if now < pace_next {
                    break;
                }
                pace_next += Duration::from_secs_f64(fl / pace as f64);
            }
            credit -= fl;
        }
        out.push(deferred.pop_front().expect("front 已判"));
        released += fl as usize;
        deferred_bytes -= fl as usize;
        if released >= quantum {
            break; // 补账量子：余量留下一拍（时刻表已推进，下拍无条件可放）
        }
    }
    if deferred.is_empty() {
        // 队列排空 = 时刻表重置（见函数头注释：空闲期不积累放行额度）
        pace_next = now;
    }
    (out, ShapeRun { credit, pace_next, deferred_bytes })
}

// 「真丢包」检测与发送塑形的历史注记（R6.6 应用层 CC 垫片，R8-8a 随 smoltcp
// 0.11→0.14 迁移**整体退役**）：拥塞控制/重传退避/零窗探测现由栈内
// `CongestionControl::Cubic`（RFC 合规）承担——cwnd 门、pacing、seq 回退检测、
// ACK 停滞判据全部删除（ROADMAP「R7 前置批 smoltcp 0.14 工单」闭环）。

/// 转发面计数器（拦截层是唯一生产写入方；键名 = 观测面契约：dialok/dialfail/flows/rejected）。
#[derive(Default)]
pub struct Stats {
    dial_ok: AtomicU64,
    dial_fail: AtomicU64,
    flows: AtomicU64,
    rejected: AtomicU64,
    /// transit UDP 会话的归宿（收到过回包 / 只有上行——udpcap 实测位）。
    udp_replied: AtomicU64,
    udp_no_reply: AtomicU64,
}

impl Stats {
    pub fn incr_ok(&self) {
        self.dial_ok.fetch_add(1, Ordering::Relaxed);
    }
    pub fn incr_fail(&self) {
        self.dial_fail.fetch_add(1, Ordering::Relaxed);
    }
    fn incr_flow(&self) {
        self.flows.fetch_add(1, Ordering::Relaxed);
    }
    fn decr_flow(&self) {
        self.flows.fetch_sub(1, Ordering::Relaxed);
    }
    pub fn incr_reject(&self) {
        self.rejected.fetch_add(1, Ordering::Relaxed);
    }
    pub fn rejects(&self) -> u64 {
        self.rejected.load(Ordering::Relaxed)
    }
    /// UDP 会话归宿上报（关闭时；只 transit 会话）。
    pub fn incr_udp_session(&self, replied: bool) {
        if replied {
            self.udp_replied.fetch_add(1, Ordering::Relaxed);
        } else {
            self.udp_no_reply.fetch_add(1, Ordering::Relaxed);
        }
    }
    pub fn snapshot(&self) -> [(&'static str, u64); 6] {
        [
            ("dialok", self.dial_ok.load(Ordering::Relaxed)),
            ("dialfail", self.dial_fail.load(Ordering::Relaxed)),
            ("flows", self.flows.load(Ordering::Relaxed)),
            ("rejected", self.rejected.load(Ordering::Relaxed)),
            ("udpReplied", self.udp_replied.load(Ordering::Relaxed)),
            ("udpNoReply", self.udp_no_reply.load(Ordering::Relaxed)),
        ]
    }
}

/// 拦截层配置（装配层注入）。
pub struct Config {
    pub tunnel_ip: Ipv4Addr,
    /// 豁免端口 → Unix socket 路径（LocalServices；UDP 不查——Go 同口径）。
    pub local_services: HashMap<u16, String>,
    /// :53 进程内代答腿 + 隧道栈内 DNS 面（None = 关闭代答，:53 按原目标过境重拨）。
    pub dns: Option<Arc<DnsProxy>>,
    /// DNS worker 的应答回投通道（与 dns 同生共死）。
    pub dns_events: Option<std::sync::mpsc::Receiver<DnsReply>>,
    /// 客户端远程解析腿端口（隧道 IP:<它> TCP；0 = 不建该面）。
    pub dns_resolve_port: u16,
    /// 出口发送整形（R8-3 8i）：None = 关（消融臂/单测直通面），Some = 字节令牌桶
    /// 参数。产品装配面由 `tx_shape_default()`（env 消融臂）填充。
    pub tx_shape: Option<TxShape>,
    /// 拦截栈内层 MTU（P2）：默认 1280；opt-in 1380（域 = stackb::clamp_inner_mtu，
    /// 开放档 {1280,1380}）。**静态**——`Interface::new` 构造期快照消费，运行中
    /// 不变（运行时降档 = P3 候选，见 docs/reviews/P2.md §3.2）。上行段尺寸由手机
    /// 侧 SYN 通告 MSS 封顶（两端不一致自洽）。
    pub inner_mtu: usize,
    pub logf: Logf,
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Transit,
    Exempt,
    Dns,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Transit => "transit",
            Kind::Exempt => "exempt",
            Kind::Dns => "dns",
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Proto {
    Tcp,
    Udp,
}

enum Phase {
    /// TCP：upstream 拨号中（SYN 缓存）；UDP：建会话窗口（pending 队列）。
    Dialing {
        cache: Vec<Vec<u8>>,
    },
    Established,
}

/// R8-4 8n 第二瓶颈归因插桩：单 TCP 流的收发观测（**驱动线程独占，无锁**）。
/// TX 累计点 = on_tx 反重写命中；RX 累计点 = on_plain 流命中（客户端 ACK）。
/// cc_stats_line 5s 窗差分消费报出。判别目标：
/// - 对端通告窗原始 u16（min~max/last）——检测 rwnd 门限（CUBIC cwnd 上限 =
///   max(64×536, 对端窗)，smoltcp 只升不降）与窗口更新稀疏度；
/// - ACK 段数/确认推进字节——ACK 时钟密度（每 2 段一个 ACK + 10ms delayed 兜底）；
/// - dup ACK——手机侧乱序/丢包代理观测；
/// - 发送段最大载荷（≈实际协商 MSS）；
/// - 在途 ≈ Σ发送 - Σ确认（含重传量，配 dup 解读）。
#[derive(Default, Clone, Copy)]
struct TcpObs {
    tx_seg: u64,
    tx_data_seg: u64,
    tx_bytes: u64,
    tx_max_seg: usize,
    ack_seg: u64,
    ack_bytes: u64,
    ack_dup: u64,
    win_min: Option<u32>,
    win_max: u32,
    win_last: u32,
    ack_last: Option<u32>,
    inflight_est: i64,
}

impl TcpObs {
    /// 客户端→出口 ACK 段累计（on_plain 点；v.proto 已判 6）。返回本段的**确认推进
    /// 字节**（8r：ACK 时钟估计的累计源；dup/回退段返回 0）。单段推进截在 1MiB
    ///（真 ACK 的推进 ≤ 对端通告窗——快路径 ~1MB 量级；超出的是五元组复用/回绕的
    /// 假推进〔真机 8r 消融实测抓到一次性 +378MB 的 wrap 假跳变把 est 顶到 3.6G〕
    /// ——截断防 est 污染，pace 误升方向的退化虽安全〔更松〕仍要防）。
    fn note_rx_ack(&mut self, v: &Ipv4View) -> u64 {
        self.ack_seg += 1;
        let ack = v.tcp_ack;
        let mut adv = 0u64;
        match self.ack_last {
            Some(prev) => {
                let d = ack.wrapping_sub(prev);
                if d == 0 && v.payload.is_empty() {
                    self.ack_dup += 1;
                } else if d < 0x8000_0000 {
                    // 单调推进（回绕安全半窗内）才计确认字节；1MiB 截断见函数头
                    adv = (d as u64).min(1024 * 1024);
                    self.ack_bytes += adv;
                    self.inflight_est -= adv as i64;
                }
            }
            None => {
                // 首 ACK 基线：不含 SYN 计数，从第二次起算推进
            }
        }
        self.ack_last = Some(ack);
        let w = v.tcp_win as u32;
        self.win_min = Some(self.win_min.map_or(w, |m| m.min(w)));
        self.win_max = self.win_max.max(w);
        self.win_last = w;
        adv
    }

    /// 出口→客户端段累计（on_tx 点；载荷长度按反重写前的包体）。
    fn note_tx_seg(&mut self, v: &Ipv4View) {
        self.tx_seg += 1;
        let n = v.payload.len();
        if n > 0 {
            self.tx_data_seg += 1;
            self.tx_bytes += n as u64;
            self.tx_max_seg = self.tx_max_seg.max(n);
            self.inflight_est += n as i64;
        }
    }
}

struct Flow {
    kind: Kind,
    proto: Proto,
    /// 客户端侧端点（栈内 socket 的对端 / 反重写时的目的）。
    client: (Ipv4Addr, u16),
    /// 原始目的（豁免/过境判定依据；TX 反重写的源地址）。
    orig_dst: (Ipv4Addr, u16),
    rw_port: u16,
    sock: Option<SocketHandle>,
    phase: Phase,
    last_active: Instant,
    /// 栈→upstream 在途字节（Written 清账；水位门控 drain）。
    unacked_out: usize,
    /// upstream→栈内 socket 写不下的余量（**部分写回补**——send_slice 只写前缀时
    /// 余量必须留住：静默丢字节 = 下游流错位【2026-10-02 实测抓出：speedtest 下行
    /// 大流量下帧错位】；poll 开窗后在 service_sockets 续写）。
    tx_backlog: Vec<u8>,
    /// upstream EOF 后待补的 FIN（**backlog 排空后才 close**：close 会把 FIN 排进
    /// socket 发送队列——backlog 里的数据若在 FIN 之后才写就永远出不去，客户端看到
    /// 「数据 + FIN + 丢尾」的流错位【2026-10-02 实测抓出：speedtest report 帧丢失】）。
    fin_pending: bool,
    /// transit UDP：是否收到过回包（udpcap 实测位）。
    udp_replied: bool,
    /// UDP 会话号（E12 关闭行用——建立时分配、关闭时回放，评审 M4）。
    udp_seq_of: u64,
    /// TCP：建流 SYN 的 seq（拨号失败构造 RST|ACK 的 ack 依据）。
    syn_seq: u32,
    /// TCP DNS 腿的 RFC1035 分帧积攒（2B 长度前缀 + 报文；跨读保留不完整帧）。
    dns_rx: Vec<u8>,
    /// TCP 归因观测（8n；UDP 流恒零值闲置）。
    obs: TcpObs,
}

/// cc_stats_line 观测行的上一窗快照（差分用；字段 = TcpObs 的累计子集）。
#[derive(Clone, Copy)]
struct TcpObsSnap {
    tx_seg: u64,
    tx_bytes: u64,
    ack_seg: u64,
    ack_bytes: u64,
    ack_dup: u64,
}

impl From<&TcpObs> for TcpObsSnap {
    fn from(o: &TcpObs) -> Self {
        Self {
            tx_seg: o.tx_seg,
            tx_bytes: o.tx_bytes,
            ack_seg: o.ack_seg,
            ack_bytes: o.ack_bytes,
            ack_dup: o.ack_dup,
        }
    }
}

/// 拦截层本体（**驱动线程独占**——RX/TX/流表/栈全在一条线程）。
pub struct Interceptor {
    cfg: Config,
    stats: Arc<Stats>,
    iface: Interface,
    sockets: SocketSet<'static>,
    device: TunDevice,
    flows: HashMap<u64, Flow>,
    by_five: HashMap<(Ipv4Addr, u16, Ipv4Addr, u16, u8), u64>,
    by_rw_port: HashMap<u16, u64>,
    next_flow: u64,
    next_rw_port: u16,
    pool: WorkerPool,
    events: std::sync::mpsc::Receiver<PoolEvent>,
    halted: bool,
    /// 栈内真 listener 的端口集（demux 优先面；3d 的 DNS listener 登记）。
    served_ports: std::collections::HashSet<u16>,
    /// 出站明文包队列（TX 反重写后待 encap——pump 返回给引擎）。
    tx_out: Vec<Vec<u8>>,
    // ---- 发送整形（R8-3 8i；驱动线程独占——与 tx_out 同生命周期） ----
    /// 滞留队列（令牌不够时的未释放出站包，FIFO 保序；TCP 面深度 ≤ Σcwnd
    /// 自钳制——UDP/DNS/ICMP 等无窗记账面的口径见 §九注记，评审 r2-3.1）。
    tx_deferred: std::collections::VecDeque<Vec<u8>>,
    /// 滞留字节数（维护量——评审 r2-自补5：峰值统计不再每拍 O(n) 扫全队列）。
    tx_deferred_bytes: usize,
    /// 当前令牌余量（B）。
    tx_credit: f64,
    /// 上次续水时刻。
    tx_last_refill: Instant,
    /// pacing 时刻表：下一包的最早放行时刻（8r；仅 pace 开时参与释放判定）。
    tx_pace_next: Instant,
    // ---- ACK 时钟吞吐估计（8r 自适应 pacing 的 est；驱动线程独占） ----
    /// 本估计窗累计的 ACK 确认字节。
    ack_clk_bytes: u64,
    /// 估计窗起点（每 ACK_EST_WIN 一拍）。
    ack_clk_last: Instant,
    /// est（B/s）：窗内 ACK 确认速率，max 攻击 / ACK_EST_DECAY 每窗慢衰减 /
    /// 下界 TX_PACE_EST0——稳态 ≈ cwnd/RTT（PaceMode::Adaptive 注释）。
    ack_rate_est: f64,
    /// 本观察窗整形拍数（cc 5s 行消费清零——「每次唤醒放行包数」的判读面，
    /// 评审 r1-1.1：pace 间隔低于循环固定成本时有效放行率由循环容量界定）。
    tx_win_pumps: u64,
    /// 本观察窗释放包数（cc_stats_line 5s 消费清零——窗语义）。
    tx_win_released: u64,
    /// 本观察窗滞留深度峰值 (包, B)（同窗消费清零）。
    tx_win_defer_peak: (usize, usize),
    /// DNS 的隧道栈内监听面（:53 UDP/TCP + 解析腿 TCP；dns 开才建）。
    dns_faces: Option<DnsFaces>,
    /// DNS worker 应答回投通道（pump 拍内 drain）。
    dns_rx: Option<std::sync::mpsc::Receiver<DnsReply>>,
    /// UDP 会话号（判据行 #N——进程级递增，对齐旧 udp relay 口径）。
    udp_seq: u64,
    /// 拨号失败日志的降噪表（形态 → (累计次数, 是否已记过首行)；R6.6 P2）。
    /// 键 = (kind, 原始目的)——**不含源端点**：手机核自连探测每次换临时源端口，
    /// 键含源则每条都成「首行」，降噪失效（评审 r1 低危整改）。
    dial_fail_seen: HashMap<DialFailKey, (u64, bool)>,
    /// CC 观测行的上次打印时刻。
    last_cc_stats: Option<Instant>,
    /// 观测行上一窗快照（8n；流 id → 累计快照——差分本窗增量）。
    obs_snaps: HashMap<u64, TcpObsSnap>,
    time0: Instant,
    smol_now: SmolInstant,
}

impl Interceptor {
    /// 装配：拦截栈（地址 = 隧道 IP）+ worker 池。E5 判据行在此打出。
    pub fn attach(mut cfg: Config, stats: Arc<Stats>) -> Self {
        let inner_mtu = crate::wgcore::stackb::clamp_inner_mtu(cfg.inner_mtu);
        let mut device = TunDevice::with_mtu(inner_mtu);
        let mut iface = Interface::new(
            IfaceConfig::new(HardwareAddress::Ip),
            &mut device,
            SmolInstant::from_millis(0),
        );
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(cfg.tunnel_ip.into(), 32))
                .expect("唯一地址必入表");
        });
        // 默认路由（TX 包目的 = 客户端地址；Medium::Ip 不做邻居解析，网关仅是锚点）
        iface
            .routes_mut()
            .add_default_ipv4_route(Ipv4Address::new(100, 64, 255, 254))
            .expect("路由表默认空");
        let (pool, events) = WorkerPool::spawn(8);
        (cfg.logf)(&format!(
            "intercept: 过境拦截就绪（隧道IP {}；豁免=转投本机同端口；TCP 并发上限 {}）",
            cfg.tunnel_ip, MAX_CONNS
        ));
        // 整形状态**独立行**（评审 r2-1.4：E5 是与 Go 基线逐串对齐的判据行——
        // Rust 侧扩展后缀会让 INTEROP-CRITERIA 的串匹配面失效）。
        match cfg.tx_shape {
            Some(s) => {
                let pace_note = match s.pace {
                    Some(PaceMode::Adaptive) => {
                        " pace=自适应（ACK 时钟估计×增益，亚毫秒拍）".to_owned()
                    }
                    Some(PaceMode::Fixed(p)) => format!(
                        " pace={}MiB/s（固定，逐包时刻表+亚毫秒拍）",
                        p / (1024 * 1024)
                    ),
                    None => String::new(),
                };
                (cfg.logf)(&format!(
                    "intercept: 发送整形=开（rate={}MiB/s burst={}KiB{pace_note}；单拍倾泻钳突发内、余量下拍续传）",
                    s.rate / (1024 * 1024),
                    s.burst / 1024
                ))
            }
            None => (cfg.logf)(
                "intercept: 发送整形=关（HOMEWAY_TX_SHAPING 消融臂或单测直通面）",
            ),
        }
        // 内层 MTU 独立行（P2；同 r2-1.4 口径：E5 原串不扩展）——升档形态才打，
        // 默认 1280 保持日志面零变化。
        if inner_mtu != crate::wgcore::stackb::MTU {
            (cfg.logf)(&format!(
                "intercept: 内层MTU={inner_mtu}（P2 opt-in；MSS={}，需手机侧同档才有下行收益）",
                inner_mtu - 40
            ));
        }
        let dns_rx = cfg.dns_events.take();
        let tx_credit0 = cfg.tx_shape.map(|s| s.burst as f64).unwrap_or(0.0);
        Self {
            cfg,
            stats,
            iface,
            sockets: SocketSet::new(vec![]),
            device,
            flows: HashMap::new(),
            by_five: HashMap::new(),
            by_rw_port: HashMap::new(),
            next_flow: 1,
            next_rw_port: 20000,
            pool,
            events,
            halted: false,
            served_ports: std::collections::HashSet::new(),
            tx_out: Vec::new(),
            tx_deferred: std::collections::VecDeque::new(),
            tx_deferred_bytes: 0,
            tx_credit: tx_credit0,
            tx_last_refill: Instant::now(),
            tx_pace_next: Instant::now(),
            ack_clk_bytes: 0,
            ack_clk_last: Instant::now(),
            ack_rate_est: TX_PACE_EST0,
            tx_win_pumps: 0,
            tx_win_released: 0,
            tx_win_defer_peak: (0, 0),
            dns_faces: None,
            dns_rx,
            udp_seq: 0,
            dial_fail_seen: HashMap::new(),
            last_cc_stats: None,
            obs_snaps: HashMap::new(),
            time0: Instant::now(),
            smol_now: SmolInstant::from_millis(0),
        }
    }

    /// DNS 面装配（attach 后单独调——监听 socket 要落在本拦截栈的 SocketSet 里）。
    /// 对应 Go `listenTunnelDNS`（FIX-60：监听面在隧道栈内，不占 host 端口）。
    pub fn attach_dns(&mut self) {
        if self.cfg.dns.is_none() {
            return;
        }
        let mut served = std::mem::take(&mut self.served_ports);
        self.dns_faces = Some(DnsFaces::attach(
            self.cfg.tunnel_ip,
            self.cfg.dns_resolve_port,
            &mut self.sockets,
            &mut served,
        ));
        self.served_ports = served;
    }

    fn now_smol(&mut self) -> SmolInstant {
        self.smol_now = SmolInstant::from_millis(self.time0.elapsed().as_millis() as i64);
        self.smol_now
    }

    /// RX：WG decap 出的明文包（源校验已过）。
    pub fn on_plain(&mut self, pkt: Vec<u8>) {
        let Some(v) = Ipv4View::parse(&pkt) else {
            return; // 畸形：静默丢（IP 层）
        };
        let l4_off = if v.proto == 6 {
            20
        } else if v.proto == 17 {
            8
        } else {
            0
        };
        let payload_start = v.header_len + l4_off;
        let snapshot = View5 {
            src: v.src,
            src_port: v.src_port,
            dst: v.dst,
            dst_port: v.dst_port,
            proto: v.proto,
            tcp_flags: v.tcp_flags,
            tcp_seq: v.tcp_seq,
            tcp_ack: v.tcp_ack,
            udp_payload: (payload_start, v.total_len),
        };
        if v.dst == self.cfg.tunnel_ip && self.served_ports.contains(&v.dst_port) {
            // demux 先投栈内**真 listener**（DNS :53/:5300——3d 建并登记）；
            // 未登记的隧道 IP 端口走 NAT 豁免路径（files/term/speedtest 经拦截层转投——
            // Go 的 SetTransportProtocolHandler 也在 demux 未命中后才接手，同序）
            self.device.rx_push(&pkt);
            return;
        }
        let proto = if v.proto == 6 { Proto::Tcp } else { Proto::Udp };
        let five = (v.src, v.src_port, v.dst, v.dst_port, v.proto);
        if let Some(&flow) = self.by_five.get(&five) {
            let Some(f) = self.flows.get_mut(&flow) else {
                return;
            };
            let rw = f.rw_port;
            let is_dialing = matches!(f.phase, Phase::Dialing { .. });
            if is_dialing {
                // 建会话窗口：包进缓存（重放用）——上限丢最新（TCP ≤4 / UDP ≤16）。
                // UDP 缓存**纯载荷**（重放直投 upstream）；TCP 缓存整包（就绪后重写注栈）。
                let cap = if proto == Proto::Tcp {
                    SYN_CACHE_MAX
                } else {
                    UDP_PENDING_MAX
                };
                let item = if proto == Proto::Udp {
                    let (a, b) = snapshot.udp_payload;
                    pkt[a.min(pkt.len())..b.min(pkt.len())].to_vec()
                } else {
                    pkt
                };
                if let Phase::Dialing { cache } = &mut f.phase {
                    if cache.len() < cap {
                        cache.push(item);
                    }
                }
                return;
            }
            // 8n 归因插桩：客户端→出口 ACK 段累计（含通告窗原始 u16）——在 pkt move
            // 前观测（v 借用 pkt）；推进字节进 ACK 时钟估计（8r）。
            if proto == Proto::Tcp {
                let adv = self
                    .flows
                    .get_mut(&flow)
                    .map(|f| f.obs.note_rx_ack(&v))
                    .unwrap_or(0);
                self.ack_clk_bytes += adv;
            }
            let mut p = pkt;
            nat::rewrite_dst(&mut p, self.cfg.tunnel_ip, rw);
            self.device.rx_push(&p);
            if let Some(f) = self.flows.get_mut(&flow) {
                f.last_active = Instant::now();
            }
            return;
        }
        match proto {
            Proto::Tcp => self.tcp_new(&snapshot, pkt),
            Proto::Udp => self.udp_new(&snapshot, pkt),
        }
    }

    /// TCP 新流：拨号先行（设计 §4.1——SYN 缓存不注栈）。
    fn tcp_new(&mut self, v: &View5, pkt: Vec<u8>) {
        if !v.is_tcp_syn() {
            // 未知四元组的非 SYN：回 RST（gVisor HandleUnknownDestinationPacket 同义）
            let rst = build_rst_for(v);
            self.tx_out.push(rst);
            return;
        }
        if self.halted {
            self.tx_out.push(build_rst_for(v));
            return;
        }
        let tcp_flows = self
            .flows
            .values()
            .filter(|f| f.proto == Proto::Tcp)
            .count();
        if tcp_flows >= MAX_CONNS {
            self.stats.incr_reject();
            let rejects = self.stats.rejects();
            (self.cfg.logf)(&format!(
                "intercept: tcp 拒绝 {}:{} ← {}:{}（并发上限 {}，在册 {}，累计拒绝 {}）",
                v.dst,
                v.dst_port,
                v.src,
                v.src_port,
                MAX_CONNS,
                tcp_flows + 1,
                rejects
            ));
            self.tx_out.push(build_rst_for(v));
            return;
        }
        let (kind, upstream) = self.route_upstream(v.dst, v.dst_port, Proto::Tcp);
        let flow = self.alloc_flow(v, kind, Proto::Tcp, vec![pkt]);
        let _ = upstream;
        if kind == Kind::Dns {
            // M3：TCP DNS 腿不走 worker 拨号——直接建栈内 listen + 注入缓存
            //（SYN-ACK 即刻可产）；数据面走进程内代答（dns_tcp_feed）。
            let legs = self
                .flows
                .values()
                .filter(|f| f.kind == Kind::Dns && f.proto == Proto::Tcp)
                .count();
            if legs >= MAX_TCP_DNS_LEGS {
                (self.cfg.logf)(&format!(
                    "intercept: tcp dns {}:{} ← {}:{} 拒绝（并发上限 {}）",
                    v.dst, v.dst_port, v.src, v.src_port, MAX_TCP_DNS_LEGS
                ));
                self.tx_out.push(build_rst_for(v));
                self.remove_flow(flow);
                return;
            }
            self.dns_tcp_establish(flow);
            return;
        }
        self.start_dial(flow, v);
    }

    /// TCP DNS 腿建立（Go serveDNSTCP 同义：CreateEndpoint → 「进程内代答」判据行 →
    /// ServeStream；此处 = 建 listen + 注入缓存，后续每拍 service_sockets 喂数据）。
    fn dns_tcp_establish(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let rw = f.rw_port;
        let mut sock = TcpSocket::new(
            tcp::SocketBuffer::new(vec![0u8; FLOW_BUF]),
            tcp::SocketBuffer::new(vec![0u8; FLOW_TX_BUF]),
        );
        sock.set_nagle_enabled(false);
        sock.set_congestion_control(self.cc_algo()); // R8-8a CUBIC（R8-2 起 HOMEWAY_CC 可消融）
        sock.set_timeout(Some(smoltcp::time::Duration::from_secs(
            TCP_DNS_IDLE.as_secs(),
        )));
        if let Err(e) = sock.listen(IpEndpoint::new(self.cfg.tunnel_ip.into(), rw)) {
            (self.cfg.logf)(&format!("intercept: tcp listen rw_port {rw} 失败：{e:?}"));
            self.teardown_flow(flow, false);
            return;
        }
        let h = self.sockets.add(sock);
        let f = self.flows.get_mut(&flow).expect("刚判存在");
        let Phase::Dialing { cache } = std::mem::replace(&mut f.phase, Phase::Established) else {
            unreachable!("alloc_flow 后必为 Dialing");
        };
        f.sock = Some(h);
        for mut p in cache {
            nat::rewrite_dst(&mut p, self.cfg.tunnel_ip, rw);
            self.device.rx_push(&p);
        }
        let (orig_dst, client) = (f.orig_dst, f.client);
        self.stats.incr_flow();
        (self.cfg.logf)(&format!(
            "intercept: tcp dns {}:{} ← {}:{}（进程内代答）",
            orig_dst.0, orig_dst.1, client.0, client.1
        ));
    }

    /// TCP DNS 腿数据面：RFC1035 分帧积攒 → 完整报文投 DNS worker（qtcp 计数面；
    /// 应答经 DnsRoute::TcpFlow 回投）。超长帧（>64KB+2B 缓冲界）按对端异常收线。
    fn dns_tcp_feed(&mut self, flow: u64, data: &[u8]) {
        let Some(f) = self.flows.get_mut(&flow) else {
            return;
        };
        f.dns_rx.extend_from_slice(data);
        f.last_active = Instant::now();
        // 循环取完整帧（一条读可能含多条报文）
        loop {
            let (mlen, query) = {
                let Some(f) = self.flows.get(&flow) else {
                    return;
                };
                if f.dns_rx.len() < 2 {
                    return;
                }
                let mlen = u16::from_be_bytes([f.dns_rx[0], f.dns_rx[1]]) as usize;
                if f.dns_rx.len() < 2 + mlen {
                    return;
                }
                (mlen, f.dns_rx[2..2 + mlen].to_vec())
            };
            if let Some(f) = self.flows.get_mut(&flow) {
                f.dns_rx.drain(..2 + mlen);
            }
            let dns = self.cfg.dns.clone().expect("kind=Dns 必有腿");
            let tag = self
                .dns_faces
                .as_mut()
                .map(|faces| faces.route_tag(DnsRoute::TcpFlow(flow)))
                .unwrap_or(0);
            if tag != 0 {
                dns.submit_tcp(tag, query);
            }
        }
    }

    /// 豁免/过境/DNS 的 upstream 决策（Go serveTCP 的 target/LocalServices 同口径）。
    fn route_upstream(&self, dst: Ipv4Addr, port: u16, proto: Proto) -> (Kind, Upstream) {
        if self.cfg.dns.is_some() && port == 53 {
            // DNS 腿（FIX-60）：UDP 与 **TCP**（M3，R5 补）都不落地真实网络——
            // 进程内代答，应答源地址 = 原目的（如 8.8.8.8:53）。
            return match proto {
                Proto::Udp => (Kind::Dns, Upstream::Udp(loopback(port))),
                Proto::Tcp => (Kind::Dns, Upstream::Tcp(loopback(port))), // 占位：腿不拨号
            };
        }
        if dst == self.cfg.tunnel_ip {
            // 豁免：LocalServices 命中 → UDS（UDP 不查表——Go 同口径）；未命中 → 回环同端口
            if proto == Proto::Tcp {
                if let Some(sock) = self.cfg.local_services.get(&port) {
                    return (Kind::Exempt, Upstream::Unix(sock.clone()));
                }
            }
            let target = loopback(port);
            return if proto == Proto::Tcp {
                (Kind::Exempt, Upstream::Tcp(target))
            } else {
                (Kind::Exempt, Upstream::Udp(target))
            };
        }
        let target = SocketAddrV4::new(dst, port).into();
        if proto == Proto::Tcp {
            (Kind::Transit, Upstream::Tcp(target))
        } else {
            (Kind::Transit, Upstream::Udp(target))
        }
    }

    fn alloc_flow(&mut self, v: &View5, kind: Kind, proto: Proto, cache: Vec<Vec<u8>>) -> u64 {
        let flow = self.next_flow;
        self.next_flow += 1;
        let rw = self.alloc_rw_port();
        self.by_rw_port.insert(rw, flow);
        self.by_five
            .insert((v.src, v.src_port, v.dst, v.dst_port, v.proto), flow);
        self.flows.insert(
            flow,
            Flow {
                kind,
                proto,
                client: (v.src, v.src_port),
                orig_dst: (v.dst, v.dst_port),
                rw_port: rw,
                sock: None,
                phase: Phase::Dialing { cache },
                last_active: Instant::now(),
                unacked_out: 0,
                tx_backlog: Vec::new(),
                fin_pending: false,
                udp_seq_of: 0,
                udp_replied: false,
                syn_seq: v.tcp_seq,
                dns_rx: Vec::new(),
                obs: TcpObs::default(),
            },
        );
        flow
    }

    /// rw_port 分配（避开在用与低端口；环形递增）。
    fn alloc_rw_port(&mut self) -> u16 {
        loop {
            let p = self.next_rw_port;
            self.next_rw_port = if self.next_rw_port >= 61000 {
                20000
            } else {
                self.next_rw_port + 1
            };
            if !self.by_rw_port.contains_key(&p) {
                return p;
            }
        }
    }

    fn start_dial(&mut self, flow: u64, v: &View5) {
        let (_, upstream) = self.route_upstream(v.dst, v.dst_port, Proto::Tcp);
        let worker = (flow as usize) % self.pool.workers();
        // UDP 的 dns 腿不经 pool（无 socket）——udp_new 已分流；此处仅 TCP 形态
        self.pool.spawn_dial(flow, upstream, worker);
    }

    /// UDP 新会话（首包）：置在建位 + pending；DNS 腿直接就绪，其余投 worker 拨号。
    fn udp_new(&mut self, v: &View5, pkt: Vec<u8>) {
        if self.halted {
            if let Some(icmp) = nat::build_icmp_unreachable(&pkt) {
                self.tx_out.push(icmp);
            }
            return;
        }
        let udp_flows = self
            .flows
            .values()
            .filter(|f| f.proto == Proto::Udp)
            .count();
        if udp_flows >= MAX_UDP_SESSIONS {
            self.stats.incr_reject();
            (self.cfg.logf)(&format!(
                "intercept: udp 会话上限 {} 已满，丢 {}:{} ← {}:{}",
                MAX_UDP_SESSIONS, v.dst, v.dst_port, v.src, v.src_port
            ));
            if let Some(icmp) = nat::build_icmp_unreachable(&pkt) {
                self.tx_out.push(icmp);
            }
            return;
        }
        let (kind, upstream) = self.route_upstream(v.dst, v.dst_port, Proto::Udp);
        // 建会话窗口：栈内 udp socket 即刻建（后续包经重写命中）；pending 缓存首包**纯载荷**
        let (pa, pb) = v.udp_payload;
        let first = pkt[pa.min(pkt.len())..pb.min(pkt.len())].to_vec();
        let flow = self.alloc_flow(v, kind, Proto::Udp, vec![first]);
        if kind == Kind::Dns {
            // 进程内腿：无拨号——直接「就绪」+ 重放
            self.udp_ready(flow);
            return;
        }
        let worker = (flow as usize) % self.pool.workers();
        self.pool.spawn_dial(flow, upstream, worker);
    }

    /// UDP 会话就绪（DialOk 或 DNS 腿）：重放 first+pending（upstream 直投，不注栈）。
    fn udp_ready(&mut self, flow: u64) {
        let (replays, kind, orig_dst, client) = {
            let Some(f) = self.flows.get_mut(&flow) else {
                return;
            };
            let Phase::Dialing { cache } = &f.phase else {
                return;
            };
            let snap = (cache.clone(), f.kind, f.orig_dst, f.client);
            f.phase = Phase::Established;
            snap
        };
        // 栈内 udp socket 就位（DNS 进程内腿不经 worker——on_dial_ok 之外也要建；
        // ensure 幂等，DialOk 路径重复调用无害）
        self.ensure_udp_socket(flow);
        // 会话号 + 判据行（E12 建立）
        self.udp_seq += 1;
        let seq = self.udp_seq;
        self.stats.incr_flow();
        (self.cfg.logf)(&format!(
            "udp intercept: 会话 #{seq} {} 建立（{}:{} ← {}:{}）",
            kind.as_str(),
            orig_dst.0,
            orig_dst.1,
            client.0,
            client.1
        ));
        if kind == Kind::Dns {
            // DNS 腿：逐包投进程内代答（**异步**——H3 整改：阻塞面全长 2.5s/查询，
            // 不得占驱动线程；应答经回投通道在 pump 拍内路由回该 flow）。
            // Go `Answer()` 直调口径：不计 q 计数（q 只在隧道栈 UDP listener 面计）。
            let dns = self.cfg.dns.clone().expect("kind=Dns 必有腿");
            for q in replays {
                let tag = self
                    .dns_faces
                    .as_mut()
                    .map(|f| f.route_tag(DnsRoute::UdpFlow(flow)))
                    .unwrap_or(0);
                if tag != 0 {
                    dns.submit_leg(tag, q);
                }
            }
            return;
        }
        for p in replays {
            self.pool.send_for(flow, PoolCmd::Out { flow, data: p });
        }
    }

    /// TCP DNS 腿应答回投：RFC1035 帧化（2B BE 长度 + 报文）进 tx_backlog——
    /// service_sockets 的 backlog 续写会把它排进栈内 socket（与 worker 上行同路径，
    /// 背压/部分写语义一致）。
    fn dns_tcp_send(&mut self, flow: u64, resp: &[u8]) {
        let Some(f) = self.flows.get_mut(&flow) else {
            return;
        };
        let mut frame = Vec::with_capacity(2 + resp.len());
        frame.extend_from_slice(&(resp.len() as u16).to_be_bytes());
        frame.extend_from_slice(resp);
        f.tx_backlog.extend_from_slice(&frame);
        f.last_active = Instant::now();
    }

    /// 把一段数据经栈内 udp socket 回投客户端（DNS 应答/UpstreamData 共用）。
    fn udp_send_to_client(&mut self, flow: u64, data: &[u8]) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let Some(h) = f.sock else { return };
        let ep = IpEndpoint::new(f.client.0.into(), f.client.1);
        let sock = self.sockets.get_mut::<UdpSocket>(h);
        let _ = sock.send_slice(data, ep);
        if let Some(f) = self.flows.get_mut(&flow) {
            f.last_active = Instant::now();
        }
    }

    /// 驱动拍：事件处理 → DNS 应答回投 → 栈 poll → TX 反重写 → idle/水位 → DNS 面
    /// 服务 → 返回出站明文包（引擎 encap）。
    pub fn pump(&mut self) -> Vec<Vec<u8>> {
        // ① worker 事件
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev);
        }
        // ①' DNS 应答回投（worker 池异步产出；H3——驱动线程只做路由写回）
        self.drain_dns();
        self.ack_clk_tick();
        self.cc_stats_line();
        // ② 栈 poll
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        // ③ TX：反重写
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        // ④ 栈内 socket 数据面（读→Out / 关闭推进）+ DNS 面服务 + idle 看门狗
        self.service_sockets();
        self.service_dns();
        self.reap_idle();
        // ⑤ 再 poll 一轮（③④ 产生的状态变化让 ACK/数据尽早在本拍出站）
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        // ⑥ 发送整形（R8-3 8i）：本拍产物并入滞留 FIFO，按字节令牌桶从头释放——
        // 余量下拍续传（驱动线程每拍必调本函数）。None = 关臂直通（行为与
        // 整形前逐字节等价）。
        self.tx_shape_release(false)
    }

    /// 高水位背压拍（P1 两级前置背压；引擎面在发送 ring 高水位时以本函数替代
    /// pump）：与 pump 同拍序，唯一差别 = ⑥ 的整形释放退化为「只并入不释放」
    ///——本拍产物并入滞留 FIFO 后**不扣 credit、不推时刻表**（整形状态原样，
    /// 下拍 ring 水位回落后照常释放——无双重记账）。包留 FIFO = 真背压不丢包，
    /// 发送 ring 的满丢成为最后兜底。整形关臂（无 FIFO）保持直通——该臂满丢
    /// 即唯一兜底（消融态接受）。
    pub fn pump_hold(&mut self) -> Vec<Vec<u8>> {
        // ① worker 事件
        while let Ok(ev) = self.events.try_recv() {
            self.on_event(ev);
        }
        self.drain_dns();
        self.ack_clk_tick();
        self.cc_stats_line();
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        self.service_sockets();
        self.service_dns();
        self.reap_idle();
        let now = self.now_smol();
        self.iface.poll(now, &mut self.device, &mut self.sockets);
        let mut raw = Vec::new();
        self.device.drain_tx(&mut raw);
        for pkt in raw {
            self.on_tx(pkt);
        }
        // ⑥' 只并入不释放（tx_shape_release 的并路径同款记账；整形关 = 直通）
        match self.cfg.tx_shape {
            None => std::mem::take(&mut self.tx_out),
            Some(_) => {
                let produce = std::mem::take(&mut self.tx_out);
                if produce.is_empty() {
                    return Vec::new();
                }
                let n = produce.len();
                let b = produce.iter().map(|p| p.len()).sum::<usize>();
                self.tx_win_defer_peak = (
                    self.tx_win_defer_peak.0.max(self.tx_deferred.len() + n),
                    self.tx_win_defer_peak.1.max(self.tx_deferred_bytes + b),
                );
                self.tx_deferred.extend(produce);
                self.tx_deferred_bytes += b;
                Vec::new()
            }
        }
    }

    /// ACK 时钟估计的一拍（pump 头部调用；ACK_EST_WIN 粗节流——非窗口期零成本）。
    /// 窗粗于团块（100ms ≫ 团内 ACK 尖峰的毫秒尺度）⇒ est 反映持续速率而非团内
    /// 尖峰；max 攻击/慢衰减 ⇒ 无「估计跌 → pace 收紧 → 到达变慢 → 估计再跌」的
    /// 下探振荡面（增益 1.2 使平衡点永不被节流，双保险）。
    fn ack_clk_tick(&mut self) {
        if self.cfg.tx_shape.is_none() {
            self.ack_clk_bytes = 0; // 整形关时无消费者——不跨配置生命周期累计（r2-1.2低）
            return;
        }
        let now = Instant::now();
        let dt = now.duration_since(self.ack_clk_last);
        if dt < ACK_EST_WIN {
            return;
        }
        let inst = self.ack_clk_bytes as f64 / dt.as_secs_f64();
        self.ack_clk_bytes = 0;
        self.ack_clk_last = now;
        self.ack_rate_est = inst.max(self.ack_rate_est * ACK_EST_DECAY).max(TX_PACE_EST0);
    }

    /// 有效整形参数（Adaptive → 具体速率的解析点；驱动线程独占）：
    /// - Adaptive：pace = clamp(GAIN×est, FLOOR, CEIL)；R 抬到 ≥ TX_RATE_GAIN×est
    ///   （快路径不设人为上限——反过拟合约束 1）；
    /// - Fixed：pace 用配置值，R 不抬（诊断臂语义纯净）；
    /// - None：原样。
    fn tx_shape_eff(&self) -> Option<ShapeResolved> {
        let s = self.cfg.tx_shape?;
        let mut rate = s.rate;
        let pace = match s.pace {
            None => None,
            Some(PaceMode::Fixed(p)) => Some(p.min(rate)),
            Some(PaceMode::Adaptive) => {
                let pace = (TX_PACE_GAIN * self.ack_rate_est)
                    .clamp(TX_PACE_FLOOR as f64, TX_PACE_CEIL as f64) as u64;
                rate = rate.max((TX_RATE_GAIN * self.ack_rate_est) as u64);
                Some(pace.min(rate))
            }
        };
        Some(ShapeResolved { rate, burst: s.burst, pace })
    }

    /// 整形释放（`flush_all` = 收工宽限形态——评审 r2-1.1：宽限路径的原始语义是
    /// 「尽快把尾数据/FIN 送出去」（M2 判据），整形在这里没有收益还会丢尾包——
    /// 原实现每拍只拿令牌放得下的部分，宽限循环在流清空/到点即 break，滞留余量
    /// 随对象 drop = 静默丢尾数据）。返回本拍上线包集。
    fn tx_shape_release(&mut self, flush_all: bool) -> Vec<Vec<u8>> {
        match self.tx_shape_eff() {
            None => std::mem::take(&mut self.tx_out),
            Some(shape) => {
                let now = Instant::now();
                let dt = now.duration_since(self.tx_last_refill).as_secs_f64();
                self.tx_last_refill = now;
                let produce = std::mem::take(&mut self.tx_out);
                let n_defer = self.tx_deferred.len() + produce.len();
                let b_defer = self.tx_deferred_bytes
                    + produce.iter().map(|p| p.len()).sum::<usize>();
                self.tx_win_defer_peak = (
                    self.tx_win_defer_peak.0.max(n_defer),
                    self.tx_win_defer_peak.1.max(b_defer),
                );
                if flush_all {
                    // 宽限全量释放：deadline 由收工侧兜底，滞留清空（记账面同步）。
                    // FIFO 保序：滞留（更早产出）在前、本拍产物在后。
                    let mut out = Vec::with_capacity(self.tx_deferred.len() + produce.len());
                    for p in self.tx_deferred.drain(..) {
                        out.push(p);
                    }
                    out.extend(produce);
                    self.tx_deferred_bytes = 0;
                    self.tx_win_released += out.len() as u64;
                    out
                } else {
                    self.tx_win_pumps += 1;
                    let (out, run) = shape_slice(
                        &mut self.tx_deferred,
                        produce,
                        ShapeRun {
                            credit: self.tx_credit,
                            pace_next: self.tx_pace_next,
                            deferred_bytes: self.tx_deferred_bytes,
                        },
                        shape,
                        dt,
                        now,
                        2 * self.device.mtu(),
                    );
                    self.tx_deferred_bytes = run.deferred_bytes;
                    self.tx_credit = run.credit;
                    self.tx_pace_next = run.pace_next;
                    self.tx_win_released += out.len() as u64;
                    out
                }
            }
        }
    }

    /// 栈内 TCP socket 的 CC 算法（R8-2 归因插桩）：`crate::cc_choice()` 的默认
    /// CUBIC + **非默认值一次性记行**（消融轮的判据面——hilog/stdout 里能确证
    /// 本轮跑的是 reno/none 而不是环境变量没生效）。
    fn cc_algo(&self) -> tcp::CongestionControl {
        let cc = crate::cc_choice();
        if cc != tcp::CongestionControl::Cubic {
            static LOGGED: std::sync::atomic::AtomicBool =
                std::sync::atomic::AtomicBool::new(false);
            if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                (self.cfg.logf)(&format!(
                    "intercept: CC 消融臂生效（HOMEWAY_CC={cc:?}——非产品默认 CUBIC）"
                ));
            }
        }
        cc
    }

    /// 发送侧吞吐观测行（verbose/dlogf 面；仅存在在途 TCP 流时打——真机吞吐排障的
    /// 关键窗口：栈内 CUBIC 的在途/未收账面 + backlog）。R8-8a：垫片退役后 cwnd/
    /// 减窗计数不再可观测（栈内私有），改看 send_queue（tx_buffer 存量 = 上线在途
    /// + 待发）与 backlog。
    fn cc_stats_line(&mut self) {
        let now = Instant::now();
        if self
            .last_cc_stats
            .map(|t| now.duration_since(t) < Duration::from_secs(5))
            .unwrap_or(false)
        {
            return;
        }
        self.last_cc_stats = Some(now);
        // 整形窗口计数器先取先清（评审 r2-5.3：此前清零在 busiest 分支内——
        // 无活跃 TCP 的阶段峰值跨窗累计，首个 TCP 窗口会报出跨分钟的「本窗」）。
        let shape_cur = if self.cfg.tx_shape.is_some() {
            let cur = self.tx_deferred.len();
            let peak = self.tx_win_defer_peak;
            let rel = self.tx_win_released;
            let pumps = self.tx_win_pumps;
            let est = self.ack_rate_est;
            let pace = self.tx_shape_eff().and_then(|s| s.pace).unwrap_or(0);
            self.tx_win_defer_peak = (0, 0);
            self.tx_win_released = 0;
            self.tx_win_pumps = 0;
            Some((cur, peak, rel, pumps, est, pace))
        } else {
            None
        };
        let mut active = 0usize;
        let mut busiest: Option<(usize, usize)> = None; // (send_queue 存量, backlog)
        for f in self.flows.values() {
            if f.proto != Proto::Tcp || f.sock.is_none() {
                continue;
            }
            active += 1;
            let Some(h) = f.sock else { continue };
            let sq = self.sockets.get_mut::<TcpSocket>(h).send_queue();
            let cur = (sq, f.tx_backlog.len());
            if busiest.as_ref().map(|b| cur.0 > b.0).unwrap_or(true) {
                busiest = Some(cur);
            }
        }
        // 整形观测独立行（评审 r2-4.4：cc 行只在有活跃 TCP 流时打——纯 UDP transit
        // 场景 pacing 同样生效，est/pace 读数不能跟着 TCP 走；且 Fixed 档的 est 无
        // 驱动关系，混打易误读）。
        if let Some((cur, peak, rel, pumps, est, pace)) = shape_cur {
            // 「每次唤醒放行包数」= 有效放行率的判读面（评审 r1-1.1：pace 间隔低于
            // 驱动循环固定成本时，有效放行率由循环容量界定——均包/拍 贴 1 即该形态）。
            let per_pump = rel as f64 / pumps.max(1) as f64;
            (self.cfg.logf)(&format!(
                "intercept: 整形观测 滞留={cur}包/{}KB 峰值={}包/{}KB 窗释={rel}(均{per_pump:.1}包/拍) est={}MiB/s pace={}MiB/s",
                self.tx_deferred_bytes / 1024,
                peak.0,
                peak.1 / 1024,
                est as u64 / (1024 * 1024),
                pace / (1024 * 1024),
            ));
        }
        if let Some((sq, backlog)) = busiest {
            let shape_note = shape_cur
                .map(|(cur, _peak, _rel, _pumps, _est, _pace)| {
                    format!(
                        " 整形滞留={cur}包/{}KB",
                        self.tx_deferred_bytes / 1024,
                    )
                })
                .unwrap_or_default();
            (self.cfg.logf)(&format!(
                "intercept: cc 活跃TCP={active} 最大流 txq={sq}B backlog={backlog}B（smoltcp 0.14 CUBIC）{shape_note}"
            ));
            // 8n 归因观测行：累计发送字节最大的 TCP 流（bulk 测速场景即最大吞吐流）。
            // 窗差分（快照失配/首窗 = 报累计 + RAW 标记）；通告窗 min~max 为流生命周期
            // 值、在途/maxSeg 为窗末现值。判据面注意：本行带「tcp 观测」前缀，与 dialok
            // 判据行（「tcp transit/exempt …（dialok）」）不同前缀不互扰。
            let mut pick: Option<(u64, TcpObs)> = None;
            for (id, f) in self.flows.iter() {
                if f.proto != Proto::Tcp || f.sock.is_none() {
                    continue;
                }
                let obs = f.obs;
                if pick.as_ref().map(|(_, o)| obs.tx_bytes > o.tx_bytes).unwrap_or(true) {
                    pick = Some((*id, obs));
                }
            }
            if let Some((id, obs)) = pick.filter(|(_, o)| o.tx_seg > 0) {
                let snap = self.obs_snaps.get(&id).copied();
                let (dbytes, dack, dackb, ddup, raw) = match snap {
                    Some(s) if s.tx_seg <= obs.tx_seg => (
                        obs.tx_bytes - s.tx_bytes,
                        obs.ack_seg - s.ack_seg,
                        obs.ack_bytes - s.ack_bytes,
                        obs.ack_dup - s.ack_dup,
                        "",
                    ),
                    _ => (obs.tx_bytes, obs.ack_seg, obs.ack_bytes, obs.ack_dup, " RAW"),
                };
                self.obs_snaps.insert(id, TcpObsSnap::from(&obs));
                let win_min = obs.win_min.unwrap_or(0);
                // RAW = 首窗（快照缺失）——流 id 单调不复用，不存在「失配回退」形态。
                (self.cfg.logf)(&format!(
                    "intercept: tcp 观测 发={}KB(maxSeg={}B) ACK={}({}KB确认) dup={} 通告窗u16[min~max/末]={}~{}/{} 在途≈{}KB{raw}",
                    dbytes / 1024,
                    obs.tx_max_seg,
                    dack,
                    dackb / 1024,
                    ddup,
                    win_min,
                    obs.win_max,
                    obs.win_last,
                    obs.inflight_est.max(0) / 1024,
                ));
            }
        }
    }

    /// DNS 应答路由（回投通道 → 栈内 socket / 拦截腿 flow）。
    fn drain_dns(&mut self) {
        loop {
            let reply = match self.dns_rx.as_ref() {
                Some(rx) => match rx.try_recv() {
                    Ok(r) => r,
                    Err(_) => return,
                },
                None => return,
            };
            let Some(faces) = self.dns_faces.as_mut() else {
                continue;
            };
            let Some(route) = faces.take_route(reply.tag) else {
                continue;
            };
            let Some(resp) = reply.resp else { continue }; // 畸形不回包
            match route {
                DnsRoute::UdpFlow(flow) => self.udp_send_to_client(flow, &resp),
                DnsRoute::TcpFlow(flow) => self.dns_tcp_send(flow, &resp),
                DnsRoute::Udp53(from) => faces.deliver_udp53(&mut self.sockets, from, &resp),
                DnsRoute::Tcp(h) => faces.deliver_tcp(&mut self.sockets, h, &resp),
            }
        }
    }

    /// DNS 面服务拍（读查询 → submit worker；监听池推进）。
    fn service_dns(&mut self) {
        let Some(dns) = self.cfg.dns.clone() else {
            return;
        };
        if let Some(faces) = self.dns_faces.as_mut() {
            faces.service(&dns, &mut self.sockets);
            faces.reap(&mut self.sockets);
        }
    }

    fn on_tx(&mut self, mut pkt: Vec<u8>) {
        let Some(v) = Ipv4View::parse(&pkt) else {
            self.tx_out.push(pkt);
            return;
        };
        // 反重写：src=(隧道IP, rw_port) 命中 → src=(orig_dst)；真 listener 应答不重写
        if v.src == self.cfg.tunnel_ip && self.by_rw_port.contains_key(&v.src_port) {
            if let Some(&flow) = self.by_rw_port.get(&v.src_port) {
                if let Some(f) = self.flows.get_mut(&flow) {
                    // 8n 归因插桩：出口→客户端段累计（载荷长度按包体）。
                    if v.proto == 6 {
                        f.obs.note_tx_seg(&v);
                    }
                    let (ip, port) = f.orig_dst;
                    nat::rewrite_src(&mut pkt, ip, port);
                }
            }
        }
        self.tx_out.push(pkt);
    }

    fn on_event(&mut self, ev: PoolEvent) {
        match ev {
            PoolEvent::DialOk { flow } => self.on_dial_ok(flow),
            PoolEvent::DialFailed { flow } => self.on_dial_failed(flow),
            PoolEvent::UpstreamData { flow, data } => self.on_upstream_data(flow, data),
            PoolEvent::UpstreamEof { flow } => self.on_upstream_eof(flow),
            PoolEvent::Closed { flow } => {
                // worker 侧已收：若栈侧也已亡则清流
                self.maybe_reap(flow);
            }
            PoolEvent::Written { flow, n } => {
                if let Some(f) = self.flows.get_mut(&flow) {
                    f.unacked_out = f.unacked_out.saturating_sub(n);
                }
            }
        }
    }

    fn on_dial_ok(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let Phase::Dialing { .. } = f.phase else {
            return; // 非 Dialing（竞态）：Adopt 已发生，让 Closed 路径清
        };
        match f.proto {
            Proto::Udp => {
                // UDP：DialOk 事件仅用于驱动重放（fd 已 Adopt）——建栈内 socket 在会话建立时
                self.ensure_udp_socket(flow);
                self.udp_ready(flow);
            }
            Proto::Tcp => {
                // TCP：建栈内 listen socket + 注入缓存包（SYN-ACK 由此产生）
                let rw = f.rw_port;
                let mut sock = TcpSocket::new(
                    tcp::SocketBuffer::new(vec![0u8; FLOW_BUF]),
                    tcp::SocketBuffer::new(vec![0u8; FLOW_TX_BUF]),
                );
                sock.set_nagle_enabled(false); // Go SetDelayOption(false) 同口径
                sock.set_congestion_control(self.cc_algo()); // R8-8a CUBIC（R8-2 起 HOMEWAY_CC 可消融——下行 bulk 发送方）
                sock.set_timeout(Some(smoltcp::time::Duration::from_secs(TCP_IDLE.as_secs()))); // R2 低-10：精确 idle 回收
                if let Err(e) = sock.listen(IpEndpoint::new(self.cfg.tunnel_ip.into(), rw)) {
                    (self.cfg.logf)(&format!("intercept: tcp listen rw_port {rw} 失败：{e:?}"));
                    self.teardown_flow(flow, false);
                    return;
                }
                let h = self.sockets.add(sock);
                let f = self.flows.get_mut(&flow).expect("刚判存在");
                let Phase::Dialing { cache } = std::mem::replace(&mut f.phase, Phase::Established)
                else {
                    unreachable!("上面已判 Dialing");
                };
                f.sock = Some(h);
                // 注入缓存（重写后）——此时 SYN-ACK 会在本拍 poll 产出
                for mut p in cache {
                    nat::rewrite_dst(&mut p, self.cfg.tunnel_ip, rw);
                    self.device.rx_push(&p);
                }
                // 判据行（E10 dialok）
                let (kind, orig_dst, client) = (f.kind, f.orig_dst, f.client);
                self.stats.incr_ok();
                self.stats.incr_flow();
                (self.cfg.logf)(&format!(
                    "intercept: tcp {} {}:{} ← {}:{}（dialok）",
                    kind.as_str(),
                    orig_dst.0,
                    orig_dst.1,
                    client.0,
                    client.1
                ));
            }
        }
    }

    /// UDP 会话建立时的栈内 socket（bind rw_port；「connect 语义」用读时校验源）。
    fn ensure_udp_socket(&mut self, flow: u64) {
        let f = self.flows.get(&flow).expect("调用方已判");
        if f.sock.is_some() {
            return;
        }
        let rw = f.rw_port;
        let rx_meta: Vec<udp::PacketMetadata> =
            (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
        let tx_meta: Vec<udp::PacketMetadata> =
            (0..64).map(|_| udp::PacketMetadata::EMPTY).collect();
        let mut sock = UdpSocket::new(
            udp::PacketBuffer::new(rx_meta, vec![0u8; 64 * 1024]),
            udp::PacketBuffer::new(tx_meta, vec![0u8; 64 * 1024]),
        );
        if let Err(e) = sock.bind(IpEndpoint::new(self.cfg.tunnel_ip.into(), rw)) {
            (self.cfg.logf)(&format!("intercept: udp bind rw_port {rw} 失败：{e:?}"));
            return;
        }
        let h = self.sockets.add(sock);
        self.flows.get_mut(&flow).expect("刚判存在").sock = Some(h);
    }

    fn on_dial_failed(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let (kind, orig_dst, client, proto, f_syn_seq) =
            (f.kind, f.orig_dst, f.client, f.proto, f.syn_seq);
        self.stats.incr_fail();
        if proto == Proto::Tcp {
            // RST 回客户端（源 = orig dst——Go r.Complete(true) 同义）。ack = 记录的
            // SYN 的 iss+1（SYN-SENT 侧只认这种形态——smoltcp rst_reply 同构）。
            let syn = nat::build_tcp_syn(client.0, client.1, orig_dst.0, orig_dst.1, f_syn_seq);
            let v = Ipv4View::parse(&syn).expect("构造包恒可解析");
            self.tx_out.push(nat::build_tcp_rst(&v));
            // 日志降噪（R6.5 E2E P2-6）：手机核自连探测拨隧道 IP:1，逐次记行会刷屏
            // （dialfail 计数不受影响——判据面是计数器不是日志行）。同形态首行即记、
            // 之后每 100 次记一行汇总。
            let key = (kind.as_str(), orig_dst);
            let (log, seen) = {
                let e = self.dial_fail_seen.entry(key).or_insert((0u64, false));
                e.0 += 1;
                let log = !e.1 || e.0.is_multiple_of(100);
                e.1 = true;
                (log, e.0)
            };
            if self.dial_fail_seen.len() > 1024 {
                self.dial_fail_seen.clear(); // 排障级记忆，满表清空重记（同 src_seen 口径）
            }
            if log {
                (self.cfg.logf)(&format!(
                    "intercept: tcp {} {}:{} ← {}:{} 拨号失败：连接失败{}",
                    kind.as_str(),
                    orig_dst.0,
                    orig_dst.1,
                    client.0,
                    client.1,
                    if seen > 1 {
                        format!("（该形态累计 {seen} 次，此后每 100 次记一行）")
                    } else {
                        String::new()
                    }
                ));
            }
        } else {
            (self.cfg.logf)(&format!(
                "intercept: udp {} {}:{} ← {}:{} 开 socket 失败：连接失败",
                kind.as_str(),
                orig_dst.0,
                orig_dst.1,
                client.0,
                client.1
            ));
        }
        self.pool.forget_flow(flow); // 拨号失败：无 fd 可关，属主条目收口（H1 同族）
        self.remove_flow(flow);
    }

    fn on_upstream_data(&mut self, flow: u64, data: Vec<u8>) {
        let n = data.len();
        let Some(f) = self.flows.get_mut(&flow) else {
            return;
        };
        match f.proto {
            Proto::Tcp => {
                let has_sock = f.sock.is_some();
                if !has_sock {
                    return;
                }
                // 进栈内 socket（wire 侧节流由栈内 CUBIC 承担）；socket 收不下的滞留
                // backlog（worker 侧水位 backpressure 封顶内存）。
                f.tx_backlog.extend_from_slice(&data);
                let flushed = self.flush_backlog(flow);
                // 背压清账：只认进 socket 的字节（backlog 滞留部分不清账——worker 的
                // unacked 涨过水位即停读 UDS ⇒ 服务端 write_all 阻塞 ⇒ 泵送按墙钟限速；
                // Go gVisor 端点缓冲反压的等价物）。
                if flushed > 0 {
                    self.pool.send_for(flow, PoolCmd::Ack { flow, n: flushed });
                }
            }
            Proto::Udp => {
                if f.kind == Kind::Transit {
                    f.udp_replied = true; // downSeen：真实转发会话的实测位
                }
                self.udp_send_to_client(flow, &data);
            }
        }
        if let Some(f) = self.flows.get_mut(&flow) {
            f.last_active = Instant::now();
        }
        // TCP 的 Ack 已在写 socket 处按「实际进入量」发出；UDP 面（数据报整包）在此清账
        if n > 0 {
            let proto_udp = self
                .flows
                .get(&flow)
                .map(|f| f.proto == Proto::Udp)
                .unwrap_or(false);
            if proto_udp {
                self.pool.send_for(flow, PoolCmd::Ack { flow, n });
            }
        }
    }

    /// backlog 续写（R8-8a：CC 垫片退役后的发送门形态）：把 upstream 数据写进栈内
    /// socket（部分写留余量），**wire 侧出站节流由栈内 CUBIC 承担**（seq_to_transmit
    /// 按 cwnd_remaining 封顶——tx_buffer 里的存量不受限，只有上线的在途受控）。
    /// backlog 清空且挂起 FIN 时补 close。返回本次写进 socket 的字节数（调用方按
    /// 此对 worker 清背压账）。
    fn flush_backlog(&mut self, flow: u64) -> usize {
        let Some(f) = self.flows.get_mut(&flow) else {
            return 0;
        };
        let Some(h) = f.sock else { return 0 };
        if f.tx_backlog.is_empty() {
            return 0;
        }
        let w = self
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(&f.tx_backlog)
            .unwrap_or_default();
        if w > 0 {
            f.tx_backlog.drain(..w);
        }
        if f.tx_backlog.is_empty() && f.fin_pending {
            self.sockets.get_mut::<TcpSocket>(h).close();
        }
        w
    }

    fn on_upstream_eof(&mut self, flow: u64) {
        let Some(f) = self.flows.get_mut(&flow) else {
            return;
        };
        match f.proto {
            Proto::Tcp => {
                // 「任一方 EOF 即双向拆」的 upstream 半边：栈内 socket 发 FIN——
                // **backlog 非空时先挂起**（FIN 排队先于 backlog 会把尾数据挤丢）
                if !f.tx_backlog.is_empty() {
                    f.fin_pending = true;
                } else if let Some(h) = f.sock {
                    self.sockets.get_mut::<TcpSocket>(h).close();
                }
            }
            Proto::Udp => {
                // UDP 无 EOF 概念（读错误同拆）
                self.finish_udp(flow);
            }
        }
    }

    /// UDP 会话收尾（判据行 + 计数 + 清流 + **worker 侧 fd 收口**——评审 H1：idle
    /// 回收是 UDP 会话最常见的收尾路径，不发 Close 会让 upstream fd 与 worker 的
    /// 流属主表永久滞留 ⇒ 数小时内 EMFILE、整机出口逐渐瘫痪）。
    fn finish_udp(&mut self, flow: u64) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let (kind, orig_dst, client, replied, seq) =
            (f.kind, f.orig_dst, f.client, f.udp_replied, f.udp_seq_of);
        self.stats.decr_flow();
        if kind == Kind::Transit {
            self.stats.incr_udp_session(replied);
        }
        // 关闭行打**本会话号**（评审 M4：此前打全局最新 seq，仅单会话场景凑巧对）
        (self.cfg.logf)(&format!(
            "udp intercept: 会话 #{seq} 关闭（{}:{} ← {}:{}）",
            orig_dst.0, orig_dst.1, client.0, client.1
        ));
        self.pool.send_for(
            flow,
            PoolCmd::Close {
                flow,
                linger_rst: false,
            },
        ); // H1：fd 收口（DNS 腿无 owner，静默丢弃安全）
        self.remove_flow(flow);
    }

    /// 栈内 socket 服务：读数据 → Out（水位门控）+ TCP 关闭推进。
    fn service_sockets(&mut self) {
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        for flow in flows {
            let Some(f) = self.flows.get(&flow) else {
                continue;
            };
            let (proto, phase_ready) = (f.proto, matches!(f.phase, Phase::Established));
            if !phase_ready {
                continue;
            }
            let Some(h) = f.sock else { continue };
            let gated = f.unacked_out > WATERMARK;
            match proto {
                Proto::Tcp => {
                    let (can_recv, _may_recv, state, can_send) = {
                        let s = self.sockets.get_mut::<TcpSocket>(h);
                        (s.can_recv(), s.may_recv(), s.state(), s.can_send())
                    };
                    let _ = can_send;
                    // backlog 续写（开窗即写、部分写余量消化、FIN 挂起推进——
                    // 全在 flush_backlog 内；wire 节流归栈内 CUBIC）
                    let flushed = self.flush_backlog(flow);
                    if flushed > 0 {
                        self.pool.send_for(flow, PoolCmd::Ack { flow, n: flushed });
                    }
                    let is_dns_leg = self
                        .flows
                        .get(&flow)
                        .map(|f| f.kind == Kind::Dns)
                        .unwrap_or(false);
                    if can_recv && !gated {
                        // 读尽 → DNS 腿喂进程内代答 / 其余投 worker Out
                        let mut total = 0usize;
                        let mut dns_chunks: Vec<Vec<u8>> = Vec::new();
                        loop {
                            let mut buf = [0u8; 64 * 1024];
                            let n = self
                                .sockets
                                .get_mut::<TcpSocket>(h)
                                .recv_slice(&mut buf)
                                .unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            if is_dns_leg {
                                dns_chunks.push(buf[..n].to_vec());
                            } else {
                                let data = buf[..n].to_vec();
                                self.pool.send_for(flow, PoolCmd::Out { flow, data });
                            }
                            total += n;
                        }
                        for c in dns_chunks {
                            self.dns_tcp_feed(flow, &c);
                        }
                        if total > 0 && !is_dns_leg {
                            if let Some(f) = self.flows.get_mut(&flow) {
                                f.unacked_out += total;
                                f.last_active = Instant::now();
                            }
                        }
                    }
                    // 对端 FIN 且缓冲排空 → 本地 close（FIN 推进；CloseWait 不会自发迁移——
                    // **必须限定 CloseWait 态**：Listen/SynSent 等未连接态 may_recv 恒 false，
                    // 无条件 close 会把刚 listen 的 socket 立刻关掉）
                    if state == tcp::State::CloseWait && !can_recv {
                        let has_pending = self
                            .flows
                            .get(&flow)
                            .map(|f| f.unacked_out > 0)
                            .unwrap_or(false);
                        if !has_pending {
                            self.sockets.get_mut::<TcpSocket>(h).close();
                        }
                    }
                    // 彻底关 + 双向无在途 → 收流
                    if state == tcp::State::Closed && !can_recv && !can_send {
                        self.teardown_flow(flow, false);
                    }
                }
                Proto::Udp => {
                    // 读出（读时校验源 = 客户端——「connect 语义」的替代）→ Out
                    let expect = self
                        .flows
                        .get(&flow)
                        .map(|f| IpEndpoint::new(f.client.0.into(), f.client.1))
                        .unwrap_or_else(|| IpEndpoint::new(Ipv4Addr::UNSPECIFIED.into(), 0));
                    loop {
                        let mut buf = [0u8; 65536];
                        let (n, meta) =
                            match self.sockets.get_mut::<UdpSocket>(h).recv_slice(&mut buf) {
                                Ok(v) => v,
                                Err(_) => break,
                            };
                        if meta.endpoint != expect {
                            continue; // 非客户端来源：丢弃（包已取出，继续读）
                        }
                        let data = buf[..n].to_vec();
                        self.pool.send_for(flow, PoolCmd::Out { flow, data });
                        if let Some(f) = self.flows.get_mut(&flow) {
                            f.last_active = Instant::now();
                        }
                    }
                }
            }
        }
    }

    /// idle 看门狗（TCP 5min / UDP 60s / DNS 10s——共享活跃时间戳）。
    fn reap_idle(&mut self) {
        let now = Instant::now();
        let victims: Vec<u64> = self
            .flows
            .iter()
            .filter(|(_, f)| {
                let idle = match (f.kind, f.proto) {
                    (Kind::Dns, Proto::Tcp) => TCP_DNS_IDLE,
                    (Kind::Dns, _) => DNS_IDLE,
                    (_, Proto::Udp) => UDP_IDLE,
                    (_, Proto::Tcp) => TCP_IDLE,
                };
                now.duration_since(f.last_active) > idle
            })
            .map(|(k, _)| *k)
            .collect();
        for flow in victims {
            let is_udp = self
                .flows
                .get(&flow)
                .map(|f| f.proto == Proto::Udp)
                .unwrap_or(false);
            if is_udp {
                self.finish_udp(flow);
            } else {
                self.teardown_flow(flow, false);
            }
        }
    }

    /// 拆流（TCP 关闭路径）：栈 socket abort/close + worker Close + 判据行。
    fn teardown_flow(&mut self, flow: u64, linger_rst: bool) {
        let Some(f) = self.flows.get(&flow) else {
            return;
        };
        let (kind, orig_dst, client, proto) = (f.kind, f.orig_dst, f.client, f.proto);
        if proto == Proto::Tcp {
            if let Some(h) = f.sock {
                self.sockets.get_mut::<TcpSocket>(h).close();
                let _ = h;
            }
            self.stats.decr_flow();
            (self.cfg.logf)(&format!(
                "intercept: tcp {} {}:{} ← {}:{} 关闭",
                kind.as_str(),
                orig_dst.0,
                orig_dst.1,
                client.0,
                client.1
            ));
        }
        self.pool
            .send_for(flow, PoolCmd::Close { flow, linger_rst });
        self.remove_flow(flow);
    }

    /// 「worker 已 Closed 回执 + 栈侧已亡」的清流（Closed 事件路径）。
    fn maybe_reap(&mut self, flow: u64) {
        // 简化：Closed 到达即允许清（栈侧状态由 teardown/close 路径自理）
        let gone = self
            .flows
            .get(&flow)
            .map(|f| match f.sock {
                None => true,
                Some(h) => self.sockets.get_mut::<TcpSocket>(h).state() == tcp::State::Closed,
            })
            .unwrap_or(true);
        if gone {
            self.remove_flow(flow);
        }
    }

    /// 清流记录（表 + 栈 socket 槽位）。worker 侧 fd 由 Close 命令收。
    fn remove_flow(&mut self, flow: u64) {
        // 评审 2.1：观测快照随流回收（键单调不复用 ⇒ 不清 = 长跑出口每天 ~1.5MB
        // 的无界累积）。
        self.obs_snaps.remove(&flow);
        if let Some(f) = self.flows.remove(&flow) {
            if let Some(h) = f.sock {
                self.sockets.remove(h);
                // smoltcp 0.11 的 remove 即 prune——TIME_WAIT 期保留语义由「Closed 才移除」
                // 的调用纪律承载（service_sockets 的 state==Closed 检查）
            }
            self.by_rw_port.remove(&f.rw_port);
            self.by_five.remove(&(
                f.client.0,
                f.client.1,
                f.orig_dst.0,
                f.orig_dst.1,
                if f.proto == Proto::Tcp { 6 } else { 17 },
            ));
        }
    }

    /// 登记栈内真 listener 端口（demux 优先面；3d 的 DNS listener 建时调）。
    pub fn add_served_port(&mut self, port: u16) {
        self.served_ports.insert(port);
    }

    /// 停收新流（HaltNew——新 TCP 回 RST、新 UDP 回 ICMP；在途不受影响）。
    pub fn halt_new(&mut self) {
        self.halted = true;
    }

    /// 在册流数（驱动收工宽限的销账判据）。
    pub fn flow_count(&self) -> usize {
        self.flows.len()
    }

    /// 兼容面：drain 的旧行为（收工侧自吞出站包——引擎收工已改 pump_grace 走 encap）。
    pub fn drain(&mut self, grace: Duration) -> usize {
        let deadline = Instant::now() + grace;
        loop {
            let _ = self.pump();
            if self.flows.is_empty() {
                return 0;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        let n = flows.len();
        for flow in flows {
            self.teardown_flow(flow, true);
        }
        let out_deadline = Instant::now() + Duration::from_secs(2);
        while !self.flows.is_empty() && Instant::now() < out_deadline {
            let _ = self.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
        n
    }

    /// 收工宽限拍（评审 M2：与 drain 的差别——出站包**带回给引擎走 encap 链**，
    /// 宽限窗口内存量连接的 FIN/ACK/尾数据不丢）。
    /// 返回 (本拍出站包, 是否已到宽限末尾)；到期侧的 teardown 由 close() 承担。
    pub fn pump_grace(&mut self, _deadline: Instant) -> Vec<Vec<u8>> {
        self.halt_new();
        self.pump_with_flush()
    }

    /// 收工宽限拍（评审 r2-1.1 整改）：整流绕开（全量释放）版 pump——宽限的
    /// 原始语义 = 尾数据/FIN 尽快上线（M2），不适用发送整形。
    pub fn pump_with_flush(&mut self) -> Vec<Vec<u8>> {
        // 与 pump 同拍序（事件 → DNS → 双 poll + TX → 服务），仅释放形态不同。
        let out = self.pump();
        // pump 已按令牌释放了本拍产物；这里把滞留余量一并放净。
        if self.cfg.tx_shape.is_some() && !self.tx_deferred.is_empty() {
            let mut out = out;
            out.reserve(self.tx_deferred.len());
            for p in self.tx_deferred.drain(..) {
                out.push(p);
            }
            self.tx_deferred_bytes = 0; // 余量已计入 pump 侧窗释——不双计
            return out;
        }
        out
    }

    /// 诊断面：在册流数（同 flow_count——命名对齐）。
    pub fn flows_alive(&self) -> usize {
        self.flows.len()
    }

    /// 整形滞留是否非空（R8-3 8i：驱动循环 poll 超时自适应面）。真机实测（2026-10-05
    /// B 臂）：bulk 期 ACK 按团到达 ≈190Hz，驱动拍被 5ms poll 钉死 ⇒ 每拍只放得下
    /// 一个突发额度（160KB/5.3ms ≈ 30MB/s）——**突发额度退化成了速率上限**。滞留
    /// 非空时驱动循环应缩短 poll 超时（1ms：续水 64KB/拍 = 64MiB/s 直通面上限，
    /// 且线上团块随之细化到 ~64KB）。
    pub fn tx_pacing_pending(&self) -> bool {
        self.cfg.tx_shape.is_some() && !self.tx_deferred.is_empty()
    }

    /// 驱动循环的整形等待提示（8r）：滞留非空时返回「到下一包可放行」的时长——
    /// pacing on = min(桶门等待, 时间门等待)（亚毫秒粒度，配 pselect 用；credit 恒满
    /// 的排空期 = 时间门等待）；pacing off = 恒 1ms（8n③ 的拍频形态原样——消融
    /// 对照面）；滞留空/整形关 = None（5ms 常规拍）。返回值只是**提示**：等待早归/
    /// 迟到都由释放侧的时刻表补账语义兜住（早归 = poll 有包到即醒照常收包；
    /// 迟到 = 一次放多包）。
    pub fn tx_shape_wait(&self) -> Option<Duration> {
        let eff = self.tx_shape_eff()?;
        let front = self.tx_deferred.front()?;
        if eff.pace.is_none() {
            return Some(Duration::from_millis(1)); // pacing 关（8n③ 拍频形态）
        }
        let now = Instant::now();
        // 桶门等待：头包还差多少 credit、按续水速率折算
        let credit_wait = Duration::from_secs_f64(
            ((front.len() as f64) - self.tx_credit).max(0.0) / eff.rate as f64,
        );
        // 时间门等待：时刻表下一拍（已在过去 = 0——本拍即可放）
        let pace_wait = self.tx_pace_next.saturating_duration_since(now);
        // 两道门**串联**（credit 够且到时刻才放行）⇒ 到下一包可放行 = 两门等待的
        // **max**（评审 r2-2.1：min 会让 credit 恒满时恒返 0 ⇒ 驱动线程被 50µs 量化
        // 下限钉死在 20k 唤醒/s 空转——放行率不受影响〔时间门/补账照常〕，纯 CPU）
        Some(credit_wait.max(pace_wait))
    }

    /// 全停（teardown：在途 TCP 立即拆——收工语义）。
    pub fn close(&mut self) {
        if !self.tx_deferred.is_empty() {
            // 防御面记行（评审 r2-1.1）：close 前宽限循环应已 flush——到这还有
            // 滞留 = 宽限窗口被截断，尾数据丢失要有观测面。
            (self.cfg.logf)(&format!(
                "intercept: close 时仍有整形滞留 {} 包/{}B（宽限窗口未排空——尾数据丢弃）",
                self.tx_deferred.len(),
                self.tx_deferred_bytes
            ));
        }
        self.halt_new();
        let flows: Vec<u64> = self.flows.keys().copied().collect();
        for flow in flows {
            self.teardown_flow(flow, false);
        }
        if let Some(faces) = self.dns_faces.as_mut() {
            faces.close_all(&mut self.sockets);
        }
    }
}

fn loopback(port: u16) -> SocketAddr {
    SocketAddr::from(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port))
}

/// 入站包视图的值形态（on_plain 移交包所有权时的快照——借用/所有权解耦）。
#[derive(Clone, Copy)]
pub struct View5 {
    pub src: Ipv4Addr,
    pub src_port: u16,
    pub dst: Ipv4Addr,
    pub dst_port: u16,
    pub proto: u8,
    pub tcp_flags: u8,
    pub tcp_seq: u32,
    pub tcp_ack: u32,
    /// UDP 载荷在原包中的字节范围（首包/pending 重放取纯载荷——Go udpPayloadOf 同义）。
    pub udp_payload: (usize, usize),
}

impl View5 {
    pub fn is_tcp_syn(&self) -> bool {
        self.proto == 6 && self.tcp_flags & nat::TCP_SYN != 0 && self.tcp_flags & nat::TCP_ACK == 0
    }
}

/// 取一个明文 IPv4 包的目的地址（引擎路由 encap 用；畸形 = None）。
pub fn nat_view_dst(pkt: &[u8]) -> Option<Ipv4Addr> {
    Ipv4View::parse(pkt).map(|v| v.dst)
}

/// 按视图构造 RST（源 = 视图的目的）。
fn build_rst_for(v: &View5) -> Vec<u8> {
    let syn = nat::build_tcp_syn(v.src, v.src_port, v.dst, v.dst_port, v.tcp_seq);
    match Ipv4View::parse(&syn) {
        Some(view) => nat::build_tcp_rst(&view),
        None => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wgcore::stackb::StackB;
    use smoltcp::iface::SocketHandle;
    use smoltcp::socket::tcp::Socket as TcpSocket;
    use smoltcp::time::Instant as SmolInstant;
    use std::io::{Read, Write as _};

    fn noop_logf() -> Logf {
        Arc::new(|_| {})
    }

    /// harness 臂的产品默认整形参数（与 `tx_shape_resolve(None)` 无 env/config 覆盖
    /// 时同值——直接引常量保测试确定性：CI 环境变量不参与）。Adaptive 在
    /// Interceptor 内按墙钟 est 解析——harness 的真实 ACK 流会驱动 est 收敛到
    /// 本臂链路的实际速率（三臂形态判别的机制面，设计 §十二）。
    const PRODUCT_SHAPE: Option<TxShape> = Some(TxShape {
        rate: TX_SHAPE_RATE,
        burst: TX_SHAPE_BURST,
        pace: Some(PaceMode::Adaptive),
    });

    fn cfg_base(tunnel_ip: Ipv4Addr) -> Config {
        Config {
            tunnel_ip,
            local_services: HashMap::new(),
            dns: None,
            dns_events: None,
            dns_resolve_port: 0,
            // 功能单测直通面（整形关闭——pump 在紧循环里跑无墙钟间隔，令牌不续水；
            // 整形行为面在 shape_slice 单测 + 受控 harness 臂）。
            tx_shape: None,
            inner_mtu: crate::wgcore::stackb::MTU,
            logf: noop_logf(),
        }
    }

    /// 交叉泵：客户端栈（StackB）↔ 拦截层的包交换（n 轮；拦截层出站包注回客户端栈）。
    fn cross_pump(client: &mut StackB, itc: &mut Interceptor, rounds: usize, tick: &mut i64) {
        for _ in 0..rounds {
            // 客户端 → 拦截层（时间单调推进——栈定时器依赖）
            *tick += 5;
            let t = SmolInstant::from_millis(*tick);
            client
                .iface
                .poll(t, &mut client.device, &mut client.sockets);
            let mut out = Vec::new();
            client.device.drain_tx(&mut out);
            for p in out {
                itc.on_plain(p);
            }
            // 拦截层 → 客户端
            for p in itc.pump() {
                client.inject(&p);
            }
        }
    }

    /// 豁免流端到端：客户端栈 connect(隧道IP:port) → NAT 豁免 → 回环 echo → 数据往返 +
    /// 拨号先行语义（SYN 不提前应答——SYN-ACK 只在 DialOk 后产出）。
    #[test]
    fn exempt_flow_end_to_end() {
        // 回环 echo（豁免 upstream = 127.0.0.1:同端口）
        let echo = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_thread = std::thread::spawn(move || {
            let (mut c, _) = echo.accept().unwrap();
            let mut buf = [0u8; 4096];
            loop {
                match c.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if c.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));

        // 客户端栈（隧道侧地址 100.64.10.1；默认路由网关随便）
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 1),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, port))
            .unwrap();

        // 少量轮：SYN 已到拦截层，拨号线程可能未完成——SYN-ACK 不应出现在前几轮
        cross_pump(&mut client, &mut itc, 2, &mut 0);

        // 泵到建连（拨号 ≤ 回环即时）
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut established = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            if client.sockets.get::<TcpSocket>(h).state() == tcp::State::Established {
                established = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(
            established,
            "豁免流应建连（state={:?}）",
            client.sockets.get::<TcpSocket>(h).state()
        );
        assert!(stats.snapshot()[0].1 >= 1, "dialok 应计数");

        // 数据往返
        client
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(b"hello-exempt")
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got = Vec::new();
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            let mut buf = [0u8; 4096];
            let n = client
                .sockets
                .get_mut::<TcpSocket>(h)
                .recv_slice(&mut buf)
                .unwrap_or(0);
            if n > 0 {
                got.extend_from_slice(&buf[..n]);
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert_eq!(got, b"hello-exempt", "echo 数据应经豁免流往返");
        drop(echo_thread); // 不 join：echo 在 read 阻塞直到对端断连（测试进程退出即终结）
    }

    /// 拨号失败 → RST（客户端侧 connect 收到 refused）。
    #[test]
    fn dial_failed_gets_rst() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));

        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 2),
            tunnel,
            SmolInstant::from_millis(0),
        );
        // 隧道 IP 上无服务的端口：豁免 upstream = 127.0.0.1:<临时死端口>（绑一个
        // listener 取号再立刻关掉——无人听且不碰特权端口）。**别用 :1**：部分
        // ubuntu CI 沙箱对特权端口的出站策略是 DROP 而非 RST（连接悬死，RST 判据
        // 永远等不到——dd99ae0/7be330c/2f56af2 三轮红 + 同树 rerun 仍红、macos 恒绿、
        // 预算 15s 也不救 ⇒ 非时序面而是投递策略面）；临时端口在回环面上恒
        // ECONNREFUSED，跨平台/跨沙箱稳定。
        let dead_port = {
            let l = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let p = l.local_addr().unwrap().port();
            drop(l);
            p
        };
        let h: SocketHandle = client
            .connect(std::net::SocketAddrV4::new(tunnel, dead_port))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut refused = false;
        let mut was_syn_sent = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut 0);
            let st = client.sockets.get::<TcpSocket>(h).state();
            if st == tcp::State::SynSent {
                was_syn_sent = true;
            }
            // RST 被 smoltcp 接受后 abort：SynSent → Closed 且 endpoint 被清
            if was_syn_sent && st == tcp::State::Closed {
                refused = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(refused, "拨号失败应回 RST（死端口拨号同款语义）");
        assert!(stats.snapshot()[1].1 >= 1, "dialfail 应计数");
    }

    /// DNS 代答端到端：:53 隧道面（UDP demux → 栈内 listener → DNS worker → 回投
    /// 原源端点）+ 进程内腿（非隧道 IP :53 的拦截兜底）。fake 上游代答。
    #[test]
    fn dns_faces_end_to_end() {
        use crate::server::dnsproxy::{DnsConfig, DnsProxy};
        use smoltcp::socket::udp::Socket as CUdp;
        use smoltcp::time::Instant as SI;

        // fake 上游（A 应答固定 127.0.0.1）
        let up = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, from)) = up.recv_from(&mut buf) else {
                    return;
                };
                let mut r = Vec::new();
                r.extend_from_slice(&buf[..2]); // ID 回显
                r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]);
                r.extend_from_slice(&1u16.to_be_bytes()); // AN=1
                r.extend_from_slice(&[0, 0, 0, 0]); // NS/AR
                r.extend_from_slice(&buf[12..n]); // question 回显
                r.extend_from_slice(&[0xC0, 0x0C]);
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&60u32.to_be_bytes());
                r.extend_from_slice(&4u16.to_be_bytes());
                r.extend_from_slice(&[127, 0, 0, 1]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-itcdns-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();

        let (proxy, events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        );
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let cfg = Config {
            tunnel_ip: tunnel,
            local_services: HashMap::new(),
            dns: Some(std::sync::Arc::clone(&proxy)),
            dns_events: Some(events),
            dns_resolve_port: 5300,
            tx_shape: None,
            inner_mtu: crate::wgcore::stackb::MTU,
            logf: noop_logf(),
        };
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        itc.attach_dns();
        assert!(itc.served_ports.contains(&53), "demux 面应登记 :53");
        assert!(itc.served_ports.contains(&5300), "解析腿端口应登记");

        // 一条 A 查询（id=0x3344，example.com）
        let mut q = Vec::new();
        q.extend_from_slice(&0x3344u16.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in ["example", "com"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);

        // ① 隧道 IP:53 UDP 面：手搓 UDP 包 → on_plain → demux 投栈 → worker → 回投
        let pkt = nat::build_udp(Ipv4Addr::new(100, 64, 10, 9), 52000, tunnel, 53, &q);
        itc.on_plain(pkt);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut resp53 = None;
        while Instant::now() < deadline {
            for p in itc.pump() {
                if let Some(v) = Ipv4View::parse(&p) {
                    if v.proto == 17 && v.dst == Ipv4Addr::new(100, 64, 10, 9) && v.src == tunnel {
                        resp53 = Some(v.payload.to_vec());
                    }
                }
            }
            if resp53.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let resp = resp53.expect(":53 面应答到达");
        assert_eq!(&resp[..2], &0x3344u16.to_be_bytes(), "ID 回显");
        assert_eq!(resp[3] & 0x0F, 0, "RCODE=0");
        assert!(
            resp.windows(4).any(|w| w == [127, 0, 0, 1]),
            "A 记录 127.0.0.1 在应答里"
        );
        assert!(
            proxy.stats_line().contains("q=1"),
            "隧道 UDP 面计 q：{}",
            proxy.stats_line()
        );
        assert!(proxy.stats_line().contains("resp=1"));

        // ② 进程内腿：dst=8.8.8.8:53（非隧道 IP 的 :53）→ 拦截 dns 会话 → submit_leg
        let pkt2 = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 9),
            52001,
            Ipv4Addr::new(8, 8, 8, 8),
            53,
            &q,
        );
        itc.on_plain(pkt2);
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut resp_leg = None;
        while Instant::now() < deadline {
            for p in itc.pump() {
                if let Some(v) = Ipv4View::parse(&p) {
                    if v.proto == 17
                        && v.dst == Ipv4Addr::new(100, 64, 10, 9)
                        && v.src == Ipv4Addr::new(8, 8, 8, 8)
                    {
                        resp_leg = Some(v.payload.to_vec());
                    }
                }
            }
            if resp_leg.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let resp = resp_leg.expect("拦截腿应答到达（源反重写为 8.8.8.8）");
        assert_eq!(&resp[..2], &0x3344u16.to_be_bytes());
        // submit_leg 不计 q（Go Answer 口径）；resp 计数 +1
        assert!(
            proxy.stats_line().contains("q=1"),
            "腿不计 q：{}",
            proxy.stats_line()
        );
        assert!(proxy.stats_line().contains("resp=2"));
        let _ = SI::from_millis(0i64);
        let _ = CUdp::new(
            smoltcp::socket::udp::PacketBuffer::new(vec![], vec![]),
            smoltcp::socket::udp::PacketBuffer::new(vec![], vec![]),
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M3：非隧道 IP :53 的 **TCP** 进程内代答腿（Go serveDNSTCP 同义）——客户端栈
    /// connect(8.8.8.8:53) → 无拨号直接建立（判据行「进程内代答」）→ RFC1035 帧
    /// 化查询 → 应答帧化回投（源反重写 8.8.8.8:53）。半帧跨读（分两次 send）钉
    /// 分帧积攒边界。
    #[test]
    fn tcp_dns_leg_end_to_end() {
        use crate::server::dnsproxy::{DnsConfig, DnsProxy};
        use smoltcp::time::Instant as SI;

        // fake 上游（A 应答固定 127.0.0.1）
        let up = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let up_addr = format!("127.0.0.1:{}", up.local_addr().unwrap().port());
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, from)) = up.recv_from(&mut buf) else {
                    return;
                };
                let mut r = Vec::new();
                r.extend_from_slice(&buf[..2]);
                r.extend_from_slice(&[0x80 | 0x01, 0x80, 0x00, 0x01]);
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&[0, 0, 0, 0]);
                r.extend_from_slice(&buf[12..n]);
                r.extend_from_slice(&[0xC0, 0x0C]);
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&1u16.to_be_bytes());
                r.extend_from_slice(&60u32.to_be_bytes());
                r.extend_from_slice(&4u16.to_be_bytes());
                r.extend_from_slice(&[127, 0, 0, 1]);
                let _ = up.send_to(&r, from);
            }
        });
        let dir = std::env::temp_dir().join(format!("homeway-rs-tcpdnsleg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), format!("nameserver {up_addr}\n")).unwrap();

        let (proxy, events) = DnsProxy::spawn(
            DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::new(|_| {}),
            Arc::new(|_| {}),
        );
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let cfg = Config {
            tunnel_ip: tunnel,
            local_services: HashMap::new(),
            dns: Some(std::sync::Arc::clone(&proxy)),
            dns_events: Some(events),
            dns_resolve_port: 5300,
            tx_shape: None,
            inner_mtu: crate::wgcore::stackb::MTU,
            logf: noop_logf(),
        };
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        itc.attach_dns();

        // 客户端栈 connect(8.8.8.8:53)——非隧道 IP 的 :53 TCP
        let mut client = StackB::new(
            Ipv4Addr::new(100, 64, 10, 7),
            tunnel,
            SmolInstant::from_millis(0),
        );
        let dst = std::net::SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53);
        let h: SocketHandle = client.connect(dst).unwrap();
        let mut tick = 0i64;
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut established = false;
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            if client.sockets.get::<TcpSocket>(h).state() == tcp::State::Established {
                established = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(
            established,
            "TCP DNS 腿应无拨号直接建立（state={:?}）",
            client.sockets.get::<TcpSocket>(h).state()
        );

        // 一条 A 查询（id=0x5566，a.example）——RFC1035 帧化，**分两次 send**（半帧跨读）
        let mut q = Vec::new();
        q.extend_from_slice(&0x5566u16.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00, 0x00, 0x01, 0, 0, 0, 0, 0, 0]);
        for label in ["a", "example"] {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&[0x00, 0x01, 0x00, 0x01]);
        let mut frame = Vec::with_capacity(2 + q.len());
        frame.extend_from_slice(&(q.len() as u16).to_be_bytes());
        frame.extend_from_slice(&q);
        let (cut,) = (frame.len() / 2,);
        client
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(&frame[..cut])
            .unwrap();
        let mut tick = 0i64;
        cross_pump(&mut client, &mut itc, 6, &mut tick); // 半帧进积攒缓冲，不应有应答
        client
            .sockets
            .get_mut::<TcpSocket>(h)
            .send_slice(&frame[cut..])
            .unwrap();

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut got: Vec<u8> = Vec::new();
        while Instant::now() < deadline {
            cross_pump(&mut client, &mut itc, 4, &mut tick);
            let mut buf = [0u8; 4096];
            let n = client
                .sockets
                .get_mut::<TcpSocket>(h)
                .recv_slice(&mut buf)
                .unwrap_or(0);
            if n > 0 {
                got.extend_from_slice(&buf[..n]);
                if got.len() >= 2 {
                    let mlen = u16::from_be_bytes([got[0], got[1]]) as usize;
                    if got.len() >= 2 + mlen {
                        break;
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(2));
            // 拒绝忙转：共享 runner 满载时饿死拦截层工作线程（ubuntu CI 偶发 RST/建连超时——2ms 让出）
        }
        assert!(got.len() >= 15, "应答帧应到达（得 {got:?}）");
        let mlen = u16::from_be_bytes([got[0], got[1]]) as usize;
        assert_eq!(got.len(), 2 + mlen, "应答恰一帧");
        let resp = &got[2..];
        assert_eq!(&resp[..2], &0x5566u16.to_be_bytes(), "ID 回显");
        assert_eq!(resp[3] & 0x0F, 0, "RCODE=0");
        assert!(
            resp.windows(4).any(|w| w == [127, 0, 0, 1]),
            "A 记录在应答里"
        );
        // qtcp 单列（Go ServeStream 的 qtcp.Add 口径——M3 腿与隧道内 TCP 面同计数）
        assert!(
            proxy.stats_line().contains("qtcp=1"),
            "TCP 腿计 qtcp：{}",
            proxy.stats_line()
        );
        let _ = SI::from_millis(0i64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// UDP 会话端到端：客户端栈（udp socket 手搓包形式）→ transit → 回环 UDP echo →
    /// 回投反重写。用 nat::build_udp 手搓（不引 StackB 的 udp 面）。
    #[test]
    fn udp_session_end_to_end() {
        // 回环 UDP echo
        let echo = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = echo.local_addr().unwrap().port();
        let echo_thread = std::thread::spawn(move || {
            let mut buf = [0u8; 2048];
            while let Ok((n, from)) = echo.recv_from(&mut buf) {
                if echo.send_to(&buf[..n], from).is_err() {
                    break;
                }
            }
        });

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::clone(&stats));

        // 客户端「假栈」：直接手搓 UDP 包（src=100.64.10.3:50000 → dst=127.0.0.1:port）
        let q = nat::build_udp(
            Ipv4Addr::new(100, 64, 10, 3),
            50000,
            Ipv4Addr::LOCALHOST,
            port,
            b"udp-echo-q",
        );
        itc.on_plain(q.clone());

        let deadline = Instant::now() + Duration::from_secs(5);
        let mut resp = None;
        while Instant::now() < deadline {
            for p in itc.pump() {
                if let Some(v) = Ipv4View::parse(&p) {
                    if v.proto == 17
                        && v.dst == Ipv4Addr::new(100, 64, 10, 3)
                        && v.src == Ipv4Addr::LOCALHOST
                    {
                        resp = Some(v.payload.to_vec());
                    }
                }
            }
            if resp.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            resp.as_deref(),
            Some(&b"udp-echo-q"[..]),
            "UDP 回投应反重写到客户端"
        );
        assert!(stats.snapshot()[2].1 >= 1, "flows gauge 应计会话");
        // fd 收口回归（评审 H1）：close() 的 teardown 必须给 worker 发 Close（此前
        // idle/close 路径不发 Close，upstream fd 与属主表永久滞留 ⇒ EMFILE）。
        // 断言面 = close 后 pump 不 panic 且流表清空（fd 关闭由 worker 的 Closed
        // 回执驱动——内部通道不可直达，行为由 speedtest/文件实测覆盖）。
        itc.close();
        let _ = itc.pump();
        assert!(itc.flow_count() == 0, "close 后流表应清空");
        drop(echo_thread);
    }

    /// R6.6 P1-② 回归验收（忽略：真跑 ~5-10s 且独占机器才稳）。见 `run_shaped_download`。
    /// 阈值口径（评审 r1 整改的两轮收敛：绝对阈值在重载下假红〔天花板实测可掉到
    /// 10MB/s〕，纯天花板相对阈值在闲机假红〔天花板可上到 56MB/s 而整形链路封在
    /// 24MB/s〕）：**分母 = min(链路速率, 同轮实测 passthrough 天花板)** = 可达速率
    /// （链路容量与机器能力取小——同进程同负载自校准）。
    /// 判别力注记：深队列 2MB 下单流本就不丢包（修复前后都 ≈ 天花板）——A 单流是
    /// **无回归**判据；真正判别本 bug 的是 A 并发（评审消融：门关 9.3MB/s≈42% 红、
    /// 门开 15.8MB/s≈83% 绿）与 B 浅队列（门关塌到 MB/s 级）。
    /// R8-8a：CC 垫片退役（smoltcp 0.14 CUBIC 接管）后本 harness 仍是同一条门——
    /// 链路模型的突发额度修正见 `DirLink.burst` 注记（mega-burst 额度会把 smoltcp
    /// 接收端的 ACK 时钟拍扁，属模型失真不是 CC 缺陷；内核/gVisor 接收端无此形态）。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~5-10s，验证时 cargo test -- --ignored 显式跑（重负载下阈值随同轮天花板自校准）"]
    fn downlink_lossy_link_recovery() {
        // A：深队列（部署形态）——单流（无回归）+ 并发 6 流（判别）。
        // 传输量口径（R8-8a）：单流 16→64MB / 并发 6×3→6×8MB——上游 CUBIC 的慢启动
        // 爬坡在本 harness 的接收端 ACK 合并形态下需 ~1.5-2s（smoltcp 接收端每 poll
        // 至多一个 ACK ⇒ 爬坡期 ACK 稀疏），16MB 量级的传输被爬坡期支配（实测 5.5MB/s
        // 而稳态 24.8MB/s=满链路）——量级提到稳态支配（塌陷判别语义不变：0.4MB/s
        // 塌陷形态在 64MB 下 160s 超时必红）。
        const LINK_RATE_MB: f64 = 24.0; // DirLink::deep 的速率参数（改一处同步两处）
        let (secs, bytes, _, _) = run_shaped_download(
            1,
            16 * 1024 * 1024,
            DirLink::passthrough(),
            DirLink::passthrough(),
            None, // 天花板臂：无损透传 + 整形关——量的是机器能力，不掺整形开销
        );
        let ceiling = bytes as f64 / secs / (1024.0 * 1024.0);
        let (secs, bytes, down, tail) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::deep(),
            DirLink::deep(),
            PRODUCT_SHAPE,
        );
        let got = bytes as f64 / secs / (1024.0 * 1024.0);
        let tail = tail.expect("64MB 深队列臂实测 >2s（爬坡即 >1.5s）——尾窗必在");
        println!(
            "A 单流：无损天花板 {ceiling:.1}MB/s → 深队列有损 {got:.1}MB/s 尾2s={tail:.1}MB/s（丢 {} 包 / 峰值队列 {}B）",
            down.dropped, down.peak_queue
        );
        let reach1 = LINK_RATE_MB.min(ceiling);
        assert!(
            got >= reach1 * 0.5,
            "深队列形态下单流吞吐 {got:.1}MB/s 应 ≥ 可达速率 {reach1:.1}MB/s（min(链路 24, 天花板 {ceiling:.1})）的 50%（无回归判据）"
        );
        // R8-2 8g 尾窗速率门（评审 F1 登记义务）：最后 2s 均值 ≥ 窗口均值的 50%——
        // 防「前段突发把均值抬过门、尾段已塌」的假绿（真机口径同款判别 = speedtest
        // 结算行的 尾3s 速率）。取 50% 而非 100%：CUBIC 爬坡期均值偏低、稳态尾窗
        // 通常 ≥ 均值，50% 只拦塌陷形态不拦正常波动。
        assert!(
            tail >= got * 0.5,
            "尾窗（最后 2s）速率 {tail:.1}MB/s 应 ≥ 窗口均值 {got:.1}MB/s 的 50%（尾段塌陷 = 假绿拦截）"
        );

        let (secs, bytes, _, _) = run_shaped_download(
            6,
            3 * 1024 * 1024,
            DirLink::passthrough(),
            DirLink::passthrough(),
            None, // 同上：天花板臂
        );
        let ceiling6 = bytes as f64 / secs / (1024.0 * 1024.0);
        let (secs, bytes, down6, _) = run_shaped_download(
            6,
            8 * 1024 * 1024,
            DirLink::deep(),
            DirLink::deep(),
            PRODUCT_SHAPE,
        );
        let got6 = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "A 并发 6 流：无损天花板 {ceiling6:.1}MB/s → 深队列有损 {got6:.1}MB/s（丢 {} 包 / 峰值队列 {}B）",
            down6.dropped, down6.peak_queue
        );
        // R8-8a 重标定：并发臂的「塌陷判别」职责移交 B 臂（修正模型下 CC=None 在浅队列
        // 仍复现 <0.2MB/s 级塌陷实测；深队列 mega-blast 塌陷形态随突发额度修正不再构成
        // 判别信号）。本臂保留为吞吐回归门，阈值取可达速率的 1/6=4.0MB/s：上游 CUBIC
        // 无 pacing，N 流慢启动过冲 + 2MB 尾丢缓冲的同步振荡（harness 接收端 ACK 合并
        // 放大）实测聚合 5.0-11.5MB/s 波动——4.0 门内留 25% 余量，同时仍远高于 0.4MB/s
        // 塌陷基线（10×）防「CC 被意外关掉」类回归漏检（B 臂另有兜底）。
        let reach6 = LINK_RATE_MB.min(ceiling6);
        assert!(
            got6 >= reach6 / 6.0,
            "深队列形态下并发短流聚合 {got6:.1}MB/s 应 ≥ 可达速率 {reach6:.1}MB/s（min(链路 24, 天花板 {ceiling6:.1})）的 1/6（吞吐回归门；塌陷判别在 B 臂）"
        );

        // B：浅队列（192KB ≪ BDP）——修复前真代码（无门控无 pacing）实测 0.4MB/s
        // （2026-10-04，commit c0a244f 前的 4325c1b 基线 + 同链路形态；部分消融
        // 〔仅去 allowed 上限、保留 pacing〕实测 5.2MB/s，介于两者之间——判据下界
        // 取保守的 3.2MB/s = 0.4×8）。⚠️ 绝对门 0 余量（评审 r1-F18 实复现：负载下
        // 3.2 vs 门 3.2 红；闲机 3.2-3.4、旧垫片 8.1）——R8-1 F5 的「自校准分母」
        // 整改挂 R8-3（再动阈值需对照跑先行，8f 已备数据）；引用本臂数字一律带
        // 负载状态（闲机/负载）。
        let (secs, bytes, downb, _) = run_shaped_download(
            1,
            8 * 1024 * 1024,
            DirLink::shallow(),
            DirLink::shallow(),
            PRODUCT_SHAPE,
        );
        let gotb = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "B 浅队列单流（整形 on）：{gotb:.1}MB/s（丢 {} 包 / 峰值队列 {}B；整形前真代码 3.2MB/s 带状 / 塌陷基线 0.4MB/s）",
            downb.dropped, downb.peak_queue
        );
        // R8-3 F5/F18 重标定（r1-F18 两轮复现负载下 0 余量红——旧绝对门 3.2 与实测
        // 带重合）。**为什么不用 A 臂同款自校准分母**：本臂的稳态带不随天花板走——
        // 实测（8f/R8-3 同带）整形前后都钉在 ~3.2-3.4MB/s（此值由 harness 接收端
        // ACK 时钟形态决定——smoltcp 每 poll 至多一个 ACK 的稀疏时钟 + 浅队列小窗
        // 均衡，与链路 24/天花板 25-66 无关：R8-3 实测 A 臂天花板 25.3/65.8 时本臂
        // 仍 3.4）。天花板派生分母会把门耦合到机器负载而带不动——正是 F18 假红的
        // 根因形态。**取而代之：绝对门 = 健康带与塌陷基线的几何中点** sqrt(3.3×0.4)
        // ≈ 1.2（8f 数据：现行 CUBIC 带 3.2 / 旧垫片 8.1 / 塌陷基线 0.4）——两侧各
        // 留 ≥2.7×/3× 余量：负载把带压半（1.6）仍 1.3× 过门；CC 关/pacing 丢失类
        // 塌陷（0.4）仍 3× 红差。引用本臂数据一律带负载状态（闲机/负载）。
        const B_ARM_GATE_MB: f64 = 1.2;
        assert!(
            gotb >= B_ARM_GATE_MB,
            "浅队列压力形态吞吐 {gotb:.1}MB/s 应 ≥ {B_ARM_GATE_MB}MB/s（健康带 3.2-3.4 与塌陷基线 0.4 的几何中点门——两侧 ≥2.7× 判别余量；塌陷回归）"
        );
    }

    /// P1 两级前置背压：pump_hold 只并入不释放（credit/时刻表原样——下拍照常
    /// 释放，无双重记账）；整形关臂直通。
    #[test]
    fn pump_hold_defers_without_double_accounting() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        itc.cfg.tx_shape = Some(TxShape { rate: 1 << 20, burst: 1024, pace: None });
        for i in 0..4u8 {
            itc.tx_out.push(vec![i; 8]);
        }
        let out = itc.pump_hold();
        assert!(out.is_empty(), "hold 拍应零释放");
        assert_eq!(itc.tx_deferred.len(), 4, "4 包全部滞留 FIFO");
        assert_eq!(itc.tx_deferred_bytes, 32);
        // credit 不被 hold 拍扣减（tx_shape_release 才扣）——下拍全量释放验证：若
        // hold 拍扣过 credit，释放量会短缺。先攒 credit（续水按 dt——测试瞬间
        // dt≈0 ⇒ credit≈0，sleep 20ms 攒出 rate×0.02s = 20KB ≫ 4×8B 额度）。
        std::thread::sleep(Duration::from_millis(20));
        let out2 = itc.pump();
        assert_eq!(out2.len(), 4, "hold 后首拍应全量释放（credit 完整）");
        let seq: Vec<u8> = out2.iter().map(|p| p[0]).collect();
        assert_eq!(seq, (0..4u8).collect::<Vec<_>>(), "FIFO 保序");
        // 整形关臂：hold = 直通（无 FIFO 可滞留）
        itc.cfg.tx_shape = None;
        itc.tx_out.push(vec![9u8; 8]);
        let out3 = itc.pump_hold();
        assert_eq!(out3.len(), 1, "整形关臂 hold 直通");
    }

    /// 宽限全量释放的清空语义（评审 r2-1.1 整改验收）：滞留队列在 pump_with_flush
    /// 后必须清空——「宽限期尾数据/FIN 不丢」（M2）不因整形回归。构造面 =
    /// 直接驱动 tx_shape_release 的 flush_all 分支（同代码路径）+ pump_with_flush
    /// 的滞留排空断言（无流量面，验证的是清空语义本身）。
    #[test]
    fn grace_flush_drains_deferred() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        itc.cfg.tx_shape = Some(TxShape { rate: 1, burst: 16, pace: None }); // 极小额度：一切都会滞留
        // 直接产出一批待发包（模拟拦截栈排空产物——不建真实流）
        for i in 0..8u8 {
            itc.tx_out.push(vec![i; 8]);
        }
        let out = itc.tx_shape_release(false);
        assert!(out.is_empty(), "极小额度下本拍应零释放（全滞留）");
        assert_eq!(itc.tx_deferred.len(), 8, "8 包应全部滞留");
        assert_eq!(itc.tx_deferred_bytes, 64);
        // flush_all：全量释放 + 记账清零
        for i in 8..12u8 {
            itc.tx_out.push(vec![i; 8]);
        }
        let out = itc.tx_shape_release(true);
        assert_eq!(out.len(), 12, "宽限全量释放（滞留 8 + 本拍 4）");
        assert!(itc.tx_deferred.is_empty());
        assert_eq!(itc.tx_deferred_bytes, 0, "字节记账随清空归零");
        let seq: Vec<u8> = out.iter().map(|p| p[0]).collect();
        assert_eq!(seq, (0..12u8).collect::<Vec<_>>(), "FIFO 保序（含滞留部分）");
        // pump_with_flush 幂等（已空 = 空返回）
        let out2 = itc.pump_with_flush();
        assert!(out2.is_empty());
    }

    /// env 值匹配语义（评审 r2-自补1）：`off/0/false` = 关，未设/`on/1/true` = 开。
    /// 注意本测试直接钉 `tx_shape_default` 的匹配逻辑面（env 是进程级——并行
    /// 测试共享环境会互相污染，值面以 match 分支形态为准，不真设 env）。
    #[test]
    fn tx_shaping_env_value_semantics() {
        // 与 tx_shape_default 同款的匹配表（镜像断言——env 本身不在单测里设）
        let parse = |v: Option<&str>| match v {
            Some("off") | Some("0") | Some("false") => false,
            Some(_) | None => true,
        };
        assert!(!parse(Some("off")) && !parse(Some("0")) && !parse(Some("false")));
        assert!(parse(None) && parse(Some("on")) && parse(Some("1")) && parse(Some("true")));
        // 与 `=1` 被当关的旧语义（HOMEWAY_UDP_NO_BATCH 惯例误用面）显式区分
        assert!(parse(Some("1")), "`=1` 必须是开（旧 presence 语义会把消融方向搞反）");
        // HOMEWAY_TX_PACING（8r）双向值匹配：off/0/false = 关（= 8n③ 纯桶形态），
        // on/adaptive/1/true = 开（adaptive）——**默认关**（D-3 止损裁定：8s 在位时
        // pacing 无增益 + 组合形态 2-4MB 常驻滞留；8r 面向无 8s 的接收端路径，
        // config/env 显式开）。镜像断言 match 分支形态（不真设 env）。
        let parse_pacing = |v: Option<&str>, cfg: Option<bool>| match (v, cfg) {
            (Some("off") | Some("0") | Some("false"), _) => false,
            (Some("on") | Some("adaptive") | Some("1") | Some("true"), _) => true,
            (_, Some(true)) => true,  // config pacing="adaptive"
            (_, _) => false,          // 未设未配 = 默认关
        };
        assert!(!parse_pacing(Some("off"), None) && !parse_pacing(None, None));
        assert!(parse_pacing(Some("on"), None) && parse_pacing(Some("1"), None));
        assert!(parse_pacing(None, Some(true)), "config adaptive 显式开");
    }

    /// 令牌桶释放的机制单测（R8-3 8i；纯函数面——时间注入，不依赖墙钟）：
    /// ① 突发额度截断本拍释放深度；② 续水后下拍续传（FIFO 保序）；③ dt=0 时
    /// 零续水（紧循环不放大）；④ 大于 burst 的包防御性直通（防极小 burst 死锁）。
    #[test]
    fn shape_slice_budget_and_continuation() {
        // 纯桶形态（pace=None——8n③ 语义的机制面；参数与 8n③ 实现同构）
        let mut deferred = std::collections::VecDeque::new();
        let now = Instant::now();
        // 5×1000B 倾泻，dt=0（紧拍）：只放 3（额度 3000）
        let (out, run) = shape_slice(
            &mut deferred,
            vec![vec![0u8; 1000]; 5],
            ShapeRun { credit: 3000.0, pace_next: now, deferred_bytes: 0 },
            ShapeResolved { rate: 1000, burst: 3000, pace: None },
            0.0,
            now,
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 3, "突发额度 3000B 截断本拍释放");
        assert_eq!(deferred.len(), 2, "余量滞留");
        assert!(run.credit < 1000.0);
        // dt=1s：续水 1000（cap 3000，余 credit）→ 放 1
        let (out, run) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: run.credit, pace_next: now, deferred_bytes: 2000 },
            ShapeResolved { rate: 1000, burst: 3000, pace: None },
            1.0,
            now,
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 1, "下拍续传一包（续水 1000B）");
        assert_eq!(deferred.len(), 1);
        // 空闲 60s：令牌回满 → 余量全放
        let (out, _) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: run.credit, pace_next: now, deferred_bytes: 1000 },
            ShapeResolved { rate: 1000, burst: 3000, pace: None },
            60.0,
            now,
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 1, "空闲后桶满，滞留清空");
        let _ = &run;
        assert!(deferred.is_empty());
        // 保序：1..=6 依次入，分两拍释放，并起来仍是 1..=6
        let mut deferred = std::collections::VecDeque::new();
        let pkts: Vec<Vec<u8>> = (1..=6u8).map(|i| vec![i; 1000]).collect();
        let (mut out1, run) = shape_slice(
            &mut deferred,
            pkts,
            ShapeRun { credit: 3000.0, pace_next: now, deferred_bytes: 0 },
            ShapeResolved { rate: 1000, burst: 3000, pace: None },
            0.0,
            now,
            2 * crate::wgcore::stackb::MTU,
        );
        let (mut out2, _) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: run.credit, pace_next: now, deferred_bytes: 3000 },
            ShapeResolved { rate: 1000, burst: 3000, pace: None },
            60.0,
            now,
            2 * crate::wgcore::stackb::MTU,
        );
        out1.append(&mut out2);
        let seq: Vec<u8> = out1.iter().map(|p| p[0]).collect();
        assert_eq!(seq, vec![1, 2, 3, 4, 5, 6], "FIFO 释放保序");
        // 极小 burst 配置：大于 burst 的包直通（不死锁）
        let mut deferred = std::collections::VecDeque::new();
        let (out, _) = shape_slice(
            &mut deferred,
            vec![vec![7u8; 500]],
            ShapeRun { credit: 100.0, pace_next: now, deferred_bytes: 0 },
            ShapeResolved { rate: 1000, burst: 100, pace: None },
            0.0,
            now,
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 1, "大于 burst 的包防御性直通");
        assert!(deferred.is_empty());
    }

    /// 8r 逐包时刻表的机制单测（D-3；时间注入面）：① 团首包直通（时刻表在过去）
    /// 其余按 len/pace 间隔门住；② 到点补账（一次放多包、间隔不向 now 钳——迟到
    /// 时长不折进下一包间隔）+ 补账上界 ≤ burst（评审 r1-1.2：pacer 最坏形态 =
    /// 8n③ 单拍形态）；③ 队列排空时时刻表重置到 now（空闲后新团不整团放）；
    /// ④ 与桶门串联（credit 不足时先断在桶门）；⑤ 等价门（评审 r1-2.4）：pace 极大
    /// 时与 pace=None 同输入同输出（稳态直通面逐包等价）。
    #[test]
    fn shape_slice_pacing_schedule() {
        // pace=1000B/s：1000B 包间隔恰 1s——时刻表用整秒可断言
        let t0 = Instant::now();
        let mut deferred = std::collections::VecDeque::new();
        // ① t0 倾泻 4×1000B（时刻表在 t0——首包直通）+ 第 2 包起被时间门门住
        let (out, run) = shape_slice(
            &mut deferred,
            vec![vec![0u8; 1000]; 4],
            ShapeRun { credit: 3000.0, pace_next: t0, deferred_bytes: 0 },
            ShapeResolved { rate: 10_000_000, burst: 3000, pace: Some(1000) },
            0.0,
            t0,
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 1, "团首包直通（时刻表在过去）");
        assert_eq!(deferred.len(), 3);
        assert_eq!(run.pace_next, t0 + Duration::from_secs(1), "时刻前进 len/pace");
        let pace_next = run.pace_next;
        // ② t0+1s 到点：放 1 包（补账只放到期者）；时刻 = t0+2s
        let (out, next_run) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: run.credit, pace_next, deferred_bytes: 3000 },
            ShapeResolved { rate: 10_000_000, burst: 3000, pace: Some(1000) },
            0.0,
            t0 + Duration::from_secs(1),
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 1);
        let pace_next = next_run.pace_next;
        assert_eq!(pace_next, t0 + Duration::from_secs(2));
        // ②' 迟到 2.5s（时刻停在 t0+2s）：补账一次放 2（2s、3s 两拍都已过——间隔
        //     不向 now 钳）；放完队列空 ⇒ 时刻表重置到本次 now（见 ③ 的机制）
        let (out, next_run) = shape_slice(
            &mut deferred,
            vec![],
            ShapeRun { credit: 3000.0, pace_next, deferred_bytes: 2000 },
            ShapeResolved { rate: 10_000_000, burst: 3000, pace: Some(1000) },
            0.0,
            t0 + Duration::from_millis(4500),
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 2, "迟到补账一次放到期包（不向 now 钳）");
        assert!(deferred.is_empty());
        assert_eq!(
            next_run.pace_next,
            t0 + Duration::from_millis(4500),
            "放完排空 ⇒ 时刻表重置到 now"
        );
        let pace_next = next_run.pace_next; // 排空重置后的时刻表（t0+4.5s）
        // ②'' 补账上界 ≤ burst（评审 r1-1.2 不变量）：10 包滞留 + 时刻表全过期 +
        //      credit 满 ⇒ 单次释放 ≤ burst/包长 = 3 包——pacer 最坏形态 = 8n③ 单拍
        let mut deferred = std::collections::VecDeque::new();
        let (out, _) = shape_slice(
            &mut deferred,
            vec![vec![0u8; 1000]; 10],
            ShapeRun {
                credit: 3000.0,
                pace_next: t0 - Duration::from_secs(600),
                deferred_bytes: 0,
            },
            ShapeResolved { rate: 10_000_000, burst: 3000, pace: Some(1000) },
            0.0,
            t0,
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 3, "补账上界 = burst（credit 不变量）——不会比 8n③ 更差");
        assert_eq!(deferred.len(), 7);
        // ③ 空闲后再来新团：首包直通（排空时已重置，不把空闲前的余量带进来）。
        //     注：空闲超过一个 gap 时第 2 拍（重置时刻+gap）也已到期——至多多放
        //     1 包（2 包小团，无累积——slot 只在放行时推进，空闲时长不折算成额度）。
        let mut deferred = std::collections::VecDeque::new();
        let (out, _) = shape_slice(
            &mut deferred,
            vec![vec![0u8; 1000]; 4],
            ShapeRun { credit: 3000.0, pace_next, deferred_bytes: 0 },
            ShapeResolved { rate: 10_000_000, burst: 3000, pace: Some(1000) },
            0.0,
            t0 + Duration::from_millis(4900),
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 1, "空闲后新团首包直通（间隔未满 gap 时第 2 包仍门住）");
        // ④ 桶门串联：credit 只够 1 包时，时间门即便全开也只放 1
        let mut deferred = std::collections::VecDeque::new();
        let (out, _) = shape_slice(
            &mut deferred,
            vec![vec![0u8; 1000]; 3],
            ShapeRun { credit: 1000.0, pace_next: t0, deferred_bytes: 0 },
            ShapeResolved { rate: 10_000_000, burst: 3000, pace: Some(1000) },
            0.0,
            t0,
            2 * crate::wgcore::stackb::MTU,
        );
        assert_eq!(out.len(), 1, "桶门先断（credit 只够 1 包）");
        // ⑤ 等价门（评审 r1-2.4）：pace 极大（间隔 < 1ns）与 None 同输入同输出——
        //    「稳态到达 < pace 时逐字节等价」的机械化面
        let pkts: Vec<Vec<u8>> = (1..=8u8).map(|i| vec![i; 1000]).collect();
        for dt in [0.0, 1.2, 60.0] {
            let mut a = std::collections::VecDeque::new();
            let mut b = std::collections::VecDeque::new();
            let (mut oa, ra) = shape_slice(
                &mut a,
                pkts.clone(),
                ShapeRun { credit: 2500.0, pace_next: t0, deferred_bytes: 0 },
                ShapeResolved { rate: 1000, burst: 3000, pace: Some(u64::MAX) },
                dt,
                t0,
                2 * crate::wgcore::stackb::MTU,
            );
            let (ob, rb) = shape_slice(
                &mut b,
                pkts.clone(),
                ShapeRun { credit: 2500.0, pace_next: t0, deferred_bytes: 0 },
                ShapeResolved { rate: 1000, burst: 3000, pace: None },
                dt,
                t0,
                2 * crate::wgcore::stackb::MTU,
            );
            let sa: Vec<u8> = oa.iter().map(|p| p[0]).collect();
            let sb: Vec<u8> = ob.iter().map(|p| p[0]).collect();
            assert_eq!(sa, sb, "pace 极大与 None 逐包等价（dt={dt}）");
            assert_eq!(a.len(), b.len());
            assert_eq!(ra.credit, rb.credit);
            assert_eq!(ra.pace_next, rb.pace_next, "排空重置语义两形态一致");
            oa.clear();
        }
    }

    /// tx_shape_wait 四态单测（评审 r2-6.2：min/max 缺陷恰好在此——零覆盖是它漏网的
    /// 原因）：① credit 饱和 ⇒ wait == pace_wait（r2-2.1 的回归钉：min 会返 0）；
    /// ② credit 亏空且时刻已到 ⇒ wait == credit_wait；③ 两门都未到 ⇒ max(两门)；
    /// ④ pacing 关 ⇒ 恒 1ms。
    #[test]
    fn tx_shape_wait_serial_gate_semantics() {
        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let mut itc = Interceptor::attach(cfg_base(tunnel), Arc::new(Stats::default()));
        itc.cfg.tx_shape = Some(TxShape {
            rate: 10_000_000,
            burst: 4096,
            pace: Some(PaceMode::Fixed(1000)),
        });
        itc.tx_deferred.push_back(vec![0u8; 1000]);
        // ① credit 饱和（= burst）+ 时刻表在未来 0.5s ⇒ wait = pace_wait（≈0.5s）
        itc.tx_credit = 4096.0;
        itc.tx_pace_next = Instant::now() + Duration::from_millis(500);
        let w = itc.tx_shape_wait().expect("滞留非空");
        assert!(
            w >= Duration::from_millis(400) && w <= Duration::from_millis(600),
            "credit 饱和 ⇒ wait = pace_wait（实测 {w:?}；min 缺陷形态会返 ~0）"
        );
        // ② 时刻已到（过去）+ credit 亏空 900B ⇒ wait = credit_wait = 900/10MB/s = 90µs
        itc.tx_pace_next = Instant::now() - Duration::from_secs(1);
        itc.tx_credit = 100.0;
        let w = itc.tx_shape_wait().expect("滞留非空");
        assert!(w < Duration::from_millis(1), "credit_wait 90µs 应取亚毫秒档（实测 {w:?}）");
        // ③ 两门都在未来 ⇒ max：时刻 0.3s、credit_wait ~99.9µs ⇒ wait ≈ 0.3s
        itc.tx_pace_next = Instant::now() + Duration::from_millis(300);
        let w = itc.tx_shape_wait().expect("滞留非空");
        assert!(w >= Duration::from_millis(200), "两门串联取 max（实测 {w:?}）");
        // ④ pacing 关 ⇒ 恒 1ms
        itc.cfg.tx_shape = Some(TxShape { rate: 10_000_000, burst: 4096, pace: None });
        assert_eq!(itc.tx_shape_wait(), Some(Duration::from_millis(1)));
    }

    /// tx_shape_resolve 的 config 面表格测试（评审 r2-6.3：env>config>默认 的解析面
    /// 零覆盖——config 分支不碰 env，可直测；env 面保留镜像测试并注明局限）。
    #[test]
    fn tx_shape_resolve_config_table() {
        let mk = |pacing: Option<PaceCfg>, pace_mbps: Option<u64>| TxShapeCfg {
            rate_mbps: None,
            burst_kb: None,
            pacing,
            pace_mbps,
        };
        // 默认（无 config）= off
        let r = tx_shape_resolve(None).unwrap();
        assert_eq!(r.pace, None, "默认 pacing off（D-3 止损裁定）");
        // config adaptive
        let r = tx_shape_resolve(Some(mk(Some(PaceCfg::Adaptive), None))).unwrap();
        assert_eq!(r.pace, Some(PaceMode::Adaptive));
        // config fixed + pace_mbps
        let r = tx_shape_resolve(Some(mk(Some(PaceCfg::Fixed), Some(96)))).unwrap();
        assert_eq!(r.pace, Some(PaceMode::Fixed(96 * 1024 * 1024)));
        // config fixed 缺 pace_mbps ⇒ 回落 adaptive（与字段注释一致——r2-2.7）
        let r = tx_shape_resolve(Some(mk(Some(PaceCfg::Fixed), None))).unwrap();
        assert_eq!(r.pace, Some(PaceMode::Adaptive));
        // config off
        let r = tx_shape_resolve(Some(mk(Some(PaceCfg::Off), None))).unwrap();
        assert_eq!(r.pace, None);
        // rate/burst 覆盖
        let r = tx_shape_resolve(Some(TxShapeCfg {
            rate_mbps: Some(64),
            burst_kb: Some(128),
            pacing: None,
            pace_mbps: None,
        }))
        .unwrap();
        assert_eq!(r.rate, 64 * 1024 * 1024);
        assert_eq!(r.burst, 128 * 1024);
        // fixed 超过 rate ⇒ 交叉钳制到 rate
        let r = tx_shape_resolve(Some(TxShapeCfg {
            rate_mbps: Some(32),
            burst_kb: None,
            pacing: Some(PaceCfg::Fixed),
            pace_mbps: Some(512),
        }))
        .unwrap();
        assert_eq!(r.pace, Some(PaceMode::Fixed(32 * 1024 * 1024)), "pace ≤ rate 交叉钳制");
    }

    /// 冷空口形态验证（R8-3 8i；r2-5.1 复核修正后的口径）。**模型判别力（实测
    /// 2026-10-05，串行跑）**：空口速率 80MiB/s（须高于产品整形 R=64MiB/s——把
    /// 「持续过载」与「团块」两种到达丢失解耦）时，on/off 稳定分离 ~2.5×：
    /// on 9.6-9.8MB/s / **0 到达丢失**（团块钳在产品突发 160KiB < 额度 192KiB），
    /// off 3.9MB/s / 28-32 到达丢失（无钳制团块随拍频变粗）。**边界**：闲机上
    /// harness 泵节奏（500µs/拍）自身就是整形器，off 臂团块也 ≤~190KB ⇒ 无分离
    /// （分离幅度随机器负载变化）——off 臂保数据行不设门，**冷悬崖的终裁 = 真机
    /// 8j 矩阵**（已过：B 冷/热 0.85）。判别面：
    /// ① 10ms 到达额度臂（空口容忍充分——0 到达丢失）= 本链路可达速率自校准分母；
    /// ② 冷形态臂（1.2× 产品突发额度）整形 on：吞吐 ≥ 可达速率 50% 且到达丢失
    ///    ≤ 万分之一（团块事件应零——非零即 R>空口速率的混叠回归）；
    /// ③ off 臂数据行（无门——分离幅度负载依赖，见上）。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~15-20s（三臂 64MB @ 冷链路），验证时 cargo test -- --ignored 显式跑"]
    fn cold_air_form_verification() {
        // ① 可达速率臂：空口容忍充分（10ms 额度）——整形 on，量「这条冷链路+本机」
        //    无到达丢失形态下跑得到的速率（BDP≈2MB > FLOW_TX_BUF 1MB ⇒ 实测
        //    受窗口约束 ~10-15MB/s，分母取实测量而非 80——同轮自校准）
        let (secs, bytes, down, _) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::cold_with(10 * 80 * 1024 * 1024 / 1000),
            DirLink::cold_with(10 * 80 * 1024 * 1024 / 1000),
            PRODUCT_SHAPE,
        );
        let reach = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "冷空口 可达臂（10ms 额度，整形 on）：{reach:.1}MB/s（到达口丢 {} / 队列丢 {}）",
            down.air_dropped, down.dropped
        );
        assert_eq!(down.air_dropped, 0, "可达臂不应有到达丢失（额度 ≫ 线上团块）");
        // ② 冷形态臂：1.2× 产品突发额度（192KiB）
        let (secs, bytes, down, tail) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::cold(),
            DirLink::cold(),
            PRODUCT_SHAPE,
        );
        let got = bytes as f64 / secs / (1024.0 * 1024.0);
        let total_pkgs = (bytes / 1300).max(1);
        println!(
            "冷空口 冷形态臂（192KiB 额度，整形 on）：{got:.1}MB/s（到达口丢 {} / 队列丢 {}；可达 {reach:.1}）",
            down.air_dropped, down.dropped
        );
        assert!(
            got >= reach * 0.5,
            "冷形态臂 {got:.1}MB/s 应 ≥ 可达速率 {reach:.1}MB/s 的 50%（整形下团块不触冷预算——无塌陷）"
        );
        assert!(
            down.air_dropped as f64 <= total_pkgs as f64 / 10000.0,
            "到达丢失 {} 包应 ≤ 万分之一（整形把线上团块钳在产品突发 160KiB < 额度 192KiB）",
            down.air_dropped
        );
        // 尾窗门（与 A 臂同款——拦前段达标尾段塌陷）
        if let Some(tail) = tail {
            assert!(
                tail >= got * 0.5,
                "冷形态臂尾窗 {tail:.1}MB/s 应 ≥ 均值 {got:.1}MB/s 的 50%"
            );
        }
        // ③ off 臂：数据行（无门——见头注「模型表达力边界」）
        let (secs, bytes, down, _) = run_shaped_download(
            1,
            64 * 1024 * 1024,
            DirLink::cold(),
            DirLink::cold(),
            None,
        );
        let got_off = bytes as f64 / secs / (1024.0 * 1024.0);
        println!(
            "冷空口 off 臂（192KiB 额度，整形 off——数据行）：{got_off:.1}MB/s（到达口丢 {} / 队列丢 {}）",
            down.air_dropped, down.dropped
        );
    }

    /// F2 burst 敏感性矩阵（评审 §五 F2 登记项——「整形参数即该矩阵实践面」的数据
    /// 面；诊断打印、不设门）：冷空口（80MiB/s）到达额度扫 {1,2,5,10}ms =
    /// {80,160,400,800}KiB，产品整形 on——观察额度贴着/低于产品突发额度（160KiB）
    /// 时的劣化拐点。产出表入册 PERF-AB §9（R8-3 终版）。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~6-10s（4 臂各 16MB 量级），验证时 cargo test -- --ignored 显式跑"]
    fn cold_air_burst_allowance_matrix() {
        for ms in [1u64, 2, 5, 10] {
            let allowance = (80 * 1024 * 1024usize / 1000) * (ms as usize);
            let (secs, bytes, down, _) = run_shaped_download(
                1,
                16 * 1024 * 1024,
                DirLink::cold_with(allowance),
                DirLink::cold_with(allowance),
                PRODUCT_SHAPE,
            );
            let got = bytes as f64 / secs / (1024.0 * 1024.0);
            println!(
                "F2 矩阵：到达额度 {ms}ms（{allowance}B，产品突发 160KiB）→ {got:.1}MB/s（到达口丢 {} / 队列丢 {}）",
                down.air_dropped, down.dropped
            );
        }
    }

    /// 三臂 harness（D-3 反过拟合约束 2：调参不得过拟合当前环境——所有真机测量都在
    /// 家宽+Wi-Fi 形态，慢/快路径以 harness 为准）：慢路径（2.5MB/s + 40ms + 64KB
    /// 队列——2.4GHz/蜂窝形态）与快路径（120MB/s + 2ms + 8MB 队列——千兆有线形态）
    /// 上，**adaptive pacing 不得劣化**：on ≥ 0.7×off（慢臂防滴流劣化、快臂防人为
    /// 限速）。现役深队列臂（24MB/s/26ms/2MB）= downlink_lossy_link_recovery 的
    /// A/B 臂（不在此重复）。0.7 门 = 判「结构性劣化」而非噪声（harness 轮间波动
    /// ~10-20%）；两臂同链路同轮对照，链路差异被除掉。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~25-40s（四臂），验证时 cargo test -- --ignored 显式跑（串行——r2-自补7）"]
    fn pacing_three_path_arms() {
        // (名, 队列cap, 速率, 单向时延, 传输量, 说明)
        let arms: &[(&str, usize, usize, Duration, usize)] = &[
            ("慢路径", 64 * 1024, 2_621_440, Duration::from_millis(40), 8 * 1024 * 1024),
            ("快路径", 8 * 1024 * 1024, 125_829_120, Duration::from_millis(2), 48 * 1024 * 1024),
        ];
        // 每臂 3 轮取中位（评审 r2-6.5：单发采样曾在慢臂出现「队丢不变但吞吐差
        // 60%」的不自洽读数——R8-2「单轮数据不可用于 A/B」教训在 harness 面同样成立）
        for (name, cap, rate, delay, bytes) in arms {
            let mut off_meds = Vec::new();
            let mut on_meds = Vec::new();
            let mut drops_off = 0u64;
            let mut drops_on = 0u64;
            for _ in 0..3 {
                let (secs, got, down, _) = run_shaped_download(
                    1,
                    *bytes,
                    DirLink::new(*cap, *rate, *delay),
                    DirLink::new(*cap, *rate, *delay),
                    None,
                );
                off_meds.push(got as f64 / secs / (1024.0 * 1024.0));
                drops_off += down.dropped;
                let (secs, got, down, _) = run_shaped_download(
                    1,
                    *bytes,
                    DirLink::new(*cap, *rate, *delay),
                    DirLink::new(*cap, *rate, *delay),
                    PRODUCT_SHAPE,
                );
                on_meds.push(got as f64 / secs / (1024.0 * 1024.0));
                drops_on += down.dropped;
            }
            off_meds.sort_by(|a, b| a.partial_cmp(b).unwrap());
            on_meds.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (mbps_off, mbps_on) = (off_meds[1], on_meds[1]);
            println!(
                "三臂 {name}（链路 {}MB/s）：off={mbps_off:.1}MB/s（队丢 {}） on(adaptive)={mbps_on:.1}MB/s（队丢 {}）",
                rate / (1024 * 1024),
                drops_off,
                drops_on
            );
            assert!(
                mbps_on >= mbps_off * 0.7,
                "{name} 臂 pacing on={mbps_on:.1} 不应劣化 off={mbps_off:.1} 的 70%（慢臂滴流/快臂限速 = 过拟合回归）"
            );
        }
    }

    /// 单向链路模型：有限 FIFO 队列（超额即丢）+ 速率出队 + 固定传播时延——
    /// 模拟真机 WiFi（出口下行突发超过队列容量 ⇒ 突发规模丢包，E2E 20× 塌陷的形态）。
    struct DirLink {
        queue: std::collections::VecDeque<Vec<u8>>,
        queued: usize,
        cap: usize,
        rate: usize,
        /// 外层 PMTU 瓶颈注入（P2 坑 4 回归专测）：包长超限即丢（DF 语义——
        /// 模拟「外层路径承载不住」的丢包形态，**不覆盖**无 DF 分片可达那条
        /// 真实兜底；评审 P2-r1-7(c)）。None = 不注入。
        mtu_limit: Option<usize>,
        /// mtu_limit 丢包独立计数（不污染 dropped 的冷/浅臂零丢断言）。
        mtu_dropped: u64,
        /// 观测到的最大包长（P2 段尺寸机器判据：1380 档下载数据段 = 1340+40）。
        seen_max: usize,
        credit: f64,
        /// 令牌桶的**突发额度**（R8-8a 修正）：出队许可以 credit 计，空闲期累积的
        /// credit 以此为上限——与队列容量解耦。此前误把额度上限设成队列容量
        /// （2MB），空闲后一次放出 2MB = 24MB/s 链路 85ms 的量——物理链路没有这种
        /// 突发；该 mega-burst 形态把到达拍打成「每 RTT 一大团」，smoltcp 接收端
        /// 每 poll 至多回一个 ACK（顺序数据）⇒ 每团一 ACK ⇒ 发送端 ACK 时钟饿死
        /// （CUBIC 每 RTT 只 +1 MSS——0.14 迁移实测 3.5MB/s 的根因；内核/gVisor
        /// 接收端无此形态——团内每 2 段回 ACK）。额度 = rate×2ms（≈48KB，比一个
        /// BDP 小一个量级、比驱动拍粒度大一个量级——整形器的物理突发参数）。
        burst: usize,
        last: Instant,
        delay: Duration,
        flying: Vec<(Instant, Vec<u8>)>,
        dropped: u64,
        peak_queue: usize,
        // ---- 冷空口形态（R8-3 8i；§9.5 悬崖机制的模型化） ----
        /// 到达侧冷预算 (突发额度 B, 当前 credit, 上次续水)：Some = 启用。瞬时到达
        /// 超过「额度 + rate×经过时间」的部分在**到达口**丢弃（先于队列判满——deep
        /// 队列也丢；物理面对应 WiFi 电源态未热时空口对团块的成组丢失，与队列容量
        /// 无关）。`air_dropped` 单独计数。
        cold: Option<(usize, f64, Instant)>,
        air_dropped: u64,
    }

    impl DirLink {
        /// 无损透传（量天花板用）。
        fn passthrough() -> Self {
            Self::new(usize::MAX, usize::MAX / 2, Duration::from_millis(1))
        }
        /// WiFi 部署形态：24MB/s 速率 + 2MB 队列（WG socket SO_SNDBUF=4MB 的保守
        /// 折半，见 bind.rs）+ 13ms 单向时延。
        fn deep() -> Self {
            Self::new(2 * 1024 * 1024, 24 * 1024 * 1024, Duration::from_millis(13))
        }
        /// 浅队列压力形态：同速率/时延，队列 192KB（≪ BDP 648KB）。
        fn shallow() -> Self {
            Self::new(192 * 1024, 24 * 1024 * 1024, Duration::from_millis(13))
        }
        /// 冷电源态形态（R8-3 8i；r2-5.1 复核后改 80MiB/s 空口）：**速率必须高于
        /// 产品整形 R=64MiB/s**——否则「持续到达 > 空口续水」的到达丢失与团块
        /// 丢失混在同一计数里，硬门随机器负载随机红（负载拖慢 harness 拍频 ⇒
        /// 释放节奏变粗 ⇒ 持续过载面被放大）。80 > 64 ⇒ 到达口丢的只剩团块事件，
        /// 「整形把团块钳在额度内 ⇒ 0 到达丢失」的断言才是纯的。1MB 队列
        /// （bufferbloat 深度——队列本身不构成瓶颈）+ 到达侧冷预算 192KiB =
        /// 产品突发额度 160KiB 的 1.2×（模型假设：空口容忍数百 µs 级团块、不容忍
        /// ms 级团块——即修复论点；真机裁决 = 8j 矩阵冷连形态）。F2 矩阵臂经
        /// `cold_with` 扫额度。
        fn cold() -> Self {
            Self::cold_with(192 * 1024)
        }
        /// 冷形态 + 自定到达额度（F2 burst 敏感性矩阵 {1,2,5,10}ms×rate 用）。
        fn cold_with(allowance: usize) -> Self {
            let mut l = Self::new(1024 * 1024, 80 * 1024 * 1024, Duration::from_millis(13));
            l.cold = Some((allowance, allowance as f64, Instant::now()));
            l
        }
        fn new(cap: usize, rate: usize, delay: Duration) -> Self {
            Self {
                queue: Default::default(),
                queued: 0,
                cap,
                rate,
                credit: 0.0,
                burst: rate / 500, // rate × 2ms（见字段注记；500 = 1s/2ms）
                last: Instant::now(),
                delay,
                flying: Vec::new(),
                dropped: 0,
                peak_queue: 0,
                cold: None,
                air_dropped: 0,
                mtu_limit: None,
                mtu_dropped: 0,
                seen_max: 0,
            }
        }
        fn send(&mut self, pkt: Vec<u8>) {
            // MTU 瓶颈（到达口最先判——形态学 = 外层一跳就丢）
            if let Some(limit) = self.mtu_limit {
                if pkt.len() > limit {
                    self.mtu_dropped += 1;
                    return;
                }
            }
            self.seen_max = self.seen_max.max(pkt.len());
            // 到达口冷预算：先于队列判满（团块敌意与队列容量无关——见 cold 字段注记）
            if let Some((allow, credit, last)) = &mut self.cold {
                let now = Instant::now();
                let dt = now.duration_since(*last).as_secs_f64();
                *last = now;
                *credit = (*credit + self.rate as f64 * dt).min(*allow as f64);
                if pkt.len() as f64 > *credit {
                    self.air_dropped += 1;
                    return;
                }
                *credit -= pkt.len() as f64;
            }
            if self.queued + pkt.len() > self.cap {
                self.dropped += 1;
                return;
            }
            self.queued += pkt.len();
            self.peak_queue = self.peak_queue.max(self.queued);
            self.queue.push_back(pkt);
        }
        fn advance(&mut self, now: Instant) -> Vec<Vec<u8>> {
            let dt = now.duration_since(self.last).as_secs_f64();
            self.last = now;
            self.credit = (self.credit + self.rate as f64 * dt).min(self.burst as f64);
            while let Some(front) = self.queue.front() {
                if self.credit < front.len() as f64 {
                    break;
                }
                self.credit -= front.len() as f64;
                self.queued -= front.len();
                let pkt = self.queue.pop_front().expect("front 已判");
                self.flying.push((now + self.delay, pkt));
            }
            let mut out = Vec::new();
            let mut i = 0;
            while i < self.flying.len() {
                if self.flying[i].0 <= now {
                    let (_, pkt) = self.flying.remove(i);
                    out.push(pkt);
                } else {
                    i += 1;
                }
            }
            out
        }
    }

    /// 尾窗（最后 2s）平均速率，MB/s（R8-2 8g 尾窗速率门的计算面）。`None` =
    /// 传输不足 2s（首采样点在 t≈0 预置下必在；无 ≤ t-2s 的点 ⇒ 窗口太短）——
    /// 类型承担不变量，设门侧必须显式处理（评审 r1-F3：0.0 哨兵会被当真速率）。
    /// span 构造上 ≥2s（取样点 t ≤ t_end-2）——无除零面。
    fn tail_rate_mbps(timeline: &[(f64, usize)], t_end: f64, total: usize) -> Option<f64> {
        let (_, base) = timeline
            .iter()
            .rev()
            .find(|(t, _)| *t <= t_end - 2.0)
            .copied()?;
        let span = t_end - timeline.iter().rev().find(|(t, _)| *t <= t_end - 2.0)?.0;
        Some((total - base) as f64 / span / (1024.0 * 1024.0))
    }

    /// 一轮受控下载：n_flows 条并发流（各一台栈 B 客户端，独立隧道 IP）经共享的上/下
    /// 行链路拉 bytes_each 字节；豁免腿转投回环 origin。返回（耗时秒, 总字节, 下行统计）。
    fn run_shaped_download(
        n_flows: usize,
        bytes_each: usize,
        up: DirLink,
        down: DirLink,
        tx_shape: Option<TxShape>,
    ) -> (f64, usize, DirLink, Option<f64>) {
        run_shaped_download_mtu(n_flows, bytes_each, up, down, tx_shape, crate::wgcore::stackb::MTU, Duration::from_secs(120))
    }

    /// P2：内层 MTU 臂 + 超时可调（坑 4 注入臂不烧满 120s 死线）。mtu 同时作用于
    /// 客户端栈（模型 = 手机内核按 TUN MTU 推的 MSS）与拦截栈（出口 caps）——
    /// 生产手机核 stack B 恒 1280，本面模型的是 **TUN 应用流量**两端的 MSS 口径。
    fn run_shaped_download_mtu(
        n_flows: usize,
        bytes_each: usize,
        mut up: DirLink,
        mut down: DirLink,
        tx_shape: Option<TxShape>,
        mtu: usize,
        timeout: Duration,
    ) -> (f64, usize, DirLink, Option<f64>) {
        // origin：回环 TCP，每连接写满 bytes_each 后 shutdown 写半边
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let origin = std::thread::spawn(move || {
            use std::io::Write as _;
            let mut conns = Vec::new();
            for _ in 0..n_flows {
                if let Ok((c, _)) = listener.accept() {
                    conns.push(c);
                }
            }
            let mut writers = Vec::new();
            for mut c in conns {
                let n = bytes_each;
                writers.push(std::thread::spawn(move || {
                    let chunk = vec![0x5au8; 128 * 1024];
                    let mut left = n;
                    while left > 0 {
                        let k = chunk.len().min(left);
                        if c.write_all(&chunk[..k]).is_err() {
                            break;
                        }
                        left -= k;
                    }
                    let _ = c.shutdown(std::net::Shutdown::Write);
                    // 排干对端残留（客户端只回 ACK，不占接收缓冲——防 FIN 前 RST）
                    let mut buf = [0u8; 4096];
                    loop {
                        match std::io::Read::read(&mut c, &mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {}
                        }
                    }
                }));
            }
            for w in writers {
                let _ = w.join();
            }
        });

        let tunnel = Ipv4Addr::new(100, 64, 255, 1);
        let stats = Arc::new(Stats::default());
        let mut cfg = cfg_base(tunnel);
        cfg.tx_shape = tx_shape; // 产品臂 = Some(默认参数)；消融臂 = None（harness 自控，不经 env）
        cfg.inner_mtu = mtu; // P2：出口 caps 臂（Interface::new 构造期快照即生效）
        let mut itc = Interceptor::attach(cfg, Arc::clone(&stats));
        let mut clients = Vec::new();
        for i in 0..n_flows {
            let mut s = StackB::with_mtu(
                Ipv4Addr::new(100, 64, 10, 40 + i as u8),
                tunnel,
                SmolInstant::from_millis(0),
                mtu,
            );
            let h = s
                .connect(std::net::SocketAddrV4::new(tunnel, port))
                .unwrap();
            clients.push((s, h));
        }

        let t0 = Instant::now();
        let mut received = vec![0usize; n_flows];
        // timeout 由参数（见函数头注记）
        let mut last_diag = Instant::now() - Duration::from_secs(2);
        let mut last_drop = 0u64;
        // 尾窗速率的采样线（R8-2 8g 尾窗速率门：每 0.5s 记 (秒, 累计字节)——收尾取
        // 最后 2s 的均值，判「窗口收在稳态」而非靠前段突发达标）。
        let mut timeline: Vec<(f64, usize)> = Vec::new();
        loop {
            let now = Instant::now();
            let tel = now.duration_since(t0);
            if tel > timeout {
                break;
            }
            if now.duration_since(last_diag) >= Duration::from_millis(500) {
                last_diag = now;
                timeline.push((tel.as_secs_f64(), received.iter().sum()));
                let f = itc.flows.values().next();
                if let Some(f) = f {
                    let (sq, bl) = f
                        .sock
                        .map(|h| {
                            let s = itc.sockets.get_mut::<TcpSocket>(h);
                            (s.send_queue(), 0usize)
                        })
                        .unwrap_or((0, 0));
                    println!(
                        "t={:>5}ms recv={:>4}MB txq={:>7} backlog={:>7} dropΔ={}/{}",
                        tel.as_millis(),
                        received.iter().sum::<usize>() / (1024 * 1024),
                        sq,
                        f.tx_backlog.len(),
                        down.dropped - last_drop,
                        up.dropped
                    );
                    let _ = bl;
                }
                last_drop = down.dropped;
            }
            let smol_now = SmolInstant::from_millis(tel.as_millis() as i64);
            // ① 客户端栈 poll → 上行链路
            for (s, _) in clients.iter_mut() {
                s.iface.poll(smol_now, &mut s.device, &mut s.sockets);
                let mut out = Vec::new();
                s.device.drain_tx(&mut out);
                for p in out {
                    up.send(p);
                }
            }
            // ② 上行到期 → 拦截层
            for p in up.advance(now) {
                itc.on_plain(p);
            }
            // ③ 拦截层拍 → 下行链路
            for p in itc.pump() {
                down.send(p);
            }
            // ④ 下行到期 → 各客户端（按内层目的地址分投）
            for p in down.advance(now) {
                let dst = Ipv4Addr::new(p[16], p[17], p[18], p[19]);
                if let Some((s, _)) = clients.iter_mut().find(|(s, _)| s.tunnel_ip == dst) {
                    s.inject(&p);
                }
            }
            // ⑤ 客户端读
            let mut all_done = true;
            for (i, (s, h)) in clients.iter_mut().enumerate() {
                let mut buf = [0u8; 64 * 1024];
                loop {
                    let n = s
                        .sockets
                        .get_mut::<TcpSocket>(*h)
                        .recv_slice(&mut buf)
                        .unwrap_or(0);
                    if n == 0 {
                        break;
                    }
                    received[i] += n;
                }
                if received[i] < bytes_each {
                    all_done = false;
                }
            }
            if all_done {
                let _ = origin.join();
                let total = received.iter().sum();
                let tail = tail_rate_mbps(&timeline, tel.as_secs_f64(), total);
                return (tel.as_secs_f64(), total, down, tail);
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        let _ = origin.join();
        let total = received.iter().sum();
        let tail = tail_rate_mbps(&timeline, timeout.as_secs_f64(), total);
        (timeout.as_secs_f64(), total, down, tail)
    }

    // ---------- P2：内层 MTU 面（设计 = docs/reviews/P2.md §3.7） ----------

    /// 段尺寸机器判据（快测、常开）：双端 1380 ⇒ 下载数据段 = MSS 1340 + 头 40 =
    /// IP 包 1380（`seen_max` 恰满段）；双端 1280 ⇒ 1280。这是「包数 −7.5%」
    /// 收益的机制面直接验证（吞吐增益在真机 2×2 与三臂 harness 量）。
    #[test]
    fn mtu_segment_size_machine_check() {
        for mtu in [1280usize, 1380] {
            let (secs, got, down, _) = run_shaped_download_mtu(
                1,
                256 * 1024,
                DirLink::passthrough(),
                DirLink::passthrough(),
                None,
                mtu,
                Duration::from_secs(60),
            );
            assert_eq!(got, 256 * 1024, "传输应完成（mtu={mtu}）");
            assert_eq!(down.seen_max, mtu, "满段尺寸应恰为内层 MTU（mtu={mtu}，得 {}）", down.seen_max);
            assert_eq!(down.mtu_dropped, 0);
            let _ = secs;
        }
    }

    /// 坑 4 回归专测（注入形态学）：外层人为限 1400 ⇒ 可承载内层包长上限 =
    /// 1400−62〔直连 v4 封账〕= 1338。臂 A（双端 1380）：满段 1380 全被丢 ⇒
    /// 停滞（复现「能握手、载荷黑洞」的坑 4 形态——证明注入有效）；臂 B
    /// （门拒回落 1280 = ItcConfig 1280 直通）：全通零丢 = **正确降级路径**。
    /// 「门为何拒」的裁决逻辑在 facade::mtu_gate 单测（注入缝 = ProbeOutcome），
    /// 本面验端到端行为。注记：超限即丢 = DF 语义保守臂，不覆盖 v4 分片可达
    /// 那条真实兜底（评审 P2-r1-7）。
    #[test]
    #[ignore = "性能 harness：坑 4 注入臂烧 20s 死线（停滞臂自然到点），验证时 cargo test -- --ignored 显式跑"]
    fn mtu_blackhole_injection_and_fallback() {
        let limit = 1400 - 62; // 外层 1400 − 直连 v4 封账 62 = 1338
        // 臂 A：1380 满段（1380 > 1338）全丢——坑 4 形态
        let mut down_a = DirLink::passthrough();
        down_a.mtu_limit = Some(limit);
        let (secs, got, down_a, _) = run_shaped_download_mtu(
            1,
            128 * 1024,
            DirLink::passthrough(),
            down_a,
            None,
            1380,
            Duration::from_secs(20),
        );
        assert!(got < 16 * 1024, "坑 4 形态：1380 段被注入链丢弃 ⇒ 停滞（得 {got}B / 20s）");
        assert!(down_a.mtu_dropped > 0, "注入应产生 mtu 丢包计数");
        let _ = secs;
        // 臂 B：门拒回落（= 1280 直通）——正确降级：满段 1280 ≤ 1338 全通
        let mut down_b = DirLink::passthrough();
        down_b.mtu_limit = Some(limit);
        let (_, got, down_b, _) = run_shaped_download_mtu(
            1,
            128 * 1024,
            DirLink::passthrough(),
            down_b,
            None,
            1280,
            Duration::from_secs(30),
        );
        assert_eq!(got, 128 * 1024, "降级路径：1280 段不受注入影响，应全通");
        assert_eq!(down_b.mtu_dropped, 0, "1280 段不触发 mtu 丢弃");
    }

    /// 三臂消融（P2c）：慢/快链路 × 内层 1280 vs 1380。主判据 = 机器面
    /// （段尺寸已在 mtu_segment_size_machine_check 钉死；本面补吞吐不劣化门：
    /// 1380 臂中位 ≥ 0.9×1280 臂中位——消融不设增益门，增益数据登记 PERF-AB）。
    #[test]
    #[ignore = "性能 harness：跑真墙钟 ~40-60s（两臂×三链路×3 轮），验证时 cargo test -- --ignored 显式跑（串行）"]
    fn mtu_ab_three_paths() {
        let arms: &[(&str, usize, usize, Duration, usize)] = &[
            ("慢路径", 64 * 1024, 2_621_440, Duration::from_millis(40), 8 * 1024 * 1024),
            ("快路径", 8 * 1024 * 1024, 125_829_120, Duration::from_millis(2), 48 * 1024 * 1024),
        ];
        for (name, cap, rate, delay, bytes) in arms {
            let mut m1280 = Vec::new();
            let mut m1380 = Vec::new();
            for _ in 0..3 {
                let (secs, got, down, _) = run_shaped_download_mtu(
                    1, *bytes,
                    DirLink::new(*cap, *rate, *delay),
                    DirLink::new(*cap, *rate, *delay),
                    PRODUCT_SHAPE, 1280, Duration::from_secs(120),
                );
                assert_eq!(got, *bytes, "1280 臂应完成（{name}）");
                assert_eq!(down.seen_max, 1280);
                m1280.push(got as f64 / secs / (1024.0 * 1024.0));
                let (secs, got, down, _) = run_shaped_download_mtu(
                    1, *bytes,
                    DirLink::new(*cap, *rate, *delay),
                    DirLink::new(*cap, *rate, *delay),
                    PRODUCT_SHAPE, 1380, Duration::from_secs(120),
                );
                assert_eq!(got, *bytes, "1380 臂应完成（{name}）");
                assert_eq!(down.seen_max, 1380, "1380 臂段尺寸机器判据（{name}）");
                m1380.push(got as f64 / secs / (1024.0 * 1024.0));
            }
            m1280.sort_by(|a, b| a.partial_cmp(b).unwrap());
            m1380.sort_by(|a, b| a.partial_cmp(b).unwrap());
            let (a, b) = (m1280[1], m1380[1]);
            println!("MTU A/B {name}（链路 {}MB/s）：1280={a:.1}MB/s 1380={b:.1}MB/s 比={:.3}", rate / (1024 * 1024), b / a);
            assert!(b >= 0.9 * a, "{name}：1380 臂不劣化门（0.9×）未过：{b:.1} vs {a:.1}");
        }
    }

}

