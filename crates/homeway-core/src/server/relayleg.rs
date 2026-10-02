//! 出口侧的中继注册腿（UDP）+ 控制面客户端（TCP）——R4-4c。
//!
//! 语义真源 `baseline:internal/server/{relayclient,relayctl}.go`。三条硬约束（design D4）：
//! 1. **注册腿与数据面同一本地端口**：全部出站经驱动线程 ServerBind 的同一 WG socket
//!    （`try_clone` 的 fd 副本——NAT 映射一致，打洞才打得开）；
//! 2. 注册必须证明持有 peerId 私钥（X25519 挑战响应）；
//! 3. hint 是不可信线索：只用来**盲打**（开自己的 NAT 过滤），路径是否真通由 WG 握手决定。
//!
//! 失败语义：控制面全链路「尽力而为」——连不上/断了就退避重连；一切错误只记日志，
//! 绝不影响出口主服务。收工上界：stop 打断 tick/dial/sleep（≤ 一轮 tick + dial 5s）。

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::{Duration, Instant};

use x25519_dalek::{PublicKey, StaticSecret};

use crate::relaywire as rw;
use crate::wtransport::frame::{self, relay_id};

use super::engine::EngineCmd;
use crate::Logf;

/// 注册腿/控制面错误（typed——AGENTS「错误一律 thiserror」；日志侧 Display）。
#[derive(Debug, thiserror::Error)]
pub enum LegError {
    #[error("IO: {0}")]
    Io(#[from] std::io::Error),
    #[error("腿数已达上限 {0}")]
    LegCap(usize),
}

// ---------- 常量（relayclient.go:26-31 + relayctl.go:28-32） ----------

/// 注册腿保活（中继侧 90s 过期 ⇒ 3 次容错）。
const KEEPALIVE_EVERY: Duration = Duration::from_secs(25);
/// 注册没成功时的重试节拍。
const RETRY_EVERY: Duration = Duration::from_secs(5);
/// 30s 未确认注册的一次性告警。
const WARN_AFTER: Duration = Duration::from_secs(30);
/// 每次 hint 的盲打包数（上限，不做放大器）。
const PUNCH_BURST: usize = 3;
const PUNCH_GAP: Duration = Duration::from_millis(150);
/// 同一地址的盲打节流。
const PUNCH_MIN_INTERVAL: Duration = Duration::from_secs(3);
/// lastPunch 表容量（防无界增长——清了重来，都是消耗品）。
const PUNCH_TABLE_MAX: usize = 64;

const CTL_DIAL_TIMEOUT: Duration = Duration::from_secs(5);
const CTL_RECONNECT_MIN: Duration = Duration::from_secs(1);
const CTL_RECONNECT_MAX: Duration = Duration::from_secs(30);
/// 控制面读超时（3×保活 + 15s——与中继侧配套）。
const CTL_READ_TIMEOUT: Duration = Duration::from_secs(90);

// ---------- 解析 --relay 取值 ----------

/// `--relay` 取值解析产物：中继端点 + 鉴权密钥（None = 开放模式）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelayArg {
    pub addr: SocketAddr,
    pub secret: Option<[u8; 32]>,
}

/// `ParseRelayArg`（serve.go:753-779 同义）：rl1 token（地址 + 鉴权密钥）或裸
/// host:port（开放模式）。Direct 端点优先、无 Direct 用全端点；端点非 IP:port 报错。
pub fn parse_relay_arg(v: &str) -> Result<RelayArg, String> {
    // （装配期一次性解析，错误文案直接面向 CLI 用户——保留 String 形态；运行期错误走 LegError）
    let v = v.trim();
    if v.starts_with(crate::relay::rltoken::PREFIX) {
        let tok = crate::relay::rltoken::decode_relay_token(v)
            .map_err(|e| format!("中继 token 解析失败: {e}"))?;
        let mut eps: Vec<&str> = tok.endpoints.iter().map(|e| e.addr.as_str()).collect();
        if eps.is_empty() {
            return Err("中继 token 里没有端点".to_owned());
        }
        // Direct 端点优先
        let direct: Vec<&str> = tok
            .endpoints
            .iter()
            .filter(|e| e.kind == crate::token::EndpointKind::Direct)
            .map(|e| e.addr.as_str())
            .collect();
        if !direct.is_empty() {
            eps = direct;
        }
        let addr: SocketAddr = eps[0]
            .parse()
            .map_err(|_| format!("中继 token 端点 {:?} 不是 IP:port（先用带 IP 的 token）", eps[0]))?;
        return Ok(RelayArg { addr, secret: Some(*tok.secret.as_bytes()) });
    }
    let addr: SocketAddr = v
        .parse()
        .map_err(|_| format!("{v:?} 既不是 host:port 也不是 rl1 token"))?;
    Ok(RelayArg { addr, secret: None })
}

// ---------- UDP 注册腿（relayclient.go） ----------

/// 驱动线程 → relay-leg 线程的事件（bind 钩子投递）。
pub enum LegEvent {
    /// 中继控制帧（type=3 载荷 + 源）。
    Frame(Vec<u8>, SocketAddr),
    /// hint 地址线索（地址串 + 源）。
    Hint(String, SocketAddr),
}

/// 起 UDP 注册腿线程（非阻塞；随 stop 收工）。
pub fn spawn_relay_leg(
    sock: UdpSocket,
    relay: SocketAddr,
    priv_key: StaticSecret,
    secret: Option<[u8; 32]>,
    events: std::sync::mpsc::Receiver<LegEvent>,
    stop: Arc<AtomicBool>,
    logf: Logf,
) {
    std::thread::Builder::new()
        .name("homeway-relayleg".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            run_relay_leg(sock, relay, priv_key, secret, events, &stop, &logf);
        })
        .ok();
}

fn run_relay_leg(
    sock: UdpSocket,
    relay: SocketAddr,
    priv_key: StaticSecret,
    secret: Option<[u8; 32]>,
    events: std::sync::mpsc::Receiver<LegEvent>,
    stop: &AtomicBool,
    logf: &Logf,
) {
    let pub_key = PublicKey::from(&priv_key);
    let label = relay_id(pub_key.as_bytes());
    let mut verified = false;
    let mut last_keepalive = Instant::now();
    let started = Instant::now();
    let mut warned = false;

    // punch worker：单独一条线程 + 有界队列（3×150ms 节奏 sleep 不进本线程——
    // Challenge 处理不被拖；也不 per-hint 起线程——防线程风暴）
    let (punch_tx, punch_rx) = std::sync::mpsc::sync_channel::<SocketAddr>(16);
    let punch_sock = sock.try_clone().expect("clone WG socket");
    let punch_stop = Arc::new(AtomicBool::new(false));
    let punch_stop2 = Arc::clone(&punch_stop);
    let punch_logf: Logf = Arc::clone(logf);
    std::thread::Builder::new()
        .name("homeway-punch".into())
        .stack_size(256 * 1024)
        .spawn(move || run_punch_worker(punch_sock, punch_rx, &punch_stop2, &punch_logf))
        .ok();

    send_hello(&sock, relay, label, pub_key.as_bytes(), logf);
    let mode = if secret.is_some() { "token 模式（rl1 凭据）" } else { "开放模式（无 token）" };
    (logf)(&format!("中继：注册腿开跑（中继 {relay}，{mode}）"));

    // 收工贯通（评审 中-2）：**任何** return 路径都要停 punch worker——
    // 通道断开（驱动线程先退）也走这里，防 worker 100% 空转 + socket 副本泄漏
    let shutdown = |punch_stop: &AtomicBool| {
        punch_stop.store(true, Ordering::SeqCst);
    };
    loop {
        if stop.load(Ordering::SeqCst) {
            shutdown(&punch_stop);
            return;
        }
        // 5s 节拍（事件驱动的睡法：事件随时唤醒本线程）
        match events.recv_timeout(RETRY_EVERY) {
            Ok(LegEvent::Frame(payload, src)) => {
                handle_control(&sock, relay, &priv_key, &pub_key, secret, label, &payload, src, &mut verified, logf);
            }
            Ok(LegEvent::Hint(addr, src)) => {
                // hint 源校验（#23）：中继的 per-client 分配 socket（端口动态、IP 恒为
                // 中继地址）——只比 IP；未知源的 hint 只记日志，绝不盲打
                if src.ip() != relay.ip() {
                    (logf)(&format!("中继：忽略来自未知源 {src} 的地址线索（应为中继 {relay}）"));
                    continue;
                }
                if let Ok(ap) = addr.parse::<SocketAddr>() {
                    let _ = punch_tx.try_send(ap); // 队列满丢弃（hint 是消耗品）
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                shutdown(&punch_stop);
                return;
            }
        }
        if !verified {
            send_hello(&sock, relay, label, pub_key.as_bytes(), logf);
            // 地址写错/中继没起时给一条明确告警，别让运维对着「没反应」猜
            if !warned && started.elapsed() > WARN_AFTER {
                warned = true;
                (logf)(&format!(
                    "⚠️ 中继 {relay} 30s 未确认注册：检查地址是否正确、中继是否在跑、UDP 是否通（本机 → 中继）"
                ));
            }
            continue;
        }
        if last_keepalive.elapsed() >= KEEPALIVE_EVERY {
            send_keepalive(&sock, relay, label);
            last_keepalive = Instant::now();
        }
    }
}

fn send_hello(sock: &UdpSocket, relay: SocketAddr, label: [u8; 8], pub_: &[u8; 32], logf: &Logf) {
    // 发往中继 listener 的包必须带 [0xAA][label] 路由标签（中继靠它认腿）
    let hello = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pub_));
    let mut tagged = Vec::with_capacity(9 + hello.len());
    frame::encode_tagged_frame(&label, &hello, &mut tagged);
    if let Err(e) = sock.send_to(&tagged, relay) {
        (logf)(&format!("中继：注册 Hello 发送失败（{e}）"));
    }
}

fn send_keepalive(sock: &UdpSocket, relay: SocketAddr, label: [u8; 8]) {
    let ka = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::keepalive_bytes());
    let mut tagged = Vec::with_capacity(9 + ka.len());
    frame::encode_tagged_frame(&label, &ka, &mut tagged);
    let _ = sock.send_to(&tagged, relay);
}

/// 中继回的控制消息（源**全等**判据：IP+端口；其余来源冒充中继控制帧一律忽略）。
/// payload = 子协议体（子类型字节 + 体——bind 钩子已剥 `[0xBB][3]` 帧头）。
#[allow(clippy::too_many_arguments)]
fn handle_control(
    sock: &UdpSocket,
    relay: SocketAddr,
    priv_key: &StaticSecret,
    pub_key: &PublicKey,
    secret: Option<[u8; 32]>,
    label: [u8; 8],
    payload: &[u8],
    src: SocketAddr,
    verified: &mut bool,
    logf: &Logf,
) {
    if src != relay {
        return;
    }
    let sub = payload.first().copied().unwrap_or(0);
    if sub == rw::sub::CHALLENGE {
        let whole = payload; // decode_* 以「子类型 + 体」整条为对象
        let Some((eph_pub, nonce)) = rw::decode_challenge(whole) else { return };
        // Proof 的 DH = 本端静态私钥 × 中继临时公钥（与中继侧校验式配对）
        let dh = priv_key.diffie_hellman(&PublicKey::from(eph_pub));
        let psk = secret.map(|s| rw::auth_mac(&s, &nonce, pub_key.as_bytes()));
        let proof = rw::encode_proof(&nonce, dh.as_bytes(), pub_key.as_bytes(), psk.as_ref());
        let pf = frame::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &proof);
        let mut tagged = Vec::with_capacity(9 + pf.len());
        frame::encode_tagged_frame(&label, &pf, &mut tagged);
        if let Err(e) = sock.send_to(&tagged, relay) {
            (logf)(&format!("中继：注册证明发送失败（{e}）"));
        }
        return;
    }
    if sub == rw::sub::AGAIN {
        // 中继说「腿不在了」（它重启过/腿过期）：清掉本地状态，下一拍重新走注册
        let was = *verified;
        *verified = false;
        if was {
            (logf)("中继：对方要求重新注册（中继重启过或注册腿过期）—— 重新走挑战响应");
        }
        send_hello(sock, relay, label, pub_key.as_bytes(), logf);
        return;
    }
    if sub == rw::sub::OK {
        let first = !*verified;
        *verified = true;
        if first {
            (logf)(&format!(
                "中继：注册成功（腿 {} → {relay}）—— 客户端可经它到达本机",
                sock.local_addr().map(|a| a.port()).unwrap_or(0)
            ));
        }
    }
    // 未知 subtype：忽略（前向兼容）
}

/// 盲打 worker：对 hint 地址打几包（开自己 NAT 过滤）；每地址 3s 节流；小载荷腿帧。
fn run_punch_worker(
    sock: UdpSocket,
    rx: std::sync::mpsc::Receiver<SocketAddr>,
    stop: &AtomicBool,
    logf: &Logf,
) {
    // 节流键 = IP（Go 同义；端口动态——按地址键可被换端口绕过）
    let mut last_punch: std::collections::HashMap<std::net::IpAddr, Instant> = std::collections::HashMap::new();
    while !stop.load(Ordering::SeqCst) {
        let client = match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(c) => c,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(t) = last_punch.get(&client.ip()) {
            if t.elapsed() < PUNCH_MIN_INTERVAL {
                continue;
            }
        }
        if last_punch.len() > PUNCH_TABLE_MAX {
            last_punch.clear(); // 防无界增长：清了重来（都是消耗品）
        }
        last_punch.insert(client.ip(), Instant::now());
        // 判据行（每地址 3s 节流，量可控——摘要级）
        (logf)(&format!(
            "中继：收到对端地址线索 {client} → 盲打 {PUNCH_BURST} 包（开自己 NAT 过滤；能否直连仍由 WG 握手决定）"
        ));
        for _ in 0..PUNCH_BURST {
            // 小载荷腿帧：对端解析不出数据会静默丢弃，但 NAT 过滤已被打开
            let f = frame::frame_bytes(frame::FrameKind::Data, &[0, 0, 0, 0]);
            if sock.send_to(&f, client).is_err() {
                break;
            }
            std::thread::sleep(PUNCH_GAP);
        }
    }
}

// ---------- TCP 控制面客户端（relayctl.go） ----------

/// 起控制面线程（非阻塞；随 stop 收工；SESSION/RELEASE 经 EngineCmd 交驱动线程）。
pub fn spawn_relay_ctl(
    relay: SocketAddr,
    priv_key: StaticSecret,
    secret: Option<[u8; 32]>,
    cmd_tx: Sender<EngineCmd>,
    stop: Arc<AtomicBool>,
    logf: Logf,
) {
    std::thread::Builder::new()
        .name("homeway-relayctl".into())
        .stack_size(512 * 1024)
        .spawn(move || {
            run_control_loop(relay, priv_key, secret, cmd_tx, &stop, &logf);
        })
        .ok();
}

fn run_control_loop(
    relay: SocketAddr,
    priv_key: StaticSecret,
    secret: Option<[u8; 32]>,
    cmd_tx: Sender<EngineCmd>,
    stop: &AtomicBool,
    logf: &Logf,
) {
    let mut backoff = CTL_RECONNECT_MIN;
    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        let (handshake_ok, err) = run_control_conn(relay, &priv_key, secret, &cmd_tx, stop, logf);
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if handshake_ok {
            // 曾健康运行的断线：退避从最小值重新起（只增不减会让长期运行后的
            // 每次断线恢复都等满 30s 上限）
            backoff = CTL_RECONNECT_MIN;
        }
        match err {
            Some(e) => (logf)(&format!("中继控制面断开（{e}）—— {backoff:?} 后重连")),
            None => (logf)(&format!("中继控制面退出 —— {backoff:?} 后重连")),
        }
        // stop 可打断的退避睡
        let deadline = Instant::now() + backoff;
        while Instant::now() < deadline {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200).min(deadline - Instant::now()));
        }
        backoff = (backoff * 2).min(CTL_RECONNECT_MAX);
    }
}

/// 一次完整连接生命周期（返回 = 连接结束）。
/// handshake_ok：本次是否完成握手进入读循环（true = 曾健康运行）。
fn run_control_conn(
    relay: SocketAddr,
    priv_key: &StaticSecret,
    secret: Option<[u8; 32]>,
    cmd_tx: &Sender<EngineCmd>,
    stop: &AtomicBool,
    logf: &Logf,
) -> (bool, Option<String>) {
    // 拨号（5s 超时；stop 打断）
    let dial_started = Instant::now();
    let mut conn = loop {
        if stop.load(Ordering::SeqCst) {
            return (false, None);
        }
        match TcpStream::connect_timeout(&relay, CTL_DIAL_TIMEOUT.min(Duration::from_secs(1))) {
            Ok(c) => break c,
            Err(e) => {
                if Instant::now() - dial_started > CTL_DIAL_TIMEOUT {
                    return (false, Some(format!("拨控制通道: {e}")));
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        }
    };
    let _ = conn.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = conn.set_write_timeout(Some(CTL_DIAL_TIMEOUT));

    let pub_key = PublicKey::from(priv_key);
    // ① HELLO
    if let Err(e) = write_ctl(&mut conn, &rw::encode_hello(pub_key.as_bytes())) {
        return (false, Some(format!("发 HELLO: {e}")));
    }
    // ② CHALLENGE（5s 硬期限；静默拍轮 stop）
    let mut dec = rw::CtlDecoder::new();
    let mut pending: Vec<(u8, Vec<u8>)> = Vec::new();
    let deadline = Instant::now() + CTL_DIAL_TIMEOUT;
    let msg = loop {
        if stop.load(Ordering::SeqCst) {
            return (false, None);
        }
        if Instant::now() > deadline {
            return (false, Some("读 CHALLENGE: 超时".to_owned()));
        }
        match read_ctl_once(&mut dec, &mut conn, &mut pending) {
            Ok(ReadOutcome::Msg(m)) => break m,
            Ok(ReadOutcome::Quiet) => continue,
            Err(e) => return (false, Some(format!("读 CHALLENGE: {e}"))),
        }
    };
    if msg.first() != Some(&rw::sub::CHALLENGE) {
        return (false, Some("控制面握手不是 CHALLENGE".to_owned()));
    }
    let Some((eph_pub, nonce)) = rw::decode_challenge(&msg) else {
        return (false, Some("解析 CHALLENGE 失败".to_owned()));
    };
    // ③ PROOF（v2 自报版本；token 模式带 PSK）
    let dh = priv_key.diffie_hellman(&PublicKey::from(eph_pub));
    let mac_psk = secret.map(|s| rw::auth_mac(&s, &nonce, pub_key.as_bytes()));
    let proof = rw::encode_proof(&nonce, dh.as_bytes(), pub_key.as_bytes(), mac_psk.as_ref());
    if let Err(e) = write_ctl(&mut conn, &proof) {
        return (false, Some(format!("发 PROOF: {e}")));
    }
    // ④ OK（恒 17B；token 模式必须验中继身份 MAC——#29）
    let deadline = Instant::now() + CTL_DIAL_TIMEOUT;
    let msg = loop {
        if stop.load(Ordering::SeqCst) {
            return (false, None);
        }
        if Instant::now() > deadline {
            return (false, Some("读 OK: 超时".to_owned()));
        }
        match read_ctl_once(&mut dec, &mut conn, &mut pending) {
            Ok(ReadOutcome::Msg(m)) => break m,
            Ok(ReadOutcome::Quiet) => continue,
            Err(e) => return (false, Some(format!("读 OK: {e}"))),
        }
    };
    if msg.first() != Some(&rw::sub::OK) {
        return (false, Some(format!("控制面握手被拒（type=0x{:02x}）", msg.first().copied().unwrap_or(0))));
    }
    let Some(mac) = rw::decode_ok_auth(&msg) else {
        return (false, Some("控制面 OK 形状不符（对端不是 v2 中继）".to_owned()));
    };
    // relayAuthed := secret == 零——开放模式即视为已认证（测试用途放行）；
    // token 模式必须 OK-MAC 过（能应答 TCP 的一方若算不出 MAC 就绕不过这层）。
    //（R4-§7.8 整改：认证状态用 enum 承载——Open / TokenVerified，bool 不再可半程混用；
    // token 模式 MAC 不过 = 握手路径直接断，未认证态不落地。）
    #[derive(Clone, Copy, PartialEq)]
    enum Auth {
        /// 开放模式（relay 无密钥）：连接建立即等价已认证。
        Open,
        /// token 模式：OK-MAC 已通过。
        TokenVerified,
    }
    let auth = match &secret {
        None => Auth::Open,
        Some(key) => {
            let want = rw::ok_auth_mac(key, &nonce);
            if !rw::ct_eq_16(&want, mac) {
                return (false, Some("控制面 OK 的中继身份校验不过（MAC 不匹配：对端不持有本 token 的密钥）".to_owned()));
            }
            (logf)("中继控制面：中继身份已认证（OK-MAC 通过）");
            Auth::TokenVerified
        }
    };
    // 重连对账（B1）：旧腿全部作废——中继会立刻重放活跃会话，按重放重建
    let _ = cmd_tx.send(EngineCmd::LegsClear);
    (logf)(&format!("中继控制面已连（{relay}）—— 已清腿表，等待会话重放"));

    // 读循环（每 1s 静默拍发保活/轮 stop/90s 判死——保活在**外层**，不藏在阻塞读里）
    let mut last_keepalive = Instant::now();
    let mut last_msg = Instant::now();
    let mut refused_sessions = 0u32;
    loop {
        if stop.load(Ordering::SeqCst) {
            return (true, None);
        }
        if last_keepalive.elapsed() >= KEEPALIVE_EVERY {
            if write_ctl(&mut conn, &rw::keepalive_bytes()).is_err() {
                return (true, Some("控制面写: 保活失败".to_owned()));
            }
            last_keepalive = Instant::now();
        }
        match read_ctl_once(&mut dec, &mut conn, &mut pending) {
            Ok(ReadOutcome::Quiet) => {
                if last_msg.elapsed() > CTL_READ_TIMEOUT {
                    return (true, Some("控制面读: 超时（90s 无消息）".to_owned()));
                }
                continue;
            }
            Err(e) => return (true, Some(format!("控制面读: {e}"))),
            Ok(ReadOutcome::Msg(msg)) => {
                last_msg = Instant::now();
                let sub = msg.first().copied().unwrap_or(0);
                if sub == rw::sub::SESSION {
                    if secret.is_some() && auth != Auth::TokenVerified {
                        // 未认证通道不得指挥拨腿（结构性守卫：token 模式 MAC 不过
                        // 在握手路径已断——走到这里的只可能是未来改动引入的降级路径）
                        refused_sessions += 1;
                        if refused_sessions <= 3 || refused_sessions.is_multiple_of(50) {
                            (logf)(&format!(
                                "中继控制面：拒绝未认证通道下发的 SESSION（疑似伪造/降级；累计 {refused_sessions} 次）"
                            ));
                        }
                        continue;
                    }
                    match rw::decode_session(&msg) {
                        Some(sess) => {
                            // DataPort 校验（#29）：中继数据口是临时端口（≥32768），
                            // 0 与 <1024 保留段一定不是它
                            if sess.data_port == 0 || sess.data_port < 1024 {
                                (logf)(&format!(
                                    "中继控制面：会话 #{} 的数据口非法（port={}，保留段）—— 忽略",
                                    sess.id, sess.data_port
                                ));
                                continue;
                            }
                            let remote = SocketAddr::new(relay.ip(), sess.data_port);
                            let marker = rw::legup_payload(
                                sess.id,
                                &sess.cookie,
                                &legup_key_for(secret, &sess.cookie),
                            );
                            let _ = cmd_tx.send(EngineCmd::LegRegister { id: sess.id, remote, marker });
                            (logf)(&format!("中继控制面：会话 #{} 已拨腿 → {remote}（认证腿）", sess.id));
                        }
                        None => {
                            (logf)("中继控制面：SESSION 通告畸形 —— 忽略");
                        }
                    }
                } else if sub == rw::sub::RELEASE {
                    if let Some(id) = rw::decode_release(&msg) {
                        let _ = cmd_tx.send(EngineCmd::LegRemove { id });
                        (logf)(&format!("中继控制面：会话 #{id} 已拆腿（RELEASE）"));
                    }
                } else if sub == rw::sub::KEEPALIVE {
                    // 中继方向的保活：无需处理（TCP 本身就是活性的证明）
                }
                // 未知子类型：前向兼容忽略
            }
        }
    }
}

/// 读侧产物：一条完整消息 / 本拍静默（1s 无数据——外层借机发保活与轮 stop）。
enum ReadOutcome {
    Msg(Vec<u8>),
    Quiet,
}

/// 块读 + 半帧状态机（`rw::CtlDecoder`）：`read()` 不越权填缓冲（部分帧安全），
/// socket 级 1s 读超时把控制权还给调用方——**保活发送在外层循环**，绝不藏在
/// 阻塞读里（实测教训：read_exact(None 预算) 在静默期不返回 ⇒ 保活永发不出 ⇒
/// 中继 90s 判死循环重连）。
fn read_ctl_once(dec: &mut rw::CtlDecoder, conn: &mut TcpStream, out: &mut Vec<(u8, Vec<u8>)>) -> Result<ReadOutcome, String> {
    let mut buf = [0u8; 1024];
    // 涓流自检（评审 低-13）：有字节就不停地喂——对端每 <1s 滴字节可把本函数
    // 钉死在外层之外；上界后按静默拍返回（外层仍会再进来——活性/期限由调用方管）
    let entry = Instant::now();
    loop {
        if entry.elapsed() > Duration::from_secs(2) {
            return Ok(ReadOutcome::Quiet);
        }
        if !out.is_empty() {
            // （feed 已把全部完整消息按序入列；取首条，其余留给后续调用）
            let m = out.remove(0);
            let mut whole = vec![m.0];
            whole.extend_from_slice(&m.1);
            return Ok(ReadOutcome::Msg(whole));
        }
        match conn.read(&mut buf) {
            Ok(0) => return Err("EOF".to_owned()),
            Ok(n) => {
                if dec.feed(&buf[..n], out).is_err() {
                    return Err("长度行非法".to_owned());
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                return Ok(ReadOutcome::Quiet);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
}

fn write_ctl(conn: &mut TcpStream, msg: &[u8]) -> Result<(), String> {
    let mut wire = Vec::with_capacity(2 + msg.len());
    rw::ctl_frame_into(msg, &mut wire);
    conn.write_all(&wire).map_err(|e| e.to_string())
}

/// 腿认证 MAC 的密钥（token 模式 = 中继密钥；开放模式 = cookie 低半字）。
fn legup_key_for(secret: Option<[u8; 32]>, cookie: &[u8; 16]) -> [u8; 32] {
    match secret {
        Some(s) => s,
        None => {
            let mut k = [0u8; 32];
            k[..16].copy_from_slice(cookie);
            k
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_relay_arg_forms() {
        // 裸地址 = 开放模式
        let r = parse_relay_arg("127.0.0.1:42741").unwrap();
        assert_eq!(r.addr, "127.0.0.1:42741".parse().unwrap());
        assert_eq!(r.secret, None);
        // rl1 token = token 模式（端点 + 密钥）
        let secret = [7u8; 32];
        let tok = crate::relay::rltoken::encode_relay_token(
            &secret,
            &[crate::token::Endpoint { addr: "127.0.0.1:42742".into(), kind: crate::token::EndpointKind::Direct }],
        )
        .unwrap();
        let r = parse_relay_arg(&tok).unwrap();
        assert_eq!(r.addr, "127.0.0.1:42742".parse().unwrap());
        assert_eq!(r.secret, Some(secret));
        // 环境错误形态
        assert!(parse_relay_arg("nonsense").is_err());
        assert!(parse_relay_arg("rl1AAAA").is_err());
    }
}
