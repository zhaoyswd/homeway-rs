//! 客户端侧的**中继包封/剥壳 socket**（M1 设计 §1.6 末段 / §10 S2-7）——与出口侧
//! [`crate::exit::socket`] **同构**（一条抽象 socket 承载两条物理路径），差别在路由键：
//!
//! | 路径 | 上行（发） | 下行（收） |
//! |---|---|---|
//! | **直连**（`Via::Direct` 候选） | 裸 QUIC 包 | 裸 QUIC 包 |
//! | **中继**（`Via::Relay{label}` 候选） | `[0xAA][label8]‖[0xBB][5]‖pkt` | `[0xBB][5]‖pkt` 剥壳 |
//!
//! 三条形态约束（逐条给理由）：
//!
//! 1. **本地 socket 只有一枚**（直连与中继共用）：QUIC 连接的路径身份 = 本地地址；
//!    出口/中继看到的源端口必须唯一（`Rebind` 是 endpoint 粒度、经中继迁移 = 换源 ⇒
//!    中继新 assoc ⇒ 出口重拨腿，设计 §2.3）。两枚 socket 会让「连接」出现两个源，
//!    把路径语义弄乱。
//! 2. **发送侧路由键 = `Transmit.destination`**（QUIC 眼里的连接对端地址）：候选是
//!    `{addr, via}`，中继候选的 addr 就是中继端点 ⇒ 查 [`RelayTable`] 命中即包封。
//!    表由岛在 `SetCandidates`/`Connect` 时装配（`set` 整体替换 + 现任 `pin`）。
//! 3. **接收侧按首字节分流**：`0xBB` 开头的报文是腿帧（中继下行恒 `[0xBB][kind]`），
//!    `kind=5` 剥壳喂 quinn、**其余 kind 忽略**（中继会推 hint 控制帧——S1c 探针实测
//!    `rx_ignored=4`）；非 `0xBB` 开头 = 裸 QUIC 包（`0xAA`/`0xBB` 都不是合法的 QUIC v1
//!    首字节：长头保留位/固定位约束，短头首字节 ∈ [0x40,0x7F]）。
//!
//! 段能力 = 1（与出口侧同款，Q-K 登记）：`max_transmit_segments/max_receive_segments`
//! 返回 1 ⇒ 丢掉 quinn-udp 内建 socket 的多段批处理（GSO/sendmmsg）。**客户端侧同一
//! 约束**（本 socket 覆盖直连与中继两条路径），退路同 Q-K（直连路径单独走
//! `quinn::udp::UdpSocketState` 保 GSO）。
//!
//! `may_fragment() = true`：本 socket 不设 `DONTFRAG`/`IP_MTU_DISCOVER`（那是 quinn-udp
//! 内建 socket 干的）⇒ 如实为真 ⇒ `allow_mtud = !may_fragment() = false` ⇒ quinn 走
//! `MtuDiscovery::disabled`（与设计 §1.2 的 `upper_bound == initial_mtu` 结论等价：上探
//! 本就构造性关闭；黑障检测恒建，窄路径保护不丢）。**与出口侧同款形态**。

use std::collections::HashMap;
use std::io::{self, IoSliceMut};
use std::net::{SocketAddr, SocketAddrV4};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};

use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use tokio::net::UdpSocket as TokioUdp;

use crate::cmd::{Candidate, Logf, Via};
use crate::exit::FRAME_KIND_QUIC;
use crate::sync_util::lock_unpoison;

use super::log_due;

/// 标签帧魔数（`[0xAA][label8]` 前缀；真源 = `wtransport::frame::RELAY_TAG_MAGIC`）。
pub(crate) const RELAY_TAG_MAGIC: u8 = 0xAA;
/// 腿帧魔数（`[0xBB][kind]` 前缀；真源 = `wtransport::frame::FRAME_MAGIC`）。
pub(crate) const FRAME_MAGIC: u8 = 0xBB;
/// 标签帧头长（`[0xAA][label8]`）。
pub(crate) const RELAY_TAG_LEN: usize = 9;
/// 腿帧头长（`[0xBB][kind]`）。
pub(crate) const ENV_LEN: usize = 2;

/// 剥壳结论（**借用视图**；`Ignored` 不喂 quinn、不当数据）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Downlink<'a> {
    /// 裸 QUIC 包（直连路径；原样喂 quinn）。
    Bare,
    /// 腿帧 `kind=5` ⇒ 剥掉 `[0xBB][5]` 后的 QUIC 载荷。
    Quic(&'a [u8]),
    /// 非 `kind=5` 的腿帧（中继 hint 控制帧等）或畸形的 `0xBB` 开头报文——**忽略**。
    Ignored,
}

/// 接收判据（纯函数：字节级可测）。
pub(crate) fn strip_downlink(buf: &[u8]) -> Downlink<'_> {
    match buf.first() {
        Some(&FRAME_MAGIC) => match buf.get(1) {
            Some(&FRAME_KIND_QUIC) => Downlink::Quic(&buf[ENV_LEN..]),
            // 非 kind=5（hint/reg/容器…）或只有魔数的畸形帧：忽略（不喂 quinn）
            Some(_) | None => Downlink::Ignored,
        },
        // 空包/非腿帧首字节：按裸包交 quinn（能不能解是 QUIC 的事）
        _ => Downlink::Bare,
    }
}

/// 上行包封：`[0xAA][label8]‖[0xBB][5]‖pkt`（中继候选专用）。
pub(crate) fn wrap_uplink(label: [u8; 8], pkt: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(RELAY_TAG_LEN + ENV_LEN + pkt.len());
    out.push(RELAY_TAG_MAGIC);
    out.extend_from_slice(&label);
    out.push(FRAME_MAGIC);
    out.push(FRAME_KIND_QUIC);
    out.extend_from_slice(pkt);
    out
}

/// 中继候选表（发送侧路由）：`中继端点地址 → label`。
///
/// `cands` = 本轮候选清单（`set` 整体替换）；`pinned` = **现任连接**的条目（`pin` 单条
/// 替换）——`set` 不该把在用中继连接的条目抹掉（否则现任连接的上行会突然变成裸包）。
#[derive(Default)]
pub(crate) struct RelayTable {
    cands: Mutex<HashMap<SocketAddr, [u8; 8]>>,
    pinned: Mutex<Option<(SocketAddr, [u8; 8])>>,
}

impl RelayTable {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// 装配本轮候选清单（只收 `Via::Relay` 的条目；整体替换）。
    pub(crate) fn set(&self, cands: &[Candidate]) {
        let mut m = lock_unpoison(&self.cands);
        m.clear();
        for c in cands {
            if let Via::Relay { label } = c.via {
                m.insert(SocketAddr::V4(c.addr), label);
            }
        }
    }

    /// 钉住现任连接的条目（`set` 后仍在表里）。
    pub(crate) fn pin(&self, addr: SocketAddrV4, label: [u8; 8]) {
        *lock_unpoison(&self.pinned) = Some((SocketAddr::V4(addr), label));
    }

    /// 发送侧查询（`None` = 直连路径 ⇒ 裸包）。
    pub(crate) fn label_of(&self, dest: &SocketAddr) -> Option<[u8; 8]> {
        if let Some(l) = lock_unpoison(&self.cands).get(dest) {
            return Some(*l);
        }
        match &*lock_unpoison(&self.pinned) {
            Some((a, l)) if a == dest => Some(*l),
            _ => None,
        }
    }
}

/// socket 读数（非原子化不必要——本 crate 的单线程结构不变量；计数在岛线程内读写）。
#[derive(Default, Debug)]
pub(crate) struct SockStats {
    /// 上行包封次数（中继路径）。
    pub(crate) relay_tx: u64,
    /// 下行收到的腿帧中**非 kind=5** 的条数（忽略面；S1c 探针实测中继会推 hint）。
    pub(crate) rx_ignored: u64,
    /// 收包总数（含被忽略的腿帧）与发包容貌（排障读数）。
    pub(crate) rx_dgrams: u64,
    pub(crate) tx_dgrams: u64,
}

/// 岛侧抽象 socket（`quinn::AsyncUdpSocket`）：直连裸包 + 中继包封/剥壳。
pub(crate) struct ClientSock {
    io: TokioUdp,
    local: SocketAddr,
    relays: Arc<RelayTable>,
    /// 计数（`Arc` 与岛共享；岛线程写、快照读）。
    stats: Arc<Mutex<SockStats>>,
    logf: Logf,
}

impl std::fmt::Debug for ClientSock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClientSock")
            .field("local", &self.local)
            .finish_non_exhaustive()
    }
}

impl ClientSock {
    /// 建一枚绑定好的 socket（非阻塞；调用方须在 runtime 上下文内——`from_std` 需要）。
    pub(crate) fn open(
        bind: Option<SocketAddrV4>,
        relays: Arc<RelayTable>,
        stats: Arc<Mutex<SockStats>>,
        logf: Logf,
    ) -> io::Result<(Arc<Self>, SocketAddrV4)> {
        let addr = bind.map(SocketAddr::V4);
        let std_sock = match addr {
            Some(a) => std::net::UdpSocket::bind(a)?,
            None => std::net::UdpSocket::bind((std::net::Ipv4Addr::UNSPECIFIED, 0))?,
        };
        std_sock.set_nonblocking(true)?;
        let local = std_sock.local_addr()?;
        let SocketAddr::V4(v4) = local else {
            return Err(io::Error::other("本地 socket 不是 IPv4（形态异常）"));
        };
        let io = TokioUdp::from_std(std_sock)?;
        Ok((
            Arc::new(Self {
                io,
                local,
                relays,
                stats,
                logf,
            }),
            v4,
        ))
    }
}

impl AsyncUdpSocket for ClientSock {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(SockPoller { sock: self })
    }

    fn try_send(&self, transmit: &Transmit<'_>) -> io::Result<()> {
        // 段能力 = 1（模块头 Q-K）：quinn 不该给出 `segment_size`；真出现就按段拆发
        // （不静默截断——与出口侧同款处置）。
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
        match self.relays.label_of(&transmit.destination) {
            Some(label) => {
                let framed = wrap_uplink(label, transmit.contents);
                self.io.try_send_to(&framed, transmit.destination).map(|_| ())?;
                let mut st = lock_unpoison(&self.stats);
                st.relay_tx += 1;
                st.tx_dgrams += 1;
                Ok(())
            }
            None => {
                self.io
                    .try_send_to(transmit.contents, transmit.destination)
                    .map(|_| ())?;
                lock_unpoison(&self.stats).tx_dgrams += 1;
                Ok(())
            }
        }
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
        debug_assert_eq!(bufs.len(), meta.len(), "quinn 恒成对传入 bufs/meta");
        let buf = &mut bufs[0];
        loop {
            match self.io.poll_recv_ready(cx) {
                Poll::Ready(Ok(())) => {}
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            }
            let (n, src) = match self.io.try_recv_from(&mut buf[..]) {
                Ok(v) => v,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => continue,
                Err(e) => return Poll::Ready(Err(e)),
            };
            lock_unpoison(&self.stats).rx_dgrams += 1;
            let (start, payload) = match strip_downlink(&buf[..n]) {
                Downlink::Bare => (0, &buf[..n]),
                Downlink::Quic(p) => (ENV_LEN, p),
                // 非 kind=5 帧：**不喂 quinn**（中继的 hint 控制帧等）；计数 + 节流记行
                Downlink::Ignored => {
                    let n_ignored = {
                        let mut st = lock_unpoison(&self.stats);
                        st.rx_ignored += 1;
                        st.rx_ignored
                    };
                    if log_due(n_ignored) {
                        (self.logf)(&format!(
                            "quic: 忽略非 kind=5 腿帧（来自 {src}；第 {n_ignored} 次；计数行首 3 + 每 100）"
                        ));
                    }
                    continue;
                }
            };
            let len = payload.len();
            if start > 0 {
                buf.copy_within(start..n, 0);
            }
            meta[0] = RecvMeta {
                addr: src,
                len,
                stride: len,
                ecn: None,
                dst_ip: None,
            };
            return Poll::Ready(Ok(1));
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

    /// 不设 `DONTFRAG`/`MTU_DISCOVER` ⇒ 如实 `true`（见模块头：与设计 §1.2 等价）。
    fn may_fragment(&self) -> bool {
        true
    }
}

/// 写就绪 poller（单 socket 两个方向共用）。
struct SockPoller {
    sock: Arc<ClientSock>,
}

impl std::fmt::Debug for SockPoller {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SockPoller").finish_non_exhaustive()
    }
}

impl UdpPoller for SockPoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sock.io.poll_send_ready(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **判据（S2-7 的字节级面）**：上行包封逐字节 = `[0xAA][label8]‖[0xBB][5]‖pkt`；
    /// 下行剥壳还原逐字节；非 kind=5 帧（hint）与畸形帧**不入 quinn**。
    #[test]
    fn encap_and_strip_are_byte_exact() {
        let pkt = [0x40u8, 0x01, 0x02, 0x03]; // 短头形态的裸 QUIC 包（示意）
        let label = [0xA1u8, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18];
        let wrapped = wrap_uplink(label, &pkt);
        assert_eq!(wrapped.len(), RELAY_TAG_LEN + ENV_LEN + pkt.len());
        assert_eq!(&wrapped[..9], &[0xAA, 0xA1, 0xB2, 0xC3, 0xD4, 0xE5, 0xF6, 0x07, 0x18]);
        assert_eq!(&wrapped[9..11], &[0xBB, 5]);
        assert_eq!(&wrapped[11..], &pkt);

        // 下行：剥壳还原（逐字节）
        let down = [0xBB, 5, 0x40, 0xAA, 0xBB];
        assert_eq!(strip_downlink(&down), Downlink::Quic(&[0x40, 0xAA, 0xBB]));
        // 直连：裸包原样
        assert_eq!(strip_downlink(&pkt), Downlink::Bare);
        // 非 kind=5 帧（hint=1 / 数据=0 / 容器=4）⇒ 忽略
        for kind in [0u8, 1, 2, 4] {
            let f = [0xBB, kind, 0x11, 0x22];
            assert_eq!(strip_downlink(&f), Downlink::Ignored, "kind={kind} 必须忽略");
        }
        // 畸形：只有魔数 / 空包（空包按裸包交 quinn）
        assert_eq!(strip_downlink(&[0xBB]), Downlink::Ignored);
        assert_eq!(strip_downlink(&[]), Downlink::Bare);
    }

    /// 中继表：`set` 整体替换候选面；`pin` 的现任条目在 `set` 后仍在（防在用连接的上行
    /// 突然变裸包）。
    #[test]
    fn relay_table_set_and_pin() {
        let t = RelayTable::new();
        let a: SocketAddrV4 = "127.0.0.1:42781".parse().unwrap();
        let b: SocketAddrV4 = "127.0.0.1:42782".parse().unwrap();
        let c: SocketAddrV4 = "127.0.0.1:42652".parse().unwrap();
        let la = [1u8; 8];
        let lb = [2u8; 8];
        t.set(&[
            Candidate {
                addr: a,
                via: Via::Relay { label: la },
            },
            Candidate {
                addr: c,
                via: Via::Direct,
            },
        ]);
        assert_eq!(t.label_of(&SocketAddr::V4(a)), Some(la));
        assert_eq!(t.label_of(&SocketAddr::V4(c)), None, "直连候选不进中继表");
        // 现任钉住后重设候选清单：现任仍在
        t.pin(b, lb);
        t.set(&[Candidate {
            addr: c,
            via: Via::Direct,
        }]);
        assert_eq!(t.label_of(&SocketAddr::V4(a)), None, "旧候选已替换");
        assert_eq!(t.label_of(&SocketAddr::V4(b)), Some(lb), "现任条目必须留存");
    }
}
