//! service — term 服务装配（R6 6f-3b）。行为真源 = baseline 克隆
//! `pkg/term/{service.go,term_leg.go,term_surface_session.go,term_surface_leg.go}`。
//!
//! 八模块装配：[`frames`]（帧）+ [`session`]（注册表纯状态机——多腿全序/选举/ENDED
//! 词表）+ [`codec`]（surface 体）+ [`manifest`]（检测规则）+ [`scan`]（旁路扫描器）+
//! [`agent`]（检测融合/卫生）+ [`pty`]（子进程）+ [`ring`]（输出环）。
//!
//! # 并发模型（自管线程 + poll(2)；Go goroutine 面的语义等价收敛）
//!
//! - **每连接一线程**（`serve_conn`：GREETING → HELLO 门 → 读循环）；**每腿一个写者
//!   线程**（有界队列 + [`wire::FrameIo`] 断尾续写——会话级生产者只入队、永不碰
//!   socket，慢腿不影响子进程与其它腿，design D5/B'；conn 的关闭权归写者）；
//! - **每会话**：pump 线程（PTY 读 → 环/扫描器/vt 喂入 + 唤醒）+ 应答写者线程
//!   （查询应答独占写 PTY，FIX-25）+ surface 投递线程（合并窗 16–33ms）；
//!   服务级 sample 线程 1s 一拍；
//! - **锁纪律**（Go「每会话一把锁 × 服务锁」的收敛）：全部会话/腿状态在**一把服务锁**
//!   （[`State`]）内。锁内**不做阻塞 I/O**——socket 写帧/PTY 读/gzip/分片/`ps` 全在
//!   锁外（PTY 写经 [`PtyShared`] 的独立小锁，只包 write 本身；PTY 读归 pump 线程
//!   独占——reader 在 spawn 时被移走）。刻意的例外（微秒级、Go 会话锁内同款）：
//!   PTY setsize（ioctl）、日志行、manifest 求值与 `screen_text`（正则/文本切片）。
//!
//! # HOMEWAY_TERM=off
//!
//! [`disabled_by_env`] 是唯一关闭面（engine 据此不打就绪行、不挂 socket）；
//! 调参变量族 [`TermConfig::from_env`]（非法值回落默认 + 一行告警，同 Go）。

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::net::UnixListener;

use homeway_quic::ServiceIntake;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::SyncSender;
use std::sync::mpsc::TrySendError;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;

use super::agent;
use super::agent::ProcInfo;
use super::codec::DiffBody;
use super::codec::ScrollbarWire;
use super::codec::SnapshotBody;
use super::frames;
use super::frames::create_flags;
use super::frames::hello_flags;
use super::frames::Frame;
use super::frames::Op;
use super::legout::LegOut;
use super::legout::WriteItem;
use super::manifest;
use super::pty::PtySession;
use super::ring::OutputRing;
use super::ring::ReplayEpoch;
use super::scan::TermScan;
use super::session::FinishReason;
use super::session::LegDescriptor;
use super::session::LegEnd;
use super::session::LegKey;
use super::session::LegKind;
use super::session::LegView;
use super::session::SessionRegistry;
use super::session::TermError;
use super::size::Size;
use super::vt::ClipEvt;
use super::vt::SessionVt;
use super::wire::FrameIo;
use super::wire::WriteFail;
use crate::Logf;

const DEFAULT_HISTORY: usize = 1 << 20;
const DEFAULT_REPLAY: usize = 256 << 10;
const REPLAY_BUDGET: Duration = Duration::from_secs(2);
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
const HELLO_TIMEOUT: Duration = Duration::from_secs(15);
const KILL_GRACE: Duration = Duration::from_millis(500);
const SAMPLE_PERIOD: Duration = Duration::from_secs(1);
const RAW_STALL_LIMIT: Duration = Duration::from_secs(60);
const RAW_STALL_RETRY: Duration = Duration::from_secs(1);
/// 应答队列长度（FIX-25：一问一答短序列，16 条吸收突发；满即丢弃计数，绝不阻塞）。
const RESP_QUEUE_LEN: usize = 16;
/// 单帧（未压缩体）默认上限（背压失败模式一）。
const PENDING_CAP: usize = 4 << 20;
/// 每腿队列默认上限（背压失败模式二）。
const QUEUE_BYTES: usize = 8 << 20;
/// 差分合并窗（design D2 的 16–33ms）。
const MERGE_WINDOW_MIN: Duration = Duration::from_millis(16);
const MERGE_WINDOW_MAX: Duration = Duration::from_millis(33);
/// 快照镜像窗口的**行数预算**（F1c；按 48 B/格保守折算成字节口径）：材质 `Vec<Row>`
/// 在旧口径（`rows × MIRROR_VIEWPORTS` = 10 视口）下上限处最坏 5000 行 × 1000 列
/// ≈ 120 MiB（24 B/格）且反复 RESIZE 可反复触发 ⇒ 改为按列反推行数。
///
/// 口径注记（Q-D 代码门 M2/A3）：① 48 B/格只对**常态内容**保守（空白/短 symbol）；
/// 病态内容（F5 截断后单格 127 B 字素簇）材质可达 ~127 B/格 ⇒ 本预算在极端形态下
/// 低估，兜底 = 发送侧 `pending_cap`（超限快照打回重试，不会 abort）。② 组帧峰值
/// ≈3× 材质（`enc_snapshot_body` 的 grid/mirror `clone()` + gzip 临时缓冲，见
/// `flush_surface` 的 `snapshot_mat`）。
const MIRROR_BYTES_BUDGET: usize = 32 << 20;
/// 单次 FETCH-ROWS 行数上限。
const FETCH_ROWS_MAX: u16 = 512;
/// 剪贴板读缓存上限（与 Go 的 clipMaxBytes 同值：载荷 u16 长度域的精确上限）。
const CLIP_MAX_BYTES: usize = super::frames::MAX_PAYLOAD - 3;

/// 快照镜像窗口行数预算（F1c）：`MIRROR_BYTES_BUDGET / (cols × 48)`，下限 64 行。
/// 与视口的 `rows × MIRROR_VIEWPORTS` 取 min 在调用点（[`mirror_window_rows`]）。
/// 下限 64 是**纯防御**：仅 `cols > ~10922` 才触发，生产被 `Size::MAX_COLS`（1000）
/// 封住 ⇒ 恒不触发（留给直连调用点/未来放宽上限）。窄屏（80/100 列）结果与旧行为
/// 一致；宽屏变小——客户端仍可经 FETCH-ROWS 按需拉更多（规格场景不变）。
fn mirror_rows_budget(cols: usize) -> usize {
    (MIRROR_BYTES_BUDGET / (cols.max(1) * 48)).max(64)
}

/// 会话名词法（Go `termNameRx` 同串面）。
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'_' || b == b'-')
}

/// 服务端 vt 逃生口（HOMEWAY_TERM_VT=off：本会话全部退化为 legacy-only——
/// surface 腿得 surface_unavailable；Go `vtGloballyDisabled` 同款）。
pub fn vt_disabled_by_env() -> bool {
    std::env::var("HOMEWAY_TERM_VT")
        .map(|v| v.trim().eq_ignore_ascii_case("off"))
        .unwrap_or(false)
}

/// HOMEWAY_TERM=off 是唯一关闭方式（刻意不做 CLI 旗标）。
pub fn disabled_by_env() -> bool {
    std::env::var("HOMEWAY_TERM")
        .map(|v| v.trim().eq_ignore_ascii_case("off"))
        .unwrap_or(false)
}

fn env_int(name: &str, def: usize) -> usize {
    match std::env::var(name) {
        Ok(v) => {
            let t = v.trim().to_string();
            if t.is_empty() {
                return def;
            }
            match t.parse::<usize>() {
                Ok(n) if n > 0 => n,
                _ => def,
            }
        }
        Err(_) => def,
    }
}

fn env_str(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// 服务配置（全部来自环境变量，默认值即可用；Go `termConfigFromEnv` 同面）。
#[derive(Debug, Clone)]
pub struct TermConfig {
    pub shell: Option<String>,
    pub history: usize,
    pub replay: usize,
    pub replay_epoch: ReplayEpoch,
    pub max_sessions: usize,
    pub max_clients: usize,
    pub write_timeout: Duration,
    pub raw_stall_limit: Duration,
    pub pending_cap: usize,
    pub queue_bytes: usize,
    pub detect: bool,
    pub scrollback_lines: usize,
}

impl Default for TermConfig {
    fn default() -> Self {
        TermConfig {
            shell: None,
            history: DEFAULT_HISTORY,
            replay: DEFAULT_REPLAY,
            replay_epoch: ReplayEpoch::Whole,
            max_sessions: super::session::DEFAULT_MAX_SESSIONS,
            max_clients: super::session::DEFAULT_MAX_CLIENTS,
            write_timeout: WRITE_TIMEOUT,
            raw_stall_limit: RAW_STALL_LIMIT,
            pending_cap: PENDING_CAP,
            queue_bytes: QUEUE_BYTES,
            detect: true,
            scrollback_lines: super::vt::DEFAULT_SCROLLBACK_LINES,
        }
    }
}

impl TermConfig {
    pub fn from_env() -> Self {
        let mut cfg = TermConfig {
            shell: env_str("HOMEWAY_TERM_SHELL"),
            history: env_int("HOMEWAY_TERM_HISTORY", DEFAULT_HISTORY),
            replay: env_int("HOMEWAY_TERM_REPLAY", DEFAULT_REPLAY),
            replay_epoch: match std::env::var("HOMEWAY_TERM_REPLAY_EPOCH") {
                Ok(v) if v.trim().eq_ignore_ascii_case("last") => ReplayEpoch::Last,
                _ => ReplayEpoch::Whole,
            },
            max_sessions: env_int("HOMEWAY_TERM_MAX_SESSIONS", super::session::DEFAULT_MAX_SESSIONS),
            max_clients: env_int("HOMEWAY_TERM_MAX_CLIENTS", super::session::DEFAULT_MAX_CLIENTS),
            write_timeout: Duration::from_millis(env_int(
                "HOMEWAY_TERM_WRITE_TIMEOUT_MS",
                WRITE_TIMEOUT.as_millis() as usize,
            ) as u64),
            raw_stall_limit: Duration::from_millis(env_int(
                "HOMEWAY_TERM_STALL_LIMIT_MS",
                RAW_STALL_LIMIT.as_millis() as usize,
            ) as u64),
            pending_cap: env_int("HOMEWAY_TERM_PENDING_CAP_BYTES", PENDING_CAP),
            queue_bytes: env_int("HOMEWAY_TERM_QUEUE_BYTES", QUEUE_BYTES),
            detect: !std::env::var("HOMEWAY_TERM_DETECT")
                .map(|v| v.trim().eq_ignore_ascii_case("off"))
                .unwrap_or(false),
            scrollback_lines: env_int(
                "HOMEWAY_TERM_SCROLLBACK_LINES",
                super::vt::DEFAULT_SCROLLBACK_LINES,
            ),
        };
        if cfg.replay > cfg.history {
            cfg.replay = cfg.history;
        }
        // 低9（exec-r1）防御性夹取：误配置在配置层就不可达
        if cfg.max_clients < 1 {
            cfg.max_clients = super::session::DEFAULT_MAX_CLIENTS;
        }
        if cfg.pending_cap < 1024 {
            cfg.pending_cap = PENDING_CAP;
        }
        if cfg.queue_bytes < 4096 {
            cfg.queue_bytes = QUEUE_BYTES;
        }
        cfg
    }
}

/// 「设置了但非法」的调参给一行提示（普通用户至少能在日志里看到被忽略了）。
fn warn_invalid_term_env(logf: &Logf) {
    const CHECKS: &[(&str, usize)] = &[
        ("HOMEWAY_TERM_MAX_CLIENTS", 1),
        ("HOMEWAY_TERM_WRITE_TIMEOUT_MS", 1),
        ("HOMEWAY_TERM_STALL_LIMIT_MS", 1),
        ("HOMEWAY_TERM_PENDING_CAP_BYTES", 1024),
        ("HOMEWAY_TERM_QUEUE_BYTES", 4096),
    ];
    for (name, min) in CHECKS {
        if let Ok(v) = std::env::var(name) {
            let t = v.trim();
            if t.is_empty() {
                continue;
            }
            let bad = match t.parse::<usize>() {
                Ok(n) => n < *min,
                Err(_) => true,
            };
            if bad {
                logf(&format!("term: ⚠️ {name}={v:?} 非法（需 ≥{min}），已忽略、用默认值"));
            }
        }
    }
}

/// 现在的 Unix 毫秒（LIST JSON 的时间面）。
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// 线程 panic 兜底（F7）：角色 → 处置表 + 统一 catch
// ---------------------------------------------------------------------------

/// 生产线程角色（处置表按 enum 匹配；评审 5.2）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ThreadRole {
    /// 服务级 1s 采样（按会话 catch——单会话 panic 不停摆全线）。
    Sample,
    /// 每连接线程（GREETING/HELLO/读循环）。
    Conn,
    /// 每腿写者（raw/surface）。
    LegWriter,
    /// 每会话 PTY 泵。
    Pump,
    /// 每会话应答写者。
    Resp,
    /// 每会话 surface 投递。
    Surface,
}

impl ThreadRole {
    const fn as_str(self) -> &'static str {
        match self {
            ThreadRole::Sample => "term-sample",
            ThreadRole::Conn => "term-conn",
            ThreadRole::LegWriter => "term-leg-writer",
            ThreadRole::Pump => "term-pump",
            ThreadRole::Resp => "term-resp",
            ThreadRole::Surface => "term-surface",
        }
    }
}

/// panic 后的处置（F7 处置表；纯函数——单测钉住）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanicAction {
    /// pump：走既有收尾（wait_bounded + finalize_exit）——无 reader 的会话必死，不留僵尸。
    FinalizePump,
    /// leg writer：裸断该腿（leg_write_failed），单腿失败不拖垮会话。
    BreakLeg,
    /// surface：全 surface 腿 finish_quit + 裸断（app 见断连可重连）。
    DropSurface,
    /// resp：线程退出（查询应答降级），会话主体不受影响。
    DropResp,
    /// conn：单连接隔离（drop conn）。
    DropConn,
    /// sample：跳过该会话本拍，下一拍继续。
    SkipSession,
}

fn panic_action(role: ThreadRole) -> PanicAction {
    match role {
        ThreadRole::Pump => PanicAction::FinalizePump,
        ThreadRole::LegWriter => PanicAction::BreakLeg,
        ThreadRole::Surface => PanicAction::DropSurface,
        ThreadRole::Resp => PanicAction::DropResp,
        ThreadRole::Conn => PanicAction::DropConn,
        ThreadRole::Sample => PanicAction::SkipSession,
    }
}

/// panic 载荷 → 一行文本（String/&str 都吃；其余给占位）。
fn panic_payload_text(p: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "非字符串载荷".to_string()
    }
}

/// 线程体包装（参考 `facade/tun_exec.rs:1243-1258` 先例）：捕获 panic → 日志 →
/// 返回处置（None = 正常退出）。**处理器自身不得 panic**（只做 downcast + 日志）。
/// 前提已核：全链无 `panic = "abort"`（根 manifest / `.cargo/config.toml` / capi crate /
/// tier `build-core.sh` 全无）。
fn guard_thread(role: ThreadRole, ctx: &str, logf: &Logf, body: impl FnOnce()) -> Option<PanicAction> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
        Ok(()) => None,
        Err(p) => {
            logf(&format!(
                "term: {ctx}{} 线程 panic（已兜住）：{}",
                role.as_str(),
                panic_payload_text(&*p)
            ));
            Some(panic_action(role))
        }
    }
}

/// 测试注入点（`#[cfg(test)]`）：命中 `(role, 会话名)` 即 panic，**单次消费**；
/// 会话名用既有 `tmp_name` 唯一化。生产构建 = 空函数。
#[cfg(test)]
mod panic_inject {
    use super::ThreadRole;
    use std::sync::Mutex;

    static INJECT: Mutex<Vec<(ThreadRole, String)>> = Mutex::new(Vec::new());

    pub(super) fn arm(role: ThreadRole, name: &str) {
        INJECT.lock().unwrap_or_else(|e| e.into_inner()).push((role, name.to_string()));
    }

    pub(super) fn hit(role: ThreadRole, name: &str) {
        let mut g = INJECT.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = g.iter().position(|(r, n)| *r == role && n == name) {
            g.remove(i);
            drop(g);
            panic!("测试注入 panic（{}）", role.as_str());
        }
    }
}

#[cfg(test)]
fn maybe_inject_panic(role: ThreadRole, name: &str) {
    panic_inject::hit(role, name);
}

#[cfg(not(test))]
#[inline]
fn maybe_inject_panic(_role: ThreadRole, _name: &str) {}

// ---------------------------------------------------------------------------
// PTY 共享面（reader 归 pump 线程独占；master/writer/child 在独立小锁内）
// ---------------------------------------------------------------------------

/// PTY 小锁的中毒恢复口径（F7③，与 `lock_state` 统一）：持锁线程 panic 后
/// `PtySession` 的**可接受不一致面**（例：writer 半写后中断）不影响其余调用可用；
/// 处置保守——上层把「写失败/尺寸应用失败」按既有失败路径处理（断腿/重试），
/// 而不是让整个 term 面级联崩溃。
struct PtyShared {
    inner: Mutex<PtySession>,
}

impl PtyShared {
    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.inner.lock().unwrap_or_else(|e| e.into_inner()).resize(cols, rows);
    }

    fn write_input(&self, p: &[u8]) -> bool {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).write_input(p).is_ok()
    }

    fn foreground_pgid(&self) -> i32 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).foreground_pgid()
    }

    fn pid(&self) -> i32 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).pid
    }

    fn kill_start(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).kill_start();
    }

    fn kill_force(&self) {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).kill_force();
    }

    fn wait_bounded(&self, grace: Duration) -> i32 {
        self.inner.lock().unwrap_or_else(|e| e.into_inner()).wait_bounded(grace)
    }
}

// ---------------------------------------------------------------------------
// surface 腿的投递状态（Go term_surface_leg.go）
// ---------------------------------------------------------------------------

/// wire 屏态（基线对账面：光标/模式位/回滚条/备用屏）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct SurfState {
    cursor: super::codec::SurfaceCursor,
    modes: u32,
    total: u64,
    offset: u64,
    len: u16,
    alt: bool,
}

/// surface 腿观测计数器（断开日志用；Go surfaceStats 同面，writeTimeout 计数合入
/// 断腿路径）。
#[derive(Debug, Clone, Copy, Default)]
struct SurfStats {
    snapshots: u64,
    diffs: u64,
    degrades: u64,
    backpressure: u64,
    queue_overflow: u64,
    encode_failed: u64,
    fragments: u64,
    bytes_out: u64,
    fetch_hits: u64,
    fetch_miss: u64,
    trims: u64,
    shifts: u64,
    write_timeout: u64,
}

/// 一条 surface 腿的投递状态（基线/世代/背压都在腿上——掉队腿全量重建、健康腿继续差分）。
#[derive(Debug, Default)]
struct SurfaceLeg {
    revision: u32,
    need_snapshot: bool,
    has_base: bool,
    base: SurfState,
    last_write_cost: Duration,
    /// 回滚条基线 = **上一帧已告知客户端**的回滚条（随成功入队的帧推进，快照与差分都算）；
    /// 非「快照时刻的 total」——输出增长期的回落会漏判（基线 8167cb7）。
    has_last_sent: bool,
    last_sent: ScrollbarWire,
    stats: SurfStats,
}

impl SurfaceLeg {
    fn new() -> Self {
        SurfaceLeg { need_snapshot: true, ..Default::default() }
    }

    fn mark_need_snapshot(&mut self, reason: &str) {
        self.need_snapshot = true;
        match reason {
            "backpressure" => self.stats.backpressure += 1,
            "queue_overflow" => self.stats.queue_overflow += 1,
            "encode_failed" => self.stats.encode_failed += 1,
            _ => {}
        }
    }

    fn take_snapshot_flag(&mut self) -> bool {
        std::mem::take(&mut self.need_snapshot)
    }

    fn next_revision(&mut self) -> u32 {
        self.revision += 1;
        self.revision
    }

    fn under_pressure(&self) -> bool {
        self.last_write_cost > MERGE_WINDOW_MAX
    }

    fn state_unchanged(&self, st: &SurfState) -> bool {
        self.has_base
            && st.cursor == self.base.cursor
            && st.modes == self.base.modes
            && st.total == self.base.total
            && st.offset == self.base.offset
            && st.len == self.base.len
    }

    /// 回滚回落检测（基线 8167cb7 平移判据）：total 回落分两类——
    /// 平移型（len 不变且距底 total-offset 不变：真裁剪/清回滚/erase 前缀）不再强制
    /// 全量，客户端自己平移缓存，差分照发；非平移（距底/len 变化，行号语义不再纯平移）
    /// ⇒ 强制全量重建。返回 true = 触发了非平移回落。
    fn note_scrollbar(&mut self, sb: ScrollbarWire) -> bool {
        if !self.has_last_sent {
            return false; // 还没告知过客户端任何回滚条：首次 attach 本来就走全量
        }
        let prev = self.last_sent;
        if sb.total >= prev.total {
            return false; // 增长/持平 = 输出推进，不是回落
        }
        let pure_shift = sb.len == prev.len && (prev.total - prev.offset) == (sb.total - sb.offset);
        if pure_shift {
            self.stats.shifts += 1;
            return false;
        }
        self.need_snapshot = true;
        self.stats.trims += 1;
        true
    }
}

/// raw 腿的握手计划（写者执行；锁内构建）。
struct RawHandshake {
    attached: Vec<u8>,
    start: u64,
    end: u64,
    truncated: bool,
    epochs: Vec<u64>,
    budget: Duration,
    nudge_focus: bool,
}

// ---------------------------------------------------------------------------
// 会话运行态（服务锁内）
// ---------------------------------------------------------------------------

/// 一条腿的运行态（连接归写者线程——`out` 与 surface 状态共享）。
struct LegRt {
    key: LegKey,
    kind: LegKind,
    /// HELLO 尾随声明了 capsRawTerminal（应答让位判据——与 surface 位无关，
    /// surface+rawCapable 组合同样让位；Go rawTermLegs 计数器同义，评审 M4）。
    raw_capable: bool,
    /// **本腿**键编码平台口径（Q-J F1）：由 HELLO caps 声明（`KEY_ALT_*`），未声明/
    /// 歧义 = 宿主推断。按腿存放——同会话两条不同声明的腿各按各自口径编码。
    key_flavor: super::keyenc::KeyFlavor,
    out: Arc<LegOut>,
    surface: Option<SurfaceLeg>,
    theme_known: bool,
    theme: ([u8; 3], [u8; 3]),
    clip_cache: Option<String>,
}

/// 一秒输出桶（输出腿数据源）。
#[derive(Clone, Copy, Default)]
struct OutBucket {
    sec: i64,
    n: i64,
}

/// 每会话 PTY 注入队列的条目（F3）：查询应答（FIX-25）与焦点 nudge 共用
/// `resp_tx` 这一**每会话唯一 PTY 写者**——队列序 = 状态迁移序。
enum PtyInject {
    /// 查询应答字节（服务端代答；让位规则见 pump 的 suppress）。
    Resp(Vec<u8>),
    /// 焦点 nudge（`\x1b[I`/`\x1b[O`；写失败的行文与旧实现同串）。
    /// `&'static [u8]` 而非 `Vec<u8>`——两个字节常量零堆分配（代码门 L3）。
    Nudge(&'static [u8]),
}

/// 会话运行态。
struct SessRt {
    name: String,
    /// 会话代数（同名重建 +1；旧线程回查的身份面，见 [`rt_of`]）。
    gen: u64,
    created_ms: u64,
    pty: Arc<PtyShared>,
    ring: OutputRing,
    scan: TermScan,
    /// None = 本会话无服务端 vt（legacy-only；surface attach 得明确错误）。
    vt: Option<SessionVt>,
    legs: Vec<LegRt>,
    writers: usize,
    // 采样/检测态
    agent: u8,
    state_v2: u8,
    hygiene: agent::Hygiene,
    prev_cpu: Option<i64>,
    prev_quiet: i32,
    content_seq: u64,
    last_scan_seq: u64,
    last_scan_proc: String,
    out_buckets: [OutBucket; 4],
    last_active_ms: u64,
    sentinel_count: usize,
    last_notified: String,
    /// active 腿上报的剪贴板读缓存（OSC 52 读请求的应答源）。
    clip_cache: Option<String>,
    resp_tx: SyncSender<PtyInject>,
    /// 观测面（F6）：查询应答/剪贴板写/PTY 注入（nudge）的丢弃计数（队列满）。
    resp_dropped: Arc<AtomicU64>,
    clip_dropped: Arc<AtomicU64>,
    nudge_dropped: Arc<AtomicU64>,
    /// `RESIZE 0×0` 忽略计数（F1a 的 0 值面观测；节流日志用）。
    zero_resize_ignored: u64,
    /// surface 唤醒（pump/attach → 投递线程；u32 代数 + Condvar）。
    surf_wake: Arc<(Mutex<u32>, Condvar)>,
    /// 剪贴板写投递（vt 事件面 → surface 投递线程；只发 surface 腿）。
    clip_tx: SyncSender<String>,
    /// 会话收工位（surface/应答线程的退出节拍）。
    stopped: Arc<AtomicBool>,
}

impl SessRt {
    fn count_out(&mut self, n: usize, now_ms: u64) {
        let sec = (now_ms / 1000) as i64;
        for b in &mut self.out_buckets {
            if b.sec == sec {
                b.n += n as i64;
                return;
            }
        }
        let mut oldest = 0;
        for i in 0..self.out_buckets.len() {
            if self.out_buckets[i].sec < self.out_buckets[oldest].sec {
                oldest = i;
            }
        }
        self.out_buckets[oldest] = OutBucket { sec, n: n as i64 };
    }

    fn out_bytes(&self, now_ms: u64) -> i64 {
        let floor = (now_ms / 1000) as i64 - agent::OUT_WINDOW_SEC as i64 + 1;
        self.out_buckets.iter().filter(|b| b.sec >= floor).map(|b| b.n).sum()
    }

    fn any_surface(&self) -> bool {
        self.legs.iter().any(|l| l.surface.is_some())
    }

    fn wake_surface(&self) {
        let (m, cv) = &*self.surf_wake;
        let mut g = m.lock().unwrap_or_else(|e| e.into_inner());
        *g = g.wrapping_add(1);
        cv.notify_all();
    }

    /// 会话当前尺寸（epoch 表尾——note_size 恒在尺寸变化时记录；类型保证非 0 且在限内）。
    fn size(&self) -> Size {
        self.ring
            .epochs
            .last()
            .map(|e| e.size)
            .unwrap_or(Size::DEFAULT)
    }
}

/// 服务全局状态（一把锁；Go「会话锁 × N + 服务锁」的收敛面——锁内不做任何 I/O）。
struct State {
    registry: SessionRegistry,
    sessions: HashMap<String, SessRt>,
    manifests: Option<manifest::Loader>,
    /// 会话代数发号器：同名会话重建后旧线程（pump/写者/投递）按（名字, 代）回查，
    /// 代不符即视为 Gone——等价 Go `remove(name, who)` 的身份比对（评审 N1）。
    next_sess_gen: u64,
}

/// 按身份（名字 + 代数）取会话运行态；代不符 = 已被同名重建顶掉（None）。
fn rt_of<'a>(st: &'a State, name: &str, gen: u64) -> Option<&'a SessRt> {
    st.sessions.get(name).filter(|rt| rt.gen == gen)
}

/// 同 [`rt_of`] 的可变版。
fn rt_of_mut<'a>(st: &'a mut State, name: &str, gen: u64) -> Option<&'a mut SessRt> {
    st.sessions.get_mut(name).filter(|rt| rt.gen == gen)
}

// ---------------------------------------------------------------------------
// TermService
// ---------------------------------------------------------------------------

/// 出口侧终端服务。
pub struct TermService {
    cfg: TermConfig,
    logf: Logf,
    state: Mutex<State>,
    stop: Arc<AtomicBool>,
    /// F1 观测面：HELLO caps 两位同置（歧义，含「裸 ID 尾随块」既有形态）的计数与
    /// 一次性告警位——语义按「未声明（宿主推断）」处理，绝不拒腿。
    key_flavor_ambiguous: AtomicU64,
    key_flavor_ambiguous_warned: AtomicBool,
    /// F1 观测面：`handle_input` 查不到腿（`end_leg` 竞态）而丢弃输入的计数。
    leg_missing_input_drops: AtomicU64,
    /// **连接级在册计数**（M3 §1.7 设计门 2-4 的闸；容量见 [`Self::conn_capacity`]）——
    /// 占位在 accept 处原子取，线程退出（含 panic/隔离）即归还。
    active_conns: Arc<AtomicUsize>,
    /// 连接级超限被收线的累计（观测面；行文节流首 3 + 每 100）。
    conn_over_cap: AtomicU64,
    /// 测试身份（panic 注入表按服务唯一化——并发测试的其它服务 tick 不会误消费；
    /// 生产构建无此字段）。
    #[cfg(test)]
    svc_id: u64,
}

/// 连接级名额的归还卫兵（线程退出即 `Drop`；`spawn` 失败时由调用侧显式归还）。
struct ConnGuard {
    n: Arc<AtomicUsize>,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        self.n.fetch_sub(1, Ordering::Relaxed);
    }
}

/// 测试服务的身份发号器（`#[cfg(test)]`）。
#[cfg(test)]
fn next_svc_id() -> u64 {
    static N: AtomicU64 = AtomicU64::new(1);
    N.fetch_add(1, Ordering::Relaxed)
}

impl TermService {
    /// 起服务。`state_dir` = 出口 state 目录（manifest 覆盖目录在
    /// `<state_dir>/agent-detection/`；None = 只用内嵌 manifest）。
    pub fn new(logf: Logf, state_dir: Option<&std::path::Path>) -> Arc<Self> {
        let cfg = TermConfig::from_env();
        warn_invalid_term_env(&logf);
        let mut manifests = None;
        if cfg.detect {
            let override_dir = state_dir.map(|d| d.join(super::manifest::OVERRIDE_DIR_NAME));
            let l = manifest::Loader::new(override_dir.as_deref());
            for w in l.warnings() {
                logf(&format!("term: ⚠️ 检测规则加载告警：{w}"));
            }
            let desc = match &override_dir {
                Some(d) => d.display().to_string(),
                None => "无（只用内嵌）".to_string(),
            };
            logf(&format!("term: 检测规则已加载 {} 份（覆盖目录 {}）", l.ids().len(), desc));
            manifests = Some(l);
        }
        let registry = SessionRegistry::new(cfg.max_sessions, cfg.max_clients);
        let svc = Arc::new(TermService {
            cfg,
            logf,
            state: Mutex::new(State {
                registry,
                sessions: HashMap::new(),
                manifests,
            next_sess_gen: 1,
            }),
            stop: Arc::new(AtomicBool::new(false)),
            key_flavor_ambiguous: AtomicU64::new(0),
            key_flavor_ambiguous_warned: AtomicBool::new(false),
            leg_missing_input_drops: AtomicU64::new(0),
            active_conns: Arc::new(AtomicUsize::new(0)),
            conn_over_cap: AtomicU64::new(0),
            #[cfg(test)]
            svc_id: next_svc_id(),
        });
        // sample 线程（服务级 1s 一拍；Go sampleLoop）
        let tick_svc = Arc::clone(&svc);
        std::thread::Builder::new()
            .name("term-sample".into())
            .spawn(move || tick_svc.sample_loop())
            .ok();
        svc
    }

    /// 会话数上限（`HOMEWAY_TERM_MAX_SESSIONS`，缺省 16）——服务入口容量 = 它 + K
    /// （M3 §1.7 设计门 2-4：term 今天**没有**连接级在册闸，入口容量按会话上限 + K 取，
    /// 是**新引入的连接级上限**，行为变化见 `docs/reviews/M3-design.md` §8.2-16）。
    pub fn max_sessions(&self) -> usize {
        self.cfg.max_sessions
    }

    /// **连接级在册上限**（= 会话上限 + `INTAKE_K`，缺省 16+4=20；§1.7 设计门 2-4）。
    ///
    /// 代码门 r18（C2-1）的整改：S2 只把该值用作**出口 intake 的队列容量**（取出即释放名额），
    /// 而 term 的 accept 循环对每条连接 spawn 一个 `term-conn` 线程、**从不计数** ⇒ 登记里
    /// 「20 = 新引入的连接级上限，防一条设备开满 bidi 流把 term 线程数打成无界」当时是空头承诺。
    /// 现在 accept 处**原子占名额**、线程退出即归还（含 panic），超限 ⇒ 收线 + 一行 + 计数。
    pub fn conn_capacity(&self) -> usize {
        homeway_quic::tuning::service_defaults::intake_capacity(self.cfg.max_sessions)
    }

    /// 当刻连接级在册数（观测面；测试判据同源）。
    pub fn active_conns(&self) -> usize {
        self.active_conns.load(Ordering::Relaxed)
    }

    /// 连接级超限被收线的累计（观测面）。
    pub fn conn_over_cap(&self) -> u64 {
        self.conn_over_cap.load(Ordering::Relaxed)
    }

    /// **受理一条连接**（两源共用；连接级闸先于 spawn）。
    ///
    /// 超限 ⇒ **收线**（普通 close：客户端见 EOF，与「服务未启用」同面）+ 节流行 + 计数；
    /// 不排队（排队会把「已受理但无会话」的连接与在册语义搅在一起；QUIC 侧还有 intake
    /// 队列作为第二道缓冲）。
    fn serve_one(self: &Arc<Self>, stream: UnixStream) {
        let cap = self.conn_capacity();
        // 先占名额（占位发生在 spawn 之前 ⇒ 上限不被 spawn 延迟漏掉）
        let mut cur = self.active_conns.load(Ordering::Relaxed);
        loop {
            if cur >= cap {
                let n = self.conn_over_cap.fetch_add(1, Ordering::Relaxed) + 1;
                if n <= 3 || n.is_multiple_of(100) {
                    (*self.logf)(&format!(
                        "term: ⚠️ 连接超限（在册 {cur}/{cap}）—— 收线（连接级上限；M3 §1.7 设计门 2-4；第 {n} 次）"
                    ));
                }
                drop(stream); // 普通 close ⇒ 对端 EOF（不占线程、不占会话）
                return;
            }
            if self
                .active_conns
                .compare_exchange_weak(cur, cur + 1, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                break;
            }
            cur = self.active_conns.load(Ordering::Relaxed);
        }
        let svc = Arc::clone(self);
        let n = Arc::clone(&self.active_conns);
        let spawned = std::thread::Builder::new()
            .name("term-conn".into())
            .spawn(move || {
                // 名额随线程退出归还（含 panic / `guard_thread` 的单连接隔离面）
                let _guard = ConnGuard { n };
                // F7：单连接隔离——panic 只 drop 该连接（stream 随闭包 drop）
                let _ = guard_thread(ThreadRole::Conn, "", &svc.logf, || {
                    svc.serve_conn(stream)
                });
            });
        if let Err(e) = spawned {
            self.active_conns.fetch_sub(1, Ordering::Relaxed); // 线程未起 ⇒ 名额立即归还
            crate::syncutil::log_spawn_failed(&self.logf, "term-conn", &e, "连接被丢弃（名额已归还）");
        }
    }

    /// 登录 shell 文本（就绪行用）。
    pub fn shell_text(&self) -> String {
        super::pty::login_shell()
    }

    /// 历史窗口文本（就绪行用；Go `%dKiB`）。
    pub fn history_text(&self) -> String {
        format!("{}KiB", self.cfg.history >> 10)
    }

    /// 本构建能力位文本（就绪行用；Go FeaturesText——本面恒全开）。
    pub fn features_text(&self) -> &'static str {
        "list,replay,modes,agent,title,surface"
    }

    /// 服务端 vt 现状文本（就绪行用；HOMEWAY_TERM_VT 逃生口的判据面）。
    pub fn vt_text(&self) -> &'static str {
        if vt_disabled_by_env() {
            "off（HOMEWAY_TERM_VT）"
        } else {
            "on"
        }
    }

    /// UDS 监听循环（每连接一线程；engine 挂在 term.sock 上；阻塞 accept 形态——
    /// 测试面用；engine 用 [`Self::serve_stoppable`] 纳入 stop_flags 收口）。
    pub fn serve(self: &Arc<Self>, ln: UnixListener) {
        for conn in ln.incoming() {
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            match conn {
                Ok(stream) => self.serve_one(stream),
                Err(_) => return, // listener 已关
            }
        }
    }

    /// 同 [`Self::serve`]，但 accept 走**两源入口**（[`ServiceIntake`]）+ 停止位轮询——
    /// 服务收工时监听线程可退出（不陪跑到进程结束；Go Shutdown 里的 `s.termLn.Close()` 同效）。
    ///
    /// **M3 S2 的重写点（设计 §2.2）**：形参 `UnixListener → ServiceIntake`，poll 目标改成
    /// intake 登记的**就绪 fd 集**（⑤：UDS 监听 fd + QUIC 唤醒读端）——两条性质逐条保住：
    /// ①「poll 到达即 accept——**无 200ms 空闲延迟**」（评审 P7；唤醒面由 intake 的
    /// 自唤醒管道承担，QUIC 入队写 1B 即刻可读）；②停止位在窗内可查（200ms 节拍不变）。
    /// `ServiceIntake::from_listener` = 改前的单 UDS 形态（既有 P7 用例走它，语义零改）。
    pub fn serve_stoppable(self: &Arc<Self>, intake: ServiceIntake, stop: Arc<AtomicBool>) {
        loop {
            if stop.load(Ordering::Relaxed) || self.stop.load(Ordering::Relaxed) {
                return;
            }
            // poll 就绪 fd 集（连接**到达即返回**——纯 sleep 轮询会给空闲期连接加
            // 平均 100ms 延迟；200ms 只是空闲醒来看停止位的节拍，评审 P7）
            let mut pfds: Vec<libc::pollfd> = intake
                .ready_fds()
                .into_iter()
                .map(|fd| libc::pollfd { fd, events: libc::POLLIN, revents: 0 })
                .collect();
            let r = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, 200) };
            if r < 0 {
                let err = std::io::Error::last_os_error();
                if err.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return;
            }
            if r == 0 {
                continue; // 空闲：只查停止位
            }
            match intake.accept() {
                Ok(stream) => self.serve_one(stream),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(_) => return,
            }
        }
    }

    /// 关停服务：全部会话 ENDED(service_stopped) + 收尸（不影响其它服务）。
    pub fn close(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let names: Vec<String> = {
            let st = self.lock_state();
            st.registry.session_names().into_iter().map(String::from).collect()
        };
        // 并发收工（Go FIX-33：finish 每条会话最多等子进程 2s，串行收 16 条最坏 32s——
        // 关停路径不该被线性放大）
        std::thread::scope(|scope| {
            for name in &names {
                scope.spawn(|| self.finish_session(name, FinishReason::ServiceStopped));
            }
        });
    }

    // ---- 连接处理（Go ServeConn）----

    fn serve_conn(self: &Arc<Self>, stream: UnixStream) {
        let mut io = FrameIo::new(stream);
        if io.write_frame(Op::GREETING, &frames::enc_greeting(), WRITE_TIMEOUT).is_err() {
            return;
        }
        let first = match io.read_frame_deadline(HELLO_TIMEOUT) {
            Ok(f) => f,
            Err(_) => return,
        };
        match first.op {
            Op::LIST => {
                let json = self.list_json();
                let _ = io.write_frame(Op::LIST, json.as_bytes(), WRITE_TIMEOUT);
            }
            Op::EXPLAIN => {
                let name = match dec_name_or(&first) {
                    Ok(n) => n,
                    Err(payload) => {
                        let _ = io.write_frame(Op::ERROR, &payload, WRITE_TIMEOUT);
                        return;
                    }
                };
                match self.explain_json(&name) {
                    Ok(out) => {
                        let _ = io.write_frame(Op::EXPLAIN, out.as_bytes(), WRITE_TIMEOUT);
                    }
                    Err(e) => {
                        let _ =
                            io.write_frame(Op::ERROR, &frames::enc_error(e.code.as_str(), &e.msg), WRITE_TIMEOUT);
                    }
                }
            }
            Op::KILL => {
                let name = match dec_name_or(&first) {
                    Ok(n) => n,
                    Err(payload) => {
                        let _ = io.write_frame(Op::ERROR, &payload, WRITE_TIMEOUT);
                        return;
                    }
                };
                match self.kill(&name) {
                    Ok(()) => {
                        let _ = io.write_frame(Op::OK, &[], WRITE_TIMEOUT);
                    }
                    Err(e) => {
                        let _ =
                            io.write_frame(Op::ERROR, &frames::enc_error(e.code.as_str(), &e.msg), WRITE_TIMEOUT);
                    }
                }
            }
            Op::CREATE => {
                // 创建不接入（任务 6.2）：不动 PTY 尺寸、不产生腿、不触发哨兵/焦点
                let (flags, name) = match frames::dec_create(&first.payload) {
                    Ok(v) => v,
                    Err(e) => {
                        let _ = io
                            .write_frame(Op::ERROR, &frames::enc_error("bad_create", &e.to_string()), WRITE_TIMEOUT);
                        return;
                    }
                };
                if !valid_name(&name) {
                    let _ = io.write_frame(
                        Op::ERROR,
                        &frames::enc_error("invalid_name", "会话名只能是 [A-Za-z0-9._-]{1,64}"),
                        WRITE_TIMEOUT,
                    );
                    return;
                }
                match self.create_only(&name, flags & create_flags::REUSE_IF_EXISTS != 0) {
                    Ok(()) => {
                        let _ = io.write_frame(Op::OK, &[], WRITE_TIMEOUT);
                    }
                    Err(e) => {
                        let _ =
                            io.write_frame(Op::ERROR, &frames::enc_error(e.code.as_str(), &e.msg), WRITE_TIMEOUT);
                    }
                }
            }
            Op::HELLO => self.serve_hello(first, io),
            op => {
                let _ = io.write_frame(
                    Op::ERROR,
                    &frames::enc_error("bad_op", &format!("首帧必须是 HELLO/LIST/KILL/CREATE（收到 0x{:02x}）", op.0)),
                    WRITE_TIMEOUT,
                );
            }
        }
    }

    /// HELLO：能力协商/版本门 → attachOrCreate → 注册腿 + 写者 + 读循环。
    fn serve_hello(self: &Arc<Self>, hello: Frame, mut io: FrameIo) {
        let (cols, rows, flags, name, tail) = match frames::dec_hello(&hello.payload) {
            Ok(v) => v,
            Err(e) => {
                let _ = io.write_frame(Op::ERROR, &frames::enc_error("bad_hello", &e.to_string()), WRITE_TIMEOUT);
                return;
            }
        };
        let ht = match frames::dec_hello_tail(tail) {
            Ok(t) => t,
            Err(e) => {
                let _ = io.write_frame(Op::ERROR, &frames::enc_error("bad_capability", &e.to_string()), WRITE_TIMEOUT);
                return;
            }
        };
        // 服务端版本门（FIX-29）：声明版本 ≠ 本出口 ⇒ 拒腿 + 可行动文案
        if ht.ver_present && ht.ver != frames::PROTO_VER {
            let _ = io.write_frame(
                Op::ERROR,
                &frames::enc_error(
                    "term_version",
                    &format!(
                        "客户端终端协议版本 {} 与本出口 {} 不符：请把 App / homeway term 与出口升到同一版本",
                        ht.ver,
                        frames::PROTO_VER
                    ),
                ),
                WRITE_TIMEOUT,
            );
            return;
        }
        let surface = ht.caps_present && ht.caps & frames::caps::SURFACE != 0;
        let raw_capable = ht.caps_present && ht.caps & frames::caps::RAW_TERMINAL != 0;
        // F1：键编码平台口径（与本块 surface/raw 判定同域——`dec_hello_tail` 只做形状）
        let key_flavor = self.key_flavor_from_caps(ht.caps, ht.caps_present);
        if !valid_name(&name) {
            let _ = io.write_frame(
                Op::ERROR,
                &frames::enc_error("invalid_name", "会话名只能是 [A-Za-z0-9._-]{1,64}"),
                WRITE_TIMEOUT,
            );
            return;
        }
        let create = flags & hello_flags::CREATE != 0;
        let only_if_absent = flags & hello_flags::ONLY_IF_ABSENT != 0;
        let takeover = flags & hello_flags::TAKEOVER != 0;
        if let Err(e) = self.attach_or_create(&name, cols, rows, create, only_if_absent) {
            let _ = io.write_frame(Op::ERROR, &frames::enc_error(e.code.as_str(), &e.msg), WRITE_TIMEOUT);
            return;
        }
        // surface 腿门：本会话无 vt ⇒ 明确报错（不静默降级）
        if surface {
            let has_vt = {
                let st = self.lock_state();
                st.sessions.get(&name).is_some_and(|s| s.vt.is_some())
            };
            if !has_vt {
                let _ = io.write_frame(
                    Op::ERROR,
                    &frames::enc_error(
                        "surface_unavailable",
                        "本出口没有服务端 vt（HOMEWAY_TERM_VT=off 或平台不支持）",
                    ),
                    WRITE_TIMEOUT,
                );
                return;
            }
        }
        // 尺寸归一（Go spawnLocked 的 80/24 缺省；HELLO 0x0 不产生退化几何——评审 L8；
        // 超限**夹取** = F1a/P0-3：`HELLO 65535x65535` 不再让 alacritty 发起 96 GiB 起
        // 的分配——分配失败走 handle_alloc_error（abort），必须在入径拦下）
        let size = Size::normalized(cols, rows);
        let clamped = !Size::is_exact(cols, rows);
        // 注册腿（全序：同实例替换 → 接管 → 腾位 → 入表即活动）
        let desc = LegDescriptor {
            client_id: ht.client_id.clone(),
            surface,
            raw_capable,
            size,
        };
        let (out, hs, reg, sess_gen) = {
            let mut guard = self.lock_state();
            let st = &mut *guard;
            // ATTACHED/回放计划用**选举前**的会话尺寸（Go registerLegLocked 同序）
            let prev_size = st.registry.session(&name).map(|s| s.size).unwrap_or(size);
            let Some(sess_gen) = st.sessions.get(&name).map(|rt| rt.gen) else {
                return;
            };
            // 实时停滞快照（F4：淘汰排序真源 = 各腿 LegOut；服务锁内取——锁序
            // state → legout 与 end_leg 的既有取序一致，无死锁环）
            let stalled: Vec<(LegKey, Duration)> = st
                .sessions
                .get(&name)
                .map(|rt| {
                    rt.legs
                        .iter()
                        .filter_map(|l| l.out.stalled_snapshot().map(|d| (l.key, d)))
                        .collect()
                })
                .unwrap_or_default();
            let regres = st.registry.register_leg(&name, desc, takeover, now_ms(), &stalled);
            let reg = match regres {
                Ok(o) => o,
                Err(e) => {
                    let payload = frames::enc_error(e.code.as_str(), &e.msg);
                    drop(guard); // 先出锁再写错误帧
                    let _ = io.write_frame(Op::ERROR, &payload, WRITE_TIMEOUT);
                    return;
                }
            };
            let rt = st
                .sessions
                .get_mut(&name)
                .expect("注册成功的会话必在运行态（同一把锁内插入）");
            // 注册引发的腿断（替换/接管/腾位）落运行态：ENDED/裸断经各自写者送达
            for e in &reg.ended {
                apply_end_to_leg_rt(&self.logf, rt, e);
            }
            let out = Arc::new(LegOut::new());
            rt.legs.push(LegRt {
                key: reg.key,
                kind: LegKind::of(surface, raw_capable),
                raw_capable,
                key_flavor,
                out: Arc::clone(&out),
                surface: surface.then(SurfaceLeg::new),
                theme_known: false,
                theme: ([0, 0, 0], [0, 0, 0]),
                clip_cache: None,
            });
            self.logf(&reg.log);
            // 夹取日志（评审 1.4：只在**真发生夹取且尺寸真被应用**时打——同一超限值重复
            // 上报走「尺寸未变 ⇒ size_applied=None」天然不刷屏）
            if clamped && reg.size_applied.is_some() {
                self.logf(&format!(
                    "term: 会话 {name} 尺寸夹取 {cols}x{rows} → {size}（上限 {}x{}）",
                    Size::MAX_COLS,
                    Size::MAX_ROWS
                ));
            }
            // 接入即活动的尺寸应用（H2：注册表只改记账，PTY/vt/环 epoch 在这里跟上；
            // 哨兵不在注册路径注入——stream 统一一次）
            if let Some(sz) = reg.size_applied {
                apply_size_locked(rt, sz);
            }
            // 活动切换的主题/剪贴板回落（评审 P3：注册即活动——Go noteActivityLocked
            // 内建；新腿无上报时把会话缓存清成它的（空）值）
            apply_active_theme_locked(rt, st.registry.session(&name).and_then(|sm| sm.active));
            // ATTACHED/握手计划（锁内构建；ATTACHED 先入队——此刻腿已在表内且锁在手）
            let attached = frames::enc_attached(
                prev_size.cols(),
                prev_size.rows(),
                rt.scan.modes(),
                rt.agent,
                rt.state_v2,
                &rt.name,
            );
            let hs = if surface {
                out.enqueue(WriteItem::new(Op::ATTACHED, attached), 0);
                if let Some(s) = rt.legs.last_mut().and_then(|l| l.surface.as_mut()) {
                    s.mark_need_snapshot("attach");
                }
                None
            } else {
                let (start, truncated) = rt.ring.replay_start(self.cfg.replay, self.cfg.replay_epoch);
                Some(RawHandshake {
                    attached,
                    start,
                    end: rt.ring.written(),
                    truncated,
                    epochs: rt.ring.epochs.iter().map(|e| e.off).collect(),
                    budget: REPLAY_BUDGET,
                    nudge_focus: reg.first,
                })
            };
            (out, hs, reg, sess_gen)
        };
        // 写者线程（conn 的写半归它；读半留在本线程）。起不来 ⇒ 摘腿收线
        // （writers 计数只在 spawn 成功后 +1——评审 L6：失败的写者永不回收）。
        let wstream = match io.try_clone_stream() {
            Ok(s) => s,
            Err(_) => {
                self.logf(&format!("term: 会话 {name} 写者流克隆失败，腿收线"));
                self.end_leg(&name, reg.key, i32::MIN, "", "client_closed");
                return;
            }
        };
        let wio = FrameIo::new(wstream);
        // 记账先加后滚：自增在 spawn **之前**（spawn 后才加存在「子线程先Exited 减到
        // 0、父线程再 +1」的幽灵写者竞态——评审 P4a；自增在 spawn 前则子线程尚未
        // 存在、无竞态，Err 时回滚即可）
        {
            let mut st = self.lock_state();
            if let Some(rt) = rt_of_mut(&mut st, &name, sess_gen) {
                rt.writers += 1;
            }
        }
        let spawned = {
            let svc = Arc::clone(self);
            let tname = name.clone();
            let tkey = reg.key;
            let tout = Arc::clone(&out);
            std::thread::Builder::new()
                .name("term-leg-writer".into())
                .spawn(move || {
                    let ctx = format!("会话 {tname} ");
                    let svc2 = Arc::clone(&svc);
                    let tname2 = tname.clone();
                    let body = move || {
                        if let Some(hs) = hs {
                            svc2.run_raw_writer(&tname2, sess_gen, tkey, wio, tout, hs);
                        } else {
                            svc2.run_surface_writer(&tname2, sess_gen, tkey, wio, tout);
                        }
                    };
                    if let Some(action) = guard_thread(ThreadRole::LegWriter, &ctx, &svc.logf, body) {
                        debug_assert_eq!(action, PanicAction::BreakLeg);
                        // 裸断该腿（单腿失败不拖垮会话）+ 写者计数回收
                        // （panic 可能发生在 writer_exited 之前，不补会让运行态永驻）
                        svc.leg_write_failed(&tname, tkey, "panicked");
                        svc.writer_exited(&tname, sess_gen);
                    }
                })
        };
        if let Err(e) = spawned {
            self.logf(&format!("term: 会话 {name} 写者线程起不来（{e}）——腿收线"));
            {
                let mut st = self.lock_state();
                if let Some(rt) = rt_of_mut(&mut st, &name, sess_gen) {
                    rt.writers = rt.writers.saturating_sub(1);
                }
            }
            self.end_leg(&name, reg.key, i32::MIN, "", "client_closed");
            return;
        }
        // 尺寸哨兵：raw 腿在回放之后（写者按握手计划执行）；surface 腿在快照下发前
        // （投递循环还在合并窗里，快照取到的已是按最终尺寸重绘过的屏）。
        self.sentinel_repaint(&name, sess_gen);
        if surface {
            let st = self.lock_state();
            if let Some(rt) = rt_of(&st, &name, sess_gen) {
                rt.wake_surface();
            }
        }
        // 读循环（连接存续期间）
        self.stream_read_loop(&name, sess_gen, reg.key, io, Arc::clone(&out));
        // 正常断开：摘腿（裸断——客户端已关）
        self.end_leg(&name, reg.key, i32::MIN, "", "client_closed");
    }

    /// 腿读循环（Go stream 的读半）：输入/尺寸/上行帧 → 会话面。
    fn stream_read_loop(
        self: &Arc<Self>,
        name: &str,
        sess_gen: u64,
        key: LegKey,
        mut io: FrameIo,
        out: Arc<LegOut>,
    ) {
        loop {
            let f = match io.read_frame() {
                Ok(f) => f,
                Err(_) => return,
            };
            match f.op {
                Op::DATA => {
                    let pty = {
                        let mut st = self.lock_state();
                        let sz = st.registry.note_activity(name, key, now_ms());
                        let active = st.registry.session(name).and_then(|s| s.active);
                        let Some(rt) = rt_of_mut(&mut st, name, sess_gen) else { return };
                        // 已被收尾/腾位的腿不得再向 PTY 注入（评审 H3：写者收线后
                        // 读循环可能还有在途帧）
                        if !rt.legs.iter().any(|l| l.key == key) {
                            return;
                        }
                        apply_active_theme_locked(rt, active);
                        if let Some(sz) = sz {
                            apply_size_locked(rt, sz);
                            self.sentinel_repaint_locked(rt);
                        }
                        Arc::clone(&rt.pty)
                    };
                    if !f.payload.is_empty() && !pty.write_input(&f.payload) {
                        return; // PTY 写失败：收尾走 client_closed 同款摘腿
                    }
                }
                Op::RESIZE => {
                    let (cols, rows) = match frames::dec_resize(&f.payload) {
                        Ok(v) => v,
                        Err(e) => {
                            out.enqueue(WriteItem::new(Op::ERROR, frames::enc_error("bad_resize", &e.to_string())), 0);
                            continue;
                        }
                    };
                    // F1b：`RESIZE 0×0` 忽略本次上报（Go 的会话尺寸写点在 0 门**之后**；
                    // 旧实现先写会话几何再被 apply 的 0 门放空 ⇒ 几何被污染成 0×0，
                    // LIST/ATTACHED/surface 体全 0 而格流仍按 vt 宽编 = 客户端错位）。
                    // 超限夹取（F1a）：不发起巨型分配。
                    let Some(size) = Size::from_report(cols, rows) else {
                        let mut st = self.lock_state();
                        if let Some(rt) = rt_of_mut(&mut st, name, sess_gen) {
                            rt.zero_resize_ignored += 1;
                            let n = rt.zero_resize_ignored;
                            if n <= 3 || n.is_multiple_of(100) {
                                self.logf(&format!(
                                    "term: 会话 {name} 忽略 RESIZE 0×0 上报 {n} 次（会话几何保持）"
                                ));
                            }
                        }
                        continue;
                    };
                    let clamped = !Size::is_exact(cols, rows);
                    let mut st = self.lock_state();
                    let _ = st.registry.leg_resize(name, key, size);
                    let sz = st.registry.note_activity(name, key, now_ms());
                    let active = st.registry.session(name).and_then(|s| s.active);
                    if let Some(rt) = rt_of_mut(&mut st, name, sess_gen) {
                        apply_active_theme_locked(rt, active);
                        if let Some(sz) = sz {
                            apply_size_locked(rt, sz);
                            self.sentinel_repaint_locked(rt);
                            if clamped {
                                self.logf(&format!(
                                    "term: 会话 {name} 尺寸夹取 {cols}x{rows} → {sz}（上限 {}x{}）",
                                    Size::MAX_COLS,
                                    Size::MAX_ROWS
                                ));
                            }
                        }
                    }
                }
                // surface 族上行帧只认 surface 腿（Go 同款 `if client.surface` 门——
                // raw 腿可注入按键/抢占 FETCH 应答，评审 A4）
                Op::INPUT if self.leg_is_surface(name, key) => {
                    self.handle_input(name, sess_gen, key, &f.payload, &out)
                }
                Op::THEME if self.leg_is_surface(name, key) => self.handle_theme(name, key, &f.payload),
                Op::CLIPBOARD if self.leg_is_surface(name, key) => {
                    self.handle_clipboard_answer(name, key, &f.payload)
                }
                Op::FETCH_ROWS if self.leg_is_surface(name, key) => {
                    self.handle_fetch_rows(name, key, &f.payload, &out)
                }
                Op::FETCH_SNAPSHOT if self.leg_is_surface(name, key) => {
                    self.handle_fetch_snapshot(name, key)
                }
                // 非 surface 腿的 surface 族帧：**静默丢弃**（Go 只有 `if client.surface`
                // 无 else——评审 P5：多回一帧 ERROR 是 Go 没有的行为）
                Op::INPUT | Op::THEME | Op::CLIPBOARD | Op::FETCH_ROWS | Op::FETCH_SNAPSHOT => {}
                Op::KILL => {
                    let name_k = match frames::dec_name(&f.payload) {
                        Ok(n) => n,
                        Err(e) => {
                            out.enqueue(WriteItem::new(Op::ERROR, frames::enc_error("bad_name", &e.to_string())), 0);
                            continue;
                        }
                    };
                    match self.kill(&name_k) {
                        Ok(()) => {
                            out.enqueue(WriteItem::new(Op::OK, Vec::new()), 0);
                        }
                        Err(e) => {
                            out.enqueue(WriteItem::new(Op::ERROR, frames::enc_error(e.code.as_str(), &e.msg)), 0);
                        }
                    }
                }
                Op::LIST => {
                    let json = self.list_json();
                    out.enqueue(WriteItem::new(Op::LIST, json.into_bytes()), 0);
                }
                Op::EXPLAIN => {
                    let name_e = match frames::dec_name(&f.payload) {
                        Ok(n) => n,
                        Err(e) => {
                            out.enqueue(WriteItem::new(Op::ERROR, frames::enc_error("bad_name", &e.to_string())), 0);
                            continue;
                        }
                    };
                    match self.explain_json(&name_e) {
                        Ok(json) => {
                            out.enqueue(WriteItem::new(Op::EXPLAIN, json.into_bytes()), 0);
                        }
                        Err(e) => {
                            out.enqueue(WriteItem::new(Op::ERROR, frames::enc_error(e.code.as_str(), &e.msg)), 0);
                        }
                    }
                }
                op => {
                    // 错误回执也走腿队列（每腿唯一写者 = 帧组原子性）
                    out.enqueue(
                        WriteItem::new(Op::ERROR, frames::enc_error("bad_op", &format!("未知帧 0x{:02x}", op.0))),
                        0,
                    );
                }
            }
        }
    }

    // ---- 上行帧处理（surface 族）----

    /// 抽象输入：按 vt 真实模式编码写 PTY（任务 2.7；输入 = 活动，不注入哨兵）。
    fn handle_input(self: &Arc<Self>, name: &str, sess_gen: u64, key: LegKey, payload: &[u8], out: &Arc<LegOut>) {
        let ev = match frames::dec_input(payload) {
            Ok(e) => e,
            Err(e) => {
                out.enqueue(WriteItem::new(Op::ERROR, frames::enc_error("bad_input", &e.to_string())), 0);
                return;
            }
        };
        let (pty, encoded) = {
            let mut st = self.lock_state();
            let sz = st.registry.note_activity(name, key, now_ms());
            let active = st.registry.session(name).and_then(|s| s.active);
            let Some(rt) = rt_of_mut(&mut st, name, sess_gen) else { return };
            apply_active_theme_locked(rt, active);
            if let Some(sz) = sz {
                // 输入也可能改会话尺寸（另一条不同尺寸的腿刚改过——FIX-26），但不注入哨兵
                apply_size_locked(rt, sz);
            }
            if rt.stopped.load(Ordering::Relaxed) {
                return;
            }
            // F1：按腿取本腿键编码口径（与 `leg_is_surface` 同形态）。查不到腿 =
            // `end_leg` 竞态 ⇒ 丢弃该输入 + 计数；**任何路径不得回落 `host_default()`**
            //（防「默认宿主隐式回潮」——纪律见设计 F1-③）。
            let leg_flavor = rt.legs.iter().find(|l| l.key == key).map(|l| l.key_flavor);
            let Some(leg_flavor) = leg_flavor else {
                // 代码门 L5：丢弃面可观测（首 1 次 + 每 100 次——有界、可归因）
                let n = self.leg_missing_input_drops.fetch_add(1, Ordering::Relaxed) + 1;
                if n == 1 || n.is_multiple_of(100) {
                    self.logf(&format!(
                        "term: 会话 {name} 收到无主腿输入（腿已断——end_leg 竞态），已丢弃 n={n}"
                    ));
                }
                return;
            };
            let Some(vt) = rt.vt.as_ref() else { return };
            let enc = match &ev {
                frames::InputEvent::Key { key, mods, action, text } => {
                    let Some(action) = super::keyenc::KeyAction::from_wire(*action) else {
                        return;
                    };
                    vt.encode_key(
                        &super::keyenc::KeyEvent {
                            key: super::keyenc::Key(*key),
                            action,
                            mods: super::keyenc::Mods(*mods),
                            text,
                            composing: false,
                        },
                        leg_flavor,
                    )
                }
                frames::InputEvent::Text { flags, text } => {
                    let paste = flags & frames::text_bits::PASTE != 0;
                    let more = flags & frames::text_bits::MORE != 0;
                    let cont = flags & frames::text_bits::CONT != 0;
                    let bracketed = vt.modes().bracketed_paste;
                    if !paste || !bracketed {
                        text.clone().into_bytes()
                    } else {
                        super::keyenc::encode_paste_part(text.as_bytes(), true, !cont, !more)
                            .unwrap_or_default()
                    }
                }
                frames::InputEvent::Mouse { action, button, mods, x, y } => {
                    let Some(action) = super::keyenc::MouseAction::from_wire(*action) else {
                        return;
                    };
                    vt.encode_mouse(&super::keyenc::MouseEvent {
                        action,
                        button: super::keyenc::MouseButton(*button),
                        mods: super::keyenc::Mods(*mods),
                        x: *x,
                        y: *y,
                    })
                }
                frames::InputEvent::Focus { gained } => {
                    if !vt.modes().focus_events {
                        Vec::new() // 程序没开焦点上报：写进去就是垃圾字节
                    } else {
                        super::keyenc::encode_focus(*gained)
                    }
                }
            };
            (Arc::clone(&rt.pty), enc)
        };
        if !encoded.is_empty() && !pty.write_input(&encoded) {
            self.end_leg(name, key, i32::MIN, "", "ptmx_write_failed");
        }
    }

    /// 主题上报：记到腿上，active 腿的主题落到会话 vt。
    fn handle_theme(&self, name: &str, key: LegKey, payload: &[u8]) {
        let Ok((fg, bg, _dark)) = super::codec::dec_theme(payload) else {
            return; // 主题是尽力而为的通道：坏帧不报错
        };
        let mut st = self.lock_state();
        let is_active = st.registry.session(name).is_some_and(|s| s.active == Some(key));
        let Some(rt) = st.sessions.get_mut(name) else { return };
        if let Some(l) = rt.legs.iter_mut().find(|l| l.key == key) {
            l.theme_known = true;
            l.theme = (fg, bg);
        }
        if is_active {
            if let Some(vt) = rt.vt.as_mut() {
                vt.set_default_colors(fg, bg);
            }
        }
    }

    /// 客户端对剪贴板读请求的应答：记缓存，active 腿的缓存即读请求的应答源。
    fn handle_clipboard_answer(&self, name: &str, key: LegKey, payload: &[u8]) {
        let Ok(text) = super::codec::dec_clipboard_answer(payload) else {
            return;
        };
        let text = String::from_utf8_lossy(&text[..text.len().min(CLIP_MAX_BYTES)]).into_owned();
        let mut st = self.lock_state();
        let is_active = st.registry.session(name).is_some_and(|s| s.active == Some(key));
        let Some(rt) = st.sessions.get_mut(name) else { return };
        if let Some(l) = rt.legs.iter_mut().find(|l| l.key == key) {
            l.clip_cache = Some(text.clone());
        }
        if is_active {
            rt.clip_cache = Some(text);
        }
    }

    /// FETCH-ROWS：锁内取行、锁外建帧入队（应答走同一队列——帧组原子性）。
    fn handle_fetch_rows(self: &Arc<Self>, name: &str, key: LegKey, payload: &[u8], out: &Arc<LegOut>) {
        let mut req = match super::codec::dec_fetch_rows_req(payload) {
            Ok(r) => r,
            Err(e) => {
                out.enqueue(WriteItem::new(Op::ERROR, frames::enc_error("bad_fetch", &e.to_string())), 0);
                return;
            }
        };
        if req.count > FETCH_ROWS_MAX {
            req.count = FETCH_ROWS_MAX;
        }
        let built = {
            let mut guard = self.lock_state();
            let st = &mut *guard;
            let Some(rt) = st.sessions.get_mut(name) else { return };
            if rt.stopped.load(Ordering::Relaxed) {
                None
            } else if let Some(vt) = rt.vt.as_mut() {
                let geom = st.registry.session(name).map(|s| s.size).unwrap_or(Size::DEFAULT);
                let rev = rt
                    .legs
                    .iter()
                    .find(|l| l.key == key)
                    .and_then(|l| l.surface.as_ref())
                    .map(|s| s.revision)
                    .unwrap_or(0);
                let rows_enc = super::codec::encode_rows(&vt.rows_at(req.from, req.count as usize));
                Some((geom, rev, rows_enc))
            } else {
                None
            }
        };
        let Some((geom, rev, rows_enc)) = built else {
            out.enqueue(
                WriteItem::new(Op::ERROR, frames::enc_error("surface_unavailable", "本会话没有服务端 vt")),
                0,
            );
            return;
        };
        let reply = super::codec::FetchRowsReply {
            revision: rev,
            cols: geom.cols(),
            rows: geom.rows(),
            from: req.from,
            count: req.count,
            row_bytes: rows_enc,
        };
        let gz = super::codec::gzip_bytes(&super::codec::enc_fetch_rows_reply(&reply));
        let items: Vec<WriteItem> = super::codec::fragment_payload(&gz)
            .into_iter()
            .map(|frag| WriteItem::new(Op::FETCH_ROWS, frag))
            .collect();
        let hit = !reply.row_bytes.is_empty(); // Go noteFetchRows(len(enc)>0)：判行字节非空
        let cap = self.cfg.queue_bytes;
        let ok = {
            let st = self.lock_state();
            match st.sessions.get(name).and_then(|rt| rt.legs.iter().find(|l| l.key == key)) {
                Some(l) => l.out.enqueue_group(items, cap),
                None => false,
            }
        };
        let mut st = self.lock_state();
        if let Some(rt) = st.sessions.get_mut(name) {
            if let Some(l) = rt.legs.iter_mut().find(|l| l.key == key) {
                if let Some(s) = l.surface.as_mut() {
                    if ok {
                        if hit {
                            s.stats.fetch_hits += 1;
                        } else {
                            s.stats.fetch_miss += 1;
                        }
                    } else {
                        s.mark_need_snapshot("queue_overflow");
                    }
                }
            }
        }
    }

    /// FETCH-SNAPSHOT：revision 断档/病态补丁被拒后客户端要全量。
    fn handle_fetch_snapshot(&self, name: &str, key: LegKey) {
        let mut st = self.lock_state();
        let Some(rt) = st.sessions.get_mut(name) else { return };
        if let Some(l) = rt.legs.iter_mut().find(|l| l.key == key) {
            if let Some(s) = l.surface.as_mut() {
                s.mark_need_snapshot("client");
            }
        }
        rt.wake_surface();
    }

    // ---- 会话生命周期 ----

    /// HELLO 接入语义（Go attachOrCreate 四象限；创建路径先起运行态再进注册表，
    /// spawn 失败不留半截状态）。尺寸入径已归一（[`Size::normalized`]）。
    fn attach_or_create(
        self: &Arc<Self>,
        name: &str,
        cols: u16,
        rows: u16,
        create: bool,
        only_if_absent: bool,
    ) -> Result<(), TermError> {
        let size = Size::normalized(cols, rows);
        let mut st = self.lock_state();
        if st.registry.session(name).is_none() && create {
            if st.registry.session_names().len() >= self.cfg.max_sessions {
                return Err(TermError::new(
                    super::session::TermErrorCode::TooMany,
                    format!("会话数已达上限 {}，请先关闭一些会话", self.cfg.max_sessions),
                ));
            }
            self.spawn_session_locked(&mut st, name, size)?;
            // 注册表建立（80x24 归一——Go spawnLocked 同款；此后腿接入尺寸归选举）
            return st.registry.attach_or_create(name, size, true, false).map(|_| ());
        }
        st.registry.attach_or_create(name, size, create, only_if_absent).map(|_| ())
    }

    /// `CREATE` 不接入（Go createOnly，`new -d`）：不动尺寸、不产生腿、不触发哨兵/焦点。
    fn create_only(self: &Arc<Self>, name: &str, reuse_if_exists: bool) -> Result<(), TermError> {
        let mut st = self.lock_state();
        if st.registry.session(name).is_some() {
            return st.registry.create_only(name, reuse_if_exists);
        }
        if st.registry.session_names().len() >= self.cfg.max_sessions {
            return st.registry.create_only(name, false);
        }
        self.spawn_session_locked(&mut st, name, Size::DEFAULT)?;
        let r = st.registry.attach_or_create(name, Size::DEFAULT, true, false).map(|_| ());
        if r.is_ok() {
            self.logf(&format!("term: 创建会话 {name}（不接入，默认尺寸）"));
        }
        r
    }

    /// 起一个会话（调用方持服务锁；PTY spawn 是毫秒级 fork——Go spawnLocked 同在锁内）。
    fn spawn_session_locked(
        self: &Arc<Self>,
        st: &mut State,
        name: &str,
        size: Size,
    ) -> Result<(), TermError> {
        let (cols, rows) = (size.cols(), size.rows());
        let spawned = super::pty::spawn(name, cols, rows, self.cfg.shell.as_deref()).map_err(|e| {
            TermError::new(super::session::TermErrorCode::SpawnFailed, e)
        })?;
        let (cols, rows) = (spawned.cols, spawned.rows);
        let shell = spawned.shell.clone();
        let pid = spawned.pid;
        // PTY 拆分：reader 归 pump 线程；其余进共享面
        let mut inner = spawned;
        let reader = std::mem::replace(&mut inner.reader, Box::new(std::io::empty()));
        let pty = Arc::new(PtyShared { inner: Mutex::new(inner) });

        let (resp_tx, resp_rx) = std::sync::mpsc::sync_channel::<PtyInject>(RESP_QUEUE_LEN);
        let (clip_tx, clip_rx) = std::sync::mpsc::sync_channel::<String>(8);
        let vt = if vt_disabled_by_env() {
            self.logf(&format!("term: 会话 {name} 无服务端 vt（HOMEWAY_TERM_VT=off）→ 该会话仅 legacy 原始字节模式"));
            None
        } else {
            match SessionVt::new(cols, rows, self.cfg.scrollback_lines) {
                Ok(v) => Some(v),
                // 归一后本分支**不可达**（`Size::normalized` 入径 + `pty::spawn` 的 0→80/24
                // 归一回填）：设计 §2 F1a（评审 1.7）的 legacy-only 退化只对**未来直连
                // 调用点**是活路径；组件层硬拒由 vt.rs 单测直接覆盖（代码门 M1）。
                Err(e) => {
                    self.logf(&format!("term: 会话 {name} 无服务端 vt（{e}）→ 该会话仅 legacy 原始字节模式"));
                    None
                }
            }
        };
        let sess_gen = st.next_sess_gen;
        st.next_sess_gen += 1;
        let mut rt = SessRt {
            name: name.to_string(),
            gen: sess_gen,
            created_ms: now_ms(),
            pty: Arc::clone(&pty),
            ring: OutputRing::new(self.cfg.history),
            scan: TermScan::new(),
            vt,
            legs: Vec::new(),
            writers: 0,
            agent: frames::agent::UNKNOWN,
            state_v2: frames::state_v2::UNKNOWN,
            hygiene: agent::Hygiene::default(),
            prev_cpu: None,
            prev_quiet: 0,
            content_seq: 0,
            last_scan_seq: 0,
            last_scan_proc: String::new(),
            out_buckets: Default::default(),
            last_active_ms: now_ms(),
            sentinel_count: 0,
            last_notified: String::new(),
            clip_cache: None,
            resp_tx,
            resp_dropped: Arc::new(AtomicU64::new(0)),
            clip_dropped: Arc::new(AtomicU64::new(0)),
            nudge_dropped: Arc::new(AtomicU64::new(0)),
            zero_resize_ignored: 0,
            surf_wake: Arc::new((Mutex::new(0), Condvar::new())),
            clip_tx,
            stopped: Arc::new(AtomicBool::new(false)),
        };
        rt.ring.note_size(Size::normalized(cols, rows));
        let surf_wake = Arc::clone(&rt.surf_wake);
        let stopped = Arc::clone(&rt.stopped);
        st.sessions.insert(name.to_string(), rt);
        self.logf(&format!("term: 新建会话 {name}（pid={pid} {cols}x{rows} shell={shell}）"));

        // 会话线程：pump（PTY 读）/ 应答写者 / surface 投递。
        // pump 起不来 = 无 reader（会话必死）——立即回收；另两条失败只降功能不致命
        //（应答尽力、surface 腿会被 surface_unavailable 门挡住），打告警行（评审 L6）。
        let svc = Arc::clone(self);
        let tname = name.to_string();
        let pump_spawn = std::thread::Builder::new()
            .name("term-pump".into())
            .spawn(move || {
                let ctx = format!("会话 {tname} ");
                if let Some(action) = guard_thread(ThreadRole::Pump, &ctx, &svc.logf, || {
                    svc.pump_loop(&tname, sess_gen, reader)
                }) {
                    debug_assert_eq!(action, PanicAction::FinalizePump);
                    // 主动收尾：无 reader 的会话必死（子进程可能未真退出——wait_bounded
                    // 超时内含 kill_force 兜底），不留僵尸；客户端可重建会话
                    svc.finalize_panicked_pump(&tname, sess_gen);
                }
            });
        if pump_spawn.is_err() {
            self.logf(&format!("term: 会话 {name} pump 线程起不来——立即回收"));
            st.sessions.remove(name);
            // 收尸挪出锁（调用方持服务锁——锁内不做 2s 阻塞等待；评审 P4b）
            std::thread::spawn(move || {
                pty.kill_start();
                let _ = pty.wait_bounded(Duration::from_secs(2));
            });
            return Err(TermError::new(
                super::session::TermErrorCode::SpawnFailed,
                format!("会话 {name} 的读泵线程起不来"),
            ));
        }
        let resp_pty = Arc::clone(&pty);
        let resp_stopped = Arc::clone(&stopped);
        let resp_logf = Arc::clone(&self.logf);
        let resp_name = name.to_string();
        if std::thread::Builder::new()
            .name("term-resp".into())
            .spawn(move || {
                let ctx = format!("会话 {resp_name} ");
                let inner_logf = Arc::clone(&resp_logf);
                let inner_name = resp_name.clone();
                if let Some(action) = guard_thread(ThreadRole::Resp, &ctx, &resp_logf, || {
                    Self::response_writer_loop(resp_rx, resp_pty, resp_stopped, inner_logf, inner_name)
                }) {
                    debug_assert_eq!(action, PanicAction::DropResp);
                    // 线程退出：查询应答降级（会话主体不受影响）
                }
            })
            .is_err()
        {
            self.logf(&format!("term: 会话 {name} 应答写者线程起不来（查询应答将不代答）"));
        }
        let svc = Arc::clone(self);
        let tname = name.to_string();
        if std::thread::Builder::new()
            .name("term-surface".into())
            .spawn(move || {
                let ctx = format!("会话 {tname} ");
                if let Some(action) = guard_thread(ThreadRole::Surface, &ctx, &svc.logf, || {
                    svc.surface_loop(&tname, sess_gen, clip_rx, surf_wake, stopped)
                }) {
                    debug_assert_eq!(action, PanicAction::DropSurface);
                    // surface 唯一投递面已死：全 surface 腿 finish_quit + 裸断（无 ENDED），
                    // 线程退出——app 见断连可重连
                    svc.surface_panicked(&tname, sess_gen);
                }
            })
            .is_err()
        {
            self.logf(&format!("term: 会话 {name} surface 投递线程起不来（surface 腿不可用）"));
        }
        Ok(())
    }

    /// 尺寸哨兵（外部入口）：sentinel → 真实尺寸，两次 SIGWINCH 逼 TUI 重绘。
    /// 带会话代数——注册与哨兵之间同名重建时不打新会话（评审 P8）。
    fn sentinel_repaint(&self, name: &str, sess_gen: u64) {
        let mut guard = self.lock_state();
        let st = &mut *guard;
        if st.registry.session(name).is_none_or(|s| s.done) {
            return;
        }
        let Some(rt) = st.sessions.get_mut(name).filter(|rt| rt.gen == sess_gen) else { return };
        self.sentinel_repaint_locked(rt);
    }

    /// 同上（调用方持锁 + rt 在手）。
    fn sentinel_repaint_locked(&self, rt: &mut SessRt) {
        if rt.stopped.load(Ordering::Relaxed) {
            return;
        }
        rt.sentinel_count += 1; // 低7 判据计数器：一次 attach 只应 +1
        let size = rt.size();
        let (cols, rows) = (size.cols(), size.rows());
        let sentinel = if cols > 1 { cols - 1 } else { cols + 1 };
        rt.pty.resize(sentinel, rows);
        rt.pty.resize(cols, rows);
    }

    /// 摘腿（幂等「只摘一次」）：注册表 + 运行态 + 派生（重选举/末腿 focus-out）+ 日志。
    fn end_leg(&self, name: &str, key: LegKey, code: i32, reason: &str, why: &'static str) {
        let mut guard = self.lock_state();
        let st = &mut *guard;
        let Some(outcome) = st.registry.end_leg(name, key, code, reason, why, now_ms()) else {
            return;
        };
        let Some(rt) = st.sessions.get_mut(name) else { return };
        let idx = rt.legs.iter().position(|l| l.key == key);
        let mut leg_kind = outcome.end.kind;
        let mut surf_stats: Option<SurfStats> = None;
        if let Some(i) = idx {
            let l = &rt.legs[i];
            leg_kind = l.kind;
            surf_stats = l.surface.as_ref().map(|s| s.stats);
            let l = rt.legs.remove(i);
            match &outcome.end.ended {
                Some((c, r)) => {
                    l.out.finish_ended(WriteItem::new(Op::ENDED, frames::enc_ended(*c, r)));
                }
                None => l.out.finish_quit(),
            }
        }
        match surf_stats {
            Some(s) => self.logf(&format!(
                "term: 会话 {} 腿断开（kind={} 原因={}）｜快照={} 差分={} 降级={} 背压={} 队列溢出={} 编码失败={} 分片={} 下行={}B FETCH 命中={} 落空={}",
                rt.name, leg_kind.as_str(), why, s.snapshots, s.diffs, s.degrades, s.backpressure,
                s.queue_overflow, s.encode_failed, s.fragments, s.bytes_out, s.fetch_hits, s.fetch_miss
            )),
            None => {
                self.logf(&format!("term: 会话 {} 腿断开（kind={} 原因={}）", rt.name, leg_kind.as_str(), why))
            }
        }
        // afterLegsChanged 派生面
        if outcome.focus_out {
            // 末腿离开 → focus-out（TUI 停动画）。F3：入每会话 PTY 注入队列（锁内 try_send，
            // 顺序 = 状态迁移序；实际写在 response_writer_loop 锁外做——锁内不做阻塞 I/O）
            if let Some(bytes) = Self::focus_nudge_bytes(rt, false) {
                enqueue_pty_inject(rt, bytes, &self.logf);
            }
        }
        // 摘腿重选举 ⇒ 新 active 腿的主题/剪贴板回落（评审 P3：re_elected 的读者；
        // 旧腿已摘出 rt.legs，正好取到新 active）
        if outcome.re_elected.is_some() {
            let active = {
                // rt 是 st.sessions 的可变借用——active 经临时拆借（先算好再进 rt 块）
                let reg = st.registry.session(name).and_then(|sm| sm.active);
                reg
            };
            apply_active_theme_locked(rt, active);
        }
        if let Some(sz) = outcome.size_applied {
            apply_size_locked(rt, sz);
            self.sentinel_repaint_locked(rt);
        }
    }

    /// 焦点事件注入的**判据**（调用方持锁）：需要注入时返回字节（F3）。
    ///
    /// 仅当 TUI 开了 `?1004`（不开的程序读到这些字节只会当普通输入）。字节**不在这里写**
    /// ——写经每会话 PTY 注入队列（`resp_tx`，由 `response_writer_loop` 锁外消费），
    /// 入队由调用方在锁内 `try_send`（非阻塞）⇒ 队列序 = 状态迁移序（消除「出锁写」的
    /// 时序竞态：末腿 focus-out 与并发 attach 的 focus-in 不会交错）。
    /// 队列满时丢弃并计数（F6 观测面）；focus-in 被丢的降级 = 尺寸哨兵兜底
    /// （少数 TUI 需按键恢复），focus-out 被丢只影响省电语义。
    fn focus_nudge_bytes(rt: &SessRt, focus_in: bool) -> Option<&'static [u8]> {
        nudge_bytes_for(
            rt.stopped.load(Ordering::Relaxed),
            rt.scan.modes() & super::codec::mode_bits::FOCUS != 0,
            focus_in,
        )
    }

    /// kill 主动结束（SIGHUP → 宽限 → SIGKILL）；ENDED 由 pump 收尾路径发。
    fn kill(&self, name: &str) -> Result<(), TermError> {
        let pty = {
            let mut st = self.lock_state();
            st.registry.kill_mark(name)?;
            match st.sessions.get(name) {
                Some(rt) => Arc::clone(&rt.pty),
                None => {
                    // kill_mark 已过（存在且未结束）⇒ 运行态必在——防御面按 no_session 报
                    return Err(TermError::new(
                        super::session::TermErrorCode::NoSession,
                        format!("会话 {name} 不存在"),
                    ));
                }
            }
        };
        pty.kill_start();
        let grace_pty = Arc::clone(&pty);
        let grace_stopped = {
            let st = self.lock_state();
            st.sessions.get(name).map(|rt| Arc::clone(&rt.stopped))
        };
        std::thread::spawn(move || {
            std::thread::sleep(KILL_GRACE);
            // 会话已收工（子进程已死、宽限内自然退出）则不补发——pid 回收误伤面（D1）
            if let Some(stopped) = &grace_stopped {
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
            }
            grace_pty.kill_force();
        });
        self.logf(&format!("term: 关闭会话 {name}（pid={}）", pty.pid()));
        Ok(())
    }

    /// 会话收尾（ENDED 送达 + 资源回收四步：等子进程 → 关 master → 删注册表 → 释放历史）。
    fn finish_session(&self, name: &str, reason: FinishReason) {
        let (pty, remove_now) = {
            let mut st = self.lock_state();
            let ends = st.registry.finish(name, reason);
            if ends.is_empty() {
                // 幂等守卫之外的形态：已收尾过——只确保唤醒，不动回收面
                if let Some(rt) = st.sessions.get_mut(name) {
                    rt.stopped.store(true, Ordering::Relaxed);
                    rt.wake_surface();
                }
                return;
            }
            let Some(rt) = st.sessions.get_mut(name) else { return };
            rt.stopped.store(true, Ordering::Relaxed);
            rt.wake_surface();
            for e in &ends {
                apply_end_to_leg_rt(&self.logf, rt, e);
            }
            (Some(Arc::clone(&rt.pty)), rt.writers == 0)
        };
        // 子进程收尸（有界 + SIGKILL 兜底）——锁外；master 随 PtyShared 释放。
        // wait_bounded 内部只在超时才 kill_force——成功收尸后**不再补发**
        //（pid/pgid 可能已被系统回收，组信号会误伤无关进程——评审 D1）
        if let Some(pty) = pty {
            pty.kill_start();
            let _ = pty.wait_bounded(Duration::from_secs(2));
        }
        // 注册面回收；运行态在最后一个写者退出时移除（无写者则即刻——r5 M1）
        let mut st = self.lock_state();
        st.registry.remove_session(name);
        if remove_now && st.sessions.get(name).is_none_or(|rt| rt.writers == 0) {
            st.sessions.remove(name);
        }
    }

    /// pump 的自然退出收尾（子进程已死）：ENDED = 退出码 / killed ⇒ -2。
    fn finalize_exit(self: &Arc<Self>, name: &str, code: i32) {
        let killed = {
            let st = self.lock_state();
            st.registry.session(name).is_some_and(|s| s.killed)
        };
        let reason = if killed { FinishReason::Killed } else { FinishReason::Exit(code) };
        self.finish_session(name, reason);
    }

    /// pump 线程 panic 后的主动收尾（F7 处置表）：无 reader 的会话必死——有界收尸
    /// （超时内含 SIGKILL 兜底）后走统一 finish（ENDED 经各腿写者送达），不留僵尸。
    fn finalize_panicked_pump(self: &Arc<Self>, name: &str, sess_gen: u64) {
        let pty = {
            let st = self.lock_state();
            rt_of(&st, name, sess_gen).map(|rt| Arc::clone(&rt.pty))
        };
        let Some(pty) = pty else { return };
        let code = pty.wait_bounded(Duration::from_secs(2));
        self.finalize_exit(name, code);
    }

    /// surface 投递线程 panic 后的收尾（F7 处置表）：surface 唯一投递面已死 ⇒
    /// 全 surface 腿 finish_quit + 裸断（无 ENDED——客户端见断连可重连），线程退出。
    fn surface_panicked(&self, name: &str, sess_gen: u64) {
        let keys: Vec<LegKey> = {
            let st = self.lock_state();
            rt_of(&st, name, sess_gen)
                .map(|rt| rt.legs.iter().filter(|l| l.surface.is_some()).map(|l| l.key).collect())
                .unwrap_or_default()
        };
        for key in keys {
            self.leg_write_failed(name, key, "panicked");
        }
    }

    /// 写者退出计数；会话已收尾且这是最后一个写者 ⇒ 释放运行态（r5 M1：ENDED 前排干
    /// 窗口里环还在）。
    fn writer_exited(&self, name: &str, sess_gen: u64) {
        let mut st = self.lock_state();
        let stopped = rt_of(&st, name, sess_gen).is_some_and(|rt| rt.stopped.load(Ordering::Relaxed));
        if let Some(rt) = rt_of_mut(&mut st, name, sess_gen) {
            rt.writers = rt.writers.saturating_sub(1);
            if stopped && rt.writers == 0 {
                st.sessions.remove(name);
            }
        }
    }

    // ---- pump / 应答写者 / surface 投递 ----

    /// pump 常驻读 PTY：写历史、喂扫描器与 vt（收集应答/剪贴板事件）、唤醒各腿写者。
    /// 锁内只做入队/唤醒（design D5/B'）。
    fn pump_loop(self: &Arc<Self>, name: &str, sess_gen: u64, mut reader: Box<dyn Read + Send>) {
        let mut buf = vec![0u8; 32 << 10];
        loop {
            let n = match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            // 注入点放在首次读**成功之后**：测试要求腿已接入（ENDED 可达）才触发
            maybe_inject_panic(ThreadRole::Pump, name);
            let mut responses: Vec<Vec<u8>> = Vec::new();
            let mut clip_store: Option<String> = None;
            type ClipLoad = Arc<dyn Fn(&str) -> String + Send + Sync>;
            let mut clip_loads: Vec<ClipLoad> = Vec::new();
            #[allow(unused_assignments)]
            let mut clip_cache: Option<String> = None;
            let (clip_tx, resp_tx, resp_dropped, clip_dropped): (
                SyncSender<String>,
                SyncSender<PtyInject>,
                Arc<AtomicU64>,
                Arc<AtomicU64>,
            ) = {
                let mut guard = self.lock_state();
                let st = &mut *guard;
                let Some(rt) = rt_of_mut(st, name, sess_gen) else { break };
                if rt.stopped.load(Ordering::Relaxed) {
                    break;
                }
                rt.ring.append(&buf[..n]);
                rt.scan.write(&buf[..n]);
                // 应答让位（任务 5.1 窄规则）：capsRawTerminal 腿在场 ⇒ 服务端不代答
                //（按腿上的 caps 位计数——surface+rawCapable 组合同样让位，Go
                // rawTermLegs 计数器同义；评审 M4）
                let suppress = rt.legs.iter().any(|l| l.raw_capable);
                if let Some(vt) = rt.vt.as_mut() {
                    let mut resp: Vec<Vec<u8>> = Vec::new();
                    vt.write_collecting(&buf[..n], &mut |r| {
                        if !suppress {
                            resp.push(r.to_vec());
                        }
                    });
                    responses = resp;
                    for ev in vt.take_clip_events() {
                        match ev {
                            ClipEvt::Store(t) => clip_store = Some(t),
                            ClipEvt::Load(f) => clip_loads.push(f),
                        }
                    }
                }
                clip_cache = rt.clip_cache.clone();
                let txs = (
                    rt.clip_tx.clone(),
                    rt.resp_tx.clone(),
                    Arc::clone(&rt.resp_dropped),
                    Arc::clone(&rt.clip_dropped),
                );
                rt.content_seq += 1;
                let now = now_ms();
                rt.count_out(n, now);
                rt.last_active_ms = now;
                let any_surface = rt.any_surface();
                if any_surface {
                    rt.wake_surface();
                }
                if rt.scan.take_changed() {
                    push_state_to_legs(rt);
                    // 裸 OSC 9 通知转发（只发 surface 腿；双语义判别已在扫描器做完）
                    let text = rt.scan.notify().to_string();
                    if !text.is_empty() && text != rt.last_notified && any_surface {
                        rt.last_notified = text.clone();
                        let cap = self.cfg.queue_bytes;
                        for l in &rt.legs {
                            if l.surface.is_some() {
                                l.out.enqueue(
                                    WriteItem::new(Op::NOTIFY, super::codec::enc_notify(text.as_bytes())),
                                    cap,
                                );
                            }
                        }
                    }
                }
                for l in &rt.legs {
                    if l.surface.is_none() {
                        l.out.wake_writer();
                    }
                }
                txs
            };
            // 锁外投递：查询应答（FIX-25）+ 剪贴板读应答 + 剪贴板写转发。
            // F6：队列满 ⇒ 计数 + 节流日志（原实现只写不读，观测面接上）
            let send = |bytes: Vec<u8>| {
                try_send_or_count(
                    &resp_tx,
                    PtyInject::Resp(bytes),
                    &resp_dropped,
                    &self.logf,
                    name,
                    "查询应答",
                    "（队列满）",
                );
            };
            for r in responses {
                send(r);
            }
            for f in clip_loads {
                if let Some(cache) = &clip_cache {
                    send(f(cache).into_bytes());
                }
            }
            if let Some(text) = clip_store {
                // 满/无消费者：宁可拒绝，也不阻塞 VT 流（F6：丢弃计数 + 节流日志）
                if clip_tx.try_send(text).is_err() {
                    count_drop(&clip_dropped, &self.logf, name, "剪贴板写", "");
                }
            }
        }
        // 子进程退出 / PTY 关闭：收尸 → 统一收尾（ENDED + 回收四步）
        let code = {
            let st = self.lock_state();
            match rt_of(&st, name, sess_gen) {
                Some(rt) => Arc::clone(&rt.pty),
                None => return, // 会话已被服务关停路径收走（或同名重建——身份不符）
            }
        };
        let code = code.wait_bounded(Duration::from_secs(2));
        self.finalize_exit(name, code);
    }

    /// 应答写者（FIX-25）：独占消费每会话 PTY 注入队列（查询应答 + 焦点 nudge，F3）。
    /// 退出 = 会话收工或 PTY 写失败。nudge 写失败的日志行文与旧实现同串（判据面不变）。
    fn response_writer_loop(
        rx: Receiver<PtyInject>,
        pty: Arc<PtyShared>,
        stopped: Arc<AtomicBool>,
        logf: Logf,
        name: String,
    ) {
        loop {
            match rx.recv_timeout(SAMPLE_PERIOD) {
                Ok(item) => {
                    let is_nudge = matches!(item, PtyInject::Nudge(_));
                    let ok = match &item {
                        PtyInject::Resp(b) => pty.write_input(b),
                        PtyInject::Nudge(b) => pty.write_input(b),
                    };
                    if !ok {
                        if is_nudge {
                            logf(&format!("term: 会话 {name} focus nudge 写入失败"));
                        }
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if stopped.load(Ordering::Relaxed) {
                        return;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    /// surface 投递循环（每会话一条）：合并窗 → flush；剪贴板写立即下发（不并窗）。
    ///
    /// 唤醒是**代数**（u32 单调递增，每会话独立、从 0 起）：等待方与自己上次见过的
    /// 代数比——首拍 `seen=0` 刻意落后：线程启动前发生的唤醒（attach 的首拍快照）
    /// 不丢（经典丢唤醒竞态：以「拿锁时刻的代数」为基准则吞掉先到的通知，静默会话
    /// 的首拍快照就永远不来；代价只是启动时一次多余的 flush 尝试——无 surface 腿时
    /// 早退，无害）。
    fn surface_loop(
        self: &Arc<Self>,
        name: &str,
        sess_gen: u64,
        clip_rx: Receiver<String>,
        surf_wake: Arc<(Mutex<u32>, Condvar)>,
        stopped: Arc<AtomicBool>,
    ) {
        let read_gen = |w: &Arc<(Mutex<u32>, Condvar)>| {
            let (m, _) = &**w;
            *m.lock().unwrap_or_else(|e| e.into_inner())
        };
        let mut seen = 0u32; // 见上：刻意从 0 起——启动前的唤醒不丢
        loop {
            // 等一个「要发东西」的事件：surface 唤醒 / 收工（500ms 轮询）；
            // 剪贴板写随时插队（转发后继续等——它不触发 flush，Go 的 clipChan 分支同义）。
            loop {
                if stopped.load(Ordering::Relaxed) {
                    return;
                }
                let woke = {
                    let (m, cv) = &*surf_wake;
                    let g = m.lock().unwrap_or_else(|e| e.into_inner());
                    if *g != seen {
                        true
                    } else {
                        let (g2, _) = cv
                            .wait_timeout(g, Duration::from_millis(500))
                            .unwrap_or_else(|e| e.into_inner());
                        *g2 != seen
                    }
                };
                while let Ok(text) = clip_rx.try_recv() {
                    self.forward_clipboard(name, sess_gen, &text);
                }
                if woke {
                    break;
                }
            }
            if stopped.load(Ordering::Relaxed) {
                return;
            }
            seen = read_gen(&surf_wake);
            // 合并窗：窗内再来唤醒就顺延到上限，把高频小输出并成一帧（16–33ms）
            let start = Instant::now();
            let mut last = start;
            loop {
                let now = Instant::now();
                if now.duration_since(start) >= MERGE_WINDOW_MAX {
                    break;
                }
                let wait_until = last + MERGE_WINDOW_MIN;
                if wait_until >= start + MERGE_WINDOW_MAX {
                    break;
                }
                let timeout = wait_until.saturating_duration_since(Instant::now());
                let (m, cv) = &*surf_wake;
                let g = m.lock().unwrap_or_else(|e| e.into_inner());
                let g0 = *g;
                let (g2, _) = cv.wait_timeout(g, timeout).unwrap_or_else(|e| e.into_inner());
                let woke = *g2 != g0;
                drop(g2);
                while let Ok(text) = clip_rx.try_recv() {
                    self.forward_clipboard(name, sess_gen, &text);
                }
                if !woke {
                    break; // MIN 窗内无新唤醒 ⇒ flush
                }
                last = Instant::now();
            }
            self.flush_surface(name, sess_gen);
        }
    }

    /// 剪贴板写（OSC 52）：立即下发，只发 surface 腿（raw 腿字节流自带该序列）。
    fn forward_clipboard(&self, name: &str, sess_gen: u64, text: &str) {
        let mut st = self.lock_state();
        let Some(rt) = rt_of_mut(&mut st, name, sess_gen) else { return };
        let cap = self.cfg.queue_bytes;
        for l in &rt.legs {
            if l.surface.is_some() {
                l.out.enqueue(
                    WriteItem::new(Op::CLIPBOARD, super::codec::enc_clipboard(0, text.as_bytes())),
                    cap,
                );
            }
        }
    }

    /// surface 一拍（Go flushSurface）：锁内取快照/建体、锁外压缩入队。
    fn flush_surface(self: &Arc<Self>, name: &str, sess_gen: u64) {
        let pendings: Vec<Pending> = {
            let mut guard = self.lock_state();
            let st = &mut *guard;
            if st.registry.session(name).is_none_or(|s| s.done) {
                return;
            }
            let geom = st.registry.session(name).map(|s| s.size).unwrap_or(Size::DEFAULT);
            let Some(rt) = rt_of_mut(st, name, sess_gen) else { return };
            if !rt.any_surface() {
                return;
            }
            let Some(vt) = rt.vt.as_mut() else { return };
            let (cols, rows) = (geom.cols(), geom.rows());
            let title = rt.scan.title().to_string();
            // 回滚回落检测（noteScrollbar 在 takeSnapshotFlag 之前——本拍消费）
            let cur_sb = ScrollbarWire::from(vt.scrollbar());
            for l in &mut rt.legs {
                if let Some(s) = l.surface.as_mut() {
                    if s.note_scrollbar(cur_sb) {
                        self.logf(&format!(
                            "term: 会话 {} 回滚条非平移回落 ⇒ 强制全量重建（行号语义已变）",
                            rt.name
                        ));
                    }
                }
            }
            // 每拍取一次脏行（所有 surface 腿共享；count 可为 0——光标/模式位走腿基线判定）
            let _ = vt.update();
            let dirty = vt.dirty_rows();
            let count = dirty.len();
            let enc = if count > 0 { super::codec::encode_rows(&dirty) } else { Vec::new() };
            let sb = vt.scrollbar();
            let (modes, kitty, misc) = super::codec::surface_modes_of(&vt.modes());
            let st_tick = SurfState {
                cursor: super::codec::surface_cursor_of(&vt.cursor()),
                modes,
                total: sb.total,
                offset: sb.offset,
                len: sb.len as u16,
                alt: modes & super::codec::mode_bits::ALT_SCREEN != 0,
            };
            let mut snapshot_mat: Option<(Vec<u8>, Vec<u8>)> = None; // (grid, mirror)
            let mut out: Vec<Pending> = Vec::new();
            for l in rt.legs.iter_mut() {
                let Some(s) = l.surface.as_mut() else { continue };
                let mut force = s.take_snapshot_flag() || s.under_pressure();
                if !force && (!s.has_base || st_tick.alt != s.base.alt) {
                    s.stats.degrades += 1; // 首次基线缺失 / 备用屏进出 ⇒ 该腿全量
                    force = true;
                }
                if force {
                    let mat = snapshot_mat.get_or_insert_with(|| {
                        // F2：快照材质提交全屏指纹（下发成功后增量不再重复带已发过的行）
                        let grid = super::codec::encode_grid(cols, rows, &vt.rows_and_commit());
                        // F1c：镜像窗口 = min(rows × MIRROR_VIEWPORTS, 字节预算反推的行数)
                        let mirror_rows = vt.mirror_rows(mirror_window_rows(cols, rows));
                        let mirror = if mirror_rows.is_empty() {
                            Vec::new()
                        } else {
                            super::codec::encode_grid(cols, mirror_rows.len() as u16, &mirror_rows)
                        };
                        (grid, mirror)
                    });
                    let rev = s.next_revision();
                    // 回滚条基线不在这里记（基线 8167cb7 起）：noteSentScrollbar 在入队
                    // 成功后统一推进（本函数只建体；入队失败时基线不能前移，否则回落检测漏判）。
                    let body = super::codec::enc_snapshot_body(&SnapshotBody {
                        revision: rev,
                        cols,
                        rows,
                        cursor: st_tick.cursor,
                        modes: st_tick.modes,
                        kitty,
                        misc,
                        title: title.clone(),
                        scroll: ScrollbarWire {
                            total: st_tick.total,
                            offset: st_tick.offset,
                            len: st_tick.len,
                        },
                        grid: mat.0.clone(),
                        mirror: mat.1.clone(),
                    });
                    out.push(Pending { key: l.key, op: Op::SNAPSHOT, body, st: st_tick });
                } else {
                    if count == 0 && s.state_unchanged(&st_tick) {
                        continue; // 无脏行且该腿基线无差异 ⇒ 整拍跳过
                    }
                    let body = super::codec::enc_diff_body(&DiffBody {
                        revision: s.revision,
                        cols,
                        rows,
                        cursor: st_tick.cursor,
                        modes: st_tick.modes,
                        scroll: ScrollbarWire {
                            total: st_tick.total,
                            offset: st_tick.offset,
                            len: st_tick.len,
                        },
                        row_bytes: enc.clone(),
                        row_count: count as u16,
                    });
                    out.push(Pending { key: l.key, op: Op::SURFACE_DIFF, body, st: st_tick });
                }
            }
            if !out.is_empty() {
                vt.clean(); // 乐观消费：入队/写失败由 needSnapshot 全量兜底
            }
            out
        };
        // 锁外：gzip/分片/入队（两种失败模式分开：单帧超上限 vs 队列积压超上限）
        for p in pendings {
            if p.body.len() > self.cfg.pending_cap {
                // 失败模式一（单帧超上限）：标记需全量、下一拍重试
                self.mark_leg_surface(name, p.key, |s| s.mark_need_snapshot("backpressure"));
                continue;
            }
            let gz = super::codec::gzip_bytes(&p.body);
            let mut items: Vec<WriteItem> = super::codec::fragment_payload(&gz)
                .into_iter()
                .map(|frag| WriteItem::new(p.op, frag))
                .collect();
            if p.op == Op::SNAPSHOT {
                // 完成标志与分片同组入队（快照语义上是一段连续字节）
                items.push(WriteItem::new(Op::SNAPSHOT_DONE, frames::enc_replay_done(p.body.len() as u32, 0)));
            }
            let bytes: usize = items.iter().map(|i| i.payload.len()).sum();
            let frag_count = items.len() as u64;
            let queue_ok = self.with_leg_out(name, p.key, |out| out.enqueue_group(items, self.cfg.queue_bytes));
            if !queue_ok {
                // 失败模式二（队列积压超上限）：丢弃待发 + 标记需全量
                self.mark_leg_surface(name, p.key, |s| s.mark_need_snapshot("queue_overflow"));
                continue;
            }
            // 观测计数 + 基线提交（入队成功后）
            let mut st = self.lock_state();
            if let Some(rt) = st.sessions.get_mut(name) {
                if let Some(l) = rt.legs.iter_mut().find(|l| l.key == p.key) {
                    if let Some(s) = l.surface.as_mut() {
                        if p.op == Op::SNAPSHOT {
                            s.stats.snapshots += 1;
                        } else {
                            s.stats.diffs += 1;
                        }
                        s.stats.fragments += frag_count;
                        s.stats.bytes_out += bytes as u64;
                        s.base = p.st;
                        s.has_base = true;
                        // 回滚条基线随**成功入队的帧**推进（快照与差分都算）——noteScrollbar
                        // 的回落判据与客户端「上一帧已知的回滚条」对齐（基线 8167cb7）。
                        s.last_sent = ScrollbarWire {
                            total: p.st.total,
                            offset: p.st.offset,
                            len: p.st.len,
                        };
                        s.has_last_sent = true;
                    }
                }
            }
        }
    }

    /// 服务锁内对一条腿的 surface 状态做一次变更。
    fn mark_leg_surface(&self, name: &str, key: LegKey, f: impl FnOnce(&mut SurfaceLeg)) {
        let mut st = self.lock_state();
        if let Some(rt) = st.sessions.get_mut(name) {
            if let Some(l) = rt.legs.iter_mut().find(|l| l.key == key) {
                if let Some(s) = l.surface.as_mut() {
                    f(s);
                }
            }
        }
    }

    /// 服务锁内取腿队列引用执行一次操作。
    fn with_leg_out(&self, name: &str, key: LegKey, f: impl FnOnce(&LegOut) -> bool) -> bool {
        let st = self.lock_state();
        match st.sessions.get(name).and_then(|rt| rt.legs.iter().find(|l| l.key == key)) {
            Some(l) => f(&l.out),
            None => false,
        }
    }

    // ---- raw / surface 写者 ----

    /// raw 腿写者：握手（ATTACHED/清屏/回放/REPLAY-DONE）→ 实时（控制帧 + 字节环）。
    /// 停滞语义（任务 4.2 / D10-7）：写超时只标记落后并退避重试（恢复后从可用起点
    /// 续投），不得在超时分支 close 或发 ENDED；只有硬错误或连续停滞超上限才断腿
    /// （断腿不发 ENDED——客户端看到裸 EOF）。
    fn run_raw_writer(
        self: &Arc<Self>,
        name: &str,
        sess_gen: u64,
        key: LegKey,
        mut io: FrameIo,
        out: Arc<LegOut>,
        hs: RawHandshake,
    ) {
        maybe_inject_panic(ThreadRole::LegWriter, name);
        if !self.raw_write_frame(name, key, &mut io, &out, Op::ATTACHED, &hs.attached) {
            self.writer_exited(name, sess_gen);
            return;
        }
        if !self.raw_write_frame(name, key, &mut io, &out, Op::DATA, b"\x1b[3J\x1b[2J\x1b[H") {
            self.writer_exited(name, sess_gen);
            return; // 换屏前序：客户端 vt 是新建的，回放起点要确定
        }
        // 回放：锁内读环、锁外写；时间预算用尽就跳到实时（丢头部保尾部）。
        // cut_by_done：回放途中会话收尾时，实时起点留在断点——不跳过未送出的窗口。
        let mut sent = hs.start;
        let mut replayed: u32 = 0;
        let mut cut_by_done = false;
        let deadline = Instant::now() + hs.budget;
        while sent < hs.end {
            match self.next_ring_chunk(name, sess_gen, key, &mut sent) {
                NextChunk::Gone => {
                    cut_by_done = true;
                    break;
                }
                NextChunk::CaughtUp => break,
                NextChunk::Data(chunk) => {
                    if chunk.is_empty() {
                        break;
                    }
                    if !self.raw_write_frame(name, key, &mut io, &out, Op::DATA, &chunk) {
                        self.writer_exited(name, sess_gen);
                        return;
                    }
                    replayed = replayed.saturating_add(chunk.len() as u32);
                    if Instant::now() > deadline && sent < hs.end {
                        break; // 预算用尽：剩余历史直接跳过，从「现在」接实时流
                    }
                }
            }
        }
        let truncated = hs.truncated || sent < hs.end;
        let mut flags = 0u8;
        if truncated {
            flags |= frames::replay_flags::TRUNCATED;
        }
        if hs.epochs.iter().any(|e| *e > hs.start && *e < sent) {
            flags |= frames::replay_flags::SIZE_CHANGE;
        }
        if !self.raw_write_frame(
            name,
            key,
            &mut io,
            &out,
            Op::REPLAY_DONE,
            &frames::enc_replay_done(replayed, flags),
        ) {
            self.writer_exited(name, sess_gen);
            return;
        }
        let mut off = hs.end;
        if cut_by_done {
            off = sent;
        }
        if hs.nudge_focus {
            // 首腿：回放完成后注入 focus-in，逼 TUI 立即全屏重绘。F3：锁内入队即回
            //（try_send 非阻塞），实际 PTY 写在 response_writer_loop 锁外做。
            let mut st = self.lock_state();
            if let Some(rt) = rt_of_mut(&mut st, name, sess_gen) {
                if let Some(bytes) = Self::focus_nudge_bytes(rt, true) {
                    enqueue_pty_inject(rt, bytes, &self.logf);
                }
            }
        }
        // 实时循环：控制帧（STATE/ERROR/ENDED）优先、字节环随后。
        loop {
            let (items, ended, quit) = out.take();
            for it in &items {
                if !self.raw_write_frame(name, key, &mut io, &out, it.op, &it.payload) {
                    self.writer_exited(name, sess_gen);
                    return;
                }
            }
            if let Some(ended_payload) = ended {
                // ENDED 前必须把 [off, written) 排干（r5 M1：env 截断竞态的根因修复）
                if !self.drain_ring_before_end(name, sess_gen, key, &mut io, &out, &mut off) {
                    self.writer_exited(name, sess_gen);
                    return;
                }
                self.send_ended_stalled(name, key, &mut io, &out, &ended_payload);
                drop_stream(io);
                self.writer_exited(name, sess_gen);
                return;
            }
            if quit {
                drop_stream(io);
                self.writer_exited(name, sess_gen);
                return;
            }
            if !items.is_empty() {
                continue;
            }
            if out.is_stalled() {
                std::thread::sleep(self.stall_retry_backoff()); // 退避放写前：停一拍再试
            }
            match self.peek_ring_chunk(name, sess_gen, key, &mut off) {
                Peek::Gone => {
                    // 腿已被会话侧收尾：控制队列里可能还压着 ENDED——排空再退出
                    let (items, ended, _) = out.take();
                    for it in &items {
                        if !self.raw_write_frame(name, key, &mut io, &out, it.op, &it.payload) {
                            self.writer_exited(name, sess_gen);
                            return;
                        }
                    }
                    if let Some(ended_payload) = ended {
                        if !self.drain_ring_before_end(name, sess_gen, key, &mut io, &out, &mut off) {
                            self.writer_exited(name, sess_gen);
                            return;
                        }
                        self.send_ended_stalled(name, key, &mut io, &out, &ended_payload);
                    }
                    drop_stream(io);
                    self.writer_exited(name, sess_gen);
                    return;
                }
                Peek::CaughtUp => {
                    if out.is_stalled() {
                        continue; // 停滞中且暂无数据：回环再退避（不睡死在 wake 上）
                    }
                    out.wait(Duration::from_millis(500));
                }
                Peek::Data(chunk) => {
                    if !chunk.is_empty()
                        && self.raw_write_frame(name, key, &mut io, &out, Op::DATA, &chunk)
                    {
                        off += chunk.len() as u64; // 写成功才推进（停滞重试重写同一片）
                    } else if !chunk.is_empty() {
                        self.writer_exited(name, sess_gen);
                        return;
                    }
                }
            }
        }
    }

    /// 停滞退避节拍：默认 1s；停滞上限较短时自适应缩短到上限的 1/4（exec-r1 高2）。
    fn stall_retry_backoff(&self) -> Duration {
        let r = self.cfg.raw_stall_limit / 4;
        if !r.is_zero() && r < RAW_STALL_RETRY {
            r
        } else {
            RAW_STALL_RETRY
        }
    }

    /// 写一帧到 raw 腿（停滞感知）；false = 写者收工。
    #[allow(clippy::too_many_arguments)] // 写者路径的固定参数组（Go rawWriteFrame 同构）
    fn raw_write_frame(
        &self,
        name: &str,
        key: LegKey,
        io: &mut FrameIo,
        out: &Arc<LegOut>,
        op: Op,
        payload: &[u8],
    ) -> bool {
        match io.write_frame(op, payload, self.cfg.write_timeout) {
            Ok(()) => {
                if out.is_stalled() {
                    out.note_stall(false, self.cfg.raw_stall_limit);
                }
                true
            }
            Err(WriteFail::Timeout) => {
                // 停滞：标记落后、退避重试（D10-7：绝不能在这里 close/发 ENDED）
                if out.note_stall(true, self.cfg.raw_stall_limit) {
                    self.break_leg(name, key, "stalled_over_limit");
                    return false;
                }
                let kind = self.leg_kind_of(name, key);
                self.logf(&format!(
                    "term: 会话 {name} raw 腿（{kind}）写停滞，退避重试（不断腿）"
                ));
                true // 外层循环负责退避与续投
            }
            Err(e) => {
                self.logf(&format!("term: 会话 {name} raw 腿写失败断腿（{e}）"));
                self.leg_write_failed(name, key, "write_failed");
                false
            }
        }
    }

    /// ENDED 帧的停滞感知续投（「ENDED 先于 close」是任务 4.1 的硬要求，不吞错）。
    fn send_ended_stalled(&self, name: &str, key: LegKey, io: &mut FrameIo, out: &Arc<LegOut>, ended: &[u8]) {
        loop {
            match io.write_frame(Op::ENDED, ended, self.cfg.write_timeout) {
                Ok(()) => return,
                Err(WriteFail::Timeout) => {
                    if out.note_stall(true, self.cfg.raw_stall_limit) {
                        self.break_leg(name, key, "stalled_over_limit");
                        return;
                    }
                    std::thread::sleep(self.stall_retry_backoff());
                }
                Err(_) => return, // 硬错误：对端已不可达
            }
        }
    }

    /// 把本腿的 [off, written) 窗口全部写出（r5 M1）；false = 放弃（硬错误/停滞超限）。
    fn drain_ring_before_end(
        &self,
        name: &str,
        sess_gen: u64,
        key: LegKey,
        io: &mut FrameIo,
        out: &Arc<LegOut>,
        off: &mut u64,
    ) -> bool {
        loop {
            match self.peek_ring_chunk_final(name, sess_gen, off) {
                PeekFinal::Drained => return true,
                PeekFinal::Data(chunk) => match io.write_frame(Op::DATA, &chunk, self.cfg.write_timeout) {
                    Ok(()) => {
                        *off += chunk.len() as u64;
                    }
                    Err(WriteFail::Timeout) => {
                        if out.note_stall(true, self.cfg.raw_stall_limit) {
                            self.break_leg(name, key, "stalled_over_limit");
                            return false;
                        }
                        std::thread::sleep(self.stall_retry_backoff());
                    }
                    Err(_) => return false,
                },
            }
        }
    }

    /// 回放读环（推进 off；Gone = 腿已摘/会话已收工）。
    fn next_ring_chunk(&self, name: &str, sess_gen: u64, key: LegKey, off: &mut u64) -> NextChunk {
        let st = self.lock_state();
        let Some(rt) = rt_of(&st, name, sess_gen) else { return NextChunk::Gone };
        if st.registry.session(name).is_none_or(|s| s.done)
            || !rt.legs.iter().any(|l| l.key == key)
        {
            return NextChunk::Gone;
        }
        if *off < rt.ring.start() {
            *off = rt.ring.start(); // 落后被环覆盖：跳到可用起点（宁可丢也不阻塞）
        }
        if *off >= rt.ring.written() {
            return NextChunk::CaughtUp;
        }
        let chunk = rt.ring.read(*off, frames::DATA_CHUNK);
        *off += chunk.len() as u64;
        NextChunk::Data(chunk)
    }

    /// 实时读环（**不推进 off**——写出成功才由调用方推进；停滞重试要重写同一片）。
    /// 有界追赶：落后超过一个回放窗口就跳到 written-replay（跳头部保尾部，D5）。
    fn peek_ring_chunk(&self, name: &str, sess_gen: u64, key: LegKey, off: &mut u64) -> Peek {
        let st = self.lock_state();
        let Some(rt) = rt_of(&st, name, sess_gen) else { return Peek::Gone };
        if st.registry.session(name).is_none_or(|s| s.done)
            || !rt.legs.iter().any(|l| l.key == key)
        {
            return Peek::Gone;
        }
        if self.cfg.replay > 0 && rt.ring.written().saturating_sub(*off) > self.cfg.replay as u64 {
            *off = rt.ring.written() - self.cfg.replay as u64;
        }
        if *off < rt.ring.start() {
            *off = rt.ring.start();
        }
        if *off >= rt.ring.written() {
            return Peek::CaughtUp;
        }
        Peek::Data(rt.ring.read(*off, frames::DATA_CHUNK))
    }

    /// 终局读环（r5 M1）：不因 removed/done 拒绝——排干 [off, written)。
    fn peek_ring_chunk_final(&self, name: &str, sess_gen: u64, off: &mut u64) -> PeekFinal {
        let st = self.lock_state();
        let Some(rt) = rt_of(&st, name, sess_gen) else { return PeekFinal::Drained };
        if self.cfg.replay > 0 && rt.ring.written().saturating_sub(*off) > self.cfg.replay as u64 {
            *off = rt.ring.written() - self.cfg.replay as u64;
        }
        if *off < rt.ring.start() {
            *off = rt.ring.start();
        }
        if *off >= rt.ring.written() {
            return PeekFinal::Drained;
        }
        PeekFinal::Data(rt.ring.read(*off, frames::DATA_CHUNK))
    }

    /// 写失败（硬错误）= 断腿：摘除（幂等）+ 关连接；**不发 ENDED**（裸 EOF）。surface/raw 共用。
    fn leg_write_failed(&self, name: &str, key: LegKey, why: &'static str) {
        let st = self.lock_state();
        if let Some(rt) = st.sessions.get(name) {
            if let Some(l) = rt.legs.iter().find(|l| l.key == key) {
                l.out.finish_quit();
            }
        }
        drop(st);
        self.end_leg(name, key, i32::MIN, "", why);
    }

    /// 服务端主动断腿（停滞超限等）：同 leg_write_failed 的收尾路径（conn 由写者 drop）。
    fn break_leg(&self, name: &str, key: LegKey, why: &'static str) {
        self.leg_write_failed(name, key, why);
    }

    /// surface 腿写者：出队 → 写 → 收尾（写失败/超时 = 断腿，任务 4.3 只对 raw 引入停滞语义）。
    fn run_surface_writer(self: &Arc<Self>, name: &str, sess_gen: u64, key: LegKey, mut io: FrameIo, out: Arc<LegOut>) {
        maybe_inject_panic(ThreadRole::LegWriter, name);
        loop {
            let (items, ended, quit) = out.take();
            for it in &items {
                let started = Instant::now();
                match io.write_frame(it.op, &it.payload, WRITE_TIMEOUT) {
                    Ok(()) => {
                        // 写耗时上报（FIX-28）：超过合并窗 ⇒ underPressure ⇒ 下一拍走全量
                        let cost = started.elapsed();
                        self.mark_leg_surface(name, key, |s| s.last_write_cost = cost);
                    }
                    Err(WriteFail::Timeout) => {
                        self.mark_leg_surface(name, key, |s| {
                            s.stats.write_timeout += 1;
                            s.last_write_cost = started.elapsed();
                        });
                        self.leg_write_failed(name, key, "write_timeout");
                        drop_stream(io);
                        self.writer_exited(name, sess_gen);
                        return;
                    }
                    Err(e) => {
                        self.logf(&format!("term: 会话 {name} surface 腿写失败断腿（{e}）"));
                        self.mark_leg_surface(name, key, |s| s.stats.write_timeout += 1);
                        self.leg_write_failed(name, key, "write_failed");
                        drop_stream(io);
                        self.writer_exited(name, sess_gen);
                        return;
                    }
                }
            }
            if let Some(ended_payload) = ended {
                let _ = io.write_frame(Op::ENDED, &ended_payload, WRITE_TIMEOUT);
                drop_stream(io);
                self.writer_exited(name, sess_gen);
                return;
            }
            if quit {
                drop_stream(io);
                self.writer_exited(name, sess_gen);
                return;
            }
            if !items.is_empty() {
                continue;
            }
            out.wait(Duration::from_millis(250));
        }
    }

    // ---- LIST / EXPLAIN ----

    /// LIST-REPLY 的 JSON（Go listJSON；derive 结构体的**声明序** = Go struct 序——
    /// `serde_json::Map` 默认 BTreeMap 会把键排成字典序，对象字面量手插也不行，
    /// 评审 M3）。
    fn list_json(&self) -> String {
        let st = self.lock_state();
        let mut out: Vec<SessionEntryJson> = Vec::new();
        for name in st.registry.session_names() {
            let Some(reg) = st.registry.session(name) else { continue };
            if reg.done {
                continue;
            }
            let Some(rt) = st.sessions.get(name) else { continue };
            let clients: Vec<ClientEntryJson> = reg
                .legs()
                .into_iter()
                .map(|l: LegView| ClientEntryJson {
                    kind: l.kind.as_str(),
                    cols: l.size.cols(),
                    rows: l.size.rows(),
                    since_ms: l.since_ms as i64,
                    active: reg.active == Some(l.key),
                })
                .collect();
            // legacy-only 会话（无服务端 vt）恒无 cwd——Go cwdLocked 同款（评审 L9）
            let cwd = rt
                .vt
                .is_some()
                .then(|| rt.scan.pwd_path())
                .filter(|c| !c.is_empty());
            out.push(SessionEntryJson {
                name: rt.name.clone(),
                created_ms: rt.created_ms as i64,
                last_active_ms: rt.last_active_ms as i64,
                attached: !clients.is_empty(),
                agent: agent::agent_name(rt.agent),
                state_v2: frames::state_v2::name(rt.state_v2),
                title: rt.scan.title().to_string(),
                cwd,
                cols: reg.size.cols(),
                rows: reg.size.rows(),
                pid: rt.pty.pid(),
                clients,
            });
        }
        #[derive(serde::Serialize)]
        struct Reply<'a> {
            sessions: &'a [SessionEntryJson],
        }
        match serde_json::to_string(&Reply { sessions: &out }) {
            Ok(json) => json,
            Err(_) => "{\"sessions\":[]}".to_string(),
        }
    }

    fn explain_json(&self, name: &str) -> Result<String, TermError> {
        // 两段式：ps（10~50ms）在服务锁**外**取——锁内不做阻塞 I/O（评审 M1；
        // Go 持的是单会话锁，收敛成服务锁后爆炸半径是全服务）
        let pty = {
            let mut st = self.lock_state();
            let Some(l) = &st.manifests else {
                return Err(TermError::new(
                    super::session::TermErrorCode::DetectOff,
                    "本出口的检测被 HOMEWAY_TERM_DETECT=off 关闭",
                ));
            };
            let _ = l;
            let Some(rt) = st.sessions.get_mut(name) else {
                return Err(TermError::new(
                    super::session::TermErrorCode::NoSession,
                    format!("会话 {name} 不存在"),
                ));
            };
            if rt.vt.is_none() {
                return Err(TermError::new(
                    super::session::TermErrorCode::NoVt,
                    format!("会话 {name} 没有服务端 vt（legacy-only），没有屏幕证据可判"),
                ));
            }
            Arc::clone(&rt.pty)
        };
        let procs = agent::read_procs();
        let fg = pty.foreground_pgid();
        let mut guard = self.lock_state();
        let st = &mut *guard;
        let agent_name = {
            let l = st.manifests.as_ref();
            let screen_probe = st.sessions.get(name).map(|rt| rt.vt.is_some());
            if screen_probe.is_none() {
                return Err(TermError::new(
                    super::session::TermErrorCode::NoSession,
                    format!("会话 {name} 不存在"),
                ));
            }
            let Some(l) = l else {
                return Err(TermError::new(
                    super::session::TermErrorCode::DetectOff,
                    "本出口的检测被 HOMEWAY_TERM_DETECT=off 关闭",
                ));
            };
            agent::foreground_agent_name(&procs, fg, |n| l.for_process(n).is_some())
        };
        if agent_name.is_empty() {
            return Err(TermError::new(
                super::session::TermErrorCode::NoAgent,
                format!("会话 {name} 前台不是已知 agent，没有规则可跑"),
            ));
        }
        // 整屏 + 回滚的纯文本（Go PlainText 口径——M2；定检路径仍用视口 ScreenText）
        let screen = st
            .sessions
            .get_mut(name)
            .and_then(|rt| rt.vt.as_mut())
            .expect("上则判过 vt")
            .plain_text();
        let Some(l) = st.manifests.as_ref() else { unreachable!("上则判过") };
        let out = run_explain(l, &agent_name, &screen, name);
        serde_json::to_string(&out).map_err(|e| {
            TermError::new(
                super::session::TermErrorCode::Marshal,
                format!("explain 输出编码失败：{e}"),
            )
        })
    }

    /// 腿的呈现分类（日志用）。
    fn leg_kind_of(&self, name: &str, key: LegKey) -> &'static str {
        let st = self.lock_state();
        st.sessions
            .get(name)
            .and_then(|rt| rt.legs.iter().find(|l| l.key == key))
            .map(|l| l.kind.as_str())
            .unwrap_or("legacy")
    }

    /// 腿是否声明了 surface 能力（上行帧的门面）。
    fn leg_is_surface(&self, name: &str, key: LegKey) -> bool {
        let st = self.lock_state();
        st.sessions
            .get(name)
            .and_then(|rt| rt.legs.iter().find(|l| l.key == key))
            .is_some_and(|l| l.surface.is_some())
    }

    /// HELLO caps → 本腿键编码平台口径（Q-J F1）：`KEY_ALT_*` 两位互斥声明；
    /// 未声明（两位全不置）= 宿主推断（缺省兼容，逐字节同今日）。
    /// **两位同置 = 歧义 ⇒ 按未声明处理（fail-soft）+ 计数 + 一次性告警，绝不拒腿**——
    /// 「裸 ID 尾随块」按 caps 解析出 `caps=0x7F` 恰含 bit2|bit3（`frames::dec_hello_tail`
    /// 既有测试钉死），任何拒绝面都会把今天可服务的腿变成 `bad_capability`。
    fn key_flavor_from_caps(&self, caps: u8, caps_present: bool) -> super::keyenc::KeyFlavor {
        use super::keyenc::KeyFlavor;
        if !caps_present {
            return KeyFlavor::host_default();
        }
        let no_esc = caps & frames::caps::KEY_ALT_NO_ESC_PREFIX != 0;
        let esc = caps & frames::caps::KEY_ALT_ESC_PREFIX != 0;
        match (no_esc, esc) {
            (true, false) => KeyFlavor::AltNoEscPrefix,
            (false, true) => KeyFlavor::AltEscPrefix,
            (false, false) => KeyFlavor::host_default(),
            (true, true) => {
                self.key_flavor_ambiguous.fetch_add(1, Ordering::Relaxed);
                if !self.key_flavor_ambiguous_warned.swap(true, Ordering::Relaxed) {
                    self.logf(
                        "⚠️ term: 客户端 caps 同时声明 KEY_ALT_ESC_PREFIX 与 KEY_ALT_NO_ESC_PREFIX\
（歧义——含「裸 ID 尾随块」既有形态）—— 按未声明处理（宿主推断），不拒腿；本形态计数已开启",
                    );
                }
                KeyFlavor::host_default()
            }
        }
    }

    fn logf(&self, msg: &str) {
        (self.logf)(msg);
    }

    /// 服务锁获取（中毒恢复口径统一：任一持锁线程 panic 不该让整个 term 面殉葬——
    /// **锁安全 ≠ 状态一致**：恢复后可能读到半更新字段，处置保守优于级联崩溃。
    /// 后果口径（Q-D 代码门 H1）：最坏 = 一拍脏数据——采样写入的
    /// `state_v2/last_scan_seq/prev_cpu/prev_quiet` 每拍全量重算，`content_seq` 随
    /// 每批 PTY 输出自增 ⇒ 屏扫短路会随下次输出自愈；`Hygiene` 的 `pending_idle`
    /// 按墙钟过期。真正不可自愈的只有单字段漂移类（例 `LegOut.qbytes`），其处置
    /// 路径保守：该腿持续「超限入队失败 → needSnapshot」）。
    fn lock_state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- 采样：agent / 任务状态（Go sampleLoop/sampleOnce/sample）----

    /// tick 级注入键（测试唯一化；见 `svc_id`）。
    #[cfg(test)]
    fn tick_inject_key(&self) -> String {
        format!("tick:{}", self.svc_id)
    }

    fn sample_loop(&self) {
        loop {
            std::thread::sleep(SAMPLE_PERIOD);
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            if !self.cfg.detect {
                continue;
            }
            // F7（代码门 A2）：一拍**整体**兜底——`read_procs`（/proc 或 ps）与会话名快照
            // 记账 panic 不得让**服务级**检测线程停摆（按会话的 catch 只保「单会话不停摆」，
            // 设计 §2 F7 给 sample 的理由正是「不能全线停摆」——整拍门在此补齐）
            let _ = guard_thread(ThreadRole::Sample, "", &self.logf, || self.sample_tick());
        }
    }

    /// 采样一拍（进程表 + 逐会话 [`TermService::sample_once`]）。
    fn sample_tick(&self) {
        #[cfg(test)]
        maybe_inject_panic(ThreadRole::Sample, &self.tick_inject_key());
        let procs = agent::read_procs();
        // 快照（名字, 代）：同名重建的瞬间不用旧 procs 刷新会话（评审 P8——
        // Go sampleOnce 持的是会话指针快照）
        let named: Vec<(String, u64)> = {
            let st = self.lock_state();
            st.registry
                .session_names()
                .into_iter()
                .filter_map(|n| st.sessions.get(n).map(|rt| (n.to_string(), rt.gen)))
                .collect()
        };
        for (name, gen) in named {
            // F7：采样是服务级线程——按会话 catch，单会话 panic 只跳本拍
            let ctx = format!("会话 {name} ");
            if let Some(action) =
                guard_thread(ThreadRole::Sample, &ctx, &self.logf, || self.sample_once(&name, gen, &procs))
            {
                debug_assert_eq!(action, PanicAction::SkipSession);
            }
        }
    }

    fn sample_once(&self, name: &str, sess_gen: u64, procs: &[ProcInfo]) {
        maybe_inject_panic(ThreadRole::Sample, name);
        let now = now_ms();
        let mut guard = self.lock_state();
        let st = &mut *guard;
        if st.registry.session(name).is_none_or(|s| s.done) {
            return;
        }
        // 字段级拆分借用（rt_of_mut 收整个 &mut State 会锁死 manifests 的并读）
        let Some(rt) = st.sessions.get_mut(name).filter(|rt| rt.gen == sess_gen) else { return };
        if rt.stopped.load(Ordering::Relaxed) {
            return;
        }
        let fg = rt.pty.foreground_pgid();
        let prev_agent = rt.agent;

        // 身份腿先跑：屏幕证据的短路判据要「agent 已知/是否变化」（任务 4.7）。
        // manifests（不可变字段）与 rt（sessions 可变字段）是 State 的拆分借用。
        let ev = screen_evidence_locked(st.manifests.as_ref(), rt, procs, fg);
        let probe = agent::AgentProbe {
            procs,
            fg_pgid: fg,
            prev_cpu: rt.prev_cpu,
            out_bytes: rt.out_bytes(now),
            prev_state: rt.state_v2,
            prev_quiet: rt.prev_quiet,
            shell_pid: rt.pty.pid(),
            screen: ev.as_ref(),
            osc_status: rt.scan.osc_status(),
        };
        let v = agent::classify_agent(&probe);
        if v.cpu >= 0 {
            rt.prev_cpu = Some(v.cpu);
        }
        rt.prev_quiet = v.quiet;
        let mut agent_changed = false;
        if v.agent != prev_agent {
            rt.scan.clear_osc_evidence();
            agent_changed = true;
        }
        if ev.is_some() {
            rt.last_scan_seq = rt.content_seq;
        }
        // 状态机卫生（任务 4.7）：working→普通 idle 确认窗 / skip_state_update 冻结 /
        // blocked 定期重发
        let process_exited = v.agent == frames::agent::UNKNOWN && v.state_v2 == frames::state_v2::IDLE;
        let visible_idle = ev.as_ref().is_some_and(|e| e.visible_idle);
        let visible_blocker = ev.as_ref().is_some_and(|e| e.visible_blocker);
        let freeze = ev.as_ref().is_some_and(|e| e.skip_update);
        if freeze && v.agent == prev_agent {
            return; // 覆盖屏 + 身份未变：整拍不发布（状态冻结，列表停在上一状态）
        }
        let next_state = if freeze { rt.state_v2 } else { v.state_v2 };
        if next_state != rt.state_v2
            && rt.hygiene.should_hold_working_to_idle(
                rt.state_v2,
                next_state,
                visible_idle,
                visible_blocker,
                agent_changed,
                process_exited,
                Instant::now(),
            )
        {
            return; // 本拍按住不发（暂态空屏被确认窗吸收）
        }
        let mut publish = next_state != rt.state_v2 || v.agent != prev_agent;
        if !publish && rt.hygiene.should_republish_blocked(rt.state_v2, Instant::now()) {
            publish = true; // blocked 持续期间定期重发，保持消费方新鲜
        }
        if !publish {
            return;
        }
        rt.agent = v.agent;
        rt.state_v2 = next_state;
        let suffix = if freeze { "（skip_state_update 冻结）" } else { "" };
        self.logf(&format!(
            "term: 会话 {} 状态 {}/{}（fg={} procs={} 依据={}）{}",
            rt.name,
            agent::agent_name(v.agent),
            frames::state_v2::name(next_state),
            fg,
            procs.len(),
            v.evidence,
            suffix
        ));
        push_state_to_legs(rt);
    }
}

// ---------------------------------------------------------------------------
// 锁内助手（自由函数——避开 &self 与 &mut State 的借用交叠）
// ---------------------------------------------------------------------------

/// STATE 帧投递到所有腿（latest-wins；状态单轨化——surface 与 raw 腿同一枚举）。
fn push_state_to_legs(rt: &SessRt) {
    if rt.legs.is_empty() {
        return;
    }
    let payload = frames::enc_state(rt.agent, rt.state_v2, rt.scan.title());
    for l in &rt.legs {
        l.out.enqueue_state(WriteItem::new(Op::STATE, payload.clone()));
    }
}

/// 注册引发的腿断落运行态（ENDED 经各自写者送达；从 rt.legs 摘除 + 判据行日志——
/// Go endLegLocked 对每条断腿都打「腿断开」行，finish/takeover/self_reconnect 同面）。
fn apply_end_to_leg_rt(logf: &Logf, rt: &mut SessRt, e: &LegEnd) {
    if let Some(idx) = rt.legs.iter().position(|l| l.key == e.key) {
        let l = rt.legs.remove(idx);
        match l.surface.as_ref().map(|s| s.stats) {
            Some(st) => logf(&format!(
                "term: 会话 {} 腿断开（kind={} 原因={}）｜快照={} 差分={} 降级={} 背压={} 队列溢出={} 编码失败={} 分片={} 下行={}B FETCH 命中={} 落空={}",
                rt.name, l.kind.as_str(), e.why, st.snapshots, st.diffs, st.degrades, st.backpressure,
                st.queue_overflow, st.encode_failed, st.fragments, st.bytes_out, st.fetch_hits, st.fetch_miss
            )),
            None => logf(&format!(
                "term: 会话 {} 腿断开（kind={} 原因={}）",
                rt.name, l.kind.as_str(), e.why
            )),
        }
        if let Some((code, reason)) = &e.ended {
            l.out.finish_ended(WriteItem::new(Op::ENDED, frames::enc_ended(*code, reason)));
        } else {
            l.out.finish_quit();
        }
    }
}

/// 活动腿的主题/剪贴板回落（Go applyActiveThemeLocked，任务 3.3）：活动切换后
/// 新 active 腿的主题落到会话 vt（OSC 10/11 查询按它应答）、剪贴板读缓存发布。
fn apply_active_theme_locked(rt: &mut SessRt, active: Option<LegKey>) {
    let Some(active) = active else { return };
    let Some(l) = rt.legs.iter().find(|l| l.key == active) else { return };
    let (theme_known, theme, clip) = (l.theme_known, l.theme, l.clip_cache.clone());
    if theme_known {
        if let Some(vt) = rt.vt.as_mut() {
            vt.set_default_colors(theme.0, theme.1);
        }
    }
    rt.clip_cache = clip;
}

/// 尺寸应用（applySizeLocked）：PTY setsize + epoch + vt 重排 + 全 surface 腿标记全量
/// （被动腿会拒收几何不符的差分，任务 3.1 的连带义务）。
///
/// 入参是 [`Size`]——非 0 且在限内由类型保证（入径归一 + [`super::vt::SessionVt`] 硬拒
/// 之外的门移到编译期）；`RESIZE 0×0` 在入径已被拦下（F1b），这里不再需要 0 判断。
fn apply_size_locked(rt: &mut SessRt, size: Size) {
    if rt.stopped.load(Ordering::Relaxed) {
        return;
    }
    if rt.size() == size {
        return;
    }
    rt.pty.resize(size.cols(), size.rows());
    rt.ring.note_size(size);
    if let Some(vt) = rt.vt.as_mut() {
        let _ = vt.resize(size.cols(), size.rows());
    }
    for l in &mut rt.legs {
        if let Some(s) = l.surface.as_mut() {
            s.mark_need_snapshot("resize");
        }
    }
    if rt.any_surface() {
        rt.wake_surface();
    }
}

/// 焦点 nudge 字节的纯判据（F3 三态单测面）：会话已收工 / 程序未开 `?1004` ⇒ None。
fn nudge_bytes_for(stopped: bool, focus_mode: bool, focus_in: bool) -> Option<&'static [u8]> {
    if stopped || !focus_mode {
        return None;
    }
    Some(if focus_in { b"\x1b[I" } else { b"\x1b[O" })
}

/// 快照镜像窗口的实际行数（F1c 调用点口径）：`min(rows × MIRROR_VIEWPORTS, 行数预算)`。
fn mirror_window_rows(cols: u16, rows: u16) -> usize {
    (rows as usize * super::codec::MIRROR_VIEWPORTS).min(mirror_rows_budget(cols as usize))
}

/// 非阻塞投递一条到每会话 PTY 注入队列；队列满 ⇒ 计数 + 节流日志（F6）。
/// 调用方可以在**锁内**调（`try_send` 不阻塞——F3 的顺序保证）。
fn try_send_or_count(
    tx: &SyncSender<PtyInject>,
    item: PtyInject,
    counter: &AtomicU64,
    logf: &Logf,
    name: &str,
    what: &str,
    tail: &str,
) {
    match tx.try_send(item) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => count_drop(counter, logf, name, what, tail),
        // 消费者已退出（resp 线程 spawn 失败/写失败退出/收工退出）：同样是丢弃，
        // 但归因不同（代码门 M3——原实现静默，F6 观测面对这一面是盲区）
        Err(TrySendError::Disconnected(_)) => count_drop(counter, logf, name, what, "（消费者已退出）"),
    }
}

/// 丢弃计数 + 节流日志（首 3 次 + 每 100 次；沿用 Q-C F6 形态——防刷屏又不丢首现）。
fn count_drop(counter: &AtomicU64, logf: &Logf, name: &str, what: &str, tail: &str) {
    let n = counter.fetch_add(1, Ordering::Relaxed) + 1;
    if n <= 3 || n.is_multiple_of(100) {
        logf(&format!("term: 会话 {name} {what}丢弃 {n} 条{tail}"));
    }
}

/// PTY 注入入队（F3：调用方持锁，非阻塞——队列序 = 状态迁移序）。
fn enqueue_pty_inject(rt: &SessRt, bytes: &'static [u8], logf: &Logf) {
    try_send_or_count(
        &rt.resp_tx,
        PtyInject::Nudge(bytes),
        &rt.nudge_dropped,
        logf,
        &rt.name,
        "PTY 注入",
        "（队列满）",
    );
}

/// 屏幕证据（Go screenEvidenceLocked）：身份选表 → 空闲短路 → 求值。
fn screen_evidence_locked(
    manifests: Option<&manifest::Loader>,
    rt: &mut SessRt,
    procs: &[ProcInfo],
    fg_pgid: i32,
) -> Option<agent::ScreenEvidence> {
    let manifests = manifests?;
    let vt = rt.vt.as_mut()?;
    let proc_name =
        agent::foreground_agent_name(procs, fg_pgid, |n| manifests.for_process(n).is_some());
    if proc_name.is_empty() {
        return None;
    }
    // 空闲短路：判据用本拍进程名（agentKnown = 有名字；agentChanged = 与上次扫屏不同）
    let agent_changed = proc_name != rt.last_scan_proc;
    if rt.hygiene.should_skip_screen_scan(
        rt.state_v2,
        true,
        agent_changed,
        false,
        rt.content_seq,
        rt.last_scan_seq,
        Instant::now(),
    ) {
        return None;
    }
    let comp = manifests.for_process(&proc_name)?;
    // 只喂一屏（视口）纯文本：契约是「最近约一屏」，不是整条回滚
    let in_ = super::manifest::region::Input {
        screen: vt.screen_text(),
        osc_title: rt.scan.title_evidence().to_string(),
        osc_progress: super::scan::progress_payload(rt.scan.progress()),
    };
    let res = comp.evaluate(&in_);
    rt.last_scan_proc = proc_name;
    Some(agent::ScreenEvidence {
        state: state_v2_from_manifest(res.state),
        visible_idle: res.visible_idle,
        visible_blocker: res.visible_blocker,
        visible_working: res.visible_working,
        skip_update: res.skip_state_update,
        rule_id: res.matched_rule.as_ref().map(|r| r.id.clone()).unwrap_or_default(),
        version: comp.manifest.version.clone(),
        source: comp.manifest.source.as_str().to_string(),
        fallback: res.fallback_reason.clone(),
    })
}

/// state → stateV2 映射（Go stateV2FromManifest）。
fn state_v2_from_manifest(s: manifest::State) -> u8 {
    match s {
        manifest::State::Working => frames::state_v2::WORKING,
        manifest::State::Blocked => frames::state_v2::BLOCKED,
        manifest::State::Idle => frames::state_v2::IDLE,
        manifest::State::Unknown => frames::state_v2::UNKNOWN,
    }
}

fn dec_name_or(f: &Frame) -> Result<String, Vec<u8>> {
    frames::dec_name(&f.payload).map_err(|e| frames::enc_error("bad_name", &e.to_string()))
}

fn drop_stream(io: FrameIo) {
    // 写半是 dup 出来的 fd——drop 只关一个引用、**不打 FIN**（客户端看不到断）。
    // shutdown(Both) 作用于 socket 本体（两半共享）：裸断腿的客户端立即见 EOF、
    // 留在本线程的读半也随之退出（评审 H3，Go `c.conn.Close()` 同效）。
    let st = io.into_stream();
    let _ = st.shutdown(std::net::Shutdown::Both);
    drop(st);
}

struct Pending {
    key: LegKey,
    op: Op,
    body: Vec<u8>,
    st: SurfState,
}

enum NextChunk {
    Data(Vec<u8>),
    CaughtUp,
    Gone,
}

enum Peek {
    Data(Vec<u8>),
    CaughtUp,
    Gone,
}

enum PeekFinal {
    Data(Vec<u8>),
    Drained,
}

// ---------------------------------------------------------------------------
// LIST JSON（Go listJSON 的字段序/omitempty 同款——声明序即 wire 序）
// ---------------------------------------------------------------------------

mod list_json_shape {
    use serde::Serialize;

    /// 在场腿信息（term-host-cli 任务 2.5，design D8：字段只增不改）。
    #[derive(Serialize)]
    pub struct ClientEntryJson {
        pub kind: &'static str, // app / host / legacy
        pub cols: u16,
        pub rows: u16,
        #[serde(rename = "sinceMs")]
        pub since_ms: i64,
        pub active: bool,
    }

    #[derive(Serialize)]
    pub struct SessionEntryJson {
        pub name: String,
        #[serde(rename = "createdMs")]
        pub created_ms: i64,
        #[serde(rename = "lastActiveMs")]
        pub last_active_ms: i64,
        pub attached: bool,
        pub agent: &'static str,
        /// 状态唯一字段（旧 `state` 键已随状态单轨化退役，term-remote 3.3）。
        #[serde(rename = "stateV2")]
        pub state_v2: &'static str,
        pub title: String,
        /// OSC 7 上报的工作目录（缺省不显示）。
        #[serde(skip_serializing_if = "Option::is_none")]
        pub cwd: Option<String>,
        pub cols: u16,
        pub rows: u16,
        pub pid: i32,
        pub clients: Vec<ClientEntryJson>,
    }
}

use list_json_shape::ClientEntryJson;
use list_json_shape::SessionEntryJson;

// ---------------------------------------------------------------------------
// explain JSON（Go explainOutput 的字段序/omitempty 同款）
// ---------------------------------------------------------------------------

mod explain_json_shape {
    use serde::Serialize;

    #[derive(Serialize)]
    pub struct MatchedRule {
        pub id: String,
        pub priority: i32,
        pub region: String,
        pub state: String,
    }

    #[derive(Serialize)]
    pub struct EvaluatedRule {
        pub id: String,
        pub priority: i32,
        pub region: String,
        pub state: String,
        pub matched: bool,
        #[serde(rename = "regionBytes")]
        pub region_bytes: usize,
    }

    #[derive(Serialize)]
    pub struct ExplainOutput {
        pub agent: String,
        #[serde(skip_serializing_if = "String::is_empty")]
        pub session: String,
        #[serde(skip_serializing_if = "String::is_empty", rename = "manifestSource")]
        pub manifest_source: String,
        #[serde(skip_serializing_if = "String::is_empty", rename = "manifestVersion")]
        pub manifest_version: String,
        pub state: String,
        #[serde(skip_serializing_if = "String::is_empty", rename = "fallbackReason")]
        pub fallback: String,
        #[serde(skip_serializing_if = "Option::is_none", rename = "matchedRule")]
        pub matched: Option<MatchedRule>,
        #[serde(rename = "visibleIdle")]
        pub visible_idle: bool,
        #[serde(rename = "visibleBlocker")]
        pub visible_blocker: bool,
        #[serde(rename = "visibleWorking")]
        pub visible_working: bool,
        #[serde(rename = "skipStateUpdate")]
        pub skip_update: bool,
        pub rules: Vec<EvaluatedRule>,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        pub warnings: Vec<String>,
        #[serde(rename = "screenBytes")]
        pub screen_bytes: usize,
    }
}

/// Go `runExplain` 同义（在线/离线同一套依据链——结论与列表判定一致）。
fn run_explain(
    l: &manifest::Loader,
    agent_name: &str,
    screen: &str,
    session: &str,
) -> explain_json_shape::ExplainOutput {
    let comp = l.for_process(agent_name).or_else(|| l.for_id(agent_name));
    let Some(comp) = comp else {
        return explain_json_shape::ExplainOutput {
            agent: agent_name.to_string(),
            session: session.to_string(),
            manifest_source: String::new(),
            manifest_version: String::new(),
            state: "unknown".to_string(),
            fallback: String::new(),
            matched: None,
            visible_idle: false,
            visible_blocker: false,
            visible_working: false,
            skip_update: false,
            rules: Vec::new(),
            warnings: vec!["没有该 agent 的 manifest".to_string()],
            screen_bytes: screen.len(),
        };
    };
    let in_ = super::manifest::region::Input {
        screen: screen.to_string(),
        osc_title: String::new(),
        osc_progress: String::new(),
    };
    let res = comp.evaluate(&in_);
    explain_json_shape::ExplainOutput {
        agent: agent_name.to_string(),
        session: session.to_string(),
        manifest_source: comp.manifest.source.as_str().to_string(),
        manifest_version: comp.manifest.version.clone(),
        state: res.state.as_str().to_string(),
        fallback: res.fallback_reason.clone(),
        matched: res.matched_rule.as_ref().map(|r| explain_json_shape::MatchedRule {
            id: r.id.clone(),
            priority: r.priority,
            region: r.region.clone(),
            state: r.state.as_str().to_string(),
        }),
        visible_idle: res.visible_idle,
        visible_blocker: res.visible_blocker,
        visible_working: res.visible_working,
        skip_update: res.skip_state_update,
        rules: res
            .rules
            .iter()
            .map(|r| explain_json_shape::EvaluatedRule {
                id: r.id.clone(),
                priority: r.priority,
                region: r.region.clone(),
                state: r.state.as_str().to_string(),
                matched: r.matched,
                region_bytes: r.region_bytes,
            })
            .collect(),
        warnings: l.warnings().to_vec(),
        screen_bytes: screen.len(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::term::wire::ReadFail;

    /// 测试面构造：注入配置与 manifest（env 面在进程级——并行测试会互踩）。
    fn svc_with(cfg: TermConfig, lines: Arc<Mutex<Vec<String>>>) -> Arc<TermService> {
        svc_with_manifests(cfg, lines, false)
    }

    fn svc_with_manifests(cfg: TermConfig, lines: Arc<Mutex<Vec<String>>>, with_manifests: bool) -> Arc<TermService> {
        let logf: Logf = Arc::new(move |m: &str| {
            lines.lock().unwrap().push(m.to_string());
        });
        let _dir = std::env::temp_dir().join(tmp_name("hwterm-state"));
        // svc_with 不落盘（manifests 由调用方给、无覆盖目录）——只需占位不建
        let mut cfg = cfg;
        if cfg.history == DEFAULT_HISTORY {
            cfg.history = 64 << 10; // 测试环小一点
        }
        // 不 from_env：直接装配（manifest 内嵌）
        let s = Arc::new(TermService {
            cfg,
            logf,
            state: Mutex::new(State {
                registry: SessionRegistry::new(16, 8),
                sessions: HashMap::new(),
                manifests: with_manifests.then(|| manifest::Loader::new(None)),
                next_sess_gen: 1,
            }),
            stop: Arc::new(AtomicBool::new(false)),
            key_flavor_ambiguous: AtomicU64::new(0),
            key_flavor_ambiguous_warned: AtomicBool::new(false),
            leg_missing_input_drops: AtomicU64::new(0),
            active_conns: Arc::new(AtomicUsize::new(0)),
            conn_over_cap: AtomicU64::new(0),
            #[cfg(test)]
            svc_id: next_svc_id(),
        });
        let tick = Arc::clone(&s);
        std::thread::Builder::new()
            .name("term-sample-test".into())
            .spawn(move || tick.sample_loop())
            .ok();
        s
    }

    /// 唯一短名（UDS 路径受 SUN_LEN 限制——纳秒时间戳太长）。
    fn tmp_name(prefix: &str) -> String {
        use std::sync::atomic::AtomicU64 as C;
        static N: C = C::new(0);
        let ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        format!("{prefix}{}-{ms}", N.fetch_add(1, Ordering::Relaxed))
    }

    /// 起监听并返回路径（测试自管 accept：直接 serve）。
    fn start_listener() -> (UnixListener, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(tmp_name("hwterm"));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("term.sock");
        let ln = UnixListener::bind(&path).unwrap();
        (ln, path)
    }

    struct Client {
        io: FrameIo,
    }

    impl Client {
        fn connect(path: &std::path::Path) -> Self {
            Self::from_stream(UnixStream::connect(path).unwrap())
        }

        /// 从**任意一条已连上的服务侧连接**起（M3 S2：QUIC 入口 = 出口泵交出的
        /// socketpair 服务端；握手帧断言与 UDS 路径共用同一份）。
        fn from_stream(stream: UnixStream) -> Self {
            let mut io = FrameIo::new(stream);
            let g = io.read_frame_deadline(Duration::from_secs(5)).unwrap();
            assert_eq!(g.op, Op::GREETING);
            let (ver, feats) = frames::dec_greeting(&g.payload).unwrap();
            assert_eq!((ver, feats), (1, frames::features::ALL));
            Client { io }
        }

        fn send(&mut self, op: Op, payload: &[u8]) {
            self.io.write_frame(op, payload, Duration::from_secs(5)).unwrap();
        }

        /// 等到指定 op（其它帧全跳过——回放期 ATTACHED→DATA…→REPLAY-DONE、ENDED 前的
        /// STATE 族都合法交错；ERROR 帧早退带细节）。
        fn expect(&mut self, op: Op, secs: u64) -> Frame {
            let deadline = Instant::now() + Duration::from_secs(secs);
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                let f = self
                    .io
                    .read_frame_deadline(left.max(Duration::from_millis(10)))
                    .unwrap_or_else(|e| panic!("等 op 0x{:02x} 超时：{e}", op.0));
                if f.op == op {
                    return f;
                }
                if f.op == Op::ERROR {
                    let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
                    panic!("期望 op 0x{:02x}，收到 ERROR({code}: {msg})", op.0);
                }
            }
        }

        /// 收集 DATA 帧载荷直到谓词命中或超时。
        fn drain_data_until(&mut self, pred: impl Fn(&[u8]) -> bool, secs: u64) -> Vec<u8> {
            let deadline = Instant::now() + Duration::from_secs(secs);
            let mut all = Vec::new();
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    panic!("data 谓词超时：已收 {:?}", String::from_utf8_lossy(&all));
                }
                let f = self
                    .io
                    .read_frame_deadline(left.max(Duration::from_millis(10)))
                    .expect("读帧");
                match f.op {
                    Op::DATA => {
                        all.extend_from_slice(&f.payload);
                        if pred(&all) {
                            return all;
                        }
                    }
                    Op::STATE | Op::NOTIFY | Op::REPLAY_DONE => continue,
                    other => panic!("意外帧 0x{:02x}", other.0),
                }
            }
        }
    }

    fn hello(name: &str, tail: &[u8]) -> Vec<u8> {
        frames::enc_hello(80, 24, 0, name, tail)
    }

    /// 全流程：CREATE(-d) → LIST → attach（raw）回放/回显 → KILL → ENDED(-2)。
    #[test]
    fn raw_attach_replay_echo_kill_ended() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig {
            shell: Some("printf TERMTEST-READY; cat".into()),
            ..TermConfig::default()
        };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }

        // CREATE 不接入
        let mut c = Client::connect(&path);
        c.send(Op::CREATE, &frames::enc_create(0, "t1"));
        let f = c.expect(Op::OK, 5);
        assert!(f.payload.is_empty());
        drop(c); // CREATE 是一锤子命令（应答即收线）——LIST 换新连接
        // LIST：在场、未接入、80x24
        let mut c = Client::connect(&path);
        c.send(Op::LIST, &[]);
        let f = c.expect(Op::LIST, 5);
        let v: serde_json::Value = serde_json::from_slice(&f.payload).unwrap();
        let sess = &v["sessions"][0];
        assert_eq!(sess["name"], "t1");
        assert_eq!(sess["attached"], false);
        assert_eq!((sess["cols"].as_u64(), sess["rows"].as_u64()), (Some(80), Some(24)));
        assert_eq!(sess["agent"], "unknown");
        assert_eq!(sess["stateV2"], "unknown");
        assert_eq!(sess["clients"].as_array().map(Vec::len), Some(0));
        drop(c);

        // attach（caps rawTerminal）：ATTACHED → 回放含 TERMTEST-READY → REPLAY-DONE
        std::thread::sleep(Duration::from_millis(700)); // 等 shell 吐完 READY 进环
        let mut c2 = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "host-1");
        c2.send(Op::HELLO, &hello("t1", &tail));
        let f = c2.expect(Op::ATTACHED, 5);
        let (_cols, _rows, _modes, agent, state, name) = frames::dec_attached(&f.payload).unwrap();
        assert_eq!((agent, state, name.as_str()), (frames::agent::UNKNOWN, frames::state_v2::UNKNOWN, "t1"));
        let data = c2.drain_data_until(|d| has_bytes(&window_bytes(d), b"TERMTEST-READY"), 8);
        assert!(has_bytes(&window_bytes(&data), b"\x1b[3J\x1b[2J\x1b[H"), "回放前有换屏前序");
        let f = c2.expect(Op::REPLAY_DONE, 5);
        let (replayed, flags) = frames::dec_replay_done(&f.payload).unwrap();
        assert!(replayed > 0, "回放窗口非空");
        assert_eq!(flags & frames::replay_flags::TRUNCATED, 0, "小历史无截断");

        // 输入回显（cat 模式：写啥回啥）
        c2.send(Op::DATA, b"ECHO-OK");
        let got = c2.drain_data_until(|d| has_bytes(&window_bytes(d), b"ECHO-OK"), 8);
        assert!(has_bytes(&window_bytes(&got), b"ECHO-OK"), "cat 会话回显");

        // LIST：接入腿 kind=host、active
        let mut c3 = Client::connect(&path);
        c3.send(Op::LIST, &[]);
        let f = c3.expect(Op::LIST, 5);
        let v: serde_json::Value = serde_json::from_slice(&f.payload).unwrap();
        let cl = &v["sessions"][0]["clients"][0];
        assert_eq!(cl["kind"], "host");
        assert_eq!(cl["active"], true);
        drop(c3);

        // KILL → ENDED(-2)
        c2.send(Op::KILL, &frames::enc_name("t1"));
        let f = c2.expect(Op::OK, 5);
        assert!(f.payload.is_empty());
        let f = c2.expect(Op::ENDED, 8);
        let (code, reason) = frames::dec_ended(&f.payload);
        assert_eq!((code, reason.as_str()), (ended_code_killed(), ""));

        // 收尾后 LIST 空
        std::thread::sleep(Duration::from_millis(400));
        let mut c4 = Client::connect(&path);
        c4.send(Op::LIST, &[]);
        let f = c4.expect(Op::LIST, 5);
        let v: serde_json::Value = serde_json::from_slice(&f.payload).unwrap();
        assert_eq!(v["sessions"].as_array().map(Vec::len), Some(0), "KILL 后会话出表");
        drop(c4);

        // 判据行形态抽查
        let logs = lines.lock().unwrap().join("\n");
        assert!(logs.contains("term: 新建会话 t1（pid="), "判据行（新建）：{logs}");
        assert!(logs.contains("term: 创建会话 t1（不接入，默认尺寸）"), "判据行（创建不接入）：{logs}");
        assert!(logs.contains("term: 会话 t1 腿接入（kind=host 80x24 id=host-1 首腿=true）n=1/8"), "判据行（腿接入）：{logs}");
        assert!(logs.contains("term: 关闭会话 t1（pid="), "判据行（关闭）：{logs}");
        assert!(
            logs.contains("term: 会话 t1 腿断开（kind=host 原因=finish）"),
            "判据行（腿断开，KILL 路径 why=finish——Go endLegLocked 同串面）：{logs}"
        );
        svc.close();
    }

    /// 自然退出：shell 自己退出 → ENDED 带退出码；同实例重连/接管的 ENDED(replaced)。
    #[test]
    fn exit_code_and_takeover_ended() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("sleep 30".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        // 腿 A（id=dev-1）
        let mut a = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "dev-1");
        a.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "tx", &tail));
        a.expect(Op::ATTACHED, 5);
        a.expect(Op::REPLAY_DONE, 5);
        // 腿 B（attach -d 接管）⇒ A 收 ENDED(-1, replaced)
        let mut b = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "dev-2");
        b.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::TAKEOVER, "tx", &tail));
        b.expect(Op::ATTACHED, 5);
        let f = a.expect(Op::ENDED, 5);
        let (code, reason) = frames::dec_ended(&f.payload);
        assert_eq!((code, reason.as_str()), (-1, "replaced"), "接管的 ENDED 归因文案");
        drop(a);
        // 同实例重连（id=dev-2）⇒ 旧 dev-2 腿 ENDED(-1, self_reconnect)
        let mut c = Client::connect(&path);
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "tx", &tail));
        c.expect(Op::ATTACHED, 5);
        let f = b.expect(Op::ENDED, 5);
        let (code, reason) = frames::dec_ended(&f.payload);
        assert_eq!((code, reason.as_str()), (-1, "self_reconnect"), "同实例替换的 ENDED 归因");
        drop(b);
        c.expect(Op::REPLAY_DONE, 5);
        // KILL → 信号死形态：ENDED(-2)（killed 词面——Go 对信号死 -1、killed 恒 -2；
        // D-19 处置后信号死退出码在「非 killed」路径才可见，见下一条测试）
        c.send(Op::KILL, &frames::enc_name("tx"));
        c.expect(Op::OK, 5);
        let f = c.expect(Op::ENDED, 8);
        let (code, _) = frames::dec_ended(&f.payload);
        assert_eq!(code, -2);
        svc.close();
    }

    /// 版本门 + surface 协商（快照族）+ exit 7 自然退出码直传。
    #[test]
    fn exit_code_passthrough_and_surface_leg() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("sleep 30".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut c = Client::connect(&path);
        // 版本门：caps 带 protoVer 位 + 版本 9 → ERROR(term_version)
        let bad: Vec<u8> = vec![1u8, frames::caps::PROTO_VER | frames::caps::SURFACE, 9];
        c.send(Op::HELLO, &frames::enc_hello(80, 24, 0, "tv", &bad));
        let f = c.expect(Op::ERROR, 5);
        let (code, msg) = frames::dec_error(&f.payload).unwrap();
        assert_eq!(code, "term_version");
        assert!(msg.contains("升到同一版本"), "{msg}");
        drop(c);
        // surface 腿：ATTACHED → SNAPSHOT 分片 + SNAPSHOT-DONE
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::SURFACE, true, "app-1");
        c.send(Op::HELLO, &frames::enc_hello(100, 30, hello_flags::CREATE, "ts", &tail));
        let f = c.expect(Op::ATTACHED, 5);
        let (cols, rows, _m, _a, _s, name) = frames::dec_attached(&f.payload).unwrap();
        assert_eq!((cols, rows, name.as_str()), (100, 30, "ts"));
        // 收 SNAPSHOT 分片 → SNAPSHOT-DONE
        let mut gz = Vec::new();
        loop {
            let f = c.io.read_frame_deadline(Duration::from_secs(8)).unwrap_or_else(|e| {
                panic!("surface 帧: {e}；服务日志：{}", lines.lock().unwrap().join("\n"))
            });
            match f.op {
                Op::SNAPSHOT => {
                    let (flags, chunk) = super::super::codec::dec_fragment(&f.payload).unwrap();
                    gz.extend_from_slice(chunk);
                    if flags & super::super::codec::FRAG_MORE_BIT == 0 {
                        break;
                    }
                }
                Op::STATE | Op::NOTIFY => continue,
                other => panic!("surface 期待 SNAPSHOT 族，收到 0x{:02x}", other.0),
            }
        }
        c.expect(Op::SNAPSHOT_DONE, 5);
        let body = super::super::codec::gunzip_bytes(&gz).unwrap();
        let snap = super::super::codec::dec_snapshot_body(&body).unwrap();
        assert_eq!((snap.cols, snap.rows), (100, 30));
        assert_eq!(snap.revision, 1, "首拍全量 revision=1");
        // KILL 收尾（-2 面）；exit 码直传面由 exit_code_and_takeover/pty 单测覆盖
        c.send(Op::KILL, &frames::enc_name("ts"));
        c.expect(Op::OK, 5);
        let f = c.expect(Op::ENDED, 8);
        let (code, _) = frames::dec_ended(&f.payload);
        assert_eq!(code, -2);
        svc.close();
    }

    /// D-19 处置判据：信号死（非 killed 路径）退出码 = -1（Go ProcessState 语义）。
    #[test]
    fn signal_death_exit_code_is_minus_one() {
        // cat 挂着；服务侧 KILL 走 killed 词面（-2）——本测试用「会话内 shell 被外力杀」
        // 不可注入 ⇒ 直接单测 PtySession::wait 的映射（portable-pty kill → SIGKILL 形态）。
        let mut ps = super::super::pty::spawn("d19", 80, 24, Some("sleep 60")).unwrap();
        std::thread::sleep(Duration::from_millis(300));
        ps.kill_force();
        let code = ps.wait();
        assert_eq!(code, -1, "信号死 ⇒ -1（Go ProcessState.ExitCode 语义；D-19 处置）");
        // 正常退出码不受影响
        let mut ps2 = super::super::pty::spawn("d19b", 80, 24, Some("exit 3")).unwrap();
        assert_eq!(ps2.wait(), 3);
    }

    /// RESIZE 帧路径 + EXPLAIN 的错误面（无检测/无 agent）。
    #[test]
    fn resize_and_explain_errors() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "h");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "tr", &tail));
        c.expect(Op::ATTACHED, 5);
        c.expect(Op::REPLAY_DONE, 5);
        // RESIZE
        c.send(Op::RESIZE, &frames::enc_resize(120, 40));
        // 轮询等 LIST 反映新尺寸（固定 sleep 在慢机上有竞态——门二 r2 §4）
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut saw = false;
        while Instant::now() < deadline {
            let mut c2 = Client::connect(&path); // LIST 一锤子——每轮换新连接
            c2.send(Op::LIST, &[]);
            let f = c2.expect(Op::LIST, 5);
            let v: serde_json::Value = serde_json::from_slice(&f.payload).unwrap();
            if v["sessions"][0]["cols"].as_u64() == Some(120) {
                saw = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        assert!(saw, "RESIZE 应在窗内落进 LIST（120x40 经活动选举）");
        // EXPLAIN：坏 name 载荷 ⇒ bad_name（本测试服务未装 manifest，detect 面另测）
        let mut c5 = Client::connect(&path);
        c5.send(Op::EXPLAIN, &[]);
        let f = c5.expect(Op::ERROR, 5);
        let (code, _) = frames::dec_error(&f.payload).unwrap();
        assert_eq!(code, "bad_name");
        drop(c5);
        // 坏首帧 op
        let mut c3 = Client::connect(&path);
        c3.send(Op::DATA, b"x");
        let f = c3.expect(Op::ERROR, 5);
        let (code, msg) = frames::dec_error(&f.payload).unwrap();
        assert_eq!(code, "bad_op");
        assert!(msg.contains("首帧必须是"), "{msg}");
        drop(c3);
        // 未知会话 KILL ⇒ no_session
        let mut c4 = Client::connect(&path);
        c4.send(Op::KILL, &frames::enc_name("nope"));
        let f = c4.expect(Op::ERROR, 5);
        let (code, msg) = frames::dec_error(&f.payload).unwrap();
        assert_eq!(code, "no_session");
        assert!(msg.contains("会话 nope 不存在"), "{msg}");
        svc.close();
    }

    /// H2 回归：createOnly 80x24 → 异尺寸 attach（100x30）⇒ PTY/vt/注册表三方几何
    /// 一致（修前 registry 100x30 而 PTY/vt 停在 80x24——真实 shell 的 stty 实证）。
    ///
    /// R8-8c（F3 处置）：整测**硬期限 60s**——本测跑真登录 shell（环境偶发挂死
    /// n=1，本机 4 轮未复现）；各内层预算（expect 5s / drain 10s）覆盖不了
    /// 「connect/close/serve 线程」层面的卡死，硬期限超时**判失败**而不是挂死
    /// 整个测试进程（评审建议的加固形态）。
    #[test]
    fn attach_size_applies_to_pty() {
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let body = std::thread::spawn(move || {
            let lines = Arc::new(Mutex::new(Vec::new()));
            let cfg = TermConfig { shell: None, ..TermConfig::default() }; // 真登录 shell
            let svc = svc_with(cfg, Arc::clone(&lines));
            let (ln, path) = start_listener();
            {
                let svc = Arc::clone(&svc);
                std::thread::spawn(move || svc.serve(ln));
            }
            let mut c0 = Client::connect(&path);
            c0.send(Op::CREATE, &frames::enc_create(0, "t-h2"));
            c0.expect(Op::OK, 5);
            drop(c0);
            std::thread::sleep(Duration::from_millis(800)); // 等 shell 就绪
            let mut c = Client::connect(&path);
            let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "h2");
            c.send(Op::HELLO, &frames::enc_hello(100, 30, 0, "t-h2", &tail));
            c.expect(Op::ATTACHED, 5);
            c.expect(Op::REPLAY_DONE, 5);
            // shell 里跑 stty size：PTY 几何真变 100x30 ⇒ "30 100"
            c.send(Op::DATA, b"stty size\r");
            let got = c.drain_data_until(|d| has_bytes(d, b"30 100"), 10);
            assert!(has_bytes(&got, b"30 100"), "PTY 尺寸未随 attach 跟进：{:?}", String::from_utf8_lossy(&got));
            svc.close();
            let _ = done_tx.send(());
        });
        match done_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(()) => { let _ = body.join(); }
            Err(_) => panic!("attach_size_applies_to_pty 硬期限 60s 超时（环境性挂死复现——按 F3 加固判失败）"),
        }
    }

    /// H3 回归：上限腾位（裸断无 ENDED）的旧腿必须**立即见 EOF**——修前 dup fd 的
    /// drop 不打 FIN，客户端双挂、输入还能继续注进 PTY。
    #[test]
    fn evicted_leg_gets_eof() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut first: Option<Client> = None;
        for i in 0..8 {
            let mut c = Client::connect(&path);
            let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, &format!("ev-{i}"));
            c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "t-h3", &tail));
            c.expect(Op::ATTACHED, 5);
            c.expect(Op::REPLAY_DONE, 5);
            if i == 0 {
                first = Some(c);
            } else {
                std::mem::forget(c); // 保持连接在场（腿满 8 条）
            }
        }
        let mut first = first.expect("首腿在");
        // 第 9 条 ⇒ 腾位（最久空闲 = 首腿，裸断无 ENDED）
        let mut c9 = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "ev-9");
        c9.send(Op::HELLO, &frames::enc_hello(80, 24, 0, "t-h3", &tail));
        c9.expect(Op::ATTACHED, 5);
        // 被腾位的首腿必须见 **FIN**（Err::Hard(UnexpectedEof)）——读超时不算数
        //（修前形态恰是 5s 超时冒充 EOF，评审 P1：断言要判别修复本体）
        let t0 = Instant::now();
        match first.io.read_frame_deadline(Duration::from_secs(5)) {
            Err(ReadFail::Hard(_)) => {} // EOF（UnexpectedEof）= shutdown(Both) 的 FIN
            Err(ReadFail::Timeout) => {
                panic!("被腾位腿应见 FIN（shutdown(Both)），5s 读超时 = 修复未生效")
            }
            Err(other) => panic!("被腾位腿应见 EOF，却收到 {other}"),
            Ok(f) => panic!("被腾位的腿应见 EOF，却收到 op 0x{:02x}", f.op.0),
        }
        assert!(t0.elapsed() < Duration::from_secs(1), "FIN 应即时到达（实测 ≤100ms 量级）");
        svc.close();
    }

    /// 自然退出端到端（评审 D7 补钉）：shell 自己退出 → pump EOF → ENDED 带退出码。
    #[test]
    fn natural_exit_ended_end_to_end() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("printf BYE; exit 7".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "nx");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "t-nx", &tail));
        c.expect(Op::ATTACHED, 5);
        let got = c.drain_data_until(|d| has_bytes(d, b"BYE"), 8);
        assert!(has_bytes(&got, b"BYE"), "回放含 BYE");
        let f = c.expect(Op::ENDED, 10);
        let (code, reason) = frames::dec_ended(&f.payload);
        assert_eq!((code, reason.as_str()), (7, ""), "自然退出码 7 直传、reason 空");
        svc.close();
    }

    /// M3 回归：LIST JSON 的 wire 键序 = Go struct 序（serde 声明序）。
    #[test]
    fn list_json_key_order_matches_go_struct() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("sleep 30".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, lines);
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "ord");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "t-ord", &tail));
        c.expect(Op::ATTACHED, 5);
        drop(c);
        let mut c2 = Client::connect(&path);
        c2.send(Op::LIST, &[]);
        let f = c2.expect(Op::LIST, 5);
        let json = String::from_utf8_lossy(&f.payload).into_owned();
        let want_prefix = "{\"sessions\":[{\"name\":\"t-ord\",\"createdMs\":";
        assert!(json.starts_with(want_prefix), "LIST JSON 首键序（name→createdMs→…）：{json}");
        let idx = |k: &str| json.find(&format!("\"{k}\":")).expect(k);
        let i_name = idx("name");
        let i_created = idx("createdMs");
        let i_active = idx("lastActiveMs");
        let i_attached = idx("attached");
        let i_agent = idx("agent");
        let i_state = idx("stateV2");
        let i_title = idx("title");
        let i_cols = idx("cols");
        let i_pid = idx("pid");
        let i_clients = idx("clients");
        assert!(i_name < i_created && i_created < i_active && i_active < i_attached);
        assert!(i_attached < i_agent && i_agent < i_state && i_state < i_title);
        assert!(i_title < i_cols && i_cols < i_pid && i_pid < i_clients, "Go struct 序：{json}");
        svc.close();
    }

    /// P6 补钉：服务层 EXPLAIN **成功路径**（exec -a codex 钉前台进程名——真实
    /// agent 进程不可注入，argv0 伪造是 6g 实测用过的同款手法）。
    #[test]
    fn explain_success_path_service_level() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig {
            shell: Some("printf EXPL-READY; exec -a codex /bin/sleep 30".into()),
            ..TermConfig::default()
        };
        let svc = svc_with_manifests(cfg, Arc::clone(&lines), true);
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "ex");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "t-ex", &tail));
        c.expect(Op::ATTACHED, 5);
        c.drain_data_until(|d| has_bytes(d, b"EXPL-READY"), 8);
        std::thread::sleep(Duration::from_millis(1500)); // 等采样识别 codex
        let mut c2 = Client::connect(&path);
        c2.send(Op::EXPLAIN, &frames::enc_name("t-ex"));
        let f = c2.expect(Op::EXPLAIN, 5);
        let v: serde_json::Value = serde_json::from_slice(&f.payload).expect("explain JSON 可解析");
        assert_eq!(v["agent"], "codex", "服务层识别到前台 agent");
        assert!(v["screenBytes"].as_u64().unwrap_or(0) > 0);
        drop(c2);
        c.send(Op::KILL, &frames::enc_name("t-ex"));
        c.expect(Op::OK, 5);
        c.expect(Op::ENDED, 8);
        svc.close();
    }

    /// **M3 S2 判据（服务入口承载面）**：经 **QUIC 源（socketpair）** 的客户端与经
    /// **UDS 源** 的客户端拿到**逐字节相同**的握手帧与 LIST 应答载荷（「应用层零改动」的
    /// 构造性钉法——HSP 帧层一行未改）；且两源在同一入口上**并发可用**（QUIC 不断开流时
    /// UDS 照常受理）。
    #[test]
    fn intake_two_sources_serve_term_protocol_byte_exact() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let svc = svc_with(TermConfig::default(), Arc::clone(&lines));
        let (ln, path) = start_listener();
        let capacity = homeway_quic::tuning::service_defaults::intake_capacity(16);
        assert_eq!(capacity, 20, "term 入口容量 = 会话上限 16 + 4（§8.2-16）");
        let (intake, tx) = homeway_quic::ServiceIntake::with_quic(ln, capacity).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        {
            let svc2 = Arc::clone(&svc);
            let st2 = Arc::clone(&stop);
            std::thread::spawn(move || svc2.serve_stoppable(intake, st2));
        }
        // 源①：UDS
        let mut c1 = Client::connect(&path);
        c1.send(Op::LIST, &[]);
        let l1 = c1.expect(Op::LIST, 5);
        // 源②：QUIC（socketpair 服务端）
        let (svc_end, cli_end) = UnixStream::pair().unwrap();
        tx.try_enqueue(svc_end).expect("入队");
        let mut c2 = Client::from_stream(cli_end);
        c2.send(Op::LIST, &[]);
        let l2 = c2.expect(Op::LIST, 5);
        assert_eq!(l2.op, l1.op, "LIST 帧类型");
        assert_eq!(l2.payload, l1.payload, "LIST 应答载荷逐字节相同（两源同协议面）");
        assert!(
            String::from_utf8_lossy(&l2.payload).contains("\"sessions\""),
            "内容面（防两源都回了空壳）：{}",
            String::from_utf8_lossy(&l2.payload)
        );
        // 两源并发：QUIC 腿保持连接（c2 不收线）时 UDS 仍能受理
        let mut c3 = Client::connect(&path);
        c3.send(Op::LIST, &[]);
        let _ = c3.expect(Op::LIST, 5);
        drop((c1, c2, c3));
        svc.close();
        stop.store(true, Ordering::Relaxed);
    }

    /// **M3 代码门 r18（C2-1）判据：term 的连接级在册闸**（§1.7 设计门 2-4）。
    ///
    /// 在册满（= 会话上限 + `INTAKE_K`，此处 2+4=6）⇒ 第 7 条连接**被收线**（对端见 EOF，
    /// 无 GREETING）+ 计数 + 一行；全部释放后名额归还（新连接照常受理）。
    /// 旧实装（accept 处从不计数）下这一支不存在：注册里「20 = 新引入的连接级上限」是空头承诺。
    #[test]
    fn conn_level_cap_closes_over_capacity_and_releases_slots() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let svc = svc_with(
            TermConfig { max_sessions: 2, ..TermConfig::default() },
            Arc::clone(&lines),
        );
        assert_eq!(svc.conn_capacity(), 6, "2 + K(4)（与出口 intake 名额同源常量）");
        let (ln, path) = start_listener();
        {
            let svc2 = Arc::clone(&svc);
            std::thread::spawn(move || svc2.serve(ln));
        }
        // 占满：只连不发（不发 HELLO ⇒ 服务线程停在读帧上，名额被占）
        let held: Vec<UnixStream> = (0..6).map(|_| UnixStream::connect(&path).unwrap()).collect();
        let t0 = Instant::now();
        while svc.active_conns() < 6 && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(svc.active_conns(), 6, "6 条全部在册（cap=6）");
        // 第 7 条 ⇒ 收线（EOF；不写 GREETING）
        let mut over = UnixStream::connect(&path).unwrap();
        over.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut b = [0u8; 1];
        assert_eq!(
            over.read(&mut b).unwrap_or(0),
            0,
            "超限连接被收线（对端见 EOF，不是 GREETING）"
        );
        assert_eq!(svc.conn_over_cap(), 1, "超限计数");
        assert!(
            lines
                .lock()
                .unwrap()
                .iter()
                .any(|l| l.contains("连接超限（在册 6/6）")),
            "须有一行归因：{:?}",
            lines.lock().unwrap()
        );
        // 全部释放 ⇒ 名额归还 0（对端 close ⇒ 服务线程即刻退出，不占 HELLO 预算 15s）
        drop(held);
        let t0 = Instant::now();
        while svc.active_conns() > 0 && t0.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(svc.active_conns(), 0, "释放后名额归还");
        // 归还后照常受理（拿到 GREETING + LIST 应答）
        let mut c = Client::connect(&path);
        c.send(Op::LIST, &[]);
        let _ = c.expect(Op::LIST, 5);
        drop(c);
        svc.close();
    }

    /// P7：serve_stoppable 置停止位后监听线程在窗内退出（engine 实际用的路径）。
    #[test]
    fn serve_stoppable_exits_on_stop() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let svc = svc_with(TermConfig::default(), lines);
        let (ln, path) = start_listener();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = Arc::clone(&svc);
        let st2 = Arc::clone(&stop);
        // M3 S2：形参换成两源入口；`from_listener` = 改前的单 UDS 形态（语义零改）
        let intake = ServiceIntake::from_listener(ln).expect("入口");
        let h = std::thread::spawn(move || s2.serve_stoppable(intake, st2));
        // 连接可用（poll 到达即 accept——无 200ms 空闲延迟）
        let mut c = Client::connect(&path);
        c.send(Op::LIST, &[]);
        c.expect(Op::LIST, 2);
        drop(c);
        stop.store(true, Ordering::Relaxed);
        let t0 = Instant::now();
        h.join().expect("监听线程干净退出");
        assert!(t0.elapsed() < Duration::from_secs(1), "停止位后 ≤1s 退出");
    }

    /// 一次 attach 恰一次尺寸哨兵（低 7 判据计数器——P10：计数器有了断言；
    /// 同尺寸活动不注哨兵、异尺寸 RESIZE 真变再 +1）。
    #[test]
    fn sentinel_once_per_attach() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, lines);
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "sc");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "t-sc", &tail));
        c.expect(Op::ATTACHED, 5);
        c.send(Op::DATA, b"x"); // 同尺寸腿活动——不注入哨兵
        std::thread::sleep(Duration::from_millis(400));
        {
            let st = svc.lock_state();
            let rt = st.sessions.get("t-sc").expect("会话在");
            assert_eq!(rt.sentinel_count, 1, "一次 attach 恰 +1");
        }
        c.send(Op::RESIZE, &frames::enc_resize(120, 40));
        std::thread::sleep(Duration::from_millis(400));
        {
            let st = svc.lock_state();
            let rt = st.sessions.get("t-sc").expect("会话在");
            assert_eq!(rt.sentinel_count, 2, "RESIZE 真变再 +1");
        }
        svc.close();
    }

    /// run_explain 的 wire 字段序（explainOutput 声明序 = Go struct 序）。
    #[test]
    fn explain_json_wire_order() {
        let l = manifest::Loader::new(None);
        let out = run_explain(&l, "codex", "some screen", "s1");
        let json = serde_json::to_string(&out).unwrap();
        assert!(
            json.starts_with("{\"agent\":\"codex\",\"session\":\"s1\",\"manifestSource\":"),
            "explain 首键序（agent→session→manifestSource→…）：{json}"
        );
        assert!(json.contains("\"screenBytes\":11"), "screenBytes= 屏幕字节数");
    }

    fn ended_code_killed() -> i32 {
        frames::ended_code::KILLED
    }

    /// note_scrollbar 平移/非平移分流（基线 8167cb7 重锚对齐；Go
    /// TestSurfaceLegNoteScrollbarTrimRule 的平移/非平移两路 + 首帧/增长边界）。
    #[test]
    fn surface_leg_note_scrollbar_translation_split() {
        let sb = |total: u64, offset: u64, len: u16| ScrollbarWire { total, offset, len };
        let mut leg = SurfaceLeg::new();
        // 首帧（从未告知过客户端）：恒 false——首次 attach 本来就走全量
        assert!(!leg.note_scrollbar(sb(100, 90, 10)));
        // 消费掉 new() 置位的首帧全量标志，后续断言才只受 note_scrollbar 影响
        assert!(leg.take_snapshot_flag());
        // 建基线：len=10、距底 = 100-90 = 10
        leg.last_sent = sb(100, 90, 10);
        leg.has_last_sent = true;
        // 增长/持平：不触发
        assert!(!leg.note_scrollbar(sb(110, 100, 10)));
        assert!(!leg.note_scrollbar(sb(100, 90, 10)));
        assert_eq!(leg.stats.trims, 0);
        // 平移型回落（len 不变 + 距底不变：erase 前缀/真裁剪）：差分继续、计 shifts
        assert!(!leg.note_scrollbar(sb(90, 80, 10)));
        assert_eq!(leg.stats.shifts, 1);
        assert!(!leg.need_snapshot);
        assert_eq!(leg.stats.trims, 0);
        // 非平移回落（len 变化）：强制全量
        assert!(leg.note_scrollbar(sb(80, 70, 9)));
        assert_eq!(leg.stats.trims, 1);
        assert!(leg.need_snapshot);
        leg.need_snapshot = false;
        // 非平移回落（距底变化）：强制全量
        assert!(leg.note_scrollbar(sb(70, 55, 10))); // 距底 15 ≠ 10
        assert_eq!(leg.stats.trims, 2);
        assert!(leg.need_snapshot);
    }

    /// F1c：镜像窗口预算（宽屏行数 ≤ 预算；窄屏与既有 golden 尺寸不变）。
    #[test]
    fn mirror_window_rows_budget() {
        // 预算 = 32MiB / (cols × 48B/格)
        assert_eq!(mirror_rows_budget(1000), 699, "1000 列 ⇒ 699 行（32MiB/(1000×48)）");
        assert_eq!(mirror_rows_budget(80), 8738, "80 列 ⇒ 预算行数远大于 rows×10");
        assert_eq!(mirror_rows_budget(u16::MAX as usize), 64, "下限 64 行（极窄/极大列都不退化）");
        // 调用点口径 = min(rows × MIRROR_VIEWPORTS, 预算)
        assert_eq!(mirror_window_rows(80, 24), 240, "窄屏不变（既有行为）");
        assert_eq!(mirror_window_rows(100, 32), 320, "golden 尺寸不变");
        assert_eq!(mirror_window_rows(1000, 500), 699, "宽屏上限处被预算夹住");
        assert_eq!(mirror_window_rows(1000, 50), 500, "宽屏但行数少 ⇒ 不夹（500 < 699）");
        assert_eq!(mirror_window_rows(1, 500), 5000, "极窄屏不受预算影响");
    }

    /// F3：nudge 判据三态（stopped / 未开 ?1004 / 正常）。
    #[test]
    fn nudge_bytes_for_three_states() {
        assert_eq!(nudge_bytes_for(true, true, true), None, "会话收工 ⇒ 不注入");
        assert_eq!(nudge_bytes_for(false, false, false), None, "未开 ?1004 ⇒ 不注入");
        assert_eq!(nudge_bytes_for(false, true, true), Some(&b"\x1b[I"[..]), "focus-in");
        assert_eq!(nudge_bytes_for(false, true, false), Some(&b"\x1b[O"[..]), "focus-out");
    }

    /// F6：丢弃计数 + 节流日志（首 3 次 + 每 100 次）。
    #[test]
    fn drop_counters_and_throttled_log() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let logf: Logf = {
            let lines = Arc::clone(&lines);
            Arc::new(move |m: &str| lines.lock().unwrap().push(m.to_string()))
        };
        let (tx, _rx) = std::sync::mpsc::sync_channel::<PtyInject>(1);
        let dropped = AtomicU64::new(0);
        try_send_or_count(&tx, PtyInject::Resp(vec![1]), &dropped, &logf, "t1", "查询应答", "（队列满）");
        assert_eq!(dropped.load(Ordering::Relaxed), 0, "首条入队成功");
        for _ in 0..4 {
            try_send_or_count(&tx, PtyInject::Resp(vec![2]), &dropped, &logf, "t1", "查询应答", "（队列满）");
        }
        assert_eq!(dropped.load(Ordering::Relaxed), 4, "队列满 ⇒ 计数增长");
        {
            let logs = lines.lock().unwrap();
            let n = logs.iter().filter(|l| l.contains("查询应答丢弃")).count();
            assert_eq!(n, 3, "首 3 次各一条（第 4 次被节流）：{logs:?}");
            assert!(logs.iter().any(|l| l.contains("term: 会话 t1 查询应答丢弃 1 条（队列满）")));
        }
        // M3（代码门）：消费者退出 ⇒ Disconnected 分支独立归因（原实现静默）
        let (tx2, rx2) = std::sync::mpsc::sync_channel::<PtyInject>(1);
        drop(rx2);
        let d2 = AtomicU64::new(0);
        try_send_or_count(&tx2, PtyInject::Resp(vec![1]), &d2, &logf, "t1", "查询应答", "（队列满）");
        assert_eq!(d2.load(Ordering::Relaxed), 1, "消费者退出也计入丢弃");
        assert!(
            lines.lock().unwrap().iter().any(|l| l.contains("查询应答丢弃 1 条（消费者已退出）")),
            "Disconnected 归因文案：{:?}",
            lines.lock().unwrap()
        );
        // 剪贴板写 / PTY 注入：独立计数，同一节流形态
        let cd = AtomicU64::new(0);
        count_drop(&cd, &logf, "t1", "剪贴板写", "");
        let nd = AtomicU64::new(0);
        count_drop(&nd, &logf, "t1", "PTY 注入", "（队列满）");
        let logs = lines.lock().unwrap();
        assert!(logs.iter().any(|l| l.contains("term: 会话 t1 剪贴板写丢弃 1 条")));
        assert!(logs.iter().any(|l| l.contains("term: 会话 t1 PTY 注入丢弃 1 条（队列满）")));
    }

    /// F7：处置表（panic → 动作）纯函数。
    #[test]
    fn panic_action_table() {
        assert_eq!(panic_action(ThreadRole::Pump), PanicAction::FinalizePump);
        assert_eq!(panic_action(ThreadRole::LegWriter), PanicAction::BreakLeg);
        assert_eq!(panic_action(ThreadRole::Surface), PanicAction::DropSurface);
        assert_eq!(panic_action(ThreadRole::Resp), PanicAction::DropResp);
        assert_eq!(panic_action(ThreadRole::Conn), PanicAction::DropConn);
        assert_eq!(panic_action(ThreadRole::Sample), PanicAction::SkipSession);
    }

    /// F7③：服务锁中毒恢复（持锁线程 panic 后其余调用不殉葬）。
    #[test]
    fn poisoned_service_lock_recovers() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let svc = svc_with(TermConfig::default(), lines);
        let svc2 = Arc::clone(&svc);
        let h = std::thread::spawn(move || {
            let _g = svc2.state.lock().unwrap();
            panic!("poison");
        });
        assert!(h.join().is_err(), "持锁线程 panic");
        let json = svc.list_json();
        assert!(json.contains("sessions"), "毒锁恢复后 list_json 可用：{json}");
        svc.close();
    }

    /// F7：guard_thread 捕获 panic 并落日志（载荷取 &str）。
    #[test]
    fn guard_thread_logs_and_reports_action() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let logf: Logf = {
            let lines = Arc::clone(&lines);
            Arc::new(move |m: &str| lines.lock().unwrap().push(m.to_string()))
        };
        assert_eq!(guard_thread(ThreadRole::Resp, "", &logf, || {}), None, "正常退出 ⇒ None");
        let a = guard_thread(ThreadRole::Pump, "会话 t9 ", &logf, || panic!("boom"));
        assert_eq!(a, Some(PanicAction::FinalizePump));
        let logs = lines.lock().unwrap();
        assert!(
            logs.iter().any(|l| l == "term: 会话 t9 term-pump 线程 panic（已兜住）：boom"),
            "兜住日志行文：{logs:?}"
        );
    }

    /// F1a/F1b 集成：RESIZE 0×0 忽略（几何保持）、极端尺寸夹取（LIST/ATTACHED/vt/环四方
    /// 一致，进程存活可继续输入输出）。**绝不让 96 GiB 分配发生**——夹取在入径完成。
    #[test]
    fn resize_clamp_and_zero_ignore() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let name = tmp_name("t-clamp");
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "cl");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, &name, &tail));
        c.expect(Op::ATTACHED, 5);
        c.expect(Op::REPLAY_DONE, 5);
        // 合法 RESIZE 生效（基线）
        c.send(Op::RESIZE, &frames::enc_resize(120, 40));
        let v = poll_list_until(&path, |v| v["sessions"][0]["cols"].as_u64() == Some(120), 5);
        assert_eq!(
            (v["sessions"][0]["cols"].as_u64(), v["sessions"][0]["rows"].as_u64()),
            (Some(120), Some(40))
        );
        // F1b：RESIZE 0×0 ⇒ 忽略本次上报（等忽略计数落地后断言几何保持；修复前 = 0×0）
        c.send(Op::RESIZE, &frames::enc_resize(0, 0));
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let n = svc.lock_state().sessions.get(&name).map(|rt| rt.zero_resize_ignored).unwrap_or(0);
            if n > 0 {
                break;
            }
            assert!(Instant::now() < deadline, "0×0 上报未被处理（忽略计数未增）");
            std::thread::sleep(Duration::from_millis(20));
        }
        let v = poll_list_until(&path, |_| true, 5);
        assert_eq!(
            (v["sessions"][0]["cols"].as_u64(), v["sessions"][0]["rows"].as_u64()),
            (Some(120), Some(40)),
            "0×0 不得改会话几何（F1b）"
        );
        // F1a：极端 RESIZE ⇒ 夹取 1000×500（修复前会原样进 alacritty ⇒ 96 GiB 起分配）
        c.send(Op::RESIZE, &frames::enc_resize(u16::MAX, u16::MAX));
        let v = poll_list_until(&path, |v| v["sessions"][0]["cols"].as_u64() == Some(1000), 5);
        assert_eq!(
            (v["sessions"][0]["cols"].as_u64(), v["sessions"][0]["rows"].as_u64()),
            (Some(1000), Some(500))
        );
        {
            let st = svc.lock_state();
            let rt = st.sessions.get(&name).expect("会话在");
            assert_eq!(rt.vt.as_ref().unwrap().size(), (1000, 500), "vt 几何跟随夹取");
            assert_eq!((rt.size().cols(), rt.size().rows()), (1000, 500), "环 epoch 跟随");
        }
        c.send(Op::DATA, b"CLAMP-OK");
        let got = c.drain_data_until(|d| has_bytes(&window_bytes(d), b"CLAMP-OK"), 8);
        assert!(has_bytes(&window_bytes(&got), b"CLAMP-OK"), "夹取后会话仍可用");
        // 夹取日志（只在真夹取时打——重复上报不刷屏）
        let logs = lines.lock().unwrap().join("\n");
        assert!(logs.contains("尺寸夹取 65535x65535 → 1000x500"), "夹取日志：{logs}");
        // F1a：HELLO 极端尺寸同样夹取（ATTACHED 即报夹取后的几何）
        let name2 = tmp_name("t-clamp2");
        let mut c2 = Client::connect(&path);
        let tail2 = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "cl2");
        c2.send(Op::HELLO, &frames::enc_hello(u16::MAX, u16::MAX, hello_flags::CREATE, &name2, &tail2));
        let f = c2.expect(Op::ATTACHED, 5);
        let (cols, rows, _m, _a, _s, _n) = frames::dec_attached(&f.payload).unwrap();
        assert_eq!((cols, rows), (1000, 500), "HELLO 极端尺寸夹取（F1a）");
        drop(c2);
        svc.close();
    }

    /// F3 集成：焦点 nudge 走每会话 PTY 注入队列——首腿 focus-in 先于末腿 focus-out
    /// （顺序 = 状态迁移序），锁内不写 PTY（写由 response_writer_loop 在锁外做）。
    #[test]
    fn focus_nudge_queue_order() {
        let marker = std::env::temp_dir().join(tmp_name("hwterm-nudge"));
        let _ = std::fs::remove_file(&marker);
        // 子进程：先开 ?1004（扫描器从输出解析 FOCUS 位），再关 canonical/echo 把收到的
        // PTY 注入原样落盘（\x1b[I / \x1b[O 都无换行，canonical 下不会落盘）
        let cmd = format!("printf '\\033[?1004h'; stty -icanon -echo; cat > {}", marker.display());
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some(cmd), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let name = tmp_name("t-nudge");
        let read_marker = |p: &std::path::Path| std::fs::read(p).unwrap_or_default();
        // 先建会话（不接入）：等子进程吐出 ?1004 被扫描器吃进 modes——首腿 attach 的
        // focus-in 判据必须在写者到达 nudge 点**之前**就位（否则竞态漏注入）
        let mut c0 = Client::connect(&path);
        c0.send(Op::CREATE, &frames::enc_create(0, &name));
        c0.expect(Op::OK, 5);
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let focus_on = {
                let st = svc.lock_state();
                st.sessions
                    .get(&name)
                    .is_some_and(|rt| rt.scan.modes() & super::super::codec::mode_bits::FOCUS != 0)
            };
            if focus_on {
                break;
            }
            assert!(Instant::now() < deadline, "?1004 未被扫描器识别");
            std::thread::sleep(Duration::from_millis(20));
        }
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "n1");
        let mut a = Client::connect(&path);
        a.send(Op::HELLO, &frames::enc_hello(80, 24, 0, &name, &tail));
        a.expect(Op::ATTACHED, 5);
        a.expect(Op::REPLAY_DONE, 5);
        // 首腿 focus-in（回放后注入）→ 落盘
        let deadline = Instant::now() + Duration::from_secs(8);
        while !read_marker(&marker).windows(3).any(|w| w == b"\x1b[I") {
            assert!(
                Instant::now() < deadline,
                "focus-in 未到 PTY（落盘：{:?}）",
                String::from_utf8_lossy(&read_marker(&marker))
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        // 第二条腿（非首腿 ⇒ 无 focus-in）→ 摘 A（非末腿 ⇒ 无 focus-out）→ 摘 B（末腿 ⇒ focus-out）
        let mut b = Client::connect(&path);
        let tail_b = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "n2");
        b.send(Op::HELLO, &frames::enc_hello(80, 24, 0, &name, &tail_b));
        b.expect(Op::ATTACHED, 5);
        b.expect(Op::REPLAY_DONE, 5);
        drop(a);
        std::thread::sleep(Duration::from_millis(200));
        drop(b);
        let deadline = Instant::now() + Duration::from_secs(8);
        while !read_marker(&marker).windows(3).any(|w| w == b"\x1b[O") {
            assert!(
                Instant::now() < deadline,
                "focus-out 未到 PTY（落盘：{:?}）",
                String::from_utf8_lossy(&read_marker(&marker))
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let data = read_marker(&marker);
        let i = data.windows(3).position(|w| w == b"\x1b[I").expect("I 在");
        let o = data.windows(3).position(|w| w == b"\x1b[O").expect("O 在");
        assert!(i < o, "注入顺序 = 状态迁移序（focus-in 先于 focus-out）");
        assert_eq!(data.windows(3).filter(|w| *w == b"\x1b[I").count(), 1, "首腿恰一次 focus-in");
        svc.close();
        let _ = std::fs::remove_file(&marker);
    }

    /// F7 注入：pump panic ⇒ 进程存活 + 会话收尾（ENDED 达）+ 日志含「线程 panic（已兜住）」。
    #[test]
    fn pump_panic_finalizes_session() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let name = tmp_name("t-ppanic");
        panic_inject::arm(ThreadRole::Pump, &name);
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "pp");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, &name, &tail));
        c.expect(Op::ATTACHED, 5);
        c.expect(Op::REPLAY_DONE, 5);
        c.send(Op::DATA, b"x"); // 触发 pump 首次读 ⇒ 注入 panic
        let f = c.expect(Op::ENDED, 10);
        let (code, _) = frames::dec_ended(&f.payload);
        assert_eq!(code, -1, "pump 主动收尾：wait_bounded 超时 SIGKILL ⇒ 信号死 -1");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !lines.lock().unwrap().iter().any(|l| l.contains("term-pump 线程 panic（已兜住）")) {
            assert!(Instant::now() < deadline, "兜住日志缺失：{:?}", lines.lock().unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
        // 会话出表（不留僵尸）；服务仍可用
        poll_list_until(&path, |v| v["sessions"].as_array().is_some_and(|a| a.is_empty()), 8);
        svc.close();
    }

    /// F7 注入：leg writer panic ⇒ 只断该腿（裸断），会话存活可再接入。
    #[test]
    fn leg_writer_panic_breaks_leg_only() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let name = tmp_name("t-lw");
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "lw1");
        let mut a = Client::connect(&path);
        a.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, &name, &tail));
        a.expect(Op::ATTACHED, 5);
        a.expect(Op::REPLAY_DONE, 5);
        // 给 B 的写者线程入口埋雷（A 的写者已过入口）
        panic_inject::arm(ThreadRole::LegWriter, &name);
        let mut b = Client::connect(&path);
        let tail_b = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "lw2");
        b.send(Op::HELLO, &frames::enc_hello(80, 24, 0, &name, &tail_b));
        // B 不会收到 ATTACHED（写者入口即 panic ⇒ 裸断）；等兜住日志 + 腿数回落
        let deadline = Instant::now() + Duration::from_secs(5);
        while !lines.lock().unwrap().iter().any(|l| l.contains("term-leg-writer 线程 panic（已兜住）")) {
            assert!(Instant::now() < deadline, "兜住日志缺失：{:?}", lines.lock().unwrap());
            std::thread::sleep(Duration::from_millis(20));
        }
        let v = poll_list_until(
            &path,
            |v| v["sessions"][0]["clients"].as_array().is_some_and(|a| a.len() == 1),
            5,
        );
        assert_eq!(v["sessions"].as_array().map(Vec::len), Some(1), "会话存活");
        assert!(
            lines.lock().unwrap().iter().any(|l| l.contains("原因=panicked")),
            "腿断开归因 panicked：{:?}",
            lines.lock().unwrap()
        );
        // 会话仍可接入新腿（写者计数回收正确——否则收尾时运行态永驻）
        let mut c = Client::connect(&path);
        let tail_c = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "lw3");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, 0, &name, &tail_c));
        c.expect(Op::ATTACHED, 5);
        c.expect(Op::REPLAY_DONE, 5);
        drop(c);
        drop(b);
        drop(a);
        svc.close();
    }

    /// F7（代码门 A2）：采样**整拍**（锁外 `read_procs`/记账）panic 也不得停摆服务级线程
    /// ——tick 级注入（键 = 空会话名）命中两次即证明循环存活。
    #[test]
    fn sample_tick_panic_keeps_loop_alive() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let svc = svc_with(TermConfig::default(), Arc::clone(&lines));
        let count_line = |lines: &Arc<Mutex<Vec<String>>>| -> usize {
            lines.lock().unwrap().iter().filter(|l| l.contains("term-sample 线程 panic（已兜住）")).count()
        };
        let key = svc.tick_inject_key();
        panic_inject::arm(ThreadRole::Sample, &key);
        let deadline = Instant::now() + Duration::from_secs(10);
        while count_line(&lines) < 1 {
            assert!(Instant::now() < deadline, "tick 级兜住日志缺失：{:?}", lines.lock().unwrap());
            std::thread::sleep(Duration::from_millis(50));
        }
        panic_inject::arm(ThreadRole::Sample, &key);
        let deadline = Instant::now() + Duration::from_secs(10);
        while count_line(&lines) < 2 {
            assert!(Instant::now() < deadline, "tick 级 panic 后循环停摆（第二轮未命中）");
            std::thread::sleep(Duration::from_millis(50));
        }
        svc.close();
    }

    /// F7 注入：sample panic ⇒ 只跳该会话本拍（采样线程继续，其余会话不受影响）。
    #[test]
    fn sample_panic_skips_only_that_session() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let svc = svc_with(TermConfig::default(), Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let n1 = tmp_name("t-sp1");
        let n2 = tmp_name("t-sp2");
        for n in [&n1, &n2] {
            let mut c = Client::connect(&path);
            c.send(Op::CREATE, &frames::enc_create(0, n));
            c.expect(Op::OK, 5);
        }
        panic_inject::arm(ThreadRole::Sample, &n1);
        let count_line = |lines: &Arc<Mutex<Vec<String>>>| -> usize {
            lines.lock().unwrap().iter().filter(|l| l.contains("term-sample 线程 panic（已兜住）")).count()
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while count_line(&lines) < 1 {
            assert!(Instant::now() < deadline, "采样兜住日志缺失：{:?}", lines.lock().unwrap());
            std::thread::sleep(Duration::from_millis(50));
        }
        // 再埋一次 ⇒ 第二轮仍被命中（采样循环没有停摆）
        panic_inject::arm(ThreadRole::Sample, &n1);
        let deadline = Instant::now() + Duration::from_secs(10);
        while count_line(&lines) < 2 {
            assert!(Instant::now() < deadline, "第二轮采样未继续（循环停摆）");
            std::thread::sleep(Duration::from_millis(50));
        }
        // 两个会话都还在
        poll_list_until(&path, |v| v["sessions"].as_array().is_some_and(|a| a.len() == 2), 5);
        svc.close();
    }

    /// 轮询 LIST 直到谓词命中（LIST 一锤子——每轮新连接；超时 panic 带现场）。
    fn poll_list_until(path: &std::path::Path, pred: impl Fn(&serde_json::Value) -> bool, secs: u64) -> serde_json::Value {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let mut c = Client::connect(path);
            c.send(Op::LIST, &[]);
            let f = c.expect(Op::LIST, 5);
            let v: serde_json::Value = serde_json::from_slice(&f.payload).unwrap();
            if pred(&v) {
                return v;
            }
            assert!(Instant::now() < deadline, "LIST 谓词超时：{v}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    // ---------- Q-J F1：HELLO caps 键编码平台口径 ----------

    /// F1 集成：两条腿（两种声明）各发 `alt+Z` ⇒ 各按本腿口径编码写 PTY。
    /// 观测面 = 同会话的 **raw 腿**（PTY 输出走 DATA 帧；surface 腿收的是快照族）。
    /// `stty raw -echo; printf READY; cat` 让 PTY 回显**精确字节**（否则 `\x1b` 被
    /// ECHOCTL 渲染成 `^[`）；READY 标记保证 raw 已生效再发输入。
    #[test]
    fn alt_flavor_declared_per_leg() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("stty raw -echo; printf READY; cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let alt_z = frames::enc_input(&frames::InputEvent::Key {
            key: 45, // key_z
            mods: 4, // alt
            action: 1,
            text: "Z".into(),
        });
        // 单会话两轮：raw 腿（观察面）+ surface 腿（输入面，带待测声明）
        let run = |name: &str, surf_caps: u8| -> Vec<u8> {
            let mut observer = Client::connect(&path);
            let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "obs");
            observer.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, name, &tail));
            observer.expect(Op::ATTACHED, 5);
            observer.expect(Op::REPLAY_DONE, 5);
            observer.drain_data_until(|d| has_bytes(d, b"READY"), 8); // raw 已生效
            let mut inputter = Client::connect(&path);
            let tail = frames::enc_hello_tail(surf_caps, true, "app");
            inputter.send(Op::HELLO, &frames::enc_hello(80, 24, 0, name, &tail));
            inputter.expect(Op::ATTACHED, 5);
            inputter.send(Op::INPUT, &alt_z);
            observer.drain_data_until(|d| has_bytes(d, b"Z"), 8)
        };
        // ① 声明 KEY_ALT_ESC_PREFIX（= 非 darwin 口径）：alt 产 ESC 前缀
        let got1 = run("f1-esc", frames::caps::SURFACE | frames::caps::KEY_ALT_ESC_PREFIX);
        assert!(
            has_bytes(&got1, b"\x1bZ"),
            "声明 KEY_ALT_ESC_PREFIX ⇒ PTY 应收到 ESC+文本：{:?}",
            String::from_utf8_lossy(&got1)
        );
        // ② 声明 KEY_ALT_NO_ESC_PREFIX（= darwin 口径）：alt 不产前缀
        let got2 = run("f1-noesc", frames::caps::SURFACE | frames::caps::KEY_ALT_NO_ESC_PREFIX);
        assert!(
            !has_bytes(&got2, b"\x1bZ"),
            "声明 KEY_ALT_NO_ESC_PREFIX ⇒ PTY 不得出现 ESC 前缀：{:?}",
            String::from_utf8_lossy(&got2)
        );
        assert!(has_bytes(&got2, b"Z"), "文本本体仍应到达：{:?}", String::from_utf8_lossy(&got2));
        svc.close();
    }

    /// F1：HELLO 声明五态（未声明 / 两种单声明 / 两位同置 / 裸 ID 形态）——**都不拒腿**；
    /// 歧义（含裸 ID 形态的 `caps=0x7F`）按未声明处理 + 计数 + 一次性告警（B1 回归钉）。
    #[test]
    fn hello_caps_flavor_ambiguity_is_fail_soft() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let attach = |path: &std::path::Path, name: &str, tail: &[u8]| {
            let mut c = Client::connect(path);
            c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, name, tail));
            c.expect(Op::ATTACHED, 5); // ATTACHED = 腿已入表的证明（不拒腿）
            c // 保持连接（腿在位）
        };
        // 未声明（caps 块无新位）：合法
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL, true, "a");
        let c1 = attach(&path, "f1-none", &tail);
        assert_eq!(svc.key_flavor_ambiguous.load(Ordering::Relaxed), 0, "未声明不算歧义");
        // 单声明两位：合法、不计数
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL | frames::caps::KEY_ALT_NO_ESC_PREFIX, true, "b");
        let c2 = attach(&path, "f1-noesc", &tail);
        let tail = frames::enc_hello_tail(frames::caps::RAW_TERMINAL | frames::caps::KEY_ALT_ESC_PREFIX, true, "c");
        let c3 = attach(&path, "f1-esc", &tail);
        assert_eq!(svc.key_flavor_ambiguous.load(Ordering::Relaxed), 0, "单声明不算歧义");
        // 两位同置：不拒腿（能拿到 ATTACHED）+ 计数
        let tail = frames::enc_hello_tail(
            frames::caps::RAW_TERMINAL | frames::caps::KEY_ALT_ESC_PREFIX | frames::caps::KEY_ALT_NO_ESC_PREFIX,
            true,
            "d",
        );
        let c4 = attach(&path, "f1-amb", &tail);
        assert_eq!(svc.key_flavor_ambiguous.load(Ordering::Relaxed), 1, "同置计一次");
        // 裸 ID 尾随块形态 `[4,'h','o','s','t']` ⇒ caps=0x7F（恰含 bit2|bit3）——既有
        // 容忍形态，**绝不拒腿**（修前若按「同置 ⇒ 拒」实现，此腿会变 bad_capability）
        let c5 = attach(&path, "f1-bare", &[4, b'h', b'o', b's', b't']);
        assert_eq!(svc.key_flavor_ambiguous.load(Ordering::Relaxed), 2, "裸 ID 形态同样按歧义计数");
        // 一次性告警：恰一行（第二次歧义不重复）
        let logs = lines.lock().unwrap().join("\n");
        assert_eq!(
            logs.matches("KEY_ALT_ESC_PREFIX 与 KEY_ALT_NO_ESC_PREFIX").count(),
            1,
            "歧义告警应恰一次：{logs}"
        );
        drop((c1, c2, c3, c4, c5));
        svc.close();
    }

    /// F1：`handle_input` 查不到腿（`end_leg` 竞态）⇒ 丢弃 + 计数，**不回落宿主口径**
    /// （注入缝：先让腿断开，再用旧 LegKey 直调 handle_input）。
    #[test]
    fn input_for_missing_leg_dropped_with_count() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let cfg = TermConfig { shell: Some("cat".into()), ..TermConfig::default() };
        let svc = svc_with(cfg, Arc::clone(&lines));
        let (ln, path) = start_listener();
        {
            let svc = Arc::clone(&svc);
            std::thread::spawn(move || svc.serve(ln));
        }
        let mut c = Client::connect(&path);
        let tail = frames::enc_hello_tail(frames::caps::SURFACE, true, "x");
        c.send(Op::HELLO, &frames::enc_hello(80, 24, hello_flags::CREATE, "f1-drop", &tail));
        c.expect(Op::ATTACHED, 5); // surface 腿无 REPLAY_DONE（快照族另路）
        let (name, gen, key) = {
            let st = svc.lock_state();
            let rt = st.sessions.get("f1-drop").expect("会话在位");
            (rt.name.clone(), rt.gen, rt.legs.first().expect("腿在位").key)
        };
        drop(c); // 断开 ⇒ 写者线程 end_leg
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if svc.lock_state().sessions.get("f1-drop").is_some_and(|rt| rt.legs.is_empty()) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            svc.lock_state().sessions.get("f1-drop").is_some_and(|rt| rt.legs.is_empty()),
            "腿应在断开后出表"
        );
        let payload = frames::enc_input(&frames::InputEvent::Key {
            key: 20,
            mods: 4,
            action: 1,
            text: "a".into(),
        });
        let out = Arc::new(LegOut::new());
        svc.handle_input(&name, gen, key, &payload, &out);
        assert_eq!(svc.leg_missing_input_drops.load(Ordering::Relaxed), 1, "查不到腿应计一次");
        let logs = lines.lock().unwrap().join("\n");
        assert_eq!(logs.matches("收到无主腿输入").count(), 1, "首次丢弃应记一行（L5）：{logs}");
        // 第二、三次丢弃不刷屏（首 1 次 + 每 100 次）
        svc.handle_input(&name, gen, key, &payload, &out);
        svc.handle_input(&name, gen, key, &payload, &out);
        assert_eq!(svc.leg_missing_input_drops.load(Ordering::Relaxed), 3);
        let logs = lines.lock().unwrap().join("\n");
        assert_eq!(logs.matches("收到无主腿输入").count(), 1, "节流：前三跳只首跳出声");
        svc.close();
    }

    /// 找子串的窗口（回放流里 PTY 字节可能跨 DATA 帧分片——拼接后再找）。
    fn window_bytes(d: &[u8]) -> Vec<u8> {
        d.to_vec()
    }

    fn has_bytes(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len().max(1)).any(|w| w == needle)
    }
}
