//! 出口 ServerBind（R3；语义真源 `baseline:pkg/servercore/bind.go` 的收发半边）。
//!
//! 与客户端 `wtransport::bind` 同名不同物（一个 = 出口腿帧收发面，对应 Go
//! `servercore.ServerBind`；一个 = 客户端候选赛跑面，对应 `wtransport.Bind`）——
//! 全路径 `server::bind` / `wtransport::bind` 区分，对称结构是有意为之（R3 评审 L11）。
//!
//! 收包按首字节判别（FIX-91 统一线格式——**所有腿都是腿帧**，含容器）：
//!
//! ```text
//! [0xBB]…  腿帧：0=数据入 device / 1=hint 回调 / 2=reg / 4=容器（reg 先登记后
//!          投递，只取首条 data——Go handleBatch 同序）/ 未知=忽略（type≥3 容忍）
//! 其余      非腿包：STUN 应答（事务 ID 匹配才消耗）/ 参照点探测（明文应答）；
//!          其余丢弃计数 + 新源日志
//! ```
//!
//! **入站新源日志**（E23 判据行，排障面）：每个新来源只记一行首包（含包形态），
//! 正常流量零噪音；手机换 NAT 映射后的第一发直连握手必落一行。容量 4096 满则
//! 清表重记（Go srcSeen 同义——排障去重表，非 correctness 状态）。
//!
//! **腿表**（R4；Go servercore relayLeg——relay-backend-dial 的出口侧）：控制面
//! SESSION 通告 → `register_leg`（连接 UDP socket 拨腿 + 发 LEGUP 认证标记）；
//! `send_wire` 的 endpoint 命中腿表走该腿 socket（回程五元组与拨出映射一致，
//! 严格 NAT 构造性穿透）；#17：腿已摘但 endpoint 命中「曾当过腿」的地址 → 丢弃
//! （防打到中继主口/被复用的数据口）。腿 fd 由驱动线程 poll，读侧与主 socket
//! 同一条 process_packet 解析。

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::os::fd::{AsRawFd as _, IntoRawFd as _};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::udpbatch::{bind_dual_stack, bind_v6_only, is_dual_stack, unmap_v4_in6};
use crate::wtransport::frame::{self, FrameKind};

use super::device::InboundOut;
use super::txring::{self, Slot};

/// 新源记录表容量上限（Go srcSeenMax）。
const SRC_SEEN_MAX: usize = 4096;

// ---- 腿表常量（Go servercore/bind.go:123, 189-198 同值） ----
/// 腿数上限（只拦新 sid；重放替换不拦——review C3）。
const RELAY_LEG_MAX: usize = 64;
/// 腿空闲回收阈值（对齐中继 IdleTimeout 90s 的 3 倍容错）。
const RELAY_LEG_IDLE: Duration = Duration::from_secs(3 * 60);
/// 回收扫描节拍（engine 驱动线程周期调 sweep_legs）。
pub const RELAY_LEG_SWEEP_PUB: Duration = Duration::from_secs(30);
/// 「最近摘除的腿地址」保留窗（窗口内对该地址的发送 = 已摘、不回落主 socket）。
const LEG_RECENT_TTL: Duration = Duration::from_secs(5 * 60);
/// 「曾当过腿的远端地址」表容量上限（满则清表重记——排障级记忆）。
const LEG_PORTS_MAX: usize = 4096;

/// 出站收口产物：一个要发上网络的 WG 包（含目标端点）。
pub struct WireOut(pub Vec<(SocketAddr, Vec<u8>)>);

// ---------- P1 发送路径拆分（浅拆；设计 = docs/reviews/P1-design.md） ----------
//
// 驱动线程串行五道工序中「密文→sendto」独立成发送线程：send_wire 的判定面
//（腿派发/#17 丢弃/族适配/腿帧帧化）留驱动线程，主 socket 的密文经 SPSC ring
// 交接给发送线程批量发出（Linux sendmmsg / macOS 逐包——udpbatch::send_batch 原样
// 复用）。收益：sendto 不再占驱动线程（下行倾泻期 ACK 消费不被排后）；发送节奏
// 脱离驱动线程 poll 拍（排空循环事件驱动、单轮团块 ≤ burst）；批量化有归宿。
// v0.2.2 简洁化裁定：**发送线程是唯一发送路径**（内联直发形态与降级切换删除）——
// 发送线程 panic 后无接管，ring 满丢（丢新，TCP 尾丢语义）+ 重传兜底。

/// 主 socket 发送面统计（发送线程统一写、驱动线程 5s 读）。**「轮」口径**
///（评审 p1a-D-1）：一次排空轮（单轮 ≤ burst 字节）。
/// 窗口语义字段（batch_max/hist/depth_peak）消费即清零；累计字段差分。
pub(crate) struct TxStats {
    pub bytes: AtomicU64,
    pub calls: AtomicU64,
    pub pkgs: AtomicU64,
    /// send_batch 短返余量/失败批丢弃。
    pub drops: AtomicU64,
    /// ring 满丢（驱动线程写——丢新，TCP 尾丢语义）。
    pub ring_drops: AtomicU64,
    /// 发送线程唤醒数。
    pub wakeups: AtomicU64,
    /// 发送线程排空耗时累计（ns）。
    pub drain_ns: AtomicU64,
    pub batch_max: AtomicU64,
    pub depth_peak: AtomicU64,
    pub hist: [AtomicU64; 13],
}

impl TxStats {
    fn new() -> Self {
        Self {
            bytes: AtomicU64::new(0),
            calls: AtomicU64::new(0),
            pkgs: AtomicU64::new(0),
            drops: AtomicU64::new(0),
            ring_drops: AtomicU64::new(0),
            wakeups: AtomicU64::new(0),
            drain_ns: AtomicU64::new(0),
            batch_max: AtomicU64::new(0),
            depth_peak: AtomicU64::new(0),
            hist: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

/// 队列模式交接面（ServerBind 持有；发送线程只经原子/裸 fd 交互——fd 生命周期
/// 锚定 ServerBind，发送线程不持所有权 ⇒ panic 展开不 close 任何 fd，唤醒 send
/// 永不面对 EPIPE〔评审 p1a-G-1：socketpair + MSG_NOSIGNAL，本仓 SIGPIPE 已恢复
/// 默认处置〕）。
struct QueuedFace {
    producer: txring::TxProducer,
    /// 唤醒合并位：producer 侧 `push → !swap(true) 才 send`；consumer 侧
    /// `store(false) → 读干 → 取队`——次序颠倒会丢唤醒睡死（评审 p1a-G-2）。
    pending: Arc<AtomicBool>,
    /// socketpair 写端（fd 属 ServerBind；-1 = 未建/已关）。
    wake_w: i32,
    /// 收工位（驱动线程置位 + 写唤醒——恒「先 store 再 send」；发送线程每轮读 +
    /// 空转路径读干后的电平复查〔r1-①〕）。
    stop: Arc<AtomicBool>,
}

/// 入站消费结果（recv_packet 的返回契约——驱动循环 drain 语义同客户端 bind）。
/// **reg 先于 data 应用**：容器帧 `[reg][init]`（Go 客户端首包形态）里 reg 登记设备表
/// 后 init 才进 device——字段序即应用序（Go handleBatch 同序；评审 H1）。
pub struct Inbound {
    /// 要进 device 的 WG 包（容器帧取首条 data）。
    pub data: Option<(SocketAddr, Vec<u8>)>,
    /// reg 帧载荷（驱动线程先于 data 应用到设备表）。
    pub regs: Vec<(Vec<u8>, SocketAddr)>,
}

/// hint 帧回调（装配层接；Go OnHint 语义——src 校验归接线方：hint 源 IP = 中继 IP）。
pub type OnHint = Box<dyn FnMut(&str, SocketAddr) + Send>;

/// 中继控制帧（type=3）回调（R4；Go OnLegFrame——转发给 relay-leg 线程，解析不进驱动线程）。
pub type OnLegFrame = Box<dyn FnMut(&[u8], SocketAddr) + Send>;

/// 一条到中继数据口的连接 UDP 腿（relay-backend-dial 的出口侧）。
struct RelayLeg {
    id: u64,
    remote: SocketAddr,
    sock: UdpSocket,
    /// 最近活动（收发双向刷——纯下行活跃的腿不误回收）。
    last: Instant,
}

/// 出口腿帧收发面（驱动线程独占）。
pub struct ServerBind {
    sock: UdpSocket,
    /// socket 是 AF_INET6 **双栈**（V6ONLY=0）：v4 目标发送前须 map 成 v4-mapped、
    /// v4 包的源地址（v4-mapped 形态）须 unmap 回纯 v4——内部表示恒纯 v4/v6
    /// （Go `net.ListenUDP("udp", nil)` 双栈 + 内部 AsSlice/unmap 同义）。
    dual: bool,
    /// socket 当前钉住的网卡（钉卡事实——IP_BOUND_IF/SO_BINDTODEVICE 成功即真；
    /// 公网端点公布的 pinned 判据读这里，Go `PinnedIface()` 同义）。None = 未钉。
    pub pinned: Option<(u32, String)>,
    build: String,
    /// 探测应答的能力位（udpcap 周期结论；3e 接——现在恒 0）。
    caps: u8,
    /// 探测应答端点列表段来源（公网端点公布面；3f 接——现在恒空）。
    probe_endpoints: Vec<SocketAddr>,
    logf: crate::Logf,
    /// 细节日志面（#17 丢弃行——Go logfD 对应）。
    dlogf: crate::Logf,
    on_hint: Option<OnHint>,
    on_leg_frame: Option<OnLegFrame>,
    src_seen: HashMap<SocketAddr, ()>,
    // ---- 腿表（驱动线程独占；R4） ----
    leg_by_id: HashMap<u64, RelayLeg>,
    leg_by_r: HashMap<SocketAddr, u64>,
    /// 最近摘除的腿远端（TTL 内 Send 判「不回落主 socket」）。
    leg_recent: HashMap<SocketAddr, Instant>,
    /// 曾当过一次腿的远端地址（Send 兜底丢弃判据；不受 5min 窗限制——#17）。
    leg_ports: HashSet<SocketAddr>,
    leg_dropped: u64,
    /// STUN 观测等待者（事务 ID + 应答回执通道——同 socket 观测「监听端口的 NAT 映射」）。
    stun_wait_txid: Option<[u8; 12]>,
    stun_wait_result: Option<std::sync::mpsc::Sender<Option<SocketAddr>>>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    recv_buf: Box<[u8; 65536]>,
    /// 发送面耗时（P1b-0 剂量插桩）：send_wire 的 wall-time 累计（ns，空批早退
    /// 不计——那不是发送成本）。驱动线程被发送面占住的 ms/s 的直接观测面：
    /// 判定+入队侧耗时（发送线程排空耗时另列 `TxStats.drain_ns`）。
    pub tx_wall_ns: u64,
    // ---- P1 发送路径拆分（见模块头「P1 发送路径拆分」注释块） ----
    /// 发送线程交接面。None = tx_start 前的装配窗口（此间 send_wire 不可达——
    /// 装配不变量，None 分支 unreachable）；tx_shutdown 收工后回 None（幂等）。
    tx_queued: Option<Box<QueuedFace>>,
    /// 发送线程句柄（驱动线程收工时 join——无超时，必返论证见 tx_shutdown）。
    tx_thread: Option<std::thread::JoinHandle<()>>,
    /// socketpair 读端（fd 生命周期锚 ServerBind；-1 = 未建/已关）。
    wake_r: i32,
    /// 主 socket 发送面统计（发送线程写；驱动线程 5s 行读快照。R8-2/R8-3 时代的
    /// ServerBind 侧窗口统计〔calls/pkgs/batch_max/hist/dropped〕P1 起全部迁此）。
    tx_stats: Arc<TxStats>,
}

impl ServerBind {
    /// 直方图桶数（1,2,4,…,4096 共 13 档；>4096 并入末桶——修复后不应出现）。
    const TX_HIST_BUCKETS: usize = 13;
    /// 监听固定端口（被占退让 +1…+9 → 随机；Go listenWithFallback 同序）。
    /// 返回 Err = 全失败（出口起不来——占端口硬失败，R0.6 评审 M13 同口径）。
    pub fn open(port: u16, build: &str, logf: crate::Logf) -> io::Result<Self> {
        Self::open_bound(port, build, None, None, logf)
    }

    /// 绑定形态：`bind_ip`（**IP 字面量单栈绑**（Go `BindAddr`：该族的
    /// `udp4`/`udp6` 单栈 socket）；None = 双栈 `[::]`——v4/v6 客户端都能连、
    /// 同 socket 的 STUN 观测对两族都成立）+ 可选的整 socket 钉卡（index + 名——
    /// darwin IP_BOUND_IF/IPV6_BOUND_IF 两族 / linux SO_BINDTODEVICE；**绑卡不绑
    /// 地址**，Go `BindIface` 语义：钉卡保持双栈，v6 直连路径才不被 v4 源地址绑死）。
    /// 钉卡失败不致命（Go 同义告警 + 保守公布）。
    pub fn open_bound(
        port: u16,
        build: &str,
        bind_ip: Option<IpAddr>,
        pin: Option<(u32, String)>,
        logf: crate::Logf,
    ) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;
        let sock = listen_with_fallback_addr(port, bind_ip)?;
        // 运行期事实判据：AF_INET6 且 V6ONLY=0 才是双栈（v6 字面量单栈 socket 上
        // 不能对 v4 目标做 mapped map——Go udp6 socket 发 v4 报错同义）
        let dual = is_dual_stack(&sock);
        let mut pinned = None;
        if let Some((index, name)) = pin {
            match super::egress::pin_socket_to_iface(sock.as_raw_fd(), index, &name) {
                Ok(()) => pinned = Some((index, name)),
                Err(e) => {
                    // Go bind.go:546 同串：钉不上卡不致命，公网端点公布自动变保守
                    (logf)(&format!(
                        "⚠️ 钉网卡 {name} 失败（{e}）—— 继续以未绑卡运行：STUN 观测可能被 TUN 型代理污染，公网端点公布会因此变保守"
                    ));
                }
            }
        }
        // 大收发缓冲：拦截栈每拍可产 ~1MB 突发（MTU 1280 × 数百段），内核默认
        // SO_SNDBUF/SO_RCVBUF（~128-9216B）会整包丢弃 WG 数据报 ⇒ TCP 层 RTO
        // 重传、吞吐塌到 ~8MB/s【2026-10-02 实测抓出】。尽力而为抬高（超过系统
        // 上限的值由内核自动钳制/报错忽略——macOS kern.ipc.maxsockbuf 缺省 4MB）。
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
        if sock.local_addr()?.port() != port {
            (logf)(&format!(
                "⚠️ 监听端口 {port} 被占用 —— 改用 {}；token 里的端口以公布/签发为准",
                sock.local_addr()?.port()
            ));
        }
        sock.set_nonblocking(true)?;
        Ok(Self {
            sock,
            dual,
            pinned,
            build: build.to_string(),
            caps: 0,
            probe_endpoints: Vec::new(),
            logf: Arc::clone(&logf),
            dlogf: logf,
            on_hint: None,
            on_leg_frame: None,
            src_seen: HashMap::new(),
            leg_by_id: HashMap::new(),
            leg_by_r: HashMap::new(),
            leg_recent: HashMap::new(),
            leg_ports: HashSet::new(),
            leg_dropped: 0,
            stun_wait_txid: None,
            stun_wait_result: None,
            rx_bytes: 0,
            tx_bytes: 0,
            recv_buf: Box::new([0u8; 65536]),
            tx_wall_ns: 0,
            tx_queued: None,
            tx_thread: None,
            wake_r: -1,
            tx_stats: Arc::new(TxStats::new()),
        })
    }

    pub fn local_port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub fn set_on_hint(&mut self, f: OnHint) {
        self.on_hint = Some(f);
    }

    /// 中继控制帧（type=3）钩子（R4：转发给 relay-leg 线程；未装 = 照旧忽略）。
    pub fn set_on_leg_frame(&mut self, f: OnLegFrame) {
        self.on_leg_frame = Some(f);
    }

    /// WG socket 的 fd 副本（`try_clone`——注册腿与数据面**同本地端口**的硬约束
    /// D4#1；relay-leg 线程经它发 Hello/Keepalive/Proof/盲打。装配窗口 = 本对象
    /// move 进驱动线程前——评审 ④-2）。
    pub fn try_clone_socket(&self) -> io::Result<UdpSocket> {
        self.sock.try_clone()
    }

    /// 3e/3f 的接线面。
    pub fn set_caps(&mut self, caps: u8) {
        self.caps = caps;
    }

    pub fn set_probe_endpoints(&mut self, eps: Vec<SocketAddr>) {
        self.probe_endpoints = eps;
    }

    /// 收一个数据报（非阻塞）。返回契约：`Ok(Some(Inbound))` = 有 reg 待应用或数据帧
    /// 待进 device；`Ok(None)` = 本包被内部消费（hint/probe/STUN/畸形），继续收；
    /// `Err(WouldBlock|TimedOut)` = 本轮无包；其它 Err = 读错误（驱动循环限流记一行
    /// 原地慢转——绝不退出，防永久失聪）。
    pub fn recv_packet(&mut self) -> io::Result<Option<Inbound>> {
        let (n, src) = match self.sock.recv_from(&mut self.recv_buf[..]) {
            Ok(v) => v,
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                return Err(e)
            }
            Err(e) => return Err(e),
        };
        self.rx_bytes += n as u64;
        // 双栈 socket 上 v4 包的源地址是 v4-mapped v6（::ffff:x.y.z.w）——归一成
        // 纯 v4（Go readOnce 的 `Is4In6 → Unmap` 同义）：内部表示（新源表/endpoint/
        // 腿表）恒纯 v4/v6，发送面再按 socket 族 map 回去。
        let src = unmap_v4_in6(src);
        // 拷出本包再解析（recv_buf 与 &mut self 的借用分离；长度按 n——热路径一次
        // 分配，R5 性能批再消）
        let pkt = self.recv_buf[..n].to_vec();
        Ok(self.process_packet(&pkt, src))
    }

    /// 一个入站 UDP 包的完整解析（Go processPacket 同序：STUN → probe → 腿帧 → 丢弃）。
    fn process_packet(&mut self, buf: &[u8], src: SocketAddr) -> Option<Inbound> {
        // STUN 应答：只认事务 ID 匹配的应答（观测等待者消费——映射地址回执后清位；
        // 不匹配的照常走后续判定，但 STUN 应答不是腿帧，最终走非帧丢弃）。
        if stun_looks_like_response(buf) {
            if let Some(txid) = self.stun_wait_txid {
                if buf.get(8..20) == Some(&txid[..]) {
                    self.note_new_src(src, "STUN应答", buf.len());
                    let mapped = super::egress::parse_stun_response(buf).map(|(_, ap)| ap);
                    if let Some(tx) = self.stun_wait_result.take() {
                        let _ = tx.send(mapped);
                    }
                    self.stun_wait_txid = None;
                    return None; // 已消耗
                }
            }
        }

        // 参照点探测：明文一问一答，不进 WG、不登记 peer。
        if let Some(resp) =
            crate::probe::respond_ex(buf, &self.build, self.caps, &self.probe_endpoints)
        {
            self.note_new_src(src, "参照点探测", buf.len());
            let _ = self.sock.send_to(&resp, self.xmit_addr(src));
            return None;
        }

        if buf.first() == Some(&frame::FRAME_MAGIC) {
            let Some((kind, payload)) = frame::decode_frame(buf) else {
                self.note_new_src(src, "畸形腿帧", buf.len());
                return None; // 畸形腿帧：丢弃不中断
            };
            match kind {
                k if k == FrameKind::Data.to_wire() => {
                    self.note_new_src(src, "腿帧数据", buf.len());
                    return Some(Inbound {
                        data: Some((src, payload.to_vec())),
                        regs: Vec::new(),
                    });
                }
                k if k == FrameKind::Reg.to_wire() => {
                    self.note_new_src(src, "腿帧注册", buf.len());
                    return Some(Inbound {
                        data: None,
                        regs: vec![(payload.to_vec(), src)],
                    });
                }
                k if k == FrameKind::Control.to_wire() => {
                    self.note_new_src(src, "腿帧控制", buf.len());
                    if let (Some(f), Some(addr)) =
                        (&mut self.on_hint, frame::decode_hint_payload(payload))
                    {
                        f(addr, src);
                    }
                    return None;
                }
                k if k == FrameKind::Batch.to_wire() => {
                    return self.handle_batch(buf, payload, src)
                }
                k if k == crate::relaywire::FRAME_TYPE_RELAY_REG => {
                    // 中继控制帧：转发给 relay-leg 线程（解析/源校验不进驱动线程）
                    self.note_new_src(src, "腿帧type=3", buf.len());
                    if let Some(f) = &mut self.on_leg_frame {
                        f(payload, src);
                    }
                    return None;
                }
                _ => {
                    // 其它未知 type：容忍忽略（前向兼容）
                    self.note_new_src(src, &format!("腿帧type={kind}"), buf.len());
                    return None;
                }
            }
        }

        // FIX-91 统一线格式：非帧包不是腿（旧客户端裸 WG / 垃圾）——丢弃计数。
        let shape = match buf.first() {
            Some(b) => format!("非帧（{}，首字节=0x{b:02x}）", wg_msg_name(*b)),
            None => "非帧包".to_string(),
        };
        self.note_new_src(src, &shape, buf.len());
        None
    }

    /// 容器帧（Go handleBatch 同序）：reg 收集（**先于 data 应用**）→ data 只取首条投
    /// device → control 交 hint；未知消息类型忽略；无 data 且无 reg = 内部消费。
    fn handle_batch(&mut self, raw: &[u8], payload: &[u8], src: SocketAddr) -> Option<Inbound> {
        let Some(msgs) = frame::decode_batch(payload) else {
            self.note_new_src(src, "畸形容器", raw.len());
            return None;
        };
        let mut data: Option<&[u8]> = None;
        let mut regs: Vec<(Vec<u8>, SocketAddr)> = Vec::new();
        for (kind, mpayload) in msgs {
            match kind {
                k if k == FrameKind::Reg.to_wire() => {
                    regs.push((mpayload.to_vec(), src));
                }
                k if k == FrameKind::Data.to_wire() => {
                    if data.is_none() {
                        data = Some(mpayload); // 多条时取首条（UDP 单数据报只投一次）
                    }
                }
                k if k == FrameKind::Control.to_wire() => {
                    if let (Some(f), Some(addr)) =
                        (&mut self.on_hint, frame::decode_hint_payload(mpayload))
                    {
                        f(addr, src);
                    }
                }
                _ => {} // 未知消息类型：忽略（前向兼容）
            }
        }
        let d = data.map(|d| (src, d.to_vec()));
        if d.is_none() && regs.is_empty() {
            self.note_new_src(src, "容器（无数据）", raw.len());
            return None;
        }
        self.note_new_src(src, "容器数据", raw.len());
        Some(Inbound { data: d, regs })
    }

    // ---------- 腿表（relay-backend-dial；驱动线程独占） ----------

    /// 向中继数据口拨一条腿：连接 socket + 发认证标记（v2 = LEGUP‖cookie‖MAC）+
    /// 入双表。同 id 或同远端重复注册 = 先拆旧再建（中继侧会话重建的语义）。
    /// 上限只拦新 id（C3：重放替换不拦）。
    pub fn register_leg(
        &mut self,
        id: u64,
        remote: SocketAddr,
        marker: &[u8],
    ) -> Result<(), super::relayleg::LegError> {
        // 腿 socket 按远端族建（中继 v6 地址形态；connected socket 族必须匹配）
        let sock = match remote {
            SocketAddr::V4(_) => UdpSocket::bind("0.0.0.0:0")?,
            SocketAddr::V6(_) => UdpSocket::bind("[::]:0")?,
        };
        sock.connect(remote)?;
        sock.set_nonblocking(true).ok();
        sock.send(marker)?;
        if !self.leg_by_id.contains_key(&id) && self.leg_by_id.len() >= RELAY_LEG_MAX {
            return Err(crate::server::relayleg::LegError::LegCap(RELAY_LEG_MAX));
        }
        if let Some(old) = self.leg_by_id.get(&id) {
            let old_remote = old.remote;
            self.remove_leg_by(&id, &old_remote);
        }
        if let Some(old_id) = self.leg_by_r.get(&remote).copied() {
            self.remove_leg_by(&old_id, &remote);
        }
        if self.leg_ports.len() >= LEG_PORTS_MAX {
            self.leg_ports.clear(); // 满表清空（排障级记忆，丢了只影响归因）
        }
        self.leg_ports.insert(remote); // 记住「这个端口当过腿」（Send 的兜底丢弃判据）
        self.leg_recent.remove(&remote); // 同地址重拨成功：撤掉「最近摘除」标记
        self.leg_by_r.insert(remote, id);
        self.leg_by_id.insert(
            id,
            RelayLeg {
                id,
                remote,
                sock,
                last: Instant::now(),
            },
        );
        Ok(())
    }

    /// 按会话号拆腿（RELEASE / 收工）。不存在 = no-op。
    pub fn remove_leg(&mut self, id: u64) {
        if let Some(lg) = self.leg_by_id.get(&id) {
            let remote = lg.remote;
            self.remove_leg_by(&id, &remote);
        }
    }

    /// 拆全部腿（控制面重连对账——中继在 OK 后会重放全量 SESSION）。
    pub fn clear_legs(&mut self) {
        let ids: Vec<u64> = self.leg_by_id.keys().copied().collect();
        for id in ids {
            self.remove_leg(id);
        }
    }

    /// 摘一条腿（双表摘除 + socket 随 drop 关闭 + 摘除地址进 leg_recent）。
    fn remove_leg_by(&mut self, id: &u64, remote: &SocketAddr) {
        if let Some(lg) = self.leg_by_id.remove(id) {
            let _ = lg; // socket 关闭
            self.leg_recent.insert(*remote, Instant::now());
        }
        if self.leg_by_r.get(remote) == Some(id) {
            self.leg_by_r.remove(remote);
        }
    }

    /// 腿 fd 可读：recv（连接 socket，源恒为腿远端）→ LEGUP 标记吞包防御 →
    /// 同一条 process_packet 解析（产出 Inbound 交驱动线程——**与主 socket 同一
    /// 消费管线**：data 进 device / reg 进设备表 / hint 回调）。
    /// 返回 (存活, Inbound)。
    pub fn leg_readable(&mut self, fd: std::os::fd::RawFd) -> (bool, Option<Inbound>) {
        // 按 fd 找腿（驱动线程 poll 回指）
        let Some((id, remote)) = self
            .leg_by_id
            .values()
            .find(|lg| lg.sock.as_raw_fd() == fd)
            .map(|lg| (lg.id, lg.remote))
        else {
            return (false, None);
        };
        let Some(lg) = self.leg_by_id.get_mut(&id) else {
            return (false, None);
        };
        let mut buf = [0u8; 65536];
        match lg.sock.recv(&mut buf) {
            Ok(0) => {
                self.leg_read_exit(id, remote);
                (false, None)
            }
            Ok(n) => {
                lg.last = Instant::now();
                let pkt = buf[..n].to_vec();
                // LEGUP 标记防御（中继侧已吞，正常到不了这里）
                if pkt == b"LEGUP" || crate::relaywire::legup_cookie(&pkt).is_some() {
                    return (true, None);
                }
                let inbound = self.process_packet(&pkt, remote);
                (true, inbound)
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => (true, None),
            Err(_) => {
                // 读错误（ICMP 拒绝等）= 腿死亡：摘除（Go 读循环退出的等价物）
                self.leg_read_exit(id, remote);
                (false, None)
            }
        }
    }

    fn leg_read_exit(&mut self, id: u64, remote: SocketAddr) {
        self.remove_leg_by(&id, &remote);
        (self.logf)(&format!("腿（会话 #{id} → {remote}）读循环退出，摘除"));
    }

    /// 腿空闲回收（驱动线程周期调；含 leg_recent 过期清理）。
    pub fn sweep_legs(&mut self) {
        let now = Instant::now();
        let dead: Vec<(u64, SocketAddr)> = self
            .leg_by_id
            .values()
            .filter(|lg| now.duration_since(lg.last) > RELAY_LEG_IDLE)
            .map(|lg| (lg.id, lg.remote))
            .collect();
        for (id, remote) in dead {
            self.remove_leg_by(&id, &remote);
            (self.logf)(&format!("腿（会话 #{id} → {remote}）空闲超 3m0s，回收"));
        }
        self.leg_recent
            .retain(|_, at| now.duration_since(*at) <= LEG_RECENT_TTL);
    }

    /// 腿表当前 fd 集（驱动线程 poll 用）。
    pub fn leg_fds(&self) -> Vec<std::os::fd::RawFd> {
        self.leg_by_id
            .values()
            .map(|lg| lg.sock.as_raw_fd())
            .collect()
    }

    /// 收工：拆全部腿（不打卡日志——收工路径）。
    pub fn shutdown_legs(&mut self) {
        let ids: Vec<u64> = self.leg_by_id.keys().copied().collect();
        for id in ids {
            if let Some(lg) = self.leg_by_id.get(&id) {
                let remote = lg.remote;
                self.remove_leg_by(&id, &remote);
            }
        }
    }

    /// STUN 观测（**在本 Bind 的 UDP socket 上**问一次——监听端口那个 socket 的 NAT
    /// 映射；从临时 socket 问出来的是默认路由的映射，两者在 TUN 代理机器上完全不同）。
    /// v4/v6 目标都可问（双栈 socket 同一条观测面——Go `STUNQuery`/`STUNQueryV6`
    /// 共用同一 socket 的同义面）。应答经 result 通道回执（None = 应答未到/不合法
    /// ——调用方按超时收场）；同一时刻只允许一次查询（观测周期都是分钟级——Go
    /// stunPending 同义）。
    pub fn stun_query(
        &mut self,
        server: SocketAddr,
        result: std::sync::mpsc::Sender<Option<SocketAddr>>,
    ) -> io::Result<()> {
        if self.stun_wait_txid.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "server: 已有一次 STUN 查询在等",
            ));
        }
        let txid = super::egress::new_txid();
        let req = super::egress::stun_request(&txid, ""); // servercore 形态：20B 无属性
        self.stun_wait_txid = Some(txid);
        self.stun_wait_result = Some(result);
        self.sock.send_to(&req, self.xmit_addr(server))?;
        Ok(())
    }

    /// 观测超时收位（调用方超时后清——防下一次查询被占位挡住）。
    pub fn stun_query_abort(&mut self) {
        self.stun_wait_txid = None;
        self.stun_wait_result = None;
    }

    /// 发送目标的族适配（`udpbatch::xmit_addr` 的 dual=self.dual 面：双栈 socket 发
    /// v4 目标须 map 成 v4-mapped；单栈 socket 原样，族不匹配错误如实上报）。
    fn xmit_addr(&self, ep: SocketAddr) -> SocketAddr {
        crate::udpbatch::xmit_addr(ep, self.dual)
    }

    /// 把当前 socket 重钉到指定网卡（绑卡看护换卡/重挑用；Go `RepinTo` 同义——两族
    /// 都设、单栈容错）。驱动线程内执行（socket 由本结构独占）。
    pub fn repin_to(&mut self, index: u32, name: &str) -> io::Result<()> {
        use std::os::fd::AsRawFd as _;
        super::egress::pin_socket_to_iface(self.sock.as_raw_fd(), index, name)
        // 注：不回写 self.pinned——该字段只在装配期被读一次（pinned_flag 的初值），
        // 运行期真源是 pinned_flag（看护重钉成功后置位；评审 r1-Z3 防双真源漂移）
    }

    /// 出站收口：把 device 产出的 wire 批封装腿帧发出（Send 恒发腿帧——Go 同义）。
    /// endpoint 命中腿表走该腿 socket（回程五元组与拨出映射一致）；命中
    /// leg_recent/leg_ports = 腿已摘（#17 丢弃——打到中继主口/被复用的数据口只会
    /// 污染别的会话）；否则主 socket（唯一路径 = 经 ring 交发送线程批量发出）。
    pub fn send_wire(&mut self, out: &InboundOut) {
        if out.wire.is_empty() {
            return; // 常态快速路径（handle_inbound 的逐包调用多无产出）
        }
        let t0 = Instant::now();
        // 借用拆分：take → 处理 → 放回（判定面需要 &mut self 做腿派发）
        let mut q = self
            .tx_queued
            .take()
            .expect("发送线程未装配（send_wire 先于 tx_start——装配序违约）");
        self.send_wire_queued(out, &mut q);
        self.tx_queued = Some(q);
        self.tx_wall_ns += t0.elapsed().as_nanos() as u64;
    }

    /// 判定面单条（两模式共用）：腿命中 → 腿 socket 直发（tx_bytes 记账）；
    /// #17 丢弃（腿已摘）。返回 true = 该条已消化（不走主 socket）。
    fn dispatch_leg(&mut self, ep: &SocketAddr, wg: &[u8], out: &InboundOut) -> bool {
        // 腿表命中（先取 fd 再发——借用分离）
        let leg_fd = self
            .leg_by_r
            .get(ep)
            .and_then(|id| self.leg_by_id.get(id))
            .map(|lg| lg.sock.as_raw_fd());
        if let Some(fd) = leg_fd {
            if let Some(lg) = self
                .leg_by_id
                .values_mut()
                .find(|lg| lg.sock.as_raw_fd() == fd)
            {
                lg.last = Instant::now(); // Send 刷 last（Go Send 同义）
                let wire = frame::frame_bytes(FrameKind::Data, wg);
                let n = unsafe {
                    // MSG_NOSIGNAL（评审 D-1 中-4：与 relay/ctlface 同款——n != len 的
                    // 错误分支依赖 EPIPE 而非进程被信号打死）。
                    libc::send(fd, wire.as_ptr().cast(), wire.len(), libc::MSG_NOSIGNAL)
                };
                if n != wire.len() as isize {
                    let remote = lg.remote;
                    let id = lg.id;
                    self.leg_read_exit(id, remote);
                } else {
                    self.tx_bytes += wg.len() as u64;
                }
                return true;
            }
        }
        let recent = self.leg_recent.contains_key(ep);
        let ever_leg = self.leg_ports.contains(ep);
        if recent || ever_leg {
            // #17：丢弃 + 节流计数（等控制面重放重建腿，或下一入站包重学 endpoint）。
            // 计数口径 = 每次 Send 派发 +1（Go bind.go:975 同义；非逐包），
            // 包数取本 endpoint 批内包数（len(bufs)），通道 = 细节日志（logfD）。
            self.leg_dropped += 1;
            let ep_pkgs = out.wire.iter().filter(|(e2, _)| e2 == ep).count();
            if self.leg_dropped <= 3 || self.leg_dropped.is_multiple_of(1000) {
                (self.dlogf)(&format!(
                    "腿已摘或非现任（{ep}）丢弃出站 {ep_pkgs} 包（等控制面重放重建腿）"
                ));
            }
            return true;
        }
        false
    }

    /// Queued 模式的 send_wire 主体（P1）：判定面 + 帧化 + 入队 + 唤醒。
    /// 满丢 = 丢新（队尾拒绝——TCP 尾丢语义，dup-ACK/RTO 恢复；**绝不丢队头/队中**
    ///——乱序禁区）。
    fn send_wire_queued(&mut self, out: &InboundOut, q: &mut QueuedFace) {
        let mut pushed = 0usize;
        let mut dropped: u64 = 0;
        let mut first_drop_ep: Option<SocketAddr> = None;
        for (ep, wg) in &out.wire {
            if self.dispatch_leg(ep, wg, out) {
                continue;
            }
            // 主 socket：帧化直写新 Vec（跨线程所有权移交的最简形态——每包一次
            // ~1.3KB 分配，39k 包/s ≈ 0.3% CPU 量级；回收环登记后续）
            let mut buf = Vec::with_capacity(wg.len() + 8);
            frame::encode_frame(FrameKind::Data, wg, &mut buf);
            if q.producer
                .push(Slot { dst: self.xmit_addr(*ep), buf })
            {
                pushed += 1;
            } else {
                dropped += 1;
                if first_drop_ep.is_none() {
                    first_drop_ep = Some(*ep);
                }
            }
        }
        if dropped > 0 {
            // 满丢节流记行（首 3 + 每 1000；含首丢端点——多 peer 归因面，p1a-A-2）
            let d = self.tx_stats.ring_drops.fetch_add(dropped, Ordering::Relaxed) + dropped;
            if d <= 3 || d.is_multiple_of(1000) {
                (self.dlogf)(&format!(
                    "发送队列满：本批丢新 {dropped} 包（累计 {d}，首丢端点 {}）——队尾丢 = TCP 尾丢语义，重传恢复",
                    first_drop_ep.map(|e| e.to_string()).unwrap_or_else(|| "?".into())
                ));
            }
        }
        if pushed > 0 && !q.pending.swap(true, Ordering::AcqRel) {
            // 唤醒（合并位：本拍已有 pending 则不写——EAGAIN〔socketpair 缓冲满〕
            // 同样无害：pending 已 true，消费侧读干后必能取到包）
            let b = [1u8];
            let rc2 = unsafe {
                libc::send(
                    q.wake_w,
                    b.as_ptr().cast(),
                    1,
                    libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
                )
            };
            if crate::envflag::tx_dbg() { eprintln!("[TXDBG] wake send rc={rc2} pushed={pushed} at={:?}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)); }
        }
    }


    // ---------- P1 发送线程面 ----------

    /// 装配面：起发送线程（浅拆；**唯一发送路径**——v0.2.2 简洁化裁定：内联直发
    /// 形态与降级切换整体删除）。socketpair/spawn 失败 = panic（与装配面
    /// `expect("spawn serve driver")` 同款纪律——发送线程起不来即出口起不来）。
    /// `sock` = try_clone 的 dup fd（发送线程独占；退出/panic 时 Drop 自关）；
    /// `burst_bytes` = 单轮排空字节上界（团块钳制——与整形器单拍上界同语义）。
    pub fn tx_start(&mut self, sock: UdpSocket, burst_bytes: usize, dlogf: crate::Logf) {
        // socketpair（fd 生命周期锚 ServerBind；SOCK_DGRAM + MSG_NOSIGNAL——本仓
        // main 已把 SIGPIPE 恢复默认处置，裸写死管道会打死进程〔评审 p1a-G-1〕；
        // F1：创建即带 CLOEXEC（linux/OHOS 原子位；darwin 建后立即补）——出口发送
        // 线程的唤醒通道不得随 exec 继承（daemon 自 exec 是真实继承面）。
        let (fds0, fds1) = match crate::sysfd::socketpair_cloexec(libc::AF_UNIX, libc::SOCK_DGRAM, 0) {
            Ok(v) => v,
            Err(e) => panic!(
                "发送线程：socketpair 失败（{e}）——发送线程是唯一发送路径，起不来即出口起不来"
            ),
        };
        let fds = [fds0.into_raw_fd(), fds1.into_raw_fd()];
        let (producer, consumer) = txring::txring_new();
        let pending = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        self.wake_r = fds[0];
        let stats = Arc::clone(&self.tx_stats);
        let builder = std::thread::Builder::new()
            .name("homeway-serve-tx".into())
            .stack_size(1024 * 1024);
        let sp = builder.spawn({
            let stop = Arc::clone(&stop);
            let dlogf = Arc::clone(&dlogf);
            // 唤醒合并位必须与 QueuedFace **共用同一实例**（P1 定位修复：实装首版
            // 此处误 new 了第二个 pending ⇒ 生产侧 swap 与消费侧 store 各打各的旗
            // ⇒ 消费侧清的是线程闭包里那个（永 false），生产侧的旗首次置 true 后
            // 无人再清 ⇒ swap 恒返 true ⇒ 首次唤醒后再无唤醒字节 ⇒ 后续包永驻
            // ring、发送线程 poll(-1) 长眠——引擎级「队列发包客户端收不到」的根因，
            // 见 docs/reviews/P1.md「P1 定位记录」）。
            let pending = Arc::clone(&pending);
            move || {
                tx_thread_loop(sock, consumer, fds[0], stats, pending, stop, burst_bytes, dlogf);
            }
        });
        match sp {
            Ok(h) => {
                self.tx_thread = Some(h);
                self.tx_queued = Some(Box::new(QueuedFace {
                    producer,
                    pending,
                    wake_w: fds[1],
                    stop,
                }));
                // 就绪行（滚动判据——启动面即见；形态/排空细节在 5s 观测行
                // 「UDP 出站[发送线程]」/「发送线程 唤醒+…」，那两行有流量才打，
                // 不能当启动判据）。
                (dlogf)(&format!(
                    "发送线程：就绪（homeway-serve-tx，单轮排空上界 {}KiB）",
                    burst_bytes / 1024
                ));
            }
            Err(e) => {
                // spawn 失败：闭包已 drop（sock 随之关闭）；关 socketpair 再 panic。
                unsafe {
                    libc::close(fds[0]);
                    libc::close(fds[1]);
                }
                self.wake_r = -1;
                panic!("发送线程：spawn 失败（{e}）——发送线程是唯一发送路径，起不来即出口起不来");
            }
        }
    }

    /// ring 高水位（P1 两级前置背压：驱动线程本拍整形放行退化为「只并入不释放」
    ///——包留整形 FIFO = 真背压不丢包，满丢成为最后兜底）。
    pub fn tx_high_water(&mut self) -> bool {
        match self.tx_queued.as_mut() {
            Some(q) => q.producer.approx_len() >= txring::HIGH_WATER,
            None => false,
        }
    }

    /// 主 socket 发送面统计快照（驱动线程 5s 行读；窗口语义字段消费即清零）。
    pub(crate) fn tx_stats_snapshot(&self) -> TxStatsSnap {
        TxStatsSnap {
            bytes: self.tx_stats.bytes.load(Ordering::Relaxed),
            calls: self.tx_stats.calls.load(Ordering::Relaxed),
            pkgs: self.tx_stats.pkgs.load(Ordering::Relaxed),
            drops: self.tx_stats.drops.load(Ordering::Relaxed),
            ring_drops: self.tx_stats.ring_drops.load(Ordering::Relaxed),
            wakeups: self.tx_stats.wakeups.load(Ordering::Relaxed),
            drain_ns: self.tx_stats.drain_ns.load(Ordering::Relaxed),
            batch_max: self.tx_stats.batch_max.swap(0, Ordering::Relaxed),
            depth_peak: self.tx_stats.depth_peak.swap(0, Ordering::Relaxed),
            hist: std::array::from_fn(|i| self.tx_stats.hist[i].swap(0, Ordering::Relaxed)),
        }
    }

    /// 收工（驱动线程收工循环后调用）：stop + 唤醒 + join（**无超时**——必返论证：
    /// **不变量：先 store(stop) 再 send(唤醒字节)**，与发送线程空转路径的「stop
    /// 电平复查」（tx_thread_loop）配对 ⇒ 字节必被 poll 看到（复查读到 false ⇒
    /// 字节必在其后发出 ⇒ 其后唯一读干已过去）；其余阻塞面 = 排空循环〔真实工作，
    /// send 非阻塞 socket EAGAIN 短返不长阻塞〕⇒ stop 后至多一轮排空（4096 包 ≈
    /// ms 级）即返。超时/detach 路径**不存在**——泄漏 dup fd → socket 占端口 →
    /// 下次启动端口漂移 → token 端点漂移〔p1a-C-1〕）。发送线程退出语义 =
    /// drain-then-exit（收 stop 先排空剩余再退——宽限期尾数据不丢）。
    pub fn tx_shutdown(&mut self, dlogf: &crate::Logf) {
        if let Some(q) = self.tx_queued.take() {
            q.stop.store(true, Ordering::SeqCst);
            let b = [1u8];
            unsafe {
                libc::send(q.wake_w, b.as_ptr().cast(), 1, libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL)
            };
            if let Some(h) = self.tx_thread.take() {
                if h.join().is_err() {
                    (dlogf)("⚠️ 发送线程 join 失败（panic）——残余存量随线程丢失（洞语义，TCP 重传恢复）");
                }
            }
            unsafe { libc::close(q.wake_w) };
        }
        if self.wake_r >= 0 {
            unsafe { libc::close(self.wake_r) };
            self.wake_r = -1;
        }
    }

    /// 底层 UDP fd（驱动线程 poll(2) 用）。
    pub fn udp_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        self.sock.as_raw_fd()
    }

    /// 从本 socket 直接发裸载荷（STUN 请求等 3e 面；SendRawTo 同义——与数据面同端口）。
    pub fn send_raw_to(&self, addr: SocketAddr, payload: &[u8]) -> io::Result<()> {
        self.sock.send_to(payload, self.xmit_addr(addr)).map(|_| ())
    }

    /// 入站新源的首包一行（每来源一次；容量满清表重记）。
    fn note_new_src(&mut self, src: SocketAddr, shape: &str, n: usize) {
        if self.src_seen.contains_key(&src) {
            return;
        }
        if self.src_seen.len() >= SRC_SEEN_MAX {
            self.src_seen.clear();
            (self.logf)(&format!("入站新源表满（{SRC_SEEN_MAX} 条），清表重记"));
        }
        self.src_seen.insert(src, ());
        (self.logf)(&format!("入站新源：{src}（{shape}，{n} 字节）"));
    }
}

impl Drop for ServerBind {
    fn drop(&mut self) {
        // 防御：未显式 tx_shutdown 的 drop 也走一遍收工（幂等——正常路径
        // driver_loop 尾已调，此处只覆盖驱动线程异常终结的形态；必返论证同
        // tx_shutdown，不引入 drop 阻塞面）。
        let dlogf = Arc::clone(&self.dlogf);
        self.tx_shutdown(&dlogf);
    }
}

// ---------- P1 发送线程体 ----------

/// 发送线程主循环：排空循环（事件驱动、单轮 ≤ burst、轮间无拍隙）+ 空窗长眠
///（poll 无限等待——stop 唤醒字节可醒）。收 stop ⇒ drain-then-exit。
#[allow(clippy::too_many_arguments)] // 线程体一次性装配入参（拆 struct 无收益）
fn tx_thread_loop(
    sock: UdpSocket,
    mut consumer: super::txring::TxConsumer,
    wake_r: i32,
    stats: Arc<TxStats>,
    pending: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    burst: usize,
    dlogf: crate::Logf,
) {
    let mut batch: Vec<Slot> = Vec::with_capacity(256);
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        // 队深峰观测（每唤醒一次，排空前可见深度）
        stats.depth_peak.fetch_max(consumer.visible_len() as u64, Ordering::Relaxed);
        let drained = tx_drain_rounds(&mut consumer, &sock, &stats, burst, &mut batch, &dlogf);
        if drained == 0 {
            // 空转路径（次序不变量 p1a-G-2）：pending 清 → 读干 → 竞态兜底再排 →
            // poll 长眠。「pending 清」必须先于「最后一次空取」——否则 push 方
            // 恰在空取后、store 前完成 ⇒ pending 旧 true ⇒ 不 send ⇒ 睡死。
            pending.store(false, Ordering::Release);
            tx_read_dry(wake_r);
            // stop 电平复查（r1-①，高）：驱动侧收工恒「先 store(stop) 再 send(唤醒
            // 字节)」（tx_shutdown / tx_degrade 的不变量）。本复查读到 false ⇒ 字节必在
            // 其后才发出 ⇒ 之后唯一的读干（上面那行）已经过去 ⇒ 进 poll 时字节必在缓冲里
            // ⇒ poll 立即返回。没有这行：stop+字节落在〔循环顶检查 → tx_read_dry〕窗口内
            // ⇒ 字节被读干、stop 不再复查 ⇒ poll(-1) 长眠 ⇒ join 永等 = 驱动线程整线程
            // 死锁（评审受控复现：窗口加宽 100ms 下 tx_shutdown 45s 不返；补本行 51ms 返）。
            if stop.load(Ordering::Acquire) {
                continue;
            }
            if consumer.has_items() {
                continue;
            }
            let mut pf = libc::pollfd { fd: wake_r, events: libc::POLLIN, revents: 0 };
            let rc = unsafe { libc::poll(&mut pf, 1, -1) };
            stats.wakeups.fetch_add(1, Ordering::Relaxed);
            if crate::envflag::tx_dbg() { eprintln!("[TXDBG] poll rc={rc} revents={:#x}", pf.revents); }
            if rc < 0 {
                let e = io::Error::last_os_error();
                if e.kind() == io::ErrorKind::Interrupted {
                    continue; // EINTR：按到点返回重查 stop/队列
                }
                // 等待原语硬错误（不应发生——EBADF 形态 = fd 被关）：退 1ms 慢轮
                // 防自旋（错误率极低，不值得更复杂的退化闸）
                std::thread::sleep(Duration::from_millis(1));
            }
            tx_read_dry(wake_r);
        }
    }
    // drain-then-exit（收工语义：排空剩余再退——宽限期尾数据不丢）
    while tx_drain_rounds(&mut consumer, &sock, &stats, burst, &mut batch, &dlogf) > 0 {}
}

/// 排空循环：循环取队（单轮 ≤ `burst` 字节——团块上界与整形器单拍上界同语义）
/// 直到空；轮间无拍隙（发送节奏脱离驱动线程 poll 拍的体现）。返回本轮排空包数。
/// msgs 每轮局建（OutMsg 借 batch——跨轮复用 msgs 会把上轮借用钉进签名；分配
/// churn 与 Vec 回收环同登记后续）。
fn tx_drain_rounds(
    consumer: &mut super::txring::TxConsumer,
    sock: &UdpSocket, // dup fd 的属主（生命周期 = 发送线程）；发送借其 raw fd 走 udpbatch
    stats: &TxStats,
    burst: usize,
    batch: &mut Vec<Slot>,
    dlogf: &crate::Logf,
) -> usize {
    let t0 = Instant::now();
    let mut total = 0usize;
    loop {
        batch.clear();
        let n = consumer.pop_batch(batch, burst);
        if n == 0 {
            break;
        }
        stats.calls.fetch_add(1, Ordering::Relaxed);
        stats.pkgs.fetch_add(n as u64, Ordering::Relaxed);
        stats.batch_max.fetch_max(n as u64, Ordering::Relaxed);
        let idx = (usize::BITS as usize - 1 - n.leading_zeros() as usize)
            .min(ServerBind::TX_HIST_BUCKETS - 1);
        stats.hist[idx].fetch_add(1, Ordering::Relaxed);
        let msgs: Vec<crate::udpbatch::OutMsg<'_>> =
            batch.iter().map(|s| crate::udpbatch::OutMsg { dst: s.dst, buf: &s.buf }).collect();
        let (sent, first_err) = crate::udpbatch::send_batch(sock.as_raw_fd(), &msgs);
        if crate::envflag::tx_dbg() {
            eprintln!("[TXDBG] round n={n} sent={sent} dst={} len={} at={:?}", batch[0].dst, batch[0].buf.len(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0));
        }
        // 记账：成功前缀（帧全长口径——Inline 模式的 TxStats.bytes 同口径切换，
        // 与 ServerBind.tx_bytes〔wg 载荷口径，腿面〕相差 4B/包，观测面均包 B 以
        // TxStats 为准）
        let ok_bytes: u64 = batch.iter().take(sent).map(|s| s.buf.len() as u64).sum();
        stats.bytes.fetch_add(ok_bytes, Ordering::Relaxed);
        let dropped = (n - sent) as u64;
        if dropped > 0 {
            let d = stats.drops.fetch_add(dropped, Ordering::Relaxed) + dropped;
            if let Some((ep, e)) = first_err {
                if d <= 3 || d.is_multiple_of(100) {
                    (dlogf)(&format!("发送线程：发送到 {ep} 失败（{e}；累计丢弃 {d} 包）"));
                }
            }
        }
        total += n;
    }
    if total > 0 {
        stats
            .drain_ns
            .fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    total
}

/// 读干唤醒 socketpair（pool.rs 同款先例——残留字节会让 poll 恒可读空转）。
fn tx_read_dry(wake_r: i32) {
    let mut b = [0u8; 64];
    unsafe {
        while libc::recv(wake_r, b.as_mut_ptr().cast(), 64, libc::MSG_DONTWAIT) > 0 {}
    }
}

/// 主 socket 发送面统计快照（驱动线程 5s 行消费；窗口语义字段已清零）。
pub(crate) struct TxStatsSnap {
    pub bytes: u64,
    pub calls: u64,
    pub pkgs: u64,
    pub drops: u64,
    pub ring_drops: u64,
    pub wakeups: u64,
    pub drain_ns: u64,
    pub batch_max: u64,
    pub depth_peak: u64,
    pub hist: [u64; 13],
}

/// 监听口被占用时的退让顺序：+1…+9，最后随机（Go listenWithFallback 同序）。
#[cfg(test)]
fn listen_with_fallback(port: u16) -> io::Result<UdpSocket> {
    listen_with_fallback_addr(port, None)
}

fn listen_with_fallback_addr(port: u16, ip: Option<IpAddr>) -> io::Result<UdpSocket> {
    let try_one = |p: u16| -> io::Result<UdpSocket> {
        match ip {
            // 双栈优先；无 v6 环境回退 v4（Go 在纯 v4 平台同样回落 AF_INET）
            None => bind_dual_stack(p).or_else(|_| UdpSocket::bind((Ipv4Addr::UNSPECIFIED, p))),
            Some(IpAddr::V4(v4)) => UdpSocket::bind((v4, p)),
            // v6 字面量 = 单栈 udp6（Go 显式 V6ONLY=1 同义）
            Some(IpAddr::V6(v6)) => bind_v6_only(v6, p),
        }
    };
    if let Ok(s) = try_one(port) {
        return Ok(s);
    }
    for p in port + 1..=port + 9 {
        if let Ok(s) = try_one(p) {
            return Ok(s);
        }
    }
    try_one(0)
}

/// STUN 应答快速判别（servercore/stun.go 同义：类型 0x0101 + magic cookie）。
fn stun_looks_like_response(b: &[u8]) -> bool {
    b.len() >= 20
        && u16::from_be_bytes([b[0], b[1]]) == 0x0101
        && u32::from_be_bytes([b[4], b[5], b[6], b[7]]) == 0x2112_A442
}

/// WG 报文类型码 → 可读名（首包日志用）。
fn wg_msg_name(b: u8) -> &'static str {
    match b {
        1 => "WG握手发起",
        2 => "WG握手应答",
        3 => "WG cookie",
        4 => "WG传输数据",
        _ => "非WG",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv6Addr;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn noop_logf() -> crate::Logf {
        Arc::new(|_| {})
    }

    /// 双栈监听 + 源地址归一：v4 与 v6 客户端都能打到同一 socket（Go
    /// `ListenUDP("udp", nil)` 双栈语义）；v4 包的 v4-mapped 源被 unmap 回纯 v4
    /// （新源表形态断言）、v6 源保持原样。
    #[test]
    fn dual_stack_listen_and_unmap() {
        let mut srv = ServerBind::open_bound(0, "t", None, None, noop_logf()).unwrap();
        assert!(srv.dual, "无 bind_ip 形态应为双栈 socket");
        let port = srv.local_port();
        let c4 = UdpSocket::bind("127.0.0.1:0").unwrap();
        let c4_addr = c4.local_addr().unwrap();
        let c6 = UdpSocket::bind("[::1]:0").unwrap();
        let c6_addr = c6.local_addr().unwrap();
        c4.send_to(b"v4", (Ipv4Addr::LOCALHOST, port)).unwrap();
        c6.send_to(b"v6", (Ipv6Addr::LOCALHOST, port)).unwrap();
        let mut got = 0;
        for _ in 0..200 {
            if got >= 2 {
                break;
            }
            match srv.recv_packet() {
                Ok(_) => got += 1,
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
        assert_eq!(got, 2, "v4/v6 两个客户端的包都应收到（双栈监听）");
        assert!(
            srv.src_seen.contains_key(&c4_addr),
            "v4 客户端应可达且源已 unmap 为纯 v4（{:?}）",
            srv.src_seen.keys().collect::<Vec<_>>()
        );
        assert!(srv.src_seen.contains_key(&c6_addr), "v6 客户端应可达");
        // 发送面族适配：双栈 socket 发 v4 目标（map 成 v4-mapped）能到达 v4 接收者
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let rx_addr = rx.local_addr().unwrap();
        srv.send_raw_to(rx_addr, b"xmit").unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 8];
        let (n, from) = rx.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"xmit");
        // 发送者地址：dual socket 的 v4 出站 = v4-mapped 形态（unmap 后即 127.0.0.1）
        assert_eq!(unmap_v4_in6(from).ip(), IpAddr::V4(Ipv4Addr::LOCALHOST));
    }

    /// 单栈绑 IP 字面量形态（Go BindAddr udp4/udp6 单栈）：族即所绑族、无双栈 map。
    #[test]
    fn single_stack_by_literal() {
        let srv4 = ServerBind::open_bound(0, "t", Some(IpAddr::V4(Ipv4Addr::LOCALHOST)), None, noop_logf()).unwrap();
        assert!(!srv4.dual);
        let srv6 = ServerBind::open_bound(0, "t", Some(IpAddr::V6(Ipv6Addr::LOCALHOST)), None, noop_logf()).unwrap();
        assert!(!srv6.dual, "v6 字面量是单栈 udp6（Go 同义）");
        // v4 单栈：v6 客户端不可达（族外）；v4 可达
        let c4 = UdpSocket::bind("127.0.0.1:0").unwrap();
        let p4 = srv4.local_port();
        c4.send_to(b"x", (Ipv4Addr::LOCALHOST, p4)).unwrap();
        // unmap 纯函数面
        let m: SocketAddr = "[::ffff:192.0.2.33]:99".parse().unwrap();
        assert!(matches!(m, SocketAddr::V6(_)), "std 解析 v4-mapped 保持 v6 形态");
        assert_eq!(unmap_v4_in6(m), "192.0.2.33:99".parse::<SocketAddr>().unwrap());
        let pure6: SocketAddr = "[2001:db8::1]:7".parse().unwrap();
        assert_eq!(unmap_v4_in6(pure6), pure6, "非 mapped 的 v6 原样");
    }

    /// 收发对拍：容器帧 [reg][data] 的 Go 客户端形态首包（评审 H1 验收）——reg 先于
    /// data 被消费（返回形态的字段序即应用序）、data 进 Inbound。
    #[test]
    fn batch_frame_reg_then_data() {
        let mut b2 = ServerBind::open(0, "test", noop_logf()).unwrap();

        let reg = vec![0x41u8; 66];
        let wg = vec![1u8, 0, 0, 0, 2, 0, 0, 0]; // 假 init 形状
        let batch = frame::batch_bytes(&[
            (FrameKind::Reg.to_wire(), &reg),
            (FrameKind::Data.to_wire(), &wg),
        ]);
        let src: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let r = b2.process_packet(&batch, src).expect("容器帧应投出");
        assert_eq!(r.regs.len(), 1, "reg 应被收集恰好一次");
        assert_eq!(r.regs[0].0, reg);
        let (dsrc, dwg) = r.data.expect("data 应在");
        assert_eq!(dsrc, src);
        assert_eq!(dwg, wg);

        // 同一源的第二包不再记新源（测试日志面：检查不 panic 即可——判据在 3f 实测）
        let r2 = b2.process_packet(&frame::frame_bytes(FrameKind::Data, &wg), src);
        assert!(matches!(r2, Some(i) if i.data.is_some()));
        let _ = (AtomicUsize::new(0), Ordering::SeqCst); // 原计数断言面由形态断言替代
    }

    /// R5-5d2（R4 遗留「出口侧测试扩展到 Go 对照面」）：Go 生产真源产的 relay
    /// 控制帧字节（fixtures/vectors/relay.json——vecgen 产自 baseline EncodeFrame/
    /// Hello/Challenge/Proof/OK/Again/Keepalive 真源）喂出口 process_packet——
    /// type=3 分派到 on_leg_frame 钩子（字节原样透传给 relay-leg 线程，不在驱动
    /// 线程解析）+ 形态行不 panic。
    #[test]
    fn leg_frames_dispatch_go_wire_bytes() {
        let v: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../fixtures/vectors/relay.json"
            ))
            .unwrap(),
        )
        .unwrap();
        let unhex = |s: &str| -> Vec<u8> {
            (0..s.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
                .collect()
        };
        use std::sync::Mutex;
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let sink = Arc::clone(&seen);
        b.set_on_leg_frame(Box::new(move |payload: &[u8], _src: SocketAddr| {
            sink.lock().unwrap().push(payload.to_vec());
        }));
        let src: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let mut n_ctl = 0usize;
        for c in v["cases"].as_array().unwrap() {
            // 向量 wire = [subtype][body] 载荷；出口腿上收到的是腿帧壳
            // （relay_reg_frame = [0xBB][3][len][payload]——Go EncodeFrame 同构）
            let payload = unhex(c["wire"].as_str().unwrap());
            let wire = crate::relaywire::relay_reg_frame(&payload);
            let r = b.process_packet(&wire, src);
            n_ctl += 1;
            assert!(r.is_none(), "type=3 帧 = 内部消费（不产 Inbound）");
        }
        let got = seen.lock().unwrap().clone();
        assert_eq!(got.len(), n_ctl, "钩子收到的 type=3 载数应与分派数一致");
        // hello 帧（首 case）载荷应原样到达钩子（透传语义——relay-leg 线程负责解析）
        let hello_payload = unhex(v["cases"][0]["wire"].as_str().unwrap());
        assert_eq!(got[0], hello_payload, "hello 载荷应字节原样透传");
    }

    /// probe 应答路径：HWQ → HWR（同 nonce/build/flags）；列表段受 pad 契约约束。
    #[test]
    fn probe_responds_on_socket_path() {
        let mut b = ServerBind::open(0, "rust-exit-test", noop_logf()).unwrap();
        let nonce = [9u8; 8];
        let req = crate::probe::encode_request(crate::probe::TYPE_PING, &nonce, 200);
        let src: SocketAddr = "127.0.0.1:5002".parse().unwrap();
        assert!(b.process_packet(&req, src).is_none(), "探测被消费");

        // 直接对 respond_ex 验细节（socket 回发路径由 3f 实测覆盖）
        let resp = crate::probe::respond_ex(
            &req,
            "rust-exit-test",
            0x01,
            &["203.0.113.9:42641".parse().unwrap()],
        )
        .unwrap();
        assert_eq!(&resp[..3], b"HWR");
        assert_eq!(&resp[5..13], &nonce);
        // pad 200 够 → 带列表
        assert!(resp.len() > 13 + 8);
        // pad 16 的老形态 → 不带列表（防放大）
        let old_req = crate::probe::encode_request(crate::probe::TYPE_PING, &nonce, 16);
        let old_resp =
            crate::probe::respond_ex(&old_req, "b", 0, &["203.0.113.9:42641".parse().unwrap()])
                .unwrap();
        assert!(old_resp.len() <= old_req.len() + 45, "45B 不变量");
    }

    /// 非帧包丢弃 + 形态串；畸形腿帧不中断。
    #[test]
    fn non_frame_and_malformed_shapes() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let src: SocketAddr = "127.0.0.1:5003".parse().unwrap();
        // 裸 WG init（非帧）
        let mut raw = vec![1u8];
        raw.resize(148, 0);
        assert!(b.process_packet(&raw, src).is_none());
        // 畸形腿帧（长度 <2——只有魔数；Go DecodeFrame 同判）
        assert!(b.process_packet(&[0xBB], src).is_none());
        // 容器畸形（越界长度）
        let bad_batch = frame::frame_bytes(FrameKind::Batch, &[0, 0xFF, 0]);
        assert!(b.process_packet(&bad_batch, src).is_none());
        // hint 帧消费
        let hint = frame::hint_bytes("1.2.3.4:9");
        assert!(b.process_packet(&hint, src).is_none());
    }

    // ---------- 腿表（R4；评审 中-4 补测） ----------

    /// 同 id 重注册 = 先拆后建（中继重放语义）；同远端替换；上限只拦新 id（C3）。
    #[test]
    fn leg_table_register_replace_and_cap() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let r1: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let r2: SocketAddr = "127.0.0.1:5002".parse().unwrap();
        assert!(b.register_leg(1, r1, b"marker").is_ok());
        assert_eq!(b.leg_fds().len(), 1);
        // 同 id 重注册（重放）= 替换：远端换成 r2，仍是 1 条
        assert!(b.register_leg(1, r2, b"marker").is_ok());
        assert_eq!(b.leg_fds().len(), 1);
        // 同远端不同 id = 顶掉旧 id
        assert!(b.register_leg(2, r2, b"marker").is_ok());
        assert_eq!(b.leg_fds().len(), 1);
        // 上限只拦新 id：填满后，已有 id 的替换仍过、新 id 拒（C3）
        // 表内此刻 = {2@r2}（id 1 被同远端顶掉）→ 补 RELAY_LEG_MAX-1 条新 id 恰满
        for i in 3..=(RELAY_LEG_MAX as u64 + 1) {
            let r: SocketAddr = format!("127.0.0.1:{}", 6000 + i).parse().unwrap();
            assert!(b.register_leg(i, r, b"m").is_ok(), "id={i} 应入表");
        }
        assert_eq!(
            b.leg_fds().len(),
            RELAY_LEG_MAX,
            "应恰好满表（id2 + 62 新 id）"
        );
        let extra: SocketAddr = "127.0.0.1:6999".parse().unwrap();
        assert!(matches!(
            b.register_leg(999, extra, b"m"),
            Err(crate::server::relayleg::LegError::LegCap(RELAY_LEG_MAX))
        ));
        // 已有 id 的替换（重放语义）不受上限拦——id 1 已被顶掉，用仍在表内的 id 5
        assert!(
            b.register_leg(5, r1, b"m").is_ok(),
            "已有 id 替换不受上限拦"
        );
        b.clear_legs();
        assert_eq!(b.leg_fds().len(), 0);
    }

    /// #17 丢弃语义：腿在 = 走腿（tx 计数）；腿摘 = 丢弃（不回落主 socket）；
    /// leg_ports 记忆不受 5min 窗限。
    #[test]
    fn leg_send_dispatch_and_drop() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        // 装配序对齐产品（发送线程 = 唯一发送路径，send_wire 前必装配）
        let sock_dup = b.try_clone_socket().unwrap();
        b.tx_start(sock_dup, 64 * 1024, noop_logf());
        // 腿远端要有真监听者（macOS 连接 UDP：对无人监听口的 ICMP 回来后
        // send=ECONNREFUSED——腿会被误判死亡）
        let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
        let r1: SocketAddr = sink.local_addr().unwrap();
        assert!(b.register_leg(7, r1, b"m").is_ok());
        let mut out = InboundOut::default();
        out.wire.push((r1, vec![1u8; 32]));
        let tx0 = b.tx_bytes;
        b.send_wire(&out);
        assert!(b.tx_bytes > tx0, "命中腿：经腿 socket 发（计数推进）");
        // 摘腿后同 endpoint → #17 丢弃（不回落主 socket——tx 不动）
        b.remove_leg(7);
        out.wire.clear();
        out.wire.push((r1, vec![2u8; 32]));
        let tx1 = b.tx_bytes;
        b.send_wire(&out);
        assert_eq!(b.tx_bytes, tx1, "#17：腿已摘的 endpoint 丢弃出站");
        b.tx_shutdown(&noop_logf());
    }

    /// LEGUP 标记吞包防御（5B/37B）与正常帧入 device 的分派。
    #[test]
    fn leg_readable_swallows_legup_markers() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        // 用一个真监听口当腿远端（连接 socket 能建）
        let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
        let r: SocketAddr = sink.local_addr().unwrap();
        assert!(b.register_leg(9, r, b"LEGUP").is_ok());
        let fd = b.leg_fds()[0];
        // 5B v1 标记：吞
        assert!(sink.send_to(b"LEGUP", r).is_ok());
        std::thread::sleep(Duration::from_millis(50));
        let (alive, inbound) = b.leg_readable(fd);
        assert!(alive);
        assert!(inbound.is_none(), "5B LEGUP 标记应吞包");
        // 37B v2 标记：吞
        let cookie = [3u8; 16];
        let marker = crate::relaywire::legup_payload(9, &cookie, &[0u8; 32]);
        assert_eq!(marker.len(), 37);
        assert!(sink.send_to(&marker, r).is_ok());
        std::thread::sleep(Duration::from_millis(50));
        let (alive, inbound) = b.leg_readable(fd);
        assert!(alive);
        assert!(inbound.is_none(), "37B LEGUP 认证标记应吞包");
    }

    /// 端口退让：占用后 +1（真实 socket 面）。
    #[test]
    fn port_fallback_on_busy() {
        let holder = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = holder.local_addr().unwrap().port();
        let s = listen_with_fallback(port).unwrap();
        assert_ne!(s.local_addr().unwrap().port(), port, "被占应退让");
    }

    // ---------- P1 发送路径拆分 ----------

    /// 端到端（Queued 模式；harness 中臂形态）：send_wire 入队 → 发送线程排空 →
    /// 回环 sink 收齐保序。覆盖：判定面/帧化/入队/唤醒次序/排空循环/统计记账全链。
    /// 包量 = 3000（< ring 容量 4096——测试形态 = 产品形态的「入队受上游整形钳制」，
    /// 不构造满丢〔满丢语义由 txring 单测钉死〕；回环全速 sink ⇒ 全收 + 保序 + 满丢 0）。
    #[test]
    fn tx_thread_end_to_end_ordered() {
        const N: u32 = 3000;
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
        // 收侧放大缓冲（8000×66B ≈ 528KB——默认 RCVBUF 会丢尾批）
        unsafe {
            let sz: libc::c_int = 8 * 1024 * 1024;
            libc::setsockopt(
                sink.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                &sz as *const _ as *const libc::c_void,
                std::mem::size_of::<libc::c_int>() as u32,
            );
        }
        let dst = sink.local_addr().unwrap();
        let sock_dup = b.try_clone_socket().unwrap();
        b.tx_start(sock_dup, 64 * 1024, noop_logf());
        // 收线程并行收（回环 RCVBUF 有限——主线程只发不收会溢出丢尾）
        let rx = std::thread::spawn({
            let sink2 = sink.try_clone().unwrap();
            move || {
                sink2.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                let mut got: Vec<u32> = Vec::new();
                let mut buf = [0u8; 128];
                while (got.len() as u32) < N {
                    match sink2.recv_from(&mut buf) {
                        // 腿帧壳 = [0xBB][kind=0][payload]——载荷在 +2 偏移
                        Ok((n, _)) => {
                            assert!(n >= 6, "腿帧壳 + 载荷");
                            assert_eq!(buf[0], 0xBB, "帧魔数");
                            got.push(u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]));
                        }
                        Err(_) => break,
                    }
                }
                got
            }
        });
        // 分批入队（每包唯一序号载荷；驱动线程面形态——多次 send_wire 小批）
        for chunk in 0..(N / 500) {
            let mut out = InboundOut::default();
            for i in 0..500u32 {
                let mut wg = (chunk * 500 + i).to_le_bytes().to_vec();
                wg.resize(64, 0);
                out.wire.push((dst, wg));
            }
            b.send_wire(&out);
        }
        let got = rx.join().unwrap();
        assert_eq!(got.len(), N as usize, "应收满 {N} 包（实收 {}）", got.len());
        let expect: Vec<u32> = (0..N).collect();
        assert_eq!(got, expect, "FIFO 保序（帧壳 2B 后载荷序号连续）");
        // 统计面：排空包数 ≥ N（轮计数 ≥ 1）+ 满丢 0
        let snap = b.tx_stats_snapshot();
        assert!(snap.pkgs >= N as u64, "排空包数应计入（{}）", snap.pkgs);
        assert!(snap.calls >= 1, "排空轮应计入");
        assert_eq!(snap.ring_drops, 0, "中臂满丢应为 0");
        assert_eq!(snap.drops, 0, "中臂 send_batch 丢弃应为 0");
        // 收工（drain-then-exit + join 必返）
        b.tx_shutdown(&noop_logf());
    }

    /// **根因回归钉**（P1 定位修复）：间隔推送必须每次重新唤醒发送线程。
    /// 实装首版 `tx_start` 误为线程闭包与 QueuedFace 各 new 一个 `pending`
    ///（生产侧 swap / 消费侧 store 各打各的旗）⇒ 首次唤醒后生产侧旗恒 true ⇒
    /// 再无唤醒字节 ⇒ 首排空窗之外的包永驻 ring（引擎级「队列发包客户端收不到」
    /// / speedtest SYN-ACK 丢 / Queued 全量死锁的根因）。既有
    /// `tx_thread_end_to_end_ordered` 是紧循环连推——全部落在首个排空窗内，
    /// 钉不住本形态；本测试用「推送→收妥→落回 poll→再推送」的间隔节拍
    ///（真实流量 request→process→response 的形态）钉死：**每次间隔推送都可达**
    ///（判据只断到达——唤醒计数在抢占下可合法合并〔线程在 send 后、pending 清
    /// 前被抢占 >50ms ⇒ 下一轮并入同周期〕，不做数值断言）。
    #[test]
    fn tx_thread_gapped_push_rewakes() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let sink = UdpSocket::bind("127.0.0.1:0").unwrap();
        let dst = sink.local_addr().unwrap();
        // 并行测试负载下线程调度延迟可观——超时给 10s（判据是「可达」不是「快」）
        sink.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let sock_dup = b.try_clone_socket().unwrap();
        b.tx_start(sock_dup, 64 * 1024, noop_logf());
        // 三轮间隔推送：每轮 1 包、轮间等收妥 + 落回 poll（50ms ≫ 排空/回睡时序）
        for round in 0..3u32 {
            let mut out = InboundOut::default();
            let mut wg = round.to_le_bytes().to_vec();
            wg.resize(64, 0);
            out.wire.push((dst, wg));
            b.send_wire(&out);
            let mut buf = [0u8; 128];
            let (n, _) = sink.recv_from(&mut buf).unwrap_or_else(|e| {
                panic!("第 {round} 轮间隔推送未到达（发送线程未再唤醒——pending 双实例回归）：{e}")
            });
            assert_eq!(u32::from_le_bytes([buf[2], buf[3], buf[4], buf[5]]), round, "帧壳载荷序号");
            assert_eq!(buf[0], 0xBB, "帧魔数");
            assert!(n >= 66);
            std::thread::sleep(Duration::from_millis(50)); // 让发送线程落回 poll 长眠
        }
        let snap = b.tx_stats_snapshot();
        assert!(snap.wakeups >= 1, "至少应有一次唤醒（实醒 {}）", snap.wakeups);
        assert!(snap.pkgs >= 3, "三轮各 1 包都应经发送线程计账（{}）", snap.pkgs);
        assert_eq!(snap.ring_drops, 0);
        b.tx_shutdown(&noop_logf());
    }

    /// harness 快臂登记（P1c）：曾实装「发送线程排空吞吐 vs 内联 send_batch」
    /// 的回环对照（60k 包）——**macOS 回环内核面不可靠**：inline 臂 tx socket
    /// 默认 ~9KB SNDBUF 的流控坑（已修：4MB 同产品）之外，更大包量下逐包
    /// send_to 仍偶发卡进内核流控（非阻塞 socket 也不返回——flaky 挂死，复现率
    /// 随系统状态漂移）。**快臂能力对照移到本地引擎端到端（local-rust-exit +
    /// speedtest，真实管线）与真机 2×2 终验**（PERF-AB P1 节）——与仓惯例一致
    ///（harness 判机制，性能终验真机判）。
    ///
    /// 收工面：tx_shutdown 幂等（重复收工/未装配面不 panic 不挂死）。
    #[test]
    fn tx_shutdown_idempotent() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let sock_dup = b.try_clone_socket().unwrap();
        b.tx_start(sock_dup, 64 * 1024, noop_logf());
        b.tx_shutdown(&noop_logf());
        b.tx_shutdown(&noop_logf()); // 幂等
    }

    /// 高水位面：tx_high_water 在空 ring 时恒 false（背压不误触发——接线面；
    /// ring 本身的满语义由 txring 单测钉死）。
    #[test]
    fn tx_high_water_idle_false() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let sock_dup = b.try_clone_socket().unwrap();
        b.tx_start(sock_dup, 64 * 1024, noop_logf());
        assert!(!b.tx_high_water(), "空 ring 不触发");
        b.tx_shutdown(&noop_logf());
    }
}
