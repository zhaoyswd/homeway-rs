//! wgcore：WG 数据面引擎（boringtun noise + 栈 B + wtransport Bind 的装配与驱动）。
//!
//! 对齐 Go 侧 `clientcore/internal/wgcore`（core.go + hub.go）的 R1 子集（设计文档
//! §1/§3/§4）：
//! - **单驱动线程**独占 Tunn + Interface + UDP socket（无锁热路径）；主线程经
//!   unbounded 命令通道投递请求、经各自 reply 通道收结果（三条死锁纪律：命令通道
//!   unbounded / WG 线程不阻塞在 channel / 不持锁跨等待）；
//! - **三唤醒源**：`poll(2)` on {UDP fd, self-pipe}，超时 = `min(poll_delay, 250ms)`
//!   （延迟 ACK 10ms 等栈定时器不迟到）；
//! - reg 搭车收口 = `Bind::send_wg`（四来源全覆盖）；expired → **一次性**重建 Tunn
//!   （新随机 index 前缀 <2^24）+ 补注册，**保采纳**（= Go 恢复阶梯 R1 档「补注册 +
//!   丢会话保采纳」的最小兜底——比设计文档 v2 登记的「丢采纳」更贴 Go，按实现修正；
//!   阶梯档位/节拍整体属 R2）；
//! - 静默丢包类 `WireGuardError` 计数继续，`ConnectionExpired` 才重建（评审 ②-9）；
//! - `decapsulate` 返回 `WriteToNetwork` 后以**空数据报重调**到 Done（冲掉握手期排队
//!   的内层包与握手响应 keepalive——评审 ③-7）。

use std::collections::HashMap;
use std::io::{self, Write as _};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, SocketAddrV4};
use std::os::fd::AsRawFd;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::StaticSecret;
use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::time::Instant as SmolInstant;

use crate::identity::Identity;
use crate::psk::Psk;
use crate::token::{PeerId, Secret};
use crate::tunnel_addr;
use crate::wtransport::{Bind, Candidate, RegCtx, Via};

pub mod stackb;

use self::stackb::{DialError, StackB};

/// TUN 面错误回调类型（fd 读写失败 → facade markUnhealthy；driver 线程执行）。
pub type OnTunError = Box<dyn Fn(&str) + Send>;

/// 出口隧道 IP 的契约常量（两端共同，dns-host-resolver 起钉死；非 token 派生）。
pub const SERVER_TUNNEL_IP: Ipv4Addr = Ipv4Addr::new(100, 64, 255, 1);
/// poll 等待上限（smoltcp poll_delay 的封顶；延迟 ACK 10ms 一类栈定时器的到点保障）。
const POLL_CAP: i32 = 250;
/// WG 网络包缓冲上界（握手 148 / 数据 = 明文 + 32B 开销）。
const WG_BUF: usize = 65536 + 148;
/// 连接的建立期限（engine 侧；主线程另有自己的 RPC 超时）。
#[cfg(not(test))]
const CONNECT_DEADLINE: Duration = Duration::from_secs(10);
#[cfg(test)]
const CONNECT_DEADLINE: Duration = Duration::from_millis(400);

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ConnErr {
    #[error("连接被拒（对端 RST）")]
    Refused,
    #[error("连接超时")]
    Timeout,
    #[error("连接已关闭")]
    Closed,
    #[error("通道已断（引擎收工）")]
    EngineGone,
    #[error(transparent)]
    Dial(#[from] DialError),
}

/// 主线程 → 驱动线程的命令（全部非阻塞投递；带 reply 的由驱动线程在事件到点时应答）。
pub enum Cmd {
    Connect {
        id: u64,
        dst: SocketAddrV4,
        /// 建立期限（引擎侧到点 abort——探测预算必须下沉引擎，caller 弃等会留残留 SYN
        /// 污染阶梯归因；R2 评审中-13）。
        deadline: Duration,
        reply: Sender<Result<(), ConnErr>>,
    },
    Write {
        id: u64,
        data: Vec<u8>,
        reply: Sender<Result<usize, ConnErr>>,
    },
    Read {
        id: u64,
        reply: Sender<Result<Vec<u8>, ConnErr>>,
    },
    /// 半关（FIN；对端仍可发）。
    Shutdown {
        id: u64,
        reply: Sender<Result<(), ConnErr>>,
    },
    Close {
        id: u64,
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 补注册（回执 = 是否真发出：false = bind 已收工或无采纳地址）。
    RefreshReg {
        reply: Option<Sender<bool>>,
    },
    /// 清采纳、重启赛跑（Go Rearm 家族的硬赛跑形态；候选重投由调用方经 SetCandidates 跟进）。
    Rearm {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 软赛跑（Go RearmSoft：中继立即参与——升直连/hint 打洞用，不停在用路径）。
    RearmSoft {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 装 hint 回调（回调在**驱动线程**执行——只允许内存操作/通道投递，严禁 RPC 回引擎）。
    /// 【测试缝】中继锁定（relay-lock 注入——见 wtransport::Bind::relay_only）。
    SetRelayOnly,
    SetOnHint {
        h: crate::wtransport::bind::OnHint,
    },
    /// 丢弃本地 WG 会话（Go ResetPeerSession 同义：peer 移除再写回 ≙ 重建 Tunn；
    /// **保采纳**）——下一发出站包全新握手。
    ResetPeerSession {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 换本地 UDP socket（Go Rebind 同义：不换 Identity、不动采纳）。
    Rebind {
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 更新候选集（学习缓存刷新后）。
    SetCandidates {
        cands: Vec<Candidate>,
    },
    /// UDP 拨号面（R3-3f：E12 出口 UDP 判据采样 + DNS 代答实测——Go 栈内 udp 的
    /// 最小客户端面）：Open = 栈内 bind（ephemeral 端口自管）；Send/Recv/Close。
    UdpOpen {
        id: u64,
        reply: Sender<Result<u16, ConnErr>>,
    },
    UdpSend {
        id: u64,
        dst: SocketAddrV4,
        data: Vec<u8>,
        reply: Sender<Result<(), ConnErr>>,
    },
    UdpRecv {
        id: u64,
        reply: Sender<Result<(Vec<u8>, SocketAddrV4), ConnErr>>,
    },
    UdpClose {
        id: u64,
        reply: Sender<Result<(), ConnErr>>,
    },
    /// 测试缝：把当前 UDP socket 置为已关形态（模拟冻结唤醒后 OS 作废 socket——
    /// R2 阶梯 R2 档注入；Go 集成测试注入假 transport 同义）。
    #[cfg(feature = "test-seams")]
    DebugPoisonSocket,
    /// L3 直通：注册应用 TUN 源（Go hub.AttachTUN——只允许一次；fd 所有权在扩展，
    /// 引擎只裸读写、**从不 close**，失效经错误回调暴露——坑 50）。
    TunAttach {
        fd: i32,
        mtu: u32,
        reply: Sender<Result<(), ConnErr>>,
    },
    /// TUN 读线程投来的应用出站包（driver encap 发送——Go hub 的 outbound 通道面）。
    TunPacket(Vec<u8>),
    /// 装 TUN 面错误回调（fd 读写失败 → facade 挂 markUnhealthy；attach 前后都可调，
    /// driver 线程执行——回调只允许原子/内存操作，Go SetOnTunError 同义）。
    SetOnTunError {
        f: OnTunError,
    },
    /// TUN 读线程报 fd 失效（driver 卸源 + 触发错误回调——与写失败同收口）。
    TunFdDead {
        msg: String,
    },
    Stop,
}

/// 引擎状态快照（主线程轮询；驱动线程独占写）。
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub via: Via,
    pub ep: Option<SocketAddr>,
    pub mirrored: u64,
    pub rx: u64,
    pub tx: u64,
    /// 最近一次**采纳路径**本地类发送错误时刻（巡检失败拍的噪声判定数据源；
    /// Go Bind.lastLocalSendErrAt 同义——镜像候选的本地错误不刷此位）。
    pub last_local_send_err: Option<Instant>,
    /// 全候选发送统计的**生命周期累计**（(尝试, 本地失败)；Bind 计数器单调，快照每
    /// 拍重写，Client::swap_send_stats 以差分等价 Go 的 swap-reset——复核 r3-F5 更正
    /// 注释：此前误写「消费后置 None」）。
    pub bind_stats: Option<(i64, i64)>,
    /// 本地发送错误累计（(采纳路径, 全部)——tunStatusJSON demand.localErr* 源；
    /// 拍板①：Go adoptedLocalErrCount/localErrCount 的累计面）。
    pub local_err: (u64, u64),
}

pub struct CoreConfig {
    pub peer_id: PeerId,
    pub secret: Secret,
    pub identity: Identity,
    pub candidates: Vec<Candidate>,
    pub logf: Arc<dyn Fn(&str) + Send + Sync>,
}

type UdpRecvReply = Result<(Vec<u8>, SocketAddrV4), ConnErr>;

/// TUN 面的共享计数（fd 字节对表 + 需求信号源；driver 与读线程两写、主线程读）。
#[derive(Debug, Default)]
pub struct TunCounters {
    /// 读（应用出站方向）累计字节——与设备 `vpn-tun` 的 TX 对表。
    pub read_bytes: AtomicU64,
    /// 写（应用入站方向）累计字节——与设备 `vpn-tun` 的 RX 对表。
    pub write_bytes: AtomicU64,
    /// App 出站包计数（demand-driven-recovery D1：巡检拍「取走清零」消费）。
    pub out_pkts: std::sync::atomic::AtomicI64,
    /// 最近出站包时刻（unix nano；0 = 本世代从未有过应用出站——tunStatusJSON 的
    /// demand.outboundAt 面）。
    pub last_outbound_ns: AtomicI64,
    /// 同一时刻的**单调相对读数**（自进程起点的 ns；0 = 从未）——下推器（D4）的
    /// 时基。评审 r2-H1：单调面与 unix 面分离（曾用 unix ns 换算 Instant，减出
    /// 56 年前的时刻 ⇒ should_push 恒 false）。
    pub last_outbound_mono_ns: AtomicI64,
}

/// 进程单调起点（last_outbound_mono_ns 的基准；懒初始化）。
fn process_mono_start() -> Instant {
    static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *START.get_or_init(Instant::now)
}

/// 引擎内的应用 TUN 源（L3 直通；Go hub 的 appTun 面收窄——栈 B 本就在 Engine 里，
/// hub 的「双源喂 device」= decap 分流 + TunPacket 命令两个动作）。
struct AppTun {
    fd: i32,
    /// 读线程停止位（Engine stop/重复 attach 时置位；线程分离不 join——阻塞在
    /// read 上，退出靠 fd 失效〔扩展 destroy〕或错误返回）。
    stop: Arc<AtomicBool>,
    counters: Arc<TunCounters>,
    /// fd 读写失败回调（facade 挂 markUnhealthy；driver 线程执行——只允许原子/内存操作）。
    on_error: Option<OnTunError>,
}

struct UdpConn {
    handle: SocketHandle,
    wait_recv: Option<Sender<UdpRecvReply>>,
}

struct Conn {
    handle: SocketHandle,
    syn_sent: bool,
    local_aborted: bool,
    established: bool,
    deadline: Instant,
    wait_est: Option<Sender<Result<(), ConnErr>>>,
    wait_read: Option<Sender<Result<Vec<u8>, ConnErr>>>,
}

fn make_tunn(identity_key: &StaticSecret, secret: &Secret, peer_pub: &[u8; 32]) -> Tunn {
    Tunn::new(
        identity_key.clone(),
        boringtun::x25519::PublicKey::from(*peer_pub),
        Some(*Psk::from(*secret).as_bytes()),
        None, // persistent_keepalive：对齐 Go 客户端（不设；保活 = probe 拍）
        rand_index(),
        None, // rate_limiter 勿改——Some 会让客户端对出口握手应答回 cookie（§2 勿改清单）
    )
    .expect("参数恒合法（dalek 钥/PSK 构造期已验）")
}

fn rand_index() -> u32 {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("系统随机源不可用");
    u32::from_le_bytes(b) & 0x00ff_ffff // < 2^24：Tunn 内部 <<8 丢高位（评审 ②-2）
}

/// 引擎主体（驱动线程内独占）。
struct Engine {
    bind: Bind,
    tunn: Tunn,
    stack: StackB,
    conns: HashMap<u64, Conn>,
    /// UDP 面：id → 栈内 socket + 待决读。
    udp: HashMap<u64, UdpConn>,
    wg_buf: Vec<u8>,
    cmd_rx: mpsc::Receiver<Cmd>,
    snapshot: Arc<Mutex<Snapshot>>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    identity_key: StaticSecret,
    secret: Secret,
    peer_id: PeerId,
    /// expired 重建执行位（一次性；避免每拍重建——update_timers 过期后每 tick 回错）。
    expired_pending: bool,
    silent_drops: u64,
    time0: Instant,
    /// L3 直通的应用 TUN 源（None = 未 attach——transit 回包丢弃，Go hub 同义）。
    app_tun: Option<AppTun>,
    /// TUN 计数（attach 前也可读——全零；detach/收尾后保留末值）。
    tun_counters: Arc<TunCounters>,
    /// 挂起的 TUN 错误回调（attach 前设置则存这里，attach 时搬进 AppTun）。
    on_tun_error: Option<OnTunError>,
    /// 投递通道（读线程投 TunPacket 用；Client::start 注入克隆）。
    cmd_tx: Option<Sender<Cmd>>,
    /// 装载的 TUN 读线程唤醒面（我-4：读线程投 TunPacket 后写同一 wake 管道——
    /// driver 的 poll(2) 最多 250ms 才醒，上行每串包白等一拍；与 Client::send 共用
    /// 同一 fd 与互斥位，stop 关闭后写入自然 no-op）。
    wake: Arc<Mutex<Option<i32>>>,
}

impl Engine {
    fn now_smol(&self) -> SmolInstant {
        SmolInstant::from_millis(self.time0.elapsed().as_millis() as i64)
    }

    /// 重建 Tunn（**唯一重建点**——expired 兜底与阶梯 ResetPeerSession 共用）：
    /// 同身份/同 secret/同 peer 的全新会话状态（新随机 index 前缀），**保采纳**
    /// （采纳/reg 都在 Bind，不受影响）。Go「peer 移除再写回」（core.go:228-246）的
    /// 单体等价物；排队内层包随旧 Tunn 丢弃（= Go flushStagedPackets）。
    fn rebuild_tunn(&mut self) {
        self.tunn = make_tunn(&self.identity_key, &self.secret, self.peer_id.as_bytes());
    }

    /// ConnectionExpired 的**一次性**重建（引擎兜底：boringtun 特有义务，wireguard-go
    /// 自管 rekey、Go 侧无对应物；**不产 RECOVER 行**，打自己的行）。置位防每拍重建
    /// （update_timers 过期后每 tick 回错）；明文包到达复位。
    fn rebuild_tunn_once(&mut self) {
        if self.expired_pending {
            return;
        }
        self.expired_pending = true;
        self.rebuild_tunn();
        if self.bind.adopted().is_some() {
            (self.logf)("wgcore: 会话过期已重建（丢会话保采纳）—— 补注册");
            self.bind.refresh_reg();
        } else {
            // 未采纳（无路径）：RREG 需要 adopted 会静默 no-op——改走 rearm 重武装 reg
            // 并主动触发一次出站（空载荷 encapsulate 无会话 ⇒ 产握手 init + 搭 reg，
            // Go 恢复阶梯 R1 档在「无路径」时刻的同义动作；评审中-1）
            (self.logf)("wgcore: 会话过期已重建（无采纳路径）—— 重赛跑 + 补注册");
            self.bind.rearm();
            self.encap_send(&[]);
        }
    }

    /// 单轮驱动：命令 → UDP 批量收 → 栈 poll → TX 出队封装 → 定时器 → 待决结算。
    /// 返回 false = 收到 Stop。
    fn pump_once(&mut self, udp_buf: &mut [u8]) -> bool {
        while let Ok(cmd) = self.cmd_rx.try_recv() {
            if !self.handle_cmd(cmd) {
                return false;
            }
        }
        self.bind.tick_unlock();
        self.drain_udp(udp_buf);
        let now = self.now_smol();
        self.stack
            .iface
            .poll(now, &mut self.stack.device, &mut self.stack.sockets);
        let mut tx: Vec<Vec<u8>> = Vec::new();
        self.stack.device.drain_tx(&mut tx);
        for pkt in &tx {
            self.encap_send(pkt);
        }
        self.timer_tick();
        self.resolve_pending();
        self.resolve_udp();
        self.update_snapshot();
        true
    }

    fn drain_udp(&mut self, buf: &mut [u8]) {
        loop {
            match self.bind.recv_from(buf) {
                Ok(Some(n)) => {
                    let src_ip = self.bind.adopted().map(|a| match a {
                        SocketAddr::V4(v4) => IpAddr::V4(*v4.ip()),
                        SocketAddr::V6(v6) => IpAddr::V6(*v6.ip()),
                    });
                    self.decapsulate_in(src_ip, &buf[..n]);
                }
                Ok(None) => continue,
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    return;
                }
                Err(_) => {
                    self.silent_drops += 1;
                    return;
                }
            }
        }
    }

    /// decapsulate + 空数据报重调协议（WriteToNetwork 后以空输入重调到 Done）。
    fn decapsulate_in(&mut self, src_ip: Option<IpAddr>, datagram: &[u8]) {
        let src_ip = src_ip.unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        // 热路径：容量恒足（构造时 WG_BUF 一次分配）；boringtun 只写前缀——
        // 不 clear/resize（每包 65KB memset 纯浪费，中-10①）。长度由返回值给。
        match self
            .tunn
            .decapsulate(Some(src_ip), datagram, &mut self.wg_buf)
        {
            TunnResult::WriteToNetwork(w) => {
                self.bind.send_wg(w);
                loop {
                    self.wg_buf.clear();
                    self.wg_buf.resize(WG_BUF, 0);
                    match self.tunn.decapsulate(Some(src_ip), &[], &mut self.wg_buf) {
                        TunnResult::WriteToNetwork(w2) => self.bind.send_wg(w2),
                        TunnResult::Err(WireGuardError::ConnectionExpired) => {
                            self.rebuild_tunn_once();
                            break;
                        }
                        TunnResult::Err(_) => {
                            self.silent_drops += 1;
                            break;
                        }
                        _ => break,
                    }
                }
            }
            TunnResult::WriteToTunnelV4(pkt, _) => {
                self.expired_pending = false; // 明文包到达 = 会话活着
                                              // L3 直通分流（Go hub.Write）：dst == B 的本地地址（派生隧道 IP）→ 栈 B
                                              // （核心自连回程），其余 → 真 TUN fd（内核投给应用）。
                if pkt.len() >= 20
                    && pkt[0] >> 4 == 4
                    && pkt[16..20] == self.stack.tunnel_ip.octets()
                {
                    self.stack.inject(pkt);
                } else if let Some(tun) = self.app_tun.as_ref() {
                    match write_fd_all(tun.fd, pkt) {
                        Ok(()) => {
                            tun.counters
                                .write_bytes
                                .fetch_add(pkt.len() as u64, Ordering::Relaxed);
                        }
                        Err(e) => {
                            let msg = format!("tun fd 写入失败：{e}");
                            (self.logf)(&msg);
                            // 卸源 + 置读线程停止位（评审 r2-我-5/评审者「不认同②」：
                            // 停止位在 AppTun 里——直接 `= None` 会把引擎侧唯一能置位
                            // 的句柄一起丢掉。先置 stop 再卸源；错误回调同拍发出
                            // 〔markUnhealthy_if_current("fd") → 扩展重建〕）。
                            if let Some(tun) = self.app_tun.take() {
                                tun.stop.store(true, Ordering::SeqCst);
                                if let Some(cb) = tun.on_error.as_ref() {
                                    cb(&msg);
                                }
                            }
                        }
                    }
                }
                // 未 attach：transit 回包不该出现（还没有应用流量）；丢弃（Go 同义）
            }
            TunnResult::WriteToTunnelV6(_, _) => {} // 内层只承载 IPv4（D4）
            TunnResult::Done => {}
            TunnResult::Err(WireGuardError::ConnectionExpired) => self.rebuild_tunn_once(),
            TunnResult::Err(_) => {
                self.silent_drops += 1; // 静默丢包类：计数继续（评审 ②-9）
            }
        }
    }

    fn encap_send(&mut self, pkt: &[u8]) {
        // 热路径：容量恒足（构造时 WG_BUF 一次分配）；boringtun 只写前缀——
        // 不 clear/resize（每包 65KB memset 纯浪费，中-10①）。长度由返回值给。
        match self.tunn.encapsulate(pkt, &mut self.wg_buf) {
            TunnResult::WriteToNetwork(w) => self.bind.send_wg(w),
            TunnResult::Err(WireGuardError::ConnectionExpired) => self.rebuild_tunn_once(),
            TunnResult::Err(_) => {
                self.silent_drops += 1;
            }
            _ => {}
        }
    }

    fn timer_tick(&mut self) {
        // 热路径：容量恒足（构造时 WG_BUF 一次分配）；boringtun 只写前缀——
        // 不 clear/resize（每包 65KB memset 纯浪费，中-10①）。长度由返回值给。
        match self.tunn.update_timers(&mut self.wg_buf) {
            TunnResult::WriteToNetwork(w) => self.bind.send_wg(w),
            TunnResult::Err(WireGuardError::ConnectionExpired) => self.rebuild_tunn_once(),
            TunnResult::Err(_) => {
                self.silent_drops += 1;
            }
            _ => {}
        }
    }

    /// UDP 面：栈内 bind（ephemeral；回环拒绝——环回不进隧道与 TCP 同口径）。
    fn start_udp(&mut self, id: u64) -> Result<u16, ConnErr> {
        use smoltcp::socket::udp;
        let port = self.stack.alloc_udp_port();
        let rx_meta: Vec<udp::PacketMetadata> =
            (0..16).map(|_| udp::PacketMetadata::EMPTY).collect();
        let tx_meta: Vec<udp::PacketMetadata> =
            (0..16).map(|_| udp::PacketMetadata::EMPTY).collect();
        let mut sock = udp::Socket::new(
            udp::PacketBuffer::new(rx_meta, vec![0u8; 64 * 1024]),
            udp::PacketBuffer::new(tx_meta, vec![0u8; 64 * 1024]),
        );
        let ep = smoltcp::wire::IpEndpoint::new(self.stack.tunnel_ip.into(), port);
        sock.bind(ep)
            .map_err(|e| ConnErr::Dial(DialError::Stack(format!("{e:?}"))))?;
        let h = self.stack.sockets.add(sock);
        self.udp.insert(
            id,
            UdpConn {
                handle: h,
                wait_recv: None,
            },
        );
        Ok(port)
    }

    fn udp_send(&mut self, id: u64, dst: SocketAddrV4, data: &[u8]) -> Result<(), ConnErr> {
        if dst.ip().is_loopback() {
            return Err(ConnErr::Dial(DialError::LoopbackRejected));
        }
        let Some(u) = self.udp.get(&id) else {
            return Err(ConnErr::Closed);
        };
        let ep = smoltcp::wire::IpEndpoint::new((*dst.ip()).into(), dst.port());
        let sock = self
            .stack
            .sockets
            .get_mut::<smoltcp::socket::udp::Socket>(u.handle);
        sock.send_slice(data, ep).map_err(|_| ConnErr::Closed)?;
        Ok(())
    }

    /// UDP 读结算（resolve_pending 的伙伴面）。
    fn resolve_udp(&mut self) {
        let ids: Vec<u64> = self.udp.keys().copied().collect();
        for id in ids {
            let Some(u) = self.udp.get_mut(&id) else {
                continue;
            };
            if u.wait_recv.is_none() {
                continue;
            }
            let handle = u.handle;
            let mut buf = vec![0u8; 65535];
            let got = self
                .stack
                .sockets
                .get_mut::<smoltcp::socket::udp::Socket>(handle)
                .recv_slice(&mut buf);
            match got {
                Ok((n, meta)) if n > 0 => {
                    buf.truncate(n);
                    let from = match meta.endpoint.addr {
                        smoltcp::wire::IpAddress::Ipv4(a) => Ipv4Addr::from(a.0),
                        _ => continue,
                    };
                    let tx = self.udp.get_mut(&id).and_then(|u| u.wait_recv.take());
                    if let Some(tx) = tx {
                        let _ = tx.send(Ok((buf, SocketAddrV4::new(from, meta.endpoint.port))));
                    }
                }
                _ => {} // 无包：继续等
            }
        }
    }

    /// 返回 false = 收到 Stop。
    fn handle_cmd(&mut self, cmd: Cmd) -> bool {
        match cmd {
            Cmd::Connect {
                id,
                dst,
                deadline,
                reply,
            } => match self.start_conn(id, dst, deadline) {
                Ok(()) => {
                    if let Some(c) = self.conns.get_mut(&id) {
                        c.wait_est = Some(reply);
                    }
                }
                Err(e) => {
                    let _ = reply.send(Err(e));
                }
            },
            Cmd::Write { id, data, reply } => {
                let r = match self.conns.get(&id) {
                    Some(c) => {
                        let sock = self.stack.sockets.get_mut::<TcpSocket>(c.handle);
                        let r = sock.send_slice(&data).map_err(|_| ConnErr::Closed);
                        if r.is_err() && std::env::var_os("HOMEWAY_WG_DEBUG").is_some() {
                            eprintln!(
                                "[wr-debug] write 失败 id={id} state={:?} may_send={} local={:?}",
                                sock.state(),
                                sock.may_send(),
                                sock.local_endpoint()
                            );
                        }
                        r
                    }
                    None => Err(ConnErr::Closed),
                };
                let _ = reply.send(r);
            }
            Cmd::Read { id, reply } => match self.conns.get_mut(&id) {
                Some(c) => c.wait_read = Some(reply),
                None => {
                    let _ = reply.send(Err(ConnErr::Closed));
                }
            },
            Cmd::Shutdown { id, reply } => {
                let r = match self.conns.get(&id) {
                    Some(c) => {
                        self.stack.sockets.get_mut::<TcpSocket>(c.handle).close();
                        Ok(())
                    }
                    None => Err(ConnErr::Closed),
                };
                let _ = reply.send(r);
            }
            Cmd::Close { id, reply } => {
                let r = match self.conns.get_mut(&id) {
                    Some(c) => {
                        self.stack.sockets.get_mut::<TcpSocket>(c.handle).abort();
                        c.local_aborted = true;
                        Ok(())
                    }
                    None => Err(ConnErr::Closed),
                };
                let _ = reply.send(r);
            }
            Cmd::UdpOpen { id, reply } => {
                let r = self.start_udp(id);
                let _ = reply.send(r);
            }
            Cmd::UdpSend {
                id,
                dst,
                data,
                reply,
            } => {
                let r = self.udp_send(id, dst, &data);
                let _ = reply.send(r);
            }
            Cmd::UdpRecv { id, reply } => match self.udp.get_mut(&id) {
                Some(u) => u.wait_recv = Some(reply),
                None => {
                    let _ = reply.send(Err(ConnErr::Closed));
                }
            },
            Cmd::UdpClose { id, reply } => {
                if let Some(u) = self.udp.remove(&id) {
                    self.stack.sockets.remove(u.handle);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::RefreshReg { reply } => {
                let sent = self.bind.refresh_reg();
                if let Some(tx) = reply {
                    let _ = tx.send(sent);
                }
            }
            Cmd::RearmSoft { reply } => {
                self.bind.rearm_soft();
                if self.bind.adopted().is_none() {
                    self.encap_send(&[]);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::SetRelayOnly => self.bind.set_relay_only(),
            Cmd::SetOnHint { h } => {
                self.bind.set_on_hint(h);
            }
            Cmd::Rearm { reply } => {
                self.bind.rearm();
                // 无采纳路径的出站触发：空载荷 encapsulate 无会话 ⇒ 产握手 init + 搭 reg
                // （评审中-1 的 rearm 语义；有采纳时 rearmed reg 由下一出站包搭车/独立补发）
                if self.bind.adopted().is_none() {
                    self.encap_send(&[]);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::ResetPeerSession { reply } => {
                // 阶梯 R1 档动作：重建会话（保采纳）+ 清 expired 位（重建即消化过期事实，
                // 引擎兜底路径复位）+ 判据行同串（core.go:244）。
                self.rebuild_tunn();
                self.expired_pending = false;
                (self.logf)("wgcore: 已丢弃本地会话（peer 移除并写回）—— 下一发出站包将全新握手");
                let _ = reply.send(Ok(()));
            }
            Cmd::Rebind { reply } => {
                let r = self
                    .bind
                    .rebind()
                    .map(|_| ())
                    .map_err(|e| ConnErr::Dial(DialError::Stack(e.to_string())));
                let _ = reply.send(r);
            }
            Cmd::SetCandidates { cands } => {
                self.bind.set_candidates(cands);
            }
            #[cfg(feature = "test-seams")]
            Cmd::DebugPoisonSocket => {
                self.bind.poison_socket_for_test();
            }
            Cmd::TunAttach { fd, mtu, reply } => {
                let r = self.attach_tun(fd, mtu);
                let _ = reply.send(r);
            }
            Cmd::TunPacket(pkt) => {
                // 应用出站包：encap 后经 bind 发出（Go hub 的 outbound → device.Read 面）
                self.encap_send(&pkt);
            }
            Cmd::SetOnTunError { f } => {
                if let Some(tun) = self.app_tun.as_mut() {
                    tun.on_error = Some(f);
                } else {
                    self.on_tun_error = Some(f);
                }
            }
            Cmd::TunFdDead { msg } => {
                if let Some(tun) = self.app_tun.take() {
                    if let Some(cb) = tun.on_error.as_ref() {
                        cb(&msg);
                    }
                }
            }
            Cmd::Stop => return false,
        }
        true
    }

    /// L3 直通 attach（Go hub.AttachTUN）：注册应用 TUN 源 + 起读线程。fd 所有权在
    /// 扩展（坑 50）——引擎裸读写、从不 close；读线程退出靠 fd 失效（错误回调）或
    /// 引擎收工（stop 位 + fd 随扩展 destroy 报错）。重复 attach = 拒绝（保会话不断）。
    fn attach_tun(&mut self, fd: i32, mtu: u32) -> Result<(), ConnErr> {
        if self.app_tun.is_some() {
            return Err(ConnErr::Closed);
        }
        let stop = Arc::new(AtomicBool::new(false));
        let counters = Arc::clone(&self.tun_counters);
        let on_error = self.on_tun_error.take();
        (self.logf)(&format!(
            "wgcore: 应用面就绪（TUN fd={fd} 已接上，mtu={mtu}，transit 直通）"
        ));
        self.app_tun = Some(AppTun {
            fd,
            stop: Arc::clone(&stop),
            counters: Arc::clone(&counters),
            on_error,
        });
        let cmd_tx = self.cmd_tx_clone();
        let logf = Arc::clone(&self.logf);
        let wake = Arc::clone(&self.wake);
        let _ = thread::Builder::new()
            .name("homeway-tun-read".into())
            .spawn(move || tun_read_loop(fd, cmd_tx, wake, stop, counters, logf));
        Ok(())
    }

    fn cmd_tx_clone(&self) -> Sender<Cmd> {
        // Engine 的投递通道（构造期由 Client::start 注入的克隆；读线程投 TunPacket 用）
        self.cmd_tx.clone().expect("cmd_tx 由 Client::start 注入")
    }

    fn start_conn(
        &mut self,
        id: u64,
        dst: SocketAddrV4,
        deadline: Duration,
    ) -> Result<(), ConnErr> {
        let handle = self.stack.connect(dst)?;
        self.conns.insert(
            id,
            Conn {
                handle,
                syn_sent: false,
                local_aborted: false,
                established: false,
                deadline: Instant::now() + deadline,
                wait_est: None,
                wait_read: None,
            },
        );
        Ok(())
    }

    /// 每轮结算：建立/超时/refused/读数据/EOF + Closed 槽位回收。
    fn resolve_pending(&mut self) {
        let now = Instant::now();
        let ids: Vec<u64> = self.conns.keys().copied().collect();
        type EstReply = (Sender<Result<(), ConnErr>>, Result<(), ConnErr>);
        type ReadReply = (Sender<Result<Vec<u8>, ConnErr>>, Result<Vec<u8>, ConnErr>);
        let mut est: Vec<EstReply> = Vec::new();
        let mut reads: Vec<ReadReply> = Vec::new();
        let mut reap: Vec<SocketHandle> = Vec::new();

        for id in ids {
            let Some(c) = self.conns.get_mut(&id) else {
                continue;
            };
            let (state, can_recv, may_recv, is_active) = {
                let s = self.stack.sockets.get_mut::<TcpSocket>(c.handle);
                (s.state(), s.can_recv(), s.may_recv(), s.is_active())
            };
            if state == tcp::State::SynSent || state == tcp::State::SynReceived {
                c.syn_sent = true;
            }
            // 建立
            if !c.established && state == tcp::State::Established {
                c.established = true;
                if let Some(tx) = c.wait_est.take() {
                    est.push((tx, Ok(())));
                }
            }
            // 超时（未建立且过期限）：本地 abort（断 SYN 重传）+ Timeout
            if !c.established && now > c.deadline {
                self.stack.sockets.get_mut::<TcpSocket>(c.handle).abort();
                c.local_aborted = true;
                if let Some(tx) = c.wait_est.take() {
                    est.push((tx, Err(ConnErr::Timeout)));
                }
            }
            // refused：SynSent→Closed 且非本地 abort/超时打断 = 对端 RST（会话活着）
            if !c.established
                && c.syn_sent
                && state == tcp::State::Closed
                && !c.local_aborted
                && c.wait_est.is_some()
            {
                let tx = c.wait_est.take().unwrap();
                est.push((tx, Err(ConnErr::Refused)));
            }
            // 读结算
            if c.wait_read.is_some() {
                if can_recv {
                    let mut buf = vec![0u8; 64 * 1024];
                    let n = self
                        .stack
                        .sockets
                        .get_mut::<TcpSocket>(c.handle)
                        .recv_slice(&mut buf)
                        .unwrap_or(0);
                    if n > 0 {
                        buf.truncate(n);
                        reads.push((c.wait_read.take().unwrap(), Ok(buf)));
                    }
                } else if !may_recv || !is_active {
                    // EOF：对端已关写半（FIN ⇒ may_recv=false，socket 停在 CloseWait 但
                    // is_active 仍真——只看 is_active 会把 EOF 判成永久等待）或整连接已关
                    reads.push((c.wait_read.take().unwrap(), Err(ConnErr::Closed)));
                }
            }
            // 对端半关、缓冲已排空且无待决读：本地补 FIN 推进到 Closed（槽位回收前提；
            // smoltcp 语义——CloseWait 不会自发迁移，应用层看到 EOF 后须 close。
            // **必须等缓冲排空**：数据+FIN 先于读命令到达时，缓冲里的数据尚未消费）
            if !may_recv && !can_recv && state == tcp::State::CloseWait && c.wait_read.is_none() {
                self.stack.sockets.get_mut::<TcpSocket>(c.handle).close();
            }
            // 彻底关、缓冲排空且无待决：回收槽位（TIME_WAIT 由 poll 推进至 Closed 后
            // 回收，评审 ③-8；can_recv 兜「Closed 但缓冲还有数据」的窗口）
            if state == tcp::State::Closed
                && !can_recv
                && c.wait_est.is_none()
                && c.wait_read.is_none()
            {
                reap.push(c.handle);
            }
        }

        for (tx, r) in est {
            let _ = tx.send(r);
        }
        for (tx, r) in reads {
            let _ = tx.send(r);
        }
        if !reap.is_empty() {
            for h in &reap {
                self.stack.sockets.remove(*h);
            }
            self.conns.retain(|_, c| !reap.contains(&c.handle));
        }
    }

    fn update_snapshot(&self) {
        let st = self.bind.status();
        let (rx, tx) = self.bind.rx_tx();
        let (tries, fails) = self.bind.send_stats_pending();
        let local_err = self.bind.local_err_counters();
        let mut s = self.snapshot.lock().expect("快照锁中毒");
        s.via = st.via;
        s.ep = st.ep;
        s.mirrored = st.mirrored;
        s.rx = rx;
        s.tx = tx;
        s.last_local_send_err = self.bind.last_local_send_err_at();
        s.bind_stats = Some((tries, fails));
        s.local_err = local_err;
    }
}

/// 客户端句柄（主线程/拨号线程共享面）：命令投递 + 状态轮询 + 收工（幂等，`&self`——
/// 经 `Arc<Client>` 共享时也能收口）。
pub struct Client {
    cmd_tx: mpsc::Sender<Cmd>,
    /// wake 管道写端（与引擎内 TUN 读线程共享同一把锁位——我-4；stop 取出后关闭）。
    wake_wr: Arc<Mutex<Option<i32>>>,
    handle: Mutex<Option<JoinHandle<()>>>,
    snapshot: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
    pub tunnel_ip: Ipv4Addr,
    /// TUN 面计数（L3 直通 attach 后与设备 vpn-tun 对表；需求信号源）。
    tun_counters: Arc<TunCounters>,
    /// 发送统计的 swap 基线（拍板①：差分等价 Go 的 swap-reset）。
    last_send_swap: Mutex<(i64, i64)>,
}

impl Client {
    /// 装配 + 起驱动线程（C2 判据行在此打出）。
    pub fn start(cfg: CoreConfig) -> io::Result<Self> {
        let pubkey = cfg.identity.public_key();
        let tunnel_ip = tunnel_addr::derive_tunnel_ip(&cfg.secret, &pubkey);
        (cfg.logf)(&format!(
            "wgcore: 隧道侧就绪（L3 直通；隧道地址 {tunnel_ip}，后端隧道 IP {SERVER_TUNNEL_IP}，核心自连经 B 拨隧道 IP）"
        ));

        let bind = Bind::open(
            &cfg.candidates,
            Some(RegCtx {
                secret: cfg.secret,
                pubkey,
                dev_tag: *cfg.identity.dev_tag().as_bytes(),
            }),
            Some(Duration::ZERO), // 直连优先窗口取缺省 2s（Go directFirst 0→2s 同义）
            cfg.peer_id.as_bytes(),
            Arc::clone(&cfg.logf),
        )?;

        let mut fds = [0i32; 2];
        if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let (wake_r, wake_w) = (fds[0], fds[1]);
        unsafe {
            libc::fcntl(wake_r, libc::F_SETFL, libc::O_NONBLOCK);
            libc::fcntl(wake_w, libc::F_SETFL, libc::O_NONBLOCK);
        }

        let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
        let snapshot = Arc::new(Mutex::new(Snapshot::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let next_id = Arc::new(AtomicU64::new(1));
        // wake 写端共享面（Client::send 与 TUN 读线程共用——我-4：读线程投包后
        // 即写管道唤醒 driver，不等 poll 超时拍）
        let wake_wr_shared = Arc::new(Mutex::new(Some(wake_w)));

        let identity_key = cfg.identity.private_key().clone();
        let secret = cfg.secret;
        let tunn = make_tunn(&identity_key, &secret, cfg.peer_id.as_bytes());
        let tun_counters = Arc::new(TunCounters::default());
        let engine = Engine {
            bind,
            tunn,
            stack: StackB::new(tunnel_ip, SERVER_TUNNEL_IP, SmolInstant::from_millis(0)),
            conns: HashMap::new(),
            udp: HashMap::new(),
            wg_buf: vec![0u8; WG_BUF],
            cmd_rx,
            snapshot: Arc::clone(&snapshot),
            logf: cfg.logf,
            identity_key,
            secret,
            peer_id: cfg.peer_id,
            expired_pending: false,
            silent_drops: 0,
            time0: Instant::now(),
            app_tun: None,
            tun_counters: Arc::clone(&tun_counters),
            on_tun_error: None,
            cmd_tx: Some(cmd_tx.clone()),
            wake: Arc::clone(&wake_wr_shared),
        };

        let stop2 = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("homeway-wg".into())
            .spawn(move || driver(engine, wake_r, stop2))?;

        Ok(Self {
            cmd_tx,
            wake_wr: wake_wr_shared,
            handle: Mutex::new(Some(handle)),
            snapshot,
            stop,
            next_id,
            tunnel_ip,
            tun_counters,
            last_send_swap: Mutex::new((0, 0)),
        })
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn send(&self, cmd: Cmd) {
        if self.cmd_tx.send(cmd).is_ok() {
            if let Some(fd) = *self.wake_wr.lock().expect("wake 锁中毒") {
                unsafe {
                    libc::write(fd, b"x".as_ptr().cast(), 1);
                }
            }
        }
    }
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().expect("快照锁中毒").clone()
    }

    /// 建连（阻塞到 Established / refused / 超时；默认期限）。
    pub fn connect(&self, dst: SocketAddrV4) -> Result<u64, ConnErr> {
        self.connect_deadline(dst, CONNECT_DEADLINE)
    }

    /// 建连（显式期限——阶梯探测/PathProbe 预算用；引擎侧到点 abort）。
    pub fn connect_deadline(&self, dst: SocketAddrV4, deadline: Duration) -> Result<u64, ConnErr> {
        let id = self.alloc_id();
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Connect {
            id,
            dst,
            deadline,
            reply: tx,
        });
        rx.recv().map_err(|_| ConnErr::EngineGone)??;
        Ok(id)
    }

    /// PathProbe：拨出口必然拒绝的端口（主入口恒 :1），拿到 RST = 隧道通、出口在、
    /// 拦截层可用（C8 `判据=wg` 的依据；超时/不可达才算死）。预算下沉引擎。
    /// UDP 面（R3-3f 测试判据用）：栈内 bind，返回 (id, 本地端口)。
    pub fn udp_open(&self) -> Result<(u64, u16), ConnErr> {
        let id = self.alloc_id();
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpOpen { id, reply: tx });
        rx.recv()
            .map_err(|_| ConnErr::EngineGone)?
            .map(|port| (id, port))
    }

    pub fn udp_send(&self, id: u64, dst: SocketAddrV4, data: Vec<u8>) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpSend {
            id,
            dst,
            data,
            reply: tx,
        });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 阻塞收一包（无超时——调用方自管整体预算；会话收工 = Closed）。
    pub fn udp_recv(&self, id: u64) -> Result<(Vec<u8>, SocketAddrV4), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpRecv { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    pub fn udp_close(&self, id: u64) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpClose { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// PathProbe：拨出口必然拒绝的端口（主入口恒 :1），拿到 RST = 隧道通、出口在、
    /// 拦截层可用（C8 `判据=wg` 的依据；超时/不可达才算死）。预算下沉引擎 + **外层
    /// 硬超时**（评审 r2-M4：引擎 driver 线程卡死时 rx.recv() 永不返回 ⇒ 暖机永久
    /// preparing/巡检线程永久卡死——Go 暖机 select+time.After(warmTimeout)、巡检
    /// time.After(perTry+2s) 的双保险同义：预算 + 2s 宽限后按超时收）。
    pub fn path_probe(&self, timeout: Duration) -> Result<(), ConnErr> {
        let id = self.alloc_id();
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Connect {
            id,
            dst: SocketAddrV4::new(SERVER_TUNNEL_IP, 1),
            deadline: timeout,
            reply: tx,
        });
        const SLACK: Duration = Duration::from_secs(2);
        match rx.recv_timeout(timeout + SLACK) {
            Ok(Ok(())) => {
                // :1 真有服务（连接成功）也说明会话活着——关掉探针连接防引擎内槽位滞留
                let _ = self.close(id);
                Ok(())
            }
            Ok(Err(ConnErr::Refused)) => Ok(()),
            Ok(Err(other)) => Err(other),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ConnErr::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ConnErr::EngineGone),
        }
    }

    pub fn write(&self, id: u64, data: Vec<u8>) -> Result<usize, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Write {
            id,
            data,
            reply: tx,
        });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    pub fn read(&self, id: u64) -> Result<Vec<u8>, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Read { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    pub fn shutdown(&self, id: u64) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Shutdown { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    pub fn close(&self, id: u64) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Close { id, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 补注册（fire-and-forget：patrol 暖机/周期刷新用）。
    pub fn refresh_reg(&self) {
        self.send(Cmd::RefreshReg { reply: None });
    }

    /// 补注册并回执（阶梯 R1 档要区分「发出/未发出」）。
    pub fn refresh_reg_result(&self) -> Result<bool, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RefreshReg { reply: Some(tx) });
        rx.recv().map_err(|_| ConnErr::EngineGone)
    }

    /// 全候选发送统计的「取走清零」面（拍板①：Go SwapSendStats——巡检拍头消费；
    /// 返回 (尝试数, 本地失败数) = 本次快照累计 − 上次取走值（Bind 计数器随引擎
    /// 生命周期单调，差分等价 Go 的 swap-reset）。
    pub fn swap_send_stats(&self) -> (i64, i64) {
        let cur = self
            .snapshot
            .lock()
            .expect("快照锁中毒")
            .bind_stats
            .unwrap_or((0, 0));
        let mut last = self.last_send_swap.lock().expect("发送统计基线锁中毒");
        let delta = (cur.0 - last.0, cur.1 - last.1);
        *last = cur;
        delta
    }

    /// 本地发送错误累计（tunStatusJSON demand.localErr* 两键源；Go
    /// adoptedLocalErrCount/localErrCount——拍板①补全）。
    pub fn local_err_counters(&self) -> (u64, u64) {
        self.snapshot.lock().expect("快照锁中毒").local_err
    }

    /// 清采纳、重启赛跑（阶梯 R3 档动作）。
    pub fn rearm(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rearm { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 软赛跑（中继立即参与；升直连/hint 打洞用）。
    pub fn rearm_soft(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RearmSoft { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 装 hint 回调（回调在驱动线程执行——只做内存操作/通道投递）。
    pub fn set_on_hint(&self, h: crate::wtransport::bind::OnHint) {
        self.send(Cmd::SetOnHint { h });
    }

    /// 【测试缝】中继锁定（relay-lock 注入）——须在首包发出前设置（连接前的装配窗口）。
    pub fn set_relay_only(&self) {
        self.send(Cmd::SetRelayOnly);
    }

    /// 档位动作的**有界** RPC（阶梯动作预算 2s：引擎卡住时按超时收轮，不让阶梯
    /// 无限等——Go runBoundedAction 的 Rust 对应；引擎正常时节拍 ≤250ms 即回）。
    pub fn reset_peer_session_bounded(&self, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::ResetPeerSession { reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    pub fn rebind_bounded(&self, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rebind { reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    pub fn rearm_bounded(&self, d: Duration) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rearm { reply: tx });
        rx.recv_timeout(d).map_err(|_| ConnErr::Timeout)?
    }

    pub fn refresh_reg_bounded(&self, d: Duration) -> Result<bool, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::RefreshReg { reply: Some(tx) });
        match rx.recv_timeout(d) {
            Ok(v) => Ok(v),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ConnErr::Timeout),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ConnErr::EngineGone),
        }
    }

    /// 丢弃本地 WG 会话（阶梯 R1 档动作；保采纳）。
    pub fn reset_peer_session(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::ResetPeerSession { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 换本地 socket（阶梯 R2 档动作；保采纳）。
    pub fn rebind(&self) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Rebind { reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 更新候选集（学习缓存刷新后；R2 形态：直连地址集）。
    pub fn set_candidates(&self, cands: Vec<Candidate>) {
        self.send(Cmd::SetCandidates { cands });
    }

    /// 测试缝：模拟冻结唤醒后 OS 作废 socket（阶梯 R2 档注入）。
    #[cfg(feature = "test-seams")]
    pub fn debug_poison_socket(&self) {
        self.send(Cmd::DebugPoisonSocket);
    }

    // ---- L3 直通面（Go hub.AttachTUN/FdStats/SwapOutboundPackets/LastOutboundAt）----

    /// 注册应用 TUN 源（两阶段启动的第二阶段；只允许一次——Err(Closed) = 已 attach）。
    /// fd 所有权在扩展：引擎裸读写、从不 close。
    pub fn attach_fd(&self, fd: i32, mtu: u32) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::TunAttach { fd, mtu, reply: tx });
        rx.recv().map_err(|_| ConnErr::EngineGone)?
    }

    /// 装 TUN 面错误回调（fd 读写失败 → 挂 markUnhealthy 的面）。
    pub fn set_on_tun_error(&self, f: Box<dyn Fn(&str) + Send>) {
        self.send(Cmd::SetOnTunError { f });
    }

    /// TUN fd 计数（read = 上行 / write = 下行；attach 前全零）。
    pub fn tun_stats(&self) -> (u64, u64) {
        let c = &self.tun_counters;
        (
            c.read_bytes.load(Ordering::Relaxed),
            c.write_bytes.load(Ordering::Relaxed),
        )
    }

    /// 取走并清零「自上次调用以来的 App 出站包数」（巡检拍消费：>0 = 本拍有真实需求）。
    pub fn swap_out_pkts(&self) -> i64 {
        self.tun_counters.out_pkts.swap(0, Ordering::Relaxed)
    }

    /// 最近一次 App 出站包的时刻（单调 Instant；None = 从未——Go LastOutboundAt
    /// 的零值形态）。**必须**用 `last_outbound_mono_ns`（单调相对读数）换算——
    /// 评审 r2-H1：曾用 unix epoch ns 换算，Instant 减出 56 年前 ⇒ D4 恒不触发。
    pub fn last_outbound_at(&self) -> Option<Instant> {
        let mono = self
            .tun_counters
            .last_outbound_mono_ns
            .load(Ordering::Relaxed);
        (mono != 0).then(|| process_mono_start() + Duration::from_nanos(mono as u64))
    }

    /// 最近出站的 unix 毫秒（tunStatusJSON demand.outboundAt 源；0 = 从未）。
    pub fn last_outbound_unix_ms(&self) -> i64 {
        self.tun_counters.last_outbound_ns.load(Ordering::Relaxed) / 1_000_000
    }

    /// 收工（幂等；Drop 同义——不显式 stop 也能停线程关 fd，评审中-11）。
    pub fn stop(&self) {
        if self.stop.swap(true, Ordering::SeqCst) {
            return; // 已收工（幂等，防 double-close fd）
        }
        self.send(Cmd::Stop);
        if let Some(h) = self.handle.lock().expect("join 锁中毒").take() {
            let _ = h.join();
        }
        if let Some(fd) = self.wake_wr.lock().expect("wake 锁中毒").take() {
            unsafe {
                libc::close(fd);
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 驱动循环：poll(2) 三唤醒源（UDP fd / self-pipe / 定时）。**UDP fd 每轮重取**
/// （Rebind 换 socket 后自动跟随——Go 接收循环「陈旧 socket 换新重试」的等价物）。
fn driver(mut engine: Engine, wake_r: i32, stop: Arc<AtomicBool>) {
    let mut udp_buf = Box::new([0u8; 65536]);
    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let udp_fd = engine.bind.socket().as_raw_fd();
        let timeout = {
            let now = engine.now_smol();
            let delay = engine
                .stack
                .iface
                .poll_delay(now, &engine.stack.sockets)
                .unwrap_or(smoltcp::time::Duration::from_millis(POLL_CAP as u64))
                .min(smoltcp::time::Duration::from_millis(POLL_CAP as u64));
            let mut ms = delay.total_millis().clamp(1, POLL_CAP as u64);
            // 读退避余量参与超时（持续 POLLERR 形态不空转——评审低-20）
            if let Some(r) = engine.bind.recv_backoff_remain() {
                ms = ms.min(r.as_millis().max(1) as u64);
            }
            ms as i32
        };
        let mut fds = [
            libc::pollfd {
                fd: udp_fd,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: wake_r,
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        unsafe {
            libc::poll(fds.as_mut_ptr(), 2, timeout);
        }
        if fds[1].revents & libc::POLLIN != 0 {
            let mut b = [0u8; 64];
            unsafe { while libc::read(wake_r, b.as_mut_ptr().cast(), 64) > 0 {} }
        }
        if !engine.pump_once(&mut udp_buf[..]) {
            break;
        }
    }
    // TUN 读线程收口（stop 位；线程若还阻塞在 read 上则等 fd 失效自然退出——
    // fd 所有权在扩展，引擎不 close）
    if let Some(tun) = engine.app_tun.take() {
        tun.stop.store(true, Ordering::SeqCst);
    }
    unsafe {
        libc::close(wake_r);
    }
    let _ = io::stdout().flush();
}

/// TUN fd 全量写（部分写回补——包写入原子性由内核 tun 语义保证，这里兜短写）。
/// EAGAIN：OHOS 的 VPN fd 是**非阻塞**的（Go tunfd_unix.go 同款真机实证）——
/// poll(POLLOUT) 等可写再续写。POLLOUT 等待有**总预算**（评审 r2-我-5：此前在
/// 唯一 driver 线程里无界重试——fd 长期不可写时 Cmd::Stop 处理不了、Client::stop
/// 的 join 挂死；超预算按超时错误收 ⇒ 卸源 + 停止位 + 错误回调）。
fn write_fd_all(fd: i32, mut buf: &[u8]) -> io::Result<()> {
    const WRITE_BUDGET: Duration = Duration::from_secs(5);
    let deadline = Instant::now() + WRITE_BUDGET;
    while !buf.is_empty() {
        let n = unsafe { libc::write(fd, buf.as_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => {
                    poll_fd(fd, libc::POLLOUT, deadline)?;
                    continue;
                }
                _ => return Err(e),
            }
        }
        buf = &buf[n as usize..];
    }
    Ok(())
}

/// 单 fd poll 等待（500ms 片——与 Go tunfd 的 poll 节拍同值；EINTR 重试；到总预算
/// 返回 TimedOut）。
fn poll_fd(fd: i32, events: i16, deadline: Instant) -> io::Result<()> {
    loop {
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "tun fd 等待可写超预算",
            ));
        }
        let mut pfd = libc::pollfd {
            fd,
            events,
            revents: 0,
        };
        let r = unsafe { libc::poll(&mut pfd, 1, 500) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(e);
        }
        return Ok(());
    }
}

/// TUN 读循环（应用出站方向）：裸 read → 计数 → 投 TunPacket 给 driver encap。
/// **OHOS 的 VPN fd 是非阻塞的**（Go tunfd_unix.go 真机实证——裸读立即返回
/// EAGAIN，被当错误上报会当场把健康隧道判死）：EAGAIN → poll(POLLIN, 500ms) →
/// 再读；stop 位在 poll 片界检查（引擎收工后 ≤~500ms 内退出）。
/// 投包后**写 wake 管道**（我-4：driver 的 poll 超时上限 250ms——不写管道时上行
/// 每串包最长排队一拍；Go 是 channel 直接唤醒 wireguard-go 读循环，无此延迟）。
/// 退出路径：fd 失效报错（EBADF/EINVAL = 扩展 destroy）/ stop 位 / channel 断
/// （driver 已死）。
fn tun_read_loop(
    fd: i32,
    cmd_tx: Sender<Cmd>,
    wake: Arc<Mutex<Option<i32>>>,
    stop: Arc<AtomicBool>,
    counters: Arc<TunCounters>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
) {
    let mut buf = vec![0u8; 65535];
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if n < 0 {
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::Interrupted => {
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                }
                io::ErrorKind::WouldBlock => {
                    // 非阻塞 fd 的常态：等 POLLIN（500ms 片——stop 位在片界检查）
                    if let Err(pe) = poll_fd(
                        fd,
                        libc::POLLIN,
                        Instant::now() + Duration::from_millis(500),
                    ) {
                        if pe.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        logf(&format!("tun fd poll 失败：{pe}（标记隧道不健康）"));
                        let _ = cmd_tx.send(Cmd::TunFdDead {
                            msg: format!("tun fd poll 失败：{pe}"),
                        });
                        return;
                    }
                    if stop.load(Ordering::SeqCst) {
                        return;
                    }
                }
                _ => {
                    logf(&format!("tun fd 读取失败：{e}（标记隧道不健康）"));
                    // 读失败即卸源（driver 侧同样处理写失败——两者都意味着 fd 已被收回）
                    let _ = cmd_tx.send(Cmd::TunFdDead {
                        msg: format!("tun fd 读取失败：{e}"),
                    });
                    return;
                }
            }
            continue;
        }
        if n == 0 {
            // n==0 不判死（评审 r2-我-8：Go 是 continue——TUN 设备的 0 字节读是可
            // 复现的空读形态，静默 return 会「看着健康、上行全丢」；随后 poll 一片
            // 防非阻塞 fd 上的热自旋）
            poll_fd(
                fd,
                libc::POLLIN,
                Instant::now() + Duration::from_millis(500),
            )
            .ok();
            continue;
        }
        let mut n = n as usize;
        // packet-info 头自动探测（评审 r2-L4 补齐：OHOS 不需要 PI，但读侧保留探测
        // 以防万一——Go tunfd_unix.go 同款判定）：4 字节 PI 后面跟合法 IP 版本号才剥。
        let looks_like_pi = (buf[2] == 0x08 && buf[3] == 0x00 && buf[4] >> 4 == 4)
            || (buf[2] == 0x86 && buf[3] == 0xdd && buf[4] >> 4 == 6);
        if n >= 5 && buf[0] == 0 && buf[1] == 0 && looks_like_pi {
            buf.copy_within(4..n, 0);
            n -= 4;
        }
        let pkt: Vec<u8> = buf[..n].to_vec();
        counters.read_bytes.fetch_add(n as u64, Ordering::Relaxed);
        // 需求信号：App 出站包到达（demand-driven-recovery D1——只计 App 源）
        counters.out_pkts.fetch_add(1, Ordering::Relaxed);
        let now_unix = now_unix_nanos();
        let mono = process_mono_start().elapsed().as_nanos() as i64;
        counters.last_outbound_ns.store(now_unix, Ordering::Relaxed);
        counters
            .last_outbound_mono_ns
            .store(mono, Ordering::Relaxed);
        if cmd_tx.send(Cmd::TunPacket(pkt)).is_err() {
            return; // driver 已死
        }
        // 唤醒 driver（我-4：与 Client::send 同一管道——写入 1 字节即触发 POLLIN）
        if let Some(wfd) = *wake.lock().expect("wake 锁中毒") {
            unsafe {
                libc::write(wfd, b"x".as_ptr().cast(), 1);
            }
        }
    }
}

fn now_unix_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use boringtun::x25519::StaticSecret;

    fn random_key() -> StaticSecret {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).unwrap();
        StaticSecret::from(b)
    }

    /// 双 Tunn 自环（noise 层互通钉死）：客户端发起 → 出口应答 → 排队内层包冲出 →
    /// 出口收到明文。PSK 混入与 x25519 面一并有假（派生错 = AEAD tag 失败此处即红）。
    #[test]
    fn two_tunn_handshake_then_queued_data() {
        let client_key = random_key();
        let server_key = random_key();
        let psk = *Psk::from(Secret::from([0x42; 32])).as_bytes();
        let mut client = Tunn::new(
            client_key.clone(),
            boringtun::x25519::PublicKey::from(&server_key),
            Some(psk),
            None,
            1,
            None,
        )
        .unwrap();
        let mut server = Tunn::new(
            server_key,
            boringtun::x25519::PublicKey::from(&client_key),
            Some(psk),
            None,
            2,
            None,
        )
        .unwrap();

        // 一个最小 IPv4 包当「内层明文」（首个出站包触发握手 + 排队）；
        // 总长字段必须正确——boringtun 按它截断（computed_len）后才回 WriteToTunnelV4。
        let mut inner = vec![0u8; 20 + 8];
        inner[0] = 0x45;
        let total = (inner.len() + 8) as u16;
        inner[2..4].copy_from_slice(&total.to_be_bytes());
        inner.extend_from_slice(b"payload!");

        let mut buf = [0u8; 65536];
        // ① 客户端 encapsulate：无会话 ⇒ 排队 + 握手 init
        let init = match client.encapsulate(&inner, &mut buf) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望握手 init，实得 {other:?}"),
        };
        // ② 出口 decapsulate：产握手应答
        let mut buf2 = [0u8; 65536];
        let resp = match server.decapsulate(None, &init, &mut buf2) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望握手应答，实得 {other:?}"),
        };
        // ③ 客户端收应答：keepalive + 建会话
        let mut buf3 = [0u8; 65536];
        match client.decapsulate(None, &resp, &mut buf3) {
            TunnResult::WriteToNetwork(_) => {} // 会话建立后的确认 keepalive
            other => panic!("期望 keepalive，实得 {other:?}"),
        }
        // ④ 空数据报重调：冲出排队的内层包
        let data = match client.decapsulate(None, &[], &mut buf3) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望排队数据冲出，实得 {other:?}"),
        };
        // ⑤ 出口解出明文
        let mut buf4 = [0u8; 65536];
        match server.decapsulate(None, &data, &mut buf4) {
            TunnResult::WriteToTunnelV4(pkt, _) => {
                assert_eq!(&pkt[inner.len() - 8..], b"payload!");
            }
            other => panic!("期望明文包，实得 {other:?}"),
        }
        // ⑥ 反向：出口 → 客户端一条数据
        let back = match server.encapsulate(&inner, &mut buf) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望数据包，实得 {other:?}"),
        };
        match client.decapsulate(None, &back, &mut buf4) {
            TunnResult::WriteToTunnelV4(pkt, _) => {
                assert_eq!(&pkt[inner.len() - 8..], b"payload!");
            }
            other => panic!("期望明文包，实得 {other:?}"),
        }
    }

    /// blackhole 反例（评审 ③-4 验收）：不可达出口 ⇒ probe 不得判通（Timeout，非 refused）。
    #[test]
    fn engine_probe_blackhole_times_out() {
        let logf: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(|_| {});
        let identity = Identity::ephemeral().unwrap();
        let client = Client::start(CoreConfig {
            peer_id: PeerId::from([1; 32]),
            secret: Secret::from([2; 32]),
            identity,
            candidates: vec![Candidate {
                // TEST-NET-3 不可达地址：镜像包无人应答 ⇒ WG 永不建会话 ⇒ probe 超时
                addr: "203.0.113.1:41641".parse().unwrap(),
                relay: false,
            }],
            logf,
        })
        .unwrap();
        let t0 = Instant::now();
        let r = client.path_probe(Duration::from_millis(1500));
        assert!(
            matches!(r, Err(ConnErr::Timeout)),
            "blackhole 应 Timeout，实得 {r:?}"
        );
        assert!(t0.elapsed() < Duration::from_secs(3), "测试期限超支");
        client.stop();
    }
}
