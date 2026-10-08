//! `tools/quic-probe` —— M1 S1-6 的**探针客户端**（harness-only，**不进产品面**）。
//!
//! 为什么需要它：S1-6 的判据是「本地中继实测：`local-rust-relay.sh` + 本地出口 + 探针
//! 客户端经中继打通」（M1 设计 §10 S1-6），而 S2 的岛客户端还没落地 ⇒ 本探针 = 那个
//! 「最小 quinn 客户端」（设计 §10 S1-6 原话允许），并顺带做 §7.2 的 B1/B2 读数。
//!
//! 它复刻三处**客户端侧**语义（真源逐条列出，便于对账）：
//!
//! 1. **S2-7 的包封/剥壳**（设计 §1.6 末段）：`via=relay` 时上行 = `[0xAA][label8]‖
//!    `[0xBB][kind=5]‖quic_pkt`、下行收 `[0xBB][5]‖quic_pkt` 剥壳；非 kind=5 帧**忽略**
//!    （中继会推 hint 控制帧）。`via=direct` 时两端都裸 QUIC 包。
//! 2. **`hr-reg4` 四帧准入**（真源 `homeway-quic::reg4`，M2 §1.2）：Hello（`"H4"`）→
//!    Challenge（`"C4"`+nonce16）→ Proof（`"P4"`；
//!    `mac = HMAC-SHA256(secret, "hr-reg4"‖pubkey‖devTag‖ts‖nonce‖exporter32)[:16]`）→
//!    Accept（`"A4"`）；`exporter32 = conn.export_keying_material(b"hw-quic-reg", b"")[:32]`
//!    （**连接绑定**）。
//! 3. **服务端 RPK 钉定**（真源 `homeway-quic::exit::rpk::client_pin`）：先比对服务端出示
//!    的 SPKI 与 token 里的 32B 公钥（换成的 44B SPKI），再走
//!    `verify_tls13_signature_with_raw_key` 真验签——**不是**跳过验证。
//!    SECURITY: harness-only —— 自定义 verifier 的 `dangerous()` 安装点**只允许**出现在
//!    `tools/`（`tools/check-quic-isolation.sh` 第 ⑤ 条断言 `crates/` 零命中）。
//! 4. **设备地址派生**（真源 `homeway-core::tunnel_addr`）：`100.64.<HMAC(secret,
//!    "hw-tun"‖pubkey)[:2]>` 与 `hw-app`（含撞车守卫）——探针据此造**源合法**的内层包。
//!
//! 用法（读数为机器可解析的 `key=value` 行；`--json` 时合成一行 JSON）：
//!
//! ```text
//! quic-probe --token <hmw1…> [--via relay --relay <ip:port>] conn    # 握手 + 四帧准入 + DNS 一问一答
//! quic-probe --token <hmw1…> [--via relay] push --n 2000 --size 1280 # 上行灌包（B1/B2 读数）
//! ```
//!
//! 读数口径（设计 §7.2 B1）：**尺寸取本 socket 的字节计数**（= 线上 UDP 载荷，含中继
//! 信封），不用 lo0 分片断言（lo0 MTU 16384 ⇒ 该断言恒真，r12 专6.7）。

use std::io::{self, IoSliceMut};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use quinn::udp::{RecvMeta, Transmit};
use quinn::{AsyncUdpSocket, UdpPoller};
use rustls::pki_types::{CertificateDer, ServerName, SubjectPublicKeyInfoDer, UnixTime};
use sha2::{Digest, Sha256};

// ---------- 线协议常量（真源见模块头） ----------
const RELAY_TAG_MAGIC: u8 = 0xAA;
const FRAME_MAGIC: u8 = 0xBB;
const FRAME_KIND_QUIC: u8 = 5;
const RELAY_TAG_LEN: usize = 9; // [0xAA][label8]
const ENV_UP: usize = 2; // [0xBB][kind]
const HELLO_LEN: usize = 50;
const CHALLENGE_LEN: usize = 18;
const PROOF_LEN: usize = 82;
const ACCEPT_LEN: usize = 2;
const EXPORTER_LABEL: &[u8] = b"hw-quic-reg";
/// 准入 Proof 的 MAC 域标签（M2 §1.2 的 `hr-reg4`；刷新帧是另一域，探针面不需要）。
const PROOF_MAC_LABEL: &[u8] = b"hr-reg4";
const SERVER_NAME: &str = "homeway";
const SERVER_TUNNEL_IP: [u8; 4] = [100, 64, 255, 1];
const INNER_MTU: usize = 1280;

// ---------- 命令行 ----------

struct Args {
    token: Option<String>,
    quic_ep: Option<SocketAddr>,
    relay_ep: Option<SocketAddr>,
    rpk_hex: Option<String>,
    secret_hex: Option<String>,
    pubkey_hex: Option<String>,
    devtag_hex: Option<String>,
    n: usize,
    size: usize,
    dns: bool,
    json: bool,
    hold: u64,
    wait_ms: u64,
    /// 发 reg3 之后、发数据报之前的等待（ms）：准入是「引擎裁决 + 回执 + 绑定」三步，
    /// 立刻发数据报会被判「未登记连接的数据报」丢掉（§1.3 的准入面）。
    reg_wait: u64,
    /// 内层包的目的地址（`ip:port`；缺省 = DNS 形态走隧道 IP:53，否则哑载荷走 10.0.0.1:30000）
    dst: Option<String>,
    /// push 模式的发送节拍（pps；0 = 尽快发——用于触中继 200pps 闸的 B2 形态）
    rate: u32,
    /// 本地绑定地址（M1 S5-3 B2 的**源桶分离**：中继限流按 `src.ip()` 计桶，而
    /// 本地出口与探针同为 `127.0.0.1` ⇒ 共桶（200pps 被两边分）；把探针绑到
    /// `127.0.0.2` 即得独立桶——形态差异照设计 §7.2 B2/r12 专4-5 登记）。
    bind: String,
    cmd: String,
}

impl Args {
    fn parse() -> Result<Args, String> {
        let argv: Vec<String> = std::env::args().collect();
        let mut a = Args {
            token: None,
            quic_ep: None,
            relay_ep: None,
            rpk_hex: None,
            secret_hex: None,
            pubkey_hex: None,
            devtag_hex: None,
            n: 64,
            size: INNER_MTU,
            dns: false,
            json: false,
            hold: 0,
            wait_ms: 3000,
            reg_wait: 400,
            dst: None,
            rate: 0,
            bind: "0.0.0.0:0".into(),
            cmd: "conn".to_owned(),
        };
        let mut i = 1;
        while i < argv.len() {
            let v = argv[i].as_str();
            let next = |i: &mut usize| -> Result<String, String> {                *i += 1;
                argv.get(*i).cloned().ok_or_else(|| format!("{v} 缺参数"))
            };
            match v {
                "--token" => a.token = Some(next(&mut i)?),
                "--quic" => a.quic_ep = Some(next(&mut i)?.parse().map_err(|e| format!("--quic: {e}"))?),
                "--relay" => a.relay_ep = Some(next(&mut i)?.parse().map_err(|e| format!("--relay: {e}"))?),
                "--rpk" => a.rpk_hex = Some(next(&mut i)?),
                "--secret" => a.secret_hex = Some(next(&mut i)?),
                "--pubkey" => a.pubkey_hex = Some(next(&mut i)?),
                "--devtag" => a.devtag_hex = Some(next(&mut i)?),
                "--n" => a.n = next(&mut i)?.parse().map_err(|e| format!("--n: {e}"))?,
                "--size" => a.size = next(&mut i)?.parse().map_err(|e| format!("--size: {e}"))?,
                "--hold" => a.hold = next(&mut i)?.parse().map_err(|e| format!("--hold: {e}"))?,
                "--wait-ms" => a.wait_ms = next(&mut i)?.parse().map_err(|e| format!("--wait-ms: {e}"))?,
                "--reg-wait" => a.reg_wait = next(&mut i)?.parse().map_err(|e| format!("--reg-wait: {e}"))?,
                "--dst" => a.dst = Some(next(&mut i)?),
                "--rate" => a.rate = next(&mut i)?.parse().map_err(|e| format!("--rate: {e}"))?,
                "--bind" => a.bind = next(&mut i)?.parse().map_err(|e| format!("--bind: {e}"))?,
                "--dns" => a.dns = true,
                "--json" => a.json = true,
                "conn" | "push" => a.cmd = v.to_owned(),
                other if other.starts_with("--") => return Err(format!("未知参数 {other}")),
                other => return Err(format!("未知子命令 {other}")),
            }
            i += 1;
        }
        Ok(a)
    }
}

// ---------- 最小 token 解析（`hmw1`；真源 crates/homeway-core/src/token.rs） ----------

struct TokenInfo {
    peer_id: [u8; 32],
    secret: [u8; 32],
    endpoints: Vec<(u8, String)>,
    rpk: Option<[u8; 32]>,
}

fn parse_token(s: &str) -> Result<TokenInfo, String> {
    let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let body = s.strip_prefix("hmw1").ok_or("缺少 hmw1 前缀")?;
    let raw = B64.decode(body).map_err(|e| format!("base64: {e}"))?;
    if raw.len() < 32 + 32 + 1 + 4 {
        return Err("载荷过短".into());
    }
    // crc = SHA-256(前文)[:4]
    let (head, tail) = raw.split_at(raw.len() - 4);
    let sum = Sha256::digest(head);
    if sum[..4] != tail[..] {
        return Err("CRC 校验失败".into());
    }
    let peer_id: [u8; 32] = head[0..32].try_into().unwrap();
    let secret: [u8; 32] = head[32..64].try_into().unwrap();
    let count = head[64] as usize;
    let mut off = 65;
    let mut endpoints = Vec::new();
    for _ in 0..count {
        if off + 2 > head.len() {
            return Err("端点段越界".into());
        }
        let typ = head[off];
        let n = head[off + 1] as usize;
        off += 2;
        if off + n > head.len() {
            return Err("端点地址越界".into());
        }
        let addr = std::str::from_utf8(&head[off..off + n]).map_err(|_| "端点地址非 UTF-8")?.to_owned();
        off += n;
        endpoints.push((typ, addr));
    }
    // 尾字段：恰 32B = RPK（M1 additive）；其余长度判非法
    let rest = &head[off..];
    let rpk = match rest.len() {
        0 => None,
        32 => Some(rest.try_into().unwrap()),
        n => return Err(format!("尾字段长度 {n} 非法（只允许 0 或 32）")),
    };
    Ok(TokenInfo { peer_id, secret, endpoints, rpk })
}

/// 端点类别：0=direct（WG）1=relay **2=QUIC**（M1 additive；真源 `EndpointKind`）。
fn kind_name(t: u8) -> &'static str {
    match t {
        0 => "wg",
        1 => "relay",
        2 => "quic",
        _ => "?",
    }
}

// ---------- 设备地址派生（真源 crates/homeway-core/src/tunnel_addr.rs） ----------

fn hmac_sum(key: &[u8; 32], label: &[u8], pubkey: &[u8; 32]) -> [u8; 32] {
    let mut m = Hmac::<Sha256>::new_from_slice(key).expect("HMAC 任意长密钥");
    m.update(label);
    m.update(pubkey);
    m.finalize().into_bytes().into()
}

fn to_v(sum: &[u8; 32], at: usize) -> u16 {
    ((u16::from(sum[at]) << 8) | u16::from(sum[at + 1])) % 65534 + 1
}

fn addr_of(v: u16) -> [u8; 4] {
    [100, 64, (v >> 8) as u8, v as u8]
}

fn derive_tunnel_ip(secret: &[u8; 32], pubkey: &[u8; 32]) -> [u8; 4] {
    let mut ip = addr_of(to_v(&hmac_sum(secret, b"hw-tun", pubkey), 0));
    let mut i = 2u8;
    while i <= 9 && ip == SERVER_TUNNEL_IP {
        ip = addr_of(to_v(&hmac_sum(secret, format!("hw-tun.{i}").as_bytes(), pubkey), 0));
        i += 1;
    }
    ip
}

fn derive_tun_ip(secret: &[u8; 32], pubkey: &[u8; 32]) -> [u8; 4] {
    let tunnel = derive_tunnel_ip(secret, pubkey);
    let mut ip = addr_of(to_v(&hmac_sum(secret, b"hw-app", pubkey), 2));
    let mut i = 2u8;
    while i <= 9 && (ip == tunnel || ip == SERVER_TUNNEL_IP) {
        ip = addr_of(to_v(&hmac_sum(secret, format!("hw-app.{i}").as_bytes(), pubkey), 2));
        i += 1;
    }
    ip
}

/// 中继路由标签 = `sha256(peerId)[:8]`（真源 `wtransport::frame::relay_id`）。
fn relay_label(peer_id: &[u8; 32]) -> [u8; 8] {
    Sha256::digest(peer_id)[..8].try_into().unwrap()
}

// ---------- hr-reg4 组帧（M2 §1.2：Hello / Proof / 刷新帧） ----------

/// Hello（50B）：`"H4"‖pubkey32‖devTag8‖ts8`。
fn hello_frame(pubkey: &[u8; 32], devtag: &[u8; 8], ts: u64) -> [u8; HELLO_LEN] {
    let mut out = [0u8; HELLO_LEN];
    out[..2].copy_from_slice(b"H4");
    out[2..34].copy_from_slice(pubkey);
    out[34..42].copy_from_slice(devtag);
    out[42..50].copy_from_slice(&ts.to_be_bytes());
    out
}

/// Proof（82B）：`"P4"‖pubkey32‖devTag8‖ts8‖nonce16‖mac16`，
/// `mac = HMAC-SHA256(secret, "hr-reg4"‖pubkey‖devTag‖ts‖nonce‖exporter32)[:16]`。
fn proof_frame(
    secret: &[u8; 32],
    pubkey: &[u8; 32],
    devtag: &[u8; 8],
    ts: u64,
    nonce: &[u8; 16],
    exporter: &[u8; 32],
) -> [u8; PROOF_LEN] {
    let mut m = Hmac::<Sha256>::new_from_slice(secret).expect("HMAC 任意长密钥");
    m.update(PROOF_MAC_LABEL);
    m.update(pubkey);
    m.update(devtag);
    m.update(&ts.to_be_bytes());
    m.update(nonce);
    m.update(exporter);
    let mac = m.finalize().into_bytes();
    let mut out = [0u8; PROOF_LEN];
    out[..2].copy_from_slice(b"P4");
    out[2..34].copy_from_slice(pubkey);
    out[34..42].copy_from_slice(devtag);
    out[42..50].copy_from_slice(&ts.to_be_bytes());
    out[50..66].copy_from_slice(nonce);
    out[66..82].copy_from_slice(&mac[..16]);
    out
}

// ---------- 内层包（合成 IP 流） ----------

/// 互联网校验和（IPv4 头 / UDP 伪头用）。
fn inet_checksum(words: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    for i in (0..words.len()).step_by(2) {
        let w = if i + 1 < words.len() {
            u16::from_be_bytes([words[i], words[i + 1]])
        } else {
            u16::from(words[i]) << 8
        };
        sum += u32::from(w);
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// 内层 IPv4 + UDP 包（DNS 查询或哑载荷）。
///
/// 校验和**必须算**：内层包会被推给出口的 smoltcp 栈（`intercept.device.rx_push`），
/// 而 smoltcp 对 IPv4 头校验和是**强制**校验的（0 = 坏包静默丢）——探针起初留 0
/// 导致 DNS 面收不到查询（现场：出口 `dns: q=0`，排查记录见 M1.md 的本批读数）。
fn inner_pkt(src: [u8; 4], dst: [u8; 4], sport: u16, dport: u16, payload: &[u8]) -> Vec<u8> {
    let mut p = vec![0u8; 20 + 8 + payload.len()];
    p[0] = 0x45;
    p[2..4].copy_from_slice(&((20 + 8 + payload.len()) as u16).to_be_bytes());
    p[8] = 64; // TTL
    p[9] = 17; // UDP
    p[12..16].copy_from_slice(&src);
    p[16..20].copy_from_slice(&dst);
    let ip_sum = inet_checksum(&p[..20]);
    p[10..12].copy_from_slice(&ip_sum.to_be_bytes());
    p[20..22].copy_from_slice(&sport.to_be_bytes());
    p[22..24].copy_from_slice(&dport.to_be_bytes());
    p[24..26].copy_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    p[28..].copy_from_slice(payload);
    // UDP 校验和（伪头 + UDP 头 + 载荷；IPv4 允许 0，但真实栈都会算）
    let mut pseudo = Vec::with_capacity(12 + 8 + payload.len());
    pseudo.extend_from_slice(&src);
    pseudo.extend_from_slice(&dst);
    pseudo.push(0);
    pseudo.push(17);
    pseudo.extend_from_slice(&((8 + payload.len()) as u16).to_be_bytes());
    pseudo.extend_from_slice(&p[20..]);
    let mut udp_sum = inet_checksum(&pseudo);
    if udp_sum == 0 {
        udp_sum = 0xffff; // RFC 768：算出 0 时置全 1
    }
    p[26..28].copy_from_slice(&udp_sum.to_be_bytes());
    p
}

/// 最小 DNS 查询（id=0x1234，A 记录）。
fn dns_query(name: &str) -> Vec<u8> {
    let mut q = Vec::new();
    q.extend_from_slice(&0x1234u16.to_be_bytes());
    q.extend_from_slice(&0x0100u16.to_be_bytes()); // RD
    q.extend_from_slice(&1u16.to_be_bytes()); // qdcount
    q.extend_from_slice(&0u16.to_be_bytes());
    q.extend_from_slice(&0u16.to_be_bytes());
    q.extend_from_slice(&0u16.to_be_bytes());
    for part in name.split('.') {
        q.push(part.len() as u8);
        q.extend_from_slice(part.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&1u16.to_be_bytes()); // A
    q.extend_from_slice(&1u16.to_be_bytes()); // IN
    q
}

// ---------- 客户端 socket（S2-7 包封/剥壳的探针形态） ----------

#[derive(Default, Debug)]
#[allow(dead_code)] // 计数面全量打印（含当前不读的字段）——harness 读数纪律
struct SockStats {
    tx_dgrams: AtomicU64,
    tx_bytes: AtomicU64,
    tx_max: AtomicU64,
    rx_dgrams: AtomicU64,
    rx_bytes: AtomicU64,
    rx_max: AtomicU64,
    rx_ignored: AtomicU64,
}

#[derive(Debug)]
struct ProbeSock {
    io: tokio::net::UdpSocket,
    label: Option<[u8; 8]>,
    stats: Arc<SockStats>,
}

impl AsyncUdpSocket for ProbeSock {
    fn create_io_poller(self: Arc<Self>) -> Pin<Box<dyn UdpPoller>> {
        Box::pin(ProbePoller { sock: self })
    }

    fn try_send(&self, t: &Transmit<'_>) -> io::Result<()> {
        let n = if let Some(label) = self.label {
            let mut buf = Vec::with_capacity(RELAY_TAG_LEN + ENV_UP + t.contents.len());
            buf.push(RELAY_TAG_MAGIC);
            buf.extend_from_slice(&label);
            buf.push(FRAME_MAGIC);
            buf.push(FRAME_KIND_QUIC);
            buf.extend_from_slice(t.contents);
            self.io.try_send_to(&buf, t.destination).map(|_| buf.len())?
        } else {
            self.io.try_send_to(t.contents, t.destination)?
        };
        self.stats.tx_dgrams.fetch_add(1, Ordering::SeqCst);
        self.stats.tx_bytes.fetch_add(n as u64, Ordering::SeqCst);
        self.stats.tx_max.fetch_max(n as u64, Ordering::SeqCst);
        Ok(())
    }

    fn poll_recv(
        &self,
        cx: &mut Context<'_>,
        bufs: &mut [IoSliceMut<'_>],
        meta: &mut [RecvMeta],
    ) -> Poll<io::Result<usize>> {
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
            self.stats.rx_dgrams.fetch_add(1, Ordering::SeqCst);
            self.stats.rx_bytes.fetch_add(n as u64, Ordering::SeqCst);
            self.stats.rx_max.fetch_max(n as u64, Ordering::SeqCst);
            // 剥壳（`via=relay`）；非 kind=5 帧忽略（中继会推 hint 控制帧）
            let start = if self.label.is_some() {
                if n < ENV_UP || buf[0] != FRAME_MAGIC || buf[1] != FRAME_KIND_QUIC {
                    self.stats.rx_ignored.fetch_add(1, Ordering::SeqCst);
                    continue;
                }
                ENV_UP
            } else {
                0
            };
            let len = n - start;
            if start > 0 {
                buf.copy_within(start..n, 0);
            }
            meta[0] = RecvMeta { addr: src, len, stride: len, ecn: None, dst_ip: None };
            return Poll::Ready(Ok(1));
        }
    }

    fn local_addr(&self) -> io::Result<SocketAddr> {
        self.io.local_addr()
    }

    fn max_transmit_segments(&self) -> usize {
        1
    }
    fn max_receive_segments(&self) -> usize {
        1
    }
}

#[derive(Debug)]
struct ProbePoller {
    sock: Arc<ProbeSock>,
}

impl UdpPoller for ProbePoller {
    fn poll_writable(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        self.sock.io.poll_send_ready(cx)
    }
}

// ---------- RPK 钉定（真源 homeway-quic/src/exit/rpk.rs::client_pin） ----------
//
// SECURITY: harness-only —— 自定义 verifier 的安装点是 `dangerous()`，本文件位于 tools/
// （产品面由 tools/check-quic-isolation.sh 第 ⑤ 条断言零命中）。钉定 ≠ 跳过验证：
// 先逐字节比对 SPKI，再走 verify_tls13_signature_with_raw_key 真验签。

#[derive(Debug)]
struct PinMismatch;

impl std::fmt::Display for PinMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RPK 与 token 钉定不符")
    }
}
impl std::error::Error for PinMismatch {}

fn spki_of(pubkey: &[u8; 32]) -> [u8; 44] {
    // Ed25519 SPKI：`302a300506032b6570032100`‖pubkey（RFC 8410）
    let mut out = [0u8; 44];
    let prefix = [0x30u8, 0x2a, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x03, 0x21, 0x00];
    out[..12].copy_from_slice(&prefix);
    out[12..].copy_from_slice(pubkey);
    out
}

#[derive(Debug)]
struct Pinning {
    pin: [u8; 44],
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for Pinning {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _i: &[CertificateDer<'_>],
        _n: &ServerName<'_>,
        _o: &[u8],
        _t: UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.pin {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::InvalidCertificate(
                rustls::CertificateError::Other(rustls::OtherError(Arc::new(PinMismatch))),
            ))
        }
    }
    fn verify_tls12_signature(
        &self,
        _m: &[u8],
        _c: &CertificateDer<'_>,
        _d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Err(rustls::Error::General("QUIC 恒 TLS1.3".into()))
    }
    fn verify_tls13_signature(
        &self,
        m: &[u8],
        c: &CertificateDer<'_>,
        d: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature_with_raw_key(
            m,
            &SubjectPublicKeyInfoDer::from(c.as_ref()),
            d,
            &self.provider.signature_verification_algorithms,
        )
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
    fn requires_raw_public_keys(&self) -> bool {
        true
    }
}

fn client_config(pubkey: &[u8; 32]) -> quinn::ClientConfig {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let rcfg = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .expect("TLS1.3 配置")
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinning {
            pin: spki_of(pubkey),
            provider,
        }))
        .with_no_client_auth();
    let qcc = quinn::crypto::rustls::QuicClientConfig::try_from(rcfg).expect("QUIC client config");
    let mut cc = quinn::ClientConfig::new(Arc::new(qcc));
    // 与出口侧同族 TransportConfig（MTU 1400 / 1 MiB datagram 缓冲；ACK 阈值随
    // 出口下发的 ACK_FREQUENCY，客户端不覆盖）
    let mut t = quinn::TransportConfig::default();
    t.initial_mtu(1400);
    t.min_mtu(1320);
    t.datagram_send_buffer_size(1 << 20);
    t.datagram_receive_buffer_size(Some(1 << 20));
    t.max_idle_timeout(Some(Duration::from_secs(30).try_into().unwrap()));
    t.keep_alive_interval(Some(Duration::from_secs(10)));
    cc.transport_config(Arc::new(t));
    cc
}

fn hex32(s: &str) -> Result<[u8; 32], String> {
    let b = hex_decode(s)?;
    b.try_into().map_err(|_| format!("{s} 不是 32B"))
}

fn hex_decode(s: &str) -> Result<Vec<u8>, String> {
    if !s.len().is_multiple_of(2) {
        return Err(format!("{s:?} 非偶数长度 hex"));
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).map_err(|e| format!("hex: {e}")))
        .collect()
}

fn sha256_32(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}

fn main() {
    if let Err(e) = run() {
        eprintln!("probe: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let args = Args::parse()?;
    // ---- 物料：token 或显式参数 ----
    let mut quic_ep = args.quic_ep;
    let mut relay_ep = args.relay_ep;
    let mut rpk: Option<[u8; 32]> = args.rpk_hex.as_deref().map(hex32).transpose()?;
    let mut secret: Option<[u8; 32]> = args.secret_hex.as_deref().map(hex32).transpose()?;
    let mut label: Option<[u8; 8]> = None;
    if let Some(t) = &args.token {
        let info = parse_token(t)?;
        secret = Some(info.secret);
        label = Some(relay_label(&info.peer_id));
        rpk = info.rpk.or(rpk);
        let mut eps: Vec<String> = Vec::new();
        for (typ, addr) in &info.endpoints {
            eps.push(format!("{}={}（{}）", kind_name(*typ), addr, typ));
            // 只有 QUIC 类端点自动取用（token 里唯一能直连的 QUIC 地址）；中继端点
            // **必须显式 `--relay <addr>`**——`via=relay` 会改变整条 socket 的封壳语义，
            // 隐式切换会让「直连读数」被误当直连（探针的读数口径必须显式）。
            if *typ == 2 && quic_ep.is_none() {
                quic_ep = addr.parse().ok();
            }
        }
        println!("token: peer={} label={} 端点[{}]", hex8(&info.peer_id), hex8(&label.unwrap()), eps.join(" "));
        println!(
            "token: rpk={}",
            rpk.map(|k| k.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .unwrap_or_else(|| "<无>".into())
        );
    }
    let secret = secret.ok_or("缺 secret（--token 或 --secret）")?;
    let rpk = rpk.ok_or("缺服务端 RPK（--token 带 rpk 或 --rpk）")?;
    let pubkey = match &args.pubkey_hex {
        Some(h) => hex32(h)?,
        None => sha256_32(b"quic-probe-device"),
    };
    let devtag: [u8; 8] = match &args.devtag_hex {
        Some(h) => hex_decode(h)?.try_into().map_err(|_| "--devtag 不是 8B")?,
        None => [0xd1, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8],
    };
    let via_relay = relay_ep.is_some();
    let target = if via_relay { relay_ep.unwrap() } else { quic_ep.ok_or("缺 QUIC 端点（--token 的 QUIC 类端点 / --quic）")? };
    let tunnel_ip = derive_tunnel_ip(&secret, &pubkey);
    let tun_ip = derive_tun_ip(&secret, &pubkey);
    println!(
        "probe: via={} target={} tunnel_ip={} tun_ip={} devtag={}",
        if via_relay { "relay" } else { "direct" },
        target,
        ip4(&tunnel_ip),
        ip4(&tun_ip),
        hex8(&devtag)
    );

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
        .map_err(|e| format!("runtime: {e}"))?;
    rt.block_on(async move {
        let stats = Arc::new(SockStats::default());
        let sock = std::net::UdpSocket::bind(&args.bind).map_err(|e| format!("bind（{}）: {e}", args.bind))?;
        sock.set_nonblocking(true).map_err(|e| format!("非阻塞: {e}"))?;
        let local = sock.local_addr().map_err(|e| format!("local_addr: {e}"))?;
        let io = tokio::net::UdpSocket::from_std(sock).map_err(|e| format!("from_std: {e}"))?;
        let abs = Arc::new(ProbeSock {
            io,
            label: if via_relay { label } else { None },
            stats: Arc::clone(&stats),
        });
        let mut endpoint = quinn::Endpoint::new_with_abstract_socket(
            quinn::EndpointConfig::default(),
            None,
            abs,
            Arc::new(quinn::TokioRuntime),
        )
        .map_err(|e| format!("endpoint: {e}"))?;
        println!("probe: 本地 socket {local}（{}）", if via_relay { "relay 信封模式" } else { "裸 QUIC 模式" });
        endpoint.set_default_client_config(client_config(&rpk));

        let t0 = Instant::now();
        let conn = endpoint
            .connect(target, SERVER_NAME)
            .map_err(|e| format!("connect 调用: {e}"))?
            .await
            .map_err(|e| format!("握手失败（RPK 钉定/网络）: {e}"))?;
        println!("握手完成：{:.1}ms（{:?}）", t0.elapsed().as_secs_f64() * 1000.0, conn.remote_address());
        let mds = conn.max_datagram_size();
        println!("max_datagram_size={:?} current_mtu={}", mds, conn.stats().path.current_mtu);

        // ---- hr-reg4 四帧准入（M2 §1.2）：Hello → 等 C4 → Proof → 等 A4 ----
        let mut exporter = [0u8; 32];
        conn.export_keying_material(&mut exporter, EXPORTER_LABEL, b"")
            .map_err(|e| format!("exporter: {e:?}"))?;
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).expect("钟").as_secs();
        let (mut send, mut recv) = conn.open_bi().await.map_err(|e| format!("open_bi: {e}"))?;
        let hello = hello_frame(&pubkey, &devtag, ts);
        send.write_all(&hello).await.map_err(|e| format!("写 Hello: {e}"))?;
        println!("准入: Hello 已发 {}B（dev={}）；等 Challenge", hello.len(), hex8(&devtag));
        let mut ch = [0u8; CHALLENGE_LEN];
        recv.read_exact(&mut ch).await.map_err(|e| format!("等 Challenge: {e}"))?;
        if &ch[..2] != b"C4" {
            return Err(format!("Challenge 魔数不符：{:02x?}", &ch[..2]));
        }
        let nonce: [u8; 16] = ch[2..18].try_into().expect("16B");
        let proof = proof_frame(&secret, &pubkey, &devtag, ts, &nonce, &exporter);
        send.write_all(&proof).await.map_err(|e| format!("写 Proof: {e}"))?;
        println!("准入: Challenge 收到（nonce 非零={}），Proof 已发 {}B；等 Accept",
            nonce.iter().any(|b| *b != 0), proof.len());
        let mut acc = [0u8; ACCEPT_LEN];
        recv.read_exact(&mut acc).await.map_err(|e| format!("等 Accept（出口拒？）: {e}"))?;
        if &acc != b"A4" {
            return Err(format!("Accept 魔数不符：{:02x?}", &acc));
        }
        println!("准入: A4 收到（四帧走通；绑定已建立）");
        // `--reg-wait` 保留（脚本面契约不变）：A4 之后的额外静置窗，排障用
        if args.reg_wait > 0 {
            tokio::time::sleep(Duration::from_millis(args.reg_wait)).await;
        }

        // ---- 数据面 ----
        let (dst, dport): ([u8; 4], u16) = match &args.dst {
            Some(v) => {
                let sa: SocketAddr = v.parse().map_err(|e| format!("--dst: {e}"))?;
                match sa.ip() {
                    std::net::IpAddr::V4(ip) => (ip.octets(), sa.port()),
                    std::net::IpAddr::V6(_) => return Err("内层包只支持 IPv4（探针面）".into()),
                }
            }
            None if args.dns => (SERVER_TUNNEL_IP, 53),
            None => ([10, 0, 0, 1], 30000),
        };
        let mk = |size: usize| -> Vec<u8> {
            if args.dns {
                inner_pkt(tun_ip, dst, 40000, 53, &dns_query("probe.homeway.test"))
            } else {
                let payload = vec![0xABu8; size.saturating_sub(28).max(1)];
                inner_pkt(tun_ip, dst, 40000, dport, &payload)
            }
        };
        let pkt0 = mk(args.size);
        if args.cmd == "conn" {
            // 一问一答（DNS 形态）：证明「客户端 → 中继 → 出口 intercept → 回程」全链
            for _ in 0..3 {
                conn.send_datagram(bytes::Bytes::from(pkt0.clone()))
                    .map_err(|e| format!("send_datagram: {e}"))?;
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            match tokio::time::timeout(Duration::from_millis(args.wait_ms), conn.read_datagram()).await {
                Ok(Ok(dg)) => {
                    println!("回程：收到 {}B（首 24B {:02x?}）", dg.len(), &dg[..dg.len().min(24)]);
                    if args.dns && dg.len() > 28 {
                        // 内层 = IPv4(20) + UDP(8) + DNS：事务 ID 在 DNS 头首 2B
                        let dns = &dg[28..];
                        println!(
                            "回程：DNS 事务 ID=0x{:02x}{:02x}（查询为 0x1234）flags=0x{:02x}{:02x}",
                            dns[0], dns[1], dns[2], dns[3]
                        );
                    }
                }
                Ok(Err(e)) => println!("回程：连接错误 {e}"),
                Err(_) => println!("回程：{}ms 内无应答（出口可能未登记/未代答）", args.wait_ms),
            }
        } else {
            // 上行灌包（B1/B2）：回程读侧并发跑（UDP 回显形态——回程包尺寸也要读数）
            let replies = Arc::new(AtomicU64::new(0));
            let reply_bytes = Arc::new(AtomicU64::new(0));
            let reply_max = Arc::new(AtomicU64::new(0));
            let drained = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (r, rb, rm, dr) = (
                Arc::clone(&replies),
                Arc::clone(&reply_bytes),
                Arc::clone(&reply_max),
                Arc::clone(&drained),
            );
            let conn_r = conn.clone();
            let reader = tokio::spawn(async move {
                while let Ok(dg) = conn_r.read_datagram().await {
                    r.fetch_add(1, Ordering::SeqCst);
                    rb.fetch_add(dg.len() as u64, Ordering::SeqCst);
                    rm.fetch_max(dg.len() as u64, Ordering::SeqCst);
                }
                dr.store(true, Ordering::SeqCst);
            });
            let t1 = Instant::now();
            let mut sent = 0usize;
            let mut buf_full = 0usize;
            for i in 0..args.n {
                if args.rate > 0 {
                    // 节拍发送（µs 粒度：gap = 1e6/rate）
                    let due = t1 + Duration::from_micros(1_000_000 * (i as u64 + 1) / u64::from(args.rate));
                    let now = Instant::now();
                    if due > now {
                        tokio::time::sleep(due - now).await;
                    }
                }
                // 预检（M1 §0.3 P4 / §1.5：**裸 `send_datagram` 在缓冲满时静默淘汰最旧**，
                // 恒返 Ok）——探针也不许静默：缓冲不够就丢 + 计数，不假装发出去
                let need = pkt0.len();
                if conn.datagram_send_buffer_space() < need {
                    buf_full += 1;
                    if buf_full == 1 {
                        println!("push: 发送缓冲满 ⇒ 丢 + 计数（裸 send_datagram 在这里会静默淘汰最旧并返 Ok）");
                    }
                    continue;
                }
                match conn.send_datagram(bytes::Bytes::from(pkt0.clone())) {
                    Ok(()) => sent += 1,
                    Err(e) => {
                        println!("push: 第 {i} 个发送失败：{e}");
                        break;
                    }
                }
            }
            let send_elapsed = t1.elapsed().as_secs_f64();
            println!(
                "push: 已发 {sent}/{} 个（{}B/个，用时 {send_elapsed:.2}s，均 {:.0}pps；缓冲满丢 {buf_full}）",
                args.n,
                pkt0.len(),
                sent as f64 / send_elapsed.max(1e-6)
            );
            // 收尾窗（等回程把 ACK/回显排空）
            tokio::time::sleep(Duration::from_millis(1500)).await;
            println!(
                "push: 回程 {} 包 / {}B / 最大 {}B（reader 结束={}）",
                replies.load(Ordering::SeqCst),
                reply_bytes.load(Ordering::SeqCst),
                reply_max.load(Ordering::SeqCst),
                drained.load(Ordering::SeqCst)
            );
            reader.abort();
        }
        if args.hold > 0 {
            tokio::time::sleep(Duration::from_secs(args.hold)).await;
        }
        // ---- 读数（设计 §7.2 B1/B2）----
        let st = conn.stats();
        let p = st.path;
        let s = &stats;
        println!(
            "sock: tx_dgrams={} tx_bytes={} tx_max={}B rx_dgrams={} rx_bytes={} rx_max={}B rx_ignored={}",
            s.tx_dgrams.load(Ordering::SeqCst),
            s.tx_bytes.load(Ordering::SeqCst),
            s.tx_max.load(Ordering::SeqCst),
            s.rx_dgrams.load(Ordering::SeqCst),
            s.rx_bytes.load(Ordering::SeqCst),
            s.rx_max.load(Ordering::SeqCst),
            s.rx_ignored.load(Ordering::SeqCst),
        );
        println!(
            "quinn: udp_tx_dgrams={} udp_tx_bytes={} udp_rx_dgrams={} udp_rx_bytes={} rtt_ms={} cwnd={} lost_pkts={} cong_events={} pmtud_probes={}/{} mtu={}",
            st.udp_tx.datagrams,
            st.udp_tx.bytes,
            st.udp_rx.datagrams,
            st.udp_rx.bytes,
            p.rtt.as_millis(),
            p.cwnd,
            p.lost_packets,
            p.congestion_events,
            p.sent_plpmtud_probes,
            p.lost_plpmtud_probes,
            p.current_mtu,
        );
        println!("conn: close_reason={:?} mds={:?}", conn.close_reason(), conn.max_datagram_size());
        conn.close(0u32.into(), b"probe done");
        endpoint.wait_idle().await;
        Ok::<(), String>(())
    })?;
    Ok(())
}

fn hex8(b: &[u8]) -> String {
    b.iter().take(8).map(|x| format!("{x:02x}")).collect()
}

fn ip4(b: &[u8; 4]) -> String {
    format!("{}.{}.{}.{}", b[0], b[1], b[2], b[3])
}
