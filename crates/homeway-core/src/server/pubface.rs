//! 出口**公共端点面**（M5 S3a）：WG 面退役后，出口唯一的公共 UDP 端口 = QUIC 出口
//! socket（`homeway_quic::ExitQuic` 的直连端口）。本模块是那一端口上**非 QUIC** 流量的
//! 协议真源——原 `ServerBind`（WG socket）上的五类公共端点面在这里重建：
//!
//! | # | 面 | 原落点 | 现落点 |
//! |---|---|---|---|
//! | ① | STUN 观测（在**公共端口**上问一次 → 该端口的 NAT 映射） | `bind.stun_query` | [`PubFace::stun_begin`] + `ExitQuic::send_plain` |
//! | ② | 参照点探测明文应答（`HWQ` → `HWR`） | `bind.probe_endpoints` + `process_packet` | [`PubFace::handle`] → `probe::respond_ex`（原函数，零改） |
//! | ③ | udpcap 能力位（应答里的 flags） | `bind.set_caps` | [`PubFace::set_caps`] |
//! | ④ | 绑卡/重钉 + 本地端口（该 socket 的事实） | `bind.repin_to`/`pinned`/`local_port` | [`PubFace::attach_socket`] + [`PubFace::repin_to`]/[`local_port`](PubFace::local_port)（socket 的 dup fd——setsockopt 作用于同一 socket 对象） |
//! | ⑤ | QUIC 腿的注入/不可投递计数 | `bind.note_quic_leg_undelivered` | 留在 `server::bind`（腿面计数，非公共端口面） |
//!
//! **为什么钩子形态**：公共端口 socket 归 QUIC 面独占（单一读者是构造性不变量），本模块
//! 只挂一个「认领与否」的闭包（`homeway_quic::PlainDatagramHook`）——岛不认识探测/STUN/
//! 腿帧协议，同步面（本模块）不认识 quinn。判据行（E23 的 `入站新源`）与值域收窄见
//! `docs/reviews/M5-design.md` §3.3-E23：本模块只产出 `STUN应答`/`参照点探测`/`畸形腿帧`/
//! `腿帧type=3`/`腿帧type=N` 五类形态。
//!
//! **中继注册腿的回执也落在这里**：注册腿的 Hello 从**公共端口**发出（同端口 = NAT 映射
//! 一致），中继的回执（`[0xBB][3]…`）回到本端口 ⇒ 经 [`PubFace::set_on_leg_frame`] 的
//! 钩子转给 `server::relayleg` 的注册腿线程（原形态：主 socket 的 `process_packet` 分派）。

use std::collections::HashMap;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use crate::legframe::{self};
use crate::Logf;

/// 新源记录表容量上限（Go srcSeenMax——与 `server::bind` 同值同义）。
const SRC_SEEN_MAX: usize = 4096;

/// 中继控制帧（`[0xBB][3]…`）回调（转发给注册腿线程；`server::bind::OnLegFrame` 同形）。
pub type OnLegFrame = Box<dyn FnMut(&[u8], SocketAddr) + Send>;

/// 入站新源首包去重表（E23 判据行；容量满清表重记——排障去重表，非 correctness 状态）。
///
/// **单源**：公共端口面（本模块）与腿面（`server::bind` 的腿 socket 解析）共用同一实现
/// ——两处产出的形态串共同构成 E23 的值域（M5 登记：`STUN应答`/`参照点探测`/`畸形腿帧`/
/// `腿帧type=3`/`腿帧type=N`/`非帧（…）`/腿面上的未知形态）。
pub(crate) struct SrcSeen {
    seen: Mutex<HashMap<SocketAddr, ()>>,
}

impl SrcSeen {
    pub(crate) fn new() -> Self {
        Self { seen: Mutex::new(HashMap::new()) }
    }

    /// 每个新来源只记一行首包（含包形态）。`logf` 由调用方给（两处日志流不同）。
    pub(crate) fn note(&self, logf: &Logf, src: SocketAddr, shape: &str, n: usize) {
        let mut g = self.seen.lock().expect("src_seen 锁中毒");
        if g.contains_key(&src) {
            return;
        }
        if g.len() >= SRC_SEEN_MAX {
            g.clear();
            (*logf)(&format!("入站新源表满（{SRC_SEEN_MAX} 条），清表重记"));
        }
        g.insert(src, ());
        drop(g);
        (*logf)(&format!("入站新源：{src}（{shape}，{n} 字节）"));
    }

    #[cfg(test)]
    pub(crate) fn contains(&self, src: &SocketAddr) -> bool {
        self.seen.lock().expect("src_seen 锁中毒").contains_key(src)
    }
}

/// 一次在等的 STUN 观测（同号只有一次——观测周期是分钟级，Go `stunPending` 同义）。
struct StunWait {
    txid: [u8; 12],
    reply: std::sync::mpsc::Sender<Option<SocketAddr>>,
}

/// 出口公共端点面（装配期构造，驱动线程与 QUIC 面线程共享——`Arc`）。
pub struct PubFace {
    build: String,
    /// 探测应答的能力位（udpcap 周期结论；原 `ServerBind.caps`）。
    caps: AtomicU8,
    /// 探测应答端点列表段来源（已公布公网端点；原 `ServerBind.probe_endpoints`）。
    probe_endpoints: Mutex<Vec<SocketAddr>>,
    /// 在等的 STUN 观测（`None` = 无）。
    stun: Mutex<Option<StunWait>>,
    /// 中继控制帧回调（注册腿线程接；未装 = 照旧忽略）。
    on_leg_frame: Mutex<Option<OnLegFrame>>,
    /// 入站新源首包去重表（E23 判据行；与腿面共用 [`SrcSeen`]）。
    src_seen: SrcSeen,
    /// 公共端口 socket 的 **dup fd**（重钉用：`setsockopt` 作用于同一 socket 对象，
    /// QUIC 面侧无需知情）。轮询期无锁（一次性装配后只读）。
    pin_sock: Mutex<Option<UdpSocket>>,
    logf: Logf,
}

impl PubFace {
    pub fn new(build: &str, logf: Logf) -> Self {
        Self {
            build: build.to_string(),
            caps: AtomicU8::new(0),
            probe_endpoints: Mutex::new(Vec::new()),
            stun: Mutex::new(None),
            on_leg_frame: Mutex::new(None),
            src_seen: SrcSeen::new(),
            pin_sock: Mutex::new(None),
            logf,
        }
    }

    /// 挂公共端口 socket 的 dup（重钉面；装配期一次——QUIC 面 start 之前）。
    pub fn attach_socket(&self, sock: UdpSocket) {
        *self.pin_sock.lock().expect("pin_sock 锁中毒") = Some(sock);
    }

    /// 公共端口（dup 的本地端口 = QUIC 面的实际端口；未挂 = 0）。
    pub fn local_port(&self) -> u16 {
        self.pin_sock
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|s| s.local_addr().ok()))
            .map(|a| a.port())
            .unwrap_or(0)
    }

    /// 把公共端口 socket 重钉到指定网卡（绑卡看护换卡/重挑；原 `bind.repin_to` 同义——
    /// 两族都设、单栈容错）。socket 由 QUIC 面独占 ⇒ 本函数只做 `setsockopt`。
    pub fn repin_to(&self, index: u32, name: &str) -> std::io::Result<()> {
        use std::os::fd::AsRawFd as _;
        let g = self.pin_sock.lock().expect("pin_sock 锁中毒");
        match g.as_ref() {
            Some(s) => super::egress::pin_socket_to_iface(s.as_raw_fd(), index, name),
            None => Err(std::io::Error::other("公共端口 socket 未挂（QUIC 面未起）")),
        }
    }

    /// udpcap 能力位（③）。
    pub fn set_caps(&self, caps: u8) {
        self.caps.store(caps, Ordering::SeqCst);
    }

    /// 探测应答的端点列表段（②；已公布公网端点）。
    pub fn set_probe_endpoints(&self, eps: Vec<SocketAddr>) {
        *self.probe_endpoints.lock().expect("probe_endpoints 锁中毒") = eps;
    }

    /// 中继控制帧回调（注册腿线程接）。
    pub fn set_on_leg_frame(&self, f: OnLegFrame) {
        *self.on_leg_frame.lock().expect("on_leg_frame 锁中毒") = Some(f);
    }

    /// 起一次 STUN 观测：占位并给出事务 ID（调用方随即用 `ExitQuic::send_plain` 把
    /// `egress::stun_request(&txid, "")` 从**公共端口**发出）。已有一次在等 ⇒ `None`
    /// （观测周期分钟级，冲突即失败——原 `bind.stun_query` 的 `WouldBlock` 同义）。
    pub fn stun_begin(
        &self,
        reply: std::sync::mpsc::Sender<Option<SocketAddr>>,
    ) -> Option<[u8; 12]> {
        let mut g = self.stun.lock().expect("stun 锁中毒");
        if g.is_some() {
            return None;
        }
        let txid = super::egress::new_txid();
        *g = Some(StunWait { txid, reply });
        Some(txid)
    }

    /// 观测超时收位（调用方超时后清——防下一次查询被占位挡住）。
    pub fn stun_abort(&self) {
        *self.stun.lock().expect("stun 锁中毒") = None;
    }

    /// 公共端口上的一个入站包判定（QUIC 面线程调用；**只认三类**）：
    /// `Some(Consumed)` = 归本面（无应答）；`Some(Reply{…})` = 归本面且应答；`None` = 不是
    /// 本面的包（交 quinn）。
    pub fn handle(
        &self,
        buf: &[u8],
        src: SocketAddr,
    ) -> Option<homeway_quic::PlainOutcome> {
        use homeway_quic::PlainOutcome;
        // **源地址归一**（原 `ServerBind::recv_packet` 同义、同位置）：出口公共端口是
        // 双栈 socket（`[::]`）——v4 包的源地址是 v4-mapped 形态（`::ffff:x.y.z.w`），
        // 归一成纯 v4。下游三处比较/键（①注册腿线程的「控制帧源全等」判据 ②新源表
        // ③STUN 回执地址）都按纯 v4/v6 口径做——不归一会让**中继注册腿的回执被静默
        // 忽略**（C4 实测：注册永不确认 ⇒ 中继不给客户端 hint）。
        let src = crate::udpbatch::unmap_v4_in6(src);
        // ① STUN 观测应答（只认事务 ID 匹配的应答——不匹配的照旧不算本面）。
        if stun_looks_like_response(buf) {
            let mut g = self.stun.lock().expect("stun 锁中毒");
            if let Some(w) = g.as_ref() {
                if buf.get(8..20) == Some(&w.txid[..]) {
                    self.note_new_src(src, "STUN应答", buf.len());
                    let mapped = super::egress::parse_stun_response(buf).map(|(_, ap)| ap);
                    let wait = g.take().expect("刚判过 Some");
                    let _ = wait.reply.send(mapped);
                    return Some(PlainOutcome::Consumed);
                }
            }
            return None; // 不是本面在等的应答：交 quinn（会被当畸形 QUIC 丢弃）
        }

        // ② 参照点探测：明文一问一答（`probe::respond_ex` = 原真源，零改）。
        {
            let eps = self.probe_endpoints.lock().expect("probe_endpoints 锁中毒");
            if let Some(resp) = crate::probe::respond_ex(
                buf,
                &self.build,
                self.caps.load(Ordering::SeqCst),
                &eps,
            ) {
                drop(eps);
                self.note_new_src(src, "参照点探测", buf.len());
                return Some(PlainOutcome::Reply { dst: src, payload: resp });
            }
        }

        // ③ 腿帧（中继注册腿的回执走本端口）：type=3 = 中继控制帧（转发注册腿线程）；
        //    0xBB 前缀的其余 kind 在**公共端口上**都是异常（kind=5 只走腿 socket）——
        //    记账后丢弃（不喂 quinn：0xBB 的首字节固定位为 0，本就不是合法 QUIC 报文）。
        if buf.first() == Some(&legframe::FRAME_MAGIC) {
            let Some((kind, payload)) = legframe::decode_frame(buf) else {
                self.note_new_src(src, "畸形腿帧", buf.len());
                return Some(PlainOutcome::Consumed);
            };
            if kind == crate::relaywire::FRAME_TYPE_RELAY_REG {
                self.note_new_src(src, "腿帧type=3", buf.len());
                let mut g = self.on_leg_frame.lock().expect("on_leg_frame 锁中毒");
                if let Some(f) = g.as_mut() {
                    f(payload, src);
                }
                return Some(PlainOutcome::Consumed);
            }
            self.note_new_src(src, &format!("腿帧type={kind}"), buf.len());
            return Some(PlainOutcome::Consumed);
        }

        None // 不是本面的包（QUIC 报文）⇒ 交 quinn
    }

    /// 入站新源的首包一行（委托 [`SrcSeen`]——与腿面同源同文）。
    pub fn note_new_src(&self, src: SocketAddr, shape: &str, n: usize) {
        self.src_seen.note(&self.logf, src, shape, n);
    }

    /// [`homeway_quic::PlainDatagramHook`] 形态的钩子（装配期交给 QUIC 面）。
    pub fn hook(self: &Arc<Self>) -> homeway_quic::PlainDatagramHook {
        let me = Arc::clone(self);
        Arc::new(move |buf: &[u8], src: SocketAddr| me.handle(buf, src))
    }
}

/// STUN 应答快速判别（servercore/stun.go 同义：类型 0x0101 + magic cookie）。
fn stun_looks_like_response(b: &[u8]) -> bool {
    b.len() >= 20
        && u16::from_be_bytes([b[0], b[1]]) == 0x0101
        && u32::from_be_bytes([b[4], b[5], b[6], b[7]]) == 0x2112_A442
}
