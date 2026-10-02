//! wtransport 直连 Bind（R1 子集）——自管 UDP socket + 候选镜像 + 首回包采纳 +
//! reg 搭车（唯一 `send_wg` 收口）。
//!
//! 语义真源 `baseline:clientcore/internal/wtransport/bind.go`（本期裁剪面见模块注释）：
//! - **未采纳时**出站 WG 包镜像到全部直连候选，首包搭容器帧 `[reg][data]` 保 1 RTT
//!   （每个镜像数据报各带一份 reg——任一路径都能完成注册）；
//! - **收到任何来源的包即采纳**该来源（采纳先于帧解码，Go 同序），之后单发；
//! - 判据行：C4 MIRROR（每轮 ≤3 行且间隔 ≥1s 的双条件节流）、C5 赛跑结算（本轮首次
//!   采纳时一次）、C6 路径确立（地址变化 + 3s 节流）；
//! - **R1 明确不做**：漫游/换网（Rebind）、中继腿（relay 候选构造期过滤）、
//!   直连优先窗口（无中继候选即无门控）、恢复阶梯（R2 期）。
//!
//! 与 Go 的**已登记差异**：
//! 1. reg 搭车收口 = 「真正写出 UDP 数据报的那一次」（Go 在 `Send` 入口消费 arm，
//!    走到已采纳分支会把 reg 丢掉——其注释自认；Rust 收紧为「首个写出的数据报才
//!    消费」，评审 ④-2 登记为有意收紧，单轮场景两者等价）；
//! 2. UDP socket 绑 v4（Go 绑双栈以支持 v6 承载端点）——R1 判据出口为 v4 回环/LAN；
//!    v6 承载与双栈属 R2 换网面。

use std::collections::{HashMap, HashSet};
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::token::Secret;

use super::frame::{self, FrameKind};
use crate::go_fmt::fmt_duration_go_ms;
use super::reg;

/// 一条候选路径。
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

pub struct Bind {
    sock: UdpSocket,
    candidates: Vec<SocketAddr>,
    reg: Option<RegCtx>,
    reg_armed: bool,
    adopted: Option<SocketAddr>,
    mirrored: u64,
    race_start: Instant,
    race_seen: HashSet<SocketAddr>,
    mirror_log_n: u32,
    mirror_log_at: Option<Instant>,
    /// 发送失败限流（每目标 5s 一条；Go lastSendErrAt——本地错误是粘性的，不限流会每包刷屏）。
    send_err_log_at: HashMap<SocketAddr, Instant>,
    last_path_log_at: Option<Instant>,
    rx_bytes: u64,
    tx_bytes: u64,
    send_errs: u64,
    /// 最近一次**采纳路径**本地类发送错误时刻（Go lastLocalSendErrAt——巡检证据
    /// 分类的数据源；镜像候选的错误不刷此位）。
    last_local_send_err: Option<Instant>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    /// 发送 scratch（容器帧/腿帧拼装，热路径零分配——吞吐设计位）。
    scratch: Vec<u8>,
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
    /// `candidates` 的 relay 条目被过滤（R1 不做中继腿，见模块注释）。
    pub fn open(
        candidates: &[Candidate],
        reg: Option<RegCtx>,
        logf: Arc<dyn Fn(&str) + Send + Sync>,
    ) -> io::Result<Self> {
        let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
        sock.set_nonblocking(true)?;
        let b = Self {
            sock,
            candidates: candidates.iter().filter(|c| !c.relay).map(|c| c.addr).collect(),
            reg,
            reg_armed: true,
            adopted: None,
            mirrored: 0,
            race_start: Instant::now(),
            race_seen: HashSet::new(),
            mirror_log_n: 0,
            mirror_log_at: None,
            send_err_log_at: HashMap::new(),
            last_path_log_at: None,
            rx_bytes: 0,
            tx_bytes: 0,
            send_errs: 0,
            last_local_send_err: None,
            logf,
            scratch: Vec::with_capacity(2048),
            recv_buf: Box::new([0u8; 65536]),
            #[cfg(feature = "test-seams")]
            poisoned: false,
        };
        if b.candidates.is_empty() {
            (b.logf)("wtransport: 无直连候选（R1 不做中继腿——token 只有 relay 端点时无法建连）");
        }
        Ok(b)
    }

    pub fn local_port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    /// 唯一出站收口：所有要写上网络的 WG 包都经这里（封装腿帧 + reg 搭车 + 镜像/采纳
    /// 分派）。**reg 只在首个真正写出的数据报上搭车**（不论这个包由哪个 TunnResult
    /// 分支产出——encapsulate/update_timers/decapsulate/send_queued 四来源全覆盖）。
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
            self.scratch.clear();
            frame::encode_frame(FrameKind::Data, wg, &mut self.scratch);
            if let Err(e) = self.sock.send_to(&self.scratch, addr) {
                self.send_errs += 1;
                self.last_local_send_err = Some(Instant::now());
                self.log_send_err_throttled(addr, &e);
            } else {
                self.tx_bytes += wg.len() as u64; // 成功才计（Go bind.go:644-651 同口径）
            }
            return;
        }
        // 未采纳：镜像到全部直连候选；搭车 reg 在**首个真正写出的数据报**上（每份各带
        // ——评审中-1：零候选/全失败不得消费 arm，否则丢了首包就永久失注册）。
        self.mirrored += 1;
        let reg_pkt = self.peek_reg();
        match &reg_pkt {
            Some(r) => {
                self.scratch.clear();
                frame::encode_batch(
                    &[(FrameKind::Reg.to_wire(), r), (FrameKind::Data.to_wire(), wg)],
                    &mut self.scratch,
                );
            }
            None => {
                self.scratch.clear();
                frame::encode_frame(FrameKind::Data, wg, &mut self.scratch);
            }
        }
        let mut sent = 0; // 尝试数（Go writeCandidate 后无条件 sent++——本地不可达候选
                          // 也计入，C4 判据行与 Go 同数字；评审中-2）
        let mut sent_ok = 0;
        let mut send_errs: Vec<(SocketAddr, std::io::Error)> = Vec::new();
        for c in &self.candidates {
            match self.sock.send_to(&self.scratch, *c) {
                Ok(_) => sent_ok += 1,
                Err(e) => {
                    self.send_errs += 1;
                    send_errs.push((*c, e));
                }
            }
            sent += 1;
        }
        for (addr, e) in &send_errs {
            self.log_send_err_throttled(*addr, e);
        }
        if sent_ok > 0 {
            if reg_pkt.is_some() {
                self.reg_armed = false; // 真正写出才消费（评审中-1）
            }
            self.tx_bytes += wg.len() as u64; // 一条逻辑出站包按包记一次（成功；Go 同口径）
        }
        // C4 判据行（双条件节流：≤3 行/轮 且 间隔 ≥1s）
        let now = Instant::now();
        let loggable =
            self.mirror_log_n < MIRROR_LOG_MAX && self.mirror_log_at.is_none_or(|t| now.duration_since(t) >= MIRROR_LOG_GAP);
        if loggable {
            self.mirror_log_n += 1;
            self.mirror_log_at = Some(now);
            (self.logf)(&format!(
                "MIRROR 镜像包#{} → {} 候选（直连优先：本次直连 {} / 中继 0；本行每轮限 3 条）",
                self.mirrored, sent, sent
            ));
        }
    }

    /// 预取本轮搭车 reg（不消费 arm——消费点在「真正写出」之后，见 send_wg）。
    fn peek_reg(&mut self) -> Option<Vec<u8>> {
        if !self.reg_armed {
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

    /// 收一个数据报：采纳（先于帧解码，Go 同序）→ 腿帧解码。
    /// `Ok(Some(n))` = 数据帧载荷已拷入 buf（喂 WG decapsulate）；`Ok(None)` = 本包被
    /// 内部消费（非数据帧/非帧包，丢弃继续）；`Err(WouldBlock|TimedOut)` = 本轮无包
    /// （驱动循环据此结束 drain）；其它 `Err` = 读错误上抛（驱动循环计数限流处理，
    /// 不再在 Bind 内吞——避免驱动层「错误→继续→错误」的紧循环不可见）。
    pub fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
        #[cfg(feature = "test-seams")]
        if self.poisoned {
            return Err(io::Error::from_raw_os_error(libc::EBADF));
        }
        let (n, src) = self.sock.recv_from(&mut self.recv_buf[..])?;
        self.rx_bytes += n as u64;
        self.adopt(src);
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
                // hint 线索（R1 学习缓存内存版：会话层接线 Observe，不触发盲打）
                if let Some(addr) = frame::decode_hint_payload(payload) {
                    (self.logf)(&format!("HINT 收到对端地址线索 {addr}（来自 {src}）"));
                }
                Ok(None)
            }
            _ => Ok(None), // reg（服务端概念）/容器（服务端搭车用）/未知：忽略
        }
    }

    /// 采纳来源 + 判据行（C5 赛跑结算一次 / C6 路径确立 3s 节流）。
    fn adopt(&mut self, src: SocketAddr) {
        let was_valid = self.adopted.is_some();
        let prev = self.adopted;
        self.adopted = Some(src);
        self.race_seen.insert(src);
        let now = Instant::now();
        if !was_valid {
            // 赛跑结算：胜者 / 响应过的候选 / 全程未响应的候选（本轮仅一次）
            let mut ok_list = Vec::new();
            let mut miss_list = Vec::new();
            for c in &self.candidates {
                if self.race_seen.contains(c) {
                    ok_list.push(c.to_string());
                } else {
                    miss_list.push(format!("{} {c}", candidate_tag(*c)));
                }
            }
            let elapsed = now.duration_since(self.race_start);
            (self.logf)(&format!(
                "赛跑结算：胜出 直连 {src}（镜像 {} 包，耗时 {}）；响应过={}；未响应={}",
                self.mirrored,
                fmt_duration_go_ms(elapsed),
                ok_list.join("、"),
                miss_list.join("、")
            ));
        }
        let path_changed = prev.is_none_or(|p| p != src);
        if path_changed && self.last_path_log_at.is_none_or(|t| now.duration_since(t) >= PATH_LOG_GAP) {
            self.last_path_log_at = Some(now);
            if !was_valid {
                (self.logf)(&format!("路径确立：直连 {src}（首个回包来源）"));
            } else if let Some(p) = prev {
                (self.logf)(&format!("路径切换：直连 {p} → 直连 {src}"));
            }
        }
    }

    /// 独立补发一条注册报文（C15：出口重启/设备记录被回收后保活；不依赖 WG 会话、
    /// 不改采纳状态）。未采纳时返回 false（首次建连的注册由 send_wg 搭车负责）。
    pub fn refresh_reg(&mut self) -> bool {
        let Some(addr) = self.adopted else { return false };
        let Some(ctx) = self.reg.as_ref() else { return false };
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut pkt = Vec::with_capacity(reg::REG_LEN);
        reg::encode_reg_parts(&ctx.secret, &ctx.pubkey, &ctx.dev_tag, now, &mut pkt);
        self.scratch.clear();
        frame::encode_frame(FrameKind::Reg, &pkt, &mut self.scratch);
        if let Err(e) = self.sock.send_to(&self.scratch, addr) {
            (self.logf)(&format!("RREG 注册刷新发送失败（{e}）"));
            return false;
        }
        (self.logf)(&format!(
            "RREG 注册刷新 → {addr}（dev={}，中继=false）",
            crate::identity::DevTag(ctx.dev_tag).short()
        ));
        true
    }

    pub fn status(&self) -> Status {
        Status {
            via: if self.adopted.is_some() { Via::Direct } else { Via::None },
            ep: self.adopted,
            mirrored: self.mirrored,
        }
    }

    pub fn adopted(&self) -> Option<SocketAddr> {
        self.adopted
    }

    pub fn rx_tx(&self) -> (u64, u64) {
        (self.rx_bytes, self.tx_bytes)
    }

    /// 最近 d 内是否发生过**采纳路径**本地类发送错误（Go LocalSendErrWithin——
    /// 巡检失败拍按环境噪声处理的判定数据源）。
    pub fn local_send_err_within(&self, d: Duration) -> bool {
        self.last_local_send_err.is_some_and(|t| t.elapsed() < d)
    }

    pub fn last_local_send_err_at(&self) -> Option<Instant> {
        self.last_local_send_err
    }

    /// 发送失败一行（每目标 5s 条；Go logSendErr 同串同节流）。
    fn log_send_err_throttled(&mut self, addr: SocketAddr, e: &io::Error) {
        let now = Instant::now();
        let due = self
            .send_err_log_at
            .get(&addr)
            .is_none_or(|t| now.duration_since(*t) >= Duration::from_secs(5));
        if due {
            self.send_err_log_at.insert(addr, now);
            (self.logf)(&format!(
                "发送失败：{e}（{addr}；本地错误=该候选在本机就发不出去，与对端无响应是两回事）"
            ));
        }
    }

    pub fn socket(&self) -> &UdpSocket {
        &self.sock
    }

    /// 重启一轮赛跑（R1 仅供整会话重建时复用；Go 的 Rearm 家族含直连优先窗口，本期
    /// 无中继候选即无门控面）。
    pub fn rearm(&mut self) {
        self.adopted = None;
        self.reg_armed = true;
        self.race_start = Instant::now();
        self.race_seen.clear();
        self.mirror_log_n = 0;
        self.mirror_log_at = None;
    }

    /// 换本地 socket（Go Rebind 同义：不换 Identity、不动采纳——漫游 = 同钥换源地址，
    /// 会话保持）。新 socket 非阻塞；旧 socket 关闭（驱动线程 poll 的 fd 由每轮重取跟随）。
    pub fn rebind(&mut self) -> io::Result<u16> {
        let sock = UdpSocket::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0))?;
        sock.set_nonblocking(true)?;
        let old = std::mem::replace(&mut self.sock, sock);
        let port = self.sock.local_addr().map(|a| a.port()).unwrap_or(0);
        // 旧 socket 关闭：R1 无接收 goroutine 常驻（驱动线程按 fd 重取），无 ErrClosed
        // 收口语义需要——直接 drop 即关。
        drop(old);
        #[cfg(feature = "test-seams")]
        {
            self.poisoned = false;
        }
        (self.logf)(&format!("REBIND 本地端口 → {port}（Identity 不变）"));
        Ok(port)
    }

    /// 测试缝：模拟 socket 失效（见字段注释；`rebind` 解除）。
    #[cfg(feature = "test-seams")]
    pub fn poison_socket_for_test(&mut self) {
        self.poisoned = true;
        (self.logf)("[test-seam] UDP socket 已置为失效形态（EBADF 模拟）");
    }

    /// 更新候选集（Go SetCandidates 同义；R1 形态：集合变化打一行）。
    pub fn set_candidates(&mut self, cands: Vec<SocketAddr>) {
        let mut changed = self.candidates.len() != cands.len();
        if !changed {
            for (a, b) in self.candidates.iter().zip(cands.iter()) {
                if a != b {
                    changed = true;
                    break;
                }
            }
        }
        self.candidates = cands;
        if changed {
            let list: Vec<String> = self.candidates.iter().map(|c| format!("{c}（LAN）")).collect();
            (self.logf)(&format!("候选集更新：{} 条（{}）", self.candidates.len(), list.join("、")));
        }
    }
}

/// 候选一行标签（C5 未响应列表的 tag：中继/IPv6/LAN/公网v4；Go candidateTag 同义）。
fn candidate_tag(ap: SocketAddr) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// 收集型 logf（断言判据行文案）。
    type LogSink = (Arc<dyn Fn(&str) + Send + Sync>, mpsc::Receiver<String>);
    fn log_sink() -> LogSink {
        let (tx, rx) = mpsc::channel();
        let f: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |s: &str| {
            let _ = tx.send(s.to_owned());
        });
        (f, rx)
    }

    fn reg_ctx() -> RegCtx {
        RegCtx {
            secret: Secret::from([7; 32]),
            pubkey: [9; 32],
            dev_tag: [0xAA; 8],
        }
    }

    /// 内存 UDP 对：对端 socket 扮演出口。镜像 → 首包搭 reg 容器 → 回包采纳 → 单发。
    #[test]
    fn mirror_then_adopt_then_single_send() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let (logf, logs) = log_sink();

        let mut bind = Bind::open(
            &[Candidate { addr: exit_addr, relay: false }, Candidate { addr: "127.0.0.1:1".parse().unwrap(), relay: false }],
            Some(reg_ctx()),
            logf,
        )
        .unwrap();
        bind.sock.set_nonblocking(false).unwrap();
        bind.sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();

        // 未采纳：首包 = 容器帧 [reg][data]，两个候选各一份（127.0.0.1:1 发不出去也计数镜像）
        bind.send_wg(b"handshake-init");
        let mut buf = [0u8; 2048];
        let (n, from) = exit.recv_from(&mut buf).unwrap();
        assert_eq!(from.port(), bind.local_port());
        let (kind, payload) = frame::decode_frame(&buf[..n]).unwrap();
        assert_eq!(kind, FrameKind::Batch.to_wire());
        let msgs = frame::decode_batch(payload).unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].0, FrameKind::Reg.to_wire());
        assert_eq!(msgs[0].1.len(), reg::REG_LEN);
        assert_eq!(msgs[1], (FrameKind::Data.to_wire(), &b"handshake-init"[..]));
        // 第二包不再搭 reg（arm 已消费——收紧语义）
        bind.send_wg(b"second");
        let (n2, _) = exit.recv_from(&mut buf).unwrap();
        let (k2, p2) = frame::decode_frame(&buf[..n2]).unwrap();
        assert_eq!((k2, p2), (FrameKind::Data.to_wire(), &b"second"[..]));

        // 出口回一帧数据 → 客户端采纳 + C5/C6 判据行
        let mut resp = Vec::new();
        frame::encode_frame(FrameKind::Data, b"handshake-response", &mut resp);
        exit.send_to(&resp, format!("127.0.0.1:{}", bind.local_port())).unwrap();
        let mut rbuf = [0u8; 2048];
        let got = bind.recv_from(&mut rbuf).unwrap();
        assert_eq!(got, Some(b"handshake-response".len()));
        assert_eq!(&rbuf[..18], b"handshake-response");
        assert_eq!(bind.adopted(), Some(exit_addr));

        let all: Vec<String> = logs.try_iter().collect();
        assert!(
            all.iter().any(|l| l.contains("赛跑结算：胜出 直连 ") && l.contains("（镜像 2 包，耗时 ")),
            "C5 判据行缺失：{all:?}"
        );
        assert!(all.iter().any(|l| l.starts_with("路径确立：直连 ")), "C6 判据行缺失：{all:?}");
        // 未响应列表带 tag（LAN + 空格 + 地址）；响应列表裸地址
        let settle = all.iter().find(|l| l.contains("赛跑结算")).unwrap();
        assert!(settle.contains(&format!("响应过={exit_addr}")), "{settle}");
        assert!(settle.contains("未响应=LAN 127.0.0.1:1"), "{settle}");

        // 已采纳：单发采纳端点（纯数据腿帧）
        bind.send_wg(b"after-adopt");
        let (n3, from3) = exit.recv_from(&mut buf).unwrap();
        let (k3, p3) = frame::decode_frame(&buf[..n3]).unwrap();
        assert_eq!((k3, p3), (FrameKind::Data.to_wire(), &b"after-adopt"[..]));
        assert_eq!(from3.port(), bind.local_port());

        // refresh_reg：独立 reg 帧（不搭数据）
        assert!(bind.refresh_reg());
        let (n4, _) = exit.recv_from(&mut buf).unwrap();
        let (k4, p4) = frame::decode_frame(&buf[..n4]).unwrap();
        assert_eq!(k4, FrameKind::Reg.to_wire());
        assert_eq!(p4.len(), reg::REG_LEN);
        assert!(logs.try_iter().any(|l| l.starts_with("RREG 注册刷新 → ")));
    }

    /// 非帧包/控制帧：采纳但零数据面投递。
    #[test]
    fn non_data_frames_are_consumed_silently() {
        let exit = UdpSocket::bind("127.0.0.1:0").unwrap();
        let exit_addr = exit.local_addr().unwrap();
        let (logf, _logs) = log_sink();
        let mut bind = Bind::open(&[Candidate { addr: exit_addr, relay: false }], None, logf).unwrap();
        bind.sock.set_nonblocking(false).unwrap();
        bind.sock.set_read_timeout(Some(Duration::from_millis(500))).unwrap();

        // hint 控制帧
        exit.send_to(&frame::hint_bytes("1.2.3.4:999"), format!("127.0.0.1:{}", bind.local_port())).unwrap();
        let mut buf = [0u8; 512];
        assert_eq!(bind.recv_from(&mut buf).unwrap(), None);
        assert_eq!(bind.adopted(), Some(exit_addr));

        // 裸非帧包（垃圾）：同样采纳（Go 同序），不投数据
        exit.send_to(b"garbage", format!("127.0.0.1:{}", bind.local_port())).unwrap();
        assert_eq!(bind.recv_from(&mut buf).unwrap(), None);
    }

    /// relay 候选构造期过滤（R1 不做中继腿）。
    #[test]
    fn relay_candidates_filtered() {
        let (logf, _logs) = log_sink();
        let bind = Bind::open(
            &[
                Candidate { addr: "127.0.0.1:1".parse().unwrap(), relay: false },
                Candidate { addr: "127.0.0.1:2".parse().unwrap(), relay: true },
            ],
            None,
            logf,
        )
        .unwrap();
        assert_eq!(bind.candidates.len(), 1);
    }
}
