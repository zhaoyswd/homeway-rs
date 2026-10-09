//! 出口**腿面**（R3/R4；语义真源 `baseline:pkg/servercore/bind.go` 的腿族 + `relayctl.go`
//! 的出口侧拨腿）。M5 S3b 裁剪后本模块只剩**与承载无关的腿面**——WG 载荷面（device 解密/
//! 入队发送/发送线程/STUN/探测应答/绑定重钉）已迁出或删除：
//!
//! - 公共端点面五类 → `server::pubface`（QUIC 出口 socket 的公共端口）；
//! - WG 载荷/发送线程/`send_wire`/`txring`/`device` → 随 WG 面删除（M5 S3b）；
//! - **保留 = QUIC 经中继的唯一通路**（`docs/reviews/M5-design.md` §1.1「腿面订正」）：
//!   `legs` 腿表族（`register_leg`/`remove_leg`/`clear_legs`/`sweep_legs`/`leg_readable`/
//!   `leg_fds`/`leg_remotes`/`leg_send_handle`/`shutdown_legs`/`#17 最近摘除窗`）+
//!   腿 socket 收包解析（kind=5 → `QuicLegPkt`，交引擎注入 QUIC 面；kind=3 → 中继控制帧）
//!   + 腿面上 kind=5 的投递计数（`note_quic_leg_undelivered`）。
//!
//! 腿帧线格式（FIX-91 统一线格式）：`[0xBB][kind][len][payload]`——腿 socket 上到达的
//! 只可能是 0/3/5 三类（0/4 随 WG 面退役；其余 kind 容忍忽略）。**单一读者**是构造性
//! 不变量：一条腿 socket 的读侧只有本模块（引擎驱动线程 poll + `leg_readable`），QUIC 面
//! 只经 `leg_send_handle` 拿**发送**句柄。
//!
//! **腿表**：控制面 SESSION 通告 → `register_leg`（连接 UDP socket 拨腿 + 发 LEGUP 认证
//! 标记）；#17：腿已摘但发送目的地命中「曾当过腿」的地址 → 丢弃（防打到中继主口/被复用的
//! 数据口）。发送侧路由由 QUIC 面自己的腿表承接（`leg_send_handle` 交出去的 fd 副本）。

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::os::fd::AsRawFd as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::legframe::{self, FrameKind};
use crate::udpbatch::{bind_dual_stack, bind_v6_only};

use super::pubface::SrcSeen;

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

/// 一条腿上的 QUIC 报文（`[0xBB][5][payload]` 的解析产物；M1 S1c §1.6）。
///
/// `src` = **该腿的远端地址**（= QUIC 眼里的对端地址：中继数据口）。口必须是腿远端而
/// 不是包的来源字段——腿 socket 是 `connect` 的，中继就是靠「腿 socket ↔ assoc」对应，
/// 出口 QUIC 面的腿表也按同一地址做发送路由（`Transmit.destination`）。
pub struct QuicLegPkt {
    pub src: SocketAddr,
    pub payload: Vec<u8>,
}

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

/// 腿 fd 可读的产出（`leg_readable` 的返回契约）：`Some` = 一条 kind=5 载荷待注入
/// QUIC 面；`None` = 本包被内部消费（控制帧/畸形/未知 kind）。
pub struct LegInbound {
    pub quic: QuicLegPkt,
}

/// 出口腿面（驱动线程独占）。
pub struct ServerBind {
    logf: crate::Logf,
    /// 细节日志面（#17 丢弃行——Go logfD 对应）。
    dlogf: crate::Logf,
    on_leg_frame: Option<OnLegFrame>,
    src_seen: SrcSeen,
    // ---- 腿表（驱动线程独占；R4） ----
    leg_by_id: HashMap<u64, RelayLeg>,
    leg_by_r: HashMap<SocketAddr, u64>,
    /// 最近摘除的腿远端（TTL 内 Send 判「不回落主 socket」）。
    leg_recent: HashMap<SocketAddr, Instant>,
    /// 曾当过一次腿的远端地址（Send 兜底丢弃判据；不受 5min 窗限制——#17）。
    leg_ports: HashSet<SocketAddr>,
    leg_dropped: u64,
    /// 腿面上收到的 kind=5（QUIC 载荷）报文数（M1 S1c §1.6）。
    quic_leg_pkts: u64,
    /// 因 QUIC 面不可用而丢掉的 kind=5 报文数（丢可见面）。
    quic_leg_undelivered: u64,
}

impl ServerBind {
    /// 装配（无自有 socket：公共端口归 QUIC 面，见模块头）。
    pub fn new(logf: crate::Logf) -> Self {
        Self {
            logf: Arc::clone(&logf),
            dlogf: logf,
            on_leg_frame: None,
            src_seen: SrcSeen::new(),
            leg_by_id: HashMap::new(),
            leg_by_r: HashMap::new(),
            leg_recent: HashMap::new(),
            leg_ports: HashSet::new(),
            leg_dropped: 0,
            quic_leg_pkts: 0,
            quic_leg_undelivered: 0,
        }
    }

    /// 中继控制帧（type=3）钩子（R4：转发给 relay-leg 线程；未装 = 照旧忽略）。
    pub fn set_on_leg_frame(&mut self, f: OnLegFrame) {
        self.on_leg_frame = Some(f);
    }

    /// 腿 socket 上的一个入站包解析（kind 分类；**单一读者**见模块头）。
    /// `pub(crate)` 形态：本函数是腿 socket 读侧的唯一入口（`leg_readable` 调）。
    fn process_packet(&mut self, buf: &[u8], src: SocketAddr) -> Option<LegInbound> {
        if buf.first() == Some(&legframe::FRAME_MAGIC) {
            let Some((kind, payload)) = legframe::decode_frame(buf) else {
                self.src_seen.note(&self.logf, src, "畸形腿帧", buf.len());
                return None; // 畸形腿帧：丢弃不中断
            };
            if kind == FrameKind::Quic.to_wire() {
                // QUIC 载荷（M1 S1c §1.6）：**不记新源行**（Q-P——设计 §3.4 E23 条：
                // 每个 kind 分支都会调 `note_new_src`，QUIC 档必须显式跳过；该档的
                // 「换源」语义由 E-q2（`路径变更`）承接）。载荷原样交给 QUIC 面。
                self.quic_leg_pkts += 1;
                return Some(LegInbound { quic: QuicLegPkt { src, payload: payload.to_vec() } });
            }
            if kind == crate::relaywire::FRAME_TYPE_RELAY_REG {
                // 中继控制帧：转发给 relay-leg 线程（解析/源校验不进驱动线程）
                self.src_seen.note(&self.logf, src, "腿帧type=3", buf.len());
                if let Some(f) = &mut self.on_leg_frame {
                    f(payload, src);
                }
                return None;
            }
            // 其余 kind（0/1/2/4 随 WG 面退役 / 未知）：容忍忽略（前向兼容）
            self.src_seen.note(&self.logf, src, &format!("腿帧type={kind}"), buf.len());
            return None;
        }
        // 非腿帧包（旧客户端裸 WG / 垃圾 / 公网面误入）：丢弃计数。
        let shape = match buf.first() {
            Some(b) => format!("非帧（首字节=0x{b:02x}）"),
            None => "非帧包".to_string(),
        };
        self.src_seen.note(&self.logf, src, &shape, buf.len());
        None
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
        self.leg_ports.insert(remote); // 记住「这个端口当过腿」（#17 的兜底丢弃判据）
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
    /// 同一条 `process_packet` 解析（产出交驱动线程——**与公共端口分离**：kind=5 的
    /// 载荷交 QUIC 面注入、kind=3 交注册腿线程）。返回 (存活, 产出)。
    pub fn leg_readable(&mut self, fd: std::os::fd::RawFd) -> (bool, Option<LegInbound>) {
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
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => (true, None),
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

    /// 当前腿远端集（QUIC 面腿表做差分——新腿登记发送句柄、摘除的腿摘句柄）。
    pub fn leg_remotes(&self) -> Vec<SocketAddr> {
        self.leg_by_id.values().map(|lg| lg.remote).collect()
    }

    /// 某腿的**发送句柄**（`try_clone` 的 fd 副本）：与读侧共享同一 socket、同一本地
    /// 端口，故中继仍认成同一条腿；QUIC 面按远端地址路由发送（`Transmit.destination`）。
    /// None = 该远端没有腿 / 克隆失败（失败只记行）。
    pub fn leg_send_handle(&mut self, remote: &SocketAddr) -> Option<UdpSocket> {
        let lg = self.leg_by_id.values().find(|lg| lg.remote == *remote)?;
        match lg.sock.try_clone() {
            Ok(s) => Some(s),
            Err(e) => {
                (self.logf)(&format!(
                    "腿（→ {remote}）发送句柄克隆失败（{e}）—— 该腿上的 QUIC 报文将无法回程"
                ));
                None
            }
        }
    }

    /// #17 的出站兜底丢弃（QUIC 面腿表的「最近摘除」窗由面侧自持；本计数面留给
    /// 腿面上的旧路径与观测——语义 = 「目标曾当过腿、现已摘除，不回落公共端口」）。
    pub fn note_leg_dropped(&mut self, ep: &SocketAddr, pkgs: usize) {
        self.leg_dropped += 1;
        let c = self.leg_dropped;
        if c <= 3 || c.is_multiple_of(1000) {
            (self.dlogf)(&format!("腿已摘或非现任（{ep}）丢弃出站 {pkgs} 包（等控制面重放重建腿）"));
        }
    }

    /// kind=5 帧**投不进 QUIC 面**（面缺席/已死）的计数 + 节流记行（首 3 + 每 100——
    /// 仓内既有口径）。M1 §6.4 的「丢弃可观测、不静默」在出口腿面的落点。
    pub fn note_quic_leg_undelivered(&mut self, n: usize) {
        self.quic_leg_undelivered += 1;
        let c = self.quic_leg_undelivered;
        if c <= 3 || c.is_multiple_of(100) {
            (self.logf)(&format!(
                "腿上的 QUIC 报文无法投递（出口 QUIC 面不可用：未起/已收工）——已丢 {c} 个（最近 {n} 字节）"
            ));
        }
    }

    /// 腿面上收到的 kind=5 报文数（测试/观测面）。
    pub fn quic_leg_pkts(&self) -> u64 {
        self.quic_leg_pkts
    }

    /// 因 QUIC 面不可用而丢掉的 kind=5 报文数（测试/观测面）。
    pub fn quic_leg_undelivered(&self) -> u64 {
        self.quic_leg_undelivered
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
}

/// 监听口被占用时的退让顺序：+1…+9，最后随机（Go listenWithFallback 同序）。
/// **M5 起这是 QUIC 端口的绑定面**（出口唯一公共端口；`serve.quic_listen` 缺省
/// `listen+1`，被占用 +1…+9 → 随机）。
pub(crate) fn listen_with_fallback_addr(port: u16, ip: Option<IpAddr>) -> std::io::Result<UdpSocket> {
    let try_one = |p: u16| -> std::io::Result<UdpSocket> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_logf() -> crate::Logf {
        Arc::new(|_| {})
    }

    /// kind=5 帧 ⇒ `LegInbound.quic` 承载（src = 腿远端、载荷字节原样）；**不**记新源行
    /// （Q-P：E23 对 QUIC 腿不适用）。
    #[test]
    fn quic_kind_frame_carries_leg_packet() {
        let mut b = ServerBind::new(noop_logf());
        let src: SocketAddr = "127.0.0.1:5101".parse().unwrap();
        let payload = b"\x40\x01\x02\x03quic-initial-bytes";
        let wire = legframe::frame_bytes(FrameKind::Quic, payload);
        let got = b.process_packet(&wire, src).expect("kind=5 应产出");
        assert_eq!(got.quic.src, src, "src = 腿远端（QUIC 眼里的对端地址）");
        assert_eq!(got.quic.payload, payload, "载荷字节原样（零解释）");
        assert_eq!(b.quic_leg_pkts(), 1, "收面计数 +1");
        assert!(
            !b.src_seen.contains(&src),
            "Q-P：kind=5 分支不得记新源行（E23 对 QUIC 腿不适用）"
        );
    }

    /// 对照：kind=3（中继控制帧）**记**新源行并把载荷原样交钩子；畸形腿帧容忍丢弃。
    #[test]
    fn relay_control_frame_dispatches_and_notes_src() {
        use std::sync::Mutex;
        let mut b = ServerBind::new(noop_logf());
        let src: SocketAddr = "127.0.0.1:5102".parse().unwrap();
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        b.set_on_leg_frame(Box::new(move |payload: &[u8], _src: SocketAddr| {
            sink.lock().unwrap().push(payload.to_vec());
        }));
        let wire = crate::relaywire::relay_reg_frame(b"hello-payload");
        assert!(b.process_packet(&wire, src).is_none(), "控制帧 = 内部消费");
        assert_eq!(seen.lock().unwrap().len(), 1, "载荷交钩子一次");
        assert_eq!(seen.lock().unwrap()[0], b"hello-payload".to_vec());
        assert!(b.src_seen.contains(&src), "kind=3 记新源行（E23 值域之一）");
        // 畸形腿帧（长度 <2——只有魔数；Go DecodeFrame 同判）：容忍丢弃
        let other: SocketAddr = "127.0.0.1:5103".parse().unwrap();
        assert!(b.process_packet(&[0xBB], other).is_none());
        assert!(b.src_seen.contains(&other), "畸形腿帧记新源行");
        // 非帧包（裸 WG / 垃圾）：记一行「非帧」形态后丢弃
        let mut raw = vec![1u8];
        raw.resize(148, 0);
        assert!(b.process_packet(&raw, other).is_none());
    }

    /// QUIC 面不可用（未起/已收工）时的丢面：计数 + 节流记行（首 3 + 每 100——不静默）。
    #[test]
    fn quic_leg_undelivered_is_counted_with_throttled_log() {
        let (logf, rx) = log_sink();
        let mut b = ServerBind::new(logf);
        for _ in 0..4 {
            b.note_quic_leg_undelivered(1400);
        }
        assert_eq!(b.quic_leg_undelivered(), 4, "四次全计数");
        let lines: Vec<String> = rx
            .try_iter()
            .filter(|l| l.contains("腿上的 QUIC 报文无法投递"))
            .collect();
        assert_eq!(lines.len(), 3, "首 3 次各一行（第 4 次被节流）：{lines:?}");
        assert!(lines[0].contains("已丢 1 个（最近 1400 字节）"), "行文可检索：{}", lines[0]);
    }

    /// 收集型日志（行级断言用）。
    fn log_sink() -> (crate::Logf, std::sync::mpsc::Receiver<String>) {
        let (tx, rx) = std::sync::mpsc::channel();
        (Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        }), rx)
    }

    /// 腿表读面：`leg_remotes` / `leg_send_handle`（QUIC 面差分同步的输入）。
    #[test]
    fn leg_send_handle_and_remotes_expose_leg_table() {
        let mut b = ServerBind::new(noop_logf());
        let r1: SocketAddr = "127.0.0.1:5103".parse().unwrap();
        b.register_leg(11, r1, b"LEGUP").expect("拨腿");
        assert_eq!(b.leg_remotes(), vec![r1]);
        let h = b.leg_send_handle(&r1).expect("发送句柄可得");
        assert_ne!(h.local_addr().unwrap().port(), 0, "句柄是同一 socket 的副本（本地端口相同）");
        assert!(
            b.leg_send_handle(&"127.0.0.1:5999".parse().unwrap()).is_none(),
            "不存在的腿 ⇒ None"
        );
        b.remove_leg(11);
        assert!(b.leg_remotes().is_empty(), "摘腿后集合空");
    }

    // ---------- 腿表（R4；评审 中-4 补测） ----------

    /// 同 id 重注册 = 先拆后建（中继重放语义）；同远端替换；上限只拦新 id（C3）。
    #[test]
    fn leg_table_register_replace_and_cap() {
        let mut b = ServerBind::new(noop_logf());
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
        for i in 3..=(RELAY_LEG_MAX as u64 + 1) {
            let r: SocketAddr = format!("127.0.0.1:{}", 6000 + i).parse().unwrap();
            assert!(b.register_leg(i, r, b"m").is_ok(), "id={i} 应入表");
        }
        assert_eq!(b.leg_fds().len(), RELAY_LEG_MAX, "应恰好满表（id2 + 62 新 id）");
        let extra: SocketAddr = "127.0.0.1:6999".parse().unwrap();
        assert!(matches!(
            b.register_leg(999, extra, b"m"),
            Err(crate::server::relayleg::LegError::LegCap(RELAY_LEG_MAX))
        ));
        assert!(b.register_leg(5, r1, b"m").is_ok(), "已有 id 替换不受上限拦");
        b.clear_legs();
        assert_eq!(b.leg_fds().len(), 0);
    }

    /// LEGUP 标记吞包防御（5B/37B）与正常帧分派。
    #[test]
    fn leg_readable_swallows_legup_markers() {
        let mut b = ServerBind::new(noop_logf());
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

    /// 端口退让：占用后 +1（真实 socket 面——QUIC 端口的绑定语义）。
    #[test]
    fn port_fallback_on_busy() {
        let holder = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = holder.local_addr().unwrap().port();
        let s = listen_with_fallback_addr(port, None).unwrap();
        assert_ne!(s.local_addr().unwrap().port(), port, "被占应退让");
    }
}
