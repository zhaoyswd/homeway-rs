//! relay 控制面（relay-backend-dial）的 TCP 半边：握手线程 + 已建立连接的承载结构。
//!
//! 语义真源 `baseline:internal/relay/control.go`。线程模型（R4-design §4.1）：
//! - **握手线程**（短命，并发 ≤16，RAII 槽位）：accept 后由驱动线程 spawn，阻塞跑
//!   HELLO → CHALLENGE → PROOF → OK（10s 硬期限），成功把 established fd 交还驱动线程
//!   （attach + replay 在驱动线程做）；失败即弃。
//! - **已建立连接**：fd 非阻塞 + 增量半帧状态机（`CtlDecoder`）在驱动线程 poll；
//!   90s 读超时 = `last_read` 时间戳 + reap 轮惰性判死。
//!
//! 鉴权与 UDP 注册腿同一套 X25519 挑战，但 **DH MAC 恒校（两种模式都校）**，token
//! 模式再叠加 PSK 校验（control.go:130-146——控制面必须证明持有 peerId 私钥，否则
//! 持 rl1 token 者可冒用他人 label 接走 SESSION，#29 会话劫持面）。

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use x25519_dalek::PublicKey;

use crate::relaywire as rw;
use crate::wtransport::frame::relay_id;

use super::Logf;

/// 连接死亡（EOF/RESET/长度行非法）——typed error（AGENTS 工程原则）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("控制面连接死亡")]
pub struct ConnDead;

/// 握手必须在 10s 内完成（Go SetDeadline 同值）。
pub const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// **握手中**的并发上限（慢握手洪水防护；认证完成即释放——长连另设 MaxCtlConns）。
pub const HANDSHAKE_MAX: i32 = 16;

/// 已建立控制连接的读超时（3× 保活 25s + 15s；reap 轮按 `last_read` 惰性判死）。
pub const CTL_READ_TIMEOUT: Duration = Duration::from_secs(90);

/// 握手槽位的 RAII 守卫（drop 即减——失败也释放；评审 ④-5）。
pub struct HandshakeSlot {
    counter: Arc<AtomicI32>,
    armed: bool,
}

impl HandshakeSlot {
    pub fn acquire(counter: &Arc<AtomicI32>) -> Option<Self> {
        // fetch_add 后超限 = 拒绝（先加后判，撤销用 decrement）
        if counter.fetch_add(1, Ordering::SeqCst) + 1 > HANDSHAKE_MAX {
            counter.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Self { counter: Arc::clone(counter), armed: true })
    }

    /// 握手完成：提前释放并发槽（长连改由 ctl_established 总闸约束）。
    pub fn release(&mut self) {
        if self.armed {
            self.armed = false;
            self.counter.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Drop for HandshakeSlot {
    fn drop(&mut self) {
        self.release();
    }
}

/// 一次成功握手的交接物（握手线程 → 驱动线程）。
pub struct Handover {
    pub stream: TcpStream,
    pub label: [u8; 8],
    pub remote: SocketAddr,
}

/// 握手失败分类（驱动线程打哪行判据/是否计数用）。
#[derive(Debug)]
pub enum HsError {
    /// IO 失败/超时/畸形（静默丢弃——Go 同形：直接 return）。
    Io,
    /// PROOF 的 DH MAC 不过（Forged++ + 「DH 校验不过」判据行）。
    DhRejected,
    /// PROOF 的 token PSK 不过（Forged++ + 「token 校验不过」判据行）。
    PskRejected,
    /// 协议版本不符（「协议版本 %d 不符」判据行）。
    Version(u8),
    /// 已建立控制连接总数达上限（Dropped++ + 判据行；established 计数已回退）。
    EstablishedFull(u16),
}

/// 阻塞读一条控制消息（`[2B BE len][msg]`；超时/EOF/畸形 = Err）。
fn read_msg(s: &mut TcpStream) -> std::io::Result<Vec<u8>> {
    let mut hdr = [0u8; 2];
    s.read_exact(&mut hdr)?;
    let n = usize::from(u16::from_be_bytes(hdr));
    if n == 0 || n > rw::CTL_MAX {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "长度行非法"));
    }
    let mut msg = vec![0u8; n];
    s.read_exact(&mut msg)?;
    Ok(msg)
}

fn write_msg(s: &mut TcpStream, msg: &[u8]) -> std::io::Result<()> {
    let mut wire = Vec::with_capacity(2 + msg.len());
    rw::ctl_frame_into(msg, &mut wire);
    s.write_all(&wire)
}

/// 一条控制连接的完整握手（阻塞 + 10s 期限）。返回 established 流（非阻塞形态）。
///
/// `slot` 由调用方（spawn 的线程）持有；**成功路径在返回前 `slot.release()`**
/// （认证后长连不再占握手并发位）。`established` 是已建立连接的总闸（成功后由
/// 调用方在连接关闭时回退——Go estab/defer 同义）。
#[allow(clippy::too_many_arguments)]
pub fn run_handshake(
    mut stream: TcpStream,
    secret: Option<[u8; 32]>,
    established: &Arc<AtomicI32>,
    max_established: u16,
    slot: &mut HandshakeSlot,
    _logf: &Logf,
) -> Result<Handover, HsError> {
    let remote = match stream.peer_addr() {
        Ok(a) => a,
        Err(_) => return Err(HsError::Io),
    };
    let _ = stream.set_nonblocking(false);
    // 绝对期限（Go SetDeadline 同义）：socket 级超时是**每次 read** 的超时——
    // 慢滴对端每 9s 滴 1 字节可无限续命；此处每次读前收紧剩余量（评审 高-1）
    let deadline = Instant::now() + HANDSHAKE_DEADLINE;
    let tighten = |s: &mut TcpStream| -> bool {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            return false;
        }
        s.set_read_timeout(Some(remain)).is_ok() && s.set_write_timeout(Some(remain)).is_ok()
    };
    if !tighten(&mut stream) {
        return Err(HsError::Io);
    }

    // ① HELLO（版本不在 HELLO 自报——CHALLENGE 形状对老解码器必须不变）
    let hello = read_msg(&mut stream).map_err(|_| HsError::Io)?;
    if hello.first() != Some(&rw::sub::HELLO) {
        return Err(HsError::Io);
    }
    let pub_: [u8; 32] = rw::decode_hello(&hello).ok_or(HsError::Io)?;
    let label = relay_id(&pub_);

    if !tighten(&mut stream) {
        return Err(HsError::Io);
    }
    // ② CHALLENGE（临时密钥 + nonce；与 UDP 注册同款）
    let mut eph_bytes = [0u8; 32];
    getrandom::getrandom(&mut eph_bytes).map_err(|_| HsError::Io)?;
    let eph = x25519_dalek::StaticSecret::from(eph_bytes);
    let eph_pub = PublicKey::from(&eph);
    let mut nonce = [0u8; 16];
    getrandom::getrandom(&mut nonce).map_err(|_| HsError::Io)?;
    write_msg(&mut stream, &rw::encode_challenge(eph_pub.as_bytes(), &nonce)).map_err(|_| HsError::Io)?;

    if !tighten(&mut stream) {
        return Err(HsError::Io);
    }
    // ③ PROOF（**DH 恒校** + token 模式叠加 PSK；版本自报必须 =2）
    let proof = read_msg(&mut stream).map_err(|_| HsError::Io)?;
    if proof.first() != Some(&rw::sub::PROOF) {
        return Err(HsError::Io);
    }
    let parts = rw::decode_proof(&proof).ok_or(HsError::Io)?;
    if parts.nonce != nonce {
        return Err(HsError::Io);
    }
    if parts.ver != rw::RELAY_CTL_VER {
        return Err(HsError::Version(parts.ver));
    }
    let dh = eph.diffie_hellman(&PublicKey::from(pub_));
    if !rw::ct_eq_16(&rw::proof_mac(dh.as_bytes(), &nonce, &pub_), parts.mac_dh) {
        return Err(HsError::DhRejected);
    }
    if let Some(sec) = secret {
        let want = rw::auth_mac(&sec, &nonce, &pub_);
        if !rw::ct_eq_16(&want, parts.mac_psk) {
            return Err(HsError::PskRejected);
        }
    }

    // ④ 已建立总数闸（认证后才占长连位；满则拒——先加后判，撤销回退）
    if established.fetch_add(1, Ordering::SeqCst) + 1 > i32::from(max_established) {
        established.fetch_sub(1, Ordering::SeqCst);
        return Err(HsError::EstablishedFull(max_established));
    }

    // ⑤ OK（恒 v2 形状：token 模式带中继身份 MAC；开放模式零 MAC 占位）
    if !tighten(&mut stream) {
        return Err(HsError::Io);
    }
    let ok_mac = secret.map_or(vec![0u8; 16], |sec| rw::ok_auth_mac(&sec, &nonce).to_vec());
    write_msg(&mut stream, &rw::encode_ok_auth(&ok_mac)).map_err(|_| {
        established.fetch_sub(1, Ordering::SeqCst);
        HsError::Io
    })?;

    // 交接：非阻塞 + 清期限（驱动线程 poll；90s 读超时改由 last_read 判死）
    slot.release();
    let _ = stream.set_read_timeout(None);
    let _ = stream.set_write_timeout(None);
    let _ = stream.set_nonblocking(true);
    Ok(Handover { stream, label, remote })
}

/// 已建立控制连接（驱动线程独占）。
pub struct CtlConn {
    pub stream: TcpStream,
    pub label: [u8; 8],
    pub remote: SocketAddr,
    pub decoder: rw::CtlDecoder,
    pub last_read: std::time::Instant,
}

impl CtlConn {
    pub fn from_handover(h: Handover) -> Self {
        Self {
            stream: h.stream,
            label: h.label,
            remote: h.remote,
            decoder: rw::CtlDecoder::new(),
            last_read: std::time::Instant::now(),
        }
    }

    /// 非阻塞写一条消息。EAGAIN/短写 = 写失败（对端死/缓冲满——单驱动线程绝不能挂；
    /// 恢复路径与 Go「写失败即断连」相同：断连逼后端重连走重放对账）。
    pub fn write_msg_nonblocking(&mut self, msg: &[u8]) -> bool {
        use std::os::fd::AsRawFd as _;
        let mut wire = Vec::with_capacity(2 + msg.len());
        rw::ctl_frame_into(msg, &mut wire);
        // 单次 send：不留部分写状态（≤29B 消息 + 内核缓冲几乎不可能部分写）
        let fd = self.stream.as_raw_fd();
        let ptr = wire.as_ptr().cast::<libc::c_void>();
        let n = unsafe { libc::send(fd, ptr, wire.len(), libc::MSG_NOSIGNAL) };
        n == wire.len() as isize
    }

    /// 非阻塞读尽当前可读字节进 decoder，弹出完整消息。`Err(ConnDead)` = 连接死亡
    /// （EOF/ECONNRESET/长度行非法）——调用方走断连路径。
    pub fn read_available(&mut self, out: &mut Vec<(u8, Vec<u8>)>) -> Result<(), ConnDead> {
        use std::os::fd::AsRawFd as _;
        let fd = self.stream.as_raw_fd();
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::recv(fd, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len(), 0) };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::WouldBlock
                    || err.raw_os_error() == Some(libc::EINTR)
                {
                    return Ok(());
                }
                return Err(ConnDead);
            }
            if n == 0 {
                return Err(ConnDead); // EOF
            }
            self.decoder.feed(&buf[..n as usize], out).map_err(|_| ConnDead)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn noop_logf() -> Logf {
        Arc::new(|_| {})
    }

    /// 端到端握手：服务端握手线程与「后端」客户端对拍（token 模式 + DH 恒校 +
    /// 版本门 + established 闸）。
    #[test]
    fn handshake_token_mode_end_to_end() {
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = ln.local_addr().unwrap();

        let secret = [0x44u8; 32];
        let backend_priv = x25519_dalek::StaticSecret::from([0x55u8; 32]);
        let backend_pub = PublicKey::from(&backend_priv);

        // 服务端（握手线程形态同步跑）
        let established = Arc::new(AtomicI32::new(0));
        let slot = HandshakeSlot::acquire(&Arc::new(AtomicI32::new(0))).unwrap();
        let srv = {
            let ln = ln;
            let established = Arc::clone(&established);
            std::thread::spawn(move || {
                let (s, _) = ln.accept().unwrap();
                let mut slot = slot;
                run_handshake(s, Some(secret), &established, 64, &mut slot, &noop_logf())
            })
        };

        // 后端客户端
        let mut c = TcpStream::connect(addr).unwrap();
        let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = c.set_write_timeout(Some(Duration::from_secs(5)));
        write_msg(&mut c, &rw::encode_hello(backend_pub.as_bytes())).unwrap();
        let ch = read_msg(&mut c).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(&ch).unwrap();
        let dh = backend_priv.diffie_hellman(&PublicKey::from(eph_pub));
        let psk = rw::auth_mac(&secret, &nonce, backend_pub.as_bytes());
        write_msg(&mut c, &rw::encode_proof(&nonce, dh.as_bytes(), backend_pub.as_bytes(), Some(&psk))).unwrap();
        let ok = read_msg(&mut c).unwrap();
        let mac = rw::decode_ok_auth(&ok).unwrap();
        assert_eq!(mac, rw::ok_auth_mac(&secret, &nonce).to_vec());
        // established 计数 = 1（连接关闭前不回退）
        assert_eq!(established.load(Ordering::SeqCst), 1);

        let h = srv.join().unwrap().unwrap();
        assert_eq!(h.label, relay_id(backend_pub.as_bytes()));
    }

    /// DH MAC 恒校负例（评审 ①-1）：PSK 对、DH 错 → 拒（token 模式也校 DH）。
    #[test]
    fn handshake_rejects_wrong_dh_even_with_valid_psk() {
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = ln.local_addr().unwrap();
        let secret = [0x44u8; 32];
        let backend_priv = x25519_dalek::StaticSecret::from([0x56u8; 32]);
        let backend_pub = PublicKey::from(&backend_priv);
        let established = Arc::new(AtomicI32::new(0));
        let slot_ctr = Arc::new(AtomicI32::new(0));
        let srv = {
            let established = Arc::clone(&established);
            let slot_ctr = Arc::clone(&slot_ctr);
            std::thread::spawn(move || {
                let (s, _) = ln.accept().unwrap();
                let mut slot = HandshakeSlot::acquire(&slot_ctr).unwrap();
                run_handshake(s, Some(secret), &established, 64, &mut slot, &noop_logf())
            })
        };
        let mut c = TcpStream::connect(addr).unwrap();
        let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = c.set_write_timeout(Some(Duration::from_secs(5)));
        write_msg(&mut c, &rw::encode_hello(backend_pub.as_bytes())).unwrap();
        let ch = read_msg(&mut c).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(&ch).unwrap();
        // 用错误的 DH（别的密钥算的）+ 正确的 PSK
        let wrong_dh = x25519_dalek::StaticSecret::from([0x99u8; 32])
            .diffie_hellman(&PublicKey::from(eph_pub));
        let psk = rw::auth_mac(&secret, &nonce, backend_pub.as_bytes());
        write_msg(&mut c, &rw::encode_proof(&nonce, wrong_dh.as_bytes(), backend_pub.as_bytes(), Some(&psk))).unwrap();
        // 服务端断连（DH 拒 → return → conn 关闭）
        let r = srv.join().unwrap();
        assert!(matches!(r, Err(HsError::DhRejected)));
        assert_eq!(slot_ctr.load(Ordering::SeqCst), 0, "槽位已随 drop 释放");
        assert_eq!(established.load(Ordering::SeqCst), 0);
    }

    /// established 闸满：认证成功也拒（连接计数回退）。
    #[test]
    fn handshake_rejects_when_established_full() {
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = ln.local_addr().unwrap();
        let secret = [0x44u8; 32];
        let backend_priv = x25519_dalek::StaticSecret::from([0x57u8; 32]);
        let backend_pub = PublicKey::from(&backend_priv);
        let established = Arc::new(AtomicI32::new(1)); // 已占 1，max=1
        let srv = {
            let established = Arc::clone(&established);
            std::thread::spawn(move || {
                let (s, _) = ln.accept().unwrap();
                let mut slot = HandshakeSlot::acquire(&Arc::new(AtomicI32::new(0))).unwrap();
                run_handshake(s, Some(secret), &established, 1, &mut slot, &noop_logf())
            })
        };
        let mut c = TcpStream::connect(addr).unwrap();
        let _ = c.set_read_timeout(Some(Duration::from_secs(5)));
        let _ = c.set_write_timeout(Some(Duration::from_secs(5)));
        write_msg(&mut c, &rw::encode_hello(backend_pub.as_bytes())).unwrap();
        let ch = read_msg(&mut c).unwrap();
        let (eph_pub, nonce) = rw::decode_challenge(&ch).unwrap();
        let dh = backend_priv.diffie_hellman(&PublicKey::from(eph_pub));
        let psk = rw::auth_mac(&secret, &nonce, backend_pub.as_bytes());
        write_msg(&mut c, &rw::encode_proof(&nonce, dh.as_bytes(), backend_pub.as_bytes(), Some(&psk))).unwrap();
        let r = srv.join().unwrap();
        assert!(matches!(r, Err(HsError::EstablishedFull(1))));
        assert_eq!(established.load(Ordering::SeqCst), 1, "满员拒绝后计数回退到原值");
    }

    /// 慢滴攻击面（评审 高-1）：对端每 500ms 滴 1 字节（单次 read 远未超时），
    /// 但总时长远超 10s 绝对期限——握手必须在期限内断（不得占住槽位 ~5 分钟）。
    #[test]
    fn handshake_absolute_deadline_beats_slow_drip() {
        let ln = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = ln.local_addr().unwrap();
        let established = Arc::new(AtomicI32::new(0));
        let slot_ctr = Arc::new(AtomicI32::new(0));
        let srv = {
            let established = Arc::clone(&established);
            let slot_ctr = Arc::clone(&slot_ctr);
            std::thread::spawn(move || {
                let (s, _) = ln.accept().unwrap();
                let mut slot = HandshakeSlot::acquire(&slot_ctr).unwrap();
                let r = run_handshake(s, Some([9u8; 32]), &established, 64, &mut slot, &noop_logf());
                assert!(matches!(r, Err(HsError::Io)), "绝对期限内必须断");
            })
        };
        let mut c = TcpStream::connect(addr).unwrap();
        let drip_start = std::time::Instant::now();
        let mut i = 0u32;
        while std::time::Instant::now() - drip_start < Duration::from_secs(13) {
            // 每次只滴 1 字节（间隔远小于 10s 的单次读超时）
            let b = [(0x41 + (i % 26)) as u8];
            if c.write_all(&b).is_err() {
                break; // 服务端按期限断连
            }
            i += 1;
            std::thread::sleep(Duration::from_millis(500));
        }
        let _ = srv.join();
        assert_eq!(slot_ctr.load(Ordering::SeqCst), 0, "槽位随断连释放");
        assert_eq!(established.load(Ordering::SeqCst), 0);
    }

    /// 已建立连接的非阻塞读写往返（read_available/write_msg_nonblocking）。
    #[test]
    fn ctl_conn_nonblocking_rw() {
        let (mut a, b) = {
            let ln = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = ln.local_addr().unwrap();
            let b = TcpStream::connect(addr).unwrap();
            let (a, _) = ln.accept().unwrap();
            (a, b)
        };
        let _ = b.set_nonblocking(true);
        write_msg(&mut a, &rw::keepalive_bytes()).unwrap();
        write_msg(&mut a, &rw::encode_release(7)).unwrap();
        let mut conn = CtlConn {
            stream: b,
            label: [0; 8],
            remote: "127.0.0.1:1".parse().unwrap(),
            decoder: rw::CtlDecoder::new(),
            last_read: std::time::Instant::now(),
        };
        // 读两次（分帧可能一次不到齐——半帧状态机保证最终弹出）
        let mut msgs = Vec::new();
        for _ in 0..10 {
            if conn.read_available(&mut msgs).is_ok() && msgs.len() == 2 {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0], (rw::sub::KEEPALIVE, vec![]));
        assert_eq!(rw::decode_release(&{
            let mut w = vec![msgs[1].0];
            w.extend_from_slice(&msgs[1].1);
            w
        }), Some(7));
        // 回显写
        assert!(conn.write_msg_nonblocking(&rw::keepalive_bytes()));
    }
}
