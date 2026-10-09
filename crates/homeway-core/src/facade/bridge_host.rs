//! 服务桥宿主（语义真源 `baseline:clientcore/cmd/clientcore/app_bridge.go`——
//! openspec app-service-session / app-bridge-transport / tunnel-speedtest）。
//!
//! 三座沙箱内 UDS 桥（`<filesDir>/bridge/{files,term,speedtest}.sock` → 拨出口的
//! **服务**：QUIC 档 = `STREAM[tag]`（`facade/quic_stream.rs`），WG 档 = 经会话拨
//! 虚拟端口 7802/7724/7803），消费方（files 模块 / terminal HSP / term LIST-KILL /
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
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use crate::Logf;

use super::term_op::{write_auth, BRIDGE_AUTH_LEN};

/// 桥接流抽象（工单⑤ dial_port 接缝：远端可以是本机 UDS（服务会话直拨形态），
/// 也可以是「经隧道会话的流 id」——TunnelExec 注入 SessionConn）。
pub trait BridgeStream: Send {
    /// 拆成读/写两半（两半可在不同线程并发使用——泵模型的前提；UDS = try_clone，
    /// 会话流 = 各自持 `Arc<Client> + id` 的半句柄）。
    fn into_halves(
        self: Box<Self>,
    ) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)>;
}

/// 单向写端的半关闭（EOF 传播：UDS = shutdown(WRITE)；会话流 = 发 FIN——
/// files 的「write 关流即取消」靠它）。
pub trait WriteHalf: Write {
    fn close_write(&mut self);
}

impl BridgeStream for UnixStream {
    fn into_halves(
        self: Box<Self>,
    ) -> std::io::Result<(Box<dyn Read + Send>, Box<dyn WriteHalf + Send>)> {
        let r = (*self).try_clone()?;
        let w: Box<dyn WriteHalf + Send> = self;
        Ok((Box::new(r), w))
    }
}

impl WriteHalf for UnixStream {
    fn close_write(&mut self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

/// 本机 TCP 流的半关闭（Q-F-B F2-3/N3：portfwd 的本地监听口是 `TcpStream`，泵复用
/// 需要它也能传播 FIN——`shutdown(Write)` 与 UDS 形态同义）。
impl WriteHalf for std::net::TcpStream {
    fn close_write(&mut self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

/// 会话流的读写两半统一包装面（读半纯 `dyn Read`；写半 `dyn WriteHalf`）。
impl WriteHalf for Box<dyn WriteHalf + Send> {
    fn close_write(&mut self) {
        (**self).close_write()
    }
}

/// 宿主域的锁获取（工单②：锁中毒不 panic——c-shared 宿主进程里 panic = 扩展进程
/// 死；持锁线程 panic 后数据仍可用，into_inner 取出继续）。
fn lock_host<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// 桥拨号闭包面（工单⑤ dial_port 接缝的类型别名）。
///
/// Q-F F4：**单个 `Arc`**——`handle_conn` 从锁里克隆 Arc 后**出锁拨号**（此前持锁
/// 跨 15s 拨号，`status()`/`set_dial` 同锁被同步阻塞；原注释的「换轨窗口防半换」
/// 理由在两处生产路径都不成立：隧道域构造即注入真闭包、服务域只在 `start()` 内换
/// 一次，换轨时无在途拨号）。不留 `Arc<Mutex<…>>` 退路。
pub type DialFn = Arc<dyn Fn(u16, Duration) -> std::io::Result<Box<dyn BridgeStream>> + Send + Sync>;

/// 现有 socket 路径是否无主的探测预算（工单②：本地 UDS connect 一般即成，
/// 但对端 backlog 满时会挂——200ms 内连不上按「有活主人」处理，不误删）。
const PROBE_BUDGET: Duration = Duration::from_millis(200);

/// 有预算的 UDS 探测/拨号连接（评审 r2-M5 整改：**真非阻塞 connect**——
/// socket(SOCK_NONBLOCK) → connect 返回 EINPROGRESS → poll(2) 等可写 → SO_ERROR
/// 取连接结果；旧实现先阻塞 `UnixStream::connect` 再设非阻塞，预算完全没覆盖
/// connect 本身（对端 backlog 满正是要防的形态）。超时 = TimedOut）。
/// pub(crate)：files/term/speedtest 桥消费方共用（工单④：UDS 拨号加 connect 预算）。
pub(crate) fn connect_budget(path: &Path, budget: Duration) -> std::io::Result<UnixStream> {
    use std::os::fd::AsRawFd as _;
    // sockaddr_un 组装（路径长度由调用侧的 SUN_PATH_MAX 预检兜）
    let mut addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    addr.sun_family = libc::AF_UNIX as libc::sa_family_t;
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() >= addr.sun_path.len() {
        return Err(std::io::Error::new(
            ErrorKind::InvalidInput,
            "unix socket 路径超长（sun_path 上限）",
        ));
    }
    addr.sun_path[..bytes.len()]
        .copy_from_slice(unsafe { std::slice::from_raw_parts(bytes.as_ptr().cast(), bytes.len()) });
    // F1：socket 走 sysfd 单源（linux/OHOS 原子 SOCK_CLOEXEC；darwin 建后立即补）——
    // 与非阻塞设置合并为一次 flags 收口；出错路径 fd 随 OwnedFd drop 关。
    let fd = crate::sysfd::socket_cloexec(libc::AF_UNIX, libc::SOCK_STREAM, 0)?;
    let raw = fd.as_raw_fd();
    unsafe {
        let fl = libc::fcntl(raw, libc::F_GETFL);
        if fl < 0 || libc::fcntl(raw, libc::F_SETFL, fl | libc::O_NONBLOCK) < 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    let r = unsafe {
        libc::connect(
            raw,
            &addr as *const _ as *const libc::sockaddr,
            std::mem::size_of::<libc::sockaddr_un>() as libc::socklen_t,
        )
    };
    if r < 0 {
        let e = std::io::Error::last_os_error();
        // EINPROGRESS = 非阻塞连接已发起（darwin/linux 同码）；其余（ENOENT/ECONNREFUSED/
        // EACCES…）是即时结果，直接回
        if e.raw_os_error() != Some(libc::EINPROGRESS) {
            return Err(e);
        }
        let mut pfd = libc::pollfd {
            fd: raw,
            events: libc::POLLOUT,
            revents: 0,
        };
        let t = budget.as_millis().clamp(1, i32::MAX as u128) as i32;
        let pr = unsafe { libc::poll(&mut pfd, 1, t) };
        if pr < 0 {
            return Err(std::io::Error::last_os_error());
        }
        if pr == 0 {
            return Err(std::io::Error::new(ErrorKind::TimedOut, "探测连接超时"));
        }
        // 连接结果经 SO_ERROR 取（POLLERR 也走这里拿真实 errno）
        let mut err: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        if unsafe {
            libc::getsockopt(
                raw,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut err as *mut _ as *mut _,
                &mut len,
            )
        } != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        if err != 0 {
            return Err(std::io::Error::from_raw_os_error(err));
        }
    }
    let s = UnixStream::from(fd);
    s.set_nonblocking(false)?;
    Ok(s)
}

/// 桥 socket 的子目录（`<dir>/bridge/`）。
const BRIDGE_DIR: &str = "bridge";
/// sun_path 上限**单源**（darwin 103 / linux·OHOS 107——`sysfd::SUN_PATH_MAX`，
/// 判据统一 `len > SUN_PATH_MAX`；Q-G F4.3）。
const MAX_UNIX_SOCKET_PATH: usize = crate::sysfd::SUN_PATH_MAX;
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
/// 出口虚拟端口（桥那头要拨到的**服务标识**；M3 起 QUIC 档不再拨端口，而是按
/// 这些端口选出 `STREAM[tag]`——映射单源 = `facade/quic_stream.rs::tag_for_port`；
/// 端口号本身仍是**配置面**（E1 的值域/字段不变）与 WG 档的拨号目标）。
pub mod port {
    /// files 原生协议（app_files_native.go filesNativePort；QUIC 档 ⇒ `STREAM[tag=1]`）。
    pub const FILES: u16 = 7802;
    /// term 服务（HOMEWAY_TERM_PORT 可改——与本常量同源环境变量；QUIC 档 ⇒ `STREAM[tag=2]`）。
    pub const TERM: u16 = 7724;
    /// speedtest 服务（app_speedtest.go speedtestServicePort；QUIC 档 ⇒ `STREAM[tag=3]`）。
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
        ConnGate {
            max,
            live: Mutex::new(VecDeque::new()),
            next_ticket: AtomicU64::new(1),
        }
    }

    /// 登记：满员挤掉最老（返回其 fd 供调用方断连）。clone 失败不入表（少记一条
    /// 比误挤好）。
    fn admit(&self, conn: &UnixStream) -> (u64, Option<UnixStream>) {
        let mut live = lock_host(&self.live);
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
        lock_host(&self.live).retain(|(t, _)| *t != ticket);
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
    /// bind 出的 socket 文件身份（stop 只删自己的——防误删接管者；(dev, ino)）。
    /// （评审 r2-L8：listener 恒 None 的死字段已删——监听器由 accept 线程独占持有，
    /// stop 的收口 = stopped 位 + 100ms 节拍轮询 + 2s 有界等待，不靠关 fd。）
    own: Option<(u64, u64)>,
    /// Q-F F7b（N2 同族「假状态」）：accept 线程起不来或 listen 重试耗尽 ⇒ 置位，
    /// `status()` 对该桥**返回空路径**（App 拿不到非空路径就连接不上的谎报）。
    /// **置位点 = spawn 点与 listen 结论点**（不能放 accept_loop 入口——spawn 失败
    /// 时那里根本不执行，设计门 C5）。**本批加固：偏离 Go**（Go 的 `sockJSON` 只要
    /// token 在就上报路径，`app_bridge.go:388-395`）——登记见 §5.3。
    ///
    /// 已知形态：`start()` 到 listen 有结论之间的短暂非空窗口（异步窗口）——不是
    /// 回归，消费方按路径存在但连不上时的既有重试语义处理。
    /// （代码门 ⑥-1：本字段的全部读写都在 `Mutex<HostInner>` 内 ⇒ 普通 `bool`
    /// 足够，不用 atomic。）
    unavailable: bool,
}

impl BridgeSock {
    /// 带 bind 有界重试的 listen（工单②：**不持宿主锁**——重试最长 ~7.75s）。
    /// 残留处理按「死/活」区分：先拨一下现有路径——拨得通 = 活宿主占着（不删它的
    /// 文件，listen 自然 address in use → 走退避让位）；拨不通（ECONNREFUSED/
    /// ENOENT/ENOTSOCK）才是死残留，删掉重绑。
    fn listen_path(
        name: &str,
        path: &Path,
        stopped: &AtomicBool,
        logf: &Logf,
    ) -> Option<(UnixListener, Option<(u64, u64)>)> {
        for (attempt, backoff) in std::iter::once(Duration::ZERO)
            .chain(LISTEN_BACKOFF.iter().copied())
            .enumerate()
        {
            if stopped.load(Ordering::Acquire) {
                return None;
            }
            if !backoff.is_zero() {
                std::thread::sleep(backoff);
            }
            if sock_path_free(path) {
                let _ = std::fs::remove_file(path); // 死残留（前主人异常退出没清文件）
            }
            match UnixListener::bind(path) {
                Ok(ln) => {
                    let own = std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()));
                    ln.set_nonblocking(true).ok();
                    (logf)(&format!(
                        "{}: {} unix:{} → 出口虚拟端口（经会话）监听中",
                        "桥宿主",
                        name,
                        path.display()
                    ));
                    return Some((ln, own));
                }
                Err(e) => {
                    if attempt >= LISTEN_BACKOFF.len() {
                        (logf)(&format!(
                            "{}: {} 监听 unix:{} 失败（{e}）——重试 {attempt} 次后放弃，本轮不可用（换轨让位或目录异常）",
                            "桥宿主", name, path.display()
                        ));
                        return None;
                    }
                }
            }
        }
        None
    }
}

/// 现有 socket 路径是否无主（ENOENT / ECONNREFUSED / ENOTSOCK / 探测超时）；拨得通
/// 或状态不明都按「有活主人」（保守：不删状态不明的东西）。探测带 200ms 预算
/// （工单②：对端 backlog 满时 connect 挂死不得卡 accept 线程）。
fn sock_path_free(path: &Path) -> bool {
    match connect_budget(path, PROBE_BUDGET) {
        Ok(_) => false,
        Err(e) => {
            matches!(
                e.kind(),
                ErrorKind::NotFound | ErrorKind::ConnectionRefused | ErrorKind::NotConnected
            ) || e.raw_os_error() == Some(libc::ENOTSOCK)
        }
    }
}

/// 只删「还是自己 bind 出来的那个文件」；路径已被其它宿主接管时不动它。
/// **own=None（从未成功 bind / 身份取证失败）不删**（工单②：换轨窗口里这个
/// 文件可能是接管方在用的——误删 = 踢掉服务中的桥）。
fn remove_sock_own(path: &Path, own: Option<(u64, u64)>) {
    let Some((dev, ino)) = own else { return };
    if let Ok(md) = std::fs::metadata(path) {
        if md.dev() == dev && md.ino() == ino {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// 桥宿主（一个宿主一个实例；同路径同时只有一个宿主能 listen）。
pub struct BridgeHost {
    what: &'static str,
    dir: Option<PathBuf>,
    logf: Logf,
    /// 「经会话拨出口本机端口」的接缝（隧道宿主/服务会话宿主各注入自己的实现；
    /// 返回实现 [`BridgeStream`]——本机 UDS 或经隧道的流 id 适配器〔工单⑤〕）。
    /// 可后换（服务桥的会话句柄在受理线程里才建好——set_dial 换入真拨号面）。
    dial_port: Mutex<DialFn>,
    /// 拨号预算（缺省 DIAL_TIMEOUT=15s；随 tunConfig.dialMs 热传入——评审 r2-L5）。
    dial_timeout: Mutex<Duration>,
    inner: Mutex<HostInner>,
    stopped: Arc<AtomicBool>,
    auth_drops: AtomicU64,
    /// accept/pump 线程的收尾观测面（stop 有界等待用；工单②：stop 不再只发信号
    /// 就走——pump 线程可能正持着 gate 票在收尾，等一下让「下一次 start 不撞旧锁」）。
    live: Mutex<usize>,
    live_cv: Condvar,
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
    pub fn new(what: &'static str, dir: Option<PathBuf>, logf: Logf, dial_port: DialFn) -> Self {
        BridgeHost {
            what,
            dir,
            logf,
            dial_port: Mutex::new(dial_port),
            dial_timeout: Mutex::new(DIAL_TIMEOUT),
            inner: Mutex::new(HostInner {
                socks: Vec::new(),
                token: None,
                files_gate: Arc::new(ConnGate::new(8)),
                term_gate: Arc::new(ConnGate::new(16)),
                speed_gate: Arc::new(ConnGate::new(10)),
            }),
            stopped: Arc::new(AtomicBool::new(false)),
            auth_drops: AtomicU64::new(0),
            live: Mutex::new(0),
            live_cv: Condvar::new(),
        }
    }

    fn live_enter(&self) {
        let mut n = self.live.lock().unwrap_or_else(|e| e.into_inner());
        *n += 1;
    }

    /// 换拨号面（服务桥形态：构造时给占位，会话建好后换真拨号；幂等替换）。
    /// F4：原子换 `Arc`（在途拨号持旧 Arc 跑完，换轨不阻塞、不半换）。
    pub fn set_dial(&self, f: DialFn) {
        *lock_host(&self.dial_port) = f;
    }

    /// 换拨号预算（隧道域随 tunConfig.dialMs 传入——Go tunRunner.dialTimeout 同义；
    /// 评审 r2-L5：dial_ms 此前是死字段）。
    pub fn set_dial_timeout(&self, d: Duration) {
        *lock_host(&self.dial_timeout) = d.max(Duration::from_millis(500));
    }

    fn live_leave(&self) {
        let mut n = self.live.lock().unwrap_or_else(|e| e.into_inner());
        *n = n.saturating_sub(1);
        self.live_cv.notify_all();
    }

    /// 起桥（幂等；非阻塞——listen 重试在后台线程）。任一座桥起不来只记状态不
    /// 阻断宿主（桥是附加能力，不是数据面本体）。
    pub fn start(self: &Arc<Self>) {
        let mut inner = lock_host(&self.inner);
        if inner.token.is_some() || self.stopped.load(Ordering::Acquire) {
            return; // 已启动 / 已停止
        }
        let Some(dir) = self.dir.clone() else {
            (self.logf)(&format!(
                "{}: 没有桥目录（identityDir 未配置）—— files/term/测速 本轮不可用",
                self.what
            ));
            return;
        };
        let bridge_dir = dir.join(BRIDGE_DIR);
        if let Err(e) = std::fs::create_dir_all(&bridge_dir) {
            (self.logf)(&format!(
                "{}: 建桥目录失败（{e}）—— files/term/测速 本轮不可用",
                self.what
            ));
            return;
        }
        // sun_path 上限预检：路径超长的 bind 报 invalid argument 且重试无意义。
        // 测速桥路径比 files 长 4 字节——极端下「files 能用、测速超长」只跳过测速
        // 自己，不连累 files/term。
        let files = bridge_socket_path(&dir, "files");
        if files.as_os_str().len() > MAX_UNIX_SOCKET_PATH {
            (self.logf)(&format!(
                "{}: 桥路径超长（{} > {MAX_UNIX_SOCKET_PATH} 字节，sun_path 上限〔平台值〕）—— files/term 本轮不可用；把 filesDir 挪短或反馈",
                self.what,
                files.as_os_str().len()
            ));
            return;
        }
        let mut socks = vec![
            BridgeSock {
                name: "files-bridge",
                port: port::FILES,
                path: files,
                own: None,
                unavailable: false,
            },
            BridgeSock {
                name: "term-bridge",
                port: term_port(),
                path: bridge_socket_path(&dir, "term"),
                own: None,
                unavailable: false,
            },
        ];
        let speed_path = bridge_socket_path(&dir, "speedtest");
        if speed_path.as_os_str().len() > MAX_UNIX_SOCKET_PATH {
            (self.logf)(&format!(
                "{}: 测速桥路径超长（{} > {MAX_UNIX_SOCKET_PATH} 字节，sun_path 上限〔平台值〕）—— 测速本轮不可用（files/term 不受影响）",
                self.what,
                speed_path.as_os_str().len()
            ));
        } else {
            socks.push(BridgeSock {
                name: "speed-bridge",
                port: port::SPEEDTEST,
                path: speed_path,
                own: None,
                unavailable: false,
            });
        }
        // 令牌（32B 随机；生成失败宁可不 exposing 桥——鉴权是硬要求，不做明文回退）
        let mut tok = [0u8; 32];
        if getrandom_fill(&mut tok).is_err() {
            (self.logf)(&format!(
                "{}: 生成桥令牌失败 —— files/term/测速 本轮不可用",
                self.what
            ));
            return;
        }
        inner.socks = socks;
        inner.token = Some(tok);
        drop(inner);

        // 每座桥一个 accept 线程（一座端口被占重试不拖另一座）
        for idx in 0..3 {
            let host = Arc::clone(self);
            let spawned = std::thread::Builder::new()
                .name("hw-bridge-accept".into())
                .spawn(move || host.accept_loop(idx));
            if let Err(e) = spawned {
                // F7-4①：置位点在 spawn 点（accept_loop 在 spawn 失败时根本不会跑）
                let (name, logf) = {
                    let mut inner = lock_host(&self.inner);
                    let name = inner.socks.get(idx).map(|s| s.name).unwrap_or("bridge");
                    if let Some(s) = inner.socks.get_mut(idx) {
                        s.unavailable = true;
                    }
                    (name, Arc::clone(&self.logf))
                };
                crate::syncutil::log_spawn_failed(
                    &logf,
                    &format!("{name} accept 线程"),
                    &e,
                    "该桥本轮不可用",
                );
            }
        }
        (self.logf)(&format!("{}: 桥宿主启动（三座桥后台就位中）", self.what));
    }

    /// 一座桥的 accept 循环（非阻塞 accept + 100ms 节拍轮询 stopped——桥连接非
    /// 热路径，粗粒度足够；stop 位置起后线程退出、fd 由 drop 关）。
    /// 工单②：listen 重试**不持宿主锁**（信息交接式短临界区）；每条连接的处理
    /// spawn 出去（鉴权 5s + 经会话拨号 15s 不得卡 accept 面）。
    fn accept_loop(self: Arc<Self>, idx: usize) {
        self.live_enter();
        let (name, port, path) = {
            let inner = lock_host(&self.inner);
            match inner.socks.get(idx) {
                Some(s) => (s.name, s.port, s.path.clone()),
                None => return self.live_leave(),
            }
        };
        let ln =
            BridgeSock::listen_path(name, &path, &self.stopped, &self.logf).map(|(ln, own)| {
                let mut inner = lock_host(&self.inner);
                if let Some(s) = inner.socks.get_mut(idx) {
                    s.own = own;
                }
                ln
            });
        let Some(ln) = ln else {
            // F7-4②：listen 重试耗尽 ⇒ 置位（status 面返回空路径——不再谎报可用）
            {
                let mut inner = lock_host(&self.inner);
                if let Some(s) = inner.socks.get_mut(idx) {
                    s.unavailable = true;
                }
            }
            return self.live_leave();
        };

        while !self.stopped.load(Ordering::Acquire) {
            match ln.accept() {
                Ok((conn, _)) => {
                    conn.set_nonblocking(false).ok();
                    let host = Arc::clone(&self);
                    // F7-1：conn 线程 spawn 失败不静默（只记行——该连接收口，监听面照常）
                    if let Err(e) = std::thread::Builder::new()
                        .name("hw-bridge-conn".into())
                        .spawn(move || host.handle_conn(name, port, conn))
                    {
                        (self.logf)(&format!(
                            "{name}: 连接处理线程启动失败（{e}）—— 该连接收口（监听面不受影响）"
                        ));
                    }
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(_) => break, // 监听器被关（收工）
            }
        }
        self.live_leave();
    }

    /// 一条桥接连接：闸 → 鉴权 → 经会话拨出口 → 双向泵（**在自己的线程上**——
    /// accept 线程只接连接；鉴权 5s / 拨号 15s 都不卡它）。泵线程不进 live 计数：
    /// 在途连接随会话关闭自然断（Go bridge.stop 同形态——只关监听，不 join 泵）。
    fn handle_conn(&self, name: &'static str, port: u16, mut conn: UnixStream) {
        self.live_enter();
        let (gate, token) = {
            let inner = lock_host(&self.inner);
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
            self.live_leave();
            return;
        }
        // 预算与拨号面都**克隆出锁**再拨（F4：短临界区；拨号本身最长 15s，不得持锁）
        let budget = *lock_host(&self.dial_timeout);
        let dial = Arc::clone(&lock_host(&self.dial_port));
        let r = dial(port, budget);
        match r {
            Ok(remote) => {
                // 双向泵（两条线程；任一方向断即整体收口；守卫随最后一泵结束销票）。
                // 拆半失败不 panic（工单②——fd 耗尽/引擎收工形态按连接收口处理）。
                let halves = remote.into_halves();
                let local_r = conn.try_clone().ok();
                match (halves, local_r) {
                    (Ok((mut remote_r, mut remote_w)), Some(mut local_r)) => {
                        let mut local_w = conn;
                        let guard = Arc::new(guard);
                        let g1 = Arc::clone(&guard);
                        let g2 = Arc::clone(&guard);
                        // 泵收口观测（R8-8b 转正式）：桥泵此前静默收口——上行 bulk 断流
                        // 排障时无迹可循；累计字节 + 收口原因一行（有流量才打——
                        // 空闲连接的常规收口零噪音）。
                        let logf1 = Arc::clone(&self.logf);
                        let logf2 = Arc::clone(&self.logf);
                        drop(guard);
                        // F7-1：泵线程 spawn 失败不静默——该方向收口（对侧泵照常，
                        // 连接随会话关闭自然断）
                        if let Err(e) = std::thread::Builder::new()
                            .name("hw-bridge-pump".into())
                            .spawn(move || {
                                let _g = g1;
                                pump(&mut local_r, &mut *remote_w, &logf1, "桥泵", "up", true);
                            })
                        {
                            (self.logf)(&format!(
                                "{name}: 泵线程（up）启动失败（{e}）—— 该方向收口"
                            ));
                        }
                        if let Err(e) = std::thread::Builder::new()
                            .name("hw-bridge-pump".into())
                            .spawn(move || {
                                let _g = g2;
                                pump(&mut *remote_r, &mut local_w, &logf2, "桥泵", "down", true);
                            })
                        {
                            (self.logf)(&format!(
                                "{name}: 泵线程（down）启动失败（{e}）—— 该方向收口"
                            ));
                        }
                    }
                    (Err(_), _) => {
                        (self.logf)(&format!("{name}: 桥接流拆半失败——连接收口"));
                    }
                    (_, None) => {
                        (self.logf)(&format!("{name}: 本地 fd 复制失败——连接收口"));
                    }
                }
            }
            Err(e) => {
                (self.logf)(&format!("{name}: 经会话拨出口 {port} 失败: {e}"));
                // 按错误种类分流（Go 评审 r2-N2 同义）：
                // - refused 类 = 出口活着、该端口没服务 ⇒ 不回帧直接关——客户端
                //   请求后零字节 EOF ⇒ not_supported（「请升级出口」）；
                // - 其余（speed 桥）= 出口不在/正在恢复 ⇒ 回 report{link_down} 再
                //   有序收口（工单④：页面回等待循环自动续跑）。回帧在桥鉴权之后，
                //   不向未鉴权探测者泄任何信息。
                if name == "speed-bridge" && !is_refused_like(&e) {
                    speed_link_down_reply(&mut conn);
                }
            }
        }
        self.live_leave();
    }

    /// 收工（幂等）：关监听器，已建立的桥接连接随会话关闭自然断。令牌清零——状态
    /// 面此后读到空 bridgeAuth，消费方自然失败。
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        let socks = {
            let mut inner = lock_host(&self.inner);
            inner.token = None;
            std::mem::take(&mut inner.socks)
        };
        for s in socks {
            remove_sock_own(&s.path, s.own);
        }
        // accept/conn 线程的有界收口（工单②：在途 handle_conn 持着 gate 票在
        // 鉴权/拨号——等一下让「下一次 start 不撞旧票/旧监听」；泵线程不等待，
        // 在途连接随会话关闭自然断——Go bridge.stop 同形态）。
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut n = lock_host(&self.live);
        while *n > 0 {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let (g, _to) = self
                .live_cv
                .wait_timeout(n, left)
                .unwrap_or_else(|e| e.into_inner());
            n = g;
        }
        (self.logf)(&format!("{}: 已停止监听", self.what));
    }

    /// 当前三座桥的状态（socket 路径随令牌在场；桥未起/已停全空串）。
    pub fn status(&self) -> BridgeStatus {
        let inner = lock_host(&self.inner);
        let Some(tok) = inner.token else {
            return BridgeStatus::default();
        };
        // F7b：该桥本轮不可用（accept 起不来 / listen 耗尽）⇒ 空路径（不谎报）
        let pick = |name: &str| {
            inner
                .socks
                .iter()
                .find(|s| s.path.ends_with(format!("{name}.sock")))
                .filter(|s| !s.unavailable)
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

/// 单向泵：读尽即关对侧写端（EOF 传播——半关闭语义，FIN 穿透）；错误亦收口。
/// 两侧为 trait 对象（工单⑤：远端是会话流/本机 UDS 的统一拆半面；桥连接非
/// 热路径，dyn 派发开销可忽略）。
///
/// Q-F-B F2-4：`label` = 行文前缀（桥 = "桥泵"，旧行文逐字不变；portfwd =
/// "port-forward"），`eof_log` = 读尽（EOF）时是否记行——桥要（排障面），portfwd
/// 不记（对齐 Go `pkg/netpipe`：只对**非 EOF** 错误记行，浏览器关连接是常态）。
pub(crate) fn pump(
    r: &mut dyn Read,
    w: &mut dyn WriteHalf,
    logf: &crate::Logf,
    label: &'static str,
    dir: &'static str,
    eof_log: bool,
) {
    let mut buf = [0u8; 16 * 1024];
    let mut dbg_n = 0u64;
    let mut dbg_ok = 0u64;
    loop {
        match r.read(&mut buf) {
            Ok(0) => {
                if eof_log && dbg_n > 0 {
                    (logf)(&format!("{label}[{dir}] EOF（累计 {dbg_n}B / {dbg_ok} 次）"));
                }
                break;
            }
            Err(_) => {
                // 门槛：桥侧保持既有「有流量才记」（旧行文逐字不变）；portfwd 侧
                // （`eof_log=false`）**无条件记行**——对齐 Go `pkg/netpipe` 的错误口径
                //（`logf("pipe %v: %v after %dB")`，n 可为 0；零字节 RST 也要有迹可循）。
                if !eof_log || dbg_n > 0 {
                    (logf)(&format!(
                        "{label}[{dir}] 读错误（累计 {dbg_n}B / {dbg_ok} 次）"
                    ));
                }
                break;
            }
            Ok(n) => {
                dbg_n += n as u64;
                if let Err(e) = w.write_all(&buf[..n]) {
                    (logf)(&format!("{label}[{dir}] 写失败 after {dbg_n}B：{e}"));
                    break;
                }
                dbg_ok += 1;
            }
        }
    }
    w.close_write();
}

/// getrandom 填充（32B 令牌；不引直接依赖——getrandom crate 已在依赖面）。
fn getrandom_fill(buf: &mut [u8]) -> Result<(), ()> {
    getrandom::getrandom(buf).map_err(|_| ())
}

/// 客户端侧：连上桥后先发鉴权首包（`write_auth` 的再导出——同链分发的客户端面）。
pub fn bridge_client_auth<W: Write>(w: &mut W, auth_hex: &str) -> std::io::Result<()> {
    write_auth(w, auth_hex)
}

/// 拨出口失败的 refused 类判定（Go wgcore.IsRefusedLike：连接被拒/不可达等
/// 「对端在网络意义上明确回答了」的形态 = 出口活着、端口没服务）。
/// **只认 ErrorKind**（评审 r2-M9：隧道内拨号的 Refused 在 healing_dial 已映射成
/// `ErrorKind::ConnectionRefused` 带过接缝——不再字符串嗅探）。
fn is_refused_like(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::ConnectionReset
    )
}

/// speed 桥的 link_down 拒绝回帧（Go speedLinkDownReply / pkg/speedtest.ReplyThenClose）：
/// 有界读掉客户端已写出的请求帧（2s）→ 回 report{link_down} → 半关写端 → 短窗吞输入
/// → 关。直接回帧后关会走 RST 路径、已发出的 report 可能被对端丢弃。
fn speed_link_down_reply(conn: &mut UnixStream) {
    use std::io::Read as _;
    conn.set_read_timeout(Some(Duration::from_secs(2))).ok();
    // 有界吞一帧（15B 头 + ≤64KB 载荷；失败也继续回帧——连接本就异常，尽力而为）
    let mut sink = [0u8; 128 * 1024];
    let _ = conn.read(&mut sink);
    let frame = crate::speedtest::make_report_frame("{\"error\":\"link_down\"}");
    let _ = conn.write_all(&frame);
    let _ = conn.flush();
    let _ = conn.shutdown(std::net::Shutdown::Write); // FIN 先于任何复位
    conn.set_read_timeout(Some(Duration::from_millis(300))).ok();
    let _ = conn.read(&mut sink); // 短窗吞掉后续输入（role=send 的泵送数据）
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
        let host = Arc::new(BridgeHost::new(
            "测试桥",
            Some(dir.clone()),
            logf,
            Arc::new(|_, _| Err(std::io::Error::new(ErrorKind::ConnectionRefused, "无会话"))),
        ));
        host.start();
        // 等 listen 就位（auth_hex 在 start 同步段就有；socket 文件在后台线程落盘）
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let files_path = bridge_socket_path(&dir, "files");
        while (!Path::new(&files_path).exists()) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            Path::new(&files_path).exists(),
            "files 桥应在 2s 内监听就位"
        );
        let st = host.status();
        assert_eq!(st.auth_hex.len(), 96, "hex(16B 魔数 + 32B 令牌) = 96 hex");
        assert!(st.files_sock.ends_with("bridge/files.sock"));
        assert!(st.term_sock.ends_with("bridge/term.sock"));
        assert!(st.speed_sock.ends_with("bridge/speedtest.sock"));
        let magic_hex: String = BRIDGE_AUTH_MAGIC
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert!(
            st.auth_hex.starts_with(&magic_hex),
            "魔数 hex 前缀（TIERBRIDGEAUTH01）"
        );
        // socket 文件在盘上
        assert!(Path::new(&st.files_sock).exists());
        host.stop();
        let st = host.status();
        assert_eq!(st, BridgeStatus::default(), "stop 后状态面全空");
        assert!(
            !Path::new(&host.status().files_sock).exists() || host.status().files_sock.is_empty()
        );
    }

    /// 鉴权链：坏魔数/坏令牌 ⇒ 服务端静默断开（客户端读到 EOF）；好令牌 ⇒ 放行进泵
    /// （dial 失败也断——本测试 dial 恒败，但**在鉴权之后**才断）。
    #[test]
    fn auth_chain() {
        let dir = tmp_dir("auth");
        let (logf, _lines) = log_silent();
        let (tx, rx) = mpsc::channel::<u16>();
        let rx = Arc::new(Mutex::new(rx));
        let host = Arc::new(BridgeHost::new(
            "测试桥",
            Some(dir),
            logf,
            Arc::new(move |port, _| {
                tx.send(port).unwrap();
                Err(std::io::Error::new(ErrorKind::ConnectionRefused, "拒"))
            }),
        ));
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
        assert_eq!(
            seen_port.unwrap(),
            7724,
            "鉴权过 ⇒ 泵起 ⇒ 经会话拨 term 端口"
        );
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
        assert!(
            rx.lock().unwrap().try_recv().is_err(),
            "坏令牌不得触发 dial"
        );
        host.stop();
    }

    fn a2_from(auth: &str) -> String {
        auth.to_owned()
    }

    /// 无目录形态（dir=None）：start 即日志 + 不起桥。
    #[test]
    fn no_dir_no_bridge() {
        let (logf, lines) = log_silent();
        let host = Arc::new(BridgeHost::new(
            "测试桥",
            None,
            logf,
            Arc::new(|_, _| unreachable!()),
        ));
        host.start();
        assert_eq!(host.status(), BridgeStatus::default());
        assert!(lines
            .lock()
            .unwrap()
            .iter()
            .any(|l| l.contains("没有桥目录")));
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
        let host = Arc::new(BridgeHost::new(
            "测试桥",
            Some(dir),
            logf,
            Arc::new(|_, _| Err(std::io::Error::new(ErrorKind::ConnectionRefused, "无"))),
        ));
        host.start();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while host.status().auth_hex.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!host.status().auth_hex.is_empty(), "死残留被清理、桥就位");
        host.stop();
    }

    /// connect_budget（M-5 真非阻塞）：死 socket 路径 = 即时 ConnectionRefused；
    /// 超长路径 = InvalidInput；活监听 = 连得上。
    #[test]
    fn connect_budget_immediate_results() {
        let dir = tmp_dir("cb");
        std::fs::create_dir_all(dir.join("bridge")).unwrap();
        // 活监听
        let live = dir.join("bridge").join("live.sock");
        let ln = UnixListener::bind(&live).unwrap();
        let c = connect_budget(&live, Duration::from_secs(1)).unwrap();
        drop(c);
        // 死残留（listener 已关）= ECONNREFUSED 即时回（非超时）
        let dead = dir.join("bridge").join("dead.sock");
        UnixListener::bind(&dead).unwrap();
        drop(std::fs::read_dir(&dir).unwrap().next());
        drop(ln);
        // 等 listener 真正关闭后拨：ECONNREFUSED
        let mut got_refused = false;
        for _ in 0..20 {
            if let Err(e) = connect_budget(&dead, Duration::from_secs(1)) {
                if e.kind() == ErrorKind::ConnectionRefused {
                    got_refused = true;
                    break;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(got_refused, "死残留应即时 ConnectionRefused");
        // 不存在 = NotFound
        let e = connect_budget(
            &dir.join("bridge").join("none.sock"),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert_eq!(e.kind(), ErrorKind::NotFound);
        // 超长路径 = InvalidInput（sun_path 上限）
        let long = dir.join("x".repeat(200));
        let e = connect_budget(&long, Duration::from_secs(1)).unwrap_err();
        assert_eq!(e.kind(), ErrorKind::InvalidInput);
    }

    #[test]
    fn path_helpers() {
        let p = bridge_socket_path(Path::new("/data/f"), "files");
        assert_eq!(p, Path::new("/data/f/bridge/files.sock"));
        assert_eq!(term_port(), 7724);
    }

    /// F4：dial 出锁——A 阻塞在拨号体内时，B **已能进入**拨号（旧形态持锁跨拨号 ⇒
    /// B 被挡在锁外）；且换拨号面（`set_dial`）不被在途拨号阻塞。
    #[test]
    fn dial_port_lock_not_held_during_dial() {
        let dir = tmp_dir("dialout");
        let (logf, _lines) = log_silent();
        let entered = Arc::new(AtomicU64::new(0));
        let e2 = Arc::clone(&entered);
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let rx2 = Arc::clone(&release_rx);
        let host = Arc::new(BridgeHost::new(
            "测试桥",
            Some(dir),
            logf,
            Arc::new(move |_p, _b| {
                let n = e2.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    // 第一发「拨号」阻塞（模拟 15s 拨号在途）
                    let _ = rx2.lock().unwrap().recv();
                }
                Err(std::io::Error::new(ErrorKind::ConnectionRefused, "拒"))
            }),
        ));
        let dial_of = |h: &Arc<BridgeHost>| Arc::clone(&lock_host(&h.dial_port));
        let a = {
            let d = dial_of(&host);
            std::thread::spawn(move || d(7802, Duration::from_secs(5)))
        };
        // 等 A 进入拨号
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while entered.load(Ordering::SeqCst) == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(entered.load(Ordering::SeqCst), 1, "A 已进入拨号");
        // B：A 未返回时进入拨号（出锁拨号的直接证据）
        let b = {
            let d = dial_of(&host);
            std::thread::spawn(move || d(7724, Duration::from_secs(5)))
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while entered.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            entered.load(Ordering::SeqCst),
            2,
            "B 必须在 A 未返回时就进入拨号（锁不得跨拨号持有）"
        );
        // 换拨号面：不得被在途拨号阻塞（Arc 原子替换）
        let t0 = std::time::Instant::now();
        host.set_dial(Arc::new(|_, _| {
            Err(std::io::Error::new(ErrorKind::ConnectionRefused, "换面后"))
        }));
        assert!(
            t0.elapsed() < Duration::from_millis(300),
            "set_dial 不得等在途拨号（实耗 {:?}）",
            t0.elapsed()
        );
        let _ = release_tx.send(());
        assert!(a.join().unwrap().is_err());
        assert!(b.join().unwrap().is_err());
    }

    /// F7b：accept 起不来 / listen 耗尽 ⇒ `unavailable` 置位 ⇒ `status()` 对该桥返回
    /// **空路径**（不再谎报可用）；`auth_hex` 不变。
    #[test]
    fn unavailable_bridge_reports_empty_path() {
        let dir = tmp_dir("unavail");
        let (logf, _lines) = log_silent();
        let host = Arc::new(BridgeHost::new(
            "测试桥",
            Some(dir.clone()),
            logf,
            Arc::new(|_, _| Err(std::io::Error::new(ErrorKind::ConnectionRefused, "无会话"))),
        ));
        host.start();
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while host.status().files_sock.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        let before = host.status();
        assert!(!before.files_sock.is_empty(), "前置：正常态有路径");
        assert_eq!(before.auth_hex.len(), 96);
        // 直置位（F7-4 置位点在生产路径 = spawn 点 / listen 结论点）
        {
            let mut inner = lock_host(&host.inner);
            let s = inner
                .socks
                .iter_mut()
                .find(|s| s.name == "files-bridge")
                .expect("files 桥在册");
            s.unavailable = true;
        }
        let after = host.status();
        assert_eq!(after.files_sock, "", "不可用 ⇒ 空路径（不谎报）");
        assert_eq!(after.auth_hex, before.auth_hex, "auth_hex 不变");
        assert!(!after.term_sock.is_empty(), "其它桥不受影响");
        host.stop();
    }

    // ---- Q-F-B F2-4：`pump` 的 label/eof_log 参数化（桥侧行文逐字不变；pf 侧 EOF 零日志）----

    /// 假读端（按脚本给数据/EOF/错误）；假写端（记录写入 + `close_write` 被调）。
    struct MockRead {
        chunks: std::collections::VecDeque<Vec<u8>>,
        err: Option<std::io::ErrorKind>,
    }
    impl Read for MockRead {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            // 有数据先给数据（错误只在脚本耗尽后生效——pump 的「零字节不记行」口径
            // 需要真有流量才判读错误行）
            if let Some(c) = self.chunks.pop_front() {
                let n = c.len().min(out.len());
                out[..n].copy_from_slice(&c[..n]);
                return Ok(n);
            }
            match self.err.take() {
                Some(k) => Err(std::io::Error::from(k)),
                None => Ok(0),
            }
        }
    }
    struct MockWrite {
        closed: Arc<AtomicBool>,
        written: Vec<u8>,
    }
    impl Write for MockWrite {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    impl WriteHalf for MockWrite {
        fn close_write(&mut self) {
            self.closed.store(true, Ordering::SeqCst);
        }
    }

    /// 桥形（label="桥泵"、eof_log=true）：有流量时 EOF 记行（**旧行文逐字不变**）。
    #[test]
    fn pump_bridge_label_and_eof_line() {
        let (logf, lines) = log_silent();
        let mut r = MockRead {
            chunks: std::collections::VecDeque::from(vec![b"abc".to_vec()]),
            err: None,
        };
        let closed = Arc::new(AtomicBool::new(false));
        let mut w = MockWrite {
            closed: Arc::clone(&closed),
            written: Vec::new(),
        };
        pump(&mut r, &mut w, &logf, "桥泵", "up", true);
        assert_eq!(w.written, b"abc");
        assert!(closed.load(Ordering::SeqCst), "EOF ⇒ 关对侧写端（FIN 传播）");
        assert!(
            lines.lock().unwrap().iter().any(|l| l == "桥泵[up] EOF（累计 3B / 1 次）"),
            "桥侧 EOF 行文逐字不变：{:?}",
            lines.lock().unwrap()
        );
    }

    /// pf 形（label="port-forward"、eof_log=false）：**EOF 零日志**（对齐 Go netpipe——
    /// 浏览器关连接是常态）；读错误仍记行且带 port-forward 标签。
    #[test]
    fn pump_pf_label_eof_silent_errors_logged() {
        let (logf, lines) = log_silent();
        // 有流量 + EOF：零日志
        let mut r = MockRead {
            chunks: std::collections::VecDeque::from(vec![b"xyz".to_vec()]),
            err: None,
        };
        let closed = Arc::new(AtomicBool::new(false));
        let mut w = MockWrite {
            closed: Arc::clone(&closed),
            written: Vec::new(),
        };
        pump(&mut r, &mut w, &logf, "port-forward", "down", false);
        assert_eq!(w.written, b"xyz");
        assert!(closed.load(Ordering::SeqCst));
        assert!(lines.lock().unwrap().is_empty(), "pf 侧 EOF 零日志：{:?}", lines.lock().unwrap());
        // 读错误：记行（带 label）
        let mut r2 = MockRead {
            chunks: std::collections::VecDeque::from(vec![b"q".to_vec()]),
            err: Some(ErrorKind::ConnectionReset),
        };
        let mut w2 = MockWrite {
            closed: Arc::new(AtomicBool::new(false)),
            written: Vec::new(),
        };
        pump(&mut r2, &mut w2, &logf, "port-forward", "down", false);
        assert!(
            lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("port-forward[down] 读错误（累计 1B / 1 次）")),
            "{:?}",
            lines.lock().unwrap()
        );
        // 零字节读错误：pf 侧**无条件记行**（Go netpipe 口径；桥侧保留旧门槛）
        lines.lock().unwrap().clear();
        let mut r3 = MockRead {
            chunks: std::collections::VecDeque::new(),
            err: Some(ErrorKind::ConnectionReset),
        };
        let mut w3 = MockWrite {
            closed: Arc::new(AtomicBool::new(false)),
            written: Vec::new(),
        };
        pump(&mut r3, &mut w3, &logf, "port-forward", "up", false);
        assert!(
            lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.starts_with("port-forward[up] 读错误（累计 0B / 0 次）")),
            "零字节错误也要有迹可循：{:?}",
            lines.lock().unwrap()
        );
        // 桥侧同形态：保留旧门槛（零字节不记行——行文逐字不变）
        lines.lock().unwrap().clear();
        let mut r4 = MockRead {
            chunks: std::collections::VecDeque::new(),
            err: Some(ErrorKind::ConnectionReset),
        };
        let mut w4 = MockWrite {
            closed: Arc::new(AtomicBool::new(false)),
            written: Vec::new(),
        };
        pump(&mut r4, &mut w4, &logf, "桥泵", "up", true);
        assert!(lines.lock().unwrap().is_empty(), "桥侧旧门槛不变");
    }
}
