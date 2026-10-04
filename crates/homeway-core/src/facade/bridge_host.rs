//! 服务桥宿主（语义真源 `baseline:clientcore/cmd/clientcore/app_bridge.go`——
//! openspec app-service-session / app-bridge-transport / tunnel-speedtest）。
//!
//! 三座沙箱内 UDS 桥（`<filesDir>/bridge/{files,term,speedtest}.sock` → 经会话拨
//! 出口虚拟端口 7802/7724/7803），消费方（files 模块 / terminal HSP / term LIST-KILL /
//! 测速）只连 socket 路径——桥 hosted 在哪个进程（隧道宿主/服务会话宿主）对它们透明。
//!
//! **首包令牌鉴权**（为什么仍然要：沙箱隔离挡的是「别的应用」，不是「本应用的别的
//! 进程」——不鉴权的桥等于把出口 $HOME 读写与 shell 免费送给任意同 UID 进程）：
//! 连接建立后客户端**先**发 48 字节（16B 魔数 + 32B 本会话随机令牌）再进各自协议；
//! 服务端读满校验，不匹配立即断开且**不回写任何字节**（不给探测者反馈面）。
//! 令牌每次 start 随机生成、只经状态 JSON（bridgeAuth = hex(魔数+令牌)）分发。
//!
//! 并发闸含**满员自愈**：对端泄漏的连接会把泵永久挂在对端读上占死坑位——满员时
//! 不拒绝，挤掉最老的一条（正常使用连接只活几秒，最老的几乎必是泄漏的）。
//!
//! 自管线程（accept 线程 ×3 + 每连接泵线程 ×2）——无 tokio、无运行时注入。

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::Logf;

use super::term_op::{write_auth, BRIDGE_AUTH_LEN};

/// 桥 socket 的子目录（`<dir>/bridge/`）。
const BRIDGE_DIR: &str = "bridge";
/// sockaddr_un.sun_path 的保守上限（darwin 104 / linux 108）。
const MAX_UNIX_SOCKET_PATH: usize = 100;
/// 桥鉴权魔数（16 字节；只在本模块与状态 JSON 的 blob 里存在——消费方拿到的是
/// 「魔数+令牌」完整 blob，无需复刻常量）。
const BRIDGE_AUTH_MAGIC: &[u8; 16] = b"TIERBRIDGEAUTH01";
/// 读鉴权首包的期限（正常消费方连上即发；5s 兜慢启动）。
const BRIDGE_AUTH_TIMEOUT: Duration = Duration::from_secs(5);
/// bind 失败（端口被占）的重试间隔：换轨窗口里旧宿主的监听器还没关、几秒内会让
/// 出来；被第三方长期占住时重试完如实报不可用。
const LISTEN_BACKOFF: [Duration; 5] = [
    Duration::from_millis(250),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
];
/// 经会话拨出口虚拟端口的预算。
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);
/// 出口虚拟端口（桥那头要拨到的地方）。
pub mod port {
    /// files 原生协议（app_files_native.go filesNativePort）。
    pub const FILES: u16 = 7802;
    /// term 服务（HOMEWAY_TERM_PORT 可改——与本常量同源环境变量）。
    pub const TERM: u16 = 7724;
    /// speedtest 服务（app_speedtest.go speedtestServicePort）。
    pub const SPEEDTEST: u16 = 7803;
}

/// term 端口的可变面（HOMEWAY_TERM_PORT 同源；测试/特殊部署用）。
pub fn term_port() -> u16 {
    std::env::var("HOMEWAY_TERM_PORT")
        .ok()
        .and_then(|v| v.parse::<u16>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(port::TERM)
}

/// 桥 socket 路径：`<dir>/bridge/<name>.sock`。
pub fn bridge_socket_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(BRIDGE_DIR).join(format!("{name}.sock"))
}

/// 并发闸（只防重试循环失控；满员自愈 = 挤掉最老的一条连接——对端泄漏的连接会把
/// 泵永久挂在对端读上占死坑位）。
struct ConnGate {
    max: usize,
    /// (票, fd 克隆——仅用于满员时 shutdown 挤掉)。
    live: Mutex<VecDeque<(u64, UnixStream)>>,
    next_ticket: AtomicU64,
}

impl ConnGate {
    fn new(max: usize) -> Self {
        ConnGate { max, live: Mutex::new(VecDeque::new()), next_ticket: AtomicU64::new(1) }
    }

    /// 登记：满员挤掉最老（返回其 fd 供调用方断连）。clone 失败不入表（少记一条
    /// 比误挤好）。
    fn admit(&self, conn: &UnixStream) -> (u64, Option<UnixStream>) {
        let mut live = self.live.lock().expect("gate 锁中毒");
        let evicted = if live.len() >= self.max {
            live.pop_front().map(|(_, c)| c)
        } else {
            None
        };
        let ticket = self.next_ticket.fetch_add(1, Ordering::Relaxed);
        if let Ok(c) = conn.try_clone() {
            live.push_back((ticket, c));
        }
        (ticket, evicted)
    }

    fn leave(&self, ticket: u64) {
        self.live.lock().expect("gate 锁中毒").retain(|(t, _)| *t != ticket);
    }
}

/// 闸票守卫：连接两端全断时（guard Drop）销票。
struct GateGuard {
    gate: Arc<ConnGate>,
    ticket: u64,
}

impl Drop for GateGuard {
    fn drop(&mut self) {
        self.gate.leave(self.ticket);
    }
}

/// 一座桥的运行态。
struct BridgeSock {
    name: &'static str,
    port: u16,
    path: PathBuf,
    listener: Option<UnixListener>,
    /// bind 出的 socket 文件身份（stop 只删自己的——防误删接管者；(dev, ino)）。
    own: Option<(u64, u64)>,
}

impl BridgeSock {
    /// 带重试的 UDS listen（返回 None = 重试耗尽或已被 stop）。listener 归 accept
    /// 线程所有（stop 经 stopped 位让线程退出，fd 由 drop 关——Rust std 不在 drop 时
    /// unlink，文件删除统一走 remove_sock_own 身份比对）。
    /// 残留处理按「死/活」区分：先拨一下现有路径——拨得通 = 活宿主占着（不删它的
    /// 文件，listen 自然 address in use → 走退避让位）；拨不通（ECONNREFUSED/
    /// ENOENT/ENOTSOCK）才是死残留，删掉重绑。
    fn listen(&mut self, stopped: &AtomicBool, logf: &Logf) -> Option<UnixListener> {
        for (attempt, backoff) in std::iter::once(Duration::ZERO).chain(LISTEN_BACKOFF.iter().copied()).enumerate() {
            if stopped.load(Ordering::Acquire) {
                return None;
            }
            if !backoff.is_zero() {
                std::thread::sleep(backoff);
            }
            if sock_path_free(&self.path) {
                let _ = std::fs::remove_file(&self.path); // 死残留（前主人异常退出没清文件）
            }
            match UnixListener::bind(&self.path) {
                Ok(ln) => {
                    self.own = std::fs::metadata(&self.path).ok().map(|m| (m.dev(), m.ino()));
                    ln.set_nonblocking(true).ok();
                    (logf)(&format!(
                        "{}: {} unix:{} → 出口虚拟端口（经会话）监听中",
                        "桥宿主", self.name, self.path.display()
                    ));
                    return Some(ln);
                }
                Err(e) => {
                    if attempt >= LISTEN_BACKOFF.len() {
                        (logf)(&format!(
                            "{}: {} 监听 unix:{} 失败（{e}）——重试 {attempt} 次后放弃，本轮不可用（换轨让位或目录异常）",
                            "桥宿主", self.name, self.path.display()
                        ));
                        return None;
                    }
                }
            }
        }
        None
    }
}

/// 现有 socket 路径是否无主（ENOENT / ECONNREFUSED / ENOTSOCK）；拨得通或状态
/// 不明都按「有活主人」（保守：不删状态不明的东西）。
fn sock_path_free(path: &Path) -> bool {
    match UnixStream::connect(path) {
        Ok(_) => false,
        Err(e) => matches!(e.kind(), ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::NotConnected)
            || e.raw_os_error() == Some(libc::ENOTSOCK),
    }
}

/// 只删「还是自己 bind 出来的那个文件」；路径已被其它宿主接管时不动它。
fn remove_sock_own(path: &Path, own: Option<(u64, u64)>) {
    match own {
        None => {
            let _ = std::fs::remove_file(path);
        }
        Some((dev, ino)) => {
            if let Ok(md) = std::fs::metadata(path) {
                if md.dev() == dev && md.ino() == ino {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
    }
}

/// 桥宿主（一个宿主一个实例；同路径同时只有一个宿主能 listen）。
pub struct BridgeHost {
    what: &'static str,
    dir: Option<PathBuf>,
    logf: Logf,
    /// 「经会话拨出口本机端口」的接缝（隧道宿主/服务会话宿主各注入自己的实现）。
    dial_port: Box<dyn Fn(u16, Duration) -> std::io::Result<UnixStream> + Send + Sync>,
    inner: Mutex<HostInner>,
    stopped: Arc<AtomicBool>,
    auth_drops: AtomicU64,
}

struct HostInner {
    socks: Vec<BridgeSock>,
    /// 32B 令牌；None = 未启动/已停止。
    token: Option<[u8; 32]>,
    files_gate: Arc<ConnGate>,
    term_gate: Arc<ConnGate>,
    speed_gate: Arc<ConnGate>,
}

/// 桥状态快照（状态 JSON 的 bridge 四键源）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BridgeStatus {
    /// hex(魔数+令牌)，96 hex 字符；桥未起/已停为空串。只经状态通道分发，不落盘
    /// 不进日志。
    pub auth_hex: String,
    pub files_sock: String,
    pub term_sock: String,
    pub speed_sock: String,
}

impl BridgeHost {
    /// 构造（未启动）。`dir` = filesDir（socket 落 `<dir>/bridge/*.sock`；None = 不起桥）。
    pub fn new(
        what: &'static str,
        dir: Option<PathBuf>,
        logf: Logf,
        dial_port: Box<dyn Fn(u16, Duration) -> std::io::Result<UnixStream> + Send + Sync>,
    ) -> Self {
        BridgeHost {
            what,
            dir,
            logf,
            dial_port,
            inner: Mutex::new(HostInner {
                socks: Vec::new(),
                token: None,
                files_gate: Arc::new(ConnGate::new(8)),
                term_gate: Arc::new(ConnGate::new(16)),
                speed_gate: Arc::new(ConnGate::new(10)),
            }),
            stopped: Arc::new(AtomicBool::new(false)),
            auth_drops: AtomicU64::new(0),
        }
    }

    /// 起桥（幂等；非阻塞——listen 重试在后台线程）。任一座桥起不来只记状态不
    /// 阻断宿主（桥是附加能力，不是数据面本体）。
    pub fn start(self: &Arc<Self>) {
        let mut inner = self.inner.lock().expect("桥宿主锁中毒");
        if inner.token.is_some() || self.stopped.load(Ordering::Acquire) {
            return; // 已启动 / 已停止
        }
        let Some(dir) = self.dir.clone() else {
            (self.logf)(&format!("{}: 没有桥目录（identityDir 未配置）—— files/term/测速 本轮不可用", self.what));
            return;
        };
        let bridge_dir = dir.join(BRIDGE_DIR);
        if let Err(e) = std::fs::create_dir_all(&bridge_dir) {
            (self.logf)(&format!("{}: 建桥目录失败（{e}）—— files/term/测速 本轮不可用", self.what));
            return;
        }
        // sun_path 上限预检：路径超长的 bind 报 invalid argument 且重试无意义。
        // 测速桥路径比 files 长 4 字节——极端下「files 能用、测速超长」只跳过测速
        // 自己，不连累 files/term。
        let files = bridge_socket_path(&dir, "files");
        if files.as_os_str().len() >= MAX_UNIX_SOCKET_PATH {
            (self.logf)(&format!(
                "{}: 桥路径超长（{} ≥ {MAX_UNIX_SOCKET_PATH} 字节，sun_path 上限）—— files/term 本轮不可用；把 filesDir 挪短或反馈",
                self.what,
                files.as_os_str().len()
            ));
            return;
        }
        let mut socks = vec![
            BridgeSock { name: "files-bridge", port: port::FILES, path: files, listener: None, own: None },
            BridgeSock { name: "term-bridge", port: term_port(), path: bridge_socket_path(&dir, "term"), listener: None, own: None },
        ];
        let speed_path = bridge_socket_path(&dir, "speedtest");
        if speed_path.as_os_str().len() >= MAX_UNIX_SOCKET_PATH {
            (self.logf)(&format!(
                "{}: 测速桥路径超长（{} ≥ {MAX_UNIX_SOCKET_PATH} 字节）—— 测速本轮不可用（files/term 不受影响）",
                self.what,
                speed_path.as_os_str().len()
            ));
        } else {
            socks.push(BridgeSock { name: "speed-bridge", port: port::SPEEDTEST, path: speed_path, listener: None, own: None });
        }
        // 令牌（32B 随机；生成失败宁可不 exposing 桥——鉴权是硬要求，不做明文回退）
        let mut tok = [0u8; 32];
        if getrandom_fill(&mut tok).is_err() {
            (self.logf)(&format!("{}: 生成桥令牌失败 —— files/term/测速 本轮不可用", self.what));
            return;
        }
        inner.socks = socks;
        inner.token = Some(tok);
        drop(inner);

        // 每座桥一个 accept 线程（一座端口被占重试不拖另一座）
        for idx in 0..3 {
            let host = Arc::clone(self);
            std::thread::Builder::new()
                .name("hw-bridge-accept".into())
                .spawn(move || host.accept_loop(idx))
                .ok();
        }
        (self.logf)(&format!("{}: 桥宿主启动（三座桥后台就位中）", self.what));
    }

    /// 一座桥的 accept 循环（非阻塞 accept + 100ms 节拍轮询 stopped——桥连接非
    /// 热路径，粗粒度足够；stop 位置起后线程退出、fd 由 drop 关）。
    fn accept_loop(self: Arc<Self>, idx: usize) {
        let (name, port) = {
            let inner = self.inner.lock().expect("桥宿主锁中毒");
            match inner.socks.get(idx) {
                Some(s) => (s.name, s.port),
                None => return,
            }
        };
        let ln = {
            let mut inner = self.inner.lock().expect("桥宿主锁中毒");
            let Some(sock) = inner.socks.get_mut(idx) else { return };
            sock.listen(&self.stopped, &self.logf)
        };
        let Some(ln) = ln else { return };

        while !self.stopped.load(Ordering::Acquire) {
            match ln.accept() {
                Ok((conn, _)) => {
                    conn.set_nonblocking(false).ok();
                    self.handle_conn(name, port, conn);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => return, // 监听器被关（收工）
            }
        }
    }

    /// 一条桥接连接：闸 → 鉴权 → 经会话拨出口 → 双向泵。
    fn handle_conn(&self, name: &'static str, port: u16, mut conn: UnixStream) {
        let (gate, token) = {
            let inner = self.inner.lock().expect("桥宿主锁中毒");
            let g = match name {
                "files-bridge" => Arc::clone(&inner.files_gate),
                "term-bridge" => Arc::clone(&inner.term_gate),
                _ => Arc::clone(&inner.speed_gate),
            };
            (g, inner.token)
        };
        let (ticket, evicted) = gate.admit(&conn);
        if let Some(old) = evicted {
            // 满员自愈：挤掉最老的（shutdown 其克隆 fd——泵读立即以错误返回）
            let _ = old.shutdown(std::net::Shutdown::Both);
            (self.logf)(&format!("{name}: 并发已达上限，挤掉最老的一条连接自愈（疑似客户端泄漏——正常使用连接只活几秒）"));
        }
        let guard = GateGuard { gate, ticket };
        if !Self::server_auth(&mut conn, token) {
            drop(conn);
            let n = self.auth_drops.fetch_add(1, Ordering::Relaxed);
            if n <= 5 || n.is_multiple_of(50) {
                (self.logf)(&format!(
                    "{name}: 回环连接鉴权失败已断开（累计 {}；多为其它应用探测本机端口）",
                    n + 1
                ));
            }
            return;
        }
        match (self.dial_port)(port, DIAL_TIMEOUT) {
            Ok(remote) => {
                // 双向泵（两条线程；任一方向断即整体收口；守卫随最后一泵结束销票）
                let mut up_r = conn.try_clone().expect("桥接 fd 复制");
                let mut up_w = remote.try_clone().expect("桥接 fd 复制");
                let mut down_r = remote;
                let mut down_w = conn;
                // 守卫克隆进两泵（Arc 强计数——最后一泵结束才销票）
                let guard = Arc::new(guard);
                let g1 = Arc::clone(&guard);
                let g2 = Arc::clone(&guard);
                drop(guard);
                std::thread::Builder::new()
                    .name("hw-bridge-pump".into())
                    .spawn(move || {
                        let _g = g1;
                        pump(&mut up_r, &mut up_w);
                    })
                    .ok();
                std::thread::Builder::new()
                    .name("hw-bridge-pump".into())
                    .spawn(move || {
                        let _g = g2;
                        pump(&mut down_r, &mut down_w);
                    })
                    .ok();
            }
            Err(e) => {
                (self.logf)(&format!("{name}: 经会话拨出口 {port} 失败: {e}"));
                // 不回帧直接关（refused 类由客户端零字节 EOF 归 not_supported；
                // link_down 回帧面属 speedtest 引擎层，本泵不解释协议）
            }
        }
    }

    /// 收工（幂等）：关监听器，已建立的桥接连接随会话关闭自然断。令牌清零——状态
    /// 面此后读到空 bridgeAuth，消费方自然失败。
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let socks = {
            let mut inner = self.inner.lock().expect("桥宿主锁中毒");
            inner.token = None;
            std::mem::take(&mut inner.socks)
        };
        for s in socks {
            if let Some(ln) = s.listener {
                drop(ln);
            }
            remove_sock_own(&s.path, s.own);
        }
        (self.logf)(&format!("{}: 已停止监听", self.what));
    }

    /// 当前三座桥的状态（socket 路径随令牌在场；桥未起/已停全空串）。
    pub fn status(&self) -> BridgeStatus {
        let inner = self.inner.lock().expect("桥宿主锁中毒");
        let Some(tok) = inner.token else {
            return BridgeStatus::default();
        };
        let pick = |name: &str| {
            inner
                .socks
                .iter()
                .find(|s| s.path.ends_with(format!("{name}.sock")))
                .map(|s| s.path.display().to_string())
                .unwrap_or_default()
        };
        let mut blob = Vec::with_capacity(BRIDGE_AUTH_LEN);
        blob.extend_from_slice(BRIDGE_AUTH_MAGIC);
        blob.extend_from_slice(&tok);
        BridgeStatus {
            auth_hex: blob.iter().map(|b| format!("{b:02x}")).collect(),
            files_sock: pick("files"),
            term_sock: pick("term"),
            speed_sock: pick("speedtest"),
        }
    }

    /// 鉴权首包的服务端校验（读满 48B 比对；不匹配断开且不回写任何字节）。
    fn server_auth(conn: &mut UnixStream, token: Option<[u8; 32]>) -> bool {
        let Some(token) = token else { return false }; // 正在收工
        conn.set_read_timeout(Some(BRIDGE_AUTH_TIMEOUT)).ok();
        let mut buf = [0u8; BRIDGE_AUTH_LEN];
        if conn.read_exact(&mut buf).is_err() {
            return false;
        }
        conn.set_read_timeout(None).ok();
        buf[..16] == *BRIDGE_AUTH_MAGIC && buf[16..] == token
    }
}

/// 单向泵：读尽即关对侧写端（EOF 传播）；错误亦收口。
fn pump(r: &mut UnixStream, w: &mut UnixStream) {
    let mut buf = [0u8; 16 * 1024];
    loop {
        match r.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if w.write_all(&buf[..n]).is_err() {
                    break;
                }
            }
        }
    }
    // 关写端传播 EOF（shutdown 不影响另一方向的读）
    let _ = w.shutdown(std::net::Shutdown::Write);
}

/// getrandom 填充（32B 令牌；不引直接依赖——getrandom crate 已在依赖面）。
fn getrandom_fill(buf: &mut [u8]) -> Result<(), ()> {
    getrandom::getrandom(buf).map_err(|_| ())
}

/// 客户端侧：连上桥后先发鉴权首包（`write_auth` 的再导出——同链分发的客户端面）。
pub fn bridge_client_auth<W: Write>(w: &mut W, auth_hex: &str) -> std::io::Result<()> {
    write_auth(w, auth_hex)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;
    use std::sync::mpsc;

    fn tmp_dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hwbridge-{:?}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn log_silent() -> (Logf, Arc<Mutex<Vec<String>>>) {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let l2 = Arc::clone(&lines);
        (
            Arc::new(move |m: &str| {
                l2.lock().unwrap().push(m.to_owned());
            }),
            lines,
        )
    }

    /// 起停 + 状态面：start 后 auth_hex 96 字符 + 三 sock；stop 后全空。
    #[test]
    fn start_stop_status_face() {
        let dir = tmp_dir("face");
        let (logf, _lines) = log_silent();
        // dial_port 恒失败（本测试不起会话）
        let host = Arc::new(BridgeHost::new("测试桥", Some(dir.clone()), logf, Box::new(|_, _| {
            Err(std::io::Error::new(ErrorKind::ConnectionRefused, "无会话"))
        })));
        host.start();
        // 等 listen 就位（auth_hex 在 start 同步段就有；socket 文件在后台线程落盘）
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let files_path = bridge_socket_path(&dir, "files");
        while (!Path::new(&files_path).exists()) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(Path::new(&files_path).exists(), "files 桥应在 2s 内监听就位");
        let st = host.status();
        assert_eq!(st.auth_hex.len(), 96, "hex(16B 魔数 + 32B 令牌) = 96 hex");
        assert!(st.files_sock.ends_with("bridge/files.sock"));
        assert!(st.term_sock.ends_with("bridge/term.sock"));
        assert!(st.speed_sock.ends_with("bridge/speedtest.sock"));
        let magic_hex: String = BRIDGE_AUTH_MAGIC.iter().map(|b| format!("{b:02x}")).collect();
        assert!(st.auth_hex.starts_with(&magic_hex), "魔数 hex 前缀（TIERBRIDGEAUTH01）");
        // socket 文件在盘上
        assert!(Path::new(&st.files_sock).exists());
        host.stop();
        let st = host.status();
        assert_eq!(st, BridgeStatus::default(), "stop 后状态面全空");
        assert!(!Path::new(&host.status().files_sock).exists() || host.status().files_sock.is_empty());
    }

    /// 鉴权链：坏魔数/坏令牌 ⇒ 服务端静默断开（客户端读到 EOF）；好令牌 ⇒ 放行进泵
    /// （dial 失败也断——本测试 dial 恒败，但**在鉴权之后**才断）。
    #[test]
    fn auth_chain() {
        let dir = tmp_dir("auth");
        let (logf, _lines) = log_silent();
        let (tx, rx) = mpsc::channel::<u16>();
        let rx = Arc::new(Mutex::new(rx));
        let host = Arc::new(BridgeHost::new("测试桥", Some(dir), logf, Box::new(move |port, _| {
            tx.send(port).unwrap();
            Err(std::io::Error::new(ErrorKind::ConnectionRefused, "拒"))
        })));
        host.start();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while host.status().auth_hex.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let auth = host.status().auth_hex.clone();
        let sock = host.status().term_sock.clone();
        assert!(!auth.is_empty());
        // auth_hex 在 start 同步段就有了，listen 是后台线程——等 socket 文件真正落盘
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !Path::new(&sock).exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(Path::new(&sock).exists(), "term 桥应在 2s 内监听就位");

        // 好令牌：应走到 dial（rx 收到 7724）然后被断
        let a2 = auth.clone();
        let s2 = sock.clone();
        let rx2 = Arc::clone(&rx);
        let (seen_port, eof) = std::thread::spawn(move || {
            let mut c = UnixStream::connect(&s2).unwrap();
            bridge_client_auth(&mut c, &a2).unwrap();
            let mut b = [0u8; 8];
            let eof = matches!(c.read(&mut b), Ok(0) | Err(_));
            (
                rx2.lock().unwrap().recv_timeout(Duration::from_secs(2)),
                eof,
            )
        })
        .join()
        .unwrap();
        assert_eq!(seen_port.unwrap(), 7724, "鉴权过 ⇒ 泵起 ⇒ 经会话拨 term 端口");
        assert!(eof, "dial 失败后连接收口");

        // 坏令牌（改尾字节）：静默断开，且**不触发 dial**
        let mut bad = a2_from(&auth);
        let last = bad.len() - 2;
        bad.replace_range(last.., if &bad[last..] == "00" { "01" } else { "00" });
        let s3 = host.status().term_sock.clone();
        let dial_count = std::thread::spawn(move || {
            let mut c = UnixStream::connect(&s3).unwrap();
            bridge_client_auth(&mut c, &bad).unwrap();
            let mut b = [0u8; 8];
            let _ = c.read(&mut b);
        })
        .join();
        let _ = dial_count;
        assert!(rx.lock().unwrap().try_recv().is_err(), "坏令牌不得触发 dial");
        host.stop();
    }

    fn a2_from(auth: &str) -> String {
        auth.to_owned()
    }

    /// 无目录形态（dir=None）：start 即日志 + 不起桥。
    #[test]
    fn no_dir_no_bridge() {
        let (logf, lines) = log_silent();
        let host = Arc::new(BridgeHost::new("测试桥", None, logf, Box::new(|_, _| unreachable!())));
        host.start();
        assert_eq!(host.status(), BridgeStatus::default());
        assert!(lines.lock().unwrap().iter().any(|l| l.contains("没有桥目录")));
    }

    /// 死残留清理：预置一个 socket 文件（无监听）⇒ listen 应删掉重绑成功。
    #[test]
    fn stale_socket_reclaimed() {
        let dir = tmp_dir("stale");
        let (logf, _lines) = log_silent();
        std::fs::create_dir_all(dir.join("bridge")).unwrap();
        let files_sock = dir.join("bridge").join("files.sock");
        UnixListener::bind(&files_sock).unwrap(); // 死残留（listener 立即 drop，无 accept）
        drop(std::fs::read_dir(&dir).unwrap().next());
        let host = Arc::new(BridgeHost::new("测试桥", Some(dir), logf, Box::new(|_, _| {
            Err(std::io::Error::new(ErrorKind::ConnectionRefused, "无"))
        })));
        host.start();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while host.status().auth_hex.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!host.status().auth_hex.is_empty(), "死残留被清理、桥就位");
        host.stop();
    }

    #[test]
    fn path_helpers() {
        let p = bridge_socket_path(Path::new("/data/f"), "files");
        assert_eq!(p, Path::new("/data/f/bridge/files.sock"));
        assert_eq!(term_port(), 7724);
    }
}
