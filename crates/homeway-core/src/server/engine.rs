//! 出口装配与驱动循环（R3；语义真源 `internal/server/serve.go` 的 Start/Run +
//! `publicendpoint.go` + `udpcap.go`——裁剪面见 R3-design §0：relay/term/DDNS/
//! daemon 控制面不在本期）。
//!
//! 线程面（设计 §1.1）：装配线程（Start）→ 一条 WG 驱动线程（独占 device/拦截栈/
//! 设备表/ServerBind——无锁热路径）+ DNS worker 池（dnsproxy 自带）+ 拦截 worker 池
//! （intercept 自带）+ files/speedtest UDS 服务线程 + 公网端点观测线程 + udpcap 探测
//! 线程。观测线程经 `EngineCmd` 与驱动线程交互（STUN 走**同监听 socket**——
//! bind.stun_query；caps/probe 端点回填）。
//!
//! 收工序（Go Shutdown D5）：① 停新流（HaltNew）→ ② 关 UDS listeners → ③ 过境存量
//! 有界宽限（Drain）→ ④ 关 WG UDP → ⑤ DNS/观测收工 + UPnP 缩租 + 服务收工。

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::MetadataExt as _;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::server::bind::{Inbound, ServerBind, TxStatsSnap};
use crate::server::device::{Device, InboundOut, PeerConfig};
use crate::server::dnsproxy::{DnsConfig, DnsProxy};
use crate::server::egress::{self, IfaceInfo};
use crate::server::intercept::{self, Config as ItcConfig, Interceptor, Stats as ItcStats};
use crate::server::state::State;
use crate::server::table::{DevOp, DeviceTable, TableConfig};
use crate::token::{Endpoint, EndpointKind};
use crate::Logf;

/// 默认内部地址/端口（Go serve.go 常量同源）。
pub const DEFAULT_TUNNEL_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 255, 1);
pub const DEFAULT_FILES_PORT: u16 = 7802;
pub const DEFAULT_TERM_PORT: u16 = 7724;
pub const DEFAULT_SPEEDTEST_PORT: u16 = 7803;
pub const DEFAULT_DNS_PORT: u16 = 5300;
pub const DEFAULT_LISTEN_PORT: u16 = 41641;
/// 公网端点刷新（成/败）。
const PUBLIC_REFRESH_OK: Duration = Duration::from_secs(600);
const PUBLIC_REFRESH_FAIL: Duration = Duration::from_secs(120);
/// udpcap 重探间隔（Go udpCapInterval——设计表误写 10min，Go 真源 5min）。
const UDPCAP_INTERVAL: Duration = Duration::from_secs(300);

/// WG socket 钉哪张物理网卡（BindMode）。
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
    pub stun: String,
    /// v6 路径校验 STUN（Go `STUN6`；空 = 跳过 v6 公布。默认同 Go = cloudflare）。
    pub stun6: String,
    pub files_root: Option<PathBuf>,
    pub verbose: bool,
    pub build: String,
    /// serve --relay / config serve.relay：rl1 token 或裸 host:port（None = 不起注册腿）。
    pub relay: Option<String>,
    /// DDNS 裸域名（可多条，`[[serve.ddns]]` / `--ddns`）：token 叠加 `域:端口` 条目
    /// （不解析不踢除；域名记录由用户 DDNS 设施维护）+ 自检随公网端点探测同拍跑。
    pub ddns: Vec<String>,
    /// 发送整形/pacing 的 config 覆盖（D-3 反过拟合约束 3：`[serve.tx_shape]` 节；
    /// None = 全默认。env 测试缝的优先级在 `tx_shape_resolve` 内——env > config > 默认）。
    pub tx_shape_cfg: Option<crate::server::intercept::TxShapeCfg>,
}

impl Default for ServeConfig {
    fn default() -> Self {
        Self {
            state_dir: PathBuf::from("."),
            listen_port: DEFAULT_LISTEN_PORT,
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
            stun: "stun.cloudflare.com:3478".to_owned(),
            stun6: "stun.cloudflare.com:3478".to_owned(),
            files_root: None,
            verbose: false,
            build: "homeway-rs-dev".to_owned(),
            relay: None,
            ddns: Vec::new(),
            tx_shape_cfg: None,
        }
    }
}

/// 停机宽限（Go StopGrace 同值——「stop 后还能通最多 10s」的显式语义）。
pub const STOP_GRACE: Duration = Duration::from_secs(10);

/// 观测线程 → 驱动线程的命令。
pub enum EngineCmd {
    Stop { grace: Duration },
    /// 同监听 socket 的 STUN 观测（应答经 reply 回执——None = 无应答/不合法）。
    StunQuery { server: SocketAddr, reply: Sender<Option<SocketAddr>> },
    StunAbort,
    /// 探测应答的能力位（udpcap 结论）。
    SetCaps(u8),
    /// 探测应答的端点列表段（已公布公网端点）。
    SetProbeEndpoints(Vec<SocketAddr>),
    /// 公网端点立即重测（换网事件——绑卡看护的 onChange）。
    KickPublicEndpoint,
    /// 把 WG socket 重钉到指定网卡（绑卡看护换卡/重挑；驱动线程独占 socket）。
    Repin {
        index: u32,
        name: String,
        reply: Sender<std::io::Result<()>>,
    },
    /// serve.status 观测缝（B0-2b）：设备表 brief 快照（表 = 驱动线程独占——快照
    /// 必须经驱动线程取；拦截计数是共享原子、不经此路）。
    StatusQuery { reply: Sender<Vec<EnginePeerBrief>> },
    /// 控制面 SESSION 通告 → 向中继数据口拨腿（R4）。
    LegRegister { id: u64, remote: SocketAddr, marker: Vec<u8> },
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
}

/// 出口引擎句柄（装配线程持有；stop 收工）。
pub struct ServeEngine {
    cmd_tx: Sender<EngineCmd>,
    driver: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 驱动线程在世位（线程出口清零——panic 也清：supervisor 的存活判据）。
    driver_alive: Arc<AtomicBool>,
    /// 拦截计数共享原子（status 观测面直读——不占驱动线程）。
    itc_stats: Arc<super::intercept::Stats>,
    pub local_port: u16,
    stop_flags: Vec<Arc<AtomicBool>>,
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
        cfg: ServeConfig,
        logf: Logf,
        dlogf: Logf,
        tokf: Logf,
        log_paths: Option<(String, String)>,
    ) -> std::io::Result<Arc<Self>> {
        let serve_dir = cfg.state_dir.join("serve");
        let cache_dir = cfg.state_dir.join("cache");
        std::fs::create_dir_all(&serve_dir)?;
        std::fs::create_dir_all(&cache_dir)?;

        // ---- state：key/tokens/revoked（E2/E18）----
        let st = Arc::new(State::open(&serve_dir)?);
        let mut secrets = st.secrets().map_err(io_other)?;
        if secrets.is_empty() {
            // 零参首启或凭证全被吊销：先签发一条并落台账（否则打印的 token 生来无效）
            st.issue_token(vec![]).map_err(io_other)?;
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
                let r = egress::select_best(&cands, &[], Duration::from_secs(2), &move |s: &str| {
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
                        match egress::select_best(&cands, &[], Duration::from_secs(2), &move |s: &str| {
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
            let (p, rx) = DnsProxy::spawn(DnsConfig::default(), Arc::clone(&logf), Arc::clone(&dlogf));
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

        // ---- 拦截层（E5）+ LocalServices 映射 ----
        let mut local_services = std::collections::HashMap::new();
        local_services.insert(cfg.files_port, serve_dir.join("files.sock").display().to_string());
        local_services.insert(cfg.term_port, serve_dir.join("term.sock").display().to_string());
        local_services.insert(cfg.speedtest_port, serve_dir.join("speedtest.sock").display().to_string());
        let itc_stats = Arc::new(ItcStats::default());
        let dns_proxy = dns.clone();
        let dns_events = dns_events_rx;
        let mut intercept = Interceptor::attach(
            ItcConfig {
                tunnel_ip: cfg.tunnel_ip,
                local_services,
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
        let device = Device::new(priv_key.clone(), Arc::clone(&dlogf));

        // ---- ServerBind（双栈监听 + 钉卡；实际端口落盘）----
        // 绑地址形态（IP 字面量）单栈；钉卡形态不绑地址（双栈 [::]——v4/v6 客户端
        // 都能连，v6 STUN 观测同 socket 成立）。
        let mut bind = ServerBind::open_bound(cfg.listen_port, &cfg.build, bind_addr, resolved.as_ref().map(|i| (i.index, i.name.clone())), Arc::clone(&logf))?;
        let local_port = bind.local_port();
        std::fs::write(cache_dir.join("listen_port.txt"), format!("{local_port}\n"))?;
        // 公布口径的 pinned 判据 = **运行期事实**（socket 钉卡成功与否 + 绑地址），
        // 看护循环重钉后更新（Go `pinnedNow`/`PinnedIface` 同义）。
        let pinned_flag = Arc::new(AtomicBool::new(bind.pinned.is_some() || bind_addr.is_some()));

        // ---- files / speedtest / term UDS 服务（E14/E15/E16/E17）----
        let mut stop_flags = Vec::new();
        let mut socks = Vec::new();
        let mut term_srv: Option<Arc<crate::term::service::TermService>> = None;
        let files_srv = crate::files_server::FilesServer::open(cfg.files_root.as_deref(), Arc::clone(&dlogf)).ok();
        if let Some(fsrv) = files_srv {
            match crate::files_server::listen_local_service(&serve_dir, "files.sock", &dlogf) {
                Ok(ln) => {
                    let own = sock_identity(&serve_dir.join("files.sock"));
                    socks.push((serve_dir.join("files.sock"), own));
                    (logf)(&format!(
                        "files 就绪：root={} (rw) sock={}（隧道IP:{} 经拦截层转投）",
                        fsrv.root_dir().display(),
                        serve_dir.join("files.sock").display(),
                        cfg.files_port
                    ));
                    let stop = spawn_service_stop_flag(&mut stop_flags);
                    std::thread::Builder::new()
                        .name("homeway-files".into())
                        .spawn(move || {
                            let _ = fsrv.serve_stoppable(ln, stop);
                        })
                        .ok();
                }
                Err(e) => {
                    (logf)(&format!(
                        "⚠️ files 监听 {} 失败（{e}）—— 文件管理会报错（state 目录异常/被其它实例占用），其余功能不受影响",
                        serve_dir.join("files.sock").display()
                    ));
                }
            }
        } else {
            (logf)("⚠️ files 根目录不可用—— 文件管理会报错，其余功能不受影响");
        }
        let speed_srv = Arc::new(crate::speedtest_server::SpeedtestServer::new(Arc::clone(&dlogf)));
        match crate::files_server::listen_local_service(&serve_dir, "speedtest.sock", &dlogf) {
            Ok(ln) => {
                let own = sock_identity(&serve_dir.join("speedtest.sock"));
                socks.push((serve_dir.join("speedtest.sock"), own));
                (logf)(&format!(
                    "speedtest 就绪：sock={}（隧道IP:{} 经拦截层转投；内存收发不落盘）",
                    serve_dir.join("speedtest.sock").display(),
                    cfg.speedtest_port
                ));
                let stop = spawn_service_stop_flag(&mut stop_flags);
                let srv2 = Arc::clone(&speed_srv);
                std::thread::Builder::new()
                    .name("homeway-speedtest".into())
                    .spawn(move || {
                        let _ = srv2.serve_stoppable(ln, stop);
                    })
                    .ok();
            }
            Err(e) => {
                (logf)(&format!(
                    "⚠️ speedtest 监听 {} 失败（{e}）—— 测速功能会报错（state 目录异常/被其它实例占用），其余功能不受影响",
                    serve_dir.join("speedtest.sock").display()
                ));
            }
        }

        // ---- 终端会话 / agent gateway（exit-service-uds：UDS 承载 <serve>/term.sock）。
        //      客户端拨隧道 IP:<term_port>，拦截层按 LocalServices 映射转投；会话由
        //      出口持有（客户端断开只摘泵，不杀进程）；开关 HOMEWAY_TERM=off，调参
        //      HOMEWAY_TERM_*（6f-3b）。----
        if crate::term::service::disabled_by_env() {
            (logf)("term 服务被 HOMEWAY_TERM=off 关闭");
        } else {
            // stateDir 参数 = manifest 覆盖目录基（<dir>/agent-detection/）
            let tsrv = crate::term::service::TermService::new(Arc::clone(&dlogf), Some(&serve_dir));
            match crate::files_server::listen_local_service(&serve_dir, "term.sock", &dlogf) {
                Ok(ln) => {
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
                    let stop = spawn_service_stop_flag(&mut stop_flags);
                    let t = Arc::clone(&tsrv);
                    std::thread::Builder::new()
                        .name("homeway-term".into())
                        .spawn(move || t.serve_stoppable(ln, stop))
                        .ok();
                    term_srv = Some(tsrv);
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
            let relay_sock = bind
                .try_clone_socket()
                .expect("clone WG socket（注册腿与数据面同端口的硬约束）");
            // 驱动线程 → relay-leg 线程事件通道（type=3 控制帧 + hint；try_send 不阻塞驱动线程）
            let (leg_tx, leg_rx) = mpsc::sync_channel::<super::relayleg::LegEvent>(64);
            let leg_tx_frame = leg_tx.clone();
            bind.set_on_leg_frame(Box::new(move |payload, src| {
                let _ = leg_tx_frame.try_send(super::relayleg::LegEvent::Frame(payload.to_vec(), src));
            }));
            let leg_tx_hint = leg_tx.clone();
            bind.set_on_hint(Box::new(move |addr, src| {
                let _ = leg_tx_hint.try_send(super::relayleg::LegEvent::Hint(addr.to_string(), src));
            }));
            let stop = spawn_service_stop_flag(&mut stop_flags);
            super::relayleg::spawn_relay_leg(
                relay_sock,
                arg.addr,
                priv_key.clone(),
                arg.secret,
                leg_rx,
                Arc::clone(&stop),
                Arc::clone(&logf),
            );
            // 控制面（TCP，同号端口）：SESSION/RELEASE 经 EngineCmd 交驱动线程拨/拆腿
            super::relayleg::spawn_relay_ctl(
                arg.addr,
                priv_key.clone(),
                arg.secret,
                cmd_tx.clone(),
                Arc::clone(&stop),
                Arc::clone(&logf),
            );
        }

        // ---- P1 发送线程（浅拆：密文→sendto 独立；HOMEWAY_TX_SENDTHREAD 消融臂） ----
        // 单轮排空字节上界 = burst（团块钳制与整形器单拍上界同语义）；整形 off 臂
        // 用默认 TX_SHAPE_BURST（形态约束与整形开关正交）。
        if crate::server::bind::tx_sendthread_enabled() {
            match bind.try_clone_socket() {
                Ok(sock_dup) => {
                    let burst = crate::server::intercept::tx_shape_resolve(cfg.tx_shape_cfg)
                        .map(|s| s.burst)
                        .unwrap_or(crate::server::intercept::TX_SHAPE_BURST);
                    bind.tx_start(sock_dup, burst, Arc::clone(&dlogf));
                }
                Err(e) => {
                    (dlogf)(&format!("⚠️ 发送线程：dup socket 失败（{e}）—— 保持内联发送（拆分前形态）"));
                }
            }
        } else {
            (dlogf)("serve: 发送线程消融臂 HOMEWAY_TX_SENDTHREAD=off——内联发送（拆分前形态）");
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
                let mut device = device;
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
                        cfg, bind, &mut device, &mut table, &mut intercept, dns.as_ref(), &st,
                        &revoked_set, cmd_rx, &dlogf, &itc_stats, pub_kick_tx,
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
            ddns_checks: Arc::clone(&ddns_checks),
        });
        let pub_enabled = cfg.upnp || !cfg.stun.is_empty() || !cfg.public_endpoint.is_empty();
        if pub_enabled {
            let cmd_tx2 = cmd_tx.clone();
            let shared = Arc::clone(&shared);
            let kick_rx = pub_kick_rx;
            std::thread::Builder::new()
                .name("homeway-pubep".into())
                .spawn(move || {
                    public_endpoint_loop(shared, cmd_tx2, kick_rx, first_probe_tx);
                })
                .ok();
        } else {
            // 探测全关：观测线程不起、first_probe_tx 在此 drop（兜底线 pub_enabled
            // 分支不会执行 recv——它走自己的 15s 档；tx drop 只是让通道不悬空）。
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
            std::thread::Builder::new()
                .name("homeway-tokfb".into())
                .spawn(move || {
                    if pub_enabled {
                        // 等首轮探测信号；观测线程早夭（发送端全部 drop）= 兜底档
                        if first_probe_rx.recv().is_err() {
                            std::thread::sleep(fallback_wait);
                        }
                    } else {
                        // 探测全关（Go firstProbe==nil）：tokenFallbackWait 兜底档
                        std::thread::sleep(fallback_wait);
                    }
                    for _ in 0..10 {
                        if !driver_alive_tokfb.load(Ordering::SeqCst) {
                            return; // 低-6：驱动已收工，不再重试铸打
                        }
                        let (printed, published) = match shared.inner.lock() {
                            Ok(i) => (!i.last_token.is_empty(), i.last_published.clone()),
                            Err(_) => return,
                        };
                        if printed {
                            return;
                        }
                        print_client_token(&shared, &published);
                        let printed = shared
                            .inner
                            .lock()
                            .map(|i| !i.last_token.is_empty())
                            .unwrap_or(false);
                        if printed {
                            return;
                        }
                        std::thread::sleep(Duration::from_secs(1));
                    }
                })
                .ok();
        }

        // ---- udpcap 探测线程（kick = 换网卡后立即重探——绑卡看护 onChange） ----
        let (udpcap_kick_tx, udpcap_kick_rx) = mpsc::sync_channel::<()>(1);
        {
            let cmd_tx2 = cmd_tx.clone();
            let dlogf = Arc::clone(&dlogf);
            let itc_stats = Arc::clone(&itc_stats);
            std::thread::Builder::new()
                .name("homeway-udpcap".into())
                .spawn(move || udpcap_loop(cmd_tx2, itc_stats, &dlogf, udpcap_kick_rx))
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
                probe_targets: super::bindwatch::probe_targets_from_env(),
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
            "serve 就绪：wg=:{}（配置端口；被占用会自动退让）tunnel={} files={} term={} speedtest={} dns={} tokens={} key={}…",
            cfg.listen_port,
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
            driver_alive: driver_alive_ctor,
            itc_stats: Arc::clone(&itc_stats),
            local_port,
            stop_flags,
            term_srv,
            state_dir: cfg.state_dir.clone(),
            serve_dir,
            socks,
            ddns: cfg.ddns.clone(),
            ddns_checks,
        }))
    }

    /// 有序收工（D5 序）。grace = 过境 TCP 存量连接的有界宽限。
    pub fn shutdown(&self, grace: Duration) {
        for f in &self.stop_flags {
            f.store(true, Ordering::SeqCst); // ② UDS listeners（accept 循环下一拍退出）
        }
        // term：全部会话 ENDED(service_stopped) + 子进程收尸（UDS listener 由 stop_flags 收）
        if let Some(t) = &self.term_srv {
            t.close();
        }
        let _ = self.cmd_tx.send(EngineCmd::Stop { grace }); // ①③④ 驱动线程内按序执行
        // ⑤ sock 文件清理（身份比对——只删自己 bind 出来的那个）
        let handle = self.driver.lock().expect("驱动句柄锁中毒").take();
        if let Some(h) = handle {
            let _ = h.join();
        }
        for (path, own) in &self.socks {
            remove_sock_own(path, *own);
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
pub fn shrink_upnp_lease(engine: &ServeEngine, logf: &Logf) {
    let port = engine.local_port;
    let cands = crate::server::upnp::local_ipv4_candidates();
    for cand in cands {
        // 缩租路径独立 8s 预算（Go serve.go:728-739 ctx——收工不被慢网关拖死）
        let Ok(g) = crate::server::upnp::discover_igd(cand, crate::server::upnp::UPNP_SHRINK_BUDGET) else { continue };
        if let Some((ext, internal)) = g.find_our_mapping(crate::server::upnp::UPNP_MAP_DESC, cand, port) {
            if g.re_add_short_lease(ext, cand, internal, 300).is_ok() {
                (logf)(&format!("UPnP：退出前把映射 外部 {ext} 的租期缩到 5 分钟（快速重启仍会沿用这个端口）"));
            }
        }
        return;
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

fn sock_identity(p: &std::path::Path) -> (u64, u64) {
    std::fs::metadata(p).map(|m| (m.dev(), m.ino())).unwrap_or((0, 0))
}

/// 只删「还是自己 bind 出来的那个文件」（removeSockOwn；路径被接管时不动它）。
fn remove_sock_own(path: &std::path::Path, own: (u64, u64)) {
    if own == (0, 0) {
        let _ = std::fs::remove_file(path);
        return;
    }
    if let Ok(md) = std::fs::metadata(path) {
        if (md.dev(), md.ino()) == own {
            let _ = std::fs::remove_file(path);
        }
    }
}

fn spawn_service_stop_flag(flags: &mut Vec<Arc<AtomicBool>>) -> Arc<AtomicBool> {
    let f = Arc::new(AtomicBool::new(false));
    flags.push(Arc::clone(&f));
    f
}

// ---------- 驱动循环（WG 驱动线程独占：device/拦截栈/设备表/ServerBind） ----------

/// pselect 亚毫秒等待（D-3 8r）：pacing 时刻表的等待原语——poll(2) 超时是整毫秒，
/// 1ms 拍内无法逐包分时；pselect(2) 的 timespec 到纳秒（macOS/Linux/OHOS-musl 三面
/// 可用）。fd 集与 poll 路径同构（主 UDP fd + 腿 fd），返回可读的腿 fd（主 fd 的
/// 收包走 recv_packet 非阻塞排空，无需读就绪位）。EINTR 无害（按到点返回）；
/// 其它负返回**节流记行**（评审 r1-1.4：EBADF/EINVAL 会静默满速自旋，必须可见）
/// ——首 3 次 + 此后每 1000 次一行。select 语义上 POLLERR/HUP 也落在读就绪位——
/// 与 poll 路径的三事件过滤等价覆盖。调用方已保证 max(fd) < FD_SETSIZE。
/// pselect 持续错误时的退化位（连续非 EINTR 错误 ⇒ 驱动循环退回 poll 路径）。
static PSELECT_BROKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn pselect_readable(udp_fd: i32, leg_fds: &[i32], wait: Duration, dlogf: &Logf) -> Vec<i32> {
    unsafe {
        let mut set: libc::fd_set = std::mem::zeroed();
        libc::FD_ZERO(&mut set);
        libc::FD_SET(udp_fd, &mut set);
        let mut nfds = udp_fd + 1;
        for fd in leg_fds {
            libc::FD_SET(*fd, &mut set);
            nfds = nfds.max(*fd + 1);
        }
        // tv_nsec < 1e9（POSIX；评审 r2-2.2：i64::MAX 钳只在「调用方保证 <1ms」时
        // 安全——把不变量搬进函数本体。本函数本就只服务亚毫秒拍）。
        let ts = libc::timespec {
            tv_sec: 0,
            tv_nsec: wait.as_nanos().clamp(1, 999_999_999) as libc::c_long,
        };
        let rc = libc::pselect(
            nfds,
            &mut set,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &ts,
            std::ptr::null(),
        );
        if rc < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() != std::io::ErrorKind::Interrupted {
                // 连续 10 次非 EINTR 错误 ⇒ 置退化位让驱动循环退回 poll 路径（评审
                // r2-2.3：错误形态下 pselect 不等待——满速自旋，需要与 FD_SETSIZE
                // 对称的降级闸，不能只靠日志）。
                static ERR_COUNT: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let n = ERR_COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                if n <= 3 || n.is_multiple_of(1000) {
                    (dlogf)(&format!(
                        "serve: pselect 错误（{e}，累计 {n} 次）—— 立即返回不等待"
                    ));
                }
                if n >= 10 {
                    PSELECT_BROKEN.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        }
        leg_fds
            .iter()
            .copied()
            .filter(|fd| libc::FD_ISSET(*fd, &set))
            .collect()
    }
}

#[allow(clippy::too_many_arguments)] // 装配线程一次性移交全驱动状态（线程起点单参化无收益）
fn driver_loop(
    cfg: ServeConfig,
    mut bind: ServerBind,
    device: &mut Device,
    table: &mut DeviceTable,
    intercept: &mut Interceptor,
    dns: Option<&Arc<DnsProxy>>,
    st: &State,
    revoked_set: &Arc<Mutex<HashSet<[u8; 32]>>>,
    cmd_rx: mpsc::Receiver<EngineCmd>,
    dlogf: &Logf,
    _itc_stats: &Arc<ItcStats>,
    pub_kick_tx: std::sync::mpsc::SyncSender<()>,
) {
    let udp_fd = bind.udp_fd();
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
    let mut last_tx_stats = Instant::now();
    let mut last_tx_stats_snap = crate::server::bind::TxStatsSnap {
        bytes: 0,
        calls: 0,
        pkgs: 0,
        drops: 0,
        ring_drops: 0,
        wakeups: 0,
        drain_ns: 0,
        batch_max: 0,
        depth_peak: 0,
        hist: [0; 13],
    };
    let mut last_tx_wall_ns = 0u64;
    let mut out = InboundOut::default();
    let mut stop = false;
    let mut stop_grace = STOP_GRACE;
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
                    if bind.stun_query(server, reply).is_err() {
                        // 已有查询在等：直接回 None（观测周期分钟级，冲突即失败）
                    }
                }
                EngineCmd::StunAbort => bind.stun_query_abort(),
                EngineCmd::SetCaps(c) => bind.set_caps(c),
                EngineCmd::SetProbeEndpoints(eps) => bind.set_probe_endpoints(eps),
                EngineCmd::KickPublicEndpoint => {
                    let _ = pub_kick_tx.try_send(()); // 转发到观测线程（满=已有待处理 kick）
                }
                EngineCmd::Repin { index, name, reply } => {
                    // 看护循环重钉（socket 由驱动线程独占——两族都设、单栈容错）
                    let _ = reply.send(bind.repin_to(index, &name));
                }
                EngineCmd::LegRegister { id, remote, marker } => {
                    // SESSION 通告 → 拨腿（连接 socket + LEGUP 标记 + 入表）
                    if let Err(e) = bind.register_leg(id, remote, &marker) {
                        (dlogf)(&format!("中继控制面：会话 #{id} 拨腿失败（→ {remote}）：{e}"));
                    }
                }
                EngineCmd::LegRemove { id } => bind.remove_leg(id),
                EngineCmd::LegsClear => bind.clear_legs(),
            }
        }
        // ② UDP 收包（等待原语两档，R8-3 8i 拍频自适应 + D-3 8r 逐包时刻表）：
        //    - 整形滞留非空：按 tx_shape_wait 等「下一包可放行」——pacing on 时该值
        //      是亚毫秒（µs 级 pselect，poll(2) 的超时是整毫秒、1ms 拍内无法逐包
        //      分时）；pacing off 时 = 1ms（8n③ 形态原样）。
        //    - 滞留空：5ms 常规拍（不加空转唤醒成本）。
        //    腿 fd 同轮在等待集（R4：控制面通告建立的腿与主 socket 同构收包）。
        // 负 fd 防御（r2-2.4：理论上不可达——腿 fd 只取活 socket——防御位）
        let leg_fds: Vec<i32> = bind
            .leg_fds()
            .into_iter()
            .filter(|fd| *fd >= 0)
            .collect();
        let mut pollfds = Vec::with_capacity(1 + leg_fds.len());
        pollfds.push(libc::pollfd { fd: udp_fd, events: libc::POLLIN, revents: 0 });
        for fd in &leg_fds {
            pollfds.push(libc::pollfd { fd: *fd, events: libc::POLLIN, revents: 0 });
        }
        let shape_wait = intercept.tx_shape_wait();
        // 亚毫秒面（8r）：pselect 微秒级等待 + **50µs 量化下限**（评审 r1-1.1：pace
        // 间隔低于循环固定成本时驱动线程满占空——量化由释放侧补账语义吸收，唤醒率
        // 上界 20k/s）。FD_SETSIZE 防御**按 fd 编号判**（评审 r1-1.3 高危：fd_set 的
        // 容量约束是 fd 值不是 fd 数——出口的 worker/上游 socket 可把腿 fd 推过 1024，
        // Darwin 的 FD_SET 无边界检查 = 越界写 UB）；越界一次性记行后退回 poll——
        // pacing 退化为拍粒度，不静默降级（排障时可判）。
        let max_fd = leg_fds.iter().copied().chain([udp_fd]).max().unwrap_or(udp_fd);
        const MIN_PACE_WAIT: Duration = Duration::from_micros(50);
        static FD_SETSIZE_LOGGED: std::sync::atomic::AtomicBool =
            std::sync::atomic::AtomicBool::new(false);
        let fdset_ok = (max_fd as usize) < libc::FD_SETSIZE;
        if !fdset_ok
            && !FD_SETSIZE_LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            (dlogf)(&format!(
                "serve: 腿 fd 编号 ≥ FD_SETSIZE（max={max_fd}）——pacing 亚毫秒拍退回 poll 1ms（拍粒度整形）"
            ));
        }
        let sub_ms = shape_wait
            .filter(|_| {
                fdset_ok && !PSELECT_BROKEN.load(std::sync::atomic::Ordering::Relaxed)
            })
            .map(|d| d.max(MIN_PACE_WAIT))
            .filter(|d| *d < Duration::from_millis(1));
        let readable_legs: Vec<i32> = if let Some(d) = sub_ms {
            pselect_readable(udp_fd, &leg_fds, d, dlogf)
        } else {
            let poll_ms: libc::c_int = match shape_wait {
                Some(d) => (d.as_millis() as i64).clamp(1, 5) as libc::c_int,
                None => 5,
            };
            let n = unsafe {
                libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, poll_ms)
            };
            if n < 0 {
                let e = std::io::Error::last_os_error();
                if e.kind() != std::io::ErrorKind::Interrupted {
                    (dlogf)(&format!("serve: poll 错误（{e}）—— 继续循环"));
                }
            }
            pollfds[1..]
                .iter()
                .filter(|pf| pf.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0)
                .map(|pf| pf.fd)
                .collect()
        };
        // 腿 fd 可读（与主 socket 同一消费管线——data 进 device / reg 进设备表）
        for fd in readable_legs {
            let (_alive, inbound) = bind.leg_readable(fd);
            if let Some(inbound) = inbound {
                handle_inbound(inbound, device, table, intercept, &mut bind, &mut out, &cfg);
            }
        }
        let mut got_packet = false;
        loop {
            match bind.recv_packet() {
                Ok(Some(inbound)) => {
                    got_packet = true;
                    handle_inbound(inbound, device, table, intercept, &mut bind, &mut out, &cfg);
                }
                Ok(None) => continue,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break
                }
                Err(e) => {
                    (dlogf)(&format!("serve: 读错误（{e}）—— 继续循环"));
                    break;
                }
            }
        }
        let _ = got_packet;
        // ③ 拦截拍（worker 事件 + DNS 应答回投 + 栈 poll + TX）。P1 两级前置背压：
        // 发送 ring 高水位 ⇒ 本拍整形释放退化「只并入不释放」（包留整形 FIFO =
        // 真背压不丢包，满丢成为最后兜底）。
        let hold = bind.tx_high_water();
        let tx = if hold {
            intercept.pump_hold()
        } else {
            intercept.pump()
        };
        route_encap(&tx, device, &mut bind, &mut out);
        // ④ WG 定时器（握手重传/keepalive/服务端主动握手的产出面）
        device.tick_timers(&mut out);
        bind.send_wire_ctl(&out); // 控制面直发（keepalive/重传——同上）
        out.wire.clear();
        // ⑤ 周期任务
        let now = Instant::now();
        if now >= next_gc {
            let sys_now = SystemTime::now();
            for (_a, ops) in table.gc(sys_now) {
                apply_dev_ops(ops, device);
            }
            let jitter = (std::process::id() as u64 * 7919) % 60;
            next_gc = now + cfg.peer_ttl + Duration::from_secs(jitter);
        }
        if now.duration_since(last_leg_sweep) > crate::server::bind::RELAY_LEG_SWEEP_PUB {
            last_leg_sweep = now;
            bind.sweep_legs();
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
        // R8-2 归因 + P1 拆分观测：批量出站形态 5s 行（dlogf 面；本窗调用有增长才打
        // ——空闲静默）。判别面 = 均批包数（**「轮」口径**：Queued = 发送线程一次排空
        // 轮 ≤ burst 字节；Inline = 一次 send_wire〔拆分前语义〕——跨模式不可直比）
        // + 丢弃计数（send_batch 短返/失败 + ring 满丢）+ 发送面耗时剂量
        //（P1b-0：Inline = sendto 占驱动线程；Queued = 判定+入队侧，排空耗时另列）。
        if now.duration_since(last_tx_stats) > Duration::from_secs(5) {
            last_tx_stats = now;
            let s = bind.tx_stats_snapshot();
            if s.calls > last_tx_stats_snap.calls {
                let dcalls = s.calls - last_tx_stats_snap.calls;
                let dpkgs = s.pkgs - last_tx_stats_snap.pkgs;
                let dbytes = s.bytes - last_tx_stats_snap.bytes;
                let avg = dpkgs as f64 / dcalls as f64;
                let bpp = (dbytes as f64 / dpkgs.max(1) as f64) as u64;
                // P1b-0 剂量插桩：发送面耗时（send_wire wall-time 差分，ms/5s）——
                // 驱动线程被发送面占住时长的直接观测（PERF-AB §9.3 的 78% 是线程内
                // 占比，这里是绝对剂量）。
                let dwell_ms = (bind.tx_wall_ns - last_tx_wall_ns) as f64 / 1e6;
                let drain_ms = (s.drain_ns - last_tx_stats_snap.drain_ns) as f64 / 1e6;
                let mode = if bind.tx_queued_mode() { "发送线程" } else { "内联" };
                let hist: Vec<String> = s
                    .hist
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| **c > 0)
                    .map(|(i, c)| {
                        let lo = 1usize << i;
                        let hi = 1usize << (i + 1);
                        if i + 1 == s.hist.len() {
                            format!("≥{lo}:{c}")
                        } else {
                            format!("{lo}-{}:{c}", hi - 1)
                        }
                    })
                    .collect();
                let drop_seg = if s.drops > last_tx_stats_snap.drops {
                    format!(
                        " 累计丢弃{}包（+{}）",
                        s.drops,
                        s.drops - last_tx_stats_snap.drops
                    )
                } else {
                    String::new()
                };
                let ring_drop_seg = if s.ring_drops > last_tx_stats_snap.ring_drops {
                    format!(" 满丢{}（+{}）", s.ring_drops, s.ring_drops - last_tx_stats_snap.ring_drops)
                } else {
                    String::new()
                };
                let drain_seg = if bind.tx_queued_mode() {
                    format!(" 排空耗时{drain_ms:.0}ms/5s",)
                } else {
                    String::new()
                };
                (dlogf)(&format!(
                    "serve: UDP 出站[{mode}] 轮+{dcalls} 均轮{avg:.1}包 单轮最大{}包 均包{bpp}B 发送面耗时{dwell_ms:.0}ms/5s{drain_seg}{drop_seg}{ring_drop_seg} 批分布[{}]",
                    s.batch_max,
                    hist.join(" ")
                ));
                // 发送线程行（Queued 模式才有唤醒/队深面；评审 p1a-D-2：由驱动线程打
                //——窗口稳定，发送线程空窗长眠不打行）
                if bind.tx_queued_mode() {
                    let dwake = s.wakeups - last_tx_stats_snap.wakeups;
                    let per_wake = if dwake > 0 { dpkgs as f64 / dwake as f64 } else { 0.0 };
                    (dlogf)(&format!(
                        "serve: 发送线程 唤醒+{dwake} 排空+{dpkgs}包(均{per_wake:.1}包/唤醒) 队深峰{}包 累计满丢{}包",
                        s.depth_peak, s.ring_drops
                    ));
                }
            }
            last_tx_stats_snap = TxStatsSnap {
                bytes: s.bytes,
                calls: s.calls,
                pkgs: s.pkgs,
                drops: s.drops,
                ring_drops: s.ring_drops,
                wakeups: s.wakeups,
                drain_ns: s.drain_ns,
                batch_max: 0,
                depth_peak: 0,
                hist: [0; 13],
            };
            last_tx_wall_ns = bind.tx_wall_ns;
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
        let mut out2 = InboundOut::default();
        route_encap(&tx, device, &mut bind, &mut out2);
        out2.wire.clear();
        // 评审 r2-1.1 兜底：滞留未清空不提前收摊（流表空但尾数据还在整形队列——
        // 主修在 pump_grace 的全量释放面，这里是宽限循环侧的第二道闸）。
        if (intercept.flow_count() == 0 && !intercept.tx_pacing_pending())
            || Instant::now() >= deadline
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    // 发送线程收工（P1：stop + 唤醒 + join 无超时〔必返论证见 tx_shutdown〕——
    // drain-then-exit 语义保证宽限期尾入队的密文排空后才退）。在 intercept.close
    // 之前（close 的 teardown 不再产出出站包——现状码序）。
    bind.tx_shutdown(dlogf);
    // 到期：teardown（在途 TCP 立即拆——close 内逐条 teardown + 判据行）
    intercept.close();
}

/// 一个入站包的消化：**reg 先于 data 应用**（容器帧顺序契约——H1）。
fn handle_inbound(
    inbound: Inbound,
    device: &mut Device,
    table: &mut DeviceTable,
    intercept: &mut Interceptor,
    bind: &mut ServerBind,
    out: &mut InboundOut,
    cfg: &ServeConfig,
) {
    let _ = cfg;
    for (reg, _src) in inbound.regs {
        let now = SystemTime::now();
        if let Ok((_action, ops)) = table.register(&reg, now) {
            apply_dev_ops(ops, device); // 拒绝归因行由表内打出
        }
    }
    if let Some((src, wg)) = inbound.data {
        device.decapsulate(src, &wg, out);
        bind.send_wire_ctl(out); // 控制面直发（握手应答——引擎级验证：队列路径的应答客户端 WG 层拒收，见 send_wire_ctl 注释）
        out.wire.clear();
        for p in out.plain.drain(..) {
            intercept.on_plain(p);
        }
    }
}

fn apply_dev_ops(ops: Vec<DevOp>, device: &mut Device) {
    for op in ops {
        match op {
            DevOp::Add { pubkey, psk, tunnel_ip, tun_ip } => {
                device.add_peer(PeerConfig { pubkey, psk, tunnel_ip, tun_ip });
            }
            DevOp::Remove { pubkey } => {
                device.remove_peer(&pubkey);
            }
        }
    }
}

/// 拦截层出站明文包 → 按目的地址查 peer → encap → 腿帧发出。
fn route_encap(tx: &[Vec<u8>], device: &mut Device, bind: &mut ServerBind, out: &mut InboundOut) {
    for pkt in tx {
        let dst: Option<IpAddr> = intercept::nat_view_dst(pkt).map(IpAddr::V4);
        match dst {
            Some(d) => device.encapsulate(&d, pkt, out),
            None => continue, // 畸形：静默丢（拦截层产出恒可解析——防御位）
        }
    }
    bind.send_wire(out);
    out.wire.clear();
}

// ---------- 公网端点观测 + token 打印 ----------

#[derive(Default)]
struct TokenPrintState {
    last_token: String,
    revoked_logged: bool,
    last_published: Vec<String>,
}



struct TokenCtx {
    cfg: ServeConfig,
    st: Arc<State>,
    secret: [u8; 32],
    backend_priv: x25519_dalek::StaticSecret,
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
    /// DDNS 自检的滚动状态（按域名一域一份；探测线程写 / status 快照读——P0-4）。
    ddns_checks: super::ddnscheck::DdnsChecks,
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
        let ok = refresh_public_endpoint(&ctx, &cmd_tx);
        // DDNS 自检与探测同拍（换网 kick 轮也会跑到——published 取最近一轮观测；
        // Go publicendpoint.go:83 同义）。published 为空时自检内部自跳。
        {
            let published = ctx
                .inner
                .lock()
                .map(|i| i.last_published.clone())
                .unwrap_or_default();
            super::ddnscheck::run_ddns_self_check(
                &ctx.ddns_checks,
                &ctx.cfg.ddns,
                &published,
                &*ctx.logf,
            );
        }
        if first {
            first = false;
            let _ = first_probe_tx.try_send(()); // cap=1 非阻塞（Go select-default 同义）
        }
        let wait = if ok { PUBLIC_REFRESH_OK } else { PUBLIC_REFRESH_FAIL };
        match kick_rx.recv_timeout(wait) {
            Ok(()) => {
                (ctx.logf)("公网端点：收到换网事件，立即重测");
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        }
    }
}

fn refresh_public_endpoint(ctx: &Arc<TokenCtx>, cmd_tx: &Sender<EngineCmd>) -> bool {
    let endpoint_file = ctx.cfg.state_dir.join("cache").join("public_endpoint.txt");
    let manual = ctx.cfg.public_endpoint.trim().to_owned();
    if !manual.is_empty() {
        // 配置覆盖：推断在「不能枚举/不能观测」的环境里无解（macOS SSDP 被本地网络
        // 隐私拒即一例）——照常写文件 + 打 token。
        let lines: Vec<String> = manual.split(',').map(|s| s.trim().to_owned()).collect();
        let _ = std::fs::write(&endpoint_file, format!("{}\n", lines.join("\n")));
        (ctx.logf)(&format!(
            "公网端点：已按 **--public-endpoint 配置**公布 {}（跳过 UPnP/STUN 推断；写进 public_endpoint.txt）",
            format_lines(&lines)
        ));
        set_probe_endpoints_cmd(cmd_tx, &lines);
        {
            let mut inner = ctx.inner.lock().expect("token 状态锁中毒");
            inner.last_published = lines.clone();
        }
        print_client_token(ctx, &lines);
        return true;
    }

    // ---- 推断路径：UPnP 映射 + 同 socket STUN 观测（两条证据合流才公布）----
    let mut ext_port: u16 = 0;
    let mut wan_ip: Option<Ipv4Addr> = None;
    if ctx.cfg.upnp {
        let cands = crate::server::upnp::local_ipv4_candidates();
        if cands.is_empty() {
            (ctx.logf)("UPnP：找不到内网 IPv4 候选，跳过端口映射");
        } else {
            let d2 = ctx.logf.clone();
            let logf2: Logf = Arc::clone(&d2);
            match crate::server::upnp::ensure_port_mapping(&cands, ctx.local_port, &*logf2, &*logf2) {
                Err(e) => {
                    (ctx.logf)(&format!(
                        "UPnP：未取得端口映射（{e}）；出口在 NAT 后时可在路由器上手动把 UDP {} 转发到本机（候选 {:?}）",
                        ctx.local_port, cands
                    ));
                }
                Ok((ext, used)) => {
                    ext_port = ext;
                    (ctx.logf)(&format!(
                        "UPnP：已建立端口映射 外部 UDP {ext} → {used}:{}（重启时从路由器表认领，不需本地文件）",
                        ctx.local_port
                    ));
                    if let Ok(g) = crate::server::upnp::discover_igd(used, crate::server::upnp::UPNP_TOTAL_BUDGET) {
                        if let Ok(ip) = g.external_ip() {
                            if egress::is_public_addr(IpAddr::V4(ip)) {
                                wan_ip = Some(ip);
                            }
                        }
                    }
                }
            }
        }
    }

    let mut observed: Option<SocketAddr> = None;
    if !ctx.cfg.stun.is_empty() {
        let server = resolve_stun(&ctx.cfg.stun);
        match server
            .and_then(|s| stun_query_sync(cmd_tx, s, Duration::from_secs(8)))
        {
            None => {
                (ctx.logf)(&format!("STUN：从监听 socket 问 {} 失败（解析失败或无应答）", ctx.cfg.stun));
            }
            Some(ap) => {
                observed = Some(ap);
                (ctx.logf)(&format!(
                    "STUN：监听 socket（本地 {}）在 {} 眼里是 {ap}",
                    ctx.local_port, ctx.cfg.stun
                ));
            }
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
    // 不出去的候选）；v6 条目端口 = v4 公布端点的端口。
    let mut lines = vec![pub_ap.to_string()];
    if ctx.cfg.stun6.is_empty() {
        (ctx.logf)("公网端点：未配置 --stun6（需要有 AAAA 的 STUN 服务器），跳过 IPv6 公布");
    } else {
        let v6 = match resolve_stun6(&ctx.cfg.stun6) {
            None => None,
            Some(server) => stun_query_sync(cmd_tx, server, Duration::from_secs(8)),
        };
        match v6 {
            Some(ap6) => {
                let ip = ap6.ip();
                // Go `ap6.Addr().Is6() && !Is4In6 && !IsLinkLocalUnicast`：非 v4-mapped
                // 且非链路本地的 v6 才算 v6 公布证据（ULA 不在此滤——与 Go 主路径
                // 同口径；probe 应答侧的 IsPublicAddr 过滤是另一层）
                let global_v6 = match ip {
                    IpAddr::V6(v6) => {
                        v6.to_ipv4_mapped().is_none() && (v6.segments()[0] & 0xffc0) != 0xfe80
                    }
                    IpAddr::V4(_) => false,
                };
                if global_v6 {
                    // Go 逐字同串：`公布 [%v]:%d`（v6 方括号形态）
                    lines.push(format!("[{ip}]:{}", pub_ap.port()));
                    (ctx.logf)(&format!(
                        "公网端点：IPv6 路径可用（STUN 看到 {ap6}），公布 [{ip}]:{}",
                        pub_ap.port()
                    ));
                }
                // 成功但非全局 v6（v4-mapped/链路本地）：静默不加条目（Go 同义）
            }
            None => {
                // Go 的 %v 带具体原因——这里按断点分两类（解析失败 / 无应答）
                let why = if resolve_stun6(&ctx.cfg.stun6).is_none() {
                    format!("{} 解析失败（无 AAAA？）", ctx.cfg.stun6)
                } else {
                    "无应答".to_owned()
                };
                (ctx.logf)(&format!("公网端点：IPv6 不可用（{why}），本轮只公布 IPv4"));
            }
        }
    }
    let _ = std::fs::write(&endpoint_file, format!("{}\n", lines.join("\n")));
    (ctx.logf)(&format!(
        "公网端点：已公布 {}（写进 public_endpoint.txt；下次签发 token 会带上它）",
        format_lines(&lines)
    ));
    set_probe_endpoints_cmd(cmd_tx, &lines);
    {
        let mut inner = ctx.inner.lock().expect("token 状态锁中毒");
        inner.last_published = lines.clone();
    }
    print_client_token(ctx, &lines);
    true
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

fn resolve_stun(server: &str) -> Option<SocketAddr> {
    use std::net::ToSocketAddrs as _;
    // 只取 v4（Go `network="ip4"` 同义——评审 M22：AAAA 优先的主机名会让 v4 socket
    // 恒发送失败，把可公布的判成不可用）
    server.to_socket_addrs().ok()?.find(|a| a.is_ipv4())
}

/// 解析 v6 STUN 目标（Go `LookupNetIP("ip6")` 同义）：取**非 v4-mapped 的 v6**
/// 地址（stun.cloudflare.com 同名双栈；观测 v6 路径必须真走 v6）。
fn resolve_stun6(server: &str) -> Option<SocketAddr> {
    use std::net::ToSocketAddrs as _;
    server.to_socket_addrs().ok()?.find(|a| match a {
        SocketAddr::V6(v6) => v6.ip().to_ipv4_mapped().is_none(),
        SocketAddr::V4(_) => false,
    })
}

fn stun_query_sync(cmd_tx: &Sender<EngineCmd>, server: SocketAddr, timeout: Duration) -> Option<SocketAddr> {
    let (tx, rx) = mpsc::channel();
    cmd_tx.send(EngineCmd::StunQuery { server, reply: tx }).ok()?;
    match rx.recv_timeout(timeout) {
        Ok(v) => v,
        Err(_) => {
            let _ = cmd_tx.send(EngineCmd::StunAbort);
            None
        }
    }
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
    // 低-7（B0-2a 登记→D-1 收口）：inner 锁只护 revoked_logged/last_token 的读写
    // 小临界区——网卡枚举/token 编码/台账追加在锁外（Go tokMu 同款小临界区语义；
    // 台账幂等由 append_token 的末行比对自带，并发轮至多同条目双追加——无害）。
    {
        let mut inner = ctx.inner.lock().expect("token 状态锁中毒");
        // 在用凭证已被吊销：只大声一次并停打
        if ctx.revoked.lock().map(|m| m.contains(&ctx.secret)).unwrap_or(false) {
            if !inner.revoked_logged {
                inner.revoked_logged = true;
                drop(inner);
                (ctx.logf)(
                    "⚠️ 在用凭证已被吊销——**不再打印 token**（旧 token 已作废）；执行 `homeway-cli serve restart`（重启）铸出新凭证，客户端需重新粘贴",
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
    let mut add = |e: Endpoint, kind: &str| {
        if e.addr.is_empty() || seen.contains(&e.addr) {
            // 中继地址与已有直连端点相同（--relay 指向出口自己地址的退化形态）：
            // 按先到的直连形态保留——那个地址物理上就是出口的 WG socket，标 relay 反而必坏
            if e.kind == EndpointKind::Relay && seen.contains(&e.addr) {
                (dlogf)(&format!("中继端点 {} 与已有直连端点相同，按直连处理（中继腿不生效）", e.addr));
            }
            return;
        }
        seen.insert(e.addr.clone());
        labels.push(format!("{}（{kind}）", e.addr));
        eps.push(e);
    };
    for ifi in egress::physical_candidates() {
        for a in ifi.addrs {
            add(Endpoint { addr: format!("{a}:{}", ctx.local_port), kind: EndpointKind::Direct }, "内网");
        }
    }
    for p in published {
        add(Endpoint { addr: p.clone(), kind: EndpointKind::Direct }, "公网");
    }
    // DDNS 域名条目（多条，叠加不踢除——B-1 拍板）：端口口径 = 已公布公网 v4 端点的
    // 外部端口；无公网观测时回退实际监听口（让位退让后的真实口）。
    if !ctx.cfg.ddns.is_empty() {
        let p = ddns_entry_port(published, ctx.local_port);
        if p != 0 {
            for domain in &ctx.cfg.ddns {
                add(
                    Endpoint { addr: format!("{domain}:{p}"), kind: EndpointKind::Direct },
                    "域名",
                );
            }
        } else {
            // listenPort=0 的调用形态（探测应答的即时快照）：域名条目这轮缺席，下一轮补上。
            (dlogf)("域名条目：本轮拿不到端口（socket 未开？），token/列表暂不带 ddns 条目");
        }
    }
    if let Some(r) = &ctx.relay_ep {
        add(r.clone(), "中继");
    }
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
    }) {
        Ok(s) => s,
        Err(e) => {
            (ctx.logf)(&format!("客户端 token 生成失败（{e}）"));
            return;
        }
    };
    let eps_count = eps.len();
    // 台账写入纪律：与末行不同即追加（无变化不追加）；吊销拒写（分支告警——Go
    // printClientToken 的 ErrSecretRevoked 专用行，P1-5 收口）
    if let Err(e) = ctx.st.append_token(&ctx.secret, &eps) {
        match e {
            crate::server::state::StateError::SecretRevoked => {
                (ctx.logf)(&format!(
                    "⚠️ 在用凭证已被吊销（{e}）——本轮 token 未入台账；执行 `homeway-cli serve restart`（重启）铸出新凭证"
                ));
            }
            other => {
                (ctx.logf)(&format!("⚠️ token 台账追加失败（{other}）——台账末行可能与在用 token 短暂不一致"));
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
            (ctx.logf)(&format!("日志：{ev_p}（摘要）｜ {dbg_p}（细节）"));
        }
        (ctx.logf)(&format!("客户端 token（粘进 App 的「添加主机」即可；{eps_count} 个端点）：{tok_str}"));
        (ctx.logf)(&format!("端点：{}", labels.join("、")));
    } else {
        // 端点变化轮 = Go tokenToFile 形态：只进摘要文件（--verbose 才回显终端）——
        // 终端冒出第二串 token 只会让人拿错（2026-09-21 用户口径；取最新 =
        // grep 客户端 token events.log | tail -1）。
        (ctx.tokf)(&format!("客户端 token（端点已变化；粘进 App 的「添加主机」即可；{eps_count} 个端点）：{tok_str}"));
        (ctx.tokf)(&format!("端点：{}", labels.join("、")));
    }
}

// ---------- udpcap（默认路径 UDP 能力探测；结论喂探测应答 caps） ----------

fn udpcap_loop(
    cmd_tx: Sender<EngineCmd>,
    stats: Arc<ItcStats>,
    logf: &Logf,
    kick_rx: mpsc::Receiver<()>,
) {
    let mut last = "?".to_owned();
    let mut prev_replied: u64 = 0;
    let mut prev_no_reply: u64 = 0;
    loop {
        // 先探一轮再进周期（Go startUDPCapProbe 同序）
        let (dns_note, generic_note, saw, hint, flags) = probe_once(&stats, &mut prev_replied, &mut prev_no_reply);
        let _ = cmd_tx.send(EngineCmd::SetCaps(flags));
        let cur = format!("0x{flags:02x}");
        if cur != last || saw != "本轮没有转发的 UDP 会话" {
            (logf)(&format!(
                "UDP 默认路径：DNS:53 {dns_note}；通用 UDP（STUN:3478）{generic_note}；实测 {saw}{hint}（探测应答 flags=0x{flags:02x} 也会这么报）"
            ));
            last = cur;
        }
        // 周期等待可被 kick 打断（换网卡 = 换了条路，能力结论立即重测）。
        // Disconnected（看护未起：--bind-interface none / IP 字面量 / auto 挑卡
        // 失败——sender 已随 start() 返回被 drop）必须**按原节拍继续**：探测与
        // SetCaps 是 P1-4 客户端能力打行的数据源，退出或全速自旋（评审 r1-H1：
        // 吞掉 Disconnected 会 ~7 轮/秒持续探测）都不成立。
        if kick_rx.recv_timeout(UDPCAP_INTERVAL).is_err() {
            std::thread::sleep(UDPCAP_INTERVAL);
        }
    }
}

#[allow(clippy::type_complexity)]
fn probe_once(
    stats: &Arc<ItcStats>,
    prev_replied: &mut u64,
    prev_no_reply: &mut u64,
) -> (String, String, String, String, u8) {
    let mut flags: u8 = 0;
    let dns_note = match egress::probe_default(&[], Duration::from_secs(2)) {
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
        match egress::probe_stun(None, &[], Duration::from_secs(3)) {
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
    // 真实流量证据（统计窗口 = 本次探测与上次探测之间）
    let snap = stats.snapshot();
    let (replied, no_reply) = (snap[4].1, snap[5].1);
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
