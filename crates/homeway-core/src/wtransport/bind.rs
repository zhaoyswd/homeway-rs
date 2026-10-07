//! wtransport Bind：自管 UDP socket + 候选镜像 + 首回包采纳 + reg 搭车（唯一
//! `send_wg` 收口）+ 中继腿 + 直连优先窗口 + 漫游双发。
//!
//! 语义真源 `baseline:clientcore/internal/wtransport/bind.go`（R2 全量）：
//! - **未采纳时**出站 WG 包镜像到候选（直连优先窗口内只打直连）；首包搭容器帧
//!   `[reg][data]` 保 1 RTT（每个镜像数据报各带一份 reg）；
//! - **DirectFirst 解锁补发**（评审高-1/中-6）：窗口内首个未采纳出站包 + 该发搭车
//!   reg 被**捕获**；窗口到点仍无采纳 → 解锁中继候选并以容器帧 `[reg][pkt]` 重投
//!   （裸 data 帧的握手 init 会被出口按未知 pubkey 丢弃，且 reg_armed 已消费 ⇒
//!   不捕获就永久失注册——中继唯一可达时永远建不起来）；
//! - **收到任何来源的包即采纳**（先于帧解码），`relay_eps` 判定中继腿（via=relay +
//!   ⚠️ 告警行）；切到未知来源（非候选/非中继）→ 旧路径 10s 双发宽限（FIX-09）；
//! - **R1 已登记差异**：reg 搭车收口 = 「真正写出 UDP 数据报的那一次」（收紧语义）；
//!   UDP socket 绑 v4（Go 双栈承载 v6 端点）——本地实例端点恒 v4，v6 承载挂真机前补。
//!
//! 判据行：C4 MIRROR（双条件节流）、C5 赛跑结算、C6 路径确立/切换、RARM 家族、
//! REBIND、解锁补发行、中继告警行、发送失败限流行、接收读错误行、HINT 行。

use std::collections::{HashMap, HashSet};
use std::io;
use std::os::fd::AsRawFd as _;
use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::token::Secret;

use super::frame::{self, FrameKind};
use super::reg;
use crate::go_fmt::fmt_duration_go_ms;

/// 一条候选路径（relay 位参与候选集比较与镜像分派）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub addr: SocketAddr,
    pub relay: bool,
}

/// reg 搭车材料（身份公钥/标签 + token secret；Identity 不进 Bind——它属会话层）。
pub struct RegCtx {
    pub secret: Secret,
    pub pubkey: [u8; 32],
    pub dev_tag: [u8; 8],
}

/// 链路形态三态（`tunStatusJSON` link 段 via 词表：direct|relay|none）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Via {
    #[default]
    None,
    Direct,
    Relay,
}

impl Via {
    pub fn as_str(self) -> &'static str {
        match self {
            Via::None => "none",
            Via::Direct => "direct",
            Via::Relay => "relay",
        }
    }
}

/// 传输层状态快照（C10 `link:` 行的数据源）。
#[derive(Debug, Clone)]
pub struct Status {
    pub via: Via,
    pub ep: Option<SocketAddr>,
    pub mirrored: u64,
}

/// MIRROR 行节流参数（Go FIX-13）：每轮赛跑 ≤3 行、两行间隔 ≥1s。
const MIRROR_LOG_MAX: u32 = 3;
const MIRROR_LOG_GAP: Duration = Duration::from_secs(1);
/// 路径确立/切换行节流（3s）。
const PATH_LOG_GAP: Duration = Duration::from_secs(3);
/// 过渡双发宽限（FIX-09）：切到未知来源后旧路径保活时长。
const HANDOVER_GRACE: Duration = Duration::from_secs(10);
/// 接收读错误退避（Go bind.go:491：300ms 原地慢转 + 每 5s 一行）。
const RECV_BACKOFF: Duration = Duration::from_millis(300);
const RECV_ERR_LOG_GAP: Duration = Duration::from_secs(5);
/// 发送失败限流（每目标 5s 一条）。
const SEND_ERR_LOG_GAP: Duration = Duration::from_secs(5);
/// 未采纳期 reg 补投间隔（F3）：首包丢/时钟偏差不再干等分钟级恢复阶梯。
const REG_RESEND_INTERVAL: Duration = Duration::from_secs(2);
/// 未采纳期 reg 补投**次数上限**（F3×F1 交互，评审 N1）：隧道坏但出站可达的客户端
/// 若无限补投，每次 reg 都会刷新出口设备表的 `last_reg` ⇒ 条目永不 stale、永不淘汰，
/// 在 `max_devices` 表里长期占位（反噬 F1）。到上限即交恢复阶梯。
const REG_RESEND_MAX: u32 = 15;
/// 未采纳期 reg 补投**时长上限**（自首次补投起）。
const REG_RESEND_WINDOW: Duration = Duration::from_secs(60);

/// 直连优先窗口缺省（Go directFirst 0→2s 同义）。
pub const DIRECT_FIRST_DEFAULT: Duration = Duration::from_secs(2);

/// 中继路由键（Go proto.RelayID：peer 公钥的 8 字节压缩 = SHA-256(pubkey)[..8]）。
pub fn relay_id(peer_pub: &[u8; 32]) -> [u8; 8] {
    use sha2::{Digest, Sha256};
    let sum = Sha256::digest(peer_pub);
    let mut id = [0u8; 8];
    id.copy_from_slice(&sum[..8]);
    id
}

/// hint 回调（中继观察到的地址线索；回调里不得阻塞——磁盘落盘走去抖线程）。
pub type OnHint = Arc<dyn Fn(&str) + Send + Sync>;

pub struct Bind {
    sock: UdpSocket,
    /// socket 是 AF_INET6 **双栈**（V6ONLY=0——Go 客户端 `conn.Bind` 的双栈 socket
    /// 同义：v6 候选可发、v4 对端可达；发送面按族 map，接收面 unmap 归一）。
    dual: bool,
    candidates: Vec<Candidate>,
    /// 中继端点表（与候选集同临界区重建——FIX-10：收包路径的 relay 判定不吃旧表）。
    relay_eps: HashSet<SocketAddr>,
    relay_id: [u8; 8],
    /// 直连优先窗口（None = 关：全部候选同时打）。
    direct_first: Option<Duration>,
    reg: Option<RegCtx>,
    reg_armed: bool,
    /// 上次真正写出搭车 reg 的时刻（F3：未采纳期 2s 补投的节拍基准）。
    last_reg_sent: Option<Instant>,
    /// 未采纳期补投计数与起点（F3 上界；rearm 归零）。
    reg_resend_n: u32,
    reg_resend_start: Option<Instant>,
    /// 补投节拍参数（F3；生产恒为 `REG_RESEND_INTERVAL`/`REG_RESEND_WINDOW`——
    /// 测试可注入小值，避免墙钟 sleep 与负载假红，见代码门 L5/M3）。
    reg_resend_interval: Duration,
    reg_resend_window: Duration,
    adopted: Option<SocketAddr>,
    adopted_is_relay: bool,
    /// 【测试缝】中继锁定（relay-lock 注入）：模拟「直连路径全被 NAT 丢弃」的真机
    /// 中继形态——非中继源的包按「从未到达」处理（不采纳不学习）。同机回环上
    /// 直连永远通（连出口的盲打都能把客户端采纳翻成直连），中继驻留/升级条纹
    /// 不注入就测不出来。
    pub(crate) relay_only: bool,
    mirrored: u64,
    race_start: Instant,
    relay_unlocked: bool,
    /// DirectFirst 解锁补发的捕获对（本轮首个未采纳出站包 + 该发搭车 reg；
    /// unlock_once 语义：捕获一次后不再覆盖，采纳/重 arm 清空）。
    unlock_captured: Option<(Vec<u8>, Option<Vec<u8>>)>,
    race_seen: HashSet<SocketAddr>,
    mirror_log_n: u32,
    mirror_log_at: Option<Instant>,
    last_path_log_at: Option<Instant>,
    /// 过渡双发（FIX-09）：旧路径 + 是否中继 + 宽限期。
    handover: Option<(SocketAddr, bool, Instant)>,
    rx_bytes: u64,
    tx_bytes: u64,
    send_errs: u64,
    /// 全候选发送统计（拍板①：Go sendTries/sendLocalFails——尝试>0 且全部本地失败
    /// = 环境性禁发〔挂起 EPERM 全候选皆败；蜂窝下 LAN 候选 ENETUNREACH 但中继发
    /// 得出去 ⇒ 不算全失败〕，覆盖非采纳〔赛跑〕态下采纳路径粘性信号够不着的盲区）。
    send_tries: i64,
    send_local_fails: i64,
    /// 本地发送错误累计（(采纳路径, 全部)——Go adoptedLocalErrCount/localErrCount）。
    adopted_local_err_count: u64,
    local_err_count: u64,
    last_local_send_err: Option<Instant>,
    send_err_log_at: HashMap<SocketAddr, Instant>,
    /// 接收读错误退避（到点前跳过收包；poll 超时参与——持续 POLLERR 不空转）。
    recv_backoff_until: Option<Instant>,
    recv_err_log_at: Option<Instant>,
    logf: crate::Logf,
    on_hint: Option<OnHint>,
    /// 收包缓冲（UDP 数据报上界 64KB，构造时一次分配）。
    recv_buf: Box<[u8; 65536]>,
    /// 测试缝位：模拟「socket 被 OS 作废」（冻结唤醒形态）——置位后收/发全部报
    /// EBADF 类错误，`rebind` 换新 socket 即清除（阶梯 R2 档的注入面；
    /// Bind 层模拟而非真关 fd——避免 fd 复用双关的误伤面）。
    #[cfg(feature = "test-seams")]
    poisoned: bool,
}

impl Bind {
    /// 装配并打开 UDP socket（v4 随机端口，非阻塞——驱动线程经 poll 唤醒）。
    /// `direct_first`：`Some(0)` = 取缺省 2s；`None` = 显式关（全部候选同时打）。
    pub fn open(
        candidates: &[Candidate],
        reg: Option<RegCtx>,
        direct_first: Option<Duration>,
        peer_pub: &[u8; 32],
        logf: crate::Logf,
    ) -> io::Result<Self> {
        let (sock, dual) = crate::udpbatch::open_client_socket()?;
        enlarge_udp_bufs(&sock);
        sock.set_nonblocking(true)?;
        let mut b = Self {
            sock,
            dual,
            candidates: candidates.to_vec(),
            relay_eps: candidates
                .iter()
                .filter(|c| c.relay)
                .map(|c| c.addr)
                .collect(),
            relay_id: relay_id(peer_pub),
            // F11：`None` 保持 `None` = **显式关**（全部候选同时打）；`Some(0)` → 缺省 2s
            // （Go `core.go:117-124` 的 `0 → 2s` 同义）。此前 `map_or(Some(DEFAULT), …)`
            // 把 `None` 与 `Some(0)` 都映射成 `Some(DEFAULT)`——文档语义「None = 关」无实现。
            direct_first: direct_first.map(|d| if d.is_zero() { DIRECT_FIRST_DEFAULT } else { d }),
            reg,
            reg_armed: true,
            last_reg_sent: None,
            reg_resend_n: 0,
            reg_resend_start: None,
            reg_resend_interval: REG_RESEND_INTERVAL,
            reg_resend_window: REG_RESEND_WINDOW,
            adopted: None,
            adopted_is_relay: false,
            relay_only: false,
            mirrored: 0,
            race_start: Instant::now(),
            relay_unlocked: false,
            unlock_captured: None,
            race_seen: HashSet::new(),
            mirror_log_n: 0,
            mirror_log_at: None,
            last_path_log_at: None,
            handover: None,
            rx_bytes: 0,
            tx_bytes: 0,
            send_errs: 0,
            send_tries: 0,
            send_local_fails: 0,
            adopted_local_err_count: 0,
            local_err_count: 0,
            last_local_send_err: None,
            send_err_log_at: HashMap::new(),
            recv_backoff_until: None,
            recv_err_log_at: None,
            logf,
            on_hint: None,
            recv_buf: Box::new([0u8; 65536]),
            #[cfg(feature = "test-seams")]
            poisoned: false,
        };
        b.relay_unlocked = b.direct_candidates().is_empty(); // 没有直连候选就没什么可等的
        if b.candidates.is_empty() {
            (b.logf)("wtransport: 无任何候选（token 端点为空——无法建连）");
        }
        Ok(b)
    }

    fn direct_candidates(&self) -> Vec<Candidate> {
        self.candidates
            .iter()
            .copied()
            .filter(|c| !c.relay)
            .collect()
    }

    pub fn local_port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub fn set_on_hint(&mut self, h: OnHint) {
        self.on_hint = Some(h);
    }

    /// 【测试缝】中继锁定（relay-lock——见 Bind::relay_only 注释）。
    pub fn set_relay_only(&mut self) {
        self.relay_only = true;
    }

    /// 接收读错误退避余量（驱动线程 poll 超时参与——持续错误下不空转）。
    pub fn recv_backoff_remain(&self) -> Option<Duration> {
        self.recv_backoff_until
            .map(|t| t.saturating_duration_since(Instant::now()))
    }

    /// 唯一出站收口：所有要写上网络的 WG 包都经这里（封装腿帧 + reg 搭车 + 镜像/采纳
    /// 分派 + 中继路由头）。**reg 只在首个真正写出的数据报上搭车**（不论这个包由哪个
    /// TunnResult 分支产出——encapsulate/update_timers/decapsulate/send_queued 四来源全覆盖）。
    pub fn send_wg(&mut self, wg: &[u8]) {
        #[cfg(feature = "test-seams")]
        if self.poisoned {
            self.send_errs += 1;
            if self.send_errs < 10 {
                (self.logf)("[test-seam] 发送失败：socket 已置为失效形态（EBADF 模拟）");
            }
            return;
        }
        if let Some(addr) = self.adopted {
            let mut wire = Vec::with_capacity(wg.len() + 16);
            wire.clear();
            frame::encode_frame(FrameKind::Data, wg, &mut wire);
            let wire = self.tag_relay(self.adopted_is_relay, wire);
            // 发送统计（拍板①：采纳路径单发 = 一次尝试）
            self.send_tries += 1;
            match self.sock.send_to(&wire, crate::udpbatch::xmit_addr(addr, self.dual)) {
                Ok(_) => self.tx_bytes += wg.len() as u64, // 成功才计（Go bind.go:644-651）
                Err(e) => {
                    self.send_errs += 1;
                    self.send_local_fails += 1;
                    // 只进采纳面累计（复核 r3-F5：Go noteSendErr 只加
                    // adoptedLocalErrCount——localErrTotal 是镜像候选专属口径）
                    self.adopted_local_err_count += 1;
                    self.last_local_send_err = Some(Instant::now());
                    self.log_send_err_throttled(addr, &e);
                }
            }
            // 过渡双发（FIX-09）：尽力语义，不进发送计数
            if let Some((old, old_relay, until)) = self.handover {
                if Instant::now() < until && old != addr {
                    let mut wire2 = Vec::with_capacity(wg.len() + 16);
                    frame::encode_frame(FrameKind::Data, wg, &mut wire2);
                    let wire2 = self.tag_relay(old_relay, wire2);
                    let _ = self.sock.send_to(&wire2, crate::udpbatch::xmit_addr(old, self.dual));
                }
            }
            return;
        }
        // ---- 未采纳：镜像到候选（直连优先窗口） ----
        self.mirrored += 1;
        let now = Instant::now();
        let relay_ok = self.relay_unlocked
            || self.direct_candidates().is_empty()
            || self
                .direct_first
                .is_none_or(|d| d.is_zero() || now.duration_since(self.race_start) >= d);
        if relay_ok && !self.relay_unlocked {
            self.relay_unlocked = true;
        }
        // 捕获（unlock_once）：窗口锁定期的首个未采纳出站包 + 该发搭车 reg
        let need_capture = !relay_ok && self.unlock_captured.is_none();
        let was_armed = self.reg_armed;
        let reg_pkt = self.peek_reg();
        let mut frame_bytes =
            Vec::with_capacity(wg.len() + reg_pkt.as_ref().map_or(0, |r| r.len()) + 16);
        match &reg_pkt {
            Some(r) => {
                frame::encode_batch(
                    &[
                        (FrameKind::Reg.to_wire(), r),
                        (FrameKind::Data.to_wire(), wg),
                    ],
                    &mut frame_bytes,
                );
            }
            None => frame::encode_frame(FrameKind::Data, wg, &mut frame_bytes),
        }
        let mut sent = 0usize;
        let mut relay_sent = 0usize;
        let mut send_errs: Vec<(SocketAddr, io::Error)> = Vec::new();
        for c in &self.candidates {
            if c.relay && !relay_ok {
                continue;
            }
            let wire: Vec<u8> = if c.relay {
                self.tag_relay(true, frame_bytes.clone())
            } else {
                frame_bytes.clone()
            };
            // 发送统计（拍板①：镜像逐候选 = 每候选一次尝试；本地失败逐次累计）
            self.send_tries += 1;
            match self.sock.send_to(&wire, crate::udpbatch::xmit_addr(c.addr, self.dual)) {
                Ok(_) => {}
                Err(e) => {
                    self.send_errs += 1;
                    self.send_local_fails += 1;
                    self.local_err_count += 1;
                    send_errs.push((c.addr, e));
                }
            }
            sent += 1; // 尝试数（Go writeCandidate 后无条件 sent++——本地不可达也计入，C4 同数字）
            if c.relay {
                relay_sent += 1;
            }
        }
        for (addr, e) in &send_errs {
            self.log_send_err_throttled(*addr, e);
        }
        if sent > 0 {
            // 一条逻辑出站包按包记一次（Go bind.go:686-687 无条件计数——reg 是否搭车
            // 不参与计数条件；评审中-6）
            self.tx_bytes += wg.len() as u64;
            if reg_pkt.is_some() {
                self.reg_armed = false; // 真正写出才消费（R1 评审中-1 的收紧语义）
                self.last_reg_sent = Some(now);
                if !was_armed {
                    // F3：未采纳期补投（首包丢/时钟偏差的自愈）——计入上界。
                    self.reg_resend_n += 1;
                    if self.reg_resend_start.is_none() {
                        self.reg_resend_start = Some(now);
                    }
                }
            }
        }
        if need_capture {
            self.unlock_captured = Some((wg.to_vec(), reg_pkt));
        }
        // C4 判据行（双条件节流：≤3 行/轮 且 间隔 ≥1s）
        let loggable = self.mirror_log_n < MIRROR_LOG_MAX
            && self
                .mirror_log_at
                .is_none_or(|t| now.duration_since(t) >= MIRROR_LOG_GAP);
        if loggable {
            self.mirror_log_n += 1;
            self.mirror_log_at = Some(now);
            (self.logf)(&format!(
                "MIRROR 镜像包#{} → {} 候选（直连优先：本次直连 {} / 中继 {}；本行每轮限 3 条）",
                self.mirrored,
                sent,
                sent - relay_sent,
                relay_sent
            ));
        }
    }

    /// 中继腿封装：`[0xAA][relayID(8B)] ‖ 已编码腿帧`（容器帧在内、路由头在外——
    /// Go EncodeTaggedFrame 包已编码帧，bind.go:733-746 同序）。
    fn tag_relay(&self, is_relay: bool, mut frame_bytes: Vec<u8>) -> Vec<u8> {
        if !is_relay {
            return frame_bytes;
        }
        let mut out = Vec::with_capacity(9 + frame_bytes.len());
        out.push(0xAA);
        out.extend_from_slice(&self.relay_id);
        out.append(&mut frame_bytes);
        out
    }

    /// DirectFirst 解锁检查（驱动线程每拍调）：窗口到点仍无采纳 → 解锁中继 + 以捕获对
    /// 重投（`[0xAA]‖[容器帧[reg][pkt]]`——reg 必须随补发重投，评审高-1）。
    pub fn tick_unlock(&mut self) {
        if self.adopted.is_some() || self.relay_unlocked {
            return;
        }
        let Some(window) = self.direct_first.filter(|d| !d.is_zero()) else {
            return;
        };
        if Instant::now().duration_since(self.race_start) < window {
            return;
        }
        self.relay_unlocked = true;
        let Some((pkt, reg)) = self.unlock_captured.take() else {
            return;
        };
        let relay_cands: Vec<SocketAddr> = self
            .candidates
            .iter()
            .filter(|c| c.relay)
            .map(|c| c.addr)
            .collect();
        if relay_cands.is_empty() {
            return;
        }
        let mut frame_bytes =
            Vec::with_capacity(pkt.len() + reg.as_ref().map_or(0, |r| r.len()) + 16);
        match &reg {
            Some(r) => frame::encode_batch(
                &[
                    (FrameKind::Reg.to_wire(), r),
                    (FrameKind::Data.to_wire(), &pkt),
                ],
                &mut frame_bytes,
            ),
            None => frame::encode_frame(FrameKind::Data, &pkt, &mut frame_bytes),
        }
        let wire = self.tag_relay(true, frame_bytes);
        for c in &relay_cands {
            // 发送统计（复核 r3-F5：Go writeUDP 是统一计数点——注释明写「含解锁补发」；
            // FIX-09 的「不进计数」只覆盖 sendToSilent 过渡双发。漏计会让「蜂窝下
            // LAN 全败 + 解锁补发到中继成功」被误判成环境性禁发〔判据反转〕）
            self.send_tries += 1;
            if let Err(e) = self.sock.send_to(&wire, crate::udpbatch::xmit_addr(*c, self.dual)) {
                self.send_errs += 1;
                self.send_local_fails += 1;
                self.local_err_count += 1;
                let _ = e;
            }
        }
        (self.logf)(&format!(
            "MIRROR 直连窗口 {} 内无响应 → 解锁中继候选 {} 个并补发一次",
            fmt_duration_go_ms(window),
            relay_cands.len(),
        ));
    }

    /// 预取本轮搭车 reg（不消费 arm——消费点在「真正写出」之后，见 send_wg）。
    /// F3：arm 已消费后，未采纳期按 `REG_RESEND_INTERVAL`（2s）**补投**（受次数/时长
    /// 上界约束）；已采纳后不再补投（send_wg 的采纳分支根本不调本函数）。
    fn peek_reg(&mut self) -> Option<Vec<u8>> {
        if !self.reg_armed && !self.reg_resend_due() {
            return None;
        }
        let ctx = self.reg.as_ref()?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut pkt = Vec::with_capacity(reg::REG_LEN);
        reg::encode_reg_parts(&ctx.secret, &ctx.pubkey, &ctx.dev_tag, now, &mut pkt);
        Some(pkt)
    }

    /// 未采纳期是否到点补投 reg（F3）：有 reg、未采纳、间隔到、未超次数/时长上界。
    /// 上界是**每次未采纳期**口径（rearm 归零——恢复阶梯重开一轮，见登记 E8 行）。
    fn reg_resend_due(&self) -> bool {
        if self.reg.is_none() || self.adopted.is_some() {
            return false;
        }
        if self.reg_resend_n >= REG_RESEND_MAX {
            return false;
        }
        if self.reg_resend_start.is_some_and(|t| t.elapsed() >= self.reg_resend_window) {
            return false;
        }
        self.last_reg_sent.is_some_and(|t| t.elapsed() >= self.reg_resend_interval)
    }

    /// 收一个数据报：退避检查 → 采纳（先于帧解码）→ 腿帧解码。
    /// **返回契约**（驱动循环的 drain 语义）：`Ok(Some(n))` = 数据帧载荷已拷入 buf；
    /// `Ok(None)` = 本包被内部消费（继续收）；`Err(WouldBlock|TimedOut)` = 本轮无包
    /// （结束 drain）；其它 `Err` = 读错误（驱动循环记退避 + 限流日志，原地慢转等
    /// 换源/收工——Go bind.go:474-492）。
    pub fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        #[cfg(feature = "test-seams")]
        if self.poisoned {
            self.note_recv_err(&io::Error::from_raw_os_error(libc::EBADF));
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        if self.recv_backoff_until.is_some_and(|t| Instant::now() < t) {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "读退避中"));
        }
        let (n, src_raw) = match self.sock.recv_from(&mut self.recv_buf[..]) {
            Ok(v) => v,
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                return Err(e)
            }
            Err(e) => {
                self.note_recv_err(&e);
                return Err(e);
            }
        };
        self.rx_bytes += n as u64;
        if crate::envflag::tx_dbg() {
            eprintln!("[RXDBG] client got {n}B from {src_raw} first={:02x?}", &self.recv_buf[..n.min(8)]);
        }
        // 双栈 socket 上 v4 对端的源地址是 v4-mapped v6——归一成纯 v4（内部表示
        // 恒纯 v4/v6：采纳/候选比对/应答回发不因 socket 族漂移）
        let src = crate::udpbatch::unmap_v4_in6(src_raw);
        // F2：**只在解出 Data 帧时采纳**。`decode_frame` 只判 `len>=2 && buf[0]==0xBB`
        // 且未知 kind 原样返回 ⇒「解码成功」几乎无门槛（`[0xBB, 任意 1 字节]` 即通过），
        // 任意来源（含 1 字节垃圾）都能触发采纳。收紧后：Control(hint)/未知 kind/垃圾
        // 一律**不 adopt**（不写 adopted/race_seen、不打 C5/C6、不登记 handover）；
        // 真正的漫游/自愈仍由「未知来源 + 合法 Data 帧」成立（与 Go bind.go:511-515
        // 自注的收紧方向同向）。
        if frame::frame_kind(&self.recv_buf[..n]) == Some(FrameKind::Data.to_wire()) {
            self.adopt(src);
        }
        let Some((kind, payload)) = frame::decode_frame(&self.recv_buf[..n]) else {
            return Ok(None); // 非帧包（垃圾/旧对端）：丢弃
        };
        match kind {
            k if k == FrameKind::Data.to_wire() => {
                let len = payload.len().min(buf.len());
                buf[..len].copy_from_slice(&payload[..len]);
                Ok(Some(len))
            }
            k if k == FrameKind::Control.to_wire() => {
                // hint 线索（学习缓存接线：回调只改内存 + 投递落盘信号，不阻塞收包热路径）
                if let Some(addr) = frame::decode_hint_payload(payload) {
                    // F2：hint **不构成路径证据**（上面已不 adopt），但仍是候选学习线索。
                    // 未知来源的 hint 必须过 `probe_addr_acceptable`——封掉「谁能发一个
                    // Control 帧就让客户端向任意地址打洞/污染候选表」的注入面；已知来源
                    // （候选/中继腿）豁免（其地址本就可信，且本机回环候选会被卫兵误杀）。
                    let known = self.relay_eps.contains(&src)
                        || self.candidates.iter().any(|c| c.addr == src);
                    let ok = known
                        || addr
                            .parse::<SocketAddr>()
                            .is_ok_and(|ap| crate::probe::probe_addr_acceptable(&ap));
                    if !ok {
                        (self.logf)(&format!(
                            "HINT 忽略未知来源 {src} 的线索 {addr}（地址不可接受——防投喂注入）"
                        ));
                        return Ok(None);
                    }
                    match &self.on_hint {
                        Some(_) => {
                            (self.logf)(&format!("HINT 收到对端地址线索 {addr}（来自 {src}）"));
                        }
                        None => {
                            (self.logf)(&format!(
                                "HINT 收到对端地址线索 {addr}，但没有处理器（缓存未接？）"
                            ));
                        }
                    }
                    if let Some(h) = &self.on_hint {
                        h(addr);
                    }
                }
                Ok(None)
            }
            _ => Ok(None), // reg（服务端概念）/容器（服务端搭车用）/未知：忽略
        }
    }

    fn note_recv_err(&mut self, e: &io::Error) {
        self.recv_backoff_until = Some(Instant::now() + RECV_BACKOFF);
        let now = Instant::now();
        if self
            .recv_err_log_at
            .is_none_or(|t| now.duration_since(t) >= RECV_ERR_LOG_GAP)
        {
            self.recv_err_log_at = Some(now);
            (self.logf)(&format!(
                "接收读错误（{e}）：原地重试等换源/收工（不交回 wg-go——读 goroutine 死亡=永久失聪）"
            ));
        }
    }

    /// 采纳来源 + 判据行（C5 赛跑结算一次 / C6 路径确立·切换 3s 节流 / 中继告警 /
    /// handover 登记）。
    fn adopt(&mut self, src: SocketAddr) {
        if self.relay_only && !self.relay_eps.contains(&src) {
            return; // 测试缝：直连形态按「从未到达」处理（见字段注释）
        }
        let was_valid = self.adopted.is_some();
        let prev = self.adopted;
        let prev_was_relay = self.adopted_is_relay;
        let is_relay = self.relay_eps.contains(&src);
        self.adopted = Some(src);
        self.adopted_is_relay = is_relay;
        self.race_seen.insert(src);
        let now = Instant::now();
        if !was_valid {
            // 赛跑结算：胜者 / 响应过的候选 / 全程未响应的候选（本轮仅一次）
            let mut ok_list = Vec::new();
            let mut miss_list = Vec::new();
            for c in &self.candidates {
                if self.race_seen.contains(&c.addr) {
                    ok_list.push(c.addr.to_string());
                } else {
                    miss_list.push(format!("{} {}", candidate_tag(c.addr, c.relay), c.addr));
                }
            }
            (self.logf)(&format!(
                "赛跑结算：胜出 {} {}（镜像 {} 包，耗时 {}）；响应过={}；未响应={}",
                path_kind(is_relay),
                src,
                self.mirrored,
                fmt_duration_go_ms(now.duration_since(self.race_start)),
                ok_list.join("、"),
                miss_list.join("、"),
            ));
        }
        // 过渡双发登记（FIX-09）：稳态下切到「未知来源」= 保留旧路径宽限双发；
        // 切到候选/中继（hint/probe 学习路径）或首次采纳 = 清过渡。
        if was_valid && prev != Some(src) {
            let known = is_relay || self.candidates.iter().any(|c| c.addr == src);
            if !known {
                self.handover = Some((prev.unwrap(), prev_was_relay, now + HANDOVER_GRACE));
            } else {
                self.handover = None;
            }
        }
        let path_changed = prev != Some(src);
        if path_changed
            && self
                .last_path_log_at
                .is_none_or(|t| now.duration_since(t) >= PATH_LOG_GAP)
        {
            self.last_path_log_at = Some(now);
            if !was_valid {
                (self.logf)(&format!(
                    "路径确立：{} {}（首个回包来源）",
                    path_kind(is_relay),
                    src
                ));
            } else if let Some(p) = prev {
                (self.logf)(&format!(
                    "路径切换：{} {} → {} {}",
                    path_kind(prev_was_relay),
                    p,
                    path_kind(is_relay),
                    src
                ));
            }
        }
        // 「走中继」= 需要排查的 bug（口径 2026-09-19）：在采纳点记（100% 看到每次跳变）
        if is_relay && !prev_was_relay {
            (self.logf)(&format!(
                "⚠️ 链路走了中继（本应直连，属需排查的 bug）：ep={}，直连候选 {} 个全未响应（可能原因：直连地址不可达 / 出口公网映射失效 / 打洞失败）",
                src,
                self.direct_candidates().len(),
            ));
        }
    }

    /// 独立补发一条注册报文（出口重启/设备记录被回收后保活；不依赖 WG 会话、
    /// 不改采纳状态）。中继腿 = 带路由头的 reg 腿帧（Go RefreshReg 同构）。
    pub fn refresh_reg(&mut self) -> bool {
        let Some(addr) = self.adopted else {
            return false;
        };
        let Some(ctx) = self.reg.as_ref() else {
            return false;
        };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut pkt = Vec::with_capacity(reg::REG_LEN);
        reg::encode_reg_parts(&ctx.secret, &ctx.pubkey, &ctx.dev_tag, now, &mut pkt);
        let mut wire = Vec::with_capacity(pkt.len() + 16);
        frame::encode_frame(FrameKind::Reg, &pkt, &mut wire);
        let wire = self.tag_relay(self.adopted_is_relay, wire);
        // 发送统计（复核 r3-F5：Go writeUDP 统一计数点覆盖 RREG 腿；localErr* 不动——
        // Go 的 localErrTotal 只含镜像候选〔bind_test 断言口径〕，采纳路径错误只进
        // adoptedLocalErrCount）
        self.send_tries += 1;
        if let Err(e) = self.sock.send_to(&wire, crate::udpbatch::xmit_addr(addr, self.dual)) {
            self.send_errs += 1;
            self.send_local_fails += 1;
            (self.logf)(&format!("RREG 注册刷新发送失败（{e}）"));
            return false;
        }
        (self.logf)(&format!(
            "RREG 注册刷新 → {}（dev={}，中继={}）",
            addr,
            crate::identity::DevTag(ctx.dev_tag).short(),
            self.adopted_is_relay,
        ));
        true
    }

    pub fn status(&self) -> Status {
        Status {
            via: match (self.adopted, self.adopted_is_relay) {
                (None, _) => Via::None,
                (Some(_), false) => Via::Direct,
                (Some(_), true) => Via::Relay,
            },
            ep: self.adopted,
            mirrored: self.mirrored,
        }
    }

    pub fn adopted(&self) -> Option<SocketAddr> {
        self.adopted
    }

    pub fn adopted_is_relay(&self) -> bool {
        self.adopted_is_relay
    }

    pub fn rx_tx(&self) -> (u64, u64) {
        (self.rx_bytes, self.tx_bytes)
    }

    /// 全候选发送统计的生命周期累计（拍板①：(尝试数, 本地失败数)——Go sendTries/
    /// sendLocalFails 的累计面；主线程经差分消费〔swap 语义〕）。计数面 = 采纳路径
    /// 单发 + 未采纳镜像逐候选 + 解锁补发逐候选 + RREG 腿（Go writeUDP 统一计数点
    /// 全覆盖——复核 r3-F5 更正：此前把解锁补发排除并误引 FIX-09）；**不计数**的
    /// 只有过渡双发 sendToSilent 的尽力路径（Go FIX-09 的实际排除面）。
    pub fn send_stats_pending(&self) -> (i64, i64) {
        (self.send_tries, self.send_local_fails)
    }

    /// 本地发送错误累计（(采纳路径, 镜像全部)——Go adoptedLocalErrCount/localErrCount；
    /// tunStatusJSON demand.localErr* 两键源。localErrTotal 是**镜像候选专属**口径
    /// （Go bind_test 断言：采纳路径错误只进 adoptedLocalErrCount——复核 r3-F5）。
    pub fn local_err_counters(&self) -> (u64, u64) {
        (self.adopted_local_err_count, self.local_err_count)
    }

    pub fn socket(&self) -> &UdpSocket {
        &self.sock
    }

    /// 最近 d 内是否发生过**采纳路径**本地类发送错误（Go LocalSendErrWithin）。
    pub fn local_send_err_within(&self, d: Duration) -> bool {
        self.last_local_send_err.is_some_and(|t| t.elapsed() < d)
    }

    pub fn last_local_send_err_at(&self) -> Option<Instant> {
        self.last_local_send_err
    }

    /// 换本地 socket（Go Rebind 同义：不换 Identity、不动采纳——漫游 = 同钥换源地址，
    /// 会话保持）。新 socket 非阻塞；旧 socket 关闭（驱动线程 poll 的 fd 由每轮重取跟随）。
    pub fn rebind(&mut self) -> io::Result<u16> {
        let (sock, dual) = crate::udpbatch::open_client_socket()?;
        enlarge_udp_bufs(&sock);
        sock.set_nonblocking(true)?;
        self.dual = dual;
        let old = std::mem::replace(&mut self.sock, sock);
        let port = self.sock.local_addr().map(|a| a.port()).unwrap_or(0);
        drop(old);
        #[cfg(feature = "test-seams")]
        {
            self.poisoned = false;
        }
        self.recv_backoff_until = None; // 新 socket 的首个错误要能立刻打出日志
        (self.logf)(&format!("REBIND 本地端口 → {port}（Identity 不变）"));
        Ok(port)
    }

    /// 更新候选集（Go SetCandidates 同义）：忽略序比较（relay 位参与——FIX-14：腿类型
    /// 变化也是「集合变了」）；变化打一行；relay_eps 同临界区重建（FIX-10）。
    pub fn set_candidates(&mut self, cands: Vec<Candidate>) {
        let changed = !same_candidates(&self.candidates, &cands);
        self.candidates = cands;
        self.relay_eps = self
            .candidates
            .iter()
            .filter(|c| c.relay)
            .map(|c| c.addr)
            .collect();
        if changed {
            let parts: Vec<String> = self
                .candidates
                .iter()
                .map(|c| format!("{}（{}）", c.addr, candidate_tag(c.addr, c.relay)))
                .collect();
            (self.logf)(&format!(
                "候选集更新：{} 条（{}）",
                self.candidates.len(),
                parts.join("、")
            ));
        }
    }

    /// 重启一轮**新鲜赛跑**（Go Rearm：直连优先，窗口内只打直连）。
    pub fn rearm(&mut self) {
        self.adopted = None;
        self.adopted_is_relay = false;
        self.reg_armed = true;
        self.last_reg_sent = None;
        self.reg_resend_n = 0;
        self.reg_resend_start = None;
        self.race_start = Instant::now();
        self.relay_unlocked = self.direct_candidates().is_empty();
        self.unlock_captured = None;
        self.race_seen.clear();
        self.mirror_log_n = 0;
        self.mirror_log_at = None;
        self.handover = None;
        (self.logf)(&match self.direct_first {
            Some(d) => format!(
                "RARM 候选赛跑重启（直连优先：中继在 {} 后才解锁）",
                fmt_duration_go_ms(d)
            ),
            // F11：`None` = 显式关（中继立即参与）——旧文案 `unwrap_or(DEFAULT)` 会打
            // 「中继在 2s 后才解锁」，与语义相反。
            None => "RARM 候选赛跑重启（直连优先：关——中继立即参与）".to_owned(),
        });
    }

    /// 软赛跑（Go RearmSoft：中继立即参与——「已在用中继、只想试着升直连」的时刻，
    /// 不能因等直连把在用的中继路径停掉）。
    pub fn rearm_soft(&mut self) {
        self.adopted = None;
        self.adopted_is_relay = false;
        self.reg_armed = true;
        self.last_reg_sent = None;
        self.reg_resend_n = 0;
        self.reg_resend_start = None;
        self.race_start = Instant::now();
        self.relay_unlocked = true;
        self.unlock_captured = None;
        self.race_seen.clear();
        self.mirror_log_n = 0;
        self.mirror_log_at = None;
        (self.logf)("RARM 软赛跑（中继立即参与，同时试直连）");
    }

    /// 发送失败一行（每目标 5s 条；Go logSendErr 同串同节流）。
    fn log_send_err_throttled(&mut self, addr: SocketAddr, e: &io::Error) {
        let now = Instant::now();
        let due = self
            .send_err_log_at
            .get(&addr)
            .is_none_or(|t| now.duration_since(*t) >= SEND_ERR_LOG_GAP);
        if due {
            self.send_err_log_at.insert(addr, now);
            (self.logf)(&format!(
                "发送失败：{e}（{addr}；本地错误=该候选在本机就发不出去，与对端无响应是两回事）"
            ));
        }
    }

    /// 测试缝：模拟 socket 失效（见字段注释；`rebind` 解除）。
    #[cfg(feature = "test-seams")]
    pub fn poison_socket_for_test(&mut self) {
        self.poisoned = true;
        (self.logf)("[test-seam] UDP socket 已置为失效形态（EBADF 模拟）");
    }
}

fn path_kind(relay: bool) -> &'static str {
    if relay {
        "中继"
    } else {
        "直连"
    }
}

/// 集合比较（忽略顺序；relay 位参与）。**单一实现**——domain_eps 的重解析编排共用
/// （F11：此前 domain_eps 有第二份非多重集实现：`a=[A,A,B]` vs `b=[A,B,B]` 会误判相等）。
pub(crate) fn same_candidates(a: &[Candidate], b: &[Candidate]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut sa: Vec<(SocketAddr, bool)> = a.iter().map(|c| (c.addr, c.relay)).collect();
    let mut sb: Vec<(SocketAddr, bool)> = b.iter().map(|c| (c.addr, c.relay)).collect();
    sa.sort();
    sb.sort();
    sa == sb
}

/// 候选一行标签（C5 未响应列表的 tag：中继/IPv6/LAN/公网v4；Go candidateTag 同义）。
/// **单一实现**（session/tun_exec 的候选行共用——r1-M5：三处两口径曾漂移）。
pub(crate) fn candidate_tag(ap: SocketAddr, relay: bool) -> &'static str {
    if relay {
        return "中继";
    }
    match ap {
        SocketAddr::V6(_) => "IPv6",
        SocketAddr::V4(v4) => {
            let ip = v4.ip();
            if ip.is_private() || ip.is_loopback() || ip.is_link_local() {
                "LAN"
            } else {
                "公网v4"
            }
        }
    }
}

/// 大收发缓冲（出口侧拦截栈每拍可产 ~1MB 突发——内核默认 SO_RCVBUF 会整包丢
/// WG 数据报 ⇒ TCP 层 RTO 重传；尽力而为抬高，超上限由内核钳制/忽略）。
fn enlarge_udp_bufs(sock: &UdpSocket) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 收集型 logf。
    fn log_sink() -> (crate::Logf, mpsc::Receiver<String>) {
        let (tx, rx) = mpsc::channel();
        (
            Arc::new(move |s: &str| {
                let _ = tx.send(s.to_owned());
            }),
            rx,
        )
    }

    fn reg_ctx() -> RegCtx {
        RegCtx {
            secret: Secret::from([7; 32]),
            pubkey: [9; 32],
            dev_tag: [0xAA; 8],
        }
    }

    /// 客户端双栈收发面（B0-1 回归锚点——r1-L9）：候选 [::1]（v6）时镜像包能到达
    /// v6 fake 出口、且出口回包被收进（recv unmap 不动纯 v6）；与 v4 候选并存时
    /// 双族都能到达（发送面 map 只作用于 v4 目标）。
    #[test]
    fn dual_stack_client_reaches_v6_exit() {
        let exit6 = UdpSocket::bind("[::1]:0").unwrap();
        let ep6 = exit6.local_addr().unwrap();
        let exit4 = UdpSocket::bind("127.0.0.1:0").unwrap();
        let ep4 = exit4.local_addr().unwrap();
        let cands = vec![
            Candidate { addr: ep6, relay: false },
            Candidate { addr: ep4, relay: false },
        ];
        let (logf, _rx) = log_sink();
        let mut b = bind(&cands, None, &logf);
        b.send_wg(b"v6-probe");
        // v6 fake 出口收到镜像包
        exit6.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut buf = [0u8; 64];
        let (n, from) = exit6.recv_from(&mut buf).unwrap();
        assert!(n >= 2, "腿帧至少有帧头");
        // 回包（出口→客户端）：源地址 = 客户端 dual socket 的 [::1] 形态
        let cli_ep = match from {
            SocketAddr::V6(v6) => SocketAddr::V6(v6),
            other => panic!("客户端源应为 v6 形态（dual socket）：{other}"),
        };
        let resp = crate::wtransport::frame::frame_bytes(crate::wtransport::frame::FrameKind::Data, b"resp6");
        exit6.send_to(&resp, cli_ep).unwrap();
        // 客户端收到（纯 v6 不经 unmap 变形）
        exit4.set_read_timeout(Some(Duration::from_millis(200))).ok();
        let mut got = Vec::new();
        for _ in 0..100 {
            let mut b2 = [0u8; 64];
            match b.recv_from(&mut b2) {
                Ok(Some(n)) => {
                    got.extend_from_slice(&b2[..n]);
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        assert!(!got.is_empty(), "出口回包应被客户端收到");
    }

    fn bind(cands: &[Candidate], reg: Option<RegCtx>, logf: &crate::Logf) -> Bind {
        let b = Bind::open(
            cands,
            reg,
            Some(Duration::from_secs(2)),
            &[1u8; 32],
            Arc::clone(logf),
        )
        .unwrap();
        b.sock.set_nonblocking(false).unwrap();
        b.sock
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        b
    }

    /// 内存 UDP 对：镜像 → 首包搭 reg 容器 → 回包采纳 → 单发（R1 基线回归）。
    #[test]
    fn mirror_then_adopt_then_single_send() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let (logf, logs) = log_sink();
        let mut b = bind(
            &[
                Candidate {
                    addr: exit_addr,
                    relay: false,
                },
                Candidate {
                    addr: "127.0.0.1:1".parse().unwrap(),
                    relay: false,
                },
            ],
            Some(reg_ctx()),
            &logf,
        );
        b.send_wg(b"handshake-init");
        let mut buf = [0u8; 2048];
        let (n, from) = exit.recv_from(&mut buf).unwrap();
        assert_eq!(from.port(), b.local_port());
        let (kind, payload) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(kind, FrameKind::Batch.to_wire());
        let msgs = frame::decode_batch(payload).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].0, FrameKind::Reg.to_wire());
        assert_eq!(msgs[1], (FrameKind::Data.to_wire(), &b"handshake-init"[..]));
        // 第二包不再搭 reg
        b.send_wg(b"second");
        let (n2, _) = exit.recv_from(&mut buf).unwrap();
        let (k2, p2) = frame::decode_frame(&buf[..n2]).unwrap();
        assert_eq!((k2, p2), (FrameKind::Data.to_wire(), &b"second"[..]));
        // 回包采纳 + C5/C6
        let mut resp = Vec::new();
        frame::encode_frame(FrameKind::Data, b"handshake-response", &mut resp);
        exit.send_to(&resp, format!("127.0.0.1:{}", b.local_port()))
            .unwrap();
        let mut rbuf = [0u8; 2048];
        assert_eq!(
            b.recv_from(&mut rbuf).unwrap(),
            Some(b"handshake-response".len())
        );
        assert_eq!(b.adopted(), Some(exit_addr));
        assert_eq!(b.status().via, Via::Direct);
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.contains("赛跑结算：胜出 直连 ") && l.contains("未响应=LAN 127.0.0.1:1")),
            "{all:?}"
        );
        assert!(
            all.iter().any(|l| l.starts_with("路径确立：直连 ")),
            "{all:?}"
        );
        // 已采纳：单发纯数据腿帧
        b.send_wg(b"after-adopt");
        let (n3, _) = exit.recv_from(&mut buf).unwrap();
        let (k3, p3) = frame::decode_frame(&buf[..n3]).unwrap();
        assert_eq!((k3, p3), (FrameKind::Data.to_wire(), &b"after-adopt"[..]));
        // refresh_reg：独立 reg 帧
        assert!(b.refresh_reg());
        let (n4, _) = exit.recv_from(&mut buf).unwrap();
        let (k4, p4) = frame::decode_frame(&buf[..n4]).unwrap();
        assert_eq!(k4, FrameKind::Reg.to_wire());
        assert_eq!(p4.len(), reg::REG_LEN);
    }

    /// 中继腿：信封字节（[0xAA][relayID]‖容器帧[reg][data]）+ via=relay + 告警行。
    #[test]
    fn relay_envelope_and_adoption() {
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let (logf, logs) = log_sink();
        let peer_pub = [3u8; 32];
        let mut b = Bind::open(
            &[Candidate {
                addr: relay_addr,
                relay: true,
            }],
            Some(reg_ctx()),
            None, // 显式关直连优先窗口：中继立即参与
            &peer_pub,
            Arc::clone(&logf),
        )
        .unwrap();
        b.sock.set_nonblocking(false).unwrap();
        b.sock
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();

        b.send_wg(b"init");
        let mut buf = [0u8; 2048];
        let (n, _) = relay.recv_from(&mut buf).unwrap();
        // [0xAA][relayID(8)]‖[腿帧]
        assert_eq!(buf[0], 0xAA);
        assert_eq!(&buf[1..9], &relay_id(&peer_pub));
        let (kind, payload) = frame::decode_frame(&buf[9..n]).unwrap();
        assert_eq!(kind, FrameKind::Batch.to_wire());
        let msgs = frame::decode_batch(payload).unwrap();
        assert_eq!(msgs[0].0, FrameKind::Reg.to_wire());
        assert_eq!(msgs[1], (FrameKind::Data.to_wire(), &b"init"[..]));

        // 中继回数据帧（裸腿帧——FIX-91 统一线格式）→ 采纳为 relay
        let mut resp = Vec::new();
        frame::encode_frame(FrameKind::Data, b"resp", &mut resp);
        relay
            .send_to(&resp, format!("127.0.0.1:{}", b.local_port()))
            .unwrap();
        let mut rbuf = [0u8; 64];
        assert_eq!(b.recv_from(&mut rbuf).unwrap(), Some(4));
        assert_eq!(b.status().via, Via::Relay);
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.starts_with("赛跑结算：胜出 中继 ")),
            "{all:?}"
        );
        assert!(
            all.iter()
                .any(|l| l.contains("⚠️ 链路走了中继（本应直连，属需排查的 bug）")),
            "{all:?}"
        );
        assert!(
            all.iter().any(|l| l.starts_with("路径确立：中继 ")),
            "{all:?}"
        );

        // 已采纳中继：出站带路由头
        b.send_wg(b"data2");
        let (n2, _) = relay.recv_from(&mut buf).unwrap();
        assert_eq!(buf[0], 0xAA);
        let (k, p) = frame::decode_frame(&buf[9..n2]).unwrap();
        assert_eq!((k, p), (FrameKind::Data.to_wire(), &b"data2"[..]));
        // RREG：带路由头的 reg 腿帧
        assert!(b.refresh_reg());
        let (n3, _) = relay.recv_from(&mut buf).unwrap();
        assert_eq!(buf[0], 0xAA);
        let (k3, _) = frame::decode_frame(&buf[9..n3]).unwrap();
        assert_eq!(k3, FrameKind::Reg.to_wire());
    }

    /// DirectFirst 解锁补发：窗口内只打直连 → 到点解锁 → 捕获对（pkt+reg）以容器帧
    /// 重投中继（评审高-1 验收形态：只有中继可达 + DirectFirst 开启）。
    #[test]
    fn direct_first_unlock_resends_with_reg() {
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay
            .set_read_timeout(Some(Duration::from_millis(60)))
            .unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let (logf, logs) = log_sink();
        let mut b = Bind::open(
            &[
                Candidate {
                    addr: "203.0.113.1:41641".parse().unwrap(),
                    relay: false,
                }, // 死直连
                Candidate {
                    addr: relay_addr,
                    relay: true,
                },
            ],
            Some(reg_ctx()),
            Some(Duration::from_millis(150)), // 短窗口加速测试
            &[1u8; 32],
            Arc::clone(&logf),
        )
        .unwrap();
        b.sock
            .set_read_timeout(Some(Duration::from_millis(100)))
            .unwrap();

        // 窗口内：只打直连（中继不收包）；捕获对就位
        b.send_wg(b"pkt-A");
        let mut buf = [0u8; 2048];
        assert!(
            matches!(
                relay.recv_from(&mut buf).err().map(|e| e.kind()),
                Some(io::ErrorKind::WouldBlock)
            ),
            "窗口内中继不得收到包"
        );
        assert!(b.unlock_captured.is_some(), "窗口锁定期的首包被捕获");
        // tick 未到点不解锁
        b.tick_unlock();
        assert!(!b.relay_unlocked);

        std::thread::sleep(Duration::from_millis(160));
        b.tick_unlock();
        assert!(b.relay_unlocked, "窗口到点解锁");
        // 中继收到补发：[0xAA][id]‖[容器帧[reg][pkt-A]]
        let (n, _) = relay.recv_from(&mut buf).unwrap();
        assert_eq!(buf[0], 0xAA);
        let (kind, payload) = frame::decode_frame(&buf[9..n]).unwrap();
        assert_eq!(kind, FrameKind::Batch.to_wire());
        let msgs = frame::decode_batch(payload).unwrap();
        assert_eq!(
            msgs[0].0,
            FrameKind::Reg.to_wire(),
            "reg 必须随补发重投（高-1）"
        );
        assert_eq!(
            msgs[1],
            (FrameKind::Data.to_wire(), &b"pkt-A"[..]),
            "补发的是捕获首包"
        );
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter()
                .any(|l| l
                    .starts_with("MIRROR 直连窗口 150ms 内无响应 → 解锁中继候选 1 个并补发一次")),
            "{all:?}"
        );
        // 解锁后常规镜像：中继参与（带路由头）
        b.send_wg(b"pkt-B");
        let (n2, _) = relay.recv_from(&mut buf).unwrap();
        assert_eq!(buf[0], 0xAA);
        assert!(n2 > 9);
    }

    /// handover 双发（FIX-09）：稳态切未知来源 → 旧路径 10s 内尽力双发。
    #[test]
    fn handover_dual_send_on_unknown_source() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let intruder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let intruder_addr = intruder.local_addr().unwrap();
        let (logf, _logs) = log_sink();
        let mut b = bind(
            &[Candidate {
                addr: exit_addr,
                relay: false,
            }],
            None,
            &logf,
        );

        // 初始采纳 exit
        let mut resp = Vec::new();
        frame::encode_frame(FrameKind::Data, b"hello", &mut resp);
        exit.send_to(&resp, format!("127.0.0.1:{}", b.local_port()))
            .unwrap();
        let mut rbuf = [0u8; 64];
        let _ = b.recv_from(&mut rbuf).unwrap();
        assert_eq!(b.adopted(), Some(exit_addr));

        // 未知来源包（伪造/错投）→ 采纳切换 + handover 登记
        intruder
            .send_to(&resp, format!("127.0.0.1:{}", b.local_port()))
            .unwrap();
        let _ = b.recv_from(&mut rbuf).unwrap();
        assert_eq!(b.adopted(), Some(intruder_addr));
        assert!(b.handover.is_some(), "未知来源切换登记双发宽限");

        // 出站双发：intruder（采纳）+ exit（宽限）
        b.send_wg(b"dual");
        let mut buf = [0u8; 128];
        let (n1, _) = intruder.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[2..n1], b"dual");
        let (n2, _) = exit.recv_from(&mut buf).unwrap();
        assert_eq!(&buf[2..n2], b"dual", "宽限期内旧路径收到双发");
    }

    /// 接收读错误：退避 + 限流行（纯逻辑面——错误注入经 note_recv_err 直驱）。
    #[test]
    fn recv_error_backoff_and_throttle() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let (logf, logs) = log_sink();
        let mut b = bind(
            &[Candidate {
                addr: exit_addr,
                relay: false,
            }],
            None,
            &logf,
        );
        b.note_recv_err(&io::Error::from_raw_os_error(9));
        assert!(b.recv_backoff_until.is_some(), "读错误置退避");
        assert!(b.recv_backoff_remain().is_some());
        // 退避期内 recv_from 直接 WouldBlock（不触 socket）
        let mut buf = [0u8; 16];
        assert_eq!(
            b.recv_from(&mut buf).err().map(|e| e.kind()),
            Some(io::ErrorKind::WouldBlock)
        );
        // 5s 限流：限流窗内连打两发只出一行
        b.recv_backoff_until = None;
        b.note_recv_err(&io::Error::from_raw_os_error(9));
        b.note_recv_err(&io::Error::from_raw_os_error(9));
        let all: Vec<String> = logs.try_iter().collect();
        assert_eq!(
            all.iter().filter(|l| l.starts_with("接收读错误（")).count(),
            1,
            "限流窗内只出一行：{all:?}"
        );
    }

    /// 候选集更新：relay 位参与比较；变化打一行。
    #[test]
    fn set_candidates_relay_bit_semantics() {
        let a1: SocketAddr = "127.0.0.1:1".parse().unwrap();
        let (logf, logs) = log_sink();
        let mut b = bind(
            &[Candidate {
                addr: a1,
                relay: false,
            }],
            None,
            &logf,
        );
        // 同集：不刷
        b.set_candidates(vec![Candidate {
            addr: a1,
            relay: false,
        }]);
        assert_eq!(logs.try_iter().count(), 0);
        // relay 位变化 = 集合变了
        b.set_candidates(vec![Candidate {
            addr: a1,
            relay: true,
        }]);
        let all: Vec<String> = logs.try_iter().collect();
        assert_eq!(all.len(), 1, "{all:?}");
        assert!(
            all[0].starts_with("候选集更新：1 条（127.0.0.1:1（中继））"),
            "{all:?}"
        );
        assert!(b.relay_eps.contains(&a1));
    }

    /// rearm 家族：RARM 行 + 硬/软窗口状态。
    #[test]
    fn rearm_family_lines_and_state() {
        let a: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let (logf, logs) = log_sink();
        let mut b = bind(
            &[Candidate {
                addr: a,
                relay: false,
            }],
            None,
            &logf,
        );
        b.rearm();
        b.rearm_soft();
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter()
                .any(|l| l.starts_with("RARM 候选赛跑重启（直连优先：中继在 ")),
            "{all:?}"
        );
        assert!(
            all.iter()
                .any(|l| l == "RARM 软赛跑（中继立即参与，同时试直连）"),
            "{all:?}"
        );
    }

    /// F11：`direct_first(None)` = 显式关（中继立即参与 + RARM 文案正确）；`Some(0)` = 缺省 2s。
    #[test]
    fn direct_first_none_means_off() {
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        let relay_addr = relay.local_addr().unwrap();
        let (logf, logs) = log_sink();
        let mut b = Bind::open(
            &[Candidate { addr: relay_addr, relay: true }],
            None,
            None, // 显式关
            &[1u8; 32],
            Arc::clone(&logf),
        )
        .unwrap();
        assert_eq!(b.direct_first, None, "None 保持 None（不再被映射成 Some(DEFAULT)）");
        assert!(b.relay_unlocked, "无直连候选 ⇒ 中继立即参与");
        b.rearm();
        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.contains("直连优先：关——中继立即参与")),
            "None 的 RARM 文案须与语义一致：{all:?}"
        );
        // Some(0) → 缺省 2s
        let b2 = Bind::open(
            &[Candidate { addr: relay_addr, relay: true }],
            None,
            Some(Duration::ZERO),
            &[1u8; 32],
            Arc::clone(&logf),
        )
        .unwrap();
        assert_eq!(b2.direct_first, Some(DIRECT_FIRST_DEFAULT), "Some(0) → 缺省 2s");
    }

    /// 发送统计计数面（复核 r3-F5）：镜像逐候选计数 + 全本地失败判定 + localErr 口径
    /// （镜像错误进 local_err_count；采纳路径错误只进 adopted 面——Go bind_test 同口径）。
    #[test]
    fn send_stats_mirror_all_local_fail() {
        let (logf, _rx) = log_sink();
        // 必然本地错误的候选：受限广播地址 + 无 SO_BROADCAST ⇒ sendto 同步被拒
        // （实测 darwin=EADDRNOTAVAIL(49)、linux=EACCES(13)，都是本地错误面）。
        // 0.0.0.0 **不可用**：linux 把目的 0.0.0.0 当本地回环送出、sendto 会成功
        // （转正 A 批 ubuntu CI 实测抓出；darwin 面原注释「一致」不实）；TEST-NET-1
        // 在 darwin 上 sendto 会先成功、ICMP 异步回，也不能当本地错。
        let cands = [Candidate {
            addr: "255.255.255.255:9".parse().unwrap(),
            relay: false,
        }];
        let mut b = bind(&cands, Some(reg_ctx()), &logf);
        let before = b.send_stats_pending();
        assert_eq!(before, (0, 0), "未发送前零计数");
        // 未采纳 ⇒ 镜像路径逐候选计数
        b.send_wg(&[0u8; 32]);
        let (tries, fails) = b.send_stats_pending();
        assert!(tries >= 1, "镜像逐候选计数：tries={tries}");
        assert_eq!(tries, fails, "不可达候选 ⇒ 全部本地失败");
        let (adopted, total) = b.local_err_counters();
        assert_eq!(adopted, 0, "未采纳 ⇒ 无采纳面错误");
        assert!(total >= 1, "镜像本地错误进 local_err_count");
    }

    /// F2：仅 Data 帧才采纳——垃圾/未知 kind/Control 帧不 adopt、不写 race_seen、
    /// 不打 C5/C6；合法 Data 帧仍采纳（漫游/自愈不受影响）。
    #[test]
    fn only_data_frames_adopt() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let (logf, logs) = log_sink();
        let mut b = bind(
            &[Candidate { addr: exit_addr, relay: false }],
            Some(reg_ctx()),
            &logf,
        );
        let me = format!("127.0.0.1:{}", b.local_port());
        let mut buf = [0u8; 64];
        // ① 1 字节垃圾 → 不采纳
        exit.send_to(b"x", &me).unwrap();
        let _ = b.recv_from(&mut buf);
        assert_eq!(b.adopted(), None, "垃圾包不得采纳");
        // ② 0xBB + 未知 kind → 不采纳
        exit.send_to(&[0xBB, 9, 7], &me).unwrap();
        let _ = b.recv_from(&mut buf);
        assert_eq!(b.adopted(), None, "未知 kind 不得采纳");
        // ③ Control(hint) → 不采纳（hint 不构成路径证据）
        exit.send_to(&frame::hint_bytes("203.0.113.5:41641"), &me).unwrap();
        let _ = b.recv_from(&mut buf);
        assert_eq!(b.adopted(), None, "Control 帧不得采纳");
        assert!(b.race_seen.is_empty(), "赛跑来源集不得被非 Data 帧写入");
        let all: Vec<String> = logs.try_iter().collect();
        assert!(!all.iter().any(|l| l.starts_with("赛跑结算：")), "非 Data 帧不得打 C5：{all:?}");
        assert!(!all.iter().any(|l| l.starts_with("路径确立：")), "非 Data 帧不得打 C6：{all:?}");
        // ④ 合法 Data 帧 → 仍采纳
        let mut resp = Vec::new();
        frame::encode_frame(FrameKind::Data, b"hello", &mut resp);
        exit.send_to(&resp, &me).unwrap();
        assert_eq!(b.recv_from(&mut buf).unwrap(), Some(5));
        assert_eq!(b.adopted(), Some(exit_addr));
    }

    /// F2：未知来源的 hint 过 `probe_addr_acceptable`（私网/CGNAT/fake-IP 注入被拒），
    /// 可接受地址仍投给 on_hint；已知来源（候选）豁免。
    #[test]
    fn unknown_source_hint_filtered() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let intruder = UdpSocket::bind("127.0.0.1:0").unwrap();
        let (logf, logs) = log_sink();
        let hits = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let h2 = Arc::clone(&hits);
        let mut b = bind(&[Candidate { addr: exit_addr, relay: false }], None, &logf);
        b.set_on_hint(Arc::new(move |a: &str| h2.lock().unwrap().push(a.to_owned())));
        let me = format!("127.0.0.1:{}", b.local_port());
        let mut buf = [0u8; 64];
        // 未知来源 + 不可接受地址（私网）→ 丢
        intruder.send_to(&frame::hint_bytes("10.0.0.5:41641"), &me).unwrap();
        let _ = b.recv_from(&mut buf);
        assert!(hits.lock().unwrap().is_empty(), "私网 hint 应被拒");
        assert!(
            logs.try_iter().any(|l| l.contains("HINT 忽略未知来源")),
            "应打忽略行"
        );
        // 未知来源 + 可接受地址（TEST-NET-3）→ 通过
        intruder.send_to(&frame::hint_bytes("203.0.113.9:41641"), &me).unwrap();
        let _ = b.recv_from(&mut buf);
        assert_eq!(hits.lock().unwrap().as_slice(), &["203.0.113.9:41641".to_owned()]);
    }

    /// F3：未采纳期按间隔补投 reg（首包丢/时钟偏差自愈），并受次数/时长上界约束。
    /// 节拍参数注入小值（生产 2s/60s）——不依赖墙钟 sleep，负载机上不假红（代码门 L5）。
    #[test]
    fn reg_resend_while_unadopted() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        exit.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let (logf, _logs) = log_sink();
        let mut b = bind(&[Candidate { addr: exit_addr, relay: false }], Some(reg_ctx()), &logf);
        b.reg_resend_interval = Duration::from_millis(40);
        let mut buf = [0u8; 2048];
        // 首包搭 reg
        b.send_wg(b"a");
        let (n, _) = exit.recv_from(&mut buf).unwrap();
        let (k, p) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(k, FrameKind::Batch.to_wire());
        assert_eq!(frame::decode_batch(p).unwrap()[0].0, FrameKind::Reg.to_wire());
        // 间隔未到 ⇒ 不补投
        b.send_wg(b"b");
        let (n, _) = exit.recv_from(&mut buf).unwrap();
        assert_eq!(frame::decode_frame(&buf[..n]).unwrap().0, FrameKind::Data.to_wire(), "间隔内不补投");
        // 过间隔 ⇒ 补投
        std::thread::sleep(Duration::from_millis(80));
        b.send_wg(b"c");
        let (n, _) = exit.recv_from(&mut buf).unwrap();
        let (k, p) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(k, FrameKind::Batch.to_wire(), "过间隔应补投 reg");
        assert_eq!(frame::decode_batch(p).unwrap()[0].0, FrameKind::Reg.to_wire());
        assert_eq!(b.reg_resend_n, 1);
        // 次数上界：达上限后不再补投
        b.reg_resend_n = REG_RESEND_MAX;
        std::thread::sleep(Duration::from_millis(80));
        b.send_wg(b"d");
        let (n, _) = exit.recv_from(&mut buf).unwrap();
        assert_eq!(frame::decode_frame(&buf[..n]).unwrap().0, FrameKind::Data.to_wire(), "达次数上界后不再补投");
        // 时长上界：次数复位但窗口已到 ⇒ 同样不补投（代码门 M3：窗口分支必须有测试）
        b.reg_resend_n = 0;
        b.reg_resend_window = Duration::ZERO;
        std::thread::sleep(Duration::from_millis(80));
        b.send_wg(b"e");
        let (n, _) = exit.recv_from(&mut buf).unwrap();
        assert_eq!(frame::decode_frame(&buf[..n]).unwrap().0, FrameKind::Data.to_wire(), "达时长上界后不再补投");
        // rearm 归零：恢复阶梯重开一轮预算（新未采纳期）
        b.rearm();
        b.send_wg(b"f");
        let (n, _) = exit.recv_from(&mut buf).unwrap();
        let (k, p) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(k, FrameKind::Batch.to_wire(), "rearm 后重新搭 reg");
        assert_eq!(frame::decode_batch(p).unwrap()[0].0, FrameKind::Reg.to_wire());
        assert_eq!(b.reg_resend_n, 0, "rearm 归零计数");
    }
}
