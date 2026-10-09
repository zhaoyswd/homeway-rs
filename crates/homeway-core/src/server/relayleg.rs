//! 出口侧的中继注册腿（UDP）+ 控制面客户端（TCP）——R4-4c。
//!
//! 语义真源 `baseline:internal/server/{relayclient,relayctl}.go`。三条硬约束（design D4）：
//! 1. **注册腿与数据面同一本地端口**：全部出站经驱动线程 ServerBind 的同一 WG socket
//!    （`try_clone` 的 fd 副本——NAT 映射一致，打洞才打得开）；
//! 2. 注册必须证明持有 peerId 私钥（X25519 挑战响应）；
//! 3. **M5 S4**：hint 盲打面（`LegEvent::Hint`/`parse_hint_addr`/`run_punch_worker`）随
//!    WG 面退役整体删除——hint 的消费者是 WG 打洞（`bind.set_on_hint` 删于 S3b ⇒ 本面
//!    自那时起**零构造点** = 不可达代码）；单承载下「中继 → 直连升级」由岛的迁移/重赛跑
//!    承接（`G8` 登记）。
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
use crate::legframe::{self, relay_id};

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
///
/// **v4-mapped 归一（M2 代码门 G3，同类 D1）**：解析结果经 `unmap_v4_in6` 归一为
/// 纯 v4/v6。`relay` 在本模块是**三处比较/键的基准**——①hint 源校验（`src.ip() !=
/// relay.ip()`，Go `relayclient.go:78` 两侧 `.Unmap()`）②控制帧源全等判据（`src !=
/// relay`）③中继数据腿远端构造（`SocketAddr::new(relay.ip(), sess.data_port)`，Go
/// `relayctl.go:206` 显式 `relay.Addr().Unmap()`）。不归一时 `[::ffff:a.b.c.d]` 形态
/// 的 `--relay`/token 端点与 socket 面（bind 读侧已 unmap）的纯 v4 源**永不相等** ⇒
/// hint 恒被忽略、控制帧恒被忽略（注册腿静默失效）；出口 `relay_ep` 的地址串去重
/// （`engine.rs` 的 `seen: HashSet<String>`）也会与直连端点形态错开。归一在**解析
/// 一处收口**，下游三处比较/键随之全部成立。
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
        return Ok(RelayArg { addr: crate::udpbatch::unmap_v4_in6(addr), secret: Some(*tok.secret.as_bytes()) });
    }
    let addr: SocketAddr = v
        .parse()
        .map_err(|_| format!("{v:?} 既不是 host:port 也不是 rl1 token"))?;
    Ok(RelayArg { addr: crate::udpbatch::unmap_v4_in6(addr), secret: None })
}

// ---------- UDP 注册腿（relayclient.go） ----------

/// 驱动线程 → relay-leg 线程的事件（bind 钩子投递）。M5 S4 起只剩控制帧一档
/// （hint 面已删——见模块头第 3 条）。
pub enum LegEvent {
    /// 中继控制帧（type=3 载荷 + 源）。
    Frame(Vec<u8>, SocketAddr),
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

    send_hello(&sock, relay, label, pub_key.as_bytes(), logf);
    let mode = if secret.is_some() { "token 模式（rl1 凭据）" } else { "开放模式（无 token）" };
    (logf)(&format!("中继：注册腿开跑（中继 {relay}，{mode}）"));

    loop {
        if stop.load(Ordering::SeqCst) {
            return;
        }
        // 5s 节拍（事件驱动的睡法：事件随时唤醒本线程）
        match events.recv_timeout(RETRY_EVERY) {
            Ok(LegEvent::Frame(payload, src)) => {
                handle_control(&sock, relay, &priv_key, &pub_key, secret, label, &payload, src, &mut verified, logf);
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
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

/// 经腿 socket 发往端点（族适配：sock 是主 WG socket 的克隆——双栈形态发 v4 目标
/// 须 map v4-mapped，否则 EINVAL）。控制面低频路径，getsockopt 每次查无妨。
fn send_to_ep(sock: &UdpSocket, payload: &[u8], ep: SocketAddr) -> std::io::Result<usize> {
    let dual = crate::udpbatch::is_dual_stack(sock);
    sock.send_to(payload, crate::udpbatch::xmit_addr(ep, dual))
}

fn send_hello(sock: &UdpSocket, relay: SocketAddr, label: [u8; 8], pub_: &[u8; 32], logf: &Logf) {
    // 发往中继 listener 的包必须带 [0xAA][label] 路由标签（中继靠它认腿）
    let hello = legframe::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::encode_hello(pub_));
    let mut tagged = Vec::with_capacity(9 + hello.len());
    legframe::encode_tagged_frame(&label, &hello, &mut tagged);
    if let Err(e) = send_to_ep(sock, &tagged, relay) {
        (logf)(&format!("中继：注册 Hello 发送失败（{e}）"));
    }
}

fn send_keepalive(sock: &UdpSocket, relay: SocketAddr, label: [u8; 8]) {
    let ka = legframe::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &rw::keepalive_bytes());
    let mut tagged = Vec::with_capacity(9 + ka.len());
    legframe::encode_tagged_frame(&label, &ka, &mut tagged);
    let _ = send_to_ep(sock, &tagged, relay);
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
        let pf = legframe::frame_bytes(rw::FRAME_TYPE_RELAY_REG, &proof);
        let mut tagged = Vec::with_capacity(9 + pf.len());
        legframe::encode_tagged_frame(&label, &pf, &mut tagged);
        if let Err(e) = send_to_ep(sock, &tagged, relay) {
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

    /// M2 代码门 G3（D1 同类）：`--relay` 的 v4-mapped 形态归一为纯 v4。
    /// 下游三处（hint 源校验 / 控制帧源全等 / 数据腿远端构造）都以本解析为基准——
    /// socket 面源地址恒已 unmap 成纯 v4，不归一时 mapped 形态的 relay 永不匹配。
    #[test]
    fn parse_relay_arg_normalizes_v4_mapped() {
        // 裸地址：mapped 与纯 v4 解析为同一形态
        let mapped = parse_relay_arg("[::ffff:127.0.0.1]:42741").unwrap();
        let pure = parse_relay_arg("127.0.0.1:42741").unwrap();
        assert_eq!(mapped.addr, pure.addr);
        assert_eq!(mapped.addr, "127.0.0.1:42741".parse().unwrap());
        // 真 v6 / 公网 v4 原样
        assert_eq!(
            parse_relay_arg("[2001:db8::1]:42741").unwrap().addr,
            "[2001:db8::1]:42741".parse().unwrap()
        );
        assert_eq!(parse_relay_arg("198.51.100.7:42741").unwrap().addr, "198.51.100.7:42741".parse().unwrap());
        // rl1 token 端点同口径（Direct 优先分支）
        let secret = [9u8; 32];
        let tok = crate::relay::rltoken::encode_relay_token(
            &secret,
            &[crate::token::Endpoint {
                addr: "[::ffff:127.0.0.1]:42742".into(),
                kind: crate::token::EndpointKind::Direct,
            }],
        )
        .unwrap();
        let r = parse_relay_arg(&tok).unwrap();
        assert_eq!(r.addr, "127.0.0.1:42742".parse().unwrap());
        assert_eq!(r.secret, Some(secret));
    }

}
