//! 承载面管理器束（语义真源 `baseline:clientcore/facade/carriers.go`——D 批，
//! B0-2b §十裁剪的整块兑现）。
//!
//! forward / socks / speedtest 三个管理器的装配点与协调面：随 DaemonCore 生命周期
//! （stateDir = client 角色目录，与 hosts.json 同源）；「监听端口全局唯一」（跨
//! forward 规则与 socks 监听——多主机会话并发在世、共享同一回环命名空间）的检查
//! 在本层做（两管理器各自只查名下端口）；host.remove 级联清理经本层分发
//! （forward = delete 语义不强关、socks = off 语义显式关、speedtest = cancel）。
//! 拨号一律经注入的拨号缝（同记账同重建感知），无旁路直拨。
//!
//! 与 Go 的锁序拍板（FIX-05）：`add_mu` 串行「全局端口检查 + 成员检查 + 落地」与
//! 「删条目 → 级联」（DaemonCore::remove_host 先取 add_mu 再动表）——级联要么看到
//! 规则并删掉，要么规则因成员检查失败而根本落不了地；锁序恒为
//! `add_mu → hosts.inner`（表侧从不在持自己的锁时反向取 add_mu）。

pub mod dnsq;
pub mod forward;
pub mod socksmgr;
pub mod socks_srv;
pub mod speedrun;
#[cfg(test)]
pub(super) mod testutil;

use std::net::SocketAddrV4;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::StreamConn;
pub use forward::{describe_target, ForwardRule, ForwardState};
pub use socksmgr::{SocksState, SOCKS_DEFAULT_LISTEN};
pub use speedrun::{SpeedtestOutcome, SpeedtestParams, SpeedtestStatus};

/// 拨号缝闭包形（出口本机端口形态；`Duration` = 本次拨号预算——socks 的候选
/// 份额与 DNS 解析腿的 5s 都经此传导，中-1/中-8）。
pub type DialPortFn =
    Arc<dyn Fn(&str, u16, Duration) -> Result<CarrierConn, DialErr> + Send + Sync>;
/// 拨号缝闭包形（任意目标形态；`Duration` = 本次拨号预算）。
pub type DialAddrFn =
    Arc<dyn Fn(&str, SocketAddrV4, Duration) -> Result<CarrierConn, DialErr> + Send + Sync>;

/// 承载面统一拨号缝（Go carrierDial）：host = peerID hex；出口本机端口 / 任意目标
/// 两形态。同一连接的两个消费面（`.io` = StreamConn 透传/解析腿；`.speed` = 测速
/// 引擎腿）。拨号失败的四态归因（`DialErr`）供 speedtest runner 分类与 forward/socks
/// 的日志归因。
pub struct CarrierDial {
    pub dial_port: DialPortFn,
    pub dial: DialAddrFn,
}

/// 拨号缝产物：同一隧道连接的两种承载面。
pub struct CarrierConn {
    /// 透传/解析腿（forward/socks/DNS 面）。
    pub io: Arc<dyn StreamConn>,
    /// 测速引擎腿（speedtest runner 面）。
    pub speed: Arc<dyn crate::speedtest::SpeedConn>,
}

/// 拨号缝错误四态（Go ErrNoHost / ErrSessionNotCurrent / wgnet.ErrRefused / 其它）。
/// Clone = 测试注入面（FakeDial 的失败注入快照）。
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum DialErr {
    #[error("主机不在表中")]
    NoHost,
    #[error("会话不在（收工/重建窗口）")]
    NoSession,
    /// 对端 RST（出口无该服务——runner 归 not_supported 的判据）。
    #[error("连接被拒（对端 RST）")]
    Refused,
    #[error("{0}")]
    Other(String),
}

/// 承载面哨兵错误族（server 层统一落 bad_request/no_host + 归因 detail——Go
/// mapCarrierErr 的 FIX-50 口径：底层可行动归因随行不被吞）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CarrierErr {
    #[error("主机不在表中")]
    NoHost,
    #[error("端口须在 1024–65535：{0}")]
    PortRange(u16),
    #[error("监听端口已被占用（全局唯一）：{port} 已被 {owner} 占用（可用 --listen 另选）")]
    PortTaken { port: u16, owner: String },
    #[error("每主机 forward 规则上限 8 条（{0} 已 {1} 条）")]
    TooManyRules(String, usize),
    #[error("目标须为空（出口自己）或 IPv4 字面量：{0}")]
    BadTarget(String),
    #[error("forward 规则不存在：{0}")]
    NoRule(String),
    #[error("监听 127.0.0.1:{0} 失败（{1}）")]
    ListenFailed(u16, String),
    #[error("落盘失败：{0}")]
    Save(String),
}

/// 成员谓词（FIX-05：AddForward/SocksOn 的成员检查在 add_mu 临界区内跑）。
pub type HostExists = Arc<dyn Fn(&[u8; 32]) -> bool + Send + Sync>;

/// 承载面管理器束（DaemonCore 持有）。
pub struct Carriers {
    fwd: forward::ForwardManager,
    sks: socksmgr::SocksManager,
    spd: speedrun::SpeedtestManager,
    /// 「全局端口检查 + 成员检查 + 落地」与「删条目 → 级联」的串行化。
    add_mu: Mutex<()>,
    host_exists: HostExists,
}

impl Carriers {
    /// 打开三个管理器：读各自持久化文件、按表重建监听/运行面（半途失败收尾已开的，
    /// 不留半挂面——Go openCarriers 同序）。
    pub fn open(
        state_dir: &std::path::Path,
        dial: CarrierDial,
        logf: Arc<dyn Fn(&str) + Send + Sync>,
        warnf: Arc<dyn Fn(&str) + Send + Sync>,
        host_exists: HostExists,
    ) -> Result<Arc<Carriers>, String> {
        let fwd = forward::ForwardManager::open(state_dir, &dial, &logf, &warnf)?;
        let sks = match socksmgr::SocksManager::open(state_dir, &dial, &logf, &warnf) {
            Ok(m) => m,
            Err(e) => {
                fwd.close();
                return Err(e);
            }
        };
        let spd = speedrun::SpeedtestManager::new(&dial, &logf);
        Ok(Arc::new(Carriers { fwd, sks, spd, add_mu: Mutex::new(()), host_exists }))
    }

    /// 级联闸（FIX-05）：DaemonCore::remove_host 先取本锁再「删条目 → 级联」。
    pub fn cascade_lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.add_mu.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 成员检查（须在 add_mu 临界区内调用）。坏 hex = 不在表（Go peerIDFromHex 同义）。
    fn member_check(&self, host: &str) -> Result<(), CarrierErr> {
        match super::hosts::decode_peer_id_pub(host) {
            Some(id) if (self.host_exists)(&id) => Ok(()),
            _ => Err(CarrierErr::NoHost),
        }
    }

    /// 建转发规则（全局端口检查 + 委托 ForwardManager::add——当场监听失败 = 错误
    /// 返回、不入表）。Q-H F11：host 入口 canonical 化（大小写等价）。
    pub fn add_forward(&self, mut rule: ForwardRule) -> Result<(), CarrierErr> {
        let _g = self.add_mu.lock().unwrap_or_else(|e| e.into_inner());
        rule.host = canonical_host_hex(&rule.host);
        self.member_check(&rule.host)?;
        if let Some(owner) = self.fwd.port_owner(rule.listen) {
            return Err(CarrierErr::PortTaken { port: rule.listen, owner });
        }
        let port = rule.listen;
        if let Some(owner) = self.sks.port_owner(port) {
            return Err(CarrierErr::PortTaken { port, owner: format!("{owner}（可用 --listen 另选）") });
        }
        self.fwd.add(rule)
    }

    /// 删规则（不强关在世连接）。Q-H F11：host 入口 canonical 化。
    pub fn remove_forward(&self, host: &str, listen: u16) -> Result<(), CarrierErr> {
        self.fwd.remove(&canonical_host_hex(host), listen)
    }

    /// 规则表快照（host 空 = 全部；按 host/listen 稳定排序）。
    pub fn forward_states(&self, host: &str) -> Vec<ForwardState> {
        self.fwd.list(host)
    }

    /// 开 SOCKS 监听（listen 0 = 沿用记忆/缺省 1080）；全局端口检查后委托。
    /// 返回实际端口。Q-H F11：host 入口 canonical 化。
    pub fn socks_on(&self, host: &str, listen: u16) -> Result<u16, CarrierErr> {
        let _g = self.add_mu.lock().unwrap_or_else(|e| e.into_inner());
        let host = canonical_host_hex(host);
        self.member_check(&host)?;
        // socks×socks 的跨主机冲突（含记忆端口、文案含另选提示）由 SocksManager::on
        // 自带；这里只补 forward 侧的占用检查——按**解析后的端口**判（exec-r1 B1：
        // 此前 listen==0 时整个跳过，靠 On 恒落 1080 的〔错误〕假设兜着）。
        let port = if listen == 0 { self.sks.default_listen(&host) } else { listen };
        if port != 0 {
            if let Some(owner) = self.fwd.port_owner(port) {
                return Err(CarrierErr::PortTaken { port, owner });
            }
        }
        let _ = &self.fwd;
        self.sks.on(&host, listen)
    }

    /// 关监听（显式关在世连接；端口记忆保留）。返回记忆端口（CLI 文案数据源）。
    /// Q-H F11：host 入口 canonical 化。
    pub fn socks_off(&self, host: &str) -> Result<u16, CarrierErr> {
        self.sks.off(&canonical_host_hex(host))
    }

    /// socks 承载态快照（按 host 排序稳定输出）。
    pub fn socks_states(&self) -> Vec<SocksState> {
        self.sks.status()
    }

    /// speedtest 三面（per-host 单飞 + runner 状态机承载等待）。Q-H F11：host
    /// 入口 canonical 化（大小写等价；未知主机仍走各自 NoHost/无记录语义）。
    pub fn speedtest_start(&self, host: &str, p: SpeedtestParams) -> speedrun::SpeedtestAck {
        self.spd.start(&canonical_host_hex(host), p)
    }

    pub fn speedtest_status(&self, host: &str) -> Option<SpeedtestStatus> {
        self.spd.status(&canonical_host_hex(host))
    }

    pub fn speedtest_cancel(&self, host: &str) {
        self.spd.cancel(&canonical_host_hex(host))
    }

    /// host.remove 级联（**调用方须已持 add_mu**——cascade_lock）：forward 规则
    /// （delete 语义，不强关）、socks 监听（off 语义，显式 RST + 记忆消失）、
    /// speedtest（cancel）。
    pub fn remove_host_cascade_locked(&self, host_hex: &str) {
        self.fwd.remove_host(host_hex);
        self.sks.remove_host(host_hex);
        self.spd.cancel(host_hex);
    }

    /// 收工：forward 只关监听、socks 显式关在世连接、speedtest 取消全部在跑轮。
    pub fn close(&self) {
        self.fwd.close();
        self.sks.close();
        self.spd.close();
    }
}

// ---------- 局部共享件 ----------

/// 0600 原子写（tmp + rename；hosts.json 同口径）。目录缺失即建（0700）。
/// Q-G F4/A7：**创建即 0600**（`.mode`）+ handle 后 fchmod 归一——旧形态
/// 「默认权限建 → 静默 chmod」有 0644 短窗口且 chmod 失败无告警。
pub(super) fn save_json_atomic(path: &std::path::Path, body: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let dir = path.parent().unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("建目录 {}：{e}", dir.display()))?;
    let tmp = path.with_extension("json.tmp");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("建 {}：{e}", tmp.display()))?;
    // 失败**告警不阻断**（代码门②：与 A1–A5 的告警形态一致——硬失败会把
    // 「状态落盘成功」变成报错并留下 .tmp，行为变化不值得）
    if let Err(e) = f.set_permissions(std::fs::Permissions::from_mode(0o600)) {
        eprintln!("homeway: ⚠️ {} 收紧 0600 失败（{e}）——建议手工 chmod", tmp.display());
    }
    f.write_all(body).and_then(|_| f.write(b"\n")).map_err(|e| format!("写 {}：{e}", tmp.display()))?;
    drop(f);
    std::fs::rename(&tmp, path).map_err(|e| format!("rename {} → {}：{e}", tmp.display(), path.display()))
}

/// 监听失败的错误描述（bind 错误原文随行——占用/权限/不可用各形态自带 OS 文案）。
pub(super) fn describe_listen_err(port: u16, e: &std::io::Error) -> String {
    format!("bind 127.0.0.1:{port}：{e}")
}

/// peerID hex 的短形态（日志/错误文案用；Go shortHost 同串）。
///
/// **Q-H F6 口径**：Go `shortHost`（`baseline:clientcore/facade/forward.go:410-415`）
/// 是**字节**截断——对**非法输入**（非 ASCII）会产出非法 UTF-8 前缀；Rust 改字符
/// 截断后对非法输入返回整串。**「与 Go 同串」只对合法 hex（ASCII）成立**（逐字节
/// 不变）；非法输入面 Rust 取「不 panic + 整串」的自定义语义（消除外部可触发的
/// dispatcher panic 面）。
pub(super) fn short_host(host: &str) -> String {
    if host.chars().count() > 8 {
        format!("{}…", host.chars().take(8).collect::<String>())
    } else {
        host.to_owned()
    }
}

/// host 串 canonical 化（Q-H F11）：hex 解码成功 ⇒ 小写 canonical；失败 ⇒ 原样。
/// 承载面入口统一经此归一（**只规范化，不改错误分类**——add/on 的 NoHost 与
/// remove/off 的 NoRule 语义不变），保证「大写 hex 能过成员检查却存成大写入表」
/// 的级联漏删不再发生（`daemon/mod.rs` 的 remove_host 级联按小写查）。
fn canonical_host_hex(host: &str) -> String {
    match super::hosts::decode_peer_id_pub(host) {
        Some(id) => id.iter().map(|b| format!("{b:02x}")).collect(),
        None => host.to_owned(),
    }
}

/// 本地 TCP 连接 RST 收口——**单源上移到 `crate::sysfd`**（Q-F-B F2-3/D13：portfwd
/// 拨号失败面同用，不再有第四份私有副本）。本重导出保住本模块三处既有调用点零改动。
pub(super) use crate::sysfd::rst_close_tcp;

/// TCP 双向透传的半关闭单实现（Go pkg/netpipe.Both 同义：任一向 EOF 只收该向写端，
/// 两向都收工才关两端；RST 只属于失败路径，由调用方负责）。
///
/// `local` 的半关走 `TcpStream::shutdown(Write)`；`upstream` 的半关走
/// `StreamConn::shutdown_write`（不支持半关的实现退化为全关——同 Go「对端不支持
/// 则退化为 Close」）。
pub(super) fn pipe_half_close(
    logf: &Arc<dyn Fn(&str) + Send + Sync>,
    local: std::net::TcpStream,
    upstream: Arc<dyn StreamConn>,
) {
    let write_half = local.try_clone().expect("TcpStream clone 不可失败");
    let up2l = std::thread::Builder::new()
        .name("hw-pipe-l2u".to_owned())
        .spawn({
            let up = Arc::clone(&upstream);
            let logf = Arc::clone(logf);
            move || copy_local_to_up(local, up, &logf)
        })
        .expect("线程创建不可失败");
    // 本线程跑 upstream → local（读上游阻塞面 = StreamConn::read_chunk）。
    loop {
        match upstream.read_chunk() {
            Ok(v) if v.is_empty() => break,
            Ok(v) => {
                if write_all_tcp(&write_half, &v).is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    let _ = write_half.shutdown(std::net::Shutdown::Write);
    let _ = up2l.join();
    upstream.close();
    let _ = write_half.shutdown(std::net::Shutdown::Both);
}

/// local → upstream 单向拷贝（EOF/错误 = 收该向写端；错误带日志）。
fn copy_local_to_up(
    mut local: std::net::TcpStream,
    upstream: Arc<dyn StreamConn>,
    logf: &Arc<dyn Fn(&str) + Send + Sync>,
) {
    use std::io::Read as _;
    let mut buf = [0u8; 16 * 1024];
    loop {
        match local.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if write_progress_upstream(&upstream, &buf[..n]).is_err() {
                    logf("pipe: 上游写失败/停滞——收该向");
                    let _ = local.shutdown(std::net::Shutdown::Read);
                    upstream.close();
                    return;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                if e.kind() != std::io::ErrorKind::UnexpectedEof {
                    logf(&format!("pipe: 本地读失败（{e}）——收该向"));
                }
                break;
            }
        }
    }
    upstream.shutdown_write();
}

/// 上游整块写（30s 无进展预算在 `TunnelConn::write_chunk` 内部承载——此处薄封装）。
fn write_progress_upstream(
    upstream: &Arc<dyn StreamConn>,
    data: &[u8],
) -> std::io::Result<()> {
    upstream.write_chunk(data).map(|_| ())
}

/// TCP 整块写（部分写重试）。
pub(super) fn write_all_tcp(
    stream: &std::net::TcpStream,
    mut data: &[u8],
) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut stream = stream;
    while !data.is_empty() {
        match stream.write(data) {
            Ok(0) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WriteZero,
                    "本地写零接纳",
                ))
            }
            Ok(n) => data = &data[n..],
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// 阻塞 accept 的非阻塞轮询形态（50ms 节拍；listener 由本循环独占，关停 = 置
/// generation 失效 + drop listener）。
pub(super) struct PollListener {
    ln: Option<std::net::TcpListener>,
    /// accept 瞬态错误的有界线性退避（Go serveAcceptRetry*：一次瞬态错误即退 =
    /// 无人受理的僵尸监听；烧尽 = 上抛按监听失效收口）。
    backoff: u32,
    /// 【测试注入缝】下一次 accept 直返 Failed（不碰真 fd——见 socks_srv
    /// `inject_accept_failure`）。
    #[cfg(test)]
    fail_next: Option<String>,
}

pub(super) const ACCEPT_RETRY_MAX: u32 = 8;
pub(super) const ACCEPT_RETRY_STEP_MS: u64 = 50;

pub(super) enum PollAccept {
    Conn(std::net::TcpStream),
    /// 无待收（正常节拍）。
    Idle,
    /// 监听器被关（正常收口）。
    Closed,
    /// 瞬态错误烧尽（监听失效——按 failed/off 收口）。
    Failed(String),
}

impl PollListener {
    pub fn new(ln: std::net::TcpListener) -> PollListener {
        let _ = ln.set_nonblocking(true);
        PollListener {
            ln: Some(ln),
            backoff: 0,
            #[cfg(test)]
            fail_next: None,
        }
    }

    /// 【测试注入缝】下一次 `accept()` 直返 `Failed(msg)`（Q-H F4）。
    #[cfg(test)]
    pub(super) fn inject_accept_failure(&mut self, msg: &str) {
        self.fail_next = Some(msg.to_owned());
    }

    pub fn accept(&mut self) -> PollAccept {
        #[cfg(test)]
        if let Some(msg) = self.fail_next.take() {
            return PollAccept::Failed(msg);
        }
        let Some(ln) = self.ln.as_ref() else { return PollAccept::Closed };
        match ln.accept() {
            Ok((stream, _)) => {
                self.backoff = 0;
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_nodelay(true);
                PollAccept::Conn(stream)
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // 高-1 整改：无待收 = 50ms 节拍（非阻塞 accept 不空转；接入延迟
                // 0–50ms——与 Go 阻塞 accept 的零延迟差，登记 B0-2b §十六）。
                std::thread::sleep(std::time::Duration::from_millis(50));
                PollAccept::Idle
            }
            // fd/内存短缺族 = 瞬态（低-2 整改：与 Go transientAcceptError 同集——
            // 一次 EMFILE 不该把监听打死为 failed）。
            Err(e)
                if matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE) | Some(libc::ENOMEM))
                    || matches!(
                        e.kind(),
                        std::io::ErrorKind::ConnectionAborted
                            | std::io::ErrorKind::ConnectionReset
                            | std::io::ErrorKind::Interrupted
                    ) =>
            {
                // 瞬态：线性退避（烧尽 = 监听失效）。
                self.backoff += 1;
                if self.backoff > ACCEPT_RETRY_MAX {
                    return PollAccept::Failed(format!("连续失败（{} 次）：{e}", self.backoff));
                }
                std::thread::sleep(std::time::Duration::from_millis(
                    self.backoff as u64 * ACCEPT_RETRY_STEP_MS,
                ));
                PollAccept::Idle
            }
            Err(e) => PollAccept::Failed(format!("accept：{e}")),
        }
    }

    /// 关监听（幂等）。
    pub fn close(&mut self) {
        self.ln = None;
    }
}


#[cfg(test)]
mod tests {
    use super::testutil::{temp_dir, FakeDial};
    use super::*;

    #[test]
    fn short_host_char_safe() {
        // ASCII 两档（与 Go 逐字节同串）。
        assert_eq!(short_host("abc"), "abc");
        assert_eq!(short_host("12345678"), "12345678");
        assert_eq!(short_host("123456789"), "12345678…");
        assert_eq!(short_host("abcdef0123456789"), "abcdef01…");
        // 多字节：字节切片旧形态必 panic；字符截断 = 不 panic（≤8 字符原样）。
        assert_eq!(short_host("中文中文中文"), "中文中文中文"); // 6 字符 / 18 字节
        assert_eq!(short_host("字字字字"), "字字字字"); // 恰在字节 8 边界断裂的 3 字节字符
        assert_eq!(short_host("😀😀"), "😀😀"); // 4 字节 emoji
        assert_eq!(short_host("字字字字字字字字字"), "字字字字字字字字…");
        assert_eq!(short_host("😀😀😀😀😀😀😀😀😀"), "😀😀😀😀😀😀😀😀…");
    }

    fn hex_host(tag: u8) -> String {
        format!("{tag:0>64x}")
    }

    fn nop() -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(|_| {})
    }

    /// Q-H F11：大写 hex 全链（add/delete/on/off 命中 + remove_host 级联删净）——
    /// 入表 host 必须 canonical 小写（`remove_host` 级联按小写查）。
    #[test]
    fn uppercase_host_canonicalized_full_chain() {
        let dir = temp_dir("carr-canon");
        let dial = FakeDial::new();
        let id = hex_host(7);
        let exists: HostExists = Arc::new(|_x: &[u8; 32]| true);
        let c = Carriers::open(&dir, dial.carrier_dial(), nop(), nop(), exists).unwrap();
        let upper = id.to_uppercase();
        // forward：大写 add → 表内小写 → 大写 delete 命中。
        c.add_forward(ForwardRule {
            host: upper.clone(),
            listen: 20911,
            target_ip: String::new(),
            target_port: 0,
        })
        .unwrap();
        let list = c.forward_states("");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].rule.host, id, "入表 host 必须 canonical 小写");
        c.remove_forward(&upper, 20911).expect("大写 delete 必须命中");
        assert!(c.forward_states("").is_empty());
        // socks：大写 on/off 命中；级联删净（本测试的条目均经 canonical 入口写入；
        // **盘上遗留的旧大写条目不在本批迁移范围**——见 QH.md §5.2 残余）。
        assert_eq!(c.socks_on(&upper, 20912).unwrap(), 20912);
        assert_eq!(c.socks_states()[0].host, id);
        c.socks_off(&upper).expect("大写 off 必须命中");
        c.socks_on(&upper, 20913).unwrap();
        {
            let _g = c.cascade_lock();
            c.remove_host_cascade_locked(&id);
        }
        assert!(c.socks_states().is_empty(), "级联必须删净 socks 条目");
        assert!(c.forward_states("").is_empty(), "级联必须删净 forward 规则");
        // 表外/坏 hex：错误分类不变（add/on = NoHost；remove/off = NoRule）。
        assert!(matches!(
            c.add_forward(ForwardRule { host: "ZZ".into(), listen: 20914, target_ip: String::new(), target_port: 0 }),
            Err(CarrierErr::NoHost)
        ));
        assert!(matches!(c.socks_on("ZZ", 0), Err(CarrierErr::NoHost)));
        drop(c);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
