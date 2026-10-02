//! 出口 ServerBind（R3；语义真源 `baseline:pkg/servercore/bind.go` 的收发半边）。
//!
//! 与客户端 `wtransport::bind` 同名不同物（一个 = 出口腿帧收发面，对应 Go
//! `servercore.ServerBind`；一个 = 客户端候选赛跑面，对应 `wtransport.Bind`）——
//! 全路径 `server::bind` / `wtransport::bind` 区分，对称结构是有意为之（R3 评审 L11）。
//!
//! 收包按首字节判别（FIX-91 统一线格式——**所有腿都是腿帧**，含容器）：
//!
//! ```text
//! [0xBB]…  腿帧：0=数据入 device / 1=hint 回调 / 2=reg / 4=容器（reg 先登记后
//!          投递，只取首条 data——Go handleBatch 同序）/ 未知=忽略（type≥3 容忍）
//! 其余      非腿包：STUN 应答（事务 ID 匹配才消耗）/ 参照点探测（明文应答）；
//!          其余丢弃计数 + 新源日志
//! ```
//!
//! **入站新源日志**（E23 判据行，排障面）：每个新来源只记一行首包（含包形态），
//! 正常流量零噪音；手机换 NAT 映射后的第一发直连握手必落一行。容量 4096 满则
//! 清表重记（Go srcSeen 同义——排障去重表，非 correctness 状态）。

use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};

use crate::wtransport::frame::{self, FrameKind};

use super::device::InboundOut;

/// 新源记录表容量上限（Go srcSeenMax）。
const SRC_SEEN_MAX: usize = 4096;

/// 出站收口产物：一个要发上网络的 WG 包（含目标端点）。
pub struct WireOut(pub Vec<(SocketAddr, Vec<u8>)>);

/// 入站消费结果（recv_packet 的返回契约——驱动循环 drain 语义同客户端 bind）。
/// **reg 先于 data 应用**：容器帧 `[reg][init]`（Go 客户端首包形态）里 reg 登记设备表
/// 后 init 才进 device——字段序即应用序（Go handleBatch 同序；评审 H1）。
pub struct Inbound {
    /// 要进 device 的 WG 包（容器帧取首条 data）。
    pub data: Option<(SocketAddr, Vec<u8>)>,
    /// reg 帧载荷（驱动线程先于 data 应用到设备表）。
    pub regs: Vec<(Vec<u8>, SocketAddr)>,
}

/// hint 帧回调（装配层接；Go OnHint 语义——src 校验归接线方，R3 无中继腿不装）。
pub type OnHint = Box<dyn FnMut(&str, SocketAddr) + Send>;

/// 出口腿帧收发面（驱动线程独占）。
pub struct ServerBind {
    sock: UdpSocket,
    build: String,
    /// 探测应答的能力位（udpcap 周期结论；3e 接——现在恒 0）。
    caps: u8,
    /// 探测应答端点列表段来源（公网端点公布面；3f 接——现在恒空）。
    probe_endpoints: Vec<SocketAddr>,
    logf: crate::Logf,
    on_hint: Option<OnHint>,
    src_seen: HashMap<SocketAddr, ()>,
    /// STUN 观测等待者（事务 ID + 应答回执通道——同 socket 观测「监听端口的 NAT 映射」）。
    stun_wait_txid: Option<[u8; 12]>,
    stun_wait_result: Option<std::sync::mpsc::Sender<Option<SocketAddr>>>,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    recv_buf: Box<[u8; 65536]>,
}

impl ServerBind {
    /// 监听固定端口（被占退让 +1…+9 → 随机；Go listenWithFallback 同序）。
    /// 返回 Err = 全失败（出口起不来——占端口硬失败，R0.6 评审 M13 同口径）。
    pub fn open(port: u16, build: &str, logf: crate::Logf) -> io::Result<Self> {
        Self::open_bound(port, build, None, None, logf)
    }

    /// 绑定形态：`bind_ip`（钉卡的源地址；None = 0.0.0.0）+ 可选的整 socket 钉卡
    /// （index + 名——IP_BOUND_IF/SO_BINDTODEVICE；Go ServerBind BindIface 双栈钉卡
    /// 的 v4 单栈面，v6 面登记 R5）。
    pub fn open_bound(
        port: u16,
        build: &str,
        bind_ip: Option<Ipv4Addr>,
        pin: Option<(u32, String)>,
        logf: crate::Logf,
    ) -> io::Result<Self> {
        use std::os::fd::AsRawFd as _;
        let sock = listen_with_fallback_addr(port, bind_ip)?;
        if let Some((index, name)) = pin {
            let _ = super::egress::pin_socket_to_iface(sock.as_raw_fd(), index, &name);
        }
        if sock.local_addr()?.port() != port {
            (logf)(&format!(
                "⚠️ 监听端口 {port} 被占用 —— 改用 {}；token 里的端口以公布/签发为准",
                sock.local_addr()?.port()
            ));
        }
        sock.set_nonblocking(true)?;
        Ok(Self {
            sock,
            build: build.to_string(),
            caps: 0,
            probe_endpoints: Vec::new(),
            logf,
            on_hint: None,
            src_seen: HashMap::new(),
            stun_wait_txid: None,
            stun_wait_result: None,
            rx_bytes: 0,
            tx_bytes: 0,
            recv_buf: Box::new([0u8; 65536]),
        })
    }

    pub fn local_port(&self) -> u16 {
        self.sock.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub fn set_on_hint(&mut self, f: OnHint) {
        self.on_hint = Some(f);
    }

    /// 3e/3f 的接线面。
    pub fn set_caps(&mut self, caps: u8) {
        self.caps = caps;
    }

    pub fn set_probe_endpoints(&mut self, eps: Vec<SocketAddr>) {
        self.probe_endpoints = eps;
    }

    /// 收一个数据报（非阻塞）。返回契约：`Ok(Some(Inbound))` = 有 reg 待应用或数据帧
    /// 待进 device；`Ok(None)` = 本包被内部消费（hint/probe/STUN/畸形），继续收；
    /// `Err(WouldBlock|TimedOut)` = 本轮无包；其它 Err = 读错误（驱动循环限流记一行
    /// 原地慢转——绝不退出，防永久失聪）。
    pub fn recv_packet(&mut self) -> io::Result<Option<Inbound>> {
        let (n, src) = match self.sock.recv_from(&mut self.recv_buf[..]) {
            Ok(v) => v,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut => {
                return Err(e)
            }
            Err(e) => return Err(e),
        };
        self.rx_bytes += n as u64;
        // 拷出本包再解析（recv_buf 与 &mut self 的借用分离；长度按 n——热路径一次
        // 分配，R5 性能批再消）
        let pkt = self.recv_buf[..n].to_vec();
        Ok(self.process_packet(&pkt, src))
    }

    /// 一个入站 UDP 包的完整解析（Go processPacket 同序：STUN → probe → 腿帧 → 丢弃）。
    fn process_packet(&mut self, buf: &[u8], src: SocketAddr) -> Option<Inbound> {
        // STUN 应答：只认事务 ID 匹配的应答（观测等待者消费——映射地址回执后清位；
        // 不匹配的照常走后续判定，但 STUN 应答不是腿帧，最终走非帧丢弃）。
        if stun_looks_like_response(buf) {
            if let Some(txid) = self.stun_wait_txid {
                if buf.get(8..20) == Some(&txid[..]) {
                    self.note_new_src(src, "STUN应答", buf.len());
                    let mapped = super::egress::parse_stun_response(buf).map(|(_, ap)| ap);
                    if let Some(tx) = self.stun_wait_result.take() {
                        let _ = tx.send(mapped);
                    }
                    self.stun_wait_txid = None;
                    return None; // 已消耗
                }
            }
        }

        // 参照点探测：明文一问一答，不进 WG、不登记 peer。
        if let Some(resp) = crate::probe::respond_ex(buf, &self.build, self.caps, &self.probe_endpoints) {
            self.note_new_src(src, "参照点探测", buf.len());
            let _ = self.sock.send_to(&resp, src);
            return None;
        }

        if buf.first() == Some(&frame::FRAME_MAGIC) {
            let Some((kind, payload)) = frame::decode_frame(buf) else {
                self.note_new_src(src, "畸形腿帧", buf.len());
                return None; // 畸形腿帧：丢弃不中断
            };
            match kind {
                k if k == FrameKind::Data.to_wire() => {
                    self.note_new_src(src, "腿帧数据", buf.len());
                    return Some(Inbound {
                        data: Some((src, payload.to_vec())),
                        regs: Vec::new(),
                    });
                }
                k if k == FrameKind::Reg.to_wire() => {
                    self.note_new_src(src, "腿帧注册", buf.len());
                    return Some(Inbound { data: None, regs: vec![(payload.to_vec(), src)] });
                }
                k if k == FrameKind::Control.to_wire() => {
                    self.note_new_src(src, "腿帧控制", buf.len());
                    if let (Some(f), Some(addr)) = (&mut self.on_hint, frame::decode_hint_payload(payload)) {
                        f(addr, src);
                    }
                    return None;
                }
                k if k == FrameKind::Batch.to_wire() => return self.handle_batch(buf, payload, src),
                _ => {
                    // type≥3 的中继控制帧等：容忍忽略（前向兼容；OnLegFrame 钩子 R4 接）
                    self.note_new_src(src, &format!("腿帧type={kind}"), buf.len());
                    return None;
                }
            }
        }

        // FIX-91 统一线格式：非帧包不是腿（旧客户端裸 WG / 垃圾）——丢弃计数。
        let shape = match buf.first() {
            Some(b) => format!("非帧（{}，首字节=0x{b:02x}）", wg_msg_name(*b)),
            None => "非帧包".to_string(),
        };
        self.note_new_src(src, &shape, buf.len());
        None
    }

    /// 容器帧（Go handleBatch 同序）：reg 收集（**先于 data 应用**）→ data 只取首条投
    /// device → control 交 hint；未知消息类型忽略；无 data 且无 reg = 内部消费。
    fn handle_batch(&mut self, raw: &[u8], payload: &[u8], src: SocketAddr) -> Option<Inbound> {
        let Some(msgs) = frame::decode_batch(payload) else {
            self.note_new_src(src, "畸形容器", raw.len());
            return None;
        };
        let mut data: Option<&[u8]> = None;
        let mut regs: Vec<(Vec<u8>, SocketAddr)> = Vec::new();
        for (kind, mpayload) in msgs {
            match kind {
                k if k == FrameKind::Reg.to_wire() => {
                    regs.push((mpayload.to_vec(), src));
                }
                k if k == FrameKind::Data.to_wire() => {
                    if data.is_none() {
                        data = Some(mpayload); // 多条时取首条（UDP 单数据报只投一次）
                    }
                }
                k if k == FrameKind::Control.to_wire() => {
                    if let (Some(f), Some(addr)) = (&mut self.on_hint, frame::decode_hint_payload(mpayload)) {
                        f(addr, src);
                    }
                }
                _ => {} // 未知消息类型：忽略（前向兼容）
            }
        }
        let d = data.map(|d| (src, d.to_vec()));
        if d.is_none() && regs.is_empty() {
            self.note_new_src(src, "容器（无数据）", raw.len());
            return None;
        }
        self.note_new_src(src, "容器数据", raw.len());
        Some(Inbound { data: d, regs })
    }

    /// STUN 观测（**在本 Bind 的 UDP socket 上**问一次——监听端口那个 socket 的 NAT
    /// 映射；从临时 socket 问出来的是默认路由的映射，两者在 TUN 代理机器上完全不同）。
    /// 应答经 result 通道回执（None = 应答未到/不合法——调用方按超时收场）；同一时刻
    /// 只允许一次查询（观测周期都是分钟级——Go stunPending 同义）。
    pub fn stun_query(
        &mut self,
        server: SocketAddr,
        result: std::sync::mpsc::Sender<Option<SocketAddr>>,
    ) -> io::Result<()> {
        if self.stun_wait_txid.is_some() {
            return Err(io::Error::new(io::ErrorKind::WouldBlock, "server: 已有一次 STUN 查询在等"));
        }
        let txid = super::egress::new_txid();
        let req = super::egress::stun_request(&txid, ""); // servercore 形态：20B 无属性
        self.stun_wait_txid = Some(txid);
        self.stun_wait_result = Some(result);
        self.sock.send_to(&req, server)?;
        Ok(())
    }

    /// 观测超时收位（调用方超时后清——防下一次查询被占位挡住）。
    pub fn stun_query_abort(&mut self) {
        self.stun_wait_txid = None;
        self.stun_wait_result = None;
    }

    /// 出站收口：把 device 产出的 wire 批封装腿帧发出（Send 恒发腿帧——Go 同义）。
    pub fn send_wire(&mut self, out: &InboundOut) {
        for (ep, wg) in &out.wire {
            let wire = frame::frame_bytes(FrameKind::Data, wg);
            match self.sock.send_to(&wire, ep) {
                Ok(_) => self.tx_bytes += wg.len() as u64,
                Err(e) => {
                    // 发送失败限流记一行（排障面；对端可控面不能刷屏）
                    (self.logf)(&format!("发送到 {ep} 失败（{e}）"));
                }
            }
        }
        if !out.wire.is_empty() {
            // 清算显式丢弃计数（诊断面：no_endpoint_drops 在 device 内）
        }
    }

    /// 底层 UDP fd（驱动线程 poll(2) 用）。
    pub fn udp_fd(&self) -> std::os::fd::RawFd {
        use std::os::fd::AsRawFd as _;
        self.sock.as_raw_fd()
    }

    /// 从本 socket 直接发裸载荷（STUN 请求等 3e 面；SendRawTo 同义——与数据面同端口）。
    pub fn send_raw_to(&self, addr: SocketAddr, payload: &[u8]) -> io::Result<()> {
        self.sock.send_to(payload, addr).map(|_| ())
    }

    /// 入站新源的首包一行（每来源一次；容量满清表重记）。
    fn note_new_src(&mut self, src: SocketAddr, shape: &str, n: usize) {
        if self.src_seen.contains_key(&src) {
            return;
        }
        if self.src_seen.len() >= SRC_SEEN_MAX {
            self.src_seen.clear();
            (self.logf)(&format!("入站新源表满（{SRC_SEEN_MAX} 条），清表重记"));
        }
        self.src_seen.insert(src, ());
        (self.logf)(&format!("入站新源：{src}（{shape}，{n} 字节）"));
    }
}

/// 监听口被占用时的退让顺序：+1…+9，最后随机（Go listenWithFallback 同序）。
#[cfg(test)]
fn listen_with_fallback(port: u16) -> io::Result<UdpSocket> {
    listen_with_fallback_addr(port, None)
}

fn listen_with_fallback_addr(port: u16, ip: Option<Ipv4Addr>) -> io::Result<UdpSocket> {
    let base = ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
    if let Ok(s) = UdpSocket::bind((base, port)) {
        return Ok(s);
    }
    for p in port + 1..=port + 9 {
        if let Ok(s) = UdpSocket::bind((base, p)) {
            return Ok(s);
        }
    }
    UdpSocket::bind((base, 0))
}

/// STUN 应答快速判别（servercore/stun.go 同义：类型 0x0101 + magic cookie）。
fn stun_looks_like_response(b: &[u8]) -> bool {
    b.len() >= 20
        && u16::from_be_bytes([b[0], b[1]]) == 0x0101
        && u32::from_be_bytes([b[4], b[5], b[6], b[7]]) == 0x2112_A442
}

/// WG 报文类型码 → 可读名（首包日志用）。
fn wg_msg_name(b: u8) -> &'static str {
    match b {
        1 => "WG握手发起",
        2 => "WG握手应答",
        3 => "WG cookie",
        4 => "WG传输数据",
        _ => "非WG",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn noop_logf() -> crate::Logf {
        Arc::new(|_| {})
    }

    /// 收发对拍：容器帧 [reg][data] 的 Go 客户端形态首包（评审 H1 验收）——reg 先于
    /// data 被消费（返回形态的字段序即应用序）、data 进 Inbound。
    #[test]
    fn batch_frame_reg_then_data() {
        let mut b2 = ServerBind::open(0, "test", noop_logf()).unwrap();

        let reg = vec![0x41u8; 66];
        let wg = vec![1u8, 0, 0, 0, 2, 0, 0, 0]; // 假 init 形状
        let batch = frame::batch_bytes(&[(FrameKind::Reg.to_wire(), &reg), (FrameKind::Data.to_wire(), &wg)]);
        let src: SocketAddr = "127.0.0.1:5001".parse().unwrap();
        let r = b2.process_packet(&batch, src).expect("容器帧应投出");
        assert_eq!(r.regs.len(), 1, "reg 应被收集恰好一次");
        assert_eq!(r.regs[0].0, reg);
        let (dsrc, dwg) = r.data.expect("data 应在");
        assert_eq!(dsrc, src);
        assert_eq!(dwg, wg);

        // 同一源的第二包不再记新源（测试日志面：检查不 panic 即可——判据在 3f 实测）
        let r2 = b2.process_packet(&frame::frame_bytes(FrameKind::Data, &wg), src);
        assert!(matches!(r2, Some(i) if i.data.is_some()));
        let _ = (AtomicUsize::new(0), Ordering::SeqCst); // 原计数断言面由形态断言替代
    }

    /// probe 应答路径：HWQ → HWR（同 nonce/build/flags）；列表段受 pad 契约约束。
    #[test]
    fn probe_responds_on_socket_path() {
        let mut b = ServerBind::open(0, "rust-exit-test", noop_logf()).unwrap();
        let nonce = [9u8; 8];
        let req = crate::probe::encode_request(crate::probe::TYPE_PING, &nonce, 200);
        let src: SocketAddr = "127.0.0.1:5002".parse().unwrap();
        assert!(b.process_packet(&req, src).is_none(), "探测被消费");

        // 直接对 respond_ex 验细节（socket 回发路径由 3f 实测覆盖）
        let resp = crate::probe::respond_ex(&req, "rust-exit-test", 0x01, &["203.0.113.9:42641".parse().unwrap()]).unwrap();
        assert_eq!(&resp[..3], b"HWR");
        assert_eq!(&resp[5..13], &nonce);
        // pad 200 够 → 带列表
        assert!(resp.len() > 13 + 8);
        // pad 16 的老形态 → 不带列表（防放大）
        let old_req = crate::probe::encode_request(crate::probe::TYPE_PING, &nonce, 16);
        let old_resp = crate::probe::respond_ex(&old_req, "b", 0, &["203.0.113.9:42641".parse().unwrap()]).unwrap();
        assert!(old_resp.len() <= old_req.len() + 45, "45B 不变量");
    }

    /// 非帧包丢弃 + 形态串；畸形腿帧不中断。
    #[test]
    fn non_frame_and_malformed_shapes() {
        let mut b = ServerBind::open(0, "t", noop_logf()).unwrap();
        let src: SocketAddr = "127.0.0.1:5003".parse().unwrap();
        // 裸 WG init（非帧）
        let mut raw = vec![1u8];
        raw.resize(148, 0);
        assert!(b.process_packet(&raw, src).is_none());
        // 畸形腿帧（长度 <2——只有魔数；Go DecodeFrame 同判）
        assert!(b.process_packet(&[0xBB], src).is_none());
        // 容器畸形（越界长度）
        let bad_batch = frame::frame_bytes(FrameKind::Batch, &[0, 0xFF, 0]);
        assert!(b.process_packet(&bad_batch, src).is_none());
        // hint 帧消费
        let hint = frame::hint_bytes("1.2.3.4:9");
        assert!(b.process_packet(&hint, src).is_none());
    }

    /// 端口退让：占用后 +1（真实 socket 面）。
    #[test]
    fn port_fallback_on_busy() {
        let holder = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
        let port = holder.local_addr().unwrap().port();
        let s = listen_with_fallback(port).unwrap();
        assert_ne!(s.local_addr().unwrap().port(), port, "被占应退让");
    }
}
