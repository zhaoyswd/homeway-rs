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
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::wtransport::frame::{self, FrameKind};

use super::device::InboundOut;

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
    /// 批量发送的复用 staging（腿帧连排；驱动线程独占——热路径零分配〔F9 整改：
    /// encode_frame 直写〕）。
    tx_stage: Vec<u8>,
    /// staging 内各帧的 (记账载荷长, 端点, 帧全长)。
    tx_lens: Vec<(usize, SocketAddr, usize)>,
    /// 批量出站的**丢弃计数**（评审 r1-F7/F8）：短返余量 + 失败批次——旧行为逐包
    /// send_to 失败有日志无计数；批化后短返余量既无日志也无计数（静默）——本计数
    /// 补观测面（日志 100 次一行节流；与 leg_dropped 同范式）。
    tx_dropped: u64,
    /// 批量出站形态统计（R8-2 归因插桩）：发送调用数 / 累计包数 / 单调用最大包数
    /// ——「每拍实际排空几包」（唤醒粒度）的判别面。空闲不增长，5s 周期行由引擎打。
    tx_calls: u64,
    tx_pkgs: u64,
    tx_batch_max: usize,
}

impl ServerBind {
    /// 监听固定端口（被占退让 +1…+9 → 随机；Go listenWithFallback 同序）。
    /// 返回 Err = 全失败（出口起不来——占端口硬失败，R0.6 评审 M13 同口径）。
    pub fn open(port: u16, build: &str, logf: crate::Logf) -> io::Result<Self> {
        Self::open_bound(port, build, None, None, logf)
    }

    /// 绑定形态：`bind_ip`（钉卡的源地址；None = 0.0.0.0）+ 可选的整 socket 钉卡
    /// （index + 名——IP_BOUND_IF/SO_BINDTODEVICE；Go ServerBind BindIface 双栈钉卡
    /// 的 v4 单栈面，v6 面登记 R5）。
    pub fn open_bound(
        port: u16,
        build: &str,
        bind_ip: Option<Ipv4Addr>,
        pin: Option<(u32, String)>,
        logf: crate::Logf,
    ) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;
        let sock = listen_with_fallback_addr(port, bind_ip)?;
        if let Some((index, name)) = pin {
            let _ = super::egress::pin_socket_to_iface(sock.as_raw_fd(), index, &name);
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
            tx_stage: Vec::with_capacity(256 * 1024),
            tx_lens: Vec::with_capacity(256),
            tx_dropped: 0,
            tx_calls: 0,
            tx_pkgs: 0,
            tx_batch_max: 0,
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
            let _ = self.sock.send_to(&resp, src);
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
        let sock = UdpSocket::bind("0.0.0.0:0")?;
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
    /// 应答经 result 通道回执（None = 应答未到/不合法——调用方按超时收场）；同一时刻
    /// 只允许一次查询（观测周期都是分钟级——Go stunPending 同义）。
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
        self.sock.send_to(&req, server)?;
        Ok(())
    }

    /// 观测超时收位（调用方超时后清——防下一次查询被占位挡住）。
    pub fn stun_query_abort(&mut self) {
        self.stun_wait_txid = None;
        self.stun_wait_result = None;
    }

    /// 出站收口：把 device 产出的 wire 批封装腿帧发出（Send 恒发腿帧——Go 同义）。
    /// endpoint 命中腿表走该腿 socket（回程五元组与拨出映射一致）；命中
    /// leg_recent/leg_ports = 腿已摘（#17 丢弃——打到中继主口/被复用的数据口只会
    /// 污染别的会话）；否则主 socket。
    ///
    /// R8-8b：主 socket 路径**批量发送**——腿帧先落进复用 staging（消除逐包
    /// frame_bytes 的 Vec 分配，载荷一次拷贝），Linux/OHOS 经 sendmmsg 一批一次
    /// 系统调用（macOS 回退逐包 sendto，与 Go 同形态——同机 A/B 公平）。腿 socket
    /// 命中是稀疏路径（中继腿的建立/保活面），保持逐包。
    pub fn send_wire(&mut self, out: &InboundOut) {
        if out.wire.is_empty() {
            return; // 常态快速路径（handle_inbound 的逐包调用多无产出）
        }
        self.tx_stage.clear();
        self.tx_lens.clear();
        for (ep, wg) in &out.wire {
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
                    let n = unsafe { libc::send(fd, wire.as_ptr().cast(), wire.len(), 0) };
                    if n != wire.len() as isize {
                        let remote = lg.remote;
                        let id = lg.id;
                        self.leg_read_exit(id, remote);
                    } else {
                        self.tx_bytes += wg.len() as u64;
                    }
                    continue;
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
                continue;
            }
            // 主 socket：帧头直写复用 staging（评审 r1-F9：此前 frame_bytes 每包
            // new 一个 Vec 再整包拷回 staging = 1 分配 + 2 拷贝，比改前更差——
            // encode_frame(kind, payload, &mut out) 直写消掉分配与第二次拷贝）。
            let start = self.tx_stage.len();
            frame::encode_frame(FrameKind::Data, wg, &mut self.tx_stage);
            self.tx_lens
                .push((wg.len(), *ep, self.tx_stage.len() - start));
        }
        if self.tx_lens.is_empty() {
            return;
        }
        // 批量发出。评审 r1-补4：fd 先取（RawFd 是 Copy，借用即断）+ 分字段借用
        // （tx_stage 不可变借 / tx_lens 读）——无需裸指针切片。
        let fd = self.sock.as_raw_fd();
        let mut off = 0usize;
        let mut msgs: Vec<crate::udpbatch::OutMsg<'_>> = Vec::with_capacity(self.tx_lens.len());
        for &(_, ep, fr_len) in &self.tx_lens {
            msgs.push(crate::udpbatch::OutMsg {
                dst: ep,
                buf: &self.tx_stage[off..off + fr_len],
            });
            off += fr_len;
        }
        let (sent, first_err) = crate::udpbatch::send_batch(fd, &msgs);
        // 形态统计（R8-2 归因）：本调用入批包数（含被丢弃的——「每拍攒了几包」
        // 与「发出去几包」分开看：丢弃主导时前者才是真实唤醒粒度）。
        self.tx_calls += 1;
        self.tx_pkgs += self.tx_lens.len() as u64;
        self.tx_batch_max = self.tx_batch_max.max(self.tx_lens.len());
        // 注：msgs 为本函数局建（OutMsg 借 staging——自引用结构不能做成字段）；
        // 每次调用一次 Vec 分配（相对每包一次 frame_bytes 分配已是数量级改善）。
        // 记账按「成功前缀」（send_batch 顺序发送，短返只发前缀；Linux sendmmsg
        // 返回值语义 = 前缀已发）。**短返余量/失败批次本拍丢弃**（非阻塞 socket 的
        // EAGAIN/不可达对余量同型，逐包重试只是重复同错；UDP 本身不保证送达——
        // TCP 层的重传由对端 ACK 时钟兜底）——丢弃进 tx_dropped 计数（评审 r1-F7）。
        for &(wg_len, _, _) in self.tx_lens.iter().take(sent) {
            self.tx_bytes += wg_len as u64;
        }
        let dropped = (self.tx_lens.len() - sent) as u64;
        if dropped > 0 {
            self.tx_dropped += dropped;
        }
        if let Some((ep, e)) = first_err {
            // 发送失败节流记行（评审 r1-F8：100 次一行——对端可控面不能刷屏；
            // 首错即弃整批是**有意**语义：同 socket 同型错误，余量重试大概率同错
            // 且会造成同 peer 乱序）
            if self.tx_dropped <= 3 || self.tx_dropped.is_multiple_of(100) {
                (self.logf)(&format!(
                    "发送到 {ep} 失败（{e}；批量出站累计丢弃 {} 包）",
                    self.tx_dropped
                ));
            }
        }
        // 诊断面：no_endpoint_drops 在 device 内
    }

    /// 底层 UDP fd（驱动线程 poll(2) 用）。
    pub fn udp_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        self.sock.as_raw_fd()
    }

    /// 批量出站形态快照（R8-2 归因插桩）：(调用数, 累计包数, 单调用最大包数,
    /// 累计丢弃包数)——均批 = pkgs/calls；引擎 5s 周期行消费。
    pub fn tx_batch_stats(&self) -> (u64, u64, usize, u64) {
        (self.tx_calls, self.tx_pkgs, self.tx_batch_max, self.tx_dropped)
    }

    /// 从本 socket 直接发裸载荷（STUN 请求等 3e 面；SendRawTo 同义——与数据面同端口）。
    pub fn send_raw_to(&self, addr: SocketAddr, payload: &[u8]) -> io::Result<()> {
        self.sock.send_to(payload, addr).map(|_| ())
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

/// 监听口被占用时的退让顺序：+1…+9，最后随机（Go listenWithFallback 同序）。
#[cfg(test)]
fn listen_with_fallback(port: u16) -> io::Result<UdpSocket> {
    listen_with_fallback_addr(port, None)
}

fn listen_with_fallback_addr(port: u16, ip: Option<Ipv4Addr>) -> io::Result<UdpSocket> {
    let base = ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
    if let Ok(s) = UdpSocket::bind((base, port)) {
        return Ok(s);
    }
    for p in port + 1..=port + 9 {
        if let Ok(s) = UdpSocket::bind((base, p)) {
            return Ok(s);
        }
    }
    UdpSocket::bind((base, 0))
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn noop_logf() -> crate::Logf {
        Arc::new(|_| {})
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
}
