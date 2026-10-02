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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::StaticSecret;
use smoltcp::socket::tcp::{self, Socket as TcpSocket};
use smoltcp::iface::SocketHandle;
use smoltcp::time::Instant as SmolInstant;

use crate::identity::Identity;
use crate::psk::Psk;
use crate::token::{PeerId, Secret};
use crate::tunnel_addr;
use crate::wtransport::{Bind, Candidate, RegCtx, Via};

pub mod stackb;

use self::stackb::{DialError, StackB};

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
}

pub struct CoreConfig {
    pub peer_id: PeerId,
    pub secret: Secret,
    pub identity: Identity,
    pub candidates: Vec<Candidate>,
    pub logf: Arc<dyn Fn(&str) + Send + Sync>,
}

type UdpRecvReply = Result<(Vec<u8>, SocketAddrV4), ConnErr>;

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
        match self.tunn.decapsulate(Some(src_ip), datagram, &mut self.wg_buf) {
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
                self.stack.inject(pkt);
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
        let rx_meta: Vec<udp::PacketMetadata> = (0..16).map(|_| udp::PacketMetadata::EMPTY).collect();
        let tx_meta: Vec<udp::PacketMetadata> = (0..16).map(|_| udp::PacketMetadata::EMPTY).collect();
        let mut sock = udp::Socket::new(
            udp::PacketBuffer::new(rx_meta, vec![0u8; 64 * 1024]),
            udp::PacketBuffer::new(tx_meta, vec![0u8; 64 * 1024]),
        );
        let ep = smoltcp::wire::IpEndpoint::new(self.stack.tunnel_ip.into(), port);
        sock.bind(ep).map_err(|e| ConnErr::Dial(DialError::Stack(format!("{e:?}"))))?;
        let h = self.stack.sockets.add(sock);
        self.udp.insert(id, UdpConn { handle: h, wait_recv: None });
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
        let sock = self.stack.sockets.get_mut::<smoltcp::socket::udp::Socket>(u.handle);
        sock.send_slice(data, ep).map_err(|_| ConnErr::Closed)?;
        Ok(())
    }

    /// UDP 读结算（resolve_pending 的伙伴面）。
    fn resolve_udp(&mut self) {
        let ids: Vec<u64> = self.udp.keys().copied().collect();
        for id in ids {
            let Some(u) = self.udp.get_mut(&id) else { continue };
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
            Cmd::Connect { id, dst, deadline, reply } => {
                match self.start_conn(id, dst, deadline) {
                    Ok(()) => {
                        if let Some(c) = self.conns.get_mut(&id) {
                            c.wait_est = Some(reply);
                        }
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
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
            Cmd::UdpSend { id, dst, data, reply } => {
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
            Cmd::Stop => return false,
        }
        true
    }

    fn start_conn(&mut self, id: u64, dst: SocketAddrV4, deadline: Duration) -> Result<(), ConnErr> {
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
            let Some(c) = self.conns.get_mut(&id) else { continue };
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
            if state == tcp::State::Closed && !can_recv && c.wait_est.is_none() && c.wait_read.is_none() {
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
        let mut s = self.snapshot.lock().expect("快照锁中毒");
        s.via = st.via;
        s.ep = st.ep;
        s.mirrored = st.mirrored;
        s.rx = rx;
        s.tx = tx;
        s.last_local_send_err = self.bind.last_local_send_err_at();
    }
}

/// 客户端句柄（主线程/拨号线程共享面）：命令投递 + 状态轮询 + 收工（幂等，`&self`——
/// 经 `Arc<Client>` 共享时也能收口）。
pub struct Client {
    cmd_tx: mpsc::Sender<Cmd>,
    wake_wr: Mutex<Option<i32>>,
    handle: Mutex<Option<JoinHandle<()>>>,
    snapshot: Arc<Mutex<Snapshot>>,
    stop: Arc<AtomicBool>,
    next_id: Arc<AtomicU64>,
    pub tunnel_ip: Ipv4Addr,
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

        let identity_key = cfg.identity.private_key().clone();
        let secret = cfg.secret;
        let tunn = make_tunn(&identity_key, &secret, cfg.peer_id.as_bytes());
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
        };

        let stop2 = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("homeway-wg".into())
            .spawn(move || driver(engine, wake_r, stop2))?;

        Ok(Self {
            cmd_tx,
            wake_wr: Mutex::new(Some(wake_w)),
            handle: Mutex::new(Some(handle)),
            snapshot,
            stop,
            next_id,
            tunnel_ip,
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
        self.send(Cmd::Connect { id, dst, deadline, reply: tx });
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
        rx.recv().map_err(|_| ConnErr::EngineGone)?
            .map(|port| (id, port))
    }

    pub fn udp_send(&self, id: u64, dst: SocketAddrV4, data: Vec<u8>) -> Result<(), ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::UdpSend { id, dst, data, reply: tx });
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

    pub fn path_probe(&self, timeout: Duration) -> Result<(), ConnErr> {
        self.connect_deadline(SocketAddrV4::new(SERVER_TUNNEL_IP, 1), timeout)
            .map(|_| ())
            // 对端 :1 若真有服务（连接成功）也说明会话活着
            .or_else(|e| match e {
                ConnErr::Refused => Ok(()),
                other => Err(other),
            })
    }

    pub fn write(&self, id: u64, data: Vec<u8>) -> Result<usize, ConnErr> {
        let (tx, rx) = mpsc::channel();
        self.send(Cmd::Write { id, data, reply: tx });
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
            unsafe {
                while libc::read(wake_r, b.as_mut_ptr().cast(), 64) > 0 {}
            }
        }
        if !engine.pump_once(&mut udp_buf[..]) {
            break;
        }
    }
    unsafe {
        libc::close(wake_r);
    }
    let _ = io::stdout().flush();
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
