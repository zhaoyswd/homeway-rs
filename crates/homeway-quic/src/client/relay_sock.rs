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
use std::time::Instant;

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
///
/// M3 §3.1-N5 增补：`send_errs*` / `last_local_send_err_at` = **本机发送面信号**（阶梯
/// 重写的 M/R 判别输入；S4 消费）。计数点写死在 [`note_send_err`]（**唯一**入口）。
#[derive(Default, Debug)]
pub(crate) struct SockStats {
    /// 上行包封次数（中继路径）。
    pub(crate) relay_tx: u64,
    /// 下行收到的腿帧中**非 kind=5** 的条数（忽略面；S1c 探针实测中继会推 hint）。
    pub(crate) rx_ignored: u64,
    /// 收包总数（含被忽略的腿帧）与发包容貌（排障读数）。
    pub(crate) rx_dgrams: u64,
    pub(crate) tx_dgrams: u64,
    /// `try_send` 的**非 `WouldBlock`** 错误总数（N5 的第一层计数）。
    pub(crate) send_errs: u64,
    /// 其中命中 errno 白名单的条数（= 「M（本机发送面错误）」的证据；`rebind` 清零）。
    pub(crate) send_errs_local: u64,
    /// 末次**白名单**错误的时刻（新鲜度窗的源；`rebind` 清零 ⇒ `None`）。
    pub(crate) last_local_send_err_at: Option<std::time::Instant>,
}

impl SockStats {
    /// 换网清零（§3.1-N5：`Arc` 跨 socket 共享 ⇒ 必须显式清「上一次网络环境」的错误）。
    ///
    /// 清的是**白名单命中面**（M 判据的输入）；`send_errs` 总量保留（它是累计读数，
    /// 供快照/排障；拿它做判据会跨 rebind 混两个网络环境）。
    pub(crate) fn clear_local_send_err(&mut self) {
        self.send_errs_local = 0;
        self.last_local_send_err_at = None;
    }

    /// 本机发送面是否**新鲜报错**（M 判据；`now` 由调用方给——纯函数可测）。
    pub(crate) fn local_send_err_fresh(
        &self,
        now: std::time::Instant,
        window: std::time::Duration,
    ) -> bool {
        match self.last_local_send_err_at {
            Some(t) => now.saturating_duration_since(t) <= window,
            None => false,
        }
    }
}

/// 发送面错误的归类（§3.1-N5 的**纯分类**；计数与判别的单源）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SendErrClass {
    /// `WouldBlock`：quinn 契约下的**正常回执**（「发送缓冲满，回去等 poller」）——
    /// **不算错误**（计进去会把上行拥塞误判成「本机发送面错误」，而拥塞恰好也是快探
    /// 失败的时刻 ⇒ 会优先选 M（Rebind 对死对端无用）——正是设计门 4-1① 要消灭的误判）。
    WouldBlock,
    /// errno 白名单命中：`{ENETUNREACH, EHOSTUNREACH, EADDRNOTAVAIL, ENETDOWN, EINVAL}`
    /// ⇒ 判 M（本机发送面错误；换本地 socket 可能救）。
    Local,
    /// 其余错误（含 `ECONNREFUSED`/`EMSGSIZE` 等）：只计数 + 节流记行，**不进 M/R 判别**。
    Other,
}

/// 错误归类（纯函数；`WouldBlock` 与 errno 白名单的单源都在这）。
pub(crate) fn classify_send_err(e: &std::io::Error) -> SendErrClass {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        return SendErrClass::WouldBlock;
    }
    match e.raw_os_error() {
        Some(libc::ENETUNREACH)
        | Some(libc::EHOSTUNREACH)
        | Some(libc::EADDRNOTAVAIL)
        | Some(libc::ENETDOWN)
        | Some(libc::EINVAL) => SendErrClass::Local,
        _ => SendErrClass::Other,
    }
}

/// 计数落点（`try_send` 的**唯一**错误入口；返回本次归类供节流记行判定）。
pub(crate) fn note_send_err(st: &mut SockStats, e: &std::io::Error, now: std::time::Instant) -> SendErrClass {
    let class = classify_send_err(e);
    match class {
        SendErrClass::WouldBlock => {} // 正常回执：零计数（N5 的负例判据就是这条）
        SendErrClass::Local => {
            st.send_errs += 1;
            st.send_errs_local += 1;
            st.last_local_send_err_at = Some(now);
        }
        SendErrClass::Other => st.send_errs += 1,
    }
    class
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
        let framed = self.relays.label_of(&transmit.destination).map(|label| {
            let mut out = Vec::with_capacity(RELAY_TAG_LEN + ENV_LEN + transmit.contents.len());
            out.extend_from_slice(&wrap_uplink(label, transmit.contents));
            out
        });
        let (payload, relayed): (&[u8], bool) = match &framed {
            Some(f) => (f.as_slice(), true),
            None => (transmit.contents, false),
        };
        match self.io.try_send_to(payload, transmit.destination) {
            Ok(_) => {
                let mut st = lock_unpoison(&self.stats);
                st.tx_dgrams += 1;
                if relayed {
                    st.relay_tx += 1;
                }
                Ok(())
            }
            Err(e) => {
                // §3.1-N5：**非 `WouldBlock`** 才计数；`WouldBlock` 是正常回执（零计数）
                let (class, n) = {
                    let mut st = lock_unpoison(&self.stats);
                    let class = note_send_err(&mut st, &e, Instant::now());
                    (class, st.send_errs)
                };
                if class != SendErrClass::WouldBlock && log_due(n) {
                    (self.logf)(&format!(
                        "quic: 上行发送面错误（{class:?}；{e}；第 {n} 次；计数行首 3 + 每 100）"
                    ));
                }
                Err(e)
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

    /// **判据（§3.1-N5 的负例；S1 完成判据点名）**：上行拥塞（`WouldBlock` 高频）下
    /// `sock_send_errs` **不增长**——`WouldBlock` 是 quinn 契约下的正常回执
    /// （「发送缓冲满，回去等 poller」），把它计进「本机发送面错误」会在拥塞时刻
    /// 把 M（Rebind）误选成首选动作（设计门 4-1①）。
    #[test]
    fn would_block_never_grows_send_errs() {
        let mut st = SockStats::default();
        let now = Instant::now();
        for _ in 0..10_000 {
            let class = note_send_err(
                &mut st,
                &io::Error::from(io::ErrorKind::WouldBlock),
                now,
            );
            assert_eq!(class, SendErrClass::WouldBlock);
        }
        assert_eq!(st.send_errs, 0, "WouldBlock 绝不进任何计数");
        assert_eq!(st.send_errs_local, 0);
        assert!(st.last_local_send_err_at.is_none(), "不得留新鲜度位");
        assert!(!st.local_send_err_fresh(now, std::time::Duration::from_secs(5)));
    }

    /// **判据（§3.1-N5 的正例与判别面）**：errno 白名单 ⇒ `Local`（M 判据）；
    /// 其余 ⇒ `Other`（只计数）；新鲜度窗按「末次白名单错误」算；`rebind` 清零。
    #[test]
    fn local_send_err_whitelist_freshness_and_reset() {
        let mut st = SockStats::default();
        let t0 = Instant::now();
        // 白名单五项全部归 Local
        for errno in [
            libc::ENETUNREACH,
            libc::EHOSTUNREACH,
            libc::EADDRNOTAVAIL,
            libc::ENETDOWN,
            libc::EINVAL,
        ] {
            assert_eq!(
                classify_send_err(&io::Error::from_raw_os_error(errno)),
                SendErrClass::Local,
                "errno {errno} 必须在白名单"
            );
        }
        // 非白名单 ⇒ Other（含 ECONNREFUSED / 无 errno 的 Other 类）
        assert_eq!(
            classify_send_err(&io::Error::from_raw_os_error(libc::ECONNREFUSED)),
            SendErrClass::Other
        );
        assert_eq!(
            classify_send_err(&io::Error::other("x")),
            SendErrClass::Other
        );

        // 计数 + 新鲜度：t0 命中 ⇒ 5s 窗内新鲜；6s 后不新鲜
        let class = note_send_err(&mut st, &io::Error::from_raw_os_error(libc::ENETUNREACH), t0);
        assert_eq!(class, SendErrClass::Local);
        assert_eq!(st.send_errs, 1);
        assert_eq!(st.send_errs_local, 1);
        let win = std::time::Duration::from_secs(5);
        assert!(st.local_send_err_fresh(t0 + std::time::Duration::from_secs(4), win));
        assert!(!st.local_send_err_fresh(t0 + std::time::Duration::from_secs(6), win));

        // Other 只涨总量（不进 M 判据）
        note_send_err(&mut st, &io::Error::from_raw_os_error(libc::ECONNREFUSED), t0);
        assert_eq!(st.send_errs, 2);
        assert_eq!(st.send_errs_local, 1, "Other 不进白名单计数");

        // rebind 清零：白名单面归零（新鲜度位 None），总量保留（累计读数是排障面）
        st.clear_local_send_err();
        assert_eq!(st.send_errs_local, 0);
        assert!(st.last_local_send_err_at.is_none());
        assert!(!st.local_send_err_fresh(t0, win), "清零后不得再判新鲜");
        assert_eq!(st.send_errs, 2, "总量是累计读数（不清）");
    }
}
