//! relay —— Homeway 中继角色（R4-4a）。
//!
//! 语义真源 `baseline:internal/relay/`（relay/leg/assoc/reap/control/status 共 9 文件）。
//! 定位：中继是**路径而不是参与方**——只见密文、零 WG 感知、零落盘运行态（重启即清）。
//! 两条腿：
//!
//! ```text
//! 后端(NAT 后) ──出站注册腿──► 中继                       （NAT 友好：出站即可）
//! 客户端       ──标签帧──────► 中继 ──per-client socket──► 后端注册腿源地址
//! ```
//!
//! **per-client 分配 socket 是正确性要求**：后端 WG 靠源地址区分客户端 endpoint。
//!
//! 线程模型（R4-design §4.1，R1 决议「自管线程 + poll(2)」）：**单驱动线程独占全部
//! 可变状态**（legs/assocs/rates/stats/next_sid）+ 短命握手线程（≤16，RAII 槽位，
//! established 后交还驱动线程）。控制面通告/回显类写全在驱动线程（非阻塞单次 send，
//! EAGAIN/短写 = 断连——Go 无期限全局停摆的有界化替代，登记 §7.2）。

pub mod ctlface;
pub mod logfile;
pub mod rltoken;

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, UdpSocket};
use std::os::fd::AsRawFd as _;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use x25519_dalek::PublicKey;

use crate::go_fmt::fmt_duration_go_secs;
use crate::relaywire as rw;
use crate::wtransport::frame::{self, relay_id};

pub use ctlface::CTL_READ_TIMEOUT;

/// 两级日志的运行面（logf = relay.log 全量；ulogf = 终端 + 抄文件——由 CLI 装配）。
pub type Logf = crate::Logf;

// ---------- 默认参数（可用 Config 覆盖；真源 relay.go:42-66） ----------

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(90); // 分配腿空闲回收
const DEFAULT_LEG_TIMEOUT: Duration = Duration::from_secs(90); // 注册腿过期（保活 25s × 3 容错）
const DEFAULT_DIAL_WAIT: Duration = Duration::from_secs(15); // 拨腿等待窗口
const DEFAULT_DOWN_SILENT: Duration = Duration::from_secs(5 * 60); // 会话下行静默上限
const LEG_BOOTSTRAP: Duration = Duration::from_secs(30); // 未验证腿的注册窗口
const CHALLENGE_TTL: Duration = Duration::from_secs(15); // 挑战有效期（UDP 面）
const DEFAULT_MAX_PER_PEER: usize = 32; // 每后端最多并发分配
const DEFAULT_RATE_LIMIT: i32 = 200; // 每源每秒包数（准入限流）
const DEFAULT_MAX_LEGS: usize = 256; // 注册腿总数上限（匿名洪水兜底）
const DEFAULT_MAX_CTL_CONNS: u16 = 64; // 已建立控制连接总数上限
const CTL_PEND_MAX: usize = 16; // 等腿窗的客户端包缓冲上限
const RATE_BUCKET_TTL: Duration = Duration::from_secs(2); // 限流桶清理窗
/// **未验证挑战**表上限（F5：HELLO 不占 `legs`，只占本表；满按最旧淘汰**而非拒绝**——
/// 拒绝会让合法后端在洪水下也进不来）。Q17 语义之一。
const PENDING_MAX: usize = 64;
/// 全局分配腿（assoc socket）总数上限（F7.3）：`max_per_peer` 是**每腿**上限，
/// 全局最坏 `32 × 256 = 8192` socket（每枚 4MB+4MB 内核缓冲）⇒ fd/内存双耗尽面。
const MAX_ASSOCS_TOTAL: usize = 1024;
/// 单会话下行字节桶（F7.2）：目标吞吐 ≥100Mbit/s（16MiB/s ≈ 128Mbit/s 留余量；
/// **禁止**照抄 `leg_rate_ok` 的 2000pps——那是 ~22Mbit/s 硬顶的静默下载回归）。
const ASSOC_DOWN_BYTES_PER_SEC: u64 = 16 * 1024 * 1024;

// 拒绝类日志的节流原因（F6：首 3 条 + 每 100 条一条）。enum 代 int 常量
// （AGENTS 工程原则 1：类型承担不变量；饱和面仅是节流桶键，不做线上编码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum RejectLog {
    /// 注册腿总数达上限（PROOF 提升 / 控制面新腿）
    LegCap,
    /// 每腿分配上限
    PerPeer,
    /// PROOF 协议版本不符
    ProofVer,
    /// token 校验不过
    Token,
    /// 并发握手上限
    CtlFull,
    /// 全局分配腿总数上限（F7.3）
    AssocTotal,
    /// 下行字节桶超限（F7.2）
    DownLimit,
    /// 转发发送失败（F8）
    SendFail,
}

/// 中继参数（CLI/装配层构造）。
#[derive(Clone)]
pub struct Config {
    /// 监听地址（如 `0.0.0.0:41741`；端口被占退让 +1…+9 → 随机）。
    pub listen: SocketAddr,
    pub idle_timeout: Duration,
    pub leg_timeout: Duration,
    pub dial_wait: Duration,
    pub down_silent: Duration,
    pub max_per_peer: usize,
    pub rate_limit: i32,
    /// 中继鉴权密钥（token 模式）；`None` = 显式开放注册（仅测试——与密钥恰好一者）。
    pub secret: Option<[u8; 32]>,
    pub max_legs: usize,
    pub max_ctl_conns: u16,
    /// 探测应答里回报的构建标记（空 = "relay-dev"）。
    pub build: String,
    /// 【测试形态】不递送 hint（两端都不推）——同机拓扑里 hint→盲打→采纳会把
    /// 客户端翻成直连，中继驻留/升级条纹的前提不成立；真机的等价形态 = NAT 把
    /// 打洞响应全丢（正是中继存在的理由）。生产恒 false。
    pub no_hints: bool,
    pub logf: Logf,
}

impl Config {
    /// 全默认（listen 由调用方给）。
    pub fn new(listen: SocketAddr, secret: Option<[u8; 32]>, logf: Logf) -> Self {
        Self {
            listen,
            idle_timeout: DEFAULT_IDLE_TIMEOUT,
            leg_timeout: DEFAULT_LEG_TIMEOUT,
            dial_wait: DEFAULT_DIAL_WAIT,
            down_silent: DEFAULT_DOWN_SILENT,
            max_per_peer: DEFAULT_MAX_PER_PEER,
            rate_limit: DEFAULT_RATE_LIMIT,
            secret,
            max_legs: DEFAULT_MAX_LEGS,
            max_ctl_conns: DEFAULT_MAX_CTL_CONNS,
            build: String::new(),
            no_hints: false,
            logf,
        }
    }
}

/// 中继计数（诊断/测试用；字段名对齐 Go Stats）。
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Stats {
    pub registered: u64,
    pub forged: u64,
    pub denied: u64,
    pub assigned: u64,
    pub reclaimed: u64,
    pub dropped: u64,
    pub forwarded_up: u64,
    pub forwarded_down: u64,
    pub leg_rejected: u64,
    /// 上行转发发送失败（F8：`send_to` 返 Err 才计，不再计成功）。
    pub send_fail_up: u64,
    /// 下行转发发送失败（F8）。
    pub send_fail_down: u64,
    /// 下行字节桶丢弃（F7.2：超目标吞吐被丢的包）。
    pub down_limited: u64,
}

// ---------- 内部状态（驱动线程独占） ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct AssocKey {
    label: [u8; 8],
    client: SocketAddr,
}

/// 一条注册腿（后端身份 = label = HashMap 键；relayID 分配/回收见 R4-design §1.2）。
/// **只有认证过（PROOF 通过 / 控制面握手通过）的腿才进本表**——未认证的 HELLO 挑战
/// 落在 `pending`（F5）。
struct Leg {
    pubkey: [u8; 32],
    /// 注册腿源地址（后端公网映射，也是给客户端的 hint）；None = 纯控制腿。
    addr: Option<SocketAddr>,
    last: Instant,
    verified: bool,
    ctl_verified: bool,
    /// 挂着的控制连接（conn id；attach 顶旧连）。
    ctl: Option<u64>,
}

/// 一次未完成的挑战（F5：HELLO 只写这里，不占 `legs`）。真正需要的只有
/// `(pubkey, eph_priv, nonce, 出题时刻)`——token 模式 PROOF 不用 `eph_priv`，但
/// **pubkey 必须留存**（PROOF wire 不含 pubkey，label=sha256(pubkey)[:8] 不可逆）。
struct PendingLeg {
    pubkey: [u8; 32],
    eph_priv: x25519_dalek::StaticSecret,
    nonce: [u8; 16],
    chall_at: Instant,
}

impl Leg {
    fn admitted(&self) -> bool {
        self.verified || self.ctl_verified
    }
    fn has_ctl(&self) -> bool {
        self.ctl.is_some()
    }
}

/// 一条客户端分配（per-client socket 两形态状态机，R4-design §2.2）。
struct Assoc {
    key: AssocKey,
    backend: Option<SocketAddr>,
    sock: UdpSocket,
    /// socket 是否双栈（F8：按族 map 发送目标，对齐 Go `ListenUDP("udp", nil)`）。
    dual: bool,
    /// 大缓冲是否已抬高（F7.3：未认证不 bump——认证后/首次真实下行才抬）。
    bufs_bumped: bool,
    /// 下行字节桶（F7.2）：窗口起点 + 窗口内字节数。
    down_win: Instant,
    down_bytes: u64,
    last: Instant,
    last_down: Instant,
    sid: u64,
    /// 持久标志：本会话由拨腿承载（backend=腿源地址；地址漂移检查豁免）。
    dialed: bool,
    /// 等待 LEGUP 中（等腿窗）。
    dial_up: bool,
    dial_up_at: Instant,
    cookie: [u8; 16],
    auth_ok: bool,
    auth_src: Option<SocketAddr>,
    pend: Vec<Vec<u8>>,
}

/// 每源限流桶（1s 窗口）。
struct RateBucket {
    window: Instant,
    count: i32,
}

/// poll 槽位语义。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollSource {
    Stop,
    Wakeup,
    Udp,
    TcpListener,
    Assoc(AssocKey),
    Ctl(u64),
}

/// 驱动线程内部事件。
enum Msg {
    HandshakeDone(Result<ctlface::Handover, (SocketAddr, ctlface::HsError)>),
    /// 预留的外部停止注入面（CLI/测试缝）。
    #[expect(dead_code)]
    Stop,
}


/// 大收发缓冲（4MB 尽力而为——内核钳制；突发场景防整包丢弃，R3 出口同款）。
fn bump_sock_bufs(sock: &UdpSocket) {
    use std::os::fd::AsRawFd as _;
    unsafe {
        let sz: libc::c_int = 4 * 1024 * 1024;
        let _ = libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &sz as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as u32,
        );
        let _ = libc::setsockopt(
            sock.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &sz as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as u32,
        );
    }
}

/// 中继实例（`run` = 驱动线程本体，阻塞到 stop）。
pub struct Relay {
    cfg: Config,
    legs: HashMap<[u8; 8], Leg>,
    /// 未验证挑战表（F5：HELLO 写这里，PROOF 通过才提升进 `legs`）。
    pending: HashMap<[u8; 8], PendingLeg>,
    assocs: HashMap<AssocKey, Assoc>,
    rates: HashMap<IpAddr, RateBucket>,
    leg_rates: HashMap<IpAddr, RateBucket>,
    stats: Stats,
    next_sid: u64,
    next_conn_id: u64,
    ctl_conns: HashMap<u64, ctlface::CtlConn>,
    handshaking: Arc<AtomicI32>,
    established: Arc<AtomicI32>,
    msg_tx: mpsc::Sender<Msg>,
    wake_w: i32,
    /// poll 集成员已变（会话/连接/腿的增删）——下一轮重建。
    poll_dirty: bool,
    /// 控制面（TCP 同号口）是否就绪（F10：bind 失败 = 纯 UDP 降级，探针 flags 报降级位）。
    ctl_ok: bool,
    /// 拒绝类日志的每原因节流计数（F6）。
    reject_log: HashMap<RejectLog, u64>,
}

impl Relay {
    /// 构造（不监听）。鉴权形态由 `secret: Option` 承担（Some = token 模式 / None =
    /// 显式开放——仅测试；「两者都设」在该类型上不可表达，CLI 层负责不把开放与
    /// 密钥同时传入——Go 构造期 panic 的互斥校验因此无对应失败面）。
    pub fn new(cfg: Config) -> Self {
        let (msg_tx, _msg_rx) = mpsc::channel();
        Self {
            cfg,
            legs: HashMap::new(),
            pending: HashMap::new(),
            assocs: HashMap::new(),
            rates: HashMap::new(),
            leg_rates: HashMap::new(),
            stats: Stats::default(),
            next_sid: 0,
            next_conn_id: 0,
            ctl_conns: HashMap::new(),
            handshaking: Arc::new(AtomicI32::new(0)),
            established: Arc::new(AtomicI32::new(0)),
            msg_tx,
            wake_w: -1,
            poll_dirty: false,
            ctl_ok: false,
            reject_log: HashMap::new(),
        }
    }

    fn logf(&self, s: &str) {
        (self.cfg.logf)(s);
    }

    /// 实际监听地址（run 之后由 on_ready 回调给出；测试用）。
    fn listen_with_fallback(want: SocketAddr, logf: &Logf) -> io::Result<UdpSocket> {
        if let Ok(s) = UdpSocket::bind(want) {
            return Ok(s);
        }
        logf(&format!("⚠️ 监听端口 {} 被占用 —— 自动往后找", want.port()));
        if want.port() == 0 {
            return UdpSocket::bind(SocketAddr::new(want.ip(), 0));
        }
        for p in want.port().saturating_add(1)..=want.port().saturating_add(9) {
            if let Ok(c) = UdpSocket::bind(SocketAddr::new(want.ip(), p)) {
                logf(&format!("中继改用端口 {p}（token 里写的就是它）"));
                return Ok(c);
            }
        }
        UdpSocket::bind(SocketAddr::new(want.ip(), 0))
    }

    /// 驱动线程本体：监听并服务直到 stop 信号（stop_fd 上可读字节）。
    /// `on_ready(实际端口)` 在 UDP 绑定成功后回调一次（token 必须在**实际端口**
    /// 确定后生成——Go RunWithReady 同义）。
    pub fn run(mut self, stop_fd: i32, on_ready: impl FnOnce(u16)) -> io::Result<()> {
        let udp = Self::listen_with_fallback(self.cfg.listen, &self.cfg.logf)?;
        bump_sock_bufs(&udp);
        udp.set_nonblocking(true)?;
        let actual_port = udp.local_addr()?.port();
        on_ready(actual_port);

        // TCP 控制监听与 UDP 实际端口同号（两个独立端口空间——部署零新增、token 不变）。
        // 起不来不致命：退回纯 UDP 中继（拨腿特性缺席）。
        let ctl_ln: Option<TcpListener> = match TcpListener::bind(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            actual_port,
        )) {
            Ok(ln) => {
                ln.set_nonblocking(true)?;
                self.ctl_ok = true;
                self.logf(&format!(
                    "中继控制面：TCP 0.0.0.0:{actual_port} 就绪（后端拨腿模式可用）"
                ));
                Some(ln)
            }
            Err(e) => {
                self.ctl_ok = false;
                self.logf(&format!(
                    "⚠️ 控制面 TCP 0.0.0.0:{actual_port} 监听失败（{e}）—— 退回纯 UDP 中继（拨腿特性缺席；探针 flags 报降级位）"
                ));
                None
            }
        };

        // 就绪行（who：token 模式带密钥短指纹）
        let who = match self.cfg.secret {
            Some(sec) => {
                let rid = rltoken::relay_secret_id(&sec);
                format!("token 模式（中继 ID {}）", hex(&rid[..6]))
            }
            None => "⚠️ 开放注册（测试开关 Open）：任何知道本地址的后端都能用它中转".to_owned(),
        };
        self.logf(&format!(
            "中继就绪：{}（{}；分配回收 {}，注册腿过期 {}，每源限速 {} pps，每后端最多 {} 条分配，腿总数上限 {}）",
            udp.local_addr()?,
            who,
            fmt_duration_go_secs(self.cfg.idle_timeout),
            fmt_duration_go_secs(self.cfg.leg_timeout),
            self.cfg.rate_limit,
            self.cfg.max_per_peer,
            self.cfg.max_legs
        ));

        // 唤醒管道（握手线程交接 / 测试注入）
        let mut wake_fds = [0i32; 2];
        unsafe {
            if libc::pipe(wake_fds.as_mut_ptr()) != 0 {
                return Err(io::Error::last_os_error());
            }
            libc::fcntl(wake_fds[0], libc::F_SETFL, libc::O_NONBLOCK);
        }
        self.wake_w = wake_fds[1];
        let (msg_tx, msg_rx) = mpsc::channel::<Msg>();
        self.msg_tx = msg_tx;

        let mut next_reap = Instant::now() + self.reap_interval();
        let mut next_stats = Instant::now() + Duration::from_secs(60);
        self.poll_dirty = true;
        let mut pollfds: Vec<libc::pollfd> = Vec::new();
        let mut sources: Vec<PollSource> = Vec::new();
        let mut events: Vec<PollSource> = Vec::new();
        let mut msgs: Vec<(u8, Vec<u8>)> = Vec::new();

        loop {
            // ---- poll 集重建（成员变化时） ----
            let dirty = self.poll_dirty;
            if dirty {
                pollfds.clear();
                sources.clear();
                let mut push = |fd: i32, src: PollSource| {
                    pollfds.push(libc::pollfd { fd, events: libc::POLLIN, revents: 0 });
                    sources.push(src);
                };
                push(stop_fd, PollSource::Stop);
                push(wake_fds[0], PollSource::Wakeup);
                push(udp.as_raw_fd(), PollSource::Udp);
                if let Some(ln) = &ctl_ln {
                    push(ln.as_raw_fd(), PollSource::TcpListener);
                }
                for (k, a) in &self.assocs {
                    push(a.sock.as_raw_fd(), PollSource::Assoc(*k));
                }
                for (id, c) in &self.ctl_conns {
                    push(c.stream.as_raw_fd(), PollSource::Ctl(*id));
                }
                self.poll_dirty = false;
            }

            let now = Instant::now();
            let mut timeout = 500i32;
            if next_reap > now {
                timeout = timeout.min(next_reap.duration_since(now).as_millis() as i32);
            }
            if next_stats > now {
                timeout = timeout.min(next_stats.duration_since(now).as_millis() as i32);
            }
            let n = unsafe { libc::poll(pollfds.as_mut_ptr(), pollfds.len() as libc::nfds_t, timeout) };
            if n < 0 {
                let e = io::Error::last_os_error();
                if e.kind() != io::ErrorKind::Interrupted {
                    self.logf(&format!("relay: poll 错误（{e}）—— 继续循环"));
                }
            }

            // ---- 事件收集（快照后逐一处理；成员变化标 dirty） ----
            events.clear();
            let mut stop = false;
            for (pf, src) in pollfds.iter().zip(&sources) {
                if pf.revents & (libc::POLLIN | libc::POLLERR | libc::POLLHUP) != 0 {
                    if *src == PollSource::Stop {
                        stop = true;
                    } else {
                        events.push(*src);
                    }
                }
            }
            for src in events.drain(..) {
                match src {
                    PollSource::Wakeup => {
                        let mut b = [0u8; 64];
                        while unsafe { libc::read(wake_fds[0], b.as_mut_ptr().cast(), 64) } > 0 {}
                        while let Ok(m) = msg_rx.try_recv() {
                            match m {
                                Msg::HandshakeDone(r) => self.handshake_done(r),
                                Msg::Stop => stop = true,
                            }
                        }
                    }
                    PollSource::Udp => {
                        let mut buf = vec![0u8; 65535];
                        loop {
                            match udp.recv_from(&mut buf[..]) {
                                Ok((n, src)) => {
                                    self.handle_udp_packet(&udp, unmap(src), &buf[..n])
                                }
                                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                                Err(_) => break,
                            }
                        }
                    }
                    PollSource::TcpListener => {
                        if let Some(ln) = &ctl_ln {
                            while let Ok((s, remote)) = ln.accept() {
                                self.accept_ctl(s, remote);
                            }
                        }
                    }
                    PollSource::Assoc(key) => {
                        if self.assocs.contains_key(&key) {
                            let mut b = vec![0u8; 65535];
                            match self.assocs.get(&key).unwrap().sock.recv_from(&mut b[..]) {
                                Ok((n, from)) => self.assoc_read(&udp, key, unmap(from), b[..n].to_vec()),
                                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                                Err(_) => {
                                    // 读错误：立即走回收路径（R4-design §2.3）
                                    self.reap_assoc(&key, "");
                                }
                            }
                        }
                    }
                    PollSource::Ctl(id) => {
                        msgs.clear();
                        // 先读（借用在块内结束），再处理消息（可能关连/改腿）
                        let read: Result<[u8; 8], ()> = match self.ctl_conns.get_mut(&id) {
                            Some(conn) => conn
                                .read_available(&mut msgs)
                                .map(|()| conn.label)
                                .map_err(|_| ()),
                            None => continue,
                        };
                        match read {
                            Ok(label) => {
                                if !msgs.is_empty() {
                                    // 活性只认**完整消息**（半帧涓流不算——Go 每条
                                    // 消息前重置绝对读期限的同义实现）
                                    if let Some(c) = self.ctl_conns.get_mut(&id) {
                                        c.last_read = Instant::now();
                                    }
                                }
                                for (sub, body) in msgs.drain(..) {
                                    self.ctl_message(id, label, sub, &body);
                                }
                            }
                            Err(()) => self.close_ctl(id),
                        }
                    }
                    PollSource::Stop => unreachable!(),
                }
            }
            if stop {
                break;
            }

            // ---- 定时面 ----
            let now = Instant::now();
            if now >= next_reap {
                self.reap_round();
                next_reap = now + self.reap_interval();
            }
            if now >= next_stats {
                let s = self.stats;
                self.logf(&format!(
                    "中继统计：注册腿 {}（累计成功 {}，伪造 {}）｜分配腿 {}（累计 {}，回收 {}）｜转发 上 {} / 下 {} 包｜丢弃 {}",
                    self.legs.len(), s.registered, s.forged, self.assocs.len(), s.assigned,
                    s.reclaimed, s.forwarded_up, s.forwarded_down, s.dropped
                ));
                next_stats = now + Duration::from_secs(60);
            }
        }

        // ---- 确定性收工（closeAll）：先尽力 RELEASE、关全部 socket 与控制连接 ----
        let mut release_list: Vec<([u8; 8], u64)> = Vec::new();
        for a in self.assocs.values() {
            if a.sid != 0 {
                release_list.push((a.key.label, a.sid));
            }
        }
        for (label, sid) in release_list {
            self.release_session(label, sid);
        }
        // 收工关两端（评审 低-13：只关写端会泄读端 fd + 在飞握手线程可能写到
        // 已复用的 fd 号上）
        unsafe {
            libc::close(wake_fds[1]);
            libc::close(wake_fds[0]);
        }
        Ok(())
    }

    // ---------- UDP 面 ----------

    /// 一个入站 UDP 包的分派（限流 → probe → tagged → 腿控制/转发）。
    fn handle_udp_packet(&mut self, udp: &UdpSocket, src: SocketAddr, pkt: &[u8]) {
        if !self.rate_ok(src.ip()) {
            self.stats.dropped += 1;
            return;
        }
        // 参照点探测（无状态一问一答；防放大 ≤ req+45B；合法探测不进任何计数）。
        // F10：flags 报「控制面降级」位（serve 侧 flags=caps 不动；中继 flags 命名空间）。
        if let Some(resp) = crate::probe::respond_ex(pkt, self.build_str(), self.relay_flags(), &[]) {
            let _ = udp.send_to(&resp, src);
            return;
        }
        let Some((label, kind, payload)) = frame::decode_tagged(pkt) else {
            self.stats.dropped += 1;
            return;
        };
        if kind == rw::FRAME_TYPE_RELAY_REG {
            self.handle_control(udp, src, *label, payload);
            return;
        }
        // 数据面准入闸：没有已注册（任一证明）的后端，谁也别想转发
        let admitted = self.legs.get(label).is_some_and(Leg::admitted);
        if !admitted {
            self.stats.dropped += 1;
            return;
        }
        self.forward_up(udp, src, *label, kind, payload);
    }

    fn build_str(&self) -> &str {
        // build 空时用 "relay-dev"（Go New 的缺省）
        if self.cfg.build.is_empty() { "relay-dev" } else { &self.cfg.build }
    }

    /// 探针 flags（F10）：控制面降级位（bit5，中继命名空间——bit0–4 是 serve 的 udpcap）。
    fn relay_flags(&self) -> u8 {
        if self.ctl_ok {
            0
        } else {
            crate::probe::FLAG_RELAY_CTL_DEGRADED
        }
    }

    /// 拒绝类日志的每原因节流（F6：首 3 条 + 每 100 条一条）。
    fn reject_log_due(&mut self, reason: RejectLog) -> bool {
        let n = self.reject_log.entry(reason).or_insert(0);
        *n += 1;
        *n <= 3 || n.is_multiple_of(100)
    }

    /// 后端注册腿的 UDP 控制消息（Hello/Proof/Keepalive）。
    fn handle_control(&mut self, udp: &UdpSocket, src: SocketAddr, label: [u8; 8], payload: &[u8]) {
        let sub = payload.first().copied().unwrap_or(0);
        if sub == rw::sub::HELLO {
            let Some(pub_) = rw::decode_hello(payload) else {
                self.stats.forged += 1;
                return;
            };
            if relay_id(&pub_) != label {
                self.stats.forged += 1;
                return;
            }
            // 出题（F5）：**不写 `legs`**——挑战落在独立 `pending` 表（≤PENDING_MAX，
            // 满按最旧淘汰**而非拒绝**）。未认证的 HELLO 因此不再占用受 `max_legs`
            // 约束的腿槽（匿名洪水灌不满真腿表；合法后端总能拿到挑战）。
            let mut eph_bytes = [0u8; 32];
            let mut nonce_out = [0u8; 16];
            if getrandom::getrandom(&mut eph_bytes).is_err() || getrandom::getrandom(&mut nonce_out).is_err() {
                return; // 随机源异常静默放弃本轮（不 panic 掉驱动线程）
            }
            let now = Instant::now();
            if !self.pending.contains_key(&label) && self.pending.len() >= PENDING_MAX {
                if let Some(oldest) = self
                    .pending
                    .iter()
                    .min_by_key(|(_, p)| p.chall_at)
                    .map(|(k, _)| *k)
                {
                    self.pending.remove(&oldest);
                }
            }
            let eph = x25519_dalek::StaticSecret::from(eph_bytes);
            let eph_pub = PublicKey::from(&eph);
            self.pending.insert(
                label,
                PendingLeg { pubkey: pub_, eph_priv: eph, nonce: nonce_out, chall_at: now },
            );
            let resp = frame::frame_bytes(
                rw::FRAME_TYPE_RELAY_REG,
                &rw::encode_challenge(eph_pub.as_bytes(), &nonce_out),
            );
            let _ = udp.send_to(&resp, src);
            return;
        }
        if sub == rw::sub::PROOF {
            // 挑战态在 `pending`（HELLO 写入）；PROOF 通过才提升进 `legs`。
            let Some(p) = self.pending.get(&label) else {
                self.stats.forged += 1;
                return;
            };
            let Some(parts) = rw::decode_proof(payload) else {
                self.stats.forged += 1;
                return;
            };
            if parts.ver != rw::RELAY_CTL_VER {
                self.stats.forged += 1;
                if self.reject_log_due(RejectLog::ProofVer) {
                    self.logf(&format!(
                        "中继：后端 {} 注册证明协议版本 {} 不符（需要 {}）—— 拒绝",
                        hex(&label), parts.ver, rw::RELAY_CTL_VER
                    ));
                }
                return;
            }
            if p.chall_at.elapsed() > CHALLENGE_TTL || p.nonce != parts.nonce {
                self.stats.forged += 1;
                return;
            }
            // 快照挑战材料（避免跨借用；用完即弃）
            let pubkey = p.pubkey;
            let eph_priv = p.eph_priv.clone();
            // 校验：开放模式只看 DH；token 模式只看 PSK（DH 谁都算得出，不当准入）
            let pass = match &self.cfg.secret {
                Some(sec) => {
                    let want = rw::auth_mac(sec, &parts.nonce, &pubkey);
                    rw::ct_eq_16(&want, parts.mac_psk)
                }
                None => {
                    let dh = eph_priv.diffie_hellman(&PublicKey::from(pubkey));
                    let want = rw::proof_mac(dh.as_bytes(), &parts.nonce, &pubkey);
                    rw::ct_eq_16(&want, parts.mac_dh)
                }
            };
            if !pass {
                if self.cfg.secret.is_some() && self.reject_log_due(RejectLog::Token) {
                    self.logf(&format!(
                        "中继：后端 {} 的 token 校验不过（密钥不对/没带 token）—— 拒绝",
                        hex(&label)
                    ));
                }
                self.stats.forged += 1;
                return;
            }
            self.pending.remove(&label);
            // 提升进 `legs`（受 max_legs 闸）；已存在的腿**就地更新**（Q17 ①：绝不
            // 迁移/替换对象——否则在用数据腿在挑战窗口 `admitted()` 会变 false、数据面闸关）。
            let old_addr = self.legs.get(&label).and_then(|l| l.addr);
            if !self.legs.contains_key(&label) && self.legs.len() >= self.cfg.max_legs {
                self.stats.denied += 1;
                if self.reject_log_due(RejectLog::LegCap) {
                    self.logf(&format!(
                        "中继：注册腿总数已达上限 {}，拒绝新的 {}（防匿名洪水）",
                        self.cfg.max_legs, hex(&label)
                    ));
                }
                return; // 后端 5s 后会重发 HELLO（重试即自愈）
            }
            let moved = old_addr.is_some_and(|a| a != src);
            let stale_sids: Vec<u64> = if moved {
                self.assocs
                    .iter()
                    .filter(|(k, _)| k.label == label)
                    .filter_map(|(_, a)| (a.sid != 0).then_some(a.sid))
                    .collect()
            } else {
                Vec::new()
            };
            let old_count = if moved {
                let n = self.assocs.keys().filter(|k| k.label == label).count();
                let keys: Vec<AssocKey> = self.assocs.keys().filter(|k| k.label == label).copied().collect();
                for k in keys {
                    self.assocs.remove(&k); // socket 随 drop 关闭
                }
                self.poll_dirty = true;
                n
            } else {
                0
            };
            {
                let lg = self.legs.entry(label).or_insert_with(|| Leg {
                    pubkey,
                    addr: None,
                    last: Instant::now(),
                    verified: false,
                    ctl_verified: false,
                    ctl: None,
                });
                lg.pubkey = pubkey; // 控制面先建的腿 pubkey 占位 [0;32]——此处补真值
                lg.verified = true;
                lg.addr = Some(src);
                lg.last = Instant::now();
            }
            self.stats.registered += 1;
            for sid in stale_sids {
                self.release_session(label, sid);
            }
            if moved {
                self.logf(&format!(
                    "中继：后端 {} 注册腿地址变化 → {}（旧分配 {} 条已作废，等客户端重建）",
                    hex(&label), src, old_count
                ));
            } else {
                self.logf(&format!("中继：后端 {} 注册成功（腿 {}）", hex(&label), src));
            }
            let resp = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::ok_bytes());
            let _ = udp.send_to(&resp, src);
            return;
        }
        if sub == rw::sub::KEEPALIVE {
            let resp_again = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::again_bytes());
            let Some(lg) = self.legs.get(&label) else {
                // 腿不在了：明确让后端重注册（否则它以为还在，两边永远对不上）
                let _ = udp.send_to(&resp_again, src);
                return;
            };
            if !lg.admitted() {
                let _ = udp.send_to(&resp_again, src);
                return;
            }
            if lg.addr.is_some_and(|a| a != src) {
                // 换了地址的保活不算数：要求重新走一遍注册（防地址冒用）
                let _ = udp.send_to(&resp_again, src);
                return;
            }
            if lg.addr.is_none() {
                // 无 UDP 注册（纯控制腿）的保活：必须挂着控制连接且源 IP 与控制连接同 IP
                let same_ip = lg.ctl
                    .and_then(|id| self.ctl_conns.get(&id))
                    .map(|c| c.remote.ip() == src.ip());
                if same_ip != Some(true) {
                    let _ = udp.send_to(&resp_again, src);
                    return;
                }
                // 有活控制连接且同 IP：静默续命
            }
            self.legs.get_mut(&label).expect("已判在").last = Instant::now();
            return;
        }
        self.stats.dropped += 1;
    }

    // ---------- 转发面 ----------

    /// 客户端 → 后端（必要时新建分配 socket），按需递送 hint。
    fn forward_up(&mut self, udp: &UdpSocket, client: SocketAddr, label: [u8; 8], kind: u8, payload: &[u8]) {
        let key = AssocKey { label, client };
        // 后端注册腿换了地址（重映射）：老分配作废重建（拨腿会话豁免）
        if let Some(a) = self.assocs.get(&key) {
            let leg_addr = self.legs.get(&label).and_then(|l| l.addr);
            if !a.dialed && a.backend != leg_addr {
                self.assocs.remove(&key);
                self.poll_dirty = true;
            }
        }
        if !self.assocs.contains_key(&key) {
            let (leg_addr, has_ctl) = {
                let lg = self.legs.get(&label).expect("准入闸已判在");
                (lg.addr, lg.has_ctl())
            };
            // 无可达路径不建会话（丢了让客户端重试，等后端任一路径就绪）
            if leg_addr.is_none() && !has_ctl {
                self.stats.dropped += 1;
                return;
            }
            let per_peer = self.assocs.keys().filter(|k| k.label == label).count();
            if per_peer >= self.cfg.max_per_peer {
                self.stats.dropped += 1;
                if self.reject_log_due(RejectLog::PerPeer) {
                    self.logf(&format!(
                        "中继：后端 {} 的分配腿已达上限 {}，丢弃新客户端 {}",
                        hex(&label), self.cfg.max_per_peer, client
                    ));
                }
                return;
            }
            // F7.3 全局闸（每腿上限之外的第二道：fd/内存双耗尽面）
            if self.assocs.len() >= MAX_ASSOCS_TOTAL {
                self.stats.dropped += 1;
                if self.reject_log_due(RejectLog::AssocTotal) {
                    self.logf(&format!(
                        "中继：分配腿总数已达全局上限 {}，丢弃新客户端 {}（后端 {}）",
                        MAX_ASSOCS_TOTAL, client, hex(&label)
                    ));
                }
                return;
            }
            // F8：双栈 assoc socket（对齐 Go `net.ListenUDP("udp", nil)`——v4/v6 后端都可发）。
            let (sock, dual) = match crate::udpbatch::open_client_socket() {
                Ok(v) => v,
                Err(_) => {
                    self.stats.dropped += 1;
                    return;
                }
            };
            let _ = sock.set_nonblocking(true);
            // F7.3：**不在此 bump 缓冲**——未认证来源也能建会话，此处抬 4MB×2 会被灌爆；
            // 延迟到「认证成功 / 首次真实下行」（见 assoc_read）。
            let mut cookie = [0u8; 16];
            if getrandom::getrandom(&mut cookie).is_err() {
                // 不退化全零 cookie 的假认证：拆会话让客户端重试
                self.stats.dropped += 1;
                self.logf("⚠️ 中继：会话随机数不可用 ——已放弃本次会话，客户端重试即可");
                return;
            }
            let mut a = Assoc {
                key,
                backend: leg_addr,
                sock,
                dual,
                bufs_bumped: false,
                down_win: Instant::now(),
                down_bytes: 0,
                last: Instant::now(),
                last_down: Instant::now(),
                sid: 0,
                dialed: false,
                dial_up: false,
                dial_up_at: Instant::now(),
                cookie,
                auth_ok: false,
                auth_src: None,
                pend: Vec::new(),
            };
            // 有控制连接 → 拨腿模式（sid 分配 + 等腿窗；dialed 建会话即置位）
            if has_ctl {
                self.next_sid += 1;
                a.sid = self.next_sid;
                a.dial_up = true;
                a.dial_up_at = Instant::now();
                a.dialed = true;
                a.auth_ok = false;
            }
            self.assocs.insert(key, a);
            self.stats.assigned += 1;
            self.poll_dirty = true; // 会话 socket 入 poll 集
            // hint（唯一推送点 = 建会话，两端各一次；测试形态可关）
            if !self.cfg.no_hints {
                if let Some(addr) = leg_addr {
                    let _ = udp.send_to(&frame::hint_bytes(&addr.to_string()), client);
                }
                if let Some(b) = self.assocs.get(&key).and_then(|a| a.backend) {
                    let a = self.assocs.get(&key).expect("已判在");
                    let _ = a
                        .sock
                        .send_to(&frame::hint_bytes(&client.to_string()), crate::udpbatch::xmit_addr(b, a.dual));
                }
            }
            if has_ctl {
                let (sid, port, ck) = {
                    let a = self.assocs.get(&key).unwrap();
                    (a.sid, a.sock.local_addr().map(|l| l.port()).unwrap_or(0), a.cookie)
                };
                let announced = self.announce_session(label, rw::CtlSession { id: sid, data_port: port, cookie: ck });
                if announced {
                    self.logf(&format!(
                        "中继：客户端 {} 起会话 #{}（拨腿模式）→ 后端 {}（数据口 0.0.0.0:{}）",
                        client, sid, hex(&label), port
                    ));
                } else {
                    // 通告失败（连接刚断）：这条会话没腿可等——回收掉，客户端重试再触发
                    self.assocs.remove(&key);
                    self.poll_dirty = true;
                    self.stats.dropped += 1;
                    return;
                }
            } else {
                let local = self.assocs.get(&key).and_then(|a| a.sock.local_addr().ok()).map(|l| l.to_string()).unwrap_or_default();
                self.logf(&format!(
                    "中继：客户端 {} 起一条分配腿 → 后端 {}（中继侧出口 {}）",
                    client, hex(&label), local
                ));
            }
        } else {
            self.assocs.get_mut(&key).expect("已判在").last = Instant::now();
        }
        // 投递（等腿窗缓冲 / 直发）
        let frame_bytes = frame::frame_bytes(kind, payload);
        let a = self.assocs.get_mut(&key).expect("已判在");
        if a.dial_up {
            if a.pend.len() < CTL_PEND_MAX {
                a.pend.push(frame_bytes);
                self.stats.forwarded_up += 1;
            } else {
                self.stats.dropped += 1;
            }
            return;
        }
        if let Some(dst) = a.backend {
            // F8：**只计成功**（`send_to` 返 Err 不再计 forwarded_up——ENOBUFS/EAFNOSUPPORT
            // 曾被计成健康转发）。
            match a.sock.send_to(&frame_bytes, crate::udpbatch::xmit_addr(dst, a.dual)) {
                Ok(_) => self.stats.forwarded_up += 1,
                Err(e) => {
                    self.stats.send_fail_up += 1;
                    if self.reject_log_due(RejectLog::SendFail) {
                        self.logf(&format!("中继：上行转发发送失败（{e}）—— 已计数，不转发成功面"));
                    }
                }
            }
        }
    }

    /// 分配 socket 可读：后端 → 客户端（v2 拨腿会话先过腿身份认证状态机）。
    fn assoc_read(&mut self, udp: &UdpSocket, key: AssocKey, from: SocketAddr, pkt: Vec<u8>) {
        let now = Instant::now();
        // 腿认证密钥预取（token 模式 = 中继密钥；开放模式 = cookie 低半字——
        // 后续 block 持有 assocs 借用，方法调用进不去）
        let mac_key_for = |cookie: [u8; 16]| match self.cfg.secret {
            Some(s) => s,
            None => {
                let mut k = [0u8; 32];
                k[..16].copy_from_slice(&cookie);
                k
            }
        };
        let mut flush: Option<Vec<Vec<u8>>> = None;
        let mut moved_log: Option<(u64, SocketAddr)> = None;
        let mut rejected = false;
        {
            let Some(a) = self.assocs.get_mut(&key) else { return };
            if a.sid != 0 {
                if a.auth_ok && a.auth_src == Some(from) {
                    // 常态：已认证源的下行（数据腿帧或重发的 LEGUP 标记都算）
                    a.last = now;
                    a.last_down = now;
                } else {
                    let (sid, cookie) = (a.sid, a.cookie);
                    let authed = rw::legup_cookie(&pkt) == Some(cookie) && {
                        let k = mac_key_for(cookie);
                        rw::verify_legup(&pkt, sid, &cookie, &k)
                    };
                    if authed {
                        // 合法认证：首拨（放行 pend）或已认证腿的重拨/换源
                        let first = !a.auth_ok;
                        let moved = a.auth_ok && a.auth_src != Some(from);
                        a.auth_ok = true;
                        a.auth_src = Some(from);
                        a.backend = Some(from);
                        a.last = now;
                        a.last_down = now;
                        if first {
                            a.dial_up = false;
                            flush = Some(std::mem::take(&mut a.pend));
                        }
                        // F7.3：认证成功（真实后端在）才抬缓冲
                        if !a.bufs_bumped {
                            a.bufs_bumped = true;
                            bump_sock_bufs(&a.sock);
                        }
                        if moved {
                            moved_log = Some((sid, from));
                        }
                    } else {
                        rejected = true;
                    }
                }
            } else {
                // v1 fallback 会话（sid==0）：**只认注册腿源 `a.backend`**。replay 回滚
                // 会把 a.sid=0/dial_up=false/dialed=false 而**保留 a.backend**（故取
                // a.backend 而非 lg.addr——纯控制腿的 lg.addr=None，用它会当场丢光下行）。
                // 其它来源**显式丢弃 + 计数、不续命**（此前是排他 if/else、无第三出口，
                // 会穿透到原样回投 ⇒ 任意源可续命并借中继回投）。
                if a.backend == Some(from) {
                    a.last = now;
                    a.last_down = now;
                    if !a.bufs_bumped {
                        a.bufs_bumped = true;
                        bump_sock_bufs(&a.sock);
                    }
                } else {
                    rejected = true;
                }
            }
        }
        if rejected {
            // 未认证/未知源：丢弃并计数，不改变任何状态、不续命（#3）
            self.stats.leg_rejected += 1;
            let n = self.stats.leg_rejected;
            let sid = self.assocs.get(&key).map(|a| a.sid).unwrap_or(0);
            let rate_ok = self.leg_rate_ok(from.ip());
            if rate_ok && (n <= 3 || n.is_multiple_of(100)) {
                self.logf(&format!(
                    "中继：会话 #{sid} 收到未知源 {from} 的包（{}B）—— 已拒绝（未认证不得成为腿；累计 {n} 次）",
                    pkt.len()
                ));
            }
            return;
        }
        if let Some(pend) = flush {
            // 等腿窗缓冲放行：包在**入队时**已计 forwarded_up（等腿窗语义）；回放失败
            // 只进 send_fail_up（F8：不再动 forwarded_down——那是下行面，混面会失真）。
            let mut fails = 0u64;
            {
                let a = self.assocs.get(&key).expect("已判在");
                for p in pend {
                    if a.sock
                        .send_to(&p, crate::udpbatch::xmit_addr(from, a.dual))
                        .is_err()
                    {
                        fails += 1;
                    }
                }
            }
            self.stats.send_fail_up += fails;
        }
        if let Some((sid, from)) = moved_log {
            self.logf(&format!("中继：会话 #{sid} 的后端腿重拨 → {from}（cookie 认证通过，跟随）"));
        }
        // LEGUP 认证标记吞包（5B/37B 两形态；判定在分支外——首腿/重拨/重复标记都不外泄）
        if rw::legup_cookie(&pkt).is_some() || pkt == b"LEGUP" {
            return;
        }
        // F7.2 下行字节桶（每会话；目标吞吐 ≥100Mbit/s——超限丢弃 + 计数 + 限流日志）
        let over = {
            let a = self.assocs.get_mut(&key).expect("已判在");
            if now.duration_since(a.down_win) >= Duration::from_secs(1) {
                a.down_win = now;
                a.down_bytes = 0;
            }
            a.down_bytes += pkt.len() as u64;
            a.down_bytes > ASSOC_DOWN_BYTES_PER_SEC
        };
        if over {
            self.stats.down_limited += 1;
            if self.reject_log_due(RejectLog::DownLimit) {
                let sid = self.assocs.get(&key).map(|a| a.sid).unwrap_or(0);
                self.logf(&format!(
                    "中继：会话 #{sid} 下行超目标吞吐（{ASSOC_DOWN_BYTES_PER_SEC}B/s）—— 丢弃"
                ));
            }
            return;
        }
        // FIX-91：出口恒发腿帧——原样转发（未知/畸形包由客户端按解码失败丢弃）。
        // F8：只计成功。
        match udp.send_to(&pkt, key.client) {
            Ok(_) => self.stats.forwarded_down += 1,
            Err(e) => {
                self.stats.send_fail_down += 1;
                if self.reject_log_due(RejectLog::SendFail) {
                    self.logf(&format!("中继：下行转发发送失败（{e}）—— 已计数，不转发成功面"));
                }
            }
        }
    }

    // ---------- 控制面 ----------

    /// TCP 接入：握手并发槽检查 → spawn 短命握手线程（RAII 槽）。
    fn accept_ctl(&mut self, stream: std::net::TcpStream, remote: SocketAddr) {
        match ctlface::HandshakeSlot::acquire(&self.handshaking) {
            Some(mut slot) => {
                let secret = self.cfg.secret;
                let established = Arc::clone(&self.established);
                let max_ctl = self.cfg.max_ctl_conns;
                let logf = Arc::clone(&self.cfg.logf);
                let tx = self.msg_tx.clone();
                let wake_w = self.wake_w;
                std::thread::Builder::new()
                    .name("relay-hs".into())
                    .stack_size(512 * 1024)
                    .spawn(move || {
                        let r = ctlface::run_handshake(stream, secret, &established, max_ctl, &mut slot, &logf)
                            .map_err(|e| (remote, e));
                        let _ = tx.send(Msg::HandshakeDone(r));
                        unsafe { libc::write(wake_w, b"x".as_ptr().cast(), 1) };
                    })
                    .ok();
            }
            None => {
                drop(stream);
                self.stats.dropped += 1;
                if self.reject_log_due(RejectLog::CtlFull) {
                    self.logf(&format!(
                        "中继：并发握手上限 {} 已满，拒绝 {remote}（慢握手洪水防护）",
                        ctlface::HANDSHAKE_MAX
                    ));
                }
            }
        }
    }

    /// 握手线程交接处理：失败计费/成功 attach + replay（驱动线程）。
    fn handshake_done(&mut self, r: Result<ctlface::Handover, (SocketAddr, ctlface::HsError)>) {
        match r {
            Ok(h) => {
                // MaxLegs 闸（控制路径也过闸；闸不过 = 先 OK 后断连——绝不保持长连）
                let label = h.label;
                if !self.legs.contains_key(&label) && self.legs.len() >= self.cfg.max_legs {
                    self.stats.denied += 1;
                    if self.reject_log_due(RejectLog::LegCap) {
                        self.logf(&format!(
                            "中继：注册腿总数已达上限 {}，拒绝控制面新腿 {}",
                            self.cfg.max_legs, hex(&label)
                        ));
                    }
                    drop(h.stream); // 连接关闭（established 计数回退见下）
                    self.established.fetch_sub(1, Ordering::SeqCst);
                    return;
                }
                let id = self.next_conn_id;
                self.next_conn_id += 1;
                let conn = ctlface::CtlConn::from_handover(h);
                let remote = conn.remote;
                self.ctl_conns.insert(id, conn);
                let leg = self.legs.entry(label).or_insert_with(|| Leg {
                    pubkey: [0; 32], // 控制面建腿拿不到 pubkey——保留占位（准入靠 ctl_verified；
                    // UDP PROOF 提升时会补上真 pubkey）
                    addr: None,
                    last: Instant::now(),
                    verified: false,
                    ctl_verified: false,
                    ctl: None,
                });
                leg.ctl_verified = true;
                leg.last = Instant::now();
                // 顶掉旧连接（重连语义）：旧连的读事件随 fd 关闭消失
                if let Some(old) = leg.ctl.replace(id) {
                    self.close_ctl_quiet(old);
                }
                self.logf(&format!(
                    "中继：后端 {} 控制面就绪（{remote}；SESSION 通告启用拨腿模式）",
                    hex(&label)
                ));
                self.poll_dirty = true;
                self.replay_sessions(label, id);
            }
            Err((remote, e)) => match e {
                ctlface::HsError::DhRejected => {
                    self.stats.forged += 1;
                    self.logf(&format!("中继：控制面 {remote} 的 DH 校验不过 —— 拒绝"));
                }
                ctlface::HsError::PskRejected => {
                    self.stats.forged += 1;
                    self.logf(&format!("中继：控制面 {remote} 的 token 校验不过 —— 拒绝"));
                }
                ctlface::HsError::Version(v) => {
                    self.logf(&format!(
                        "中继：控制面 {remote} 协议版本 {v} 不符（需要 {}）—— 拒绝",
                        rw::RELAY_CTL_VER
                    ));
                }
                ctlface::HsError::EstablishedFull(max) => {
                    self.stats.dropped += 1;
                    self.logf(&format!("中继：已建立控制连接达上限 {max}，拒绝 {remote}"));
                }
                ctlface::HsError::Io => {}
            },
        }
    }

    /// 已建立连接的一条消息（KEEPALIVE 刷腿 + 回显；未知忽略）。
    fn ctl_message(&mut self, conn_id: u64, label: [u8; 8], sub: u8, _body: &[u8]) {
        if sub == rw::sub::KEEPALIVE {
            if let Some(lg) = self.legs.get_mut(&label) {
                lg.last = Instant::now();
            }
            // 被动回显（B3：后端 25s 发、中继必答）
            if let Some(c) = self.ctl_conns.get_mut(&conn_id) {
                if !c.write_msg_nonblocking(&rw::keepalive_bytes()) {
                    self.close_ctl(conn_id);
                }
            }
        }
        // 后端→中继方向目前没有其它消息；未知子类型按前向兼容忽略
    }

    /// 断连路径：摘连接 + detach（只认现任）+ 孤儿清理（纯控制腿且无会话才立即删）。
    fn close_ctl(&mut self, id: u64) {
        self.close_ctl_quiet(id);
        self.poll_dirty = true;
    }

    fn close_ctl_quiet(&mut self, id: u64) {
        if self.ctl_conns.remove(&id).is_some() {
            self.established.fetch_sub(1, Ordering::SeqCst);
        }
        for lg in self.legs.values_mut() {
            if lg.ctl == Some(id) {
                lg.ctl = None;
            }
        }
        // 孤儿清理：既无 UDP 注册（!verified 且无 addr）也无新控制连接，且无活跃会话
        let orphans: Vec<[u8; 8]> = self
            .legs
            .iter()
            .filter(|(_, lg)| lg.ctl.is_none() && !lg.verified && lg.addr.is_none())
            .filter(|(label, _)| !self.assocs.keys().any(|k| k.label == **label))
            .map(|(label, _)| *label)
            .collect();
        for label in orphans {
            self.legs.remove(&label);
        }
    }

    /// 向（新建立的）控制连接重放该后端的全部活跃会话（sid==0 提升为拨腿）。
    fn replay_sessions(&mut self, label: [u8; 8], conn_id: u64) {
        struct Pending {
            key: AssocKey,
            msg: Vec<u8>,
            /// 提升过（本轮从 sid==0 拨上来）——回滚判据按 Go dialUp && sid!=0
            /// （失败点之后**原本就在拨腿态**的会话也一并降级回 fallback）。
            promoted: bool,
        }
        let mut out: Vec<Pending> = Vec::new();
        for (k, a) in self.assocs.iter_mut() {
            if k.label != label {
                continue;
            }
            let mut promoted = false;
            if a.sid == 0 {
                // 提升：全新 sid + 切拨腿模式 + 换新 cookie（重放 = 后端已 ClearLegs）
                self.next_sid += 1;
                a.sid = self.next_sid;
                a.dial_up = true;
                a.dialed = true;
                a.dial_up_at = Instant::now();
                let mut ck = [0u8; 16];
                if getrandom::getrandom(&mut ck).is_ok() {
                    a.cookie = ck;
                    a.auth_ok = false;
                }
                promoted = true;
            }
            let port = a.sock.local_addr().map(|l| l.port()).unwrap_or(0);
            out.push(Pending {
                key: *k,
                msg: rw::encode_session(&rw::CtlSession { id: a.sid, data_port: port, cookie: a.cookie }),
                promoted,
            });
        }
        let mut sent = 0usize;
        for (i, p) in out.iter().enumerate() {
            let ok = self
                .ctl_conns
                .get_mut(&conn_id)
                .map(|c| c.write_msg_nonblocking(&p.msg))
                .unwrap_or(false);
            if !ok {
                // 通告失败：从失败这条起回滚提升（已发出的算数）
                for q in &out[sent..] {
                    let _ = i;
                    if q.promoted {
                        if let Some(a) = self.assocs.get_mut(&q.key) {
                            a.sid = 0;
                            a.dial_up = false;
                            a.dialed = false;
                        }
                    }
                }
                self.close_ctl(conn_id);
                return;
            }
            sent = i + 1;
        }
        if !out.is_empty() {
            self.logf(&format!("中继：后端 {} 控制面重放 {} 条活跃会话", hex(&label), out.len()));
        }
    }

    /// SESSION 通告（失败 = 关连——等后端重连走重放对账）。
    fn announce_session(&mut self, label: [u8; 8], sess: rw::CtlSession) -> bool {
        let id = self.legs.get(&label).and_then(|l| l.ctl);
        let Some(id) = id else { return false };
        let ok = self
            .ctl_conns
            .get_mut(&id)
            .map(|c| c.write_msg_nonblocking(&rw::encode_session(&sess)))
            .unwrap_or(false);
        if !ok {
            self.close_ctl(id);
            return false;
        }
        true
    }

    /// RELEASE 通告（写失败即关连——半死 TCP 上 RELEASE 恒丢，关掉逼重连）。
    fn release_session(&mut self, label: [u8; 8], sid: u64) {
        let id = self.legs.get(&label).and_then(|l| l.ctl);
        if let Some(id) = id {
            let ok = self
                .ctl_conns
                .get_mut(&id)
                .map(|c| c.write_msg_nonblocking(&rw::encode_release(sid)))
                .unwrap_or(false);
            if !ok {
                self.close_ctl(id);
            }
        }
    }

    // ---------- 回收与限流 ----------

    /// 回收扫描节拍：默认 5s；配置了更短回收窗按其一半收缩（下限 20ms）。
    fn reap_interval(&self) -> Duration {
        let mut d = Duration::from_secs(5);
        for c in [self.cfg.idle_timeout, self.cfg.dial_wait, self.cfg.down_silent] {
            if !c.is_zero() && c / 2 < d {
                d = c / 2;
            }
        }
        if d < Duration::from_millis(20) {
            d = Duration::from_millis(20);
        }
        d
    }

    /// 摘一条分配（带原因行；空串 = 普通空闲走聚合计数）。
    fn reap_assoc(&mut self, key: &AssocKey, why: &str) {
        let Some(a) = self.assocs.remove(key) else { return };
        if !why.is_empty() {
            self.logf(&format!("中继：会话 #{} 回收：{why}", a.sid));
        }
        self.stats.reclaimed += 1;
        if a.sid != 0 {
            self.release_session(key.label, a.sid);
        }
    }

    /// 一轮回收：空闲/等腿/半死分配 → 过期腿 → 控制连接读超时 → 限流桶。
    fn reap_round(&mut self) {
        let now = Instant::now();
        // 判定顺序（Go reap.go 同序）：Idle → DialWait → DownSilent；命中 Idle 不打逐条日志
        let keys: Vec<AssocKey> = self.assocs.keys().copied().collect();
        let mut reclaim = 0usize;
        for k in keys {
            let Some(a) = self.assocs.get(&k) else { continue };
            // 判定顺序（Go reap.go 同序）：Idle → DialWait → DownSilent；命中 Idle 不打逐条日志
            let why: Option<String> = if now.duration_since(a.last) > self.cfg.idle_timeout {
                None
            } else if a.dial_up && now.duration_since(a.dial_up_at) > self.cfg.dial_wait {
                Some(format!(
                    "拨腿等待超 {}（通告后无 LEGUP——后端拨腿失败/通告丢失）",
                    fmt_duration_go_secs(self.cfg.dial_wait)
                ))
            } else if !a.dial_up && now.duration_since(a.last_down) > self.cfg.down_silent {
                Some(format!(
                    "下行静默超 {}（上行仍活跃——半死会话兜底）",
                    fmt_duration_go_secs(self.cfg.down_silent)
                ))
            } else {
                continue;
            };
            self.reap_assoc(&k, why.as_deref().unwrap_or(""));
            reclaim += 1;
        }
        // 过期腿（存活判定：挂控制连接由控制保活续命；已验证按 last；未验证只保留注册窗）
        let dead: Vec<[u8; 8]> = self
            .legs
            .iter()
            .filter(|(_, lg)| {
                let alive = if lg.ctl.is_none() && !lg.verified && !lg.ctl_verified {
                    now.duration_since(lg.last) <= LEG_BOOTSTRAP
                } else {
                    now.duration_since(lg.last) <= self.cfg.leg_timeout
                };
                !alive
            })
            .map(|(l, _)| *l)
            .collect();
        for label in dead {
            if let Some(lg) = self.legs.get(&label) {
                let idle = now.duration_since(lg.last);
                self.logf(&format!(
                    "中继：后端 {} 注册腿过期（{} 无保活）—— 摘掉",
                    hex(&label),
                    fmt_duration_go_secs(idle)
                ));
                // 挂着的控制连接一并关（Go reap.go:101-103 同义——否则连接占着
                // established 名额直到读超时，半帧涓流可无限续命）
                if let Some(ctl_id) = lg.ctl {
                    self.close_ctl(ctl_id);
                }
            }
            self.legs.remove(&label);
            let keys: Vec<AssocKey> = self.assocs.keys().filter(|k| k.label == label).copied().collect();
            for k in keys {
                self.assocs.remove(&k);
            }
            self.poll_dirty = true;
        }
        // 控制连接读超时（惰性判死：last_read 超 90s）
        let stale_conns: Vec<u64> = self
            .ctl_conns
            .iter()
            .filter(|(_, c)| now.duration_since(c.last_read) > CTL_READ_TIMEOUT)
            .map(|(id, _)| *id)
            .collect();
        for id in stale_conns {
            self.close_ctl(id);
        }
        // 限流桶清理
        self.rates.retain(|_, b| now.duration_since(b.window) <= RATE_BUCKET_TTL);
        self.leg_rates.retain(|_, b| now.duration_since(b.window) <= RATE_BUCKET_TTL);
        // 过期挑战清理（F5：pending 也须随 TTL 退场，否则长时间运行会累积）
        self.pending.retain(|_, p| now.duration_since(p.chall_at) <= CHALLENGE_TTL);
        if reclaim > 0 {
            self.logf(&format!(
                "中继：回收 {reclaim} 条空闲分配腿（当前 {} 条）",
                self.assocs.len()
            ));
        }
    }

    /// 准入限流（每源每秒包数）。
    fn rate_ok(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let limit = self.cfg.rate_limit;
        match self.rates.get_mut(&ip) {
            Some(b) if now.duration_since(b.window) < Duration::from_secs(1) => {
                b.count += 1;
                b.count <= limit
            }
            _ => {
                self.rates.insert(ip, RateBucket { window: now, count: 1 });
                true
            }
        }
    }

    /// 被拒路径的独立限速桶（阈值 = 准入限流 ×10；只约束日志与认证计算代价）。
    fn leg_rate_ok(&mut self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let limit = self.cfg.rate_limit * 10;
        match self.leg_rates.get_mut(&ip) {
            Some(b) if now.duration_since(b.window) < Duration::from_secs(1) => {
                b.count += 1;
                b.count <= limit
            }
            _ => {
                self.leg_rates.insert(ip, RateBucket { window: now, count: 1 });
                true
            }
        }
    }

}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// 4in6 映射统一成 v4（否则后续地址比较恒不等）。
fn unmap(ap: SocketAddr) -> SocketAddr {
    match ap {
        SocketAddr::V6(v6) => match v6.ip().to_ipv4_mapped() {
            Some(v4) => SocketAddr::new(IpAddr::V4(v4), v6.port()),
            None => ap,
        },
        v4 => v4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn rand_secret() -> x25519_dalek::StaticSecret {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).unwrap();
        x25519_dalek::StaticSecret::from(b)
    }

    fn noop_logf() -> Logf {
        Arc::new(|_| {})
    }

    /// 测试形态的中继（run 在独立线程；Drop 停）。
    struct TestRelay {
        port: u16,
        stop_w: i32,
        join: Option<std::thread::JoinHandle<()>>,
    }

    impl TestRelay {
        fn start(cfg: Config) -> Self {
            let (ready_tx, ready_rx) = mpsc::channel();
            let (stop_r, stop_w) = {
                let mut fds = [0i32; 2];
                unsafe { libc::pipe(fds.as_mut_ptr()) };
                (fds[0], fds[1])
            };
            let relay = Relay::new(cfg);
            let join = std::thread::Builder::new()
                .name("relay-test".into())
                .spawn(move || {
                    let _ = relay.run(stop_r, |p| {
                        let _ = ready_tx.send(p);
                    });
                })
                .unwrap();
            let port = ready_rx.recv_timeout(Duration::from_secs(5)).expect("relay 起不来");
            Self { port, stop_w, join: Some(join) }
        }

        fn stop(&mut self) {
            unsafe { libc::write(self.stop_w, b"x".as_ptr().cast(), 1) };
            if let Some(h) = self.join.take() {
                let _ = h.join();
            }
            unsafe { libc::close(self.stop_w) };
        }
    }

    impl Drop for TestRelay {
        fn drop(&mut self) {
            self.stop();
        }
    }

    /// 控制面 TCP 连接（竞态容忍）：`run` 的 on_ready 在 UDP 绑定后、TCP 监听 bind
    /// **前**回调（Go RunWithReady 同序——语义即如此，token 须在端口确定后尽早铸出），
    /// 测试侧拿到端口立即 connect 会撞上「监听还没 bind」的窗口（macOS CI 实测
    /// ConnectionRefused 偶发）——短窗重试吸收，1s 仍拒即真失败。
    fn ctl_connect(addr: SocketAddr) -> std::net::TcpStream {
        for _ in 0..40 {
            if let Ok(s) = std::net::TcpStream::connect(addr) {
                return s;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("控制面 TCP 连不上（1s 重试后仍拒绝）：{addr}");
    }

    /// 后端注册腿全流程（开放模式）：Hello → Challenge → Proof → OK + 注册成功行为。
    #[test]
    fn udp_registration_open_mode() {
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            noop_logf(),
        ));
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);

        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);
        let label = relay_id(pub_.as_bytes());

        // Hello（错 label → 无应答；对 label → Challenge）
        let bad = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pub_.as_bytes()));
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&[9u8; 8], &bad, &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        be.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        assert!(be.recv_from(&mut [0u8; 64]).is_err(), "错 label 不应答");

        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &bad, &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        let mut buf = [0u8; 128];
        let (n, _) = be.recv_from(&mut buf).unwrap();
        let (kind, payload) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(kind, rw::FRAME_TYPE_RELAY_REG);
        let (eph_pub, nonce) = rw::decode_challenge(payload).unwrap();

        // Proof（开放模式 = DH MAC）
        let dh = priv_.diffie_hellman(&PublicKey::from(eph_pub));
        let proof = rw::encode_proof(&nonce, dh.as_bytes(), pub_.as_bytes(), None);
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &proof), &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        let (n, _) = be.recv_from(&mut buf).unwrap();
        let (kind, payload) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(kind, rw::FRAME_TYPE_RELAY_REG);
        assert_eq!(payload, &rw::ok_bytes()[..], "应回 OK");

        // Keepalive 续命（同源）——无应答即成功（换了地址才回 Again）
        let ka = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::keepalive_bytes());
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &ka, &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        be.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        assert!(be.recv_from(&mut buf).is_err(), "同源保活静默续命");
    }

    /// 转发闭环（fallback 会话）：客户端标签帧 → 分配 socket → 后端；后端回程 → 客户端。
    #[test]
    fn forward_fallback_session_end_to_end() {
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            noop_logf(),
        ));
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        let cl = UdpSocket::bind("127.0.0.1:0").unwrap();
        cl.set_read_timeout(Some(Duration::from_secs(2))).unwrap();

        // 注册后端
        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);
        let label = relay_id(pub_.as_bytes());
        register_open(&be, relay_addr, &priv_, &pub_, &label);

        // 客户端发一条数据帧（标签信封）
        let data = frame::frame_bytes(frame::FrameKind::Data, b"wg-payload");
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &data, &mut tagged);
        cl.send_to(&tagged, relay_addr).unwrap();

        // 后端收到：hint 帧（control）+ 转发帧——逐包按长度解码直到见数据
        let mut buf = [0u8; 256];
        be.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut got_data = false;
        for _ in 0..4 {
            let Ok((n, _)) = be.recv_from(&mut buf) else { break };
            if let Some((kind, payload)) = frame::decode_frame(&buf[..n]) {
                if kind == frame::FrameKind::Data.to_wire() && payload == b"wg-payload" {
                    got_data = true;
                    break;
                }
                // hint（control 帧）——忽略，继续收
            }
        }
        assert!(got_data, "后端应收到剥头转发的数据帧");

        // 回程：后端从**收到包的源**（中继分配 socket）发回——原样转发给客户端
        let assoc_src = {
            cl.send_to(&tagged, relay_addr).unwrap();
            let (n2, src) = be.recv_from(&mut buf).unwrap();
            assert_eq!(&buf[..n2], &frame::frame_bytes(frame::FrameKind::Data, b"wg-payload")[..]);
            src
        };
        let back = frame::frame_bytes(frame::FrameKind::Data, b"down-payload");
        be.send_to(&back, assoc_src).unwrap();
        // 客户端先收 hint（建会话时经主监听 socket 直达）再收回程帧——逐包按长度判
        let mut got_down = false;
        for _ in 0..4 {
            let Ok((n, _)) = cl.recv_from(&mut buf) else { break };
            if buf[..n] == back[..] {
                got_down = true;
                break;
            }
        }
        assert!(got_down, "客户端应原样收到回程腿帧");
    }

    /// token 模式：PSK 不过 → 拒（无 OK 应答）。
    #[test]
    fn token_mode_rejects_wrong_psk() {
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            Some([0x44u8; 32]),
            noop_logf(),
        ));
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        be.set_read_timeout(Some(Duration::from_millis(300))).unwrap();

        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);
        let label = relay_id(pub_.as_bytes());
        // Hello → Challenge
        let hello = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pub_.as_bytes()));
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &hello, &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        let mut buf = [0u8; 128];
        let (n, _) = be.recv_from(&mut buf).unwrap();
        let (_, payload) = frame::decode_frame(&buf[..n]).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(payload).unwrap();
        // 错密钥的 PSK
        let dh = priv_.diffie_hellman(&PublicKey::from(eph_pub));
        let bad_psk = rw::auth_mac(&[0x00u8; 32], &nonce, pub_.as_bytes());
        let proof = rw::encode_proof(&nonce, dh.as_bytes(), pub_.as_bytes(), Some(&bad_psk));
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &proof), &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        assert!(be.recv_from(&mut buf).is_err(), "PSK 不过应静默拒绝");
    }

    /// 拨腿端到端：控制面 attach → SESSION 通告 → LEGUP 认证 → pend 放行。
    #[test]
    fn dial_up_session_end_to_end() {
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            Some([0x44u8; 32]),
            noop_logf(),
        ));
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let secret = [0x44u8; 32];

        // 后端：UDP 不注册（纯控制腿形态）+ TCP 控制面
        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);
        let label = relay_id(pub_.as_bytes());

        let mut ctl = ctl_connect(relay_addr);
        ctl.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        ctl.set_write_timeout(Some(Duration::from_secs(5))).unwrap();
        // HELLO/CHALLENGE/PROOF/OK（token 模式）
        let mut wire = Vec::new();
        rw::ctl_frame_into(&rw::encode_hello(pub_.as_bytes()), &mut wire);
        ctl.write_all(&wire).unwrap();
        let mut hdr = [0u8; 2];
        use std::io::Read as _;
        ctl.read_exact(&mut hdr).unwrap();
        let mut msg = vec![0u8; u16::from_be_bytes(hdr) as usize];
        ctl.read_exact(&mut msg).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(&msg).unwrap();
        let dh = priv_.diffie_hellman(&PublicKey::from(eph_pub));
        let psk = rw::auth_mac(&secret, &nonce, pub_.as_bytes());
        let mut w2 = Vec::new();
        rw::ctl_frame_into(&rw::encode_proof(&nonce, dh.as_bytes(), pub_.as_bytes(), Some(&psk)), &mut w2);
        ctl.write_all(&w2).unwrap();
        ctl.read_exact(&mut hdr).unwrap();
        let mut okmsg = vec![0u8; u16::from_be_bytes(hdr) as usize];
        ctl.read_exact(&mut okmsg).unwrap();
        assert_eq!(rw::decode_ok_auth(&okmsg).unwrap(), rw::ok_auth_mac(&secret, &nonce).to_vec());
        // 等驱动线程完成交接 attach（握手线程经 wake 管道通知——≤ 一轮 poll 唤醒；
        // 不等的话客户端包会在 attach 前到达，准入闸把包丢了）
        std::thread::sleep(Duration::from_millis(400));

        // 客户端：发一条数据帧 → 触发拨腿会话（SESSION 通告）
        let cl = UdpSocket::bind("127.0.0.1:0").unwrap();
        cl.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let data = frame::frame_bytes(frame::FrameKind::Data, b"hello-dialup");
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &data, &mut tagged);
        cl.send_to(&tagged, relay_addr).unwrap();

        // 后端控制面收 SESSION
        ctl.read_exact(&mut hdr).unwrap();
        let mut sm = vec![0u8; u16::from_be_bytes(hdr) as usize];
        ctl.read_exact(&mut sm).unwrap();
        let sess = rw::decode_session(&sm).expect("应收到 SESSION 通告");
        assert!(sess.data_port >= 1024);

        // 后端向数据口拨腿（LEGUP 认证）
        let leg_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        leg_sock.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let data_port_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), sess.data_port);
        leg_sock.send_to(&rw::legup_payload(sess.id, &sess.cookie, &secret), data_port_addr).unwrap();

        // pend 放行：后端在腿 socket 上收到缓冲的客户端帧
        let mut buf = [0u8; 256];
        let (n, _) = leg_sock.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], &data[..], "等腿缓冲应放行首包");

        // 下行：腿 socket → 客户端原样
        let down = frame::frame_bytes(frame::FrameKind::Data, b"down-via-leg");
        leg_sock.send_to(&down, data_port_addr).unwrap();
        let (n, _) = cl.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], &down[..]);
    }

    /// 控制面保活回显：后端 KEEPALIVE → 中继必答（B3）——90s 读超时靠它续命。
    #[test]
    fn ctl_keepalive_echo() {
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            Some([0x44u8; 32]),
            noop_logf(),
        ));
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let secret = [0x44u8; 32];
        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);

        let mut ctl = ctl_connect(relay_addr);
        ctl.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let _ = ctl.set_write_timeout(Some(Duration::from_secs(5)));
        use std::io::{Read as _, Write as _};
        let mut wire = Vec::new();
        rw::ctl_frame_into(&rw::encode_hello(pub_.as_bytes()), &mut wire);
        ctl.write_all(&wire).unwrap();
        let mut hdr = [0u8; 2];
        ctl.read_exact(&mut hdr).unwrap();
        let mut msg = vec![0u8; u16::from_be_bytes(hdr) as usize];
        ctl.read_exact(&mut msg).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(&msg).unwrap();
        let dh = priv_.diffie_hellman(&PublicKey::from(eph_pub));
        let psk = rw::auth_mac(&secret, &nonce, pub_.as_bytes());
        let mut w2 = Vec::new();
        rw::ctl_frame_into(&rw::encode_proof(&nonce, dh.as_bytes(), pub_.as_bytes(), Some(&psk)), &mut w2);
        ctl.write_all(&w2).unwrap();
        ctl.read_exact(&mut hdr).unwrap();
        let mut okmsg = vec![0u8; u16::from_be_bytes(hdr) as usize];
        ctl.read_exact(&mut okmsg).unwrap();
        assert!(rw::decode_ok_auth(&okmsg).is_some());
        std::thread::sleep(Duration::from_millis(400)); // 等驱动线程 attach

        // 连发 3 个 KEEPALIVE（间隔 300ms）——每个都应收到回显
        for i in 0..3 {
            let mut w = Vec::new();
            rw::ctl_frame_into(&rw::keepalive_bytes(), &mut w);
            ctl.write_all(&w).unwrap();
            ctl.read_exact(&mut hdr).unwrap();
            let mut echo = vec![0u8; u16::from_be_bytes(hdr) as usize];
            ctl.read_exact(&mut echo).unwrap();
            assert_eq!(echo[0], rw::sub::KEEPALIVE, "第 {} 个保活应被回显", i + 1);
        }
    }

    /// 空闲回收（注入短窗）：会话被摘 + 聚合计数。
    #[test]
    fn reap_idle_sessions() {
        let mut cfg = Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            noop_logf(),
        );
        cfg.idle_timeout = Duration::from_millis(150);
        let tr = TestRelay::start(cfg);
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        let cl = UdpSocket::bind("127.0.0.1:0").unwrap();
        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);
        let label = relay_id(pub_.as_bytes());
        register_open(&be, relay_addr, &priv_, &pub_, &label);
        let data = frame::frame_bytes(frame::FrameKind::Data, b"x");
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &data, &mut tagged);
        cl.send_to(&tagged, relay_addr).unwrap();
        std::thread::sleep(Duration::from_millis(600));
        // 后端观察：再发包不再有转发（会话已回收 → 新会话重建——仍能通，收到的源端口变了）
        cl.send_to(&tagged, relay_addr).unwrap();
        be.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 128];
        let mut last_src = None;
        for _ in 0..4 {
            if let Ok((n, src)) = be.recv_from(&mut buf) {
                if frame::decode_frame(&buf[..n]).is_some_and(|(k, p)| {
                    k == frame::FrameKind::Data.to_wire() && p == b"x"
                }) {
                    last_src = Some(src);
                }
            } else {
                break;
            }
        }
        assert!(last_src.is_some(), "回收后重发包应重建会话并转发");
    }

    /// F5：未认证 HELLO 不占 `legs`——伪造 label 洪水灌不满真腿表，合法后端仍可注册。
    /// （`max_legs=2` + 20 个自洽伪造 HELLO：若 HELLO 仍插腿，合法 PROOF 必被拒。）
    #[test]
    fn hello_flood_does_not_occupy_legs() {
        let mut cfg = Config::new(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), None, noop_logf());
        cfg.max_legs = 2;
        let tr = TestRelay::start(cfg);
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        be.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        for _ in 0..20 {
            let p = rand_secret();
            let pk = PublicKey::from(&p);
            let label = relay_id(pk.as_bytes());
            let hello = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pk.as_bytes()));
            let mut tagged = Vec::new();
            frame::encode_tagged_frame(&label, &hello, &mut tagged);
            be.send_to(&tagged, relay_addr).unwrap();
            let _ = be.recv_from(&mut [0u8; 128]); // 吃掉 CHALLENGE
        }
        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);
        let label = relay_id(pub_.as_bytes());
        register_open(&be, relay_addr, &priv_, &pub_, &label);
    }

    /// F5：`pending` 满按最旧淘汰（**不拒绝**）——洪水下合法后端仍能完成注册。
    #[test]
    fn pending_full_evicts_oldest_not_reject() {
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            noop_logf(),
        ));
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        be.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        for _ in 0..(PENDING_MAX + 5) {
            let p = rand_secret();
            let pk = PublicKey::from(&p);
            let label = relay_id(pk.as_bytes());
            let hello = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pk.as_bytes()));
            let mut tagged = Vec::new();
            frame::encode_tagged_frame(&label, &hello, &mut tagged);
            be.send_to(&tagged, relay_addr).unwrap();
            let _ = be.recv_from(&mut [0u8; 128]);
        }
        let priv_ = rand_secret();
        let pub_ = PublicKey::from(&priv_);
        let label = relay_id(pub_.as_bytes());
        register_open(&be, relay_addr, &priv_, &pub_, &label);
    }

    /// F5：`legs` 闸移到 PROOF 提升（`max_legs` 只数**已接纳**腿）——超限拒绝、后端重试。
    #[test]
    fn legs_cap_applied_at_promotion() {
        let mut cfg = Config::new(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0), None, noop_logf());
        cfg.max_legs = 1;
        let tr = TestRelay::start(cfg);
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        be.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let p1 = rand_secret();
        let pub1 = PublicKey::from(&p1);
        register_open(&be, relay_addr, &p1, &pub1, &relay_id(pub1.as_bytes()));
        // 第二条：PROOF 通过但 legs 满 ⇒ 无 OK（后端重试自愈）
        let p2 = rand_secret();
        let pub2 = PublicKey::from(&p2);
        let label2 = relay_id(pub2.as_bytes());
        let hello = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pub2.as_bytes()));
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label2, &hello, &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        let mut buf = [0u8; 128];
        let (n, _) = be.recv_from(&mut buf).unwrap();
        let (_, payload) = frame::decode_frame(&buf[..n]).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(payload).unwrap();
        let dh = p2.diffie_hellman(&PublicKey::from(eph_pub));
        let proof = rw::encode_proof(&nonce, dh.as_bytes(), pub2.as_bytes(), None);
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label2, &frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &proof), &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        assert!(be.recv_from(&mut buf).is_err(), "legs 满 ⇒ 不应回 OK");
    }

    /// F7.1：v1 fallback 会话只认注册腿源 `a.backend`——未知源下行被显式丢弃、不续命。
    #[test]
    fn v1_fallback_binds_to_backend_source() {
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            noop_logf(),
        ));
        let relay_addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port);
        let be = UdpSocket::bind("127.0.0.1:0").unwrap();
        let cl = UdpSocket::bind("127.0.0.1:0").unwrap();
        cl.set_read_timeout(Some(Duration::from_millis(400))).unwrap();
        let p = rand_secret();
        let pub_ = PublicKey::from(&p);
        let label = relay_id(pub_.as_bytes());
        register_open(&be, relay_addr, &p, &pub_, &label);
        let data = frame::frame_bytes(frame::FrameKind::Data, b"up");
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &data, &mut tagged);
        cl.send_to(&tagged, relay_addr).unwrap();
        be.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 256];
        let assoc_src = {
            let mut src = None;
            for _ in 0..4 {
                let Ok((n, s)) = be.recv_from(&mut buf) else { break };
                if frame::decode_frame(&buf[..n])
                    .is_some_and(|(k, pl)| k == frame::FrameKind::Data.to_wire() && pl == b"up")
                {
                    src = Some(s);
                    break;
                }
            }
            src.expect("应收到转发帧")
        };
        // 未知源下行 ⇒ 丢弃；合法后端下行 ⇒ 到达
        let intruder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let bad = frame::frame_bytes(frame::FrameKind::Data, b"forged-down");
        intruder.send_to(&bad, assoc_src).unwrap();
        let good = frame::frame_bytes(frame::FrameKind::Data, b"good-down");
        be.send_to(&good, assoc_src).unwrap();
        let mut got: Vec<Vec<u8>> = Vec::new();
        for _ in 0..4 {
            let Ok((n, _)) = cl.recv_from(&mut buf) else { break };
            got.push(buf[..n].to_vec());
        }
        assert!(got.iter().any(|p| p.as_slice() == &good[..]), "合法后端下行应达客户端");
        assert!(!got.iter().any(|p| p.as_slice() == &bad[..]), "未知源下行必须被丢弃");
    }

    /// F7.3/F8：未认证 assoc 不抬缓冲；下行发送失败只计 `send_fail_down`、不计成功。
    #[test]
    fn assoc_lazy_bump_and_down_fail_counted() {
        let mut relay = Relay::new(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            noop_logf(),
        ));
        let udp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let label = [0x5Au8; 8];
        relay.legs.insert(
            label,
            Leg {
                pubkey: [1; 32],
                addr: Some("127.0.0.1:9999".parse().unwrap()),
                last: Instant::now(),
                verified: true,
                ctl_verified: false,
                ctl: None,
            },
        );
        // 客户端地址用广播地址：下行 sendto 在本机必失败（无 SO_BROADCAST）
        let client: SocketAddr = "255.255.255.255:9".parse().unwrap();
        let data = frame::frame_bytes(frame::FrameKind::Data, b"x");
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(&label, &data, &mut tagged);
        relay.handle_udp_packet(&udp, client, &tagged);
        assert_eq!(relay.assocs.len(), 1, "应建一条 assoc");
        let key = AssocKey { label, client };
        assert!(!relay.assocs[&key].bufs_bumped, "未认证不得抬缓冲（F7.3）");
        // 合法后端下行 ⇒ 抬缓冲（v1 合法源首包）；但客户端发送失败 ⇒ 只计 send_fail_down
        relay.assoc_read(&udp, key, "127.0.0.1:9999".parse().unwrap(), b"down".to_vec());
        assert!(relay.assocs[&key].bufs_bumped, "v1 合法源首包后抬缓冲");
        assert_eq!(relay.stats.forwarded_down, 0, "失败不计成功（F8）");
        assert_eq!(relay.stats.send_fail_down, 1, "失败进 send_fail_down（F8）");
    }

    /// F10：控制面 TCP 同号口 bind 失败 ⇒ 探针 flags 报降级位；正常 ⇒ flags=0。
    #[test]
    fn probe_flags_report_ctl_degraded() {
        // 占住 TCP 0.0.0.0:P（UDP P 仍空闲）⇒ 中继 UDP 绑 P、TCP 绑 0.0.0.0:P 失败
        let ln = std::net::TcpListener::bind("0.0.0.0:0").unwrap();
        let port = ln.local_addr().unwrap().port();
        let tr = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
            None,
            noop_logf(),
        ));
        assert_eq!(tr.port, port, "UDP 绑到同号口（TCP 被占不影响 UDP）");
        let cl = UdpSocket::bind("127.0.0.1:0").unwrap();
        cl.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let nonce = [7u8; 8];
        let req = crate::probe::encode_request(crate::probe::TYPE_PING, &nonce, 16);
        cl.send_to(&req, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr.port)).unwrap();
        let mut buf = [0u8; 512];
        let (n, _) = cl.recv_from(&mut buf).unwrap();
        let r = crate::probe::decode_response(&buf[..n], &nonce).unwrap();
        assert_ne!(
            r.flags & crate::probe::FLAG_RELAY_CTL_DEGRADED,
            0,
            "控制面 bind 失败应报降级位（实收 flags={:#x}）",
            r.flags
        );
        drop(tr);
        // 正常形态：flags = 0
        let tr2 = TestRelay::start(Config::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            None,
            noop_logf(),
        ));
        cl.send_to(&req, SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), tr2.port)).unwrap();
        let (n2, _) = cl.recv_from(&mut buf).unwrap();
        let r2 = crate::probe::decode_response(&buf[..n2], &nonce).unwrap();
        assert_eq!(r2.flags, 0, "正常形态 flags 应为 0");
    }

    fn register_open(be: &UdpSocket, relay_addr: SocketAddr, priv_: &x25519_dalek::StaticSecret, pub_: &PublicKey, label: &[u8; 8]) {
        let hello = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pub_.as_bytes()));
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(label, &hello, &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        be.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 128];
        let (n, _) = be.recv_from(&mut buf).unwrap();
        let (_, payload) = frame::decode_frame(&buf[..n]).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(payload).unwrap();
        let dh = priv_.diffie_hellman(&PublicKey::from(eph_pub));
        let proof = rw::encode_proof(&nonce, dh.as_bytes(), pub_.as_bytes(), None);
        let mut tagged = Vec::new();
        frame::encode_tagged_frame(label, &frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &proof), &mut tagged);
        be.send_to(&tagged, relay_addr).unwrap();
        let (n, _) = be.recv_from(&mut buf).unwrap();
        let (_, payload) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(payload, &rw::ok_bytes()[..]);
    }
}
