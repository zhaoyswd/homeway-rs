//! 出口装配与驱动循环（R3；语义真源 `internal/server/serve.go` 的 Start/Run +
//! `publicendpoint.go` + `udpcap.go`——裁剪面见 R3-design §0：relay/term/DDNS/
//! daemon 控制面不在本期）。
//!
//! 线程面（设计 §1.1；M5 S3b 后的形态）：装配线程（Start）→ 一条驱动线程（独占拦截栈/
//! 设备表/腿面 ServerBind/公共端点面 PubFace 的写侧——无锁热路径）+ DNS worker 池
//! （dnsproxy 自带）+ 拦截 worker 池（intercept 自带）+ files/speedtest UDS 服务线程 +
//! 公网端点观测线程 + udpcap 探测线程。观测线程经 `EngineCmd` 与驱动线程交互；STUN 观测
//! 走**公共端口**（QUIC 面的直连 socket——M5 S3a：`public endpoint 面迁址`），caps/probe
//! 端点回填同面。
//!
//! 收工序（Go Shutdown D5）：① 停新流（HaltNew）→ ② 关 UDS listeners → ③ 过境存量
//! 有界宽限（Drain）→ ④ 关 QUIC 面（公共 UDP 端口）→ ⑤ DNS/观测收工 + UPnP 缩租 + 服务收工。

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::server::bind::ServerBind;
use crate::server::dnsproxy::{DnsConfig, DnsProxy};
use crate::server::egress::{self, IfaceInfo};
use crate::server::intercept::{self, Config as ItcConfig, Interceptor, Stats as ItcStats};
use crate::server::pubface::PubFace;
use crate::server::state::State;
use crate::server::table::{DevOp, DeviceTable, TableConfig};
use crate::token::{Endpoint, EndpointKind};
use crate::reg2;
use crate::Logf;

/// 默认内部地址/端口（Go serve.go 常量同源）。
pub const DEFAULT_TUNNEL_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 255, 1);
pub const DEFAULT_FILES_PORT: u16 = 7802;
pub const DEFAULT_TERM_PORT: u16 = 7724;
pub const DEFAULT_SPEEDTEST_PORT: u16 = 7803;
pub const DEFAULT_DNS_PORT: u16 = 5300;
/// 出口对外端口缺省（2026-10-14 起 38241；此前 41641 沿袭自 tailcat——同机并存两套时撞端口）。
/// 已部署机器的 config 显式写了端口，不受缺省变更影响；中继缺省见 [`crate::relay::DEFAULT_LISTEN`]。
pub const DEFAULT_LISTEN_PORT: u16 = 38241;
/// 公网端点刷新（成/败）。
const PUBLIC_REFRESH_OK: Duration = Duration::from_secs(600);
const PUBLIC_REFRESH_FAIL: Duration = Duration::from_secs(120);
/// udpcap 重探间隔（Go udpCapInterval——设计表误写 10min，Go 真源 5min）。
const UDPCAP_INTERVAL: Duration = Duration::from_secs(300);

/// 公共端口 socket 钉哪张物理网卡（BindMode；历史名 = WG socket）。
#[derive(Clone, Debug, PartialEq)]
pub enum BindMode {
    /// 自动挑卡（探针取最快；看护循环每轮重挑）。
    Auto,
    /// 显式网卡名（看护循环按名重解析）。
    Explicit(String),
    /// IP 字面量：单栈绑该地址（不钉卡、不看护——历史上绕开 Surge 抢路由的形态；
    /// Go `ResolveBind` 的 BindAddr 分支同义）。
    Addr(IpAddr),
    Off,
}

/// 出口配置（flag 显式设值 > config.toml > 内置默认——nodeconfig 覆盖序）。
#[derive(Clone)]
pub struct ServeConfig {
    pub state_dir: PathBuf,
    pub listen_port: u16,
    /// QUIC 端口（M1 §1.1）：`None` = 缺省 `listen_port + 1`；被占用按 WG 同款退让
    /// 换端口（实际端口见 `cache/quic_listen_port.txt` 与 `ServeEngine::quic_local_addr`）。
    pub quic_listen_port: Option<u16>,
    pub tunnel_ip: Ipv4Addr,
    pub files_port: u16,
    pub term_port: u16,
    pub speedtest_port: u16,
    pub dns_port: u16,
    pub max_devices: usize,
    pub peer_ttl: Duration,
    pub public_endpoint: String,
    pub bind_iface: BindMode,
    pub upnp: bool,
    /// v4 STUN 候选（逗号分隔，首个解析成功且有应答者胜；空 = 不做 STUN 观测）。
    /// 2026-10-20 起默认**国内**服务器（小米/芒果 TV/B 站——阿里云与本机实测 5–22ms，
    /// RFC 5389 XOR-MAPPED 正确），域名经 `doh` 解析、不经系统解析器。IP 字面量直收。
    pub stun: String,
    /// v6 路径校验 STUN（空 = 跳过 v6 公布）。默认国内 hitv（实测有 AAAA 且 v6 应答）
    /// + cloudflare 兜底（国内可达 ~190ms）。候选语义同 `stun`。
    pub stun6: String,
    /// DoH 解析候选（`host@ip[,host@ip]`；host = SNI/证书主体，ip = **钉卡 TCP 直连**
    /// 目标——本身绕开系统解析）。公网观测腿的域名全部经它解析，不吃 fake-IP、不进
    /// TUN。空 = 退回系统解析器（仍过污染闸，但 fake-IP 环境下等于观测腿作废）。
    /// 默认阿里 + 腾讯 DNSPod。见 `server::doh`。
    pub doh: String,
    pub files_root: Option<PathBuf>,
    pub verbose: bool,
    pub build: String,
    /// serve --relay / config serve.relay：rl1 token 或裸 host:port（None = 不起注册腿）。
    pub relay: Option<String>,
    /// DDNS 裸域名（可多条，`[[serve.ddns]]` / `--ddns`）：token 叠加 `域:端口` 条目
    /// （不解析不踢除；域名记录由用户 DDNS 设施维护）+ 自检随公网端点探测同拍跑。
    pub ddns: Vec<String>,
    /// DNS 显式上游覆盖（F2 `serve.dns_upstream`，**opt-in**；空 = 跟随
    /// `/etc/resolv.conf` 的今日语义——tier `wg-native-dns:40` MUST 的默认面不动）。
    pub dns_upstream: Vec<SocketAddr>,
    /// 兜底上游（F2 `serve.dns_fallback`；默认 = `223.5.5.5`，仅连接层失败触发）。
    /// 空串 = 拒启（值域层），此处不再为空。
    pub dns_fallback: String,
    /// DDNS 自检直查解析器（F2 `serve.ddns_resolver`；默认 = 常量表投影）。
    pub ddns_resolver: Vec<SocketAddrV4>,
    /// 挑卡 / 健康探针 / udpcap `DNS:53` 能力位目标（F2 `serve.dns_probe_target`；
    /// 默认 = `default_probe_targets()`；**一键喂三路是显式登记的耦合**）。
    pub dns_probe_target: Vec<SocketAddrV4>,
    /// 通用 UDP（非 53）能力位目标（F2 `serve.stun_probe_target`；默认 =
    /// `default_stun_targets()`）。
    pub stun_probe_target: Vec<SocketAddrV4>,
    /// 发送整形的 config 覆盖（D-3 反过拟合约束 3：`[serve.tx_shape]` 节；
    /// None = 全默认。env 测试缝的优先级在 `tx_shape_resolve` 内——env > config > 默认）。
    pub tx_shape_cfg: Option<crate::server::intercept::TxShapeCfg>,
    /// 抗放大闸的 config 覆盖（M2 §3.2 的 `[serve.quic_admit]` 七键；`None` = 全设计缺省）。
    /// 值域已由 `homeway` 的严格读层校验（非法 ⇒ 拒启）；env
    /// `HOMEWAY_QUIC_ADMIT_RETRY` 的叠加在 [`crate::server::quic_admit::resolve_retry_policy`]。
    pub quic_admit: Option<crate::server::quic_admit::AdmitLimits>,
}

/// 公网观测腿缺省（2026-10-20 排障后换国内；选型实测见 tier docs/reviews 当日记录）。
pub const DEFAULT_STUN: &str = "stun.miwifi.com:3478,stun.hitv.com:3478,stun.chat.bilibili.com:3478,stun.cloudflare.com:3478";
pub const DEFAULT_STUN6: &str = "stun.hitv.com:3478,stun.cloudflare.com:3478";
pub const DEFAULT_DOH: &str = "dns.alidns.com@223.5.5.5,dns.alidns.com@223.6.6.6";

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            state_dir: PathBuf::from("."),
            listen_port: DEFAULT_LISTEN_PORT,
            quic_listen_port: None,
            tunnel_ip: DEFAULT_TUNNEL_IP,
            files_port: DEFAULT_FILES_PORT,
            term_port: DEFAULT_TERM_PORT,
            speedtest_port: DEFAULT_SPEEDTEST_PORT,
            dns_port: DEFAULT_DNS_PORT,
            max_devices: 32,
            peer_ttl: Duration::from_secs(7 * 24 * 3600),
            public_endpoint: String::new(),
            bind_iface: BindMode::Auto,
            upnp: true,
            stun: DEFAULT_STUN.to_owned(),
            stun6: DEFAULT_STUN6.to_owned(),
            doh: DEFAULT_DOH.to_owned(),
            files_root: None,
            verbose: false,
            build: "homeway-rs-dev".to_owned(),
            relay: None,
            ddns: Vec::new(),
            // F2 五键默认 = 修前硬编值（逐值不变——缺省行为兼容既有部署）
            dns_upstream: Vec::new(),
            dns_fallback: crate::server::dnsproxy::DEFAULT_FALLBACK.to_owned(),
            ddns_resolver: crate::server::ddnscheck::default_resolvers_v4(),
            dns_probe_target: egress::default_probe_targets(),
            stun_probe_target: egress::default_stun_targets(),
            tx_shape_cfg: None,
            quic_admit: None,
        }
    }
}

/// QUIC 端口定案（M1 §1.1）：显式 `serve.quic_listen` 优先，缺省 = `serve.listen + 1`。
/// 单独成函数是为了可测（端口算术 + 边界）且与「退让」解耦（退让在 socket 绑定层）。
fn quic_listen_port(cfg: &ServeConfig) -> u16 {
    cfg.quic_listen_port
        .unwrap_or_else(|| cfg.listen_port.saturating_add(1))
}

/// 停机宽限（Go StopGrace 同值——「stop 后还能通最多 10s」的显式语义）。
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// 观测线程 → 驱动线程的命令。
pub enum EngineCmd {
    Stop { grace: Duration },
    /// 公共端口（QUIC 面直连 socket）上的 STUN 观测（应答经 reply 回执——None = 无应答/
    /// 不合法；QUIC 面未起 = 无公共端口，直接回 None）。
    StunQuery { server: SocketAddr, reply: Sender<Option<SocketAddr>> },
    StunAbort,
    /// 探测应答的能力位（udpcap 结论）。
    SetCaps(u8),
    /// 探测应答的端点列表段（已公布公网端点）。
    SetProbeEndpoints(Vec<SocketAddr>),
    /// 公网端点立即重测（换网事件——绑卡看护的 onChange）。
    KickPublicEndpoint,
    /// 把**公共端口**（QUIC 面直连 socket）重钉到指定网卡（绑卡看护换卡/重挑；
    /// socket 由 QUIC 面独占 ⇒ 本命令只做 setsockopt——M5 S3a）。
    Repin {
        index: u32,
        name: String,
        reply: Sender<std::io::Result<()>>,
    },
    /// serve.status 观测缝（B0-2b）：设备表 brief 快照（表 = 驱动线程独占——快照
    /// 必须经驱动线程取；拦截计数是共享原子、不经此路）。
    StatusQuery { reply: Sender<Vec<EnginePeerBrief>> },
    /// 控制面 SESSION 通告 → 向中继数据口拨腿（R4）。
    LegRegister { id: u64, remote: SocketAddr, marker: Vec<u8>, admission_marker: Vec<u8> },
    /// 控制面 RELEASE → 拆腿。
    LegRemove { id: u64 },
    /// 控制面重连对账 → 拆全部腿（等重放重建）。
    LegsClear,
}

/// udpcap 能力位（与 Go/tier 核同源）。
pub const UDPCAP_DNS: u8 = 1 << 0;
pub const UDPCAP_GENERIC: u8 = 1 << 1;
pub const UDPCAP_OBSERVED: u8 = 1 << 2;
pub const UDPCAP_PROBED: u8 = 1 << 3;
pub const UDPCAP_SEEN: u8 = 1 << 4;

/// 设备表 brief 单条（观测面：dev = devTag 全 16hex、隧道 /32、最近注册 UnixMilli、
/// 距今毫秒——Go servercore.DeviceBrief 同形）。
#[derive(Debug, Clone)]
pub struct EnginePeerBrief {
    pub dev: String,
    pub tunnel_ip: std::net::Ipv4Addr,
    pub last_reg_ms: i64,
    pub idle_ms: i64,
}

/// 过境拦截计数 brief（共享原子读——Go ServeInterceptBits 同形）。
#[derive(Debug, Clone, Copy, Default)]
pub struct EngineInterceptBits {
    pub dial_ok: u64,
    pub dial_fail: u64,
    pub reject: u64,
    pub flows: u64,
    /// Q-B 新增丢弃计数（F3/F4/F7/F10）。
    pub udp_drop: u64,
    pub shape_drop: u64,
    pub frag_drop: u64,
    /// Q-K 新增（F1/F2/F5-d）：分片重组的成功/畸形/攻击/丢包/资源五类信号 + TX 侧无首片
    /// 丢片。**单位差异**：`frag_reasm` = 报文数（datagram），其余 = 分片包数。
    pub frag_reasm: u64,
    pub frag_bad: u64,
    pub frag_overlap: u64,
    pub frag_timeout: u64,
    pub frag_limit: u64,
    pub tx_frag_drop: u64,
}

/// 出口引擎句柄（装配线程持有；stop 收工）。
pub struct ServeEngine {
    cmd_tx: Sender<EngineCmd>,
    driver: Mutex<Option<std::thread::JoinHandle<()>>>,
    shutdown_completion: Mutex<()>,
    /// 驱动线程在世位（线程出口清零——panic 也清：supervisor 的存活判据）。
    driver_alive: Arc<AtomicBool>,
    /// 观测与兜底取消位；停止后等待两个线程退出，再允许缩租与替换。
    pubep_stop: Arc<AtomicBool>,
    publication: Arc<Mutex<()>>,
    observers: Mutex<Vec<std::thread::JoinHandle<()>>>,
    observer_threads: Vec<std::thread::ThreadId>,
    pub_kick: mpsc::SyncSender<()>,
    relay_workers: Mutex<Vec<super::relayleg::RelayWorker>>,
    /// 拦截计数共享原子（status 观测面直读——不占驱动线程）。
    itc_stats: Arc<super::intercept::Stats>,
    /// 出口**公共端口**（M5 起 = QUIC 面的直连端口；`None` = 本世代无公共端口——
    /// 面未起的形态）。`shrink_upnp_lease`/status 与 token 的端口口径同源。
    pub local_port: u16,
    /// QUIC 端点实际地址（`None` = 本世代 QUIC 面未起；M1 §1.1 的「实际端口有去处」）。
    pub quic_local_addr: Option<SocketAddr>,
    /// 出口 RPK 公钥（进 token 的 32B；M1 §12-②）。
    pub quic_rpk_public_key: Option<homeway_quic::RpkPublicKey>,
    /// 出口 QUIC 面**只读计数句柄**（M3 S2；None = 本世代面未起——`serve status --json`
    /// 的 `quic` 段随之缺席）。
    quic_stats: Option<homeway_quic::ExitStatsHandle>,
    stop_flags: Vec<Arc<AtomicBool>>,
    /// term 服务面（**windows 不装配**——用户指令豁免面，WIN 批 §D0）。
    #[cfg(not(windows))]
    term_srv: Option<Arc<crate::term::service::TermService>>,
    pub state_dir: PathBuf,
    pub serve_dir: PathBuf,
    socks: Vec<(PathBuf, (u64, u64))>,
    /// DDNS 域名表与自检状态（status 观测面快照用；与 TokenCtx 共享同一份）。
    ddns: Vec<String>,
    ddns_checks: super::ddnscheck::DdnsChecks,
}

impl ServeEngine {
    /// 装配并启动（非阻塞；判据行按 Go 同序打出：E2/E18 → E21 → E4 → E5 → E6 →
    /// E14/E17 → E19 → E1）。
    /// 日志流参数：`logf` = 摘要流〔终端+events 文件——统一进程 launchd stdout 双写
    /// 口径〕；`dlogf` = 细节流〔debug 文件恒写，verbose 回显〕；`tokf` = token 端点
    /// 变化轮流〔events 文件只写，verbose 回显〕；`log_paths` = 首轮「日志：」提示行。
    pub fn start(
        mut cfg: ServeConfig,
        logf: Logf,
        dlogf: Logf,
        tokf: Logf,
        log_paths: Option<(String, String)>,
    ) -> std::io::Result<Arc<Self>> {
        // Q5/S7b：公网端点非法 ⇒ 告警 + 按未配置处理（Go `serve.go:113-121` 的 `fill()` 同形）。
        normalize_public_endpoint(&mut cfg, &logf);
        let serve_dir = cfg.state_dir.join("serve");
        let cache_dir = cfg.state_dir.join("cache");
        std::fs::create_dir_all(&serve_dir)?;
        std::fs::create_dir_all(&cache_dir)?;

        // ---- state：key/tokens/revoked（E2/E18）----
        let st = Arc::new(State::open(&serve_dir)?);
        let mut secrets = st.secrets().map_err(io_other)?;
        if secrets.is_empty() {
            // 零参首启或凭证全被吊销：先签发一条并落台账（否则打印的 token 生来无效）。
            // 此处的 rpk = None：QUIC 面在后文才起（有 QUIC 面时的 token 由
            // `print_client_token` 铸出，带端点与 rpk——M1 S1c）。
            st.issue_token(vec![], None).map_err(io_other)?;
            (logf)("凭证：现有凭证全部不可用（首启或已吊销）——已铸出新凭证（客户端需重新粘贴新 token）");
            secrets = st.secrets().map_err(io_other)?;
            if secrets.is_empty() {
                return Err(std::io::Error::other("state: 初始凭证签发后仍读不到"));
            }
        }
        if let Ok(ledger) = st.ledger() {
            let revoked_rows = ledger.iter().filter(|e| e.revoked).count();
            (logf)(&format!(
                "凭证台账：{} 行记录 / {} 枚在用凭证（其中 {revoked_rows} 行已吊销；吊销即时对新注册生效）",
                ledger.len(),
                secrets.len()
            ));
        }
        let secret0 = secrets[0];
        let priv_key = st.private_key().map_err(io_other)?;
        let backend_label = backend_label(&priv_key);
        let backend_pub6 = {
            use x25519_dalek::PublicKey;
            let p = PublicKey::from(&priv_key);
            p.as_bytes()[..6].to_vec()
        };

        // ---- 绑卡（E21；挑不到就不绑，绝不因此拒绝启动）----
        // Go `ResolveBind` 语义拆分：网卡名/auto = **整 socket 钉卡不绑地址**（双栈
        // 监听，v6 直连路径不被 v4 源地址绑死）；IP 字面量 = 单栈绑地址（无钉卡）。
        let mut resolved: Option<IfaceInfo> = None;
        let mut bind_addr: Option<IpAddr> = None;
        match &cfg.bind_iface {
            BindMode::Off => {}
            BindMode::Addr(ip) => {
                bind_addr = Some(*ip);
                // Go bind.go Open 的 `else if laddr.IP != nil` 分支：绑源地址**且**
                // 把 socket 钉在该地址所属网卡上（IfaceForAddr——绑地址不钉卡时
                // TUN 代理机器的观测仍会被污染；查不到所属卡 = 只绑地址不钉）。
                if let Some(ifi) = egress::iface_for_addr(*ip) {
                    (logf)(&format!("绑卡：绑定地址 {ip}（钉 {}）", ifi.name));
                    resolved = Some(ifi);
                }
            }
            BindMode::Auto => {
                let cands = egress::physical_candidates();
                let d2 = Arc::clone(&dlogf);
                let r = egress::select_best(&cands, &cfg.dns_probe_target, Duration::from_secs(2), &move |s: &str| {
                    (d2)(s);
                });
                match r {
                    Ok(best) => {
                        (logf)(&format!("绑卡：自动挑到 {}（{}）", best.name, iface_state_str(&best)));
                        resolved = Some(best);
                    }
                    Err(e) => {
                        (logf)(&format!(
                            "绑卡：自动挑卡失败（{e}）—— 本轮不绑，走系统默认路由（TUN 型代理机器上请用 --bind-interface <网卡>）"
                        ));
                    }
                }
            }
            BindMode::Explicit(name) => {
                let found = egress::interfaces().into_iter().find(|i| i.name == *name);
                match found {
                    Some(i) => {
                        (logf)(&format!("绑卡：显式指定 {}（{}）", i.name, iface_state_str(&i)));
                        resolved = Some(i);
                    }
                    None => {
                        // Go cli.go ResolveBind 语义：名字不存在 → 告警后退回 auto（TUN
                        // 代理机器上「不绑」恰恰是最危险的形态——评审 M18）。**模式随
                        // 之变 auto**（看护每轮重挑——Go 返回 BindAuto 同义；评审 r1-M1：
                        // 不改写会让看护按死名字每 5s 空转）。
                        (logf)(&format!(
                            "⚠️ --bind-interface {name:?} 找不到（no such interface）—— 退回 auto（自动挑卡）"
                        ));
                        let cands = egress::physical_candidates();
                        let d2 = Arc::clone(&dlogf);
                        match egress::select_best(&cands, &cfg.dns_probe_target, Duration::from_secs(2), &move |s: &str| {
                            (d2)(s);
                        }) {
                            Ok(best) => {
                                (logf)(&format!("绑卡：自动挑到 {}（{}）", best.name, iface_state_str(&best)));
                                resolved = Some(best);
                            }
                            Err(e) => {
                                (logf)(&format!(
                                    "绑卡：自动挑卡失败（{e}）—— 本轮不绑，走系统默认路由（TUN 型代理机器上请用 --bind-interface <网卡>）"
                                ));
                            }
                        }
                    }
                }
            }
        }

        // ---- DNS 代答（E4）----
        let dns_enabled = cfg.dns_port != 0;
        let (dns, dns_events_rx) = if dns_enabled {
            // F2：上游覆盖（opt-in）+ 兜底键（默认 = 修前常量，逐值不变）
            let dns_cfg = DnsConfig {
                upstreams: cfg.dns_upstream.clone(),
                fallback_dns: cfg.dns_fallback.clone(),
                ..DnsConfig::default()
            };
            let (p, rx) = DnsProxy::spawn(dns_cfg, Arc::clone(&logf), Arc::clone(&dlogf));
            (Some(p), Some(rx))
        } else {
            (None, None)
        };
        if let Some(d) = &dns {
            if let Err(e) = d.self_check() {
                (logf)(&format!(
                    "⚠️ dns 代答自验证未通过（{e}）——上游此刻不可达（会跟随主机恢复/空表周期重试），期间查询按 SERVFAIL/兜底处理"
                ));
            }
            (logf)(&format!(
                "dns 代答就绪：tunnel={}:53（UDP+TCP）resolve={}:{}（TCP）upstream={}",
                cfg.tunnel_ip,
                cfg.tunnel_ip,
                cfg.dns_port,
                d.upstreams_text()
            ));
        }

        // ---- 拦截层（E5）----
        // M5 S4：`LocalServices` 三服务映射随豁免面退役（服务面走 QUIC `STREAM[tag]`）。
        let itc_stats = Arc::new(ItcStats::default());
        let dns_proxy = dns.clone();
        let dns_events = dns_events_rx;
        let mut intercept = Interceptor::attach(
            ItcConfig {
                tunnel_ip: cfg.tunnel_ip,
                dns: dns_proxy,
                dns_events,
                dns_resolve_port: if dns_enabled { cfg.dns_port } else { 0 },
                tx_shape: intercept::tx_shape_resolve(cfg.tx_shape_cfg),
                logf: Arc::clone(&dlogf),
            },
            Arc::clone(&itc_stats),
        );
        intercept.attach_dns();

        // ---- device + 设备表（E6）+ 吊销跟随 ----
        let revoked_set: Arc<Mutex<HashSet<[u8; 32]>>> =
            Arc::new(Mutex::new(st.revoked_secrets().map_err(io_other)?.into_iter().collect()));
        let revoked_hook_set = Arc::clone(&revoked_set);
        let revoked_hook: crate::server::table::RevokedHook =
            Arc::new(move |s: &[u8; 32]| revoked_hook_set.lock().map(|m| m.contains(s)).unwrap_or(false));
        let table = DeviceTable::new(
            secrets.clone(),
            TableConfig { max_devices: cfg.max_devices, ttl: cfg.peer_ttl, grace: Default::default(), revoked: Some(revoked_hook) },
            Arc::clone(&dlogf),
        );
        let (cap, ttl, grace) = table.limits();
        (dlogf)(&format!(
            "peer 表：设备表就绪（cap={cap}，ttl={}，grace={}；按 devTag 记账/刷新/轮换）",
            crate::go_fmt::fmt_duration_go_secs(ttl),
            crate::go_fmt::fmt_duration_go_secs(grace)
        ));
        // ---- 腿面 + 公共端点面（M5 S3b/S3a）----
        // 腿面不再自有 socket（唯一公共端口 = QUIC 面的直连 socket，见 `server::bind` 模块头）；
        // 公共端点面（探测应答 / STUN 观测 / 绑卡重钉 / 注册腿帧）见 `server::pubface`。
        let mut bind = ServerBind::new(Arc::clone(&logf));
        // 钉卡目标 cell（公共端口 socket 与 DoH 解析腿共享；见 PubFace::pin_iface）
        let pin_iface: Arc<Mutex<Option<(u32, String)>>> = Arc::new(Mutex::new(None));
        let pubface = Arc::new(PubFace::new(&cfg.build, Arc::clone(&logf), Arc::clone(&pin_iface)));

        // ---- files / speedtest / term 服务（E14/E15/E16/E17）+ 服务入口（M3 S2）----
        // 服务入口形态（设计 §2.2 方案 B′）：每个服务一枚 `ServiceIntake` = **两源**
        // （UDS 监听器〔WG 服务腿/调试〕+ QUIC stream 队列〔tag 分发，出口面泵入队〕）。
        // 容量 = 该服务在册上限 + K（§1.7 设计门 2-4/2-5：保证**应用层 busy 路径**先于
        // intake 满触发）；`ServiceIntake` 同时是 `serve_stoppable` 的形参（服务本体零改）。
        let mut stop_flags = Vec::new();
        #[cfg(unix)]
        let mut socks = Vec::new();
        #[cfg(windows)]
        let socks = Vec::new(); // windows 无本地调试腿（评审 M2）——收工清理面为空
        #[cfg(not(windows))]
        let mut term_srv: Option<Arc<crate::term::service::TermService>> = None;
        let mut service_intakes = homeway_quic::ServiceIntakes::default();
        // ---- DNS 流腿（`STREAM[tag=6]` dns-resolve；DNS-STREAM 批，2026-10-12）----
        // 可用性判据 = `dns_enabled`（**与 `dns_port` 取值无关**——tag 不携带端口；`dns_port`
        // 只管栈内 `:53/:5300` 监听面）；未装配 ⇒ 出口对该 tag 落既有 `reset(0x22)`。
        if let (Some(proxy), Some((intake, tx))) = (&dns, dns_intake_of(dns.as_ref(), &logf)) {
            service_intakes.dns = Some(tx);
            let stop = spawn_service_stop_flag(&mut stop_flags);
            let proxy2 = Arc::clone(proxy);
            let d2 = Arc::clone(&dlogf);
            let d3 = Arc::clone(&dlogf);
            if let Err(e) = std::thread::Builder::new()
                .name("homeway-dns-stream".into())
                .spawn(move || {
                    if let Err(e) =
                        crate::server::dns_stream::serve(intake, proxy2, stop, Arc::clone(&d2))
                    {
                        (d2)(&format!(
                            "dns: 流腿服务线程退工（{e}）——socks 域名目标不可用，直到重启出口"
                        ));
                    }
                })
            {
                (d3)(&format!("⚠️ dns: 流腿服务线程起不来（{e}）——socks 域名目标不可用"));
            }
        }
        let files_srv = crate::files_server::FilesServer::open(cfg.files_root.as_deref(), Arc::clone(&dlogf)).ok();
        if let Some(fsrv) = files_srv {
            // 本地调试腿（<serve_dir>/files.sock）是 **unix 面**：windows 只装 QUIC 入口
            // （主路径不缺；评审 M2——修前 windows 也 bind 本地口后即丢：端口文件残留 +
            // 就绪行误导）。
            #[cfg(unix)]
            let local = match crate::files_server::listen_local_service(&serve_dir, "files.sock", &dlogf) {
                Ok(ln) => {
                    socks.push((serve_dir.join("files.sock"), sock_identity(&serve_dir.join("files.sock"))));
                    Some(ln)
                }
                Err(e) => {
                    (logf)(&format!(
                        "⚠️ files 监听 {} 失败（{e}）—— 文件管理会报错（state 目录异常/被其它实例占用），其余功能不受影响",
                        serve_dir.join("files.sock").display()
                    ));
                    None
                }
            };
            // E14（M5 S4 定稿行文）：服务流 tag=1 经服务入口（QUIC STREAM）转投；本机
            // UDS 只服务调试面（WG 服务腿已退役）。
            #[cfg(unix)]
            (logf)(&format!(
                "files 就绪：root={} (rw) sock={}（STREAM tag=1 经服务入口转投）",
                fsrv.root_dir().display(),
                serve_dir.join("files.sock").display()
            ));
            #[cfg(windows)]
            (logf)(&format!(
                "files 就绪：root={} (rw)（STREAM tag=1 经服务入口转投；本机调试腿在 windows 不装配）",
                fsrv.root_dir().display()
            ));
            #[cfg(unix)]
            let built = local.and_then(|ln| {
                service_intake(
                    ln,
                    homeway_quic::tuning::service_defaults::intake_capacity(crate::files_server::MAX_CONNS),
                    "files",
                    &logf,
                )
            });
            #[cfg(windows)]
            let built = service_intake(
                homeway_quic::tuning::service_defaults::intake_capacity(crate::files_server::MAX_CONNS),
                "files",
                &logf,
            );
            {
                if let Some((intake, tx)) = built {
                        service_intakes.files = Some(tx);
                        let stop = spawn_service_stop_flag(&mut stop_flags);
                        let d2 = Arc::clone(&dlogf);
                        let d3 = Arc::clone(&dlogf);
                        if let Err(e) = std::thread::Builder::new()
                            .name("homeway-files".into())
                            .spawn(move || {
                                // F5：只 Fatal（监听面真没了）退工——退工记行（修前静默 `let _ =`）
                                if let Err(e) = fsrv.serve_stoppable(intake, stop) {
                                    (d2)(&format!("files: 服务线程退工（{e}）——文件管理不可用，直到重启出口"));
                                }
                            })
                        {
                            (d3)(&format!("⚠️ files: 服务线程起不来（{e}）——监听点无人受理（文件管理不可用）"));
                        }
                    }
            }
            // 入口起不来（`None`）：该服务整条腿停用（已记行；QUIC 档该 tag 落到 0x22）
        } else {
            (logf)("⚠️ files 根目录不可用—— 文件管理会报错，其余功能不受影响");
        }
        let speed_srv = Arc::new(crate::speedtest_server::SpeedtestServer::new(Arc::clone(&dlogf)));
        // 本地调试腿同 files：unix 面（评审 M2）。
        #[cfg(unix)]
        let local = match crate::files_server::listen_local_service(&serve_dir, "speedtest.sock", &dlogf) {
            Ok(ln) => {
                socks.push((
                    serve_dir.join("speedtest.sock"),
                    sock_identity(&serve_dir.join("speedtest.sock")),
                ));
                Some(ln)
            }
            Err(e) => {
                (logf)(&format!(
                    "⚠️ speedtest 监听 {} 失败（{e}）—— 测速功能会报错（state 目录异常/被其它实例占用），其余功能不受影响",
                    serve_dir.join("speedtest.sock").display()
                ));
                None
            }
        };
        // E17（M5 S4 定稿行文）：服务流 tag=3 经服务入口（QUIC STREAM）转投
        #[cfg(unix)]
        (logf)(&format!(
            "speedtest 就绪：sock={}（STREAM tag=3 经服务入口转投；内存收发不落盘）",
            serve_dir.join("speedtest.sock").display()
        ));
        #[cfg(windows)]
        (logf)("speedtest 就绪：（STREAM tag=3 经服务入口转投；内存收发不落盘；本机调试腿在 windows 不装配）");
        #[cfg(unix)]
        let built = local.and_then(|ln| {
            service_intake(
                ln,
                homeway_quic::tuning::service_defaults::intake_capacity(crate::speedtest_server::MAX_CONNS),
                "speedtest",
                &logf,
            )
        });
        #[cfg(windows)]
        let built = service_intake(
            homeway_quic::tuning::service_defaults::intake_capacity(crate::speedtest_server::MAX_CONNS),
            "speedtest",
            &logf,
        );
        {
            if let Some((intake, tx)) = built {
                    service_intakes.speedtest = Some(tx);
                    let stop = spawn_service_stop_flag(&mut stop_flags);
                    let srv2 = Arc::clone(&speed_srv);
                    let d2 = Arc::clone(&dlogf);
                    let d3 = Arc::clone(&dlogf);
                    if let Err(e) = std::thread::Builder::new()
                        .name("homeway-speedtest".into())
                        .spawn(move || {
                            // F5：只 Fatal 退工——退工记行（修前静默 `let _ =`）
                            if let Err(e) = srv2.serve_stoppable(intake, stop) {
                                (d2)(&format!("speedtest: 服务线程退工（{e}）——测速不可用，直到重启出口"));
                            }
                        })
                    {
                        (d3)(&format!("⚠️ speedtest: 服务线程起不来（{e}）——监听点无人受理（测速不可用）"));
                    }
            }
        }

        // ---- 终端会话 / agent gateway（exit-service-uds：UDS 承载 <serve>/term.sock）。
        //      客户端拨隧道 IP:<term_port>，拦截层按 LocalServices 映射转投；会话由
        //      出口持有（客户端断开只摘泵，不杀进程）；开关 HOMEWAY_TERM=off，调参
        //      HOMEWAY_TERM_*（6f-3b）。----
        // WIN 批门控：term 在 windows 不装配（用户指令豁免——docs/reviews/WIN-design.md
        // §0）；`service_intakes.term` 保持 None ⇒ 出口对该 tag 落既有 reset(0x22)。
        #[cfg(not(windows))]
        {
            if crate::term::service::disabled_by_env() {
                (logf)("term 服务被 HOMEWAY_TERM=off 关闭");
            } else {
                // stateDir 参数 = manifest 覆盖目录基（<dir>/agent-detection/）
                let tsrv = crate::term::service::TermService::new(Arc::clone(&dlogf), Some(&serve_dir));
                match crate::files_server::listen_local_service(&serve_dir, "term.sock", &dlogf) {
                    Ok(ln) => {
                        // （整块在 not(windows) 门内——r2-P3：windows 死臂已清。）
                        let own = sock_identity(&serve_dir.join("term.sock"));
                        socks.push((serve_dir.join("term.sock"), own));
                        // 就绪行（判据 E16）：socket + shell + 历史窗口 + 能力位 + vt 现状
                        (logf)(&format!(
                            "# Serving terminal sessions on sock={} (shell={}, history={}, features={}, vt={})",
                            serve_dir.join("term.sock").display(),
                            tsrv.shell_text(),
                            tsrv.history_text(),
                            tsrv.features_text(),
                            tsrv.vt_text()
                        ));
                        match service_intake(
                            ln,
                            homeway_quic::tuning::service_defaults::intake_capacity(tsrv.max_sessions()),
                            "term",
                            &logf,
                        ) {
                            Some((intake, tx)) => {
                                service_intakes.term = Some(tx);
                                let stop = spawn_service_stop_flag(&mut stop_flags);
                                let t = Arc::clone(&tsrv);
                                std::thread::Builder::new()
                                    .name("homeway-term".into())
                                    .spawn(move || t.serve_stoppable(intake, stop))
                                    .ok();
                                term_srv = Some(tsrv);
                            }
                            None => tsrv.close(),
                        }
                    }
                    Err(e) => {
                        // 与 files 同一取舍：可选服务起不来不影响隧道/转发
                        (logf)(&format!(
                            "⚠️ term 监听 {} 失败（{e}）—— 终端功能会报错（state 目录异常/被其它实例占用），其余功能不受影响",
                            serve_dir.join("term.sock").display()
                        ));
                        tsrv.close();
                    }
                }
            }
        }

        // ---- QUIC 面（M1 §1.1/§1.7：独立 UDP 端口 + 专用线程 + RPK 身份）----
        // **M5 起这是出口唯一的公共 UDP 端口**（WG 面退役）：端口 = `serve.quic_listen`
        // （缺省 = serve.listen + 1）；被占用按同款**退让**语义换端口（+1…+9 → 随机），
        // 实际端口落 `cache/quic_listen_port.txt`（与 `listen_port.txt` 同值——公共端口只有
        // 一个）并由 `ServeEngine::quic_local_addr` 暴露（token/UPnP/status 的唯一来源）。
        // 起不来 = fail-soft（进程照常起，但**没有公共端口**——行里看得见）。
        let mut quic_brief: Option<(SocketAddr, homeway_quic::RpkPublicKey)> = None;
        let mut quic_stats: Option<homeway_quic::ExitStatsHandle> = None;
        // 注册腿 socket（出口 → 中继 HELLO/Keepalive 的出站口）：公共端口的 fd 副本
        // ——「注册腿与数据面同一本地端口」的硬约束按 M5 后的口径继续成立（原 = WG socket
        // 的副本）。中继的回执回到本端口 ⇒ 经公共端点面的钩子分派（`pubface`）。
        let mut relay_sock_dup: Option<std::net::UdpSocket> = None;
        // 钉卡成功的运行期事实（公布口径的 pinned 判据；见下方 pinned_flag）
        let mut pinned_pin_ok = false;
        let quic_face = {
            match crate::server::bind::listen_with_fallback_addr(quic_listen_port(&cfg), bind_addr) {
            Ok(sock) => match sock.set_nonblocking(true) {
                Ok(()) => {
                    let actual_port = sock.local_addr().map(|a| a.port()).unwrap_or(0);
                    if actual_port != quic_listen_port(&cfg) {
                        (logf)(&format!(
                            "⚠️ QUIC 监听端口 {} 被占用 —— 改用 {}；token 里的端口以公布/签发为准",
                            quic_listen_port(&cfg),
                            actual_port
                        ));
                    }
                    // 绑卡（E21 族）：**公共端口 socket 的钉卡**（原 WG socket 同款语义——
                    // 钉卡不绑地址；失败只告警，公布自动变保守）。dup fd 交公共端点面供
                    // 运行期重钉（setsockopt 作用于同一 socket 对象）。
                    if let Some((index, name)) = resolved.as_ref().map(|i| (i.index, i.name.clone())) {
                        match egress::pin_socket_to_iface(crate::sysock::udp_raw(&sock), index, &name) {
                            Ok(()) => {
                                pinned_pin_ok = true;
                                if let Ok(mut cell) = pin_iface.lock() {
                                    *cell = Some((index, name.clone()));
                                }
                            }
                            Err(e) => {
                                (logf)(&format!(
                                    "⚠️ 钉网卡 {name} 失败（{e}）—— 继续以未绑卡运行：STUN 观测可能被 TUN 型代理污染，公网端点公布会因此变保守"
                                ));
                            }
                        }
                    }
                    match sock.try_clone() {
                        Ok(dup) => {
                            pubface.attach_socket(dup);
                            match sock.try_clone() {
                                Ok(dup2) => relay_sock_dup = Some(dup2),
                                Err(e) => (logf)(&format!("⚠️ 注册腿 socket 副本失败（{e}）—— --relay 注册腿将无法与公共端口同源")),
                            }
                        }
                        Err(e) => (logf)(&format!("⚠️ 公共端口 socket 副本失败（{e}）—— 绑卡看护重钉不可用")),
                    }
                    let seed = crate::server::quic_rpk_seed(&priv_key);
                    // 抗放大闸生效值（M2 §3.2）：config 段（严格读层已校验值域）+ env 叠加
                    // （非法 env ⇒ 记行 + 缺省，不 fail-fast）。
                    let admit = crate::server::quic_admit::resolve_retry_policy(
                        cfg.quic_admit.unwrap_or_default(),
                        crate::envflag::quic_admit_retry_raw(),
                        &logf,
                    );
                    match homeway_quic::ExitQuic::start(
                        sock,
                        homeway_quic::ExitQuicConfig::new(seed, cfg.max_devices)
                            .with_admit(admit)
                            .with_intakes(service_intakes.clone())
                            .with_plain_hook(pubface.hook()),
                        Arc::clone(&logf),
                    ) {
                        Ok(q) => {
                            // L3 可弃缓存（与 listen_port.txt 同语义）：写失败不致命
                            let _ = std::fs::write(
                                cache_dir.join("quic_listen_port.txt"),
                                format!("{}\n", q.local_addr().port()),
                            );
                            quic_brief = Some((q.local_addr(), q.rpk_public_key()));
                            // M3 S2：只读计数句柄留装配点（`serve status --json` 的
                            // `quic` 段直读——驱动线程仍独占面句柄）
                            quic_stats = Some(q.stats_handle());
                            Some(q)
                        }
                        Err(e) => {
                            (logf)(&format!(
                                "⚠️ 端点未起（{e}）—— **出口没有公共端口**（客户端无从连入；本进程只剩服务/控制面）"
                            ));
                            None
                        }
                    }
                }
                Err(e) => {
                    (logf)(&format!(
                        "⚠️ 端点未起（socket 置非阻塞失败：{e}）—— **出口没有公共端口**"
                    ));
                    None
                }
            },
            Err(e) => {
                (logf)(&format!(
                    "⚠️ 端点未起（端口 {} 退让 +1…+9 与随机端口全失败：{e}）—— **出口没有公共端口**",
                    quic_listen_port(&cfg)
                ));
                None
            }
            }
        };
        // 公共端口（= QUIC 面实际端口）落盘 + pinned 运行期事实（看护重钉后更新）。
        let local_port = quic_brief.as_ref().map(|(a, _)| a.port()).unwrap_or(0);
        // Q4/S7b：写失败**非致命**（原 `?` = 致命 ⇒ serve 启不来，与 Go `role.go:95` 相反）。
        write_listen_port_file(&cache_dir.join("listen_port.txt"), local_port, &logf);
        // 公布口径的 pinned 判据 = **运行期事实**（socket 钉卡成功与否 + 绑地址），
        // 看护循环重钉后更新（Go `pinnedNow`/`PinnedIface` 同义）。
        let pinned_flag = Arc::new(AtomicBool::new(pinned_pin_ok || bind_addr.is_some()));
        let pubep_stop = Arc::new(AtomicBool::new(false));
        let publication = Arc::new(Mutex::new(()));
        let mut observers = Vec::new();
        let mut relay_workers = Vec::new();

        // ---- 命令通道（驱动线程收；观测/udpcap/中继控制面都经它交互） ----
        let (cmd_tx, cmd_rx) = mpsc::channel::<EngineCmd>();
        // kick 通道 cap=1 + 非阻塞发送（Go kickUDPCap/kickPublicEndpoint 的
        // chan struct{} cap=1 + select-default 同义——重复 kick 合并成一次）
        let (pub_kick_tx, pub_kick_rx) = mpsc::sync_channel::<()>(1);
        // 首轮探测信号（Go s.firstProbe：公网端点第一轮结束〔成不成都算〕即发一次——
        // token 兜底线在等它，探测全关形态由兜底线自己睡 15s）
        let (first_probe_tx, first_probe_rx) = mpsc::sync_channel::<()>(1);

        // ---- 中继注册腿（--relay；R4）：必须在 TokenCtx 构造前把 relay_ep 配好
        //      （token 首轮打印要带中继端点——Go serve.go:406-409 用户口径），
        //      socket clone 窗口 = bind move 进驱动线程前 ----
        let mut relay_ep: Option<Endpoint> = None;
        let mut relay_wanted = false;
        if let Some(spec) = &cfg.relay {
            match super::relayleg::parse_relay_arg(spec) {
                Ok(arg) => {
                    relay_ep = Some(Endpoint { addr: arg.addr.to_string(), kind: EndpointKind::Relay });
                    relay_wanted = true;
                }
                Err(e) => {
                    (logf)(&format!("⚠️ --relay 解析失败（{e}）—— 跳过中继注册"));
                }
            }
        }
        // 重新解析一次拿完整 RelayArg（addr + secret；上面的 relay_ep 只端点面）
        let relay_arg = relay_ep.as_ref().and_then(|_| super::relayleg::parse_relay_arg(cfg.relay.as_deref().unwrap_or_default()).ok());
        if let Some(arg) = relay_arg {
            // fail-soft（2026-10-10 全面审查 中-1）：socket 副本缺席（端口 bind 全失败/
            // 置非阻塞失败/try_clone 失败——都是上方已按 fail-soft 记行降级的同族形态）
            // 时**不 panic**——按「--relay 解析失败」同款处置：不起注册腿与控制面，
            // 中继端点不并入 token（出口以直连形态继续运行，行为面可观测）。
            match relay_sock_dup.take() {
                Some(relay_sock) => {
                    // 驱动线程 → relay-leg 线程事件通道（type=3 控制帧；try_send 不阻塞驱动线程）。
                    // **两个来源同灌一条通道**：①腿 socket 上的 kind=3（`bind.set_on_leg_frame`）；
                    // ②公共端口上的 kind=3（中继对**注册腿** Hello 的回执——M5 起注册腿从公共端口
                    // 发出 ⇒ 回执回到公共端口，由 `pubface` 的钩子分派）。
                    let (leg_tx, leg_rx) = mpsc::sync_channel::<super::relayleg::LegEvent>(64);
                    let leg_tx_frame = leg_tx.clone();
                    let configured_relay = arg.addr;
                    bind.set_on_leg_frame(Box::new(move |payload, src| {
                        if src != configured_relay { return; }
                        let _ =
                            leg_tx_frame.try_send(super::relayleg::LegEvent::Frame(payload.to_vec(), src));
                    }));
                    let leg_tx_frame2 = leg_tx.clone();
                    pubface.set_on_leg_frame(Box::new(move |payload, src| {
                        if src != configured_relay { return; }
                        let _ =
                            leg_tx_frame2.try_send(super::relayleg::LegEvent::Frame(payload.to_vec(), src));
                    }));
                    let hint_tx = leg_tx.clone();
                    pubface.set_on_hint(Box::new(move |payload, src| {
                        if src != configured_relay { return; }
                        let _ = hint_tx.try_send(super::relayleg::LegEvent::Hint(payload.to_vec(), src));
                    }));
                    let stop = spawn_service_stop_flag(&mut stop_flags);
                    let leg = super::relayleg::spawn_relay_leg(
                        relay_sock,
                        arg.addr,
                        priv_key.clone(),
                        arg.secret,
                        cmd_tx.clone(),
                        (leg_tx, leg_rx),
                        Arc::clone(&stop),
                        Arc::clone(&logf),
                    );
                    // 控制面（TCP，同号端口）：SESSION/RELEASE 经 EngineCmd 交驱动线程拨/拆腿
                    let ctl = super::relayleg::spawn_relay_ctl(
                        arg.addr,
                        priv_key.clone(),
                        arg.secret,
                        cmd_tx.clone(),
                        Arc::clone(&stop),
                        Arc::clone(&logf),
                    );
                    // 评审 r1 低-1：线程起不全 ⇒ 与「socket 副本缺席」同款处置（token 不
                    // 带中继端点——否则客户端拿到一条静默失效的中继路径；明细行已在
                    // spawn 内部落，这里只摘发布面）。控制面单独失败时注册腿会周期
                    // Hello 但 SESSION 无法建立 ⇒ 中继路径同样不可用，一并摘。
                    let incomplete = leg.is_err() || ctl.is_err();
                    if incomplete {
                        stop.store(true, Ordering::SeqCst);
                        (logf)(
                            "⚠️ --relay 线程起不全（注册腿/控制面 spawn 失败，见上行明细）—— 中继端点不并入 token，出口仅直连形态运行",
                        );
                        relay_ep = None;
                        relay_wanted = false;
                    }
                    for worker in leg.into_iter().chain(ctl) {
                        if incomplete {
                            worker.stop_and_join();
                        } else {
                            relay_workers.push(worker);
                        }
                    }
                }
                None => {
                    (logf)(
                        "⚠️ --relay 注册腿未起（公共端口 socket 副本缺席——端口 bind 失败或副本失败）—— 中继端点不并入 token，出口仅直连形态运行",
                    );
                    relay_ep = None;
                    relay_wanted = false;
                }
            }
        }

        // ---- 驱动线程 ----
        let driver_alive = Arc::new(AtomicBool::new(true));
        let driver_alive_ctor = Arc::clone(&driver_alive); // 构造面（spawn 外）与线程体守卫各持一份
        let driver = std::thread::Builder::new()
            .name("homeway-serve-drv".into())
            .stack_size(1024 * 1024)
            .spawn({
                let cfg = cfg.clone();
                let bind = bind;
                let pubface = Arc::clone(&pubface);
                let mut table = table;
                let mut intercept = intercept;
                let dns = dns.clone();
                let st = Arc::clone(&st);
                let revoked_set = Arc::clone(&revoked_set);
                let dlogf = Arc::clone(&dlogf);
                let itc_stats = Arc::clone(&itc_stats);
                let pub_kick_tx = pub_kick_tx.clone();
                let driver_alive_t = Arc::clone(&driver_alive); // move 闭包独占一份（外层留给兜底线/构造面）
                move || {
                    // 在世守卫：线程体任何出口（含 panic 展开）都清零——supervisor 据此
                    // 判角色终结（守卫必须活在闭包体内——外层 spawn 参数块在闭包构造
                    // 后即退出，放那里 = 装配完成即误报死亡）。
                    struct AliveGuard(Arc<AtomicBool>);
                    impl Drop for AliveGuard {
                        fn drop(&mut self) {
                            self.0.store(false, Ordering::SeqCst);
                        }
                    }
                    let _alive_guard = AliveGuard(driver_alive_t);
                    driver_loop(
                        cfg, bind, pubface, &mut table, &mut intercept, dns.as_ref(), &st,
                        &revoked_set, cmd_rx, &dlogf, &itc_stats, pub_kick_tx, quic_face,
                    );
                }
            })
            .expect("spawn serve driver");

        // ---- 公网端点观测线程 + token 打印 ----
        // P0-4：DDNS 域名配置行（Go serve.go Start 同串）+ 探测全关时的跳过告示。
        if !cfg.ddns.is_empty() {
            (logf)(&format!(
                "DDNS：已配置 {} 个域名（token 叠加域名条目、既有端点全保留；自检随公网端点探测同拍跑）",
                cfg.ddns.len()
            ));
        }
        let ddns_checks: super::ddnscheck::DdnsChecks = Arc::new(
            std::collections::HashMap::new().into(),
        );
        let shared = Arc::new(TokenCtx {
            cfg: cfg.clone(),
            st: Arc::clone(&st),
            secret: secret0,
            backend_priv: priv_key,
            local_port,
            logf: Arc::clone(&logf),
            revoked: Arc::clone(&revoked_set),
            inner: Mutex::new(TokenPrintState::default()),
            relay_ep,
            relay_wanted,
            dlogf: Arc::clone(&dlogf),
            tokf,
            log_paths,
            pinned_flag: Arc::clone(&pinned_flag),
            pubep_stopped: Arc::clone(&pubep_stop),
            publication: Arc::clone(&publication),
            resolver: super::doh::DohResolver::from_list(
                super::doh::parse_list(&cfg.doh),
                Arc::clone(&pin_iface),
                Arc::clone(&logf),
            ),
            ddns_checks: Arc::clone(&ddns_checks),
            ddns_resolvers: cfg.ddns_resolver.iter().copied().map(SocketAddr::V4).collect(),
            quic_ep: quic_brief.as_ref().map(|(a, _)| *a),
            quic_rpk: quic_brief.as_ref().map(|(_, k)| crate::token::RpkPubKey::from(*k.as_bytes())),
        });
        let pub_enabled = cfg.upnp || !cfg.stun.is_empty() || !cfg.public_endpoint.is_empty();
        if pub_enabled {
            let cmd_tx2 = cmd_tx.clone();
            let shared = Arc::clone(&shared);
            let kick_rx = pub_kick_rx;
            if let Ok(handle) = std::thread::Builder::new()
                .name("homeway-pubep".into())
                .spawn(move || {
                    public_endpoint_loop(shared, cmd_tx2, kick_rx, first_probe_tx);
                })
            {
                observers.push(handle);
            }
        } else {
            // 探测全关：发送端关闭，兜底线继续按15s总期限等候（可取消）。
            drop(first_probe_tx);
            drop(pub_kick_rx);
            if !cfg.ddns.is_empty() {
                (logf)(
                    "DDNS：公网端点探测未开（--upnp=false --stun=''），自检没有观测可比对、跳过；token 的域名条目端口按实际监听口",
                );
            }
        }

        // ---- token 兜底线（Go role.go 终端兜底 goroutine 同义）----

        // 兜底等待档（S1 测试缝：HOMEWAY_TOKEN_FALLBACK_WAIT_MS 覆盖——测试注入
        // 短档；生产 15s = Go tokenFallbackWait 同值）。
        let driver_alive_tokfb = Arc::clone(&driver_alive); // 兜底线取消位（轮询驱动在世）
        let fallback_wait = std::env::var("HOMEWAY_TOKEN_FALLBACK_WAIT_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_millis)
            .unwrap_or(Duration::from_secs(15));
        // 兜底线的取消位（低-6：驱动线程退出〔正常 shutdown 或异常终结〕即取消——
        //AliveGuard 清 driver_alive，此处轮询）。
        // 首轮探测结束（成不成都算；探测全关 = 15s 后）每秒重试一小会儿，直到 token
        // 铸出。没有它，公网端点「暂不公布」（端口改写/无证据）的形态下整个进程
        // 生命周期都不会铸 token——重启后台账末行恒缺中继端点（DEPLOY-RUST-EXIT §5
        // 登记缺口：13:07 重启后探测失败 ⇒ token 从未重铸 ⇒ serve token 取到的是
        // 12:40 的无中继版本）。
        {
            let shared = Arc::clone(&shared);
            if let Ok(handle) = std::thread::Builder::new()
                .name("homeway-tokfb".into())
                .spawn(move || {
                    if !wait_first_probe(&first_probe_rx, fallback_wait, &shared.pubep_stopped, &driver_alive_tokfb) {
                        return;
                    }
                    for _ in 0..10 {
                        if shared.pubep_stopped.load(Ordering::SeqCst)
                            || !driver_alive_tokfb.load(Ordering::SeqCst)
                        {
                            return;
                        }
                        let (printed, published) = match shared.inner.lock() {
                            Ok(i) => (!i.last_token.is_empty(), i.last_published.clone()),
                            Err(_) => return,
                        };
                        if printed {
                            return;
                        }
                        print_client_token(&shared, &published);
                        if shared
                            .inner
                            .lock()
                            .map(|i| !i.last_token.is_empty())
                            .unwrap_or(false)
                        {
                            return;
                        }
                        if !wait_observer(&shared.pubep_stopped, Duration::from_secs(1)) {
                            return;
                        }
                    }
                })
            {
                observers.push(handle);
            }
        }

        // ---- udpcap 探测线程（kick = 换网卡后立即重探——绑卡看护 onChange） ----
        let (udpcap_kick_tx, udpcap_kick_rx) = mpsc::sync_channel::<()>(1);
        {
            let cmd_tx2 = cmd_tx.clone();
            let dlogf = Arc::clone(&dlogf);
            let itc_stats = Arc::clone(&itc_stats);
            let dns_targets = cfg.dns_probe_target.clone();
            let stun_targets = cfg.stun_probe_target.clone();
            std::thread::Builder::new()
                .name("homeway-udpcap".into())
                .spawn(move || udpcap_loop(cmd_tx2, itc_stats, &dlogf, udpcap_kick_rx, dns_targets, stun_targets))
                .ok();
        }

        // ---- 绑卡看护循环（Go bindwatch.go；条件同 Go role.go：钉了卡才看护
        //      （auto 挑到 / 显式名），IP 字面量单栈形态不看护）----
        if resolved.is_some() && bind_addr.is_none() {
            // （IP 字面量形态不挂看护——Go role.go `!cfg.BindAddr.IsValid()` 同义）
            let explicit_name = match &cfg.bind_iface {
                // 只有「显式名确实找到了并钉上」才按名重解析；退回 auto 的形态
                // （名字不存在）每轮重挑——评审 r1-M1
                BindMode::Explicit(name)
                    if resolved.as_ref().is_some_and(|r| &r.name == name) =>
                {
                    Some(name.clone())
                }
                _ => None, // auto：每轮重新挑（Go WatchBind Explicit=nil 同义）
            };
            let stop = spawn_service_stop_flag(&mut stop_flags);
            super::bindwatch::spawn_watcher(super::bindwatch::WatcherArgs {
                explicit_name,
                probe_targets: super::bindwatch::health_probe_targets_from_env(&cfg.dns_probe_target),
                pick_targets: super::bindwatch::pick_targets(&cfg.dns_probe_target),
                cmd_tx: cmd_tx.clone(),
                pinned_flag: Arc::clone(&pinned_flag),
                pub_kick: pub_kick_tx.clone(),
                udpcap_kick: udpcap_kick_tx,
                logf: Arc::clone(&logf),
                stop,
            });
        }

        // ---- E19 + E1 ----
        (logf)(&format!(
            "后端身份：标签 {} ｜公钥 {}…",
            hex(&backend_label),
            hex(&backend_pub6)
        ));
        (logf)(&format!(
            "serve 就绪：quic=:{}（配置端口；被占用会自动退让）tunnel={} files={} term={} speedtest={} dns={} tokens={} key={}…",
            quic_listen_port(&cfg),
            cfg.tunnel_ip,
            cfg.files_port,
            cfg.term_port,
            cfg.speedtest_port,
            dns_enabled,
            secrets.len(),
            hex(&backend_pub6)
        ));

        Ok(Arc::new(Self {
            cmd_tx,
            driver: Mutex::new(Some(driver)),
            shutdown_completion: Mutex::new(()),
            driver_alive: driver_alive_ctor,
            pubep_stop,
            publication,
            observer_threads: observers.iter().map(|handle| handle.thread().id()).collect(),
            observers: Mutex::new(observers),
            pub_kick: pub_kick_tx,
            relay_workers: Mutex::new(relay_workers),
            itc_stats: Arc::clone(&itc_stats),
            local_port,
            quic_local_addr: quic_brief.as_ref().map(|(a, _)| *a),
            quic_rpk_public_key: quic_brief.as_ref().map(|(_, k)| *k),
            quic_stats,
            stop_flags,
            #[cfg(not(windows))]
            term_srv,
            state_dir: cfg.state_dir.clone(),
            serve_dir,
            socks,
            ddns: cfg.ddns.clone(),
            ddns_checks,
        }))
    }

    /// 有序收工。观察线程内日志回调只能请求停止；外部调用返回才确认所有资源已释放。
    pub fn shutdown(&self, grace: Duration) {
        if self.on_observer_thread() {
            self.pubep_stop.store(true, Ordering::SeqCst);
            for flag in &self.stop_flags {
                flag.store(true, Ordering::SeqCst);
            }
            let _ = self.cmd_tx.send(EngineCmd::Stop { grace });
            return;
        }
        let _completion = self.shutdown_completion.lock().expect("收工锁中毒");
        self.stop_public_observation();
        for f in &self.stop_flags {
            f.store(true, Ordering::SeqCst); // ② UDS listeners（accept 循环下一拍退出）
        }
        // term：全部会话 ENDED(service_stopped) + 子进程收尸（UDS listener 由 stop_flags 收）
        #[cfg(not(windows))]
        if let Some(t) = &self.term_srv {
            t.close();
        }
        let _ = self.cmd_tx.send(EngineCmd::Stop { grace }); // ①③④ 驱动线程内按序执行
                                                             // ⑤ sock 文件清理（身份比对——只删自己 bind 出来的那个）
        let handle = self.driver.lock().expect("驱动句柄锁中毒").take();
        if let Some(h) = handle {
            let _ = h.join();
        }
        for worker in std::mem::take(&mut *self.relay_workers.lock().expect("中继句柄锁中毒")) {
            worker.stop_and_join();
        }
        for (path, own) in &self.socks {
            remove_sock_own(path, *own);
        }
    }

    fn on_observer_thread(&self) -> bool {
        self.observer_threads.contains(&std::thread::current().id())
    }

    /// 外部调用：停止并等待观测/兜底退出，缩租前必须完成；观察回调重入时仅请求停止。
    pub fn stop_public_observation(&self) {
        self.pubep_stop.store(true, Ordering::SeqCst);
        // 外部调用可能正持有句柄锁等待本线程，观察回调不得等该锁或join自己。
        if self.on_observer_thread() {
            return;
        }
        // 与发布提交线性化；日志回调始终在门锁外，允许回调触发 shutdown。
        drop(self.publication.lock().expect("发布门锁中毒"));
        let _ = self.pub_kick.try_send(());
        let mut observers = self.observers.lock().expect("观测句柄锁中毒");
        // 串行调用必须等到上一位完成join，不能把“句柄已取走”误作“线程已退出”。
        for handle in observers.drain(..) {
            let _ = handle.join();
        }
    }



    /// 同监听 socket 的 STUN 观测（公网端点线程/测试面用）。
    pub fn stun_query(&self, server: SocketAddr, timeout: Duration) -> Option<SocketAddr> {
        let (tx, rx) = mpsc::channel();
        if self.cmd_tx.send(EngineCmd::StunQuery { server, reply: tx }).is_err() {
            return None;
        }
        match rx.recv_timeout(timeout) {
            Ok(v) => v,
            Err(_) => {
                let _ = self.cmd_tx.send(EngineCmd::StunAbort);
                None
            }
        }
    }

    /// 公网端点立即重测（换网事件）。
    pub fn kick_public_endpoint(&self) {
        let _ = self.cmd_tx.send(EngineCmd::KickPublicEndpoint);
    }

    /// 出口 QUIC 面计数快照（M3 S2；`None` = 本世代面未起）。**原子直读**、不占驱动线程
    /// （与 `itc_stats` 同款观测缝），供 `serve status --json` 的 `quic` 段。
    pub fn quic_snapshot(&self) -> Option<homeway_quic::ExitQuicSnapshot> {
        self.quic_stats.as_ref().map(|h| h.snapshot())
    }

    /// 驱动线程在世位（false = 已停——正常 shutdown 或异常终结；统一进程 supervisor
    /// 的运行期失败判据）。
    pub fn alive(&self) -> bool {
        self.driver_alive.load(Ordering::SeqCst)
    }

    /// serve.status 观测面（B0-2b 缝）：peers 经驱动线程快照（设备表驱动独占），
    /// 拦截计数共享原子直读；ddns 自检快照读共享状态表。引擎已停/驱动不应答 =
    /// 空表 + 零计数（观测面不复活角色）。
    pub fn status_bits(
        &self,
    ) -> (
        Vec<EnginePeerBrief>,
        EngineInterceptBits,
        Vec<super::ddnscheck::DdnsBrief>,
    ) {
        let peers = (|| {
            let (tx, rx) = mpsc::channel();
            self.cmd_tx.send(EngineCmd::StatusQuery { reply: tx }).ok()?;
            rx.recv_timeout(Duration::from_secs(2)).ok()
        })()
        .unwrap_or_default();
        let s = self.itc_stats.snapshot();
        let get = |k: &str| s.iter().find(|(n, _)| *n == k).map(|(_, v)| *v).unwrap_or(0);
        let ddns = super::ddnscheck::briefs(&self.ddns_checks, &self.ddns);
        (
            peers,
            EngineInterceptBits {
                dial_ok: get("dialok"),
                dial_fail: get("dialfail"),
                reject: get("rejected"),
                flows: get("flows"),
                udp_drop: get("udpDrop"),
                shape_drop: get("shapeDrop"),
                frag_drop: get("fragDrop"),
                frag_reasm: get("fragReasm"),
                frag_bad: get("fragBad"),
                frag_overlap: get("fragOverlap"),
                frag_timeout: get("fragTimeout"),
                frag_limit: get("fragLimit"),
                tx_frag_drop: get("txFragDrop"),
            },
            ddns,
        )
    }

}

impl Drop for ServeEngine {
    fn drop(&mut self) {
        // 防御：未显式 shutdown 的 drop 也走一遍停（幂等——Stop 后驱动线程已退出）
        self.shutdown(Duration::from_secs(2));
    }
}

/// UPnP 退出缩租（shrinkUPnPClease：把映射租期缩到 5 分钟——快速重启沿用同一外口，
/// 出口真退休了映射自动过期；装配早失败时没有要缩租的映射）。
/// F10：**全局 8s 预算**（Go serve.go:728 单 ctx）——期限穿透到 SSDP/描述腿，候选循环
/// 共享同一 deadline（修前每候选 5s(SSDP) + 8s(SOAP) ⇒ 停机最坏 N×13s）；结果显式
/// 归因（静默跳过变可见）。
pub fn shrink_upnp_lease(engine: &ServeEngine, logf: &Logf) {
    let port = engine.local_port;
    let cands = crate::server::upnp::local_ipv4_candidates();
    let deadline = std::time::Instant::now() + crate::server::upnp::UPNP_SHRINK_TOTAL_BUDGET;
    match crate::server::upnp::shrink_lease_before(&cands, port, deadline, &**logf) {
        crate::server::upnp::ShrinkOutcome::Shrunk { ext } => {
            (logf)(&format!("UPnP：退出前把映射 外部 {ext} 的租期缩到 5 分钟（快速重启仍会沿用这个端口）"));
        }
        crate::server::upnp::ShrinkOutcome::NoIgd => {
            (logf)("UPnP：缩租跳过——8s 预算内没找到可用 IGD（映射保持原租期，自动过期）");
        }
        crate::server::upnp::ShrinkOutcome::TableIncomplete => {
            (logf)("UPnP：缩租跳过——映射表枚举残缺（清单不可信，不当作「没有映射」；映射保持原租期）");
        }
        crate::server::upnp::ShrinkOutcome::NoMapping => {
            (logf)("UPnP：缩租跳过——路由器表里没有我们的映射");
        }
        crate::server::upnp::ShrinkOutcome::Failed { ext, err } => {
            (logf)(&format!("UPnP：缩租失败（外部 {ext}：{err}）——原映射仍在，租期到期自动过期"));
        }
    }
}

fn io_other(e: crate::server::state::StateError) -> std::io::Error {
    std::io::Error::other(e.to_string())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn tok_endpoint_refs(eps: &[Endpoint]) -> Vec<crate::token::EndpointRef<'_>> {
    eps.iter()
        .map(|e| crate::token::EndpointRef { addr: e.addr.as_str(), kind: e.kind })
        .collect()
}

/// 后端的中继标签 = SHA-256(静态公钥)[:8]（proto.RelayID 同义）。
fn backend_label(priv_key: &x25519_dalek::StaticSecret) -> [u8; 8] {
    use sha2::Digest;
    let sum = sha2::Sha256::digest(x25519_dalek::PublicKey::from(priv_key).as_bytes());
    let mut out = [0u8; 8];
    out.copy_from_slice(&sum[..8]);
    out
}

fn iface_state_str(i: &IfaceInfo) -> String {
    // Go stateOf 形态：`index=%d up addrs=[ip/prefix,…]`（排序——评审 M5）
    let mut addrs = i.cidrs.clone();
    if addrs.is_empty() {
        addrs = i.addrs.iter().map(|a| a.to_string()).collect();
    }
    addrs.sort();
    format!("index={} {} addrs=[{}]", i.index, if i.up { "up" } else { "down" }, addrs.join(","))
}

/// sock 文件的 dev/ino 属主标识（removeSockOwn 的「只删自己 bind 出来的那个」比对面）。
/// WIN 批：unix 专属——windows 的 TCP IPC 无 sock 文件（socks 表该侧恒 (0,0)，收工
/// 走「删不存在路径、NotFound 容错」的无害路径）。
#[cfg(unix)]
fn sock_identity(p: &std::path::Path) -> (u64, u64) {
    std::fs::metadata(p).map(|m| (m.dev(), m.ino())).unwrap_or((0, 0))
}

/// 只删「还是自己 bind 出来的那个文件」（removeSockOwn；路径被接管时不动它）。
fn remove_sock_own(path: &std::path::Path, own: (u64, u64)) {
    if own == (0, 0) {
        let _ = std::fs::remove_file(path);
        return;
    }
    // WIN 批：dev/ino 比对是 unix 专属面（windows 恒走上面的 (0,0) 臂）。
    #[cfg(unix)]
    if let Ok(md) = std::fs::metadata(path) {
        if (md.dev(), md.ino()) == own {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 服务入口装配（M3 S2，设计 §2.2 方案 B′）：UDS 监听器 → [`homeway_quic::ServiceIntake`]
/// （两源）+ 出口侧入队句柄。`capacity` = 该服务在册上限 + K。
///
/// 失败（自唤醒管道/权限面，理论面）⇒ 与「监听失败」同一取舍：**该服务整条腿停用**
/// （记行 + 返回 `None`，调用方关掉服务对象）——`0x22`（服务不可用）才是如实观测
/// （设计门 N16 的保守面：不做「静默降级成只有 UDS」）。
/// DNS 流腿（`STREAM[tag=6]`）的入口装配（**DNS-STREAM 批；代码门 r34 M2③ 的可测面**）：
/// `dns = None`（DNS 未启用）或入口建不起来 ⇒ `None`（该 tag 落既有 `reset(0x22)`）；
/// 可用性判据 = `dns_enabled`，**与 `dns_port` 取值无关**（tag 不携带端口）。
fn dns_intake_of(
    dns: Option<&Arc<DnsProxy>>,
    logf: &Logf,
) -> Option<(homeway_quic::ServiceIntake, homeway_quic::ServiceIntakeTx)> {
    dns?;
    match homeway_quic::ServiceIntake::quic_only(
        homeway_quic::tuning::service_defaults::intake_capacity(crate::server::dns_stream::MAX_CONNS),
    ) {
        Ok((intake, tx)) => {
            (logf)(&format!(
                "dns 流腿就绪：STREAM tag=6（客户端虚拟端口 {}；代答复用 :53 同一上游 dnsproxy）",
                crate::facade::bridge_io::port::DNS_RESOLVE
            ));
            Some((intake, tx))
        }
        Err(_e) => {
            // 入口建不起来（fd/内存）：该 tag 落 0x22（与 files/speedtest 的保守选择同款）
            (logf)("⚠️ dns: 流腿入口起不来——socks 域名目标不可用（该 tag 落 0x22）");
            None
        }
    }
}

/// intake 装配（unix 两源 = UDS 调试腿 + QUIC；windows QUIC-only——评审 M2 后的
/// 显式双签名：windows 侧不再 bind 本地口〔修前 bind 后即丢、端口文件残留〕）。
#[cfg(unix)]
fn service_intake(
    ln: crate::daemon::ipc::IpcListener,
    capacity: usize,
    tag: &str,
    logf: &Logf,
) -> Option<(homeway_quic::ServiceIntake, homeway_quic::ServiceIntakeTx)> {
    let crate::daemon::ipc::IpcListener::Unix(ln) = ln;
    service_intake_built(homeway_quic::ServiceIntake::with_quic(ln, capacity), tag, logf)
}

#[cfg(windows)]
fn service_intake(
    capacity: usize,
    tag: &str,
    logf: &Logf,
) -> Option<(homeway_quic::ServiceIntake, homeway_quic::ServiceIntakeTx)> {
    service_intake_built(homeway_quic::ServiceIntake::quic_only(capacity), tag, logf)
}

fn service_intake_built(
    built: std::io::Result<(homeway_quic::ServiceIntake, homeway_quic::ServiceIntakeTx)>,
    tag: &str,
    logf: &Logf,
) -> Option<(homeway_quic::ServiceIntake, homeway_quic::ServiceIntakeTx)> {
    match built {
        Ok((intake, tx)) => Some((intake, tx)),
        Err(e) => {
            (*logf)(&format!(
                "⚠️ {tag}: 服务入口起不来（{e}）—— 该服务不可用（QUIC 档与该服务的本机入口一起停用），直到重启出口"
            ));
            None
        }
    }
}

fn spawn_service_stop_flag(flags: &mut Vec<Arc<AtomicBool>>) -> Arc<AtomicBool> {
    let f = Arc::new(AtomicBool::new(false));
    flags.push(Arc::clone(&f));
    f
}

// ---------- 驱动循环（WG 驱动线程独占：device/拦截栈/设备表/ServerBind） ----------

#[allow(clippy::too_many_arguments)] // 装配线程一次性移交全驱动状态（线程起点单参化无收益）
fn driver_loop(
    cfg: ServeConfig,
    mut bind: ServerBind,
    pubface: Arc<PubFace>,
    table: &mut DeviceTable,
    intercept: &mut Interceptor,
    dns: Option<&Arc<DnsProxy>>,
    st: &State,
    revoked_set: &Arc<Mutex<HashSet<[u8; 32]>>>,
    cmd_rx: mpsc::Receiver<EngineCmd>,
    dlogf: &Logf,
    _itc_stats: &Arc<ItcStats>,
    pub_kick_tx: std::sync::mpsc::SyncSender<()>,
    // 出口 QUIC 面句柄（`None` = 未起）：**收工链在驱动线程手里**（M1 §1.7——关闭前
    // 还要把尾包投给 intercept，故必须停在 `intercept.close()` **之前**）。
    quic_face: Option<homeway_quic::ExitQuic>,
) {
    // GC 节拍（10min ±10% 抖动）与吊销跟随（1s mtime）/DNS 统计（60s）
    let mut rng = [0u8; 8];
    getrandom::getrandom(&mut rng).expect("系统随机源不可用");
    let gc_jitter = u64::from_le_bytes(rng) % (cfg.peer_ttl.as_secs().clamp(1, 3600));
    let mut next_gc = Instant::now() + cfg.peer_ttl + Duration::from_secs(gc_jitter % 600);
    if cfg.peer_ttl.is_zero() {
        next_gc = Instant::now() + Duration::from_secs(3600 * 24); // TTL 关：不跑 GC
    }
    let mut last_revoked_check = Instant::now();
    let mut last_leg_sweep = Instant::now();
    let revoked_path = cfg.state_dir.join("serve").join("revoked.jsonl");
    let mut revoked_mtime = std::fs::metadata(&revoked_path).and_then(|m| m.modified()).ok();
    let mut last_dns_stats = Instant::now();
    // tun_ip → pubkey 映射（M5 §3.2-bis 方案 (a)）：**出口出站分流键**。数据源 = 设备表
    // 的 `DevOp`（Add 带 `tun_ip`/pubkey；Remove 带 pubkey）——原 WG 面 `device.tun_ip_owner`
    // 的等价物，**不动岛公面**。dst == 某设备 tun_ip ⇒ QUIC DATAGRAM；否则丢 + 计数 + 记行。
    let mut pub_by_tun: std::collections::HashMap<Ipv4Addr, [u8; 32]> = std::collections::HashMap::new();
    let mut unbound_dropped: u64 = 0;
    let mut stop = false;
    let mut stop_grace = STOP_GRACE;
    // 「出口面线程已死」只记一次（否则每拍一行）
    let mut quic_dead_logged = false;
    while !stop {
        // ① 命令
        while let Ok(cmd) = cmd_rx.try_recv() {
            match cmd {
                EngineCmd::Stop { grace } => {
                    stop_grace = grace;
                    stop = true;
                }
                EngineCmd::StatusQuery { reply } => {
                    let briefs = table
                        .briefs()
                        .into_iter()
                        .map(|(dev, tunnel_ip, last_reg_ms, idle_ms)| EnginePeerBrief {
                            dev,
                            tunnel_ip,
                            last_reg_ms,
                            idle_ms,
                        })
                        .collect();
                    let _ = reply.send(briefs);
                }
                EngineCmd::StunQuery { server, reply } => {
                    // 公共端口（QUIC 面直连 socket）上的观测（M5 S3a）。面未起 = 无公共端口
                    // ⇒ 直接回 None（外层打「观测失败」行）；已有查询在等 ⇒ 同 None。
                    match (pubface.stun_begin(reply), quic_face.as_ref()) {
                        (Some(txid), Some(q)) => {
                            let req = crate::server::egress::stun_request(&txid, "");
                            if !q.send_plain(server, &req) {
                                pubface.stun_abort();
                            }
                        }
                        (Some(_), None) => {
                            // 无公共端口：把占位撤掉并回 None（不悬着下一次查询）
                            pubface.stun_abort();
                            (dlogf)("STUN：观测不可用 —— 公共端口未起（QUIC 面缺席）");
                        }
                        (None, _) => {} // 已有一次在等：直接失败（观测周期分钟级）
                    }
                }
                EngineCmd::StunAbort => pubface.stun_abort(),
                EngineCmd::SetCaps(c) => pubface.set_caps(c),
                EngineCmd::SetProbeEndpoints(eps) => pubface.set_probe_endpoints(eps),
                EngineCmd::KickPublicEndpoint => {
                    let _ = pub_kick_tx.try_send(()); // 转发到观测线程（满=已有待处理 kick）
                }
                EngineCmd::Repin { index, name, reply } => {
                    // 看护循环重钉（公共端口 socket——dup fd 上 setsockopt；两族都设、单栈容错）
                    let _ = reply.send(pubface.repin_to(index, &name));
                }
                EngineCmd::LegRegister { id, remote, marker, admission_marker } => {
                    // SESSION 通告 → 拨腿（连接 socket + LEGUP 标记 + 入表）
                    if let Err(e) = bind.register_leg_with_admission(id, remote, &marker, &admission_marker) {
                        (dlogf)(&format!("中继控制面：会话 #{id} 拨腿失败（→ {remote}）：{e}"));
                    }
                }
                EngineCmd::LegRemove { id } => bind.remove_leg(id),
                EngineCmd::LegsClear => bind.clear_legs(),
            }
        }
        // 腿表 → QUIC 面（M1 §1.6）：命令面刚改过腿表 ⇒ 同拍同步（新腿的回程包不能等下一拍）
        sync_quic_legs(&mut bind, quic_face.as_ref());
        // ② UDP 收包（等待原语两档；reactor 批起 = wait_hint 扩档）：
        //    - 整形滞留非空 ∨ reactor 存在等待者（connect 在途/待写缓冲非空）：1ms 拍
        //     （每拍续水 rate×1ms、线上团块细化；上游事件最晚 1ms 被发现）；
        //    - 否则：5ms 常规拍（不加空转唤醒成本）。
        //    腿 fd 同轮在等待集（R4：控制面通告建立的腿与主 socket 同构收包）。
        // 负 fd 防御（r2-2.4：理论上不可达——腿 fd 只取活 socket——防御位）
        let leg_fds: Vec<crate::sysock::RawSock> = bind
            .leg_fds()
            .into_iter()
            .filter(|fd| crate::sysock::is_valid(*fd))
            .collect();
        // QUIC 面唤醒 fd（M1 §1.4：入站队列非空即可读——没有它每个入站包最多等 5ms，
        // TCP RTT/吞吐直接受损）。所有权在 `quic_face`，本循环只 poll + 交给它 drain。
        let quic_wake_fd: Option<crate::sysock::RawSock> =
            quic_face.as_ref().map(|q| q.wake_fd());
        // 等待集 = 腿 fd + QUIC 面唤醒 fd（M5 起**不再有**公共端口的 poll 项：公共端口由
        // QUIC 面的 reactor 独占；探针/观测经其明文钩子在那边消化）。
        // WIN 批：poll 集经 `sysock`（unix = libc::poll 原语义；windows = WSAPoll）。
        let mut pollfds: Vec<crate::sysock::PollEnt> =
            Vec::with_capacity(leg_fds.len() + usize::from(quic_wake_fd.is_some()));
        for fd in &leg_fds {
            pollfds.push(crate::sysock::PollEnt { raw: *fd, events: crate::sysock::POLLIN, revents: 0 });
        }
        let quic_poll_idx = quic_wake_fd.map(|fd| {
            pollfds.push(crate::sysock::PollEnt { raw: fd, events: crate::sysock::POLLIN, revents: 0 });
            pollfds.len() - 1
        });
        let poll_ms: i32 = match intercept.wait_hint() {
            Some(_) => 1,
            None => 5,
        };
        if let Err(e) = crate::sysock::poll_slice_timeout(&mut pollfds, poll_ms) {
            if e.kind() != std::io::ErrorKind::Interrupted {
                (dlogf)(&format!("serve: poll 错误（{e}）—— 继续循环"));
            }
        }
        let readable_legs: Vec<crate::sysock::RawSock> = pollfds[..leg_fds.len()]
            .iter()
            .filter(|pf| pf.revents & (crate::sysock::POLLIN | crate::sysock::POLLERR | crate::sysock::POLLHUP) != 0)
            .map(|pf| pf.raw)
            .collect();
        // 腿 fd 可读：kind=5（QUIC 载荷）交出口 QUIC 面注入；kind=3 已由 bind 的钩子转
        // 注册腿线程（M5 后腿面上只有这两类）
        for fd in readable_legs {
            let (_alive, inbound) = bind.leg_readable(fd);
            if let Some(inbound) = inbound {
                let pkt = inbound.quic;
                let n = pkt.payload.len();
                match quic_face.as_ref() {
                    Some(q) if match pkt.generation {
                        Some(generation) => q.inject_leg_generation(pkt.src, pkt.payload, generation),
                        None => q.inject_leg(pkt.src, pkt.payload),
                    } => {}
                    _ => bind.note_quic_leg_undelivered(n),
                }
            }
        }
        // 腿读错误会摘腿 ⇒ 同拍把摘除同步给 QUIC 面（面侧保留「最近摘除」窗）
        sync_quic_legs(&mut bind, quic_face.as_ref());
        // QUIC 入站（M1 §1.3/§1.4）：poll 唤醒后 drain（唤醒字节 + 入站队列）——
        // 准入请求在此裁决（回执非阻塞）、明文包直投 `intercept.on_plain`。
        if let Some(q) = quic_face.as_ref() {
            if quic_poll_idx.is_some_and(|i| pollfds[i].revents & crate::sysock::POLLIN != 0) {
                q.drain_inbound(|item| {
                    if let Some((remote, generation)) = on_quic_inbound(item, table, intercept, Some(q), &mut pub_by_tun, dlogf) {
                        bind.confirm_client(remote, generation);
                    }
                });
            }
            // 出口面线程死（panic/异常退出）= 引擎侧可观测的「不健康」：**记一次行**
            // （M5 起无回落面：出站按无绑定丢弃 + 计数，入站停止——设备需重连）。
            if q.is_finished() && !quic_dead_logged {
                quic_dead_logged = true;
                (dlogf)("出口 QUIC 面线程已退出 —— 公共端口随之失效：后续出站按无绑定丢弃（计数可见）、入站停止（客户端需重连）");
            }
        }
        // ③ 拦截拍（worker 事件 + DNS 应答回投 + 栈 poll + TX）。P1 两级前置背压：
        // 发送 ring 高水位 ⇒ 本拍整形释放退化「只并入不释放」（包留整形 FIFO =
        // 真背压不丢包，满丢成为最后兜底）。
        let tx = intercept.pump();
        // ④ 出站分流（M5 §3.2-bis：dst == 设备 tun_ip ⇒ QUIC DATAGRAM；无绑定 ⇒ 丢 +
        //    计数 + 节流记行——原「WG 兜底」面无替代、不静默）
        route_out(&tx, &pub_by_tun, quic_face.as_ref(), &mut unbound_dropped, dlogf);
        // 真准入确认走可靠有界重发；LEGUP 只认证出口，不能提升匿名客户端。
        bind.retry_client_admissions();
        // ⑤ 周期任务
        let now = Instant::now();
        if now >= next_gc {
            let sys_now = SystemTime::now();
            for (_a, ops) in table.gc(sys_now) {
                apply_dev_ops(ops, &mut pub_by_tun, quic_face.as_ref());
            }
            let jitter = (std::process::id() as u64 * 7919) % 60;
            next_gc = now + cfg.peer_ttl + Duration::from_secs(jitter);
        }
        if now.duration_since(last_leg_sweep) > crate::server::bind::RELAY_LEG_SWEEP_PUB {
            last_leg_sweep = now;
            bind.sweep_legs();
            // 空闲回收摘了腿 ⇒ 同步给 QUIC 面（同拍）
            sync_quic_legs(&mut bind, quic_face.as_ref());
        }
        if now.duration_since(last_revoked_check) > Duration::from_secs(1) {
            last_revoked_check = now;
            let m = std::fs::metadata(&revoked_path).and_then(|m| m.modified()).ok();
            if m != revoked_mtime {
                revoked_mtime = m;
                match st.revoked_secrets() {
                    Ok(set) => {
                        *revoked_set.lock().expect("吊销表锁中毒") = set;
                    }
                    Err(e) => {
                        (dlogf)(&format!("serve: 吊销表读取失败（{e}）—— 保留旧集合"));
                    }
                }
            }
        }
        if let Some(d) = dns {
            if now.duration_since(last_dns_stats) > Duration::from_secs(60) {
                last_dns_stats = now;
                (dlogf)(&d.stats_line());
            }
        }
    }
    // ---- 收工（D5：① 已由 Stop 置位；这里 ③④——drain 的出站包照走 encap 链
    //      （评审 M2：宽限窗口的 FIN/ACK/尾数据丢弃 = 存量连接无法自然收销账）；
    //      ⑤ 的 UPnP 缩租在 shutdown 侧面）----
    bind.shutdown_legs();
    intercept.halt_new();
    let deadline = Instant::now() + stop_grace;
    loop {
        let tx = intercept.pump_grace(deadline);
        // 宽限期尾包照走 QUIC 出站分流（WG 兜底面已退役——无绑定即丢弃 + 计数）
        route_out(&tx, &pub_by_tun, quic_face.as_ref(), &mut unbound_dropped, dlogf);
        // 评审 r2-1.1 兜底：滞留未清空不提前收摊（流表空但尾数据还在整形队列——
        // 主修在 pump_grace 的全量释放面，这里是宽限循环侧的第二道闸）。
        if (intercept.flow_count() == 0 && !intercept.tx_deferred_pending())
            || Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // QUIC 面收工（M1 §1.7 的收工链：`bind.shutdown_legs()` → `intercept.halt_new()`
    // → **QUIC 面 stop_within(CLOSE_BUDGET)** → `intercept.close()`）。位置：宽限循环
    // 之后（尾包该投的已投）、intercept teardown 之前；到点 detach 由收割线程接手
    // （不阻塞收工链）。
    if let Some(q) = quic_face {
        const QUIC_CLOSE_BUDGET: Duration = Duration::from_secs(2);
        if !q.stop_within(Instant::now() + QUIC_CLOSE_BUDGET) {
            (dlogf)("出口 QUIC 面未在收工预算内退出 —— 已 detach（收割线程接手）");
        }
    }
    // 到期：teardown（在途 TCP 立即拆——close 内逐条 teardown + 判据行）
    intercept.close();
}

/// 腿表 → QUIC 面腿表的**差分同步**（M1 §1.6 发送侧路由）：新腿登记发送句柄（该腿
/// socket 的 `try_clone` 副本）、已摘的腿从 QUIC 面摘掉（面侧保留「最近摘除」窗 ⇒
/// 不再回落直连端口）。
///
/// 调点（三处，每处都是「腿表刚被改过」之后）：①命令面处理完（LegRegister/LegRemove/
/// LegsClear）——同拍生效，消掉「腿刚拨好、QUIC 回程还找不到腿」的窗口；②腿读循环后
/// （读错误会摘腿）；③`bind.sweep_legs()` 后（空闲回收）。开销 = 腿表 ≤64 条的小遍历。
fn sync_quic_legs(bind: &mut ServerBind, quic: Option<&homeway_quic::ExitQuic>) {
    let Some(q) = quic else { return };
    let cur = bind.leg_remotes();
    for remote in q.leg_remotes() {
        if !cur.contains(&remote) {
            q.leg_close(remote);
        }
    }
    for remote in cur {
        if let Some((generation, local)) = bind.leg_generation(remote) {
            if !q.leg_matches(remote, local, generation) {
                if let Some(sock) = bind.leg_send_handle(&remote) { q.leg_open_generation(remote, sock, generation); }
            }
        } else if !q.has_leg(&remote) {
            if let Some(sock) = bind.leg_send_handle(&remote) { q.leg_open(remote, sock); }
        }
    }
}

/// 出口 QUIC 面的入站事件消化（**引擎线程内**；M2 设计 §1.4 的接线点）。
///
/// - `Reg`：准入/刷新裁决（`hr-reg4` 连接绑定 MAC → **原样走 `table.register`**）并**必须回执**
///   （回执非阻塞：oneshot `send` 是普通内存操作；拒绝原因**类型化**回传——r14 F7）；
/// - `Packet`：源校验已在出口面做完 ⇒ 直投 `intercept.on_plain`（M5 起这是**唯一**的
///   内层明文入口——WG 面的 `device.decapsulate → on_plain` 路径已随 WG 删除）。
fn on_quic_inbound(
    item: homeway_quic::ExitInbound,
    table: &mut DeviceTable,
    intercept: &mut Interceptor,
    quic: Option<&homeway_quic::ExitQuic>,
    tun_owner: &mut std::collections::HashMap<Ipv4Addr, [u8; 32]>,
    dlogf: &Logf,
) -> Option<(SocketAddr, homeway_quic::RelayGeneration)> {
    match item {
        homeway_quic::ExitInbound::Reg(req) => {
            let verdict = admit_reg4(&req.frame, &req.exporter, table, quic, tun_owner, dlogf);
            let accepted = matches!(verdict, homeway_quic::Reg4Verdict::Accepted { .. });
            let origin = req.remote.zip(req.relay_generation);
            req.reply(verdict);
            if accepted { return origin; }
        }
        homeway_quic::ExitInbound::Packet { pkt, .. } => intercept.on_plain(pkt),
        // 前向兼容（`ExitInbound` 是 `#[non_exhaustive]` 的跨 crate 消费面）
        _ => {}
    }
    None
}

/// 准入裁决（M2 设计 §1.4 步骤 4.2）：`hr-reg4` 两帧（Proof / 刷新帧）的 MAC（连接绑定）
/// 校验 ⇒ **用命中的 secret 重建 v2 报文 ⇒ `table.register`**。
///
/// 为什么重建 v2 而不是给表加「已验」入口：`register` 的语义（时间窗 ±90s / 吊销跟随 /
/// 表满淘汰 / 地址冲突 / `peer: +`·`~`·`-` 判据行与拒绝计数）必须**逐字不变**——重建报文
/// 让这些全部走原路径，`hr-reg4` 只多一层「哪条 secret 认得这一帧、且这一帧绑在本连接上」。
///
/// **刷新帧多一道前置**（S2-2 / r14 F25）：`Refresh` 帧必须**表内仍在册**才继续（`device_addrs`
/// 非空）——防「刷新帧跨过淘汰把设备 resurrect」；`Proof` 帧不走本道（完整准入 = 合法复位）。
///
/// **拒绝原因类型化**（r14 F7）：MAC 试秘在本函数内，出口面拿不到原因 ⇒ 由 verdict 携带
/// （否则出口面打不出 `hr-reg4 MAC 不符` 这条归因行）。两类落点（r14 F1）：
/// `MacMismatch` ⇒ 出口面 `hr-reg4 MAC 不符`；`EngineRejected` ⇒ 出口面 `引擎裁决拒绝` +
/// **表内自己的归因行**（`no-token`/`revoked`/`table-full`/`ip-conflict`；`ts` 超 ±90s 窗
/// 属于这一类——MAC 验过、窗拒，表内打 `reason=no-token`）。
fn admit_reg4(
    frame: &homeway_quic::Reg4Frame,
    exporter: &[u8; 32],
    table: &mut DeviceTable,
    quic: Option<&homeway_quic::ExitQuic>,
    tun_owner: &mut std::collections::HashMap<Ipv4Addr, [u8; 32]>,
    dlogf: &Logf,
) -> homeway_quic::Reg4Verdict {
    use homeway_quic::{Reg4Frame, Reg4Verdict, RejectWhy};

    // **刷新帧的第三道前置（设计 §1.4 步骤 6 ② / 设计门 r14 F25）：表内仍在册。**
    // 已淘汰（TTL/表满/显式摘除）设备的刷新帧若照走 `table.register`，表里一旦有空位它就会
    // 把设备重新 `Added`——**resurrect**：淘汰语义被一个刷新帧抹平。故本道在 MAC 试秘之前
    // （先问「还有没有这个设备」，再问「是不是你」），且拒绝**不落表、不进表内拒绝计数**
    // （该设备的淘汰归因已由 TTL/表满路径打过，重复计数会污染 E9 的输入集）。
    // 对照：完整准入（`Proof`）不受本道约束——那是**合法复位**路径（重新走四帧 + 判定）。
    if matches!(frame, Reg4Frame::Refresh(_)) && table.device_addrs(&frame.dev_tag()).is_none() {
        return Reg4Verdict::Rejected { why: RejectWhy::RefreshNotRegistered };
    }

    let Some(secret) = table.match_proof(frame, exporter) else {
        // MAC 不符：不是本出口的 token，或**同一帧换了连接**（重放——exporter 变了）。
        // 两种情形同面（不可区分，也不该区分）：都拒。
        (dlogf)(&format!(
            "准入被拒（dev={} pub={}；hr-reg4 MAC 不符——含换连接重放）",
            dev_short(&frame.dev_tag()),
            pub_short(&frame.pubkey())
        ));
        return Reg4Verdict::Rejected { why: RejectWhy::MacMismatch };
    };
    let mut reg_pkt = Vec::with_capacity(reg2::REG_LEN);
    reg2::encode_reg_parts(
        &crate::token::Secret::from(secret),
        &frame.pubkey(),
        &frame.dev_tag(),
        frame.ts(),
        &mut reg_pkt,
    );
    match table.register(&reg_pkt, SystemTime::now()) {
        Ok((_action, ops)) => {
            apply_dev_ops(ops, tun_owner, quic);
            match table.device_addrs(&frame.dev_tag()) {
                Some((tunnel_ip, tun_ip)) => Reg4Verdict::Accepted { tunnel_ip, tun_ip },
                // 理论不可达（刚注册成功必在表内）；真出现即拒（宁可让客户端重连）。
                // 归资源桶（M3 §4：这是出口侧的内部不一致，重试即可能成功）
                None => Reg4Verdict::Rejected {
                    why: RejectWhy::EngineRejected {
                        class: homeway_quic::EngineRejectClass::Resource,
                    },
                },
            }
        }
        // 表内已打拒绝归因行 + 计数（no-token/revoked/table-full/ip-conflict），此处不重复；
        // **M3 §4**：按 `RejectReason` 归桶（凭证面/资源面）随 verdict 带回出口面 ⇒ 关闭码
        // 能分「token 不对/被吊销」与「表满/地址冲突」（客户端据此给可行动文案）。
        Err(reason) => Reg4Verdict::Rejected {
            why: RejectWhy::EngineRejected {
                class: reason.engine_class(),
            },
        },
    }
}

/// 设备表变更 → 出口侧两份状态（M5 S3b：原 WG `Device` 装配点）：
///
/// - `tun_owner`：**出站分流键**（§3.2-bis 方案 (a)）——`tun_ip → pubkey`；
/// - QUIC 面的设备绑定（`unbind_pub`）：设备离表后它的绑定与连接一并拆（回程不得再发往
///   已摘除的设备；重登记会重新绑定）。
///
/// 恒等入口：`table.register`（准入）与 `table.gc`（淘汰）两处 ops 都经这里。
fn apply_dev_ops(
    ops: Vec<DevOp>,
    tun_owner: &mut std::collections::HashMap<Ipv4Addr, [u8; 32]>,
    quic: Option<&homeway_quic::ExitQuic>,
) {
    for op in ops {
        match op {
            // `psk`/`tunnel_ip` 面随 WG 删除无消费者（QUIC 准入只经 `table`；隧道地址族
            // 由表内判据行覆盖）——这里只留分流键需要的 `tun_ip`。
            DevOp::Add { pubkey, tun_ip, .. } => {
                tun_owner.insert(tun_ip, pubkey);
            }
            DevOp::Remove { pubkey } => {
                tun_owner.retain(|_, v| *v != pubkey);
                if let Some(q) = quic {
                    q.unbind_pub(&pubkey);
                }
            }
        }
    }
}

/// devTag 短指纹（4B hex——与 `table.rs` 的 `dev_short` 同形）。
fn dev_short(d: &[u8; 8]) -> String {
    d.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// 公钥短指纹（4B hex——与 `table.rs` 的 `pub_short` 同形）。
fn pub_short(p: &[u8; 32]) -> String {
    p.iter().take(4).map(|b| format!("{b:02x}")).collect()
}

/// 拦截层出站明文包 → 按目的地址分流（M1 设计 §1.5；**M5 §3.2-bis 定稿 = 方案 (a)**）：
/// `dst == 某设备 tun_ip`（`hw-app` 派生地址）⇒ **QUIC DATAGRAM**（出口 QUIC 面出站队列；
/// 满 = 丢 + 计数）；其余 ⇒ **丢 + 计数 + 节流记行**（原「WG `device.encapsulate` 原样」
/// 的兜底面随 WG 删除，登记 §3.2-bis ② 的「丢 + 计数 + 记行（首 3 + 每 100）」）。
///
/// 分流键 `tun_ip → pubkey` 由引擎侧自持（`apply_dev_ops` 维护）：数据源 = 设备表已持有的
/// 每设备隧道地址（`table.rs` 的 `DevOp`），**不动岛公面**。
fn route_out(
    tx: &[Vec<u8>],
    tun_owner: &std::collections::HashMap<Ipv4Addr, [u8; 32]>,
    quic: Option<&homeway_quic::ExitQuic>,
    unbound_dropped: &mut u64,
    dlogf: &Logf,
) {
    for pkt in tx {
        let Some(IpAddr::V4(d)) = intercept::nat_view_dst(pkt).map(IpAddr::V4) else {
            continue; // 畸形：静默丢（拦截层产出恒可解析——防御位）
        };
        let handled = match (quic, tun_owner.get(&d)) {
            (Some(q), Some(key)) => q.send_to_pub(key, pkt) == homeway_quic::ExitSend::Handled,
            _ => false,
        };
        if !handled {
            // 无 QUIC 面 / 该 tun_ip 无绑定（设备未登记、连接已死、面已收工）
            *unbound_dropped += 1;
            if *unbound_dropped <= 3 || (*unbound_dropped).is_multiple_of(100) {
                (dlogf)(&format!(
                    "出站丢弃（无 QUIC 绑定：{d}）—— 累计 {unbound_dropped} 包（WG 兜底面已退役；等设备重连）"
                ));
            }
        }
    }
}

// ---------- 公网端点观测 + token 打印 ----------

#[derive(Default)]
struct TokenPrintState {
    last_token: String,
    revoked_logged: bool,
    last_published: Vec<String>,
    /// QUIC 端口的公网端点（M1 S1c / E-q4：UPnP 映射成功后由 `refresh_quic_public` 写入；
    /// `None` = 未映射/未启用/无 QUIC 面 ⇒ token 只出 LAN 与中继 QUIC 端点）。
    quic_pub: Option<SocketAddr>,
}



struct TokenCtx {
    cfg: ServeConfig,
    st: Arc<State>,
    secret: [u8; 32],
    backend_priv: x25519_dalek::StaticSecret,
    /// 公共端口（= QUIC 端口；M5 起唯一）。
    local_port: u16,
    logf: Logf,
    revoked: Arc<Mutex<HashSet<[u8; 32]>>>,
    inner: Mutex<TokenPrintState>,
    /// --relay 的中继端点（解析产物；None = 未配）。token 打印按四块语义并入
    /// （R4-design §4.3：kind 结构流动 / 去重降级 / 顺序 / relayWanted 闸门）。
    relay_ep: Option<Endpoint>,
    /// --relay 解析成功（闸门判据——按地址 fail-open）。
    relay_wanted: bool,
    /// 细节日志（降级行等）。
    dlogf: Logf,
    /// 端点变化轮专用流（Go tokenToFile = logf 形态：**只进摘要文件**，--verbose 才
    /// 回显终端——终端只打首轮 token，端点变化冒第二串只会让人拿错，2026-09-21
    /// 用户口径；首轮仍走 logf = ulogf 形态〔终端+文件〕）。
    tokf: Logf,
    /// （events 路径, debug 路径）——首轮 token 公告前的「日志：…（摘要）｜…（细节）」
    /// 落点提示行用（Go publicendpoint.go 同串；None = 无文件日志形态）。
    log_paths: Option<(String, String)>,
    /// 「socket 已钉卡/绑地址」的运行期事实（Go `pinnedNow`——钉卡失败自动降级、
    /// 看护循环重钉成功后置回；公网端点公布的保守判据读这里）。
    pinned_flag: Arc<AtomicBool>,
    /// 公网观测腿的域名解析器（国内 DoH + 钉卡 TCP + 污染闸；见 `server::doh`）——
    /// STUN 候选的域名**不再**经系统解析器（2026-10-20：fake-IP 污染让 STUN 全程失败）。
    resolver: super::doh::DohResolver,
    /// 观测/兜底共享取消位；发布门内检查，shutdown与发布事务串行。
    pubep_stopped: Arc<AtomicBool>,
    publication: Arc<Mutex<()>>,
    /// DDNS 自检的滚动状态（按域名一域一份；探测线程写 / status 快照读——P0-4）。
    ddns_checks: super::ddnscheck::DdnsChecks,
    /// DDNS 自检直查解析器（F2：`serve.ddns_resolver` 装配期为 `SocketAddr` 形态）。
    ddns_resolvers: Vec<SocketAddr>,
    /// 出口 QUIC 面的**实际监听地址**（退让后的真实端口；M1 S1c——token 的 QUIC 端点与
    /// E-q4 的 UPnP 映射都用它）。`None` = QUIC 面未起（`serve.quic=false` 或起失败）。
    quic_ep: Option<SocketAddr>,
    /// 出口 RPK 公钥（进 token 的 32B；`None` = 无 QUIC 面 ⇒ 该字段不打）。
    quic_rpk: Option<crate::token::RpkPubKey>,
}

/// 公网端点循环：成功 10min / 失败 2min 一轮；显式端点配置覆盖最高优先（FIX-61）。
/// 首轮结束（成不成都算）发一次 first_probe 信号——token 兜底线在等它（Go 同构）。
fn public_endpoint_loop(
    ctx: Arc<TokenCtx>,
    cmd_tx: Sender<EngineCmd>,
    kick_rx: mpsc::Receiver<()>,
    first_probe_tx: mpsc::SyncSender<()>,
) {
    let mut first = true;
    loop {
        if ctx.pubep_stopped.load(Ordering::SeqCst) {
            return; // r1#10：引擎已收工（restart 替换在途）——不再探测/写盘
        }
        let ok = refresh_public_endpoint(&ctx, &cmd_tx);
        if first {
            first = false;
            let _ = first_probe_tx.try_send(());
        }
        if ctx.pubep_stopped.load(Ordering::SeqCst) {
            return;
        }
        // DDNS 自检可能独立等解析器，不能拖延首轮完成信号。
        {
            let published = ctx
                .inner
                .lock()
                .map(|i| i.last_published.clone())
                .unwrap_or_default();
            super::ddnscheck::run_ddns_self_check_cancellable(
                &ctx.ddns_checks,
                &ctx.cfg.ddns,
                &published,
                &ctx.ddns_resolvers,
                &*ctx.logf,
                &ctx.pubep_stopped,
            );
        }
        if ctx.pubep_stopped.load(Ordering::SeqCst) {
            return;
        }
        let wait = if ok { PUBLIC_REFRESH_OK } else { PUBLIC_REFRESH_FAIL };
        match kick_rx.recv_timeout(wait) {
            Ok(()) => {
                if ctx.pubep_stopped.load(Ordering::SeqCst) {
                    return; // 唤醒后发现已停（探测期间的收工）
                }
                (ctx.logf)("公网端点：收到换网事件，立即重测");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// **公共端口**（= QUIC 端口；M5 后唯一）的公网可达性（M1 S1c / S1-7）：**UPnP 映射 +
/// 实际外口写进 token 的 QUIC 类端点**；结果成/败都打 **E-q4** 行
/// （`UPnP：QUIC 端口 %d 映射 %s`），失败时 token 只公布 LAN/中继 QUIC 端点（fail-visible）。
///
/// 返回 `(外部端口, 公网 IP)` 供 `refresh_public_endpoint` 的证据合流复用——**M5 起只有
/// 这一个端口**，映射只做一次（原「WG 端口与 QUIC 端口各自独立一条映射」的两条并存形态
/// 随 WG 端口删除）。
///
/// 本函数只在 `quic_ep` 存在（QUIC 面已起）时跑；无 QUIC 面 ⇒ **不打行**（没有端口可
/// 映射，不假装）。
///
/// `--upnp=false`：明文打失败行（设计 §10 S1-7 的判据形态）——「失败可见」而不是静默缺席。
fn refresh_quic_public(ctx: &Arc<TokenCtx>, round_deadline: Instant) -> (u16, Option<Ipv4Addr>) {
    let Some(q) = ctx.quic_ep else { return (0, None);
    };
    let port = q.port();
    let set_pub = |v: Option<SocketAddr>| {
        if let Ok(mut inner) = ctx.inner.lock() {
            inner.quic_pub = v;
        }
    };
    if !ctx.cfg.upnp {
        set_pub(None);
        (ctx.logf)(&format!(
            "UPnP：QUIC 端口 {port} 映射 未启用（--upnp=false）——公网 QUIC 端点不公布（LAN/中继 QUIC 端点照常）"
        ));
        return (0, None);
    }
    let cands = crate::server::upnp::local_ipv4_candidates();
    if cands.is_empty() {
        set_pub(None);
        (ctx.logf)(&format!("UPnP：QUIC 端口 {port} 映射 失败（找不到内网 IPv4 候选）"));
        return (0, None);
    }
    let logf2: Logf = Arc::clone(&ctx.logf);
    let deadline = round_deadline.min(Instant::now() + crate::server::upnp::UPNP_TOTAL_BUDGET);
    match crate::server::upnp::ensure_port_mapping_cancellable(
        &cands,
        port,
        &*logf2,
        &*logf2,
        deadline,
        Arc::clone(&ctx.pubep_stopped),
    ) {
        Err(e) => {
            set_pub(None);
            (ctx.logf)(&format!(
                "UPnP：QUIC 端口 {port} 映射 失败（{e}）——出口在 NAT 后时可在路由器上手动把 UDP {port} 转发到本机（候选 {:?}）；公网 QUIC 端点不公布",
                cands
            ));
            (0, None)
        }
        Ok((ext, used)) => {
            // 外网 IP（IGD 的 ExternalIPAddress；非公网地址不公布）——同一轮已查到就复用
            let wan = crate::server::upnp::discover_igd_cancellable(
                used,
                deadline,
                Arc::clone(&ctx.pubep_stopped),
            )
                .ok()
                .and_then(|g| g.external_ip().ok())
                .filter(|ip| egress::is_public_addr(IpAddr::V4(*ip)));
            match wan {
                Some(ip) => {
                    let pub_ep = SocketAddr::new(IpAddr::V4(ip), ext);
                    set_pub(Some(pub_ep));
                    (ctx.logf)(&format!(
                        "UPnP：QUIC 端口 {port} 映射 外部 UDP {ext} → {used}:{port}（公布 {pub_ep}；重启时从路由器表认领）"
                    ));
                    (ext, Some(ip))
                }
                None => {
                    set_pub(None);
                    (ctx.logf)(&format!(
                        "UPnP：QUIC 端口 {port} 映射 已建立（外部 UDP {ext}）但外网 IP 不可得/非公网——公网 QUIC 端点不公布"
                    ));
                    (ext, None)
                }
            }
        }
    }
}

// ---------- S7b（Q3/Q4/Q5）：公共端点面三小修 ----------
//
// 三条都是「写盘/取值失败**不得静默、也不得致命**」的对齐（真源 = Go
// `publicendpoint.go:126/226`（告警）、`role.go:95`（非致命）、`serve.go:113-121`
// （非法值告警清空））。**成功路径的行文逐字不变**；新增面全是 additive 告警行。

/// 监听端口落盘（Q4，原 `std::fs::write(..)?` = 致命）：失败只告警。
///
/// `cache/listen_port.txt` 是 **L3 可弃缓存**（`nodestate.rs`）——“只是少了给人看的
/// 记录，不影响隧道”（Go `role.go:95` 原文）。写失败不应阻断 serve 就绪。
fn write_listen_port_file(path: &std::path::Path, port: u16, logf: &Logf) {
    if let Err(e) = std::fs::write(path, format!("{port}\n")) {
        logf(&format!(
            "⚠️ 监听端口落盘失败（{}）：{e}——服务照常就绪",
            path.display()
        ));
    }
}

/// 公网端点落盘（Q3，原 `let _ = std::fs::write(..)` = 静默）：失败告警、**不中止本轮**。
///
/// 与 Go 的残余差异（如实登记）：Go `publicendpoint.go:126/226` 在写失败时
/// `return false`（本轮不公布、不打 token）；本实现取「告警 + 本轮照常公布」——
/// 理由 = 出口启停/公告语义不应因 L3 缓存写不进去而变（token 是终端一辈子只打一轮的
/// 产品面）；两处调用点（显式端点路径 / 推断路径）共用本函数。
fn write_public_endpoint_file(path: &std::path::Path, lines: &[String], logf: &mut dyn FnMut(&str),
) {
    if let Err(e) = std::fs::write(path, format!("{}\n", lines.join("\n"))) {
        logf(&format!("⚠️ 公网端点写盘失败（{}）：{e}", path.display()));
    }
}

/// 公网端点值域归一（Q5 的**守护期形态**，对齐 Go `serve.go:113-121` 的 `fill()`）：
/// 任一段非法 ⇒ **告警 + 按未配置处理**（整串清空，Go 同形），不拒启。
///
/// CLI 显式 flag 已在 `serve_cli::validate_public_endpoint_flag` fail fast（exit 2）、
/// config 面在严格读层拒启 ⇒ 本归一在正常 CLI 形态**恒不触发**（多一道防线；覆盖
/// 直接构造 `ServeConfig` 的调用方，如 App/capi 侧）。覆盖序不变：它只做「非法即清」。
fn normalize_public_endpoint(cfg: &mut ServeConfig, logf: &Logf) {
    if cfg.public_endpoint.trim().is_empty() {
        return;
    }
    for line in cfg.public_endpoint.split(',') {
        if let Err(e) = line.trim().parse::<std::net::SocketAddr>() {
            logf(&format!(
                "⚠️ --public-endpoint {line:?} 非法（{e}）——按未配置处理"
            ));
            cfg.public_endpoint.clear();
            break;
        }
    }
}

fn refresh_public_endpoint(ctx: &Arc<TokenCtx>, cmd_tx: &Sender<EngineCmd>) -> bool {
    // r2#10：入口先查停止位——引擎收工/替换后旧 pubep 线程不得再写任何面（manual
    // 配置路径与推断路径共用本入口）。
    if ctx.pubep_stopped.load(Ordering::SeqCst) {
        return false;
    }
    // 公共端口（= QUIC 端口；M5 后 WG 端口不存在）的公网映射（M1 S1c / S1-7：**E-q4 行
    // 成/败都打**，且 `--upnp=false` 形态也打失败行 = fail-visible）。放在本轮最前：
    // 既要给 token 填 `quic_pub`，也要把外口喂给下文的证据合流。
    let deadline = Instant::now() + PUBLIC_ROUND_BUDGET;
    let (upnp_ext, upnp_wan) = refresh_quic_public(ctx, deadline);
    let manual = ctx.cfg.public_endpoint.trim().to_owned();
    if !manual.is_empty() {
        // 配置覆盖：推断在「不能枚举/不能观测」的环境里无解（macOS SSDP 被本地网络
        // 隐私拒即一例）——照常写文件 + 打 token。
        let lines: Vec<String> = manual.split(',').map(|s| s.trim().to_owned()).collect();
        return publish_endpoints(ctx, cmd_tx, lines, true);
    }

    refresh_inferred_public_endpoint(ctx, cmd_tx, deadline, upnp_ext, upnp_wan)
}

fn refresh_inferred_public_endpoint(
    ctx: &Arc<TokenCtx>,
    cmd_tx: &Sender<EngineCmd>,
    deadline: Instant,
    upnp_ext: u16,
    upnp_wan: Option<Ipv4Addr>,
) -> bool {
    // ---- 推断路径：UPnP 映射（= 上面公共端口那条）+ 同端口 STUN 观测（两条证据合流才公布）----
    // **M5 起只有一条映射**：公共端口就是 QUIC 端口（`refresh_quic_public` 已映射并打
    // E-q4 行）——这里直接取它的产物，不再重复映射一次（WG 端口不存在了）。
    let ext_port: u16 = upnp_ext;
    let wan_ip: Option<Ipv4Addr> = upnp_wan;

    let mut observed: Option<SocketAddr> = None;
    if !ctx.cfg.stun.is_empty() {
        // 候选**惰性**逐个试（r1#3：解析一个 → 查一个 → 成功即停——预解析全部候选在
        // DoH 慢/黑洞时会把整轮拖到几十秒；2026-10-20：国内多候选 + DoH 解析 + 污染闸）。
        for (label, server) in StunCandidates::new(ctx, &ctx.cfg.stun, false, deadline) {
            match stun_query_sync(
                cmd_tx,
                server,
                deadline.min(Instant::now() + STUN_PER_CANDIDATE_BUDGET),
                &ctx.pubep_stopped,
            ) {
                None => (ctx.logf)(&format!("STUN：{label}（{server}）无应答——换下一个候选")),
                Some(ap) if !stun_family_matches(ap.ip(), false) => {
                    (ctx.logf)(&format!("STUN：{label} 观测到 {ap} 地址族不符——换下一个候选"));
                }
                Some(ap) if super::doh::poisoned_ip(&ap.ip()) => {
                    // 观测结果落污染段 = 本机代理/TUN 的投影（发包虽钉卡，中间层仍可能
                    // 改写）——不可作公网证据，换候选
                    (ctx.logf)(&format!("STUN：{label} 观测到 {ap} 落污染段（fake-IP/私网）——丢弃"));
                }
                Some(ap) => {
                    observed = Some(ap);
                    (ctx.logf)(&format!(
                        "STUN：监听 socket（本地 {}）在 {label} 眼里是 {ap}",
                        ctx.local_port
                    ));
                    break;
                }
            }
        }
        if observed.is_none() {
            (ctx.logf)(&format!("STUN：从监听 socket 问 {} 失败（解析失败或无应答）", ctx.cfg.stun));
        }
    }

    // 证据合流（publicendpoint.go 同序；外口≠监听口时「STUN 的 IP + UPnP 的外口」——
    // 只有没钉网卡时才要求端口一致，防代理把 STUN 观测污染成假的；pinned 判据是
    // **运行期事实**（钉卡失败自动降级——Go pinnedNow 语义））
    let pinned = ctx.pinned_flag.load(Ordering::SeqCst);
    let pub_ap: Option<SocketAddr> = match (observed, ext_port, wan_ip) {
        (Some(ap), ext, _) if ext != 0 && ap.port() == ext && public_v4(ap) => Some(ap),
        (Some(ap), ext, _) if ext != 0 && pinned && public_v4(ap) => {
            let built = SocketAddr::new(ap.ip(), ext);
            (ctx.logf)(&format!(
                "公网端点：外口 {ext} ≠ 监听口 {}（沿用历史端口/回退），用 STUN 的 IP + UPnP 的外口公布",
                ctx.local_port
            ));
            Some(built)
        }
        (Some(ap), 0, _) if ap.port() == ctx.local_port && public_v4(ap) => Some(ap),
        (None, ext, Some(w)) if ext != 0 => {
            let built = SocketAddr::new(IpAddr::V4(w), ext);
            (ctx.logf)("公网端点：用路由器自报 WAN 地址 + UPnP 外口公布（没有同 socket STUN 证据）");
            Some(built)
        }
        (Some(ap), ext, _) if public_v4(ap) => {
            // 逐字对齐 Go publicendpoint.go:193-196（半角逗号 + 括号明细——评审 M6）
            (ctx.logf)(&format!(
                "公网端点：暂不公布 —— STUN 观测到 {ap}，但外部端口与监听/UPnP 不一致（{} vs upnp={}）, 说明路由器改写端口或有代理抢路由",
                ap.port(),
                ext
            ));
            None
        }
        _ => {
            (ctx.logf)(&format!(
                "公网端点：暂不公布（UPnP={} STUN={}；两者都没拿到可用证据）",
                ext_port != 0,
                observed.is_some()
            ));
            None
        }
    };
    let Some(pub_ap) = pub_ap else { return false };
    // v4 主体公布 + v6 路径校验（Go publicendpoint.go:205-223）：v6 无 NAT，
    // 「公网地址」= 本机在该网卡上的全局地址 + 监听端口——用**同一个监听 socket**
    // 做一次 v6 STUN 验证路径真的可用，失败就不公布 v6（宁可不给，也不给一个发
    // 不出去的候选）；v6 条目端口 = 实际监听口（v4 UPnP 外口不适用于 IPv6）。
    let mut lines = vec![pub_ap.to_string()];
    if ctx.cfg.stun6.is_empty() {
        (ctx.logf)("公网端点：未配置 --stun6（需要有 AAAA 的 STUN 服务器），跳过 IPv6 公布");
    } else {
        let mut resolved_any6 = false;
        let mut v6: Option<SocketAddr> = None;
        for (label, server) in StunCandidates::new(ctx, &ctx.cfg.stun6, true, deadline) {
            resolved_any6 = true;
            match stun_query_sync(
                cmd_tx,
                server,
                deadline.min(Instant::now() + STUN_PER_CANDIDATE_BUDGET),
                &ctx.pubep_stopped,
            ) {
                None => (ctx.logf)(&format!("STUN6：{label}（{server}）无应答——换下一个候选")),
                Some(ap) if !stun_family_matches(ap.ip(), true) => {
                    (ctx.logf)(&format!("STUN6：{label} 观测到 {ap} 地址族不符——换下一个候选"));
                }
                Some(ap) if super::doh::poisoned_ip(&ap.ip()) => {
                    (ctx.logf)(&format!("STUN6：{label} 观测到 {ap} 落污染段——丢弃"));
                }
                Some(ap) => {
                    v6 = Some(ap);
                    break;
                }
            }
        }
        match v6 {
            Some(ap6) => {
                let ip = ap6.ip();
                lines.push(format!("[{ip}]:{}", ctx.local_port));
                (ctx.logf)(&format!(
                    "公网端点：IPv6 路径可用（STUN 看到 {ap6}），公布 [{ip}]:{}",
                    ctx.local_port
                ));
            }
            None => {
                // Go 的 %v 带具体原因——这里按断点分两类（解析失败 / 无应答）
                let why = if !resolved_any6 {
                    format!("{} 解析失败（无 AAAA？）", ctx.cfg.stun6)
                } else {
                    "无应答".to_owned()
                };
                (ctx.logf)(&format!("公网端点：IPv6 不可用（{why}），本轮只公布 IPv4"));
            }
        }
    }
    publish_endpoints(ctx, cmd_tx, lines, false)
}

/// cache、探测列表、内存快照、token 台账作为同一世代的发布事务提交。
fn publish_endpoints(
    ctx: &Arc<TokenCtx>,
    cmd_tx: &Sender<EngineCmd>,
    lines: Vec<String>,
    manual: bool,
) -> bool {
    let mut logs = DeferredLogs::default();
    {
        let _publication = ctx.publication.lock().expect("发布门锁中毒");
        if ctx.pubep_stopped.load(Ordering::SeqCst) {
            return false;
        }
        let endpoint_file = ctx.cfg.state_dir.join("cache").join("public_endpoint.txt");
        write_public_endpoint_file(&endpoint_file, &lines, &mut |line| {
            logs.push(&ctx.logf, line)
        });
        let msg = if manual {
            format!("公网端点：已按 **--public-endpoint 配置**公布 {}（跳过 UPnP/STUN 推断；写进 public_endpoint.txt）", format_lines(&lines))
        } else {
            format!(
                "公网端点：已公布 {}（写进 public_endpoint.txt；下次签发 token 会带上它）",
                format_lines(&lines)
            )
        };
        logs.push(&ctx.logf, msg);
        set_probe_endpoints_cmd(cmd_tx, &lines);
        ctx.inner.lock().expect("token 状态锁中毒").last_published = lines.clone();
        print_client_token_locked(ctx, &lines, &mut logs);
    }
    logs.emit(&ctx.pubep_stopped);
    true
}

#[derive(Default)]
struct DeferredLogs(Vec<(Logf, String)>);
impl DeferredLogs {
    fn push(&mut self, log: &Logf, line: impl Into<String>) {
        self.0.push((Arc::clone(log), line.into()));
    }
    fn emit(self, stop: &AtomicBool) {
        for (log, line) in self.0 {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            log(&line);
        }
    }
}

/// 首轮信号或15s兜底任一到达即起跑；失联通道、探测黑洞、驱动退出均有界。
fn wait_first_probe(
    rx: &mpsc::Receiver<()>,
    wait: Duration,
    stop: &AtomicBool,
    driver_alive: &AtomicBool,
) -> bool {
    let deadline = Instant::now() + wait;
    while !stop.load(Ordering::SeqCst) && driver_alive.load(Ordering::SeqCst) {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return true;
        }
        let slice = remain.min(Duration::from_millis(50));
        match rx.recv_timeout(slice) {
            Ok(()) => return true,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => std::thread::sleep(slice),
        }
    }
    false
}

fn wait_observer(stop: &AtomicBool, duration: Duration) -> bool {
    let deadline = Instant::now() + duration;
    while !stop.load(Ordering::SeqCst) {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return true;
        }
        std::thread::sleep(remain.min(Duration::from_millis(50)));
    }
    false
}

fn format_lines(lines: &[String]) -> String {
    format!("[{}]", lines.join(" "))
}

/// `--ddns` 域名条目的端口 = 已公布公网 v4 端点的外部端口；无公网端点观测
/// （--upnp=false --stun=""）时回退实际监听口（`ddnsEntryPort` 同义）。
fn ddns_entry_port(published: &[String], listen_port: u16) -> u16 {
    for line in published {
        if let Ok(ap) = line.parse::<SocketAddr>() {
            if ap.is_ipv4() && ap.port() != 0 {
                return ap.port();
            }
        }
    }
    listen_port
}

fn public_v4(ap: SocketAddr) -> bool {
    match ap.ip() {
        IpAddr::V4(v4) => egress::is_public_addr(IpAddr::V4(v4)),
        IpAddr::V6(_) => false, // 公布端点这条路径保持只认 v4（v6 面 R5 补——设计 M10）
    }
}

/// 单轮网络探测总预算（UPnP、DoH、两族STUN共享）。
const PUBLIC_ROUND_BUDGET: Duration = Duration::from_secs(60);
/// 单候选STUN最多4s，同时受整轮剩余时间约束。
const STUN_PER_CANDIDATE_BUDGET: Duration = Duration::from_secs(4);

/// STUN 候选解析（2026-10-20）：`spec` 逗号分隔 `host:port`；IP 字面量直收、域名经
/// `TokenCtx::resolver`（国内 DoH + 钉卡 TCP，**不经系统解析器**——fake-IP 环境下
/// getaddrinfo 把任何域名都答成 198.18.x，钉卡的 UDP 发包再准也发向虚拟地址）；
/// 解析结果一律过污染闸。`want_v6` 选族：v4 候选取 A、v6 候选取非 v4-mapped 的 AAAA
/// （观测 v6 路径必须真走 v6——评审 M22 口径不变）。返回 (候选原文, 地址) 序。
struct StunCandidates<'a> {
    ctx: &'a TokenCtx,
    specs: std::str::Split<'a, char>,
    want_v6: bool,
    deadline: Instant,
}

impl<'a> StunCandidates<'a> {
    fn new(ctx: &'a TokenCtx, spec: &'a str, want_v6: bool, deadline: Instant) -> Self {
        Self {
            ctx,
            specs: spec.split(','),
            want_v6,
            deadline,
        }
    }
}

impl Iterator for StunCandidates<'_> {
    type Item = (String, SocketAddr);
    /// 惰性：调用方要一个才解析一个（r1#3——预解析全部候选会拖慢整轮）。
    fn next(&mut self) -> Option<Self::Item> {
        let ctx = self.ctx;
        let want_v6 = self.want_v6;
        for cand in self.specs.by_ref().map(str::trim).filter(|c| !c.is_empty()) {
            if ctx.pubep_stopped.load(Ordering::SeqCst) || Instant::now() >= self.deadline {
                return None;
            }
            let Some((host, port)) =
                cand.rsplit_once(':').and_then(|(h, p)| p.parse::<u16>().ok().map(|p| (h, p)))
            else {
                (ctx.logf)(&format!("STUN：候选 {cand} 形态非法（应为 host:port）——跳过"));
                continue;
            };
            let host = host.trim_matches(|c| c == '[' || c == ']');
            if let Ok(ip) = host.parse::<IpAddr>() {
                if stun_family_matches(ip, want_v6) && !super::doh::poisoned_ip(&ip) {
                    return Some((cand.to_owned(), SocketAddr::new(ip, port)));
                }
                continue;
            }
            let ip = if want_v6 {
                ctx.resolver.resolve6_before(host, self.deadline, &ctx.pubep_stopped)
                    .map(IpAddr::V6)
            } else {
                ctx.resolver
                    .resolve4_before(host, self.deadline, &ctx.pubep_stopped).map(IpAddr::V4)
            };
            match ip {
                Some(ip) => return Some((cand.to_owned(), SocketAddr::new(ip, port))),
                None => (ctx.logf)(&format!(
                    "STUN：{cand} 解析失败（{}）——跳过",
                    if want_v6 { "无可用 AAAA / DoH 候选已尽 / 应答落污染段" } else { "无可用 A / DoH 候选已尽 / 应答落污染段" }
                )),
            }
        }
        None
    }
}

fn stun_family_matches(ip: IpAddr, want_v6: bool) -> bool {
    match ip {
        IpAddr::V4(_) => !want_v6,
        IpAddr::V6(ip) => want_v6 && ip.to_ipv4_mapped().is_none() && (ip.segments()[0] & 0xffc0) != 0xfe80,
    }
}

fn stun_query_sync(
    cmd_tx: &Sender<EngineCmd>,
    server: SocketAddr,
    deadline: Instant,
    stop: &AtomicBool,
) -> Option<SocketAddr> {
    if stop.load(Ordering::SeqCst) || Instant::now() >= deadline {
        return None;
    }
    let (tx, rx) = mpsc::channel();
    cmd_tx
        .send(EngineCmd::StunQuery { server, reply: tx })
        .ok()?;
    while !stop.load(Ordering::SeqCst) {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            break;
        }
        match rx.recv_timeout(remain.min(Duration::from_millis(50))) {
            Ok(v) => return v,
            Err(mpsc::RecvTimeoutError::Disconnected) => return None,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
    let _ = cmd_tx.send(EngineCmd::StunAbort);
    None
}

fn set_probe_endpoints_cmd(cmd_tx: &Sender<EngineCmd>, lines: &[String]) {
    let eps: Vec<SocketAddr> = lines
        .iter()
        .filter_map(|l| l.parse().ok())
        .filter(|ap: &SocketAddr| ap.port() != 0 && egress::is_public_addr(ap.ip()))
        .take(crate::probe::MAX_ENDPOINTS)
        .collect();
    let _ = cmd_tx.send(EngineCmd::SetProbeEndpoints(eps));
}

/// 打客户端 token（E3）：终端只打第一轮（进程生命周期内不再重打——端点变化冒出
/// 第二串 token 只会让人拿错）；台账追加纪律 = 每次铸出与末行不同即追加。
fn print_client_token(ctx: &Arc<TokenCtx>, published: &[String]) {
    let mut logs = DeferredLogs::default();
    {
        let _publication = ctx.publication.lock().expect("发布门锁中毒");
        if ctx.pubep_stopped.load(Ordering::SeqCst) {
            return;
        }
        // 兜底拿到的快照可能已过时；在门内重读，以免回写上一轮公网端点。
        let current = ctx
            .inner
            .lock()
            .expect("token 状态锁中毒")
            .last_published
            .clone();
        let published = if current.is_empty() {
            published
        } else {
            &current
        };
        print_client_token_locked(ctx, published, &mut logs);
    }
    logs.emit(&ctx.pubep_stopped);
}

fn print_client_token_locked(ctx: &Arc<TokenCtx>, published: &[String], logs: &mut DeferredLogs) {
    // 低-7（B0-2a 登记→D-1 收口）：inner 锁只护 revoked_logged/last_token 的读写
    // 小临界区——网卡枚举/token 编码/台账追加由调用者的发布门串行化，
    // 防止兜底旧快照在新一轮公布之后重新成为台账末行。
    {
        let mut inner = ctx.inner.lock().expect("token 状态锁中毒");
        // 在用凭证已被吊销：只大声一次并停打
        if ctx.revoked.lock().map(|m| m.contains(&ctx.secret)).unwrap_or(false) {
            if !inner.revoked_logged {
                inner.revoked_logged = true;
                drop(inner);
                logs.push(&ctx.logf,
                    "⚠️ 在用凭证已被吊销——**不再打印 token**（旧 token 已作废）；执行 `homeway serve restart`（重启）铸出新凭证，客户端需重新粘贴",
                );
            }
            return;
        }
    }
    // 端点组装（四块语义——R4-design §4.3，评审 ⑥-8）：
    // ① add 收**整个 Endpoint（含 kind）**——relay 标记靠结构流动（57012ad 防线）；
    // ② 去重先到先得：中继地址与直连端点重合（退化形态）按直连保留 + 降级行；
    // ③ 顺序：LAN → 已公布公网 → 中继（叠加不踢除）；
    // ④ relayWanted 闸门（按地址 fail-open）：中继端点没并入前不打 token。
    let mut eps: Vec<Endpoint> = Vec::new();
    let mut labels: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let dlogf = &ctx.dlogf;
    let mut endpoint_logs = DeferredLogs::default();
    let mut add = |e: Endpoint, kind: &str| {
        if e.addr.is_empty() || seen.contains(&e.addr) {
            // 中继地址与已有直连端点相同（--relay 指向出口自己地址的退化形态）：
            // 按先到的直连形态保留——那个地址物理上就是出口的 WG socket，标 relay 反而必坏
            if e.kind == EndpointKind::Relay && seen.contains(&e.addr) {
                endpoint_logs.push(
                    dlogf,
                    format!("中继端点 {} 与已有直连端点相同，按直连处理（中继腿不生效）", e.addr));
            }
            return;
        }
        seen.insert(e.addr.clone());
        labels.push(format!("{}（{kind}）", e.addr));
        eps.push(e);
    };
    // 内网端点（M5 S3a：**唯一承载类 = Quic**——WG 端口已不存在，原 Direct 内网端点块
    // 删除；端口 = 退让后的**实际** QUIC 监听口）。
    if let Some(q) = ctx.quic_ep {
        for ifi in egress::physical_candidates() {
            for a in ifi.addrs {
                add(
                    Endpoint { addr: format!("{a}:{}", q.port()), kind: EndpointKind::Quic },
                    "QUIC 内网",
                );
            }
        }
    }
    // 已公布公网端点（E20 族：显式配置 / UPnP+STUN 合流产物）——M5 起同为 **Quic** 类
    // （该端口就是 QUIC 端口；WG 类 Direct 公网端点退役 = 设计 §8.1-E3「QUIC 类端点成为
    // 唯一公网/直连承载类」）。
    for p in published {
        add(Endpoint { addr: p.clone(), kind: EndpointKind::Quic }, "QUIC 公网");
    }
    // QUIC 公网端点（E-q4 的 UPnP 映射产物；未映射/未启用 ⇒ 缺席 = fail-visible，
    // 与 E-q4 行同步）。与上一块的地址通常相同（去重键 = addr 串 ⇒ 只留一条）。
    if let Some(qp) = ctx.inner.lock().map(|i| i.quic_pub).unwrap_or(None) {
        add(Endpoint { addr: qp.to_string(), kind: EndpointKind::Quic }, "QUIC 公网");
    }
    // DDNS 域名条目（多条，叠加不踢除——B-1 拍板）：端口口径 = 已公布公网 v4 端点的
    // 外部端口；无公网观测时回退实际监听口（让位退让后的真实口）。
    if !ctx.cfg.ddns.is_empty() {
        let p = ddns_entry_port(published, ctx.local_port);
        if p != 0 {
            for domain in &ctx.cfg.ddns {
                add(
                    Endpoint { addr: format!("{domain}:{p}"), kind: EndpointKind::Quic },
                    "域名",
                );
            }
        } else {
            // listenPort=0 的调用形态（探测应答的即时快照）：域名条目这轮缺席，下一轮补上。
            logs.push(
                dlogf,
                "域名条目：本轮拿不到端口（socket 未开？），token/列表暂不带 ddns 条目");
        }
    }
    if let Some(r) = &ctx.relay_ep {
        add(r.clone(), "中继");
    }
    logs.0.extend(endpoint_logs.0);
    if eps.is_empty() {
        return;
    }
    // ④ 闸门（按地址比对、fail-open：退化形态去重后仍能命中已并地址 ⇒ 放行）
    if ctx.relay_wanted {
        let has_relay = ctx
            .relay_ep
            .as_ref()
            .is_some_and(|r| eps.iter().any(|e| e.addr == r.addr));
        if !has_relay {
            return; // 指定了 --relay：中继端点没并入前不打（先打一版不带中继的只会误导）
        }
    }
    let peer_id = crate::token::PeerId::from(x25519_dalek::PublicKey::from(&ctx.backend_priv).to_bytes());
    let secret = crate::token::Secret::from(ctx.secret);
    let ep_refs: Vec<crate::token::EndpointRef> = tok_endpoint_refs(&eps);
    let tok_str = match crate::token::encode(&crate::token::TokenSpec {
        peer_id: &peer_id,
        secret: &secret,
        endpoints: &ep_refs,
        // M1 S1c：出口 RPK 公钥进 token（additive 尾字段；S2 的岛按它钉定服务端身份）。
        // 无 QUIC 面 ⇒ None ⇒ 串与 M1 之前逐字节同。
        rpk: ctx.quic_rpk.as_ref(),
    }) {
        Ok(s) => s,
        Err(e) => {
            logs.push(&ctx.logf, format!("客户端 token 生成失败（{e}）"));
            return;
        }
    };
    let eps_count = eps.len();
    // 台账写入纪律：与末行不同即追加（无变化不追加）；吊销拒写（分支告警——Go
    // printClientToken 的 ErrSecretRevoked 专用行，P1-5 收口）
    if let Err(e) = ctx
        .st
        .append_token(&ctx.secret, &eps, ctx.quic_rpk.as_ref().map(|k| *k.as_bytes()))
    {
        match e {
            crate::server::state::StateError::SecretRevoked => {
                logs.push(&ctx.logf, format!(
                    "⚠️ 在用凭证已被吊销（{e}）——本轮 token 未入台账；执行 `homeway serve restart`（重启）铸出新凭证"
                ));
            }
            other => {
                logs.push(
                    &ctx.logf,
                    format!("⚠️ token 台账追加失败（{other}）——台账末行可能与在用 token 短暂不一致"));
            }
        }
    }
    // 打印去重与首轮判定：小临界区（check-and-set 原子——两路并发打印只此一处互斥）。
    let (is_same, first) = {
        let mut inner = ctx.inner.lock().expect("token 状态锁中毒");
        let same = tok_str == inner.last_token;
        let first = inner.last_token.is_empty();
        if !same {
            inner.last_token = tok_str.clone();
        }
        (same, first)
    };
    if is_same {
        return;
    }
    if first {
        // 首轮（进程内仅此一次）= Go ulogf 形态：带一行日志落点提示，终端 + 摘要文件。
        if let Some((ev_p, dbg_p)) = &ctx.log_paths {
            logs.push(&ctx.logf, format!("日志：{ev_p}（摘要）｜ {dbg_p}（细节）"));
        }
        logs.push(
            &ctx.logf,
            format!("客户端 token（粘进 App 的「添加主机」即可；{eps_count} 个端点）：{tok_str}"),
        );
        logs.push(&ctx.logf, format!("端点：{}", labels.join("、")));
    } else {
        // 端点变化轮 = Go tokenToFile 形态：只进摘要文件（--verbose 才回显终端）——
        // 终端冒出第二串 token 只会让人拿错（2026-09-21 用户口径；取最新 =
        // grep 客户端 token events.log | tail -1）。
        logs.push(&ctx.tokf, format!("客户端 token（端点已变化；粘进 App 的「添加主机」即可；{eps_count} 个端点）：{tok_str}"));
        logs.push(&ctx.tokf, format!("端点：{}", labels.join("、")));
    }
}

// ---------- udpcap（默认路径 UDP 能力探测；结论喂探测应答 caps） ----------

fn udpcap_loop(
    cmd_tx: Sender<EngineCmd>,
    stats: Arc<ItcStats>,
    logf: &Logf,
    kick_rx: mpsc::Receiver<()>,
    dns_targets: Vec<SocketAddrV4>,
    stun_targets: Vec<SocketAddrV4>,
) {
    let mut last = "?".to_owned();
    let mut prev_replied: u64 = 0;
    let mut prev_no_reply: u64 = 0;
    loop {
        // 先探一轮再进周期（Go startUDPCapProbe 同序）
        let (dns_note, generic_note, saw, hint, flags) =
            probe_once(&stats, &mut prev_replied, &mut prev_no_reply, &dns_targets, &stun_targets);
        let _ = cmd_tx.send(EngineCmd::SetCaps(flags));
        let cur = format!("0x{flags:02x}");
        if cur != last || saw != "本轮没有转发的 UDP 会话" {
            (logf)(&format!(
                "UDP 默认路径：DNS:53 {dns_note}；通用 UDP（STUN:3478）{generic_note}；实测 {saw}{hint}（探测应答 flags=0x{flags:02x} 也会这么报）"
            ));
            last = cur;
        }
        // 周期等待可被 kick 打断（换网卡 = 换了条路，能力结论立即重测）。
        // 三态（Q-I F7）：Ok(kick) 与 **Timeout 都立即重探**——旧形态对 Timeout 也再
        // sleep 一个 interval（bindwatch 在位时实际周期 ~600s，Go 真源 = 5min ticker
        // + kick，`udpcap.go:26/140-152`；`--bind-interface none` 形态本已 ~300s 不变）；
        // Disconnected（看护未起：--bind-interface none / IP 字面量 / auto 挑卡失败
        // ——sender 已随 start() 返回被 drop）必须**按原节拍继续**：探测与 SetCaps 是
        // P1-4 客户端能力打行的数据源，退出或全速自旋（评审 r1-H1：吞掉 Disconnected
        // 会 ~7 轮/秒持续探测）都不成立。
        let wait = kick_rx.recv_timeout(UDPCAP_INTERVAL);
        if udpcap_disconnected_backoff(wait) {
            std::thread::sleep(UDPCAP_INTERVAL);
        }
    }
}

/// Q-I F7 纯函数：`recv_timeout` 结果 → 是否需要补睡一个 `UDPCAP_INTERVAL`。
/// 只有 `Disconnected`（看护 sender 已 drop，recv 立即返回）需要——否则自旋。
fn udpcap_disconnected_backoff(r: Result<(), mpsc::RecvTimeoutError>) -> bool {
    matches!(r, Err(mpsc::RecvTimeoutError::Disconnected))
}

#[allow(clippy::type_complexity)]
fn probe_once(
    stats: &Arc<ItcStats>,
    prev_replied: &mut u64,
    prev_no_reply: &mut u64,
    dns_targets: &[SocketAddrV4],
    stun_targets: &[SocketAddrV4],
) -> (String, String, String, String, u8) {
    let mut flags: u8 = 0;
    let dns_note = match egress::probe_default(dns_targets, Duration::from_secs(2)) {
        Ok(rtt) => {
            flags |= UDPCAP_DNS;
            format!("可用（往返 {}ms）", rtt.as_millis())
        }
        Err(e) => format!("不可用（{e}）"),
    };
    // 重试一次：启动瞬间/网络刚起来会有偶发超时
    let mut gen_note = String::new();
    let mut generic_ok = false;
    for _ in 0..2 {
        match egress::probe_stun(None, stun_targets, Duration::from_secs(3)) {
            Ok((mapped, rtt)) => {
                flags |= UDPCAP_GENERIC;
                gen_note = format!("有可校验应答（往返 {}ms，映射 {mapped}）", rtt.as_millis());
                generic_ok = true;
                break;
            }
            Err(e) => {
                gen_note = format!("**未取得正证据**（{e}）—— 可能是探针目标不可达，也可能是这条路不回包；以下面「实测」为准");
            }
        }
    }
    let _ = generic_ok;
    // 真实流量证据（统计窗口 = 本次探测与上次探测之间）。键名取数（2026-10-10 审查
    // 低-8：原按位置索引 `snap[4]/snap[5]`，与同文件 `status_bits` 的按键名查找两种
    // 取法并存——快照表一旦重排即静默读错，编译器无从兜底）。
    let snap = stats.snapshot();
    let get = |k: &str| snap.iter().find(|(n, _)| *n == k).map(|(_, v)| *v).unwrap_or(0);
    let (replied, no_reply) = (get("udpReplied"), get("udpNoReply"));
    let d_replied = replied.saturating_sub(*prev_replied);
    let d_no_reply = no_reply.saturating_sub(*prev_no_reply);
    *prev_replied = replied;
    *prev_no_reply = no_reply;
    flags |= UDPCAP_PROBED;
    if d_replied + d_no_reply > 0 {
        flags |= UDPCAP_SEEN;
    }
    if d_replied > 0 {
        flags |= UDPCAP_OBSERVED;
    }
    let (saw, hint) = if d_replied + d_no_reply == 0 {
        ("本轮没有转发的 UDP 会话".to_owned(), String::new())
    } else {
        let saw = format!("本轮 {d_replied} 条有回包 / {d_no_reply} 条只有上行");
        let hint = if d_replied == 0 {
            " —— 转发出去的 UDP 全都只有上行没有回包：这条路（多半是 TUN 型代理）不回这类 UDP，对应到应用就是 QUIC 会超时回落 TCP".to_owned()
        } else {
            String::new()
        };
        (saw, hint)
    };
    (dns_note, gen_note, saw, hint, flags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hmac::{Hmac, Mac};
    use sha2::Sha256;

    const REG_LEN: usize = 66;

    fn reg_bytes(secret: &[u8; 32], pubkey: &[u8; 32], dev: &[u8; 8], ts: u64) -> Vec<u8> {
        let mut out = Vec::with_capacity(REG_LEN);
        out.extend_from_slice(b"H2");
        out.extend_from_slice(pubkey);
        out.extend_from_slice(dev);
        out.extend_from_slice(&ts.to_be_bytes());
        let mut mac = Hmac::<Sha256>::new_from_slice(secret).unwrap();
        mac.update(b"hr-reg2");
        mac.update(&out[2..50]);
        let sum = mac.finalize().into_bytes();
        out.extend_from_slice(&sum[..16]);
        out
    }

    /// S7b/Q3：`public_endpoint.txt` 写失败 ⇒ **告警行**（原 = 静默 `let _ =`）；
    /// 成功路径 = 逐字写 + **不打任何行**（既有成功行文不变）。
    #[test]
    fn public_endpoint_write_failure_warns() {
        let (logf, rx) = log_sink();
        let dir = tmp_dir("pubep");
        // 让写入必失败：目标路径占据成**目录**（EISDIR）
        let path = dir.join("public_endpoint.txt");
        std::fs::create_dir_all(&path).unwrap();
        write_public_endpoint_file(&path, &["203.0.113.7:42650".to_owned()], &mut |line| {
            logf(line)
        });
        let lines: Vec<String> = rx.try_iter().collect();
        assert_eq!(lines.len(), 1, "写失败必须告警（且只一行）：{lines:?}");
        assert!(
            lines[0].starts_with("⚠️ 公网端点写盘失败（") && lines[0].contains("public_endpoint.txt"),
            "告警行须带路径：{}",
            lines[0]
        );
        // 子用例二：目标路径是普通目录下的文件（父目录不存在 ⇒ ENOENT）
        let path2 = dir.join("no-such-dir").join("public_endpoint.txt");
        write_public_endpoint_file(&path2, &["203.0.113.7:42650".to_owned()], &mut |line| {
            logf(line)
        });
        assert_eq!(rx.try_iter().count(), 1, "父目录缺失同样告警");
        // 成功路径：文件内容 = 每条一行 + 尾换行；零新增行
        let ok = dir.join("ok.txt");
        write_public_endpoint_file(&ok, &["a:1".to_owned(), "b:2".to_owned()], &mut |line| {
            logf(line)
        });
        assert_eq!(std::fs::read_to_string(&ok).unwrap(), "a:1\nb:2\n");
        assert_eq!(rx.try_iter().count(), 0, "成功路径不打行（既有行文不变）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S7b/Q4：`listen_port.txt` 写失败 = **告警 + 非致命**（原 `?` = 致命 ⇒ serve 启不来）；
    /// 成功路径 = `端口\n`（逐字节保持既有落盘格式）。
    #[test]
    fn listen_port_write_failure_warns_and_is_not_fatal() {
        let (logf, rx) = log_sink();
        let dir = tmp_dir("lp");
        let path = dir.join("listen_port.txt");
        std::fs::create_dir_all(&path).unwrap(); // 占据成目录 ⇒ EISDIR
        write_listen_port_file(&path, 42650, &logf); // 不 panic、不返回 Err（非致命）
        let lines: Vec<String> = rx.try_iter().collect();
        assert_eq!(lines.len(), 1, "写失败必须告警：{lines:?}");
        assert!(
            lines[0].starts_with("⚠️ 监听端口落盘失败（") && lines[0].ends_with("——服务照常就绪"),
            "告警行形态：{}",
            lines[0]
        );
        let ok = dir.join("ok.txt");
        write_listen_port_file(&ok, 42650, &logf);
        assert_eq!(std::fs::read_to_string(&ok).unwrap(), "42650\n", "落盘格式逐字节不变");
        assert_eq!(rx.try_iter().count(), 0, "成功路径不打行");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// S7b/Q5（守护期形态，对齐 Go `serve.go:113-121` 的 `fill()`）：非法值 ⇒
    /// **告警 + 清空**（按未配置处理）；合法值 ⇒ 原样保留、零输出。
    #[test]
    fn normalize_public_endpoint_warns_and_clears_on_invalid() {
        let (logf, rx) = log_sink();
        let mut cfg = ServeConfig {
            public_endpoint: "1.2.3.4:5,not-an-endpoint".to_owned(),
            ..ServeConfig::default()
        };
        normalize_public_endpoint(&mut cfg, &logf);
        assert!(cfg.public_endpoint.is_empty(), "非法 ⇒ 整串清空（Go fill() 同形）");
        let lines: Vec<String> = rx.try_iter().collect();
        assert_eq!(lines.len(), 1, "告警一行：{lines:?}");
        assert!(
            lines[0].starts_with("⚠️ --public-endpoint") && lines[0].ends_with("——按未配置处理"),
            "告警行形态：{}",
            lines[0]
        );
        // 合法（v4 + v6 字面 + 空白容错）：原样保留、零输出
        let mut cfg2 = ServeConfig {
            public_endpoint: " 1.2.3.4:5 , [::1]:6 ".to_owned(),
            ..ServeConfig::default()
        };
        normalize_public_endpoint(&mut cfg2, &logf);
        assert_eq!(cfg2.public_endpoint, " 1.2.3.4:5 , [::1]:6 ", "合法值不动（含原空白）");
        // 空值：早退、零输出（空 = 未配置）
        let mut cfg3 = ServeConfig::default();
        normalize_public_endpoint(&mut cfg3, &logf);
        assert_eq!(rx.try_iter().count(), 0, "合法/空值零输出");
    }

    fn tmp_dir(tag: &str) -> std::path::PathBuf {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let d = std::env::temp_dir().join(format!(
            "hw-eng-s7b-{tag}-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn now() -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000)
    }

    /// 测试用 `TokenCtx`（M1 S1c：token 铸造 / E-q4 两条面的最小装配——只放本模块测试
    /// 需要的字段；state 落在独立临时目录，跑完即删）。
    fn token_ctx(
        upnp: bool,
        quic_ep: Option<SocketAddr>,
        quic_rpk: Option<[u8; 32]>,
        logf: Logf,
        rx: &std::sync::mpsc::Receiver<String>,
    ) -> (Arc<TokenCtx>, std::path::PathBuf) {
        static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "hw-eng-tokctx-{}-{}",
            std::process::id(),
            N.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let st = Arc::new(State::open(&dir).expect("state 可开"));
        let cfg = ServeConfig {
            state_dir: dir.clone(),
            upnp,
            ..ServeConfig::default()
        };
        let dlog = Arc::clone(&logf);
        let ctx = Arc::new(TokenCtx {
            cfg,
            st,
            secret: [0x3Cu8; 32],
            backend_priv: x25519_dalek::StaticSecret::from([0x3Du8; 32]),
            local_port: 42650,
            logf,
            revoked: Arc::new(Mutex::new(HashSet::new())),
            inner: Mutex::new(TokenPrintState::default()),
            relay_ep: None,
            relay_wanted: false,
            dlogf: dlog,
            tokf: Arc::new(|_: &str| {}),
            log_paths: None,
            pinned_flag: Arc::new(AtomicBool::new(false)),
            pubep_stopped: Arc::new(AtomicBool::new(false)),
            publication: Arc::new(Mutex::new(())),
            resolver: crate::server::doh::DohResolver::new(
                Vec::new(),
                Arc::new(Mutex::new(None)),
                Arc::new(|_: &str| {}),
            ),
            ddns_checks: Arc::new(std::collections::HashMap::new().into()),
            ddns_resolvers: Vec::new(),
            quic_ep,
            quic_rpk: quic_rpk.map(crate::token::RpkPubKey::from),
        });
        let _ = rx;
        (ctx, dir)
    }

    #[test]
    fn stopped_refresh_cannot_append_after_replacement_from_logger_callback() {
        let (arrived_tx, arrived_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let resume_rx = Mutex::new(resume_rx);
        let logf: Logf = Arc::new(move |line| {
            if line.starts_with("⚠️ 公网端点写盘失败") {
                arrived_tx.send(()).unwrap();
                resume_rx.lock().unwrap().recv().unwrap();
            }
        });
        let (_sink, rx) = log_sink();
        let (mut old, dir) = token_ctx(false, None, None, logf, &rx);
        Arc::get_mut(&mut old).unwrap().cfg.public_endpoint = "8.8.8.8:42650".into();
        let endpoint_path = dir.join("cache/public_endpoint.txt");
        std::fs::create_dir_all(&endpoint_path).unwrap(); // 强制进入历史可重入日志缝。
        let (cmd_tx, _cmd_rx) = mpsc::channel();
        let old_thread = {
            let old = Arc::clone(&old);
            let cmd_tx = cmd_tx.clone();
            std::thread::spawn(move || refresh_public_endpoint(&old, &cmd_tx))
        };
        arrived_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        old.pubep_stopped.store(true, Ordering::SeqCst);
        // 日志发生于发布门外：停止与替换可完成，避免logger重入锁死。
        drop(old.publication.try_lock().expect("logger持有发布门"));
        let (new_log, new_rx) = log_sink();
        let (mut new, new_dir) = token_ctx(false, None, None, new_log, &new_rx);
        {
            let ctx = Arc::get_mut(&mut new).unwrap();
            ctx.cfg.state_dir = dir.clone();
            ctx.st = Arc::clone(&old.st);
            ctx.cfg.public_endpoint = "9.9.9.9:42650".into();
        }
        std::fs::remove_dir(&endpoint_path).unwrap();
        assert!(refresh_public_endpoint(&new, &cmd_tx));
        resume_tx.send(()).unwrap();
        assert!(old_thread.join().unwrap());
        assert_eq!(
            std::fs::read_to_string(&endpoint_path).unwrap(),
            "9.9.9.9:42650\n"
        );
        let latest = old.st.last_token().unwrap().unwrap();
        assert!(latest.endpoints.iter().any(|e| e.addr == "9.9.9.9:42650"));
        assert!(!latest.endpoints.iter().any(|e| e.addr == "8.8.8.8:42650"));
        // 停用后直接helper/fallback调用同样不能改台账。
        print_client_token(&old, &["8.8.4.4:42650".into()]);
        assert_eq!(
            old.st.last_token().unwrap().unwrap().endpoints,
            latest.endpoints
        );
        let _ = std::fs::remove_dir_all(dir);
        let _ = std::fs::remove_dir_all(new_dir);
    }

    #[test]
    fn inferred_refresh_skips_mapped_v6_and_wrong_family_reply_and_uses_listen_port() {
        let (logf, rx) = log_sink();
        let (mut ctx, dir) = token_ctx(false, None, None, logf, &rx);
        {
            let ctx = Arc::get_mut(&mut ctx).unwrap();
            ctx.cfg.stun = "1.1.1.1:3478".into();
            ctx.cfg.stun6 =
                "[::ffff:8.8.8.8]:3478,[2001:4860::8888]:3478,[2606:4700::1111]:3478".into();
            ctx.pinned_flag.store(true, Ordering::SeqCst);
        }
        std::fs::create_dir_all(dir.join("cache")).unwrap();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let mut queried = Vec::new();
            while let Ok(cmd) = cmd_rx.recv_timeout(Duration::from_secs(2)) {
                match cmd {
                    EngineCmd::StunQuery { server, reply } => {
                        queried.push(server);
                        let answer = match queried.len() {
                            1 => "8.8.8.8:42650", // v4已钉卡，UPnP外口可独立改写。
                            2 => "8.8.4.4:42650", // v6响应错族，不得阻断后续候选。
                            3 => "[2001:4860::7777]:42650",
                            _ => panic!("多余STUN查询"),
                        };
                        reply.send(Some(answer.parse().unwrap())).unwrap();
                    }
                    EngineCmd::SetProbeEndpoints(_) => break,
                    _ => {}
                }
            }
            queried
        });
        assert!(refresh_inferred_public_endpoint(
            &ctx,
            &cmd_tx,
            Instant::now() + Duration::from_secs(2),
            49999,
            None
        ));
        let queries = worker.join().unwrap();
        assert!(rx.try_iter().any(|line| line.contains("地址族不符——换下一个候选")));
        assert_eq!(queries.len(), 3);
        assert_eq!(queries[1], "[2001:4860::8888]:3478".parse().unwrap());
        assert_eq!(
            ctx.inner.lock().unwrap().last_published,
            vec!["8.8.8.8:49999", "[2001:4860::7777]:42650"]
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stun_wait_respects_cancel_and_round_deadline() {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let stop2 = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            stun_query_sync(
                &tx,
                "1.1.1.1:3478".parse().unwrap(),
                Instant::now() + Duration::from_secs(30),
                &stop2,
            )
        });
        let _pending = rx.recv_timeout(Duration::from_secs(1)).unwrap();
        stop.store(true, Ordering::SeqCst);
        let start = Instant::now();
        assert!(worker.join().unwrap().is_none());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(1)).unwrap(),
            EngineCmd::StunAbort
        ));
        let (tx, rx) = mpsc::channel();
        assert!(stun_query_sync(
            &tx,
            "1.1.1.1:3478".parse().unwrap(),
            Instant::now(),
            &AtomicBool::new(false)
        )
        .is_none());
        assert!(rx.try_recv().is_err(), "已过整轮期限不得发新探测");
    }

    #[test]
    fn first_probe_blackhole_is_bounded_and_cancellable() {
        let (_tx, rx) = mpsc::channel();
        let stopped = AtomicBool::new(false);
        let alive = AtomicBool::new(true);
        let start = Instant::now();
        assert!(wait_first_probe(
            &rx,
            Duration::from_millis(80),
            &stopped,
            &alive
        ));
        assert!(start.elapsed() < Duration::from_secs(1));
        stopped.store(true, Ordering::SeqCst);
        assert!(!wait_first_probe(
            &rx,
            Duration::from_secs(30),
            &stopped,
            &alive
        ));
        stopped.store(false, Ordering::SeqCst);
        alive.store(false, Ordering::SeqCst);
        assert!(!wait_first_probe(
            &rx,
            Duration::from_secs(30),
            &stopped,
            &alive
        ));
    }

    #[test]
    fn shutdown_with_relay_releases_public_udp_port() {
        let dir = tmp_dir("relay-shutdown");
        let relay = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let owner: Arc<Mutex<Option<std::sync::Weak<ServeEngine>>>> = Arc::new(Mutex::new(None));
        let owner2 = Arc::clone(&owner);
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let release_rx = Mutex::new(release_rx);
        let callback_done = Arc::new(AtomicBool::new(false));
        let callback_done2 = Arc::clone(&callback_done);
        let armed = Arc::new(AtomicBool::new(false));
        let armed2 = Arc::clone(&armed);
        let log: Logf = Arc::new(move |line| {
            if line.starts_with("公网端点：已按") && armed2.swap(false, Ordering::SeqCst) {
                started_tx.send(()).unwrap();
                release_rx.lock().unwrap().recv().unwrap();
                let engine = owner2.lock().unwrap().as_ref().unwrap().upgrade().unwrap();
                engine.stop_public_observation();
                engine.shutdown(Duration::ZERO);
                callback_done2.store(true, Ordering::SeqCst);
            }
        });
        let cfg = ServeConfig {
            state_dir: dir.clone(),
            listen_port: 0,
            quic_listen_port: Some(0),
            bind_iface: BindMode::Addr("127.0.0.1".parse().unwrap()),
            upnp: false,
            public_endpoint: "8.8.8.8:42650".into(),
            stun: String::new(),
            stun6: String::new(),
            files_port: 0,
            term_port: 0,
            speedtest_port: 0,
            dns_port: 0,
            relay: Some(relay_addr.to_string()),
            dns_probe_target: Vec::new(),
            stun_probe_target: Vec::new(),
            ..ServeConfig::default()
        };
        let engine = ServeEngine::start(cfg, Arc::clone(&log), Arc::clone(&log), log, None).unwrap();
        let addr = engine.quic_local_addr.expect("公共QUIC监听成功");
        assert_eq!(engine.relay_workers.lock().unwrap().len(), 2);
        *owner.lock().unwrap() = Some(Arc::downgrade(&engine));
        armed.store(true, Ordering::SeqCst);
        engine.kick_public_endpoint();
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        let mut stoppers = Vec::new();
        for index in 0..2 {
            let engine = Arc::clone(&engine);
            let done_tx = done_tx.clone();
            let done = Arc::clone(&callback_done);
            stoppers.push(std::thread::spawn(move || {
                if index == 0 {
                    engine.shutdown(Duration::ZERO);
                } else {
                    engine.stop_public_observation();
                }
                assert!(done.load(Ordering::SeqCst), "并发收工不得提前返回");
                engine.shutdown(Duration::ZERO);
                done_tx.send(()).unwrap();
            }));
        }
        assert!(done_rx.recv_timeout(Duration::from_millis(80)).is_err());
        release_tx.send(()).unwrap();
        for stopper in stoppers {
            stopper.join().unwrap();
        }
        let rebound = std::net::UdpSocket::bind(addr).expect("shutdown返回必须释放所有socket副本");
        assert_eq!(rebound.local_addr().unwrap(), addr);
        assert!(engine.observers.lock().unwrap().is_empty());
        assert!(engine.relay_workers.lock().unwrap().is_empty());
        drop(rebound);
        drop(engine);
        let _ = std::fs::remove_dir_all(dir);
    }

    fn log_sink() -> (Logf, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }), rx)
    }

    /// **判据（DNS-STREAM 批；代码门 r34 M2③ 的装配断言）**：`dns_intake_of` 两态——
    /// `dns = None`（DNS 未启用）⇒ 无入口、无行（该 tag 落 0x22）；`dns = Some` ⇒ 入口 + 就绪行
    /// （含虚拟端口 5300）。删掉装配接线（`service_intakes.dns = Some(tx)` 的上游）时本用例变红。
    #[test]
    fn dns_intake_assembles_only_when_dns_is_enabled() {
        let (logf, rx) = log_sink();
        assert!(dns_intake_of(None, &logf).is_none(), "DNS 未启用 ⇒ 无入口");
        assert!(rx.try_iter().next().is_none(), "DNS 未启用 ⇒ 无就绪行");

        let dir = std::env::temp_dir().join(format!("homeway-rs-dnsintake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("resolv.conf"), "nameserver 127.0.0.1:1\n").unwrap();
        let (dlogf, _drx) = log_sink();
        let (proxy, _ev) = DnsProxy::spawn(
            crate::server::dnsproxy::DnsConfig {
                resolv_path: dir.join("resolv.conf").to_string_lossy().into_owned(),
                ..Default::default()
            },
            Arc::clone(&dlogf),
            dlogf,
        );
        let got = dns_intake_of(Some(&proxy), &logf).expect("DNS 启用 ⇒ 应有入口");
        drop(got);
        let lines: Vec<String> = rx.try_iter().collect();
        assert!(
            lines.iter().any(|l| l.contains("dns 流腿就绪：STREAM tag=6")
                && l.contains("虚拟端口 5300")),
            "就绪行须在场且带虚拟端口：{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **判据（S1-7 的失败形态 / E-q4）**：`--upnp=false` ⇒ 打**失败行**（不静默缺席）
    /// 且公网 QUIC 端点被清空（token 只出 LAN/中继 QUIC 端点）。
    #[test]
    fn quic_upnp_disabled_prints_failure_line_and_clears_public() {
        let (logf, rx) = log_sink();
        let qep: SocketAddr = "127.0.0.1:42652".parse().unwrap();
        let (ctx, dir) = token_ctx(false, Some(qep), None, logf, &rx);
        // 先塞一个「上一轮的公网端点」——本函数必须把它清掉（映射未启用 = 不公布）
        ctx.inner.lock().unwrap().quic_pub = Some("203.0.113.7:42652".parse().unwrap());
        refresh_quic_public(&ctx, Instant::now() + PUBLIC_ROUND_BUDGET);
        let lines: Vec<String> = rx.try_iter().collect();
        assert_eq!(lines.len(), 1, "失败行必须打（且只此一行）：{lines:?}");
        assert!(
            lines[0].contains("UPnP：QUIC 端口 42652 映射 未启用（--upnp=false）"),
            "E-q4 失败行形态：{}",
            lines[0]
        );
        assert_eq!(ctx.inner.lock().unwrap().quic_pub, None, "未映射 ⇒ 公网 QUIC 端点缺席");
        // 无 QUIC 面（quic_ep=None）⇒ 不打行（没有端口可映射，不假装）
        let (logf2, rx2) = log_sink();
        let (ctx2, dir2) = token_ctx(false, None, None, logf2, &rx2);
        refresh_quic_public(&ctx2, Instant::now() + PUBLIC_ROUND_BUDGET);
        assert_eq!(rx2.try_iter().count(), 0, "无 QUIC 面时不打 E-q4 行");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    /// **判据（S1-8 / M5 改写）**：铸造的 token **全部端点 = QUIC 类**（内网 + 公网）+
    /// **RPK 字段**；E3 行（`端点：…`）带 `（QUIC 内网/公网）`。
    ///
    /// **M5 变更（登记）**：出口不再有 WG 端口 ⇒ 已公布公网端点（显式配置/UPnP+STUN 合流
    /// 产物）的类别由 `Direct` 改为 `Quic`（设计 §8.1-E3：「QUIC 类端点成为唯一公网/直连
    /// 承载类」）；`Direct` 类在**本出口铸出的 token 里不再出现**（旧 token 的 Direct 类
    /// 仍是客户端侧 G9 的可见失败路径，见 `quic_single_bearer_e2e`）。
    #[test]
    fn minted_token_carries_quic_endpoints_and_rpk() {
        let (logf, rx) = log_sink();
        let qep: SocketAddr = "127.0.0.1:42652".parse().unwrap();
        let rpk = [0x5Eu8; 32];
        let (ctx, dir) = token_ctx(false, Some(qep), Some(rpk), logf, &rx);
        ctx.inner.lock().unwrap().quic_pub = Some("203.0.113.7:42652".parse().unwrap());
        // published（公网 WG 端点）由参数给（真实来源 = UPnP/STUN 观测）
        print_client_token(&ctx, &["203.0.113.7:42650".to_owned()]);

        let lines: Vec<String> = rx.try_iter().collect();
        let tok_line = lines
            .iter()
            .find(|l| l.contains("客户端 token（"))
            .expect("E3 行必打");
        let eps_line = lines.iter().find(|l| l.starts_with("端点：")).expect("端点行必打");
        assert!(eps_line.contains("（QUIC 内网）"), "端点行含 QUIC 内网：{eps_line}");
        assert!(eps_line.contains("（QUIC 公网）"), "端点行含 QUIC 公网：{eps_line}");
        assert!(
            eps_line.contains("203.0.113.7:42650（QUIC 公网）"),
            "已公布公网端点同为 QUIC 类（M5 改写）：{eps_line}"
        );

        // 解出 token：QUIC 类 = 独立端口；rpk = 出口公钥
        let tok_str = tok_line.split("：").last().unwrap().trim().to_owned();
        let t = crate::token::decode(&tok_str).expect("铸出的 token 必可解");
        assert_eq!(t.rpk.map(|k| *k.as_bytes()), Some(rpk), "RPK 字段进 token");
        let quic: Vec<&str> = t
            .endpoints
            .iter()
            .filter(|e| e.kind == EndpointKind::Quic)
            .map(|e| e.addr.as_str())
            .collect();
        assert!(quic.contains(&"203.0.113.7:42652"), "公网 QUIC 端点：{quic:?}");
        // M5：LAN 端点与 quic_pub 都走 QUIC 端口；已公布公网端点（published）也归 QUIC 类
        // （其端口在生产路径 = 同一 QUIC 端口；本用例的输入 42650 是显式给的字面值）
        // ⚠️ LAN 地址**不得写死**（开发机 192.168.3.12 / CI 10.1.0.126 各异）：断言
        // 「存在一个 Quic 类端点，端口 = QUIC 端口，且既非回环也非已公布公网端点」。
        let lan_quic: Vec<&str> = t
            .endpoints
            .iter()
            .filter(|e| {
                e.kind == EndpointKind::Quic
                    && e.addr.ends_with(":42652")
                    && e.addr != "203.0.113.7:42652"
                    && !e.addr.starts_with("127.")
            })
            .map(|e| e.addr.as_str())
            .collect();
        assert!(
            !lan_quic.is_empty(),
            "须有 LAN QUIC 端点（非回环、非已公布；用 QUIC 端口 42652）：{quic:?}"
        );
        assert!(quic.contains(&"203.0.113.7:42650"), "已公布公网端点入 QUIC 类：{quic:?}");
        // M5：Direct 类端点整体退役（WG 端口不存在）——铸出的 token 里零命中
        assert!(
            t.endpoints.iter().all(|e| e.kind != EndpointKind::Direct),
            "WG 类（Direct）端点已退役：{:?}",
            t.endpoints.iter().map(|e| e.addr.as_str()).collect::<Vec<_>>()
        );
        let public = t
            .endpoints
            .iter()
            .find(|e| e.addr == "203.0.113.7:42650")
            .expect("已公布公网端点仍在（类别 = QUIC）");
        assert_eq!(public.kind, EndpointKind::Quic, "公网端点类别 = QUIC（M5 改写）");
        // 台账也带 rpk（serve token 走末行重铸 ⇒ 与运行期串一致）
        let back = ctx.st.last_token().unwrap().unwrap();
        assert_eq!(back.rpk.map(|k| *k.as_bytes()), Some(rpk), "台账末行带 rpk");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// M1 §1.1：QUIC 端口定案——显式优先，缺省 = `serve.listen + 1`（饱和：65535 不绕回）。
    #[test]
    fn quic_listen_port_defaults_to_listen_plus_one() {
        let mut cfg = ServeConfig { listen_port: 41641, ..ServeConfig::default() };
        assert_eq!(quic_listen_port(&cfg), 41642, "缺省 = listen + 1");
        cfg.quic_listen_port = Some(50000);
        assert_eq!(quic_listen_port(&cfg), 50000, "显式值优先");
        cfg.quic_listen_port = None;
        cfg.listen_port = u16::MAX;
        assert_eq!(quic_listen_port(&cfg), u16::MAX, "饱和不回绕（不得撞 0）");
    }

    /// M1 §12-② 的「出口侧产出」：RPK 种子从后端静态私钥**确定性**派生（重启不变 ⇒
    /// 客户端 token 里的钉定值不失效），且**不同私钥给不同种子**（域分离真在起作用）。
    #[test]
    fn quic_rpk_seed_is_deterministic_and_identity_bound() {
        let a = x25519_dalek::StaticSecret::from([0x11u8; 32]);
        let b = x25519_dalek::StaticSecret::from([0x22u8; 32]);
        let sa = crate::server::quic_rpk_seed(&a);
        let sa2 = crate::server::quic_rpk_seed(&a);
        let sb = crate::server::quic_rpk_seed(&b);
        assert_eq!(sa.as_bytes(), sa2.as_bytes(), "同身份 ⇒ 同种子");
        assert_ne!(sa.as_bytes(), sb.as_bytes(), "异身份 ⇒ 异种子");
        // 与 psk 域的分离：同输入不同标签必不同（防标签复制粘贴错）
        let psk = crate::psk::Psk::from(crate::token::Secret::from(a.to_bytes()));
        assert_ne!(sa.as_bytes(), psk.as_bytes(), "RPK 种子 ≠ WG PSK（标签不同）");
    }

    fn now_unix() -> u64 {
        1_800_000_000
    }

    /// Q-I F7（主判据）：`udpcap_loop` 等待三态——Ok(kick) 与 Timeout 立即重探
    /// （旧形态对 Timeout 再睡一个 interval ⇒ bindwatch 在位时实际周期 ~600s）；
    /// Disconnected 补睡一个节拍（防自旋，`--bind-interface none` 形态不变）。
    /// F2（代码门 M3②）：`probe_once` 用**配置目标**探——本地 UDP 桩同时答 DNS 探针与
    /// STUN Binding ⇒ flags 必含 `DNS:53` + 通用（非 53）+ PROBED（注入面真到消费点）。
    #[test]
    fn probe_once_uses_injected_targets() {
        let stub = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let stub_port = stub.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let mut buf = [0u8; 1500];
            loop {
                let Ok((n, from)) = stub.recv_from(&mut buf) else { return;
                };
                let req = &buf[..n];
                if n >= 20 && u16::from_be_bytes([req[0], req[1]]) == 0x0001 {
                    // STUN Binding Request → Success Response + XOR-MAPPED-ADDRESS(v4 127.0.0.1)
                    let txid = &req[8..20];
                    let cookie = 0x2112_A442u32.to_be_bytes();
                    let mut attr = vec![0x00, 0x01];
                    attr.extend_from_slice(&(stub_port ^ 0x2112).to_be_bytes());
                    let ip = [127u8, 0, 0, 1];
                    for (i, b) in ip.iter().enumerate() {
                        attr.push(b ^ cookie[i]);
                    }
                    let mut resp = Vec::new();
                    resp.extend_from_slice(&0x0101u16.to_be_bytes());
                    resp.extend_from_slice(&((attr.len() + 4) as u16).to_be_bytes());
                    resp.extend_from_slice(&cookie);
                    resp.extend_from_slice(txid);
                    resp.extend_from_slice(&0x0020u16.to_be_bytes());
                    resp.extend_from_slice(&(attr.len() as u16).to_be_bytes());
                    resp.extend_from_slice(&attr);
                    let _ = stub.send_to(&resp, from);
                } else {
                    // DNS 探针：只要求 ID 匹配 + 来源正确 ⇒ 回 12 字节头（ID 回显）
                    let mut resp = vec![0u8; 12];
                    if n >= 2 {
                        resp[0] = req[0];
                        resp[1] = req[1];
                    }
                    resp[2] = 0x81;
                    resp[3] = 0x80;
                    let _ = stub.send_to(&resp, from);
                }
            }
        });
        let target: SocketAddrV4 = format!("127.0.0.1:{stub_port}").parse().unwrap();
        let stats = Arc::new(ItcStats::default());
        let (dns_note, gen_note, _, _, flags) =
            probe_once(&stats, &mut 0, &mut 0, &[target], &[target]);
        assert_eq!(flags & UDPCAP_DNS, UDPCAP_DNS, "DNS:53 位应置（{dns_note}）");
        assert_eq!(flags & UDPCAP_GENERIC, UDPCAP_GENERIC, "通用位应置（{gen_note}）");
        assert_eq!(flags & UDPCAP_PROBED, UDPCAP_PROBED, "PROBED 位恒置");
        assert!(!dns_note.contains("不可用"), "配置目标应探通：{dns_note}");
    }

    #[test]
    fn udpcap_wait_three_states() {
        assert!(
            !udpcap_disconnected_backoff(Ok(())),
            "kick：立即重探（不补睡）"
        );
        assert!(
            !udpcap_disconnected_backoff(Err(mpsc::RecvTimeoutError::Timeout)),
            "到期：立即重探（旧形态多睡一拍 = 周期翻倍）"
        );
        assert!(
            udpcap_disconnected_backoff(Err(mpsc::RecvTimeoutError::Disconnected)),
            "看护未起：补睡一个节拍（按 300s 原节拍继续，不空转）"
        );
    }

    // ---------- S1b/M2 S1：准入（hr-reg4 四帧 + 连接绑定）/ 入站接线 / 出站分流 ----------

    /// `hr-reg4` 用 secret（与既有 reg2 用例的 secret 无关，避免互相干扰）。
    const REG3_SECRET: [u8; 32] = [0x6Au8; 32];

    /// 行收集器（断言判据行）。
    fn line_sink() -> (Arc<Mutex<Vec<String>>>, Logf) {
        let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let l2 = Arc::clone(&lines);
        let logf: Logf = Arc::new(move |s: &str| {
            l2.lock().unwrap().push(s.to_string());
        });
        (lines, logf)
    }

    fn lines_of(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
        lines.lock().unwrap().clone()
    }

    fn reg4_table(logf: &Logf) -> DeviceTable {
        DeviceTable::new(
            vec![REG3_SECRET],
            TableConfig { max_devices: 4, ..Default::default() },
            Arc::clone(logf),
        )
    }

    /// 墙钟（`admit_reg4` 内部取 `SystemTime::now()`——时间窗用例必须用真时刻，
    /// 不能用既有用例的固定 1_800_000_000）。
    fn real_now_unix() -> u64 {
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("墙钟在纪元后")
            .as_secs()
    }

    /// 组 + 解一帧 `hr-reg4` 准入 Proof（走本仓唯一的组帧实现——测试不另写一份标签顺序）。
    fn reg4_proof(
        secret: &[u8; 32],
        pubkey: &[u8; 32],
        dev_tag: &[u8; 8],
        exporter: &[u8; 32],
        ts: u64,
    ) -> homeway_quic::Reg4Frame {
        let nonce = homeway_quic::Nonce::from_bytes([0x5C; 16]);
        let raw =
            homeway_quic::ProofFrame::encode(secret, pubkey, dev_tag, ts, &nonce, exporter);
        homeway_quic::Reg4Frame::Proof(
            homeway_quic::ProofFrame::parse(&raw).expect("本仓组帧必可解"),
        )
    }

    /// 组 + 解一帧 `hr-reg4` 刷新帧（另一 MAC 域）。
    fn reg4_refresh(
        secret: &[u8; 32],
        pubkey: &[u8; 32],
        dev_tag: &[u8; 8],
        exporter: &[u8; 32],
        ts: u64,
    ) -> homeway_quic::Reg4Frame {
        let raw = homeway_quic::RefreshFrame::encode(secret, pubkey, dev_tag, ts, exporter);
        homeway_quic::Reg4Frame::Refresh(
            homeway_quic::RefreshFrame::parse(&raw).expect("本仓组帧必可解"),
        )
    }

    /// **判据（S1-3）**：合法 Proof ⇒ 采纳（`Accepted` + 派生地址回填）+ `peer: +` 判据行
    /// + 分流键（`tun_ip → pubkey`）真落（`table.register` 的原路径）。
    #[test]
    fn admit_reg4_accepts_valid_frame() {
        let (lines, logf) = line_sink();
        let mut table = reg4_table(&logf);
        let mut tun_owner = std::collections::HashMap::new();
        let pubkey = [0x21u8; 32];
        let dev_tag = [0x22u8; 8];
        let exporter = [0x23u8; 32];
        let frame = reg4_proof(&REG3_SECRET, &pubkey, &dev_tag, &exporter, real_now_unix());

        match admit_reg4(&frame, &exporter, &mut table, None, &mut tun_owner, &logf) {
            homeway_quic::Reg4Verdict::Accepted { tunnel_ip, tun_ip } => {
                let sec = crate::token::Secret::from(REG3_SECRET);
                assert_eq!(tunnel_ip, crate::tunnel_addr::derive_tunnel_ip(&sec, &pubkey));
                assert_eq!(tun_ip, crate::tunnel_addr::derive_tun_ip(&sec, &pubkey));
                assert_eq!(tun_owner.get(&tun_ip), Some(&pubkey), "分流键已落（M5 §3.2-bis）");
            }
            other => panic!("合法 Proof 必须采纳：{other:?}"),
        }
        assert_eq!(table.len(), 1);
        let ls = lines_of(&lines);
        assert!(
            ls.iter().any(|l| l.starts_with("peer: + ") && l.contains(&pub_short(&pubkey))),
            "应有 `peer: +` 判据行（table.register 原路径）：{ls:?}"
        );
    }

    /// **判据（S1-3/S1-7 的核心）**：**同帧换连接（重放）必须失败**——MAC 混入了该连接的
    /// TLS exporter，换连接后 exporter 不同 ⇒ 校验不过 ⇒ `why=MacMismatch`（**类型化**，
    /// 出口面据此打 `hr-reg4 MAC 不符`）且**不产生任何登记副作用**；
    /// 对照：同一帧在原连接上（exporter 一致）⇒ 通过。
    #[test]
    fn admit_reg4_rejects_frame_replayed_on_another_connection() {
        let (lines, logf) = line_sink();
        let pubkey = [0x31u8; 32];
        let dev_tag = [0x32u8; 8];
        let exporter_a = [0xA1u8; 32];
        let frame = reg4_proof(&REG3_SECRET, &pubkey, &dev_tag, &exporter_a, real_now_unix());

        // 重放：同一帧换到 exporter_b（另一条连接）上验
        let mut table = reg4_table(&logf);
        let mut tun_owner = std::collections::HashMap::new();
        let v = admit_reg4(&frame, &[0xB2u8; 32], &mut table, None, &mut tun_owner, &logf);
        assert!(
            matches!(v, homeway_quic::Reg4Verdict::Rejected { why: homeway_quic::RejectWhy::MacMismatch }),
            "重放必须归 MacMismatch（出口面按它打归因行）：{v:?}"
        );
        assert!(tun_owner.is_empty(), "被拒不得落分流键");
        assert_eq!(table.len(), 0, "被拒不得进设备表");
        let ls = lines_of(&lines);
        assert!(
            ls.iter().any(|l| l.contains("hr-reg4 MAC 不符")),
            "应有归因行（MAC 不符——含换连接重放）：{ls:?}"
        );
        assert!(
            !ls.iter().any(|l| l.starts_with("peer: + ")),
            "被拒不得打 `peer: +`：{ls:?}"
        );

        // 对照：原连接（exporter_a）⇒ 通过
        let mut table2 = reg4_table(&logf);
        let v2 = admit_reg4(&frame, &exporter_a, &mut table2, None, &mut Default::default(), &logf);
        assert!(
            matches!(v2, homeway_quic::Reg4Verdict::Accepted { .. }),
            "同连接上同一帧应通过：{v2:?}"
        );
    }

    /// 坏 MAC（错 secret / 篡改字段）⇒ `MacMismatch` + 归因行；不得留下任何登记副作用。
    #[test]
    fn admit_reg4_rejects_bad_mac() {
        let (lines, logf) = line_sink();
        let mut table = reg4_table(&logf);
        let mut tun_owner = std::collections::HashMap::new();
        // ① 别的 secret 组帧（不是本出口的 token）
        let other = reg4_proof(&[0x99u8; 32], &[0x41; 32], &[0x42; 8], &[0x43; 32], real_now_unix());
        assert!(matches!(
            admit_reg4(&other, &[0x43u8; 32], &mut table, None, &mut tun_owner, &logf),
            homeway_quic::Reg4Verdict::Rejected { why: homeway_quic::RejectWhy::MacMismatch }
        ));
        // ② 合法帧篡改 mac 一位
        let mut tampered =
            match reg4_proof(&REG3_SECRET, &[0x44; 32], &[0x45; 8], &[0x46; 32], real_now_unix()) {
                homeway_quic::Reg4Frame::Proof(f) => f,
                other => panic!("本仓组帧必是 Proof：{other:?}"),
            };
        tampered.mac[0] ^= 1;
        assert!(matches!(
            admit_reg4(
                &homeway_quic::Reg4Frame::Proof(tampered),
                &[0x46u8; 32],
                &mut table,
                None,
                &mut tun_owner,
                &logf
            ),
            homeway_quic::Reg4Verdict::Rejected { why: homeway_quic::RejectWhy::MacMismatch }
        ));
        assert!(tun_owner.is_empty());
        assert_eq!(table.len(), 0);
        assert!(lines_of(&lines).iter().filter(|l| l.contains("hr-reg4 MAC 不符")).count() >= 2);
    }

    /// **判据（S1-1 的域分隔，引擎面）**：把准入 Proof 的字节当**刷新帧**送去验 ⇒ 域标签
    /// 不同 ⇒ `MacMismatch`（两帧不可互相冒充）；真刷新帧 ⇒ 采纳。
    #[test]
    fn admit_reg4_separates_proof_and_refresh_domains() {
        let (lines, logf) = line_sink();
        let pubkey = [0x61u8; 32];
        let dev_tag = [0x62u8; 8];
        let exporter = [0x63u8; 32];
        let ts = real_now_unix();
        // 先让设备在册（刷新帧的语义前提）
        let mut table = reg4_table(&logf);
        let mut tun_owner = std::collections::HashMap::new();
        let proof = reg4_proof(&REG3_SECRET, &pubkey, &dev_tag, &exporter, ts);
        assert!(matches!(
            admit_reg4(&proof, &exporter, &mut table, None, &mut tun_owner, &logf),
            homeway_quic::Reg4Verdict::Accepted { .. }
        ));

        // ① 把 Proof 的 MAC 塞进刷新帧（同字段同 exporter）⇒ 域不符 ⇒ 拒
        let homeway_quic::Reg4Frame::Proof(pf) = proof else { panic!("Proof 变体") };
        let mut raw = homeway_quic::RefreshFrame::encode(&REG3_SECRET, &pubkey, &dev_tag, ts, &exporter);
        raw[50..66].copy_from_slice(&pf.mac);
        let forged = homeway_quic::Reg4Frame::Refresh(
            homeway_quic::RefreshFrame::parse(&raw).expect("可解"),
        );
        let v = admit_reg4(&forged, &exporter, &mut table, None, &mut tun_owner, &logf);
        assert!(
            matches!(v, homeway_quic::Reg4Verdict::Rejected { why: homeway_quic::RejectWhy::MacMismatch }),
            "Proof 的 MAC 不得在刷新域成立：{v:?}"
        );

        // ② 真刷新帧 ⇒ 采纳（同 devTag 同 pubkey ⇒ 表内 Refreshed 路径）
        let refresh = reg4_refresh(&REG3_SECRET, &pubkey, &dev_tag, &exporter, ts);
        let v2 = admit_reg4(&refresh, &exporter, &mut table, None, &mut tun_owner, &logf);
        assert!(matches!(v2, homeway_quic::Reg4Verdict::Accepted { .. }), "{v2:?}");
        assert_eq!(table.len(), 1, "刷新不新增设备");
        assert!(
            lines_of(&lines).iter().any(|l| l.contains("refresh (idle=")),
            "刷新走表内 `peer: ~` 原路径"
        );
    }

    /// **判据（S2-2 的第三道前置 / 设计 §1.4 步骤 6 ② / r14 F25）**：刷新帧必须**表内仍在册**
    /// ——已淘汰（GC 摘除）设备的刷新帧（MAC 完全合法、exporter 一致、ts 新鲜）必须被拒，
    /// 且**不得把设备重新 Add 回表**（淘汰语义不被一个刷新帧抹平）；对照：完整准入（`Proof`）
    /// 是**合法复位**路径，同一设备重新走四帧仍可登记。
    #[test]
    fn admit_reg4_refresh_does_not_resurrect_evicted_device() {
        let (lines, logf) = line_sink();
        let pubkey = [0x71u8; 32];
        let dev_tag = [0x72u8; 8];
        let exporter = [0x73u8; 32];
        let ts = real_now_unix();
        // 表：ttl = 1ms（能真回收），容量 4（不触发表满路径）
        let mut table = DeviceTable::new(
            vec![REG3_SECRET],
            TableConfig { max_devices: 4, ttl: Duration::from_millis(1), ..Default::default() },
            Arc::clone(&logf),
        );
        let mut tun_owner = std::collections::HashMap::new();
        let proof = reg4_proof(&REG3_SECRET, &pubkey, &dev_tag, &exporter, ts);
        assert!(matches!(
            admit_reg4(&proof, &exporter, &mut table, None, &mut tun_owner, &logf),
            homeway_quic::Reg4Verdict::Accepted { .. }
        ));
        assert_eq!(table.len(), 1);

        // 淘汰（TTL 回收）：表内已无该设备
        let evicted = table.gc(SystemTime::now() + Duration::from_secs(1));
        assert_eq!(evicted.len(), 1, "GC 必须真回收一条：{evicted:?}");
        assert_eq!(table.len(), 0, "设备已不在册");

        // 刷新帧（同 secret / 同 exporter / ts 在 ±90s 窗内）⇒ 仍必须被拒 + 不 resurrect
        // （记行基线：只断言**本道拒绝**不新增表内行——`peer: +`/`peer: -` 是前两步的合法行）
        let lines_before = lines_of(&lines).len();
        let refresh = reg4_refresh(&REG3_SECRET, &pubkey, &dev_tag, &exporter, ts);
        assert!(
            refresh.mac_matches(&REG3_SECRET, &exporter),
            "MAC 必须成立——本用例证的是「MAC 之外还有一道」"
        );
        let v = admit_reg4(&refresh, &exporter, &mut table, None, &mut tun_owner, &logf);
        assert!(
            matches!(
                v,
                homeway_quic::Reg4Verdict::Rejected {
                    why: homeway_quic::RejectWhy::RefreshNotRegistered
                }
            ),
            "已淘汰设备的刷新帧必须归 RefreshNotRegistered：{v:?}"
        );
        assert_eq!(table.len(), 0, "刷新帧不得把设备 Add 回表（resurrect）");
        assert!(
            table.reject_counts().is_empty(),
            "本道拒绝不进表内拒绝计数（E9 输入集不被污染）：{:?}",
            table.reject_counts()
        );
        let new_lines = lines_of(&lines)[lines_before.min(lines_of(&lines).len())..].to_vec();
        assert!(
            !new_lines
                .iter()
                .any(|l| l.starts_with("peer: + ") || l.contains("refresh (idle=")),
            "被拒刷新不得打表内登记行（新增行应为空）：{new_lines:?}"
        );

        // 对照：**完整准入**（Proof）是合法复位路径 ⇒ 同一设备可重新登记
        let v2 = admit_reg4(&proof, &exporter, &mut table, None, &mut Default::default(), &logf);
        assert!(
            matches!(v2, homeway_quic::Reg4Verdict::Accepted { .. }),
            "完整准入必须仍可登记（resurrect 只许经四帧准入，不许经刷新帧）：{v2:?}"
        );
        assert_eq!(table.len(), 1, "完整准入后设备重新在册");
    }

    /// 时间窗**仍由 `table.register` 原路径承载**（重建 v2 报文的意义）：超窗的 Proof
    /// MAC 验得过、但登记被拒 ⇒ `why=EngineRejected`（**出口面据此打「引擎裁决拒绝」**）
    /// + 表内 `peer: ! reject reason=no-token`（r14 F1 的双面断言）。
    #[test]
    fn admit_reg4_still_enforces_reg_window() {
        let (lines, logf) = line_sink();
        let mut table = reg4_table(&logf);
        let pubkey = [0x51u8; 32];
        let dev_tag = [0x52u8; 8];
        let exporter = [0x53u8; 32];
        let stale_ts = real_now_unix() - 3600; // 远超 ±90s
        let frame = reg4_proof(&REG3_SECRET, &pubkey, &dev_tag, &exporter, stale_ts);
        assert!(
            frame.mac_matches(&REG3_SECRET, &exporter),
            "MAC 覆盖域含 ts——超窗帧的 MAC 仍成立"
        );
        let v = admit_reg4(&frame, &exporter, &mut table, None, &mut Default::default(), &logf);
        assert!(
            matches!(
                v,
                homeway_quic::Reg4Verdict::Rejected {
                    why: homeway_quic::RejectWhy::EngineRejected {
                        // M3 §4：超窗（ts 超 ±90s 窗）在表内归 `no-token` ⇒ **凭证桶**
                        class: homeway_quic::EngineRejectClass::Credential,
                    },
                }
            ),
            "超窗必须归 EngineRejected（MAC 过、窗拒）：{v:?}"
        );
        assert_eq!(table.len(), 0);
        let ls = lines_of(&lines);
        assert!(
            ls.iter().any(|l| l.contains("reject reason=no-token")),
            "超窗归因走表内原路径（no-token——与今日 reg2 同归因）：{ls:?}"
        );
        assert!(
            !ls.iter().any(|l| l.contains("hr-reg4 MAC 不符")),
            "超窗不是 MAC 类拒绝（归因必须可分辨）：{ls:?}"
        );
    }

    /// **判据（S1-4 的引擎半边）**：`ExitInbound::Packet` 直投 `intercept.on_plain`——
    /// 合成 DNS 查询包（UDP 53）应被拦截层真消化（`udp intercept: 会话 #1` 建立行 = 证据）。
    #[test]
    fn on_quic_inbound_packet_reaches_intercept() {
        let (lines, logf) = line_sink();
        let (_, table_logf) = line_sink();
        let mut table = DeviceTable::new(
            vec![[0x7Au8; 32]],
            TableConfig { max_devices: 2, ..Default::default() },
            table_logf,
        );
        let mut itc = Interceptor::attach(
            ItcConfig {
                tunnel_ip: DEFAULT_TUNNEL_IP,
                dns: None,
                dns_events: None,
                dns_resolve_port: 0,
                tx_shape: None, // 单测直通面（与仓内既有用法一致）
                logf: Arc::clone(&logf),
            },
            Arc::new(ItcStats::default()),
        );
        let pkt = udp_pkt(
            Ipv4Addr::new(100, 64, 9, 2),
            Ipv4Addr::new(8, 8, 8, 8),
            50000,
            53,
            &dns_query(),
        );
        let _ = on_quic_inbound(
            homeway_quic::ExitInbound::Packet { dev: [0x01; 8], pkt },
            &mut table,
            &mut itc,
            None,
            &mut Default::default(),
            &logf,
        );
        let ls = lines_of(&lines);
        assert!(
            ls.iter().any(|l| l.starts_with("udp intercept: 会话 #1 ") && l.contains("建立")),
            "intercept 必须真收到并建会话：{ls:?}"
        );
    }

    /// IPv4 头 + UDP 头（入站/分流用例的构造件）。
    fn udp_pkt(src: Ipv4Addr, dst: Ipv4Addr, sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0u8; 20 + 8 + payload.len()];
        let total = p.len() as u16;
        p[0] = 0x45;
        p[2..4].copy_from_slice(&total.to_be_bytes());
        p[8] = 64; // TTL
        p[9] = 17; // UDP
        p[12..16].copy_from_slice(&src.octets());
        p[16..20].copy_from_slice(&dst.octets());
        p[20..22].copy_from_slice(&sport.to_be_bytes());
        p[22..24].copy_from_slice(&dport.to_be_bytes());
        p[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
        p[28..].copy_from_slice(payload);
        p
    }

    /// 最小 DNS 查询（12B 头 + 1 问题；够过 demux 与 :=53 判定）。
    fn dns_query() -> Vec<u8> {
        let mut q = vec![0u8; 12];
        q[0..2].copy_from_slice(&0x1234u16.to_be_bytes()); // ID
        q[5] = 1; // QDCOUNT
        q.extend_from_slice(b"\x07example\x03com\x00");
        q.extend_from_slice(&1u16.to_be_bytes()); // A
        q.extend_from_slice(&1u16.to_be_bytes()); // IN
        q
    }

    /// **判据（S1-5 的判定面 / M5 §3.2-bis 方案 (a)）**：出站分流键 = 引擎侧
    /// `tun_ip → pubkey` 映射（数据源 = `DevOp`）——`tun_ip`（hw-app）唯一命中；
    /// `tunnel_ip`（hw-tun，服务面自带连接）与未知地址都不命中。
    #[test]
    fn tun_owner_map_is_the_quic_split_key() {
        let pubkey = [0x61u8; 32];
        let tun_ip = Ipv4Addr::new(100, 64, 3, 2);
        let tunnel_ip = Ipv4Addr::new(100, 64, 3, 1);
        let mut owner: std::collections::HashMap<Ipv4Addr, [u8; 32]> =
            std::collections::HashMap::new();
        apply_dev_ops(
            vec![DevOp::Add { pubkey, psk: [0x62; 32], tunnel_ip, tun_ip }],
            &mut owner,
            None,
        );
        assert_eq!(owner.get(&tun_ip), Some(&pubkey), "tun_ip（hw-app）⇒ QUIC 档键");
        assert_eq!(owner.get(&tunnel_ip), None, "tunnel_ip（hw-tun，服务面自带连接）⇒ 不归");
        assert_eq!(owner.get(&Ipv4Addr::new(100, 64, 3, 9)), None, "未知 ⇒ 不归");
        // 摘除（淘汰/吊销/轮换）⇒ 键随之消失
        apply_dev_ops(vec![DevOp::Remove { pubkey }], &mut owner, None);
        assert!(owner.is_empty(), "摘除后分流键清空");
    }

    /// **判据（S1-5 的回落面；M5 改判）**：`route_out` 在**无绑定**时 = 丢 + 计数 + 记行
    /// （原「WG `device.encapsulate` 原样」的兜底面随 WG 删除；§3.2-bis ②）。
    #[test]
    fn route_out_drops_and_counts_when_unbound() {
        let (lines, logf) = line_sink();
        let quic = homeway_quic::ExitQuic::start(
            std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("回环可绑"),
            homeway_quic::ExitQuicConfig::new(homeway_quic::Ed25519Seed::from_bytes([0x11; 32]), 4),
            Arc::clone(&logf),
        )
        .expect("端点可起");
        let dst = Ipv4Addr::new(100, 64, 4, 2);
        let tx = vec![udp_pkt(dst, dst, 40000, 53, b"x")];
        let mut dropped = 0u64;
        // ① 空映射（设备未登记）：丢 + 计数
        route_out(&tx, &Default::default(), Some(&quic), &mut dropped, &logf);
        assert_eq!(dropped, 1, "无绑定 ⇒ 丢弃计数 +1");
        // ② 有绑定但 QUIC 面报 Unbound（未登记/连接死）：同样计丢（不静默）
        let mut owner = std::collections::HashMap::new();
        owner.insert(dst, [0x71u8; 32]);
        route_out(&tx, &owner, Some(&quic), &mut dropped, &logf);
        assert_eq!(dropped, 2, "面侧 Unbound ⇒ 丢弃计数 +1（不再回落 WG）");
        let ls = lines_of(&lines);
        assert!(
            ls.iter().any(|l| l.contains("出站丢弃（无 QUIC 绑定")),
            "丢弃必须可见（首 3 记行）：{ls:?}"
        );
        assert!(quic.stop_within(Instant::now() + Duration::from_secs(2)), "面应能收工");
    }

    /// P0-2 端到端不变量（M5 形态）：表满淘汰后 `apply_dev_ops` **真摘掉** victim 的
    /// 分流键——`tun_ip → pubkey` 不再随淘汰单调增长（表—引擎状态一致）。
    #[test]
    fn eviction_removes_tun_owner() {
        let secret = [0x66u8; 32];
        let logf: Logf = Arc::new(|_: &str| {});
        let mut table = DeviceTable::new(
            vec![secret],
            TableConfig { max_devices: 2, ..Default::default() },
            Arc::clone(&logf),
        );
        let mut tun_owner = std::collections::HashMap::new();
        let base = now();
        // d1：已超宽限（10 分钟 + 60s 前注册）
        let old_ts = now_unix() - (crate::server::table::DEFAULT_GRACE.as_secs() + 60);
        let (_, ops) = table
            .register(&reg_bytes(&secret, &[1; 32], &[1; 8], old_ts), base - Duration::from_secs(660))
            .unwrap();
        apply_dev_ops(ops, &mut tun_owner, None);
        let (_, ops) = table.register(&reg_bytes(&secret, &[2; 32], &[2; 8], now_unix()), base).unwrap();
        apply_dev_ops(ops, &mut tun_owner, None);
        assert_eq!(tun_owner.len(), 2, "两台设备 ⇒ 两个分流键");

        // d3 触发淘汰：ops = [Remove(d1.pub), Add(d3.pub)]
        let (_, ops) = table.register(&reg_bytes(&secret, &[3; 32], &[3; 8], now_unix()), base).unwrap();
        assert!(matches!(ops[0], DevOp::Remove { pubkey } if pubkey == [1; 32]));
        apply_dev_ops(ops, &mut tun_owner, None);
        assert_eq!(tun_owner.len(), 2, "淘汰一个加一个——键表不单调增长");
        assert!(!tun_owner.values().any(|p| p == &[1; 32]), "victim 的键已被真摘除");
        assert!(tun_owner.values().any(|p| p == &[2; 32]));
        assert!(tun_owner.values().any(|p| p == &[3; 32]));
    }

    /// 克隆场景的**弱不变量**（M5 形态）：同公钥挂两个 devTag 时，分流键按 `tun_ip`
    /// 分列（两键共存，QUIC 面按 pubkey 绑定同一连接）；按 pubkey 发 Remove 会把两条
    /// 键一并摘掉——与 Go `peers.go` 同序同缺口（共享），故断言写成弱不变量而非
    /// 「表—键一一对应」。
    #[test]
    fn clone_same_pubkey_shares_tun_owner_removal() {
        let secret = [0x67u8; 32];
        let logf: Logf = Arc::new(|_: &str| {});
        let mut table = DeviceTable::new(
            vec![secret],
            TableConfig { max_devices: 2, ..Default::default() },
            Arc::clone(&logf),
        );
        let mut tun_owner = std::collections::HashMap::new();
        let base = now();
        // 同公钥 [9;32]、两个 devTag（克隆形态：应用数据被复制）；d1 超宽限以便被淘汰
        let old_ts = now_unix() - (crate::server::table::DEFAULT_GRACE.as_secs() + 60);
        let (_, ops) = table
            .register(&reg_bytes(&secret, &[9; 32], &[1; 8], old_ts), base - Duration::from_secs(660))
            .unwrap();
        apply_dev_ops(ops, &mut tun_owner, None);
        let (_, ops) = table.register(&reg_bytes(&secret, &[9; 32], &[2; 8], now_unix()), base).unwrap();
        apply_dev_ops(ops, &mut tun_owner, None);
        assert_eq!(table.len(), 2, "两个 devTag 都在表（克隆合法）");
        // `derive_tun_ip` 的输入是 (secret, pubkey)——**不含 devTag** ⇒ 同 pubkey 的两个
        // devTag 派生出同一个 tun_ip ⇒ 分流键只有一条（QUIC 面同样按 pubkey 绑定单连接）
        assert_eq!(tun_owner.len(), 1, "同 pubkey ⇒ 同 tun_ip ⇒ 单键");
        // 淘汰其一 ⇒ Remove(pubkey [9;32]) ⇒ 两条键一并摘（已知共享差异）
        let (_, ops) = table.register(&reg_bytes(&secret, &[3; 32], &[3; 8], now_unix()), base).unwrap();
        assert!(matches!(ops[0], DevOp::Remove { pubkey } if pubkey == [9; 32]));
        apply_dev_ops(ops, &mut tun_owner, None);
        assert!(!tun_owner.values().any(|p| p == &[9; 32]), "该 pubkey 的键全摘（弱不变量）");
    }
}

#[cfg(test)]
mod relay_send_handle_regression {
    use super::*;
    #[test]
    fn sync_replaces_send_fd_when_control_replay_reopens_same_remote_and_cookie() {
        let logf: crate::Logf=Arc::new(|_|{});
        let q=homeway_quic::ExitQuic::start(std::net::UdpSocket::bind("127.0.0.1:0").unwrap(),homeway_quic::ExitQuicConfig::new(homeway_quic::Ed25519Seed::from_bytes([46;32]),4),Arc::clone(&logf)).unwrap();
        let sink=std::net::UdpSocket::bind("127.0.0.1:0").unwrap(); let remote=sink.local_addr().unwrap();
        let mut bind=ServerBind::new(logf); let admitted=crate::relaywire::admitted_payload(9,&[6;16],&[7;32]);
        bind.register_leg_with_admission(9,remote,b"LEGUP",&admitted).unwrap();
        let (generation,old_local)=bind.leg_generation(remote).unwrap();
        sync_quic_legs(&mut bind,Some(&q)); assert!(q.leg_matches(remote,old_local,generation));
        bind.register_leg_with_admission(9,remote,b"LEGUP",&admitted).unwrap();
        let (_,new_local)=bind.leg_generation(remote).unwrap(); assert_ne!(old_local,new_local);
        sync_quic_legs(&mut bind,Some(&q));
        assert!(q.leg_matches(remote,new_local,generation),"同remote重拨必须替换发送fd");
        assert!(!q.leg_matches(remote,old_local,generation));
        assert!(q.stop_within(Instant::now()+Duration::from_secs(2)));
    }
}
