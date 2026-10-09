//! 出口 QUIC 面的**自定义 UDP socket**（M1 设计 §1.6；风险 Q-G 的落地件）：一条 quinn
//! 抽象 socket 承载**两条物理路径**——
//!
//! | 路径 | 收 | 发 |
//! |---|---|---|
//! | **直连**（`serve.quic_listen` 独立端口） | 本 socket 直接读 | 本 socket 直接写 |
//! | **中继腿**（中继 assoc 的腿 socket） | **引擎线程**按腿帧解析后注入（见下） | 本 socket 走该腿 socket 并包 `[0xBB][5]` |
//!
//! **发送侧路由键 = `Transmit.destination`**（QUIC 眼里的连接对端地址）：命中腿表 ⇒ 走该
//! 腿 socket 并包腿帧；「最近摘除的腿」⇒ **丢 + 计数**（照 `server/bind.rs` 的 #17 纪律：
//! 回落直连端口只会把包打到中继数据口、污染别的会话）；其余 ⇒ 直连端口。
//! 腿帧 kind=5 由中继**原样透传**（中继不解释 kind）——这就是「中继代码零改动」的承载
//! 方式；kind 字节的真源 = `homeway-core` 的 `wtransport::frame::FrameKind::Quic`（本
//! crate 是叶子、不得依赖 `homeway-core`，故按字节复刻；两处一致性由 `homeway-core` 侧
//! 一条断言 `FrameKind::Quic.to_wire() == homeway_quic::FRAME_KIND_QUIC` 钉住）。
//!
//! **收侧为什么经引擎注入、而不是本 socket 直接读腿 socket**：一条腿 socket 上**两条栈的
//! 流量混在一起**（WG 的 kind=0/2/4 与 QUIC 的 kind=5 同腿），只能有一个读者——驱动线程的
//! `ServerBind` 已经是那个读者（poll + `leg_readable`），kind=5 帧经 `Inbound.quic` 送到
//! 引擎、再由引擎注入本 socket（设计 §1.6 的「引擎注入队列」）。两个读者同读一条 UDP
//! socket 会互相偷包 ⇒ **单一读者**是构造性不变量。
//!
//! 段能力（Q-K 登记）：本 socket `max_transmit_segments/max_receive_segments = 1`（quinn
//! 抽象 socket 的缺省）⇒ **丢掉多段批处理**（quinn-udp 内建在 Linux/OHOS 上可用
//! GSO/sendmmsg）。故 CPU 门槛必须用**产品路径**测（`tools/quic-ab.sh cpu` 测的是内建
//! socket）；退路 = 直连 socket 单独走 `quinn_udp::UdpSocketState`（保留 GSO）、腿 socket
//! 保持 1 段——见设计 §9.3 Q-K。
//!
//! `may_fragment()`：本 socket **不设** `IP_MTU_DISCOVER`/`DONTFRAG`（那是 quinn-udp 内建
//! socket 干的），故如实返回 `true` ⇒ `new_with_abstract_socket` 的
//! `allow_mtud = !may_fragment() = false` ⇒ quinn 走 `MtuDiscovery::disabled`（不发探测
//! 包）。**与设计 §1.2 的结论等价**：`upper_bound == initial_mtu` 时上探本就构造性关闭，
//! 且 `MtuDiscovery::disabled` 恒建 `BlackHoleDetector`（`quinn-proto/src/connection/
//! mtud.rs:47-57`，同文件有 `mtu_discovery_disabled_…_triggers_black_hole_detection`
//! 用例）⇒ 窄路径保护不丢、黑障仍一跳落到 `min_mtu`(1320)。

use std::collections::HashMap;
use std::io::{self, IoSliceMut};
use std::net::{SocketAddr, UdpSocket};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::net::UdpSocket as TokioUdp;
use tokio::sync::mpsc::{self, Receiver, Sender};

use crate::sync_util::lock_unpoison;

use super::bridge::{DropKind, ExitBridge};

/// 腿帧魔数（与 `wtransport::frame::FRAME_MAGIC` 同值）。
const FRAME_MAGIC: u8 = 0xBB;
/// 腿帧 kind=5（QUIC 载荷；真源 = `FrameKind::Quic`，跨 crate 一致性断言在
/// `homeway-core` 的 `wtransport::frame` 测试里）。
pub const FRAME_KIND_QUIC: u8 = 5;
/// 「最近摘除的腿」保留窗（与 `server/bind.rs` 的 `LEG_RECENT_TTL` 同值同义）。
const LEG_RECENT_TTL: Duration = Duration::from_secs(5 * 60);
/// 「最近摘除」窗表容量上限（满则清表重记——排障级记忆，与 #17 的 leg_ports 同口径）。
const LEG_RECENT_MAX: usize = 4096;

/// 一条注入包的落点（引擎线程 → QUIC 面；`src` = 该腿的远端地址 = QUIC 眼里的对端）。
pub(crate) struct InjPkt {
    pub(crate) src: SocketAddr,
    pub(crate) payload: Vec<u8>,
}

/// 注入队列的两端别名（有界通道；上限由 [`super::bridge::INBOUND_QUEUE_MAX`] 给）。
pub(crate) type InjectTx = Sender<InjPkt>;
pub(crate) type InjectRx = Receiver<InjPkt>;

/// 腿表（发送侧路由）：`远端地址 → 腿 socket 的**发送句柄**`。
///
/// - 发送句柄 = `UdpSocket::try_clone` 的 fd 副本：与驱动线程的**读侧共享同一 socket**
///   （同一本地端口 ⇒ 中继仍认成同一条腿；`register_leg` 已 `connect`，故 `send` 即发往
///   腿远端），与读侧无 fd 所有权冲突。
/// - `recent` = 「曾当过腿、现已摘除」的地址窗（TTL [`LEG_RECENT_TTL`]）：窗内对该地址的
///   发送**丢**而不回落直连（#17 同义）。
/// - 两条 map 都由 `ExitQuic` 的同步面方法（引擎线程）写、QUIC 线程读；持锁时间只覆盖
///   一次 `send` / 一次查表（微秒级；腿表 ≤ `RELAY_LEG_MAX`=64 条）。
#[derive(Default)]
pub(crate) struct LegTable {
    live: Mutex<HashMap<SocketAddr, UdpSocket>>,
    recent: Mutex<HashMap<SocketAddr, Instant>>,
}

impl LegTable {
    /// 登记/替换一条腿（同远端重登记 = 撤「最近摘除」标记后替换发送句柄）。
    pub(crate) fn open(&self, remote: SocketAddr, sock: UdpSocket) {
        lock_unpoison(&self.recent).remove(&remote);
        lock_unpoison(&self.live).insert(remote, sock);
    }

    /// 摘除一条腿（保留「最近摘除」窗；发送句柄随 drop 关闭该 fd 副本）。
    pub(crate) fn close(&self, remote: SocketAddr) {
        lock_unpoison(&self.live).remove(&remote);
        let mut recent = lock_unpoison(&self.recent);
        if recent.len() >= LEG_RECENT_MAX {
            recent.clear(); // 满表清空（排障级记忆，丢了只影响归因）
        }
        recent.insert(remote, Instant::now());
    }

    /// 当前腿远端集（引擎侧差分的读面）。
    pub(crate) fn remotes(&self) -> Vec<SocketAddr> {
        lock_unpoison(&self.live).keys().copied().collect()
    }

    pub(crate) fn has(&self, remote: &SocketAddr) -> bool {
        lock_unpoison(&self.live).contains_key(remote)
    }

    /// 该远端是否落在「最近摘除」窗内（窗内发送 = 丢）。
    fn is_recent(&self, remote: &SocketAddr) -> bool {
        let now = Instant::now();
        let mut recent = lock_unpoison(&self.recent);
        recent.retain(|_, at| now.duration_since(*at) <= LEG_RECENT_TTL);
        recent.contains_key(remote)
    }
}

/// 出口 QUIC 面的抽象 socket（[`quinn::AsyncUdpSocket`]）。
pub(crate) struct ExitSock {
    /// 直连端口 socket（`serve.quic_listen`；端点的 `local_addr` 也取自它）。
    direct: TokioUdp,
    local: SocketAddr,
    legs: Arc<LegTable>,
    /// 引擎注入队列（腿上的 kind=5 载荷；接收端只在 QUIC 线程 poll——锁无竞争）。
    inject: Mutex<InjectRx>,
    bridge: Arc<ExitBridge>,
}

impl std::fmt::Debug for ExitSock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExitSock")
            .field("local", &self.local)
            .field("legs", &self.legs.remotes().len())
            .finish_non_exhaustive()
    }
}

impl ExitSock {
    pub(crate) fn new(
        direct: TokioUdp,
        local: SocketAddr,
        legs: Arc<LegTable>,
        inject: InjectRx,
        bridge: Arc<ExitBridge>,
    ) -> Self {
        Self {
            direct,
            local,
            legs,
            inject: Mutex::new(inject),
            bridge,
        }
    }

    /// 注入一条腿上的 QUIC 报文（引擎线程；`false` = 面已收工 ⇒ 调用方停止注入）。
    /// 队列满 ⇒ 丢 + 计数（`未登记`；§6.4 的「不许静默」纪律），**不阻塞引擎**。
    pub(crate) fn inject(tx: &InjectTx, bridge: &ExitBridge, src: SocketAddr, payload: Vec<u8>) -> bool {
        match tx.try_send(InjPkt { src, payload }) {
            Ok(()) => true,
            Err(mpsc::error::TrySendError::Full(_)) => {
                bridge.note_drop(DropKind::Unregistered, "QUIC 注入队列满（8192 条）");
                true
            }
            Err(mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    /// `try_send` 的腿路径：包 `[0xBB][5]` 走该腿 socket。`None` = 该目的地不是腿
    /// （调用方走直连）；`Some(..)` = 已由腿路径消化。
    fn try_send_leg(&self, t: &Transmit<'_>) -> Option<io::Result<()>> {
        let live = lock_unpoison(&self.legs.live);
        let Some(sock) = live.get(&t.destination) else {
            drop(live);
            if self.legs.is_recent(&t.destination) {
                // 腿已摘：**丢**（不回落直连——#17）。连接多半已随腿之死而终，QUIC 自己的
                // 超时/重传兜底；这里只保证「不误发到别的会话」且计数可见。
                self.bridge.note_drop(
                    DropKind::Unregistered,
                    &format!("腿已摘除，QUIC 报文不回落直连（→ {}）", t.destination),
                );
                return Some(Ok(()));
            }
            return None;
        };
        let mut frame = Vec::with_capacity(2 + t.contents.len());
        frame.push(FRAME_MAGIC);
        frame.push(FRAME_KIND_QUIC);
        frame.extend_from_slice(t.contents);
        let res = match sock.send(&frame) {
            Ok(_) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                // 腿 socket 发送缓冲满（连接 socket 的 SO_SNDBUF）：**丢 + 计数**（不静默），
                // 且**不**返 WouldBlock——本 socket 的 io_poller 只覆盖直连面（返 WouldBlock
                // 会让 quinn 在直连可写时忙转重试）。丢一条 = 端到端 TCP 重传的事。
                self.bridge.note_drop(
                    DropKind::SendBufferFull,
                    &format!("腿 socket 发送缓冲满（→ {}，{}B）", t.destination, frame.len()),
                );
                Ok(())
            }
            Err(e) => Err(e),
        };
        Some(res)
    }
}

impl AsyncUdpSocket for ExitSock {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(SockPoller { sock: self })
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        // 段能力 = 1（模块头 Q-K）：quinn 不会给出 `segment_size`；真出现就按段拆开逐条发
        // （不静默截断——拆发是唯一不撒谎的处置）。
        if let Some(seg) = transmit.segment_size {
            debug_assert!(false, "本 socket 声明 max_transmit_segments=1，不应收到多段 Transmit");
            for chunk in transmit.contents.chunks(seg.max(1)) {
                let one = Transmit {
                    destination: transmit.destination,
                    ecn: transmit.ecn,
                    contents: chunk,
                    segment_size: None,
                    src_ip: transmit.src_ip,
                };
                self.try_send(&one)?;
            }
            return Ok(());
        }
        if let Some(res) = self.try_send_leg(transmit) {
            return res;
        }
        self.direct
            .try_send_to(transmit.contents, transmit.destination)
            .map(|_| ())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        debug_assert_eq!(bufs.len(), meta.len(), "quinn 恒成对传入 bufs/meta");
        debug_assert!(!bufs.is_empty(), "max_receive_segments=1 ⇒ 至少一个缓冲");
        // ① 注入队列优先（中继腿的报文与直连**不共享**延迟面；先到先喂，避免直连大流量
        //    把腿上的包压在队列里——队列有界性由 `INBOUND_QUEUE_MAX` + 丢计数保证）。
        {
            let mut rx = lock_unpoison(&self.inject);
            if let Poll::Ready(Some(p)) = rx.poll_recv(cx) {
                let buf = &mut bufs[0];
                if p.payload.len() > buf.len() {
                    // 收侧超限（理论不可达：quinn 接收缓冲按 max_udp_payload_size 分配，
                    // 上限 65527B ≫ 我们 ≤1500B 的报文）——丢 + 计数，不截断。
                    self.bridge.note_drop(
                        DropKind::TooLarge,
                        &format!(
                            "注入报文 {}B 超接收缓冲 {}B（来自 {}）",
                            p.payload.len(),
                            buf.len(),
                            p.src
                        ),
                    );
                    return Poll::Ready(Ok(0)); // 本轮无包（quinn 会立刻再 poll）
                }
                let n = p.payload.len();
                buf[..n].copy_from_slice(&p.payload);
                meta[0] = RecvMeta { addr: p.src, len: n, stride: n, ecn: None, dst_ip: None };
                return Poll::Ready(Ok(1));
            }
        }
        // ② 直连端口 socket（readiness → `try_io` 形态：WouldBlock 时自动清位并重注册
        //    唤醒——与 quinn 内建 socket 的收包循环同构）
        let buf = &mut bufs[0];
        loop {
            match self.direct.poll_recv_ready(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
            match self.direct.try_recv_from(&mut buf[..]) {
                Ok((n, src)) => {
                    meta[0] = RecvMeta { addr: src, len: n, stride: n, ecn: None, dst_ip: None };
                    return Poll::Ready(Ok(1));
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Poll::Ready(Err(e)),
            }
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        Ok(self.local)
    }

    /// 多段批处理关闭（Q-K 登记；见模块头）。
    fn max_transmit_segments(&self) -> usize {
        1
    }

    fn max_receive_segments(&self) -> usize {
        1
    }

    /// 本 socket 不设 `DONTFRAG`/`MTU_DISCOVER` ⇒ 如实 `true`（见模块头：与设计 §1.2 的
    /// `upper_bound == initial` 结论等价，黑障检测不受影响）。
    fn may_fragment(&self) -> bool {
        true
    }
}

/// 写就绪 poller（只覆盖**直连**面：腿路径的 `WouldBlock` 由「丢 + 计数」消化、不返回
/// `WouldBlock` ⇒ quinn 不会等在这里）。
struct SockPoller {
    sock: Arc<ExitSock>,
}

impl std::fmt::Debug for SockPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SockPoller").finish_non_exhaustive()
    }
}

impl UdpPoller for SockPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sock.direct.poll_send_ready(cx)
    }
}
