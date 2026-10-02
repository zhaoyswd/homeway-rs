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

use std::collections::HashSet;
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
    last_path_log_at: Option<Instant>,
    rx_bytes: u64,
    tx_bytes: u64,
    send_errs: u64,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    /// 发送 scratch（容器帧/腿帧拼装，热路径零分配——吞吐设计位）。
    scratch: Vec<u8>,
    /// 收包缓冲（UDP 数据报上界 64KB，构造时一次分配）。
    recv_buf: Box<[u8; 65536]>,
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
            last_path_log_at: None,
            rx_bytes: 0,
            tx_bytes: 0,
            send_errs: 0,
            logf,
            scratch: Vec::with_capacity(2048),
            recv_buf: Box::new([0u8; 65536]),
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
        if let Some(addr) = self.adopted {
            self.scratch.clear();
            frame::encode_frame(FrameKind::Data, wg, &mut self.scratch);
            if let Err(e) = self.sock.send_to(&self.scratch, addr) {
                self.send_errs += 1;
                if self.send_errs < 10 {
                    (self.logf)(&format!("wtransport: 发送失败 {addr}: {e}"));
                }
            }
            self.tx_bytes += wg.len() as u64;
            return;
        }
        // 未采纳：镜像到全部直连候选；首个数据报搭 [reg][data] 容器（每份各带）。
        self.mirrored += 1;
        let reg_pkt = self.take_reg();
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
        let mut sent = 0;
        for c in &self.candidates {
            if self.sock.send_to(&self.scratch, *c).is_ok() {
                sent += 1;
            }
        }
        // 一条逻辑出站包按包记一次（Go 同口径）。
        self.tx_bytes += wg.len() as u64;
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

    /// 取走（消费）本轮赛跑的搭车 reg——一次性（arm 收紧语义见模块注释）。
    fn take_reg(&mut self) -> Option<Vec<u8>> {
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
        self.reg_armed = false;
        Some(pkt)
    }

    /// 收一个数据报：采纳（先于帧解码，Go 同序）→ 腿帧解码。
    /// `Ok(Some(n))` = 数据帧载荷已拷入 buf（喂 WG decapsulate）；`Ok(None)` = 本包被
    /// 内部消费（非数据帧/非帧包，丢弃继续）；`Err(WouldBlock|TimedOut)` = 本轮无包
    /// （驱动循环据此结束 drain）；其它 `Err` = 读错误上抛（驱动循环计数限流处理，
    /// 不再在 Bind 内吞——避免驱动层「错误→继续→错误」的紧循环不可见）。
    pub fn recv_from(&mut self, buf: &mut [u8]) -> io::Result<Option<usize>> {
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
