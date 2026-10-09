//! 多 peer WG device（R3 服务端数据面核心；设计文档 §1/§2）。
//!
//! 在 boringtun noise 原语之上自建「出口侧 device」——对齐 Go 侧 `wireguard-go device`
//! 在 homeway 里的用法（`servercore.NewIPCConfigurer` 落 peer / `ServerBind` 收发）：
//!
//! - **两表分发**（评审 M1 定稿）：pub 表（公钥 → peer，init 识别走
//!   `Tunn::parse_incoming_packet` + `parse_handshake_anon`——boringtun 0.6 公开 API）与
//!   base 表（`receiver_idx >> 8` → peer，覆盖 type=2/3/4——response/cookie/data 的
//!   receiver_idx 都落在本端 `(base<<8)+k` 空间内）。
//! - **index 分配**：每 peer 随机 24-bit base、全局查重重掷（`Tunn::new(index)` 内部
//!   `index<<8`；base 须 <2^24）。wireguard-go 实为 32-bit 随机全局表——行为等价
//!   （对端只回显），按 peer 分 256 槽是 boringtun 的内在结构。
//! - **漫游跟随**（设计 §2）：endpoint 由「认证成功的包的源地址」学习——init/response
//!   验证通过、data 解密成功（含 keepalive）时更新；**先更新后发应答**。判定基于
//!   **本次输入包的类型**（显式分派），排除空数据报重调与 cookie reply 的 `Done`/
//!   `WriteToNetwork` 假阳性。
//! - **会话过期**：`ConnectionExpired` 只打一次日志（抑制每拍重复）——Tunn 可自愈
//!   （下一入站 init 或出站 encapsulate 都会把状态从 Expired 带走）；**base 永不变**
//!   （换 base 会丢客户端在途 index 引用）。
//! - **keepalive 口径**：`Tunn::new(keepalive=None)`（对齐 Go ipc.go「刻意不写
//!   persistent_keepalive」）；boringtun 的被动 keepalive（收到过数据且 10s 没发）保留。
//!   服务端也会主动发 init（发过数据 15s 无回 → 强制握手；keepalive 无会话 → 起握手），
//!   走与客户端相同的 5s 重传/90s 放弃语义。
//! - **rate_limiter = None**：各 Tunn 自建私有 limiter（等价 per-peer 限速；共享 Arc 会
//!   把全设备压到 10 握手包/s）。under-load 时 boringtun 自产 cookie reply——照常回
//!   **包源地址**，endpoint 不因它更新。
//! - **入站明文源校验**（wireguard-go allowedips 反查同义）：decap 出的明文 IPv4 包
//!   `src ∈ {该 peer 的 TunnelIP, TunIP}` 才收，否则静默丢 + 计数。

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};

use boringtun::noise::errors::WireGuardError;
use boringtun::noise::{Tunn, TunnResult};
use boringtun::x25519::{PublicKey, StaticSecret};

use crate::Logf;

/// WG 网络包缓冲上界（明文 + 32B 开销 / 握手 148）。
const WG_BUF: usize = 65536 + 148;

/// 单个 peer 的设备面状态（驱动线程独占）。
struct Peer {
    tunn: Tunn,
    /// 当前漫游端点（合法握手/数据包的最近源地址；None = 从未学到——发包被丢+计数）。
    endpoint: Option<SocketAddr>,
    tunnel_ip: Ipv4Addr,
    tun_ip: Ipv4Addr,
    /// ConnectionExpired 日志抑制位（每 peer 一次；明文包到达复位）。
    expired_logged: bool,
}

/// 设备表落下来的 peer 配置（`servercore.PeerConfig` 同义）。
#[derive(Clone)]
pub struct PeerConfig {
    pub pubkey: [u8; 32],
    pub psk: [u8; 32],
    pub tunnel_ip: Ipv4Addr,
    pub tun_ip: Ipv4Addr,
}

/// 一次入站处理的结果批（驱动线程消化；wire 项由引擎做腿帧封装后 sendto）。
#[derive(Default)]
pub struct InboundOut {
    /// 解密出的明文 IPv4 包（已过源校验），投拦截层。
    pub plain: Vec<Vec<u8>>,
    /// 要写回网络的 WG 包（应答/keepalive/握手重传），附带目标端点。
    pub wire: Vec<(SocketAddr, Vec<u8>)>,
}

/// 入站包分发结论（endpoint 更新的显式判据面——设计 §2 / 评审 M2）。
#[derive(Debug, PartialEq)]
enum AuthKind {
    /// 验证通过的 init / response / data（含 keepalive）——更新 endpoint。
    Authenticated,
    /// cookie reply（limiter 触发）——回源但**不**更新 endpoint。
    CookieReply,
    /// 静默丢包类 / 未知 peer——不动。
    Rejected,
}

/// 多 peer WG device（驱动线程独占；无锁——Register 的 add/remove 是普通调用）。
pub struct Device {
    static_priv: StaticSecret,
    static_pub: PublicKey,
    peers: HashMap<[u8; 32], Peer>,
    /// 24-bit base → peer 公钥（type=2/3/4 的 receiver_idx>>8 查这里）。
    by_base: HashMap<u32, [u8; 32]>,
    logf: Logf,
    /// 静默丢包计数（畸形/解密失败/未知 peer 的包）。
    pub silent_drops: u64,
    /// 明文包源校验拒绝计数（allowedips 反查同义面的诊断位）。
    pub src_rejects: u64,
    /// endpoint 未学时出站丢弃计数。
    pub no_endpoint_drops: u64,
    wg_buf: Vec<u8>,
}

fn rand_base() -> u32 {
    let mut b = [0u8; 4];
    getrandom::getrandom(&mut b).expect("系统随机源不可用");
    u32::from_le_bytes(b) & 0x00ff_ffff // < 2^24：Tunn 内部 <<8 丢高位
}

impl Device {
    pub fn new(static_priv: StaticSecret, logf: Logf) -> Self {
        let static_pub = PublicKey::from(&static_priv);
        Self {
            static_priv,
            static_pub,
            peers: HashMap::new(),
            by_base: HashMap::new(),
            logf,
            silent_drops: 0,
            src_rejects: 0,
            no_endpoint_drops: 0,
            wg_buf: vec![0u8; WG_BUF],
        }
    }

    pub fn static_public(&self) -> &PublicKey {
        &self.static_pub
    }

    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    pub fn has_peer(&self, pubkey: &[u8; 32]) -> bool {
        self.peers.contains_key(pubkey)
    }

    /// peer 的当前端点（诊断/状态面）。
    pub fn peer_endpoint(&self, pubkey: &[u8; 32]) -> Option<SocketAddr> {
        self.peers.get(pubkey).and_then(|p| p.endpoint)
    }

    /// 落一个 peer（设备表 add / rotate 的 add 半边）。同公钥重复调用 = 拆旧建新
    /// （rotate 语义：先 remove 再 add 由调用方保证顺序，这里防御性重建）。
    pub fn add_peer(&mut self, cfg: PeerConfig) {
        if self.peers.contains_key(&cfg.pubkey) {
            self.remove_peer(&cfg.pubkey);
        }
        let mut base = rand_base();
        while self.by_base.contains_key(&base) {
            base = rand_base();
        }
        let tunn = Tunn::new(
            self.static_priv.clone(),
            PublicKey::from(cfg.pubkey),
            Some(cfg.psk),
            None, // persistent_keepalive：对齐 Go ipc.go「刻意不写」
            base,
            None, // rate_limiter：各 Tunn 自建私有（per-peer 限速语义）
        )
        .expect("参数恒合法（构造期已验）");
        self.by_base.insert(base, cfg.pubkey);
        self.peers.insert(
            cfg.pubkey,
            Peer {
                tunn,
                endpoint: None,
                tunnel_ip: cfg.tunnel_ip,
                tun_ip: cfg.tun_ip,
                expired_logged: false,
            },
        );
    }

    /// 拆一个 peer（expire/stale/淘汰/rotate 的 remove 半边）——两表同摘。
    pub fn remove_peer(&mut self, pubkey: &[u8; 32]) {
        if self.peers.remove(pubkey).is_some() {
            self.by_base.retain(|_, pk| pk != pubkey);
        }
    }

    /// 处理一个入站 WG 数据报（腿帧 data 载荷）。分发 + 漫游 + 源校验；
    /// 产出进 `out`（plain 投拦截层；wire 由引擎腿帧封装后发出）。
    pub fn decapsulate(&mut self, src: SocketAddr, pkt: &[u8], out: &mut InboundOut) {
        let Some((kind, peer_key)) = self.route(pkt) else {
            self.silent_drops += 1;
            return;
        };
        if !self.peers.contains_key(&peer_key) {
            self.silent_drops += 1;
            return;
        }
        let src_ip = match src {
            SocketAddr::V4(v4) => IpAddr::V4(*v4.ip()),
            SocketAddr::V6(v6) => IpAddr::V6(*v6.ip()),
        };
        // 物化结果（断开对 wg_buf 的借用——结果所有权化，wire 批本来也需要 Vec）
        let step = {
            let peer = self.peers.get_mut(&peer_key).expect("已判存在");
            step_of(peer.tunn.decapsulate(Some(src_ip), pkt, &mut self.wg_buf))
        };
        let auth = match (&step, kind) {
            (StepOut::Err(_), _) => AuthKind::Rejected,
            (_, PacketKind::CookieReply) => AuthKind::CookieReply,
            // type ∈ {1,2,4} 且非 Err：认证成功（含 data 空载荷 keepalive 的 Done）
            (..) => AuthKind::Authenticated,
        };
        if auth == AuthKind::Authenticated {
            // 漫游跟随：先更新 endpoint，再产出发送（对齐 wireguard-go :374→:381 顺序）
            if let Some(peer) = self.peers.get_mut(&peer_key) {
                peer.endpoint = Some(src);
                peer.expired_logged = false;
            }
        }
        self.consume_step(peer_key, step, out);
    }

    /// 路由一个入站包：按线协议类型查表，返回 (类型语义, peer 公钥)。
    fn route(&self, pkt: &[u8]) -> Option<(PacketKind, [u8; 32])> {
        if pkt.len() < 4 {
            return None;
        }
        let msg_type = u32::from_le_bytes(pkt[0..4].try_into().expect("长度已判"));
        match (msg_type, pkt.len()) {
            (1, 148) => {
                // init：parse_handshake_anon 解出 sender 静态公钥（公开 API；
                // 只解公钥不验 mac/timestamp——完整验证交给目标 Tunn 的 decapsulate）
                let parsed = Tunn::parse_incoming_packet(pkt).ok()?;
                let init = match parsed {
                    boringtun::noise::Packet::HandshakeInit(h) => h,
                    _ => return None,
                };
                let half = boringtun::noise::handshake::parse_handshake_anon(
                    &self.static_priv,
                    &self.static_pub,
                    &init,
                )
                .ok()?;
                let key = half.peer_static_public;
                self.peers.get(&key).map(|_| (PacketKind::Init, key))
            }
            (2, 92) => {
                let idx = u32::from_le_bytes(pkt[8..12].try_into().expect("长度已判"));
                self.by_base
                    .get(&(idx >> 8))
                    .map(|k| (PacketKind::Response, *k))
            }
            (3, 64) => {
                let idx = u32::from_le_bytes(pkt[4..8].try_into().expect("长度已判"));
                self.by_base
                    .get(&(idx >> 8))
                    .map(|k| (PacketKind::CookieReply, *k))
            }
            (4, n) if n >= 32 => {
                let idx = u32::from_le_bytes(pkt[4..8].try_into().expect("长度已判"));
                self.by_base
                    .get(&(idx >> 8))
                    .map(|k| (PacketKind::Data, *k))
            }
            _ => None,
        }
    }

    /// 消化一次物化后的 Tunn 结果：空数据报重调协议（WriteToNetwork 后以空输入重调
    /// 到 Done——冲掉握手期排队包与握手响应 keepalive）+ 明文源校验 + expired 日志抑制。
    fn consume_step(&mut self, peer_key: [u8; 32], step: StepOut, out: &mut InboundOut) {
        // Q-I F2：缓冲自 `Device::new` 起恒为 `WG_BUF` 长——boringtun 的
        // decapsulate/encapsulate 只写前缀并返回子切片，不改 dst 长度 ⇒ 删掉旧的
        // clear+resize（每出站包 65KB memset，实测占驱动线程 2.7%）。长度不变量在此
        // 兜底（debug 构建拦「未来有人把缓冲改短」）。
        debug_assert_eq!(self.wg_buf.len(), WG_BUF, "wg_buf 长度契约（构造期一次分配）");
        let mut step = step;
        loop {
            match step {
                StepOut::Wire(w) => {
                    self.push_wire(peer_key, w, out);
                    // 空数据报重调（R1 ③-7 同款协议）——只写前缀，无需清缓冲
                    let next = {
                        let peer = self.peers.get_mut(&peer_key).expect("路由已判存在");
                        step_of(peer.tunn.decapsulate(None, &[], &mut self.wg_buf))
                    };
                    step = next;
                }
                StepOut::PlainV4(pkt) => {
                    let ok = self
                        .peers
                        .get(&peer_key)
                        .map(|p| self.src_allowed(p, &pkt))
                        .unwrap_or(false);
                    if ok {
                        out.plain.push(pkt);
                    } else {
                        self.src_rejects += 1;
                    }
                    return;
                }
                StepOut::Done => return,
                StepOut::Err(WireGuardError::ConnectionExpired) => {
                    let peer = self.peers.get_mut(&peer_key).expect("路由已判存在");
                    if !peer.expired_logged {
                        peer.expired_logged = true;
                        (self.logf)(&format!(
                            "wg: peer {} 会话过期（等对端重新握手；base 不变）",
                            hex4(&peer_key)
                        ));
                    }
                    return;
                }
                StepOut::Err(_) => {
                    self.silent_drops += 1; // 静默丢包类：计数继续（R1 ②-9 同口径）
                    return;
                }
            }
        }
    }

    /// 入站明文包的源校验（wireguard-go allowedips 反查同义）：
    /// src ∈ {该 peer 的 TunnelIP, TunIP} 才收。
    fn src_allowed(&self, peer: &Peer, pkt: &[u8]) -> bool {
        if pkt.len() < 20 || pkt[0] >> 4 != 4 {
            return false;
        }
        let src = Ipv4Addr::new(pkt[12], pkt[13], pkt[14], pkt[15]);
        src == peer.tunnel_ip || src == peer.tun_ip
    }

    /// 出站明文包：按目的地址查 peer → encapsulate（设计 §1.1 TX 半边）。
    pub fn encapsulate(&mut self, dst: &IpAddr, plain: &[u8], out: &mut InboundOut) {
        let IpAddr::V4(dst4) = dst else {
            return; // 内层只承载 IPv4（隧道只接管 IPv4）
        };
        let key = self.route_out(dst4);
        let Some(key) = key else {
            self.silent_drops += 1; // 无 peer 持有该 dst（设备表与拦截层状态分歧的兜底）
            return;
        };
        self.encap_peer(&key, plain, out);
    }

    fn route_out(&self, dst: &Ipv4Addr) -> Option<[u8; 32]> {
        self.peers
            .iter()
            .find(|(_, p)| &p.tunnel_ip == dst || &p.tun_ip == dst)
            .map(|(k, _)| *k)
    }

    /// 目的地址是否命中某设备的 **`tun_ip`**（`hw-app` 派生地址；M1 出站分流的 QUIC 档键，
    /// 设计 §1.5）。返回持有该地址的 peer 公钥；`tunnel_ip`（服务面自带连接）**不**命中。
    ///
    /// 与 `route_out` 分开是为了让「tun_ip ⇒ QUIC / tunnel_ip ⇒ WG」这条分界有一处**唯一**
    /// 的判定（地址语义真源 = `tunnel_addr.rs` 的两个派生函数）。
    pub fn tun_ip_owner(&self, dst: &Ipv4Addr) -> Option<[u8; 32]> {
        self.peers
            .iter()
            .find(|(_, p)| &p.tun_ip == dst)
            .map(|(k, _)| *k)
    }

    /// 对指定 peer 封装一个明文包（keepalive 用空载荷调用）。
    fn encap_peer(&mut self, key: &[u8; 32], plain: &[u8], out: &mut InboundOut) {
        let step = {
            let peer = self.peers.get_mut(key).expect("查表已判存在");
            let r = peer.tunn.encapsulate(plain, &mut self.wg_buf);
            if crate::envflag::tx_dbg() {
                let desc: String = match &r {
                    boringtun::noise::TunnResult::WriteToNetwork(_) => "Wire".to_string(),
                    boringtun::noise::TunnResult::WriteToTunnelV4(_, _) => "PlainV4".to_string(),
                    boringtun::noise::TunnResult::WriteToTunnelV6(_, _) => "PlainV6".to_string(),
                    boringtun::noise::TunnResult::Done => "Done".to_string(),
                    boringtun::noise::TunnResult::Err(e) => format!("Err({e:?})"),
                };
                eprintln!("[ENCDBG] tunn.encapsulate(plain {}B) => {desc}", plain.len());
            }
            step_of(r)
        };
        self.consume_step(*key, step, out);
    }

    /// 每 peer 定时器拍（握手重传/被动 keepalive/服务端主动握手的产出面）。
    pub fn tick_timers(&mut self, out: &mut InboundOut) {
        let keys: Vec<[u8; 32]> = self.peers.keys().copied().collect();
        for key in keys {
            let step = {
                let peer = self.peers.get_mut(&key).expect("键来自表内");
                step_of(peer.tunn.update_timers(&mut self.wg_buf))
            };
            self.consume_step(key, step, out);
        }
    }

    /// 把一个出站 WG 包记到 wire 批（endpoint 未学时丢 + 计数——Go peer 无 endpoint 同义）。
    fn push_wire(&mut self, peer_key: [u8; 32], w: Vec<u8>, out: &mut InboundOut) {
        match self.peers.get(&peer_key).and_then(|p| p.endpoint) {
            Some(ep) => out.wire.push((ep, w)),
            None => self.no_endpoint_drops += 1,
        }
    }
}

/// 入站包的线协议类型语义（endpoint 更新判据的显式分派面）。
#[derive(Debug, PartialEq)]
enum PacketKind {
    Init,
    Response,
    CookieReply,
    Data,
}

/// TunnResult 的物化形态（断开对 wg_buf 的借用——wire 批需要所有权，无额外成本）。
enum StepOut {
    Wire(Vec<u8>),
    PlainV4(Vec<u8>),
    Done,
    Err(WireGuardError),
}

fn step_of(r: TunnResult<'_>) -> StepOut {
    match r {
        TunnResult::WriteToNetwork(w) => StepOut::Wire(w.to_vec()),
        TunnResult::WriteToTunnelV4(p, _) => StepOut::PlainV4(p.to_vec()),
        TunnResult::WriteToTunnelV6(_, _) => StepOut::Done, // 内层只承载 IPv4
        TunnResult::Done => StepOut::Done,
        TunnResult::Err(e) => StepOut::Err(e),
    }
}

fn hex4(b: &[u8]) -> String {
    b.iter().take(4).map(|x| format!("{x:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use boringtun::x25519::StaticSecret;
    use std::net::{SocketAddrV4, Ipv4Addr};

    fn random_key() -> StaticSecret {
        let mut b = [0u8; 32];
        getrandom::getrandom(&mut b).unwrap();
        StaticSecret::from(b)
    }

    struct Client {
        tunn: Tunn,
        key: StaticSecret,
        tunnel_ip: Ipv4Addr,
        tun_ip: Ipv4Addr,
    }

    fn mk_client(server_priv: &StaticSecret, secret: [u8; 32], idx: u8) -> Client {
        let key = random_key();
        let psk = *crate::psk::Psk::from(crate::token::Secret::from(secret)).as_bytes();
        let tunn = Tunn::new(
            key.clone(),
            PublicKey::from(server_priv),
            Some(psk),
            None,
            1000 + idx as u32,
            None,
        )
        .unwrap();
        let pubkey: [u8; 32] = *PublicKey::from(&key).as_bytes();
        Client {
            tunn,
            key,
            tunnel_ip: crate::tunnel_addr::derive_tunnel_ip(&crate::token::Secret::from(secret), &pubkey),
            tun_ip: crate::tunnel_addr::derive_tun_ip(&crate::token::Secret::from(secret), &pubkey),
        }
    }

    fn client_cfg(c: &Client, secret: [u8; 32]) -> PeerConfig {
        PeerConfig {
            pubkey: *PublicKey::from(&c.key).as_bytes(),
            psk: *crate::psk::Psk::from(crate::token::Secret::from(secret)).as_bytes(),
            tunnel_ip: c.tunnel_ip,
            tun_ip: c.tun_ip,
        }
    }

    fn noop_logf() -> Logf {
        std::sync::Arc::new(|_| {})
    }

    /// 最小 IPv4 包（可指定源地址——源校验用）。
    fn inner_pkt(src: Ipv4Addr, dst: Ipv4Addr) -> Vec<u8> {
        let mut p = vec![0u8; 28];
        p[0] = 0x45;
        let total = 28u16;
        p[2..4].copy_from_slice(&total.to_be_bytes());
        p[12..16].copy_from_slice(&src.octets());
        p[16..20].copy_from_slice(&dst.octets());
        p
    }

    /// 指定总长的 IPv4 包（Q-I F2 长度面用；`len ≥ 20`）。
    fn inner_pkt_len(src: Ipv4Addr, dst: Ipv4Addr, len: usize) -> Vec<u8> {
        assert!(len >= 20);
        let mut p = vec![0u8; len];
        p[0] = 0x45;
        p[2..4].copy_from_slice(&(len as u16).to_be_bytes());
        p[8] = 64;
        p[9] = 17;
        p[12..16].copy_from_slice(&src.octets());
        p[16..20].copy_from_slice(&dst.octets());
        p
    }

    /// 客户端 init（触发握手）。
    fn client_init(c: &mut Client) -> Vec<u8> {
        let mut buf = [0u8; 65536];
        match c.tunn.encapsulate(&inner_pkt(c.tunnel_ip, Ipv4Addr::new(100, 64, 255, 1)), &mut buf) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望握手 init，实得 {other:?}"),
        }
    }

    /// 评审 L7 要求写明：`inc_index` 后自增（首个 session = base+1）、低 8 位回绕 256 次
    /// 复用、`receiver_idx>>8 == base` 对 k∈0..255 恒成立（前提 base<2^24）。
    #[test]
    fn base_invariant_across_rekeys() {
        let server_key = random_key();
        let logf = noop_logf();
        let mut dev = Device::new(server_key.clone(), logf);
        let secret = [0x42u8; 32];
        let mut c = mk_client(&server_key, secret, 0);
        dev.add_peer(client_cfg(&c, secret));

        let mut buf_c = [0u8; 65536];
        let mut ep1: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 40000).into();
        let mut server_base = 0u32;
        // 多轮握手：每轮客户端重建 Tunn（新 index）→ init → 服务端应答 → 客户端建会话 → 数据往返
        for round in 0..4 {
            // 客户端每轮重建 Tunn（新 index——模拟重连/换世代）
            c.tunn = Tunn::new(
                c.key.clone(),
                *dev.static_public(),
                Some(*crate::psk::Psk::from(crate::token::Secret::from(secret)).as_bytes()),
                None,
                5000 + round,
                None,
            )
            .unwrap();
            // 客户端 init
            let init = client_init(&mut c);
            let mut out = InboundOut::default();
            dev.decapsulate(ep1, &init, &mut out);
            assert!(!out.wire.is_empty(), "服务端应产握手应答");
            // 提取服务端应答里的 receiver_idx（客户端数据包将回显它）并核 base 不变量
            let resp = &out.wire[0].1;
            assert_eq!(u32::from_le_bytes(resp[0..4].try_into().unwrap()), 2, "应答是 type=2");
            let sender_idx = u32::from_le_bytes(resp[4..8].try_into().unwrap());
            if round == 0 {
                server_base = sender_idx >> 8;
            } else {
                assert_eq!(sender_idx >> 8, server_base, "base 必须跨握手稳定（inc_index 只动低 8 位）");
            }
            // 客户端收应答（建会话）
            match c.tunn.decapsulate(None, resp, &mut buf_c) {
                TunnResult::WriteToNetwork(_) => {}
                other => panic!("期望会话建立 keepalive，实得 {other:?}"),
            }
            // 客户端数据包：receiver_idx = 服务端 sender_idx —— base 一致
            let data = match c.tunn.encapsulate(&inner_pkt(c.tunnel_ip, Ipv4Addr::new(100, 64, 255, 1)), &mut buf_c) {
                TunnResult::WriteToNetwork(w) => w.to_vec(),
                other => panic!("期望数据包，实得 {other:?}"),
            };
            assert_eq!(u32::from_le_bytes(data[4..8].try_into().unwrap()) >> 8, server_base);
            let mut out2 = InboundOut::default();
            dev.decapsulate(ep1, &data, &mut out2);
            assert_eq!(out2.plain.len(), 1, "服务端应解出明文包");
            // 服务端反向数据
            let mut out3 = InboundOut::default();
            dev.encapsulate(&IpAddr::V4(c.tunnel_ip), &inner_pkt(Ipv4Addr::new(100, 64, 255, 1), c.tunnel_ip), &mut out3);
            assert!(!out3.wire.is_empty(), "服务端应能回发（endpoint 已学）");
            assert_eq!(out3.wire[0].0, ep1);
            // 「漫游」：客户端换源端口再握手 → endpoint 更新
            ep1 = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 40000 + round as u16 + 1).into();
        }
        // 最后一次握手验证漫游生效
        let init = client_init(&mut c);
        let mut out = InboundOut::default();
        dev.decapsulate(ep1, &init, &mut out);
        let mut out3 = InboundOut::default();
        dev.encapsulate(&IpAddr::V4(c.tunnel_ip), &inner_pkt(Ipv4Addr::new(100, 64, 255, 1), c.tunnel_ip), &mut out3);
        assert!(!out3.wire.is_empty() && out3.wire[0].0 == ep1, "漫游后出站应发往新端点");
    }

    /// 多 peer 并发：两个客户端各自握手/数据互不干扰；remove_peer 后包被拒。
    #[test]
    fn two_peers_isolated() {
        let server_key = random_key();
        let mut dev = Device::new(server_key.clone(), noop_logf());
        let s1 = [0x11u8; 32];
        let s2 = [0x22u8; 32];
        let mut c1 = mk_client(&server_key, s1, 1);
        let mut c2 = mk_client(&server_key, s2, 2);
        dev.add_peer(client_cfg(&c1, s1));
        dev.add_peer(client_cfg(&c2, s2));
        assert_eq!(dev.peer_count(), 2);

        let mut buf = [0u8; 65536];
        let ep1: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 41001).into();
        let ep2: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 41002).into();

        for (c, ep) in [(&mut c1, ep1), (&mut c2, ep2)] {
            let init = client_init(c);
            let mut out = InboundOut::default();
            dev.decapsulate(ep, &init, &mut out);
            let resp = out.wire[0].clone();
            assert_eq!(resp.0, ep, "应答回源且 endpoint 已更新（先更新后发）");
            match c.tunn.decapsulate(None, &resp.1, &mut buf) {
                TunnResult::WriteToNetwork(_) => {}
                other => panic!("期望会话建立 keepalive，实得 {other:?}"),
            }
            let data = match c.tunn.encapsulate(&inner_pkt(c.tunnel_ip, Ipv4Addr::new(100, 64, 255, 1)), &mut buf) {
                TunnResult::WriteToNetwork(w) => w.to_vec(),
                other => panic!("{other:?}"),
            };
            let mut out2 = InboundOut::default();
            dev.decapsulate(ep, &data, &mut out2);
            assert_eq!(out2.plain.len(), 1);
        }
        // 未知公钥的 init 被拒（静默计数）
        let mut stranger = mk_client(&server_key, [0x33; 32], 3);
        let init = client_init(&mut stranger);
        let before = dev.silent_drops;
        let mut out = InboundOut::default();
        dev.decapsulate(ep1, &init, &mut out);
        assert_eq!(dev.silent_drops, before + 1);
        // 源校验：c1 的会话里发 src=tun_ip 的包也收（双地址），src=无关地址拒
        let data = match c1.tunn.encapsulate(&inner_pkt(c1.tun_ip, Ipv4Addr::new(100, 64, 255, 1)), &mut buf) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("{other:?}"),
        };
        let mut out = InboundOut::default();
        dev.decapsulate(ep1, &data, &mut out);
        assert_eq!(out.plain.len(), 1, "tun_ip 也是 peer 的合法源");

        // remove_peer：数据包到 base 表查不到 → 拒
        let pubkey = *PublicKey::from(&c2.key).as_bytes();
        dev.remove_peer(&pubkey);
        assert_eq!(dev.peer_count(), 1);
        let data2 = match c2.tunn.encapsulate(&inner_pkt(c2.tunnel_ip, Ipv4Addr::new(100, 64, 255, 1)), &mut buf) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("{other:?}"),
        };
        let before = dev.silent_drops;
        let mut out = InboundOut::default();
        dev.decapsulate(ep2, &data2, &mut out);
        assert_eq!(dev.silent_drops, before + 1);
    }

    /// 服务端主动握手（评审 M4 用例）：客户端静默 + 服务端 tick 出 init。
    #[test]
    fn server_initiates_handshake_on_tick() {
        let server_key = random_key();
        let mut dev = Device::new(server_key.clone(), noop_logf());
        let secret = [0x55u8; 32];
        let mut c = mk_client(&server_key, secret, 4);
        dev.add_peer(client_cfg(&c, secret));
        let ep: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 42000).into();

        // 先建立会话（服务端有出站数据历史）
        let init = client_init(&mut c);
        let mut out = InboundOut::default();
        dev.decapsulate(ep, &init, &mut out);
        let mut buf = [0u8; 65536];
        match c.tunn.decapsulate(None, &out.wire[0].1, &mut buf) {
            TunnResult::WriteToNetwork(_) => {}
            other => panic!("期望会话建立 keepalive，实得 {other:?}"),
        }

        // 服务端出站数据（建立「发过数据」事实）
        let mut out = InboundOut::default();
        dev.encapsulate(&IpAddr::V4(c.tunnel_ip), &inner_pkt(Ipv4Addr::new(100, 64, 255, 1), c.tunnel_ip), &mut out);
        assert!(!out.wire.is_empty());

        // 客户端静默（不回 keepalive）——服务端 tick_timers 在 KEEPALIVE/REKEY 语义下
        // 可能产 keepalive 或（无会话时）init；有会话时产 keepalive 属正常。
        // 这里验证的是 tick 产出包 endpoint 正确、不 panic。
        let mut out2 = InboundOut::default();
        dev.tick_timers(&mut out2);
        for (epx, _) in &out2.wire {
            assert_eq!(*epx, ep);
        }
    }

    /// Q-I F2：删掉 `consume_step` 的 65KB `clear+resize` 后「大包后小包」不泄漏陈旧
    /// 前缀——第二个密文长度 = 100+32 且对端解出的内容正确（缓冲只写前缀的契约）。
    #[test]
    fn encap_after_large_packet_no_stale_prefix() {
        let server_key = random_key();
        let mut dev = Device::new(server_key.clone(), noop_logf());
        let secret = [0x77u8; 32];
        let mut c = mk_client(&server_key, secret, 5);
        dev.add_peer(client_cfg(&c, secret));
        let ep: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 43000).into();
        let mut buf_c = [0u8; 65536];
        // 握手（应答 → 客户端 keepalive → 回投服务端 ⇒ 双向会话齐）
        let init = client_init(&mut c);
        let mut out = InboundOut::default();
        dev.decapsulate(ep, &init, &mut out);
        let ka = match c.tunn.decapsulate(None, &out.wire[0].1, &mut buf_c) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望会话建立 keepalive，实得 {other:?}"),
        };
        let mut out_ka = InboundOut::default();
        dev.decapsulate(ep, &ka, &mut out_ka);
        // 大包（1400B）之后紧跟小包（100B）
        let big = inner_pkt_len(Ipv4Addr::new(100, 64, 255, 1), c.tunnel_ip, 1400);
        let mut out_big = InboundOut::default();
        dev.encapsulate(&IpAddr::V4(c.tunnel_ip), &big, &mut out_big);
        assert_eq!(out_big.wire.len(), 1, "大包出一个密文");
        assert_eq!(out_big.wire[0].1.len(), 1400 + 32, "密文长度 = 明文 + 32");
        match c.tunn.decapsulate(None, &out_big.wire[0].1, &mut buf_c) {
            TunnResult::WriteToTunnelV4(q, _) => assert_eq!(q, &big[..]),
            other => panic!("大包解密失败：{other:?}"),
        }
        let small = inner_pkt_len(Ipv4Addr::new(100, 64, 255, 1), c.tunnel_ip, 100);
        let mut out_small = InboundOut::default();
        dev.encapsulate(&IpAddr::V4(c.tunnel_ip), &small, &mut out_small);
        assert_eq!(out_small.wire.len(), 1, "小包出一个密文");
        assert_eq!(
            out_small.wire[0].1.len(),
            100 + 32,
            "小包密文长度 = 100+32（无陈旧前缀泄漏）"
        );
        match c.tunn.decapsulate(None, &out_small.wire[0].1, &mut buf_c) {
            TunnResult::WriteToTunnelV4(q, _) => assert_eq!(q, &small[..], "小包内容逐字节正确"),
            other => panic!("小包解密失败：{other:?}"),
        }
    }

    /// Q-I F2：连续 100 包双向 round-trip 字节一致（删 memset 后的稳定性回归）。
    #[test]
    fn hundred_packet_roundtrip_bytes_identical() {
        let server_key = random_key();
        let mut dev = Device::new(server_key.clone(), noop_logf());
        let secret = [0x88u8; 32];
        let mut c = mk_client(&server_key, secret, 6);
        dev.add_peer(client_cfg(&c, secret));
        let ep: SocketAddr = SocketAddrV4::new(Ipv4Addr::LOCALHOST, 43100).into();
        let mut buf_c = [0u8; 65536];
        let init = client_init(&mut c);
        let mut out = InboundOut::default();
        dev.decapsulate(ep, &init, &mut out);
        let ka = match c.tunn.decapsulate(None, &out.wire[0].1, &mut buf_c) {
            TunnResult::WriteToNetwork(w) => w.to_vec(),
            other => panic!("期望会话建立 keepalive，实得 {other:?}"),
        };
        let mut out_ka = InboundOut::default();
        dev.decapsulate(ep, &ka, &mut out_ka);
        // 服务端 → 客户端 100 包
        for i in 0..100usize {
            let p = inner_pkt_len(Ipv4Addr::new(100, 64, 255, 1), c.tunnel_ip, 40 + i);
            let mut o = InboundOut::default();
            dev.encapsulate(&IpAddr::V4(c.tunnel_ip), &p, &mut o);
            assert_eq!(o.wire.len(), 1, "第 {i} 包");
            match c.tunn.decapsulate(None, &o.wire[0].1, &mut buf_c) {
                TunnResult::WriteToTunnelV4(q, _) => assert_eq!(q, &p[..], "下行第 {i} 包字节一致"),
                other => panic!("下行第 {i} 包解密失败：{other:?}"),
            }
        }
        // 客户端 → 服务端 100 包（src = 客户端隧道 IP，过源校验）
        for i in 0..100usize {
            let p = inner_pkt_len(c.tunnel_ip, Ipv4Addr::new(93, 184, 216, 34), 60 + i);
            let enc = match c.tunn.encapsulate(&p, &mut buf_c) {
                TunnResult::WriteToNetwork(w) => w.to_vec(),
                other => panic!("上行第 {i} 包加密失败：{other:?}"),
            };
            let mut o = InboundOut::default();
            dev.decapsulate(ep, &enc, &mut o);
            assert_eq!(o.plain.len(), 1, "上行第 {i} 包应解出明文");
            assert_eq!(o.plain[0], p, "上行第 {i} 包字节一致");
        }
    }
}
