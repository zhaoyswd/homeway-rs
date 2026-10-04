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
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicU64;
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
/// SNAPSHOT 附带的回滚镜像窗口（视口数的倍数）。
const MIRROR_VIEWPORTS: usize = 10;
/// 单次 FETCH-ROWS 行数上限。
const FETCH_ROWS_MAX: u16 = 512;
/// 剪贴板读缓存上限（与 Go 的 clipMaxBytes 同值：载荷 u16 长度域的精确上限）。
const CLIP_MAX_BYTES: usize = super::frames::MAX_PAYLOAD - 3;

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
// PTY 共享面（reader 归 pump 线程独占；master/writer/child 在独立小锁内）
// ---------------------------------------------------------------------------

struct PtyShared {
    inner: Mutex<PtySession>,
}

impl PtyShared {
    fn resize(&self, cols: u16, rows: u16) {
        let _ = self.inner.lock().expect("pty").resize(cols, rows);
    }

    fn write_input(&self, p: &[u8]) -> bool {
        self.inner.lock().expect("pty").write_input(p).is_ok()
    }

    fn foreground_pgid(&self) -> i32 {
        self.inner.lock().expect("pty").foreground_pgid()
    }

    fn pid(&self) -> i32 {
        self.inner.lock().expect("pty").pid
    }

    fn kill_start(&self) {
        self.inner.lock().expect("pty").kill_start();
    }

    fn kill_force(&self) {
        self.inner.lock().expect("pty").kill_force();
    }

    fn wait_bounded(&self, grace: Duration) -> i32 {
        self.inner.lock().expect("pty").wait_bounded(grace)
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
    resp_tx: SyncSender<Vec<u8>>,
    resp_dropped: Arc<AtomicU64>,
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

    /// 会话当前尺寸（epoch 表尾——note_size 恒在尺寸变化时记录）。
    fn size(&self) -> (u16, u16) {
        self.ring
            .epochs
            .last()
            .map(|e| (e.cols, e.rows))
            .unwrap_or((80, 24))
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
        });
        // sample 线程（服务级 1s 一拍；Go sampleLoop）
        let tick_svc = Arc::clone(&svc);
        std::thread::Builder::new()
            .name("term-sample".into())
            .spawn(move || tick_svc.sample_loop())
            .ok();
        svc
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
                Ok(stream) => {
                    let svc = Arc::clone(self);
                    std::thread::Builder::new()
                        .name("term-conn".into())
                        .spawn(move || svc.serve_conn(stream))
                        .ok();
                }
                Err(_) => return, // listener 已关
            }
        }
    }

    /// 同 [`Self::serve`]，但 accept 走非阻塞 + 停止位轮询——服务收工时监听线程
    /// 可退出（不陪跑到进程结束；Go Shutdown 里的 `s.termLn.Close()` 同效）。
    pub fn serve_stoppable(self: &Arc<Self>, ln: UnixListener, stop: Arc<AtomicBool>) {
        use std::os::unix::io::AsRawFd as _;
        let _ = ln.set_nonblocking(true);
        let fd = ln.as_raw_fd();
        loop {
            if stop.load(Ordering::Relaxed) || self.stop.load(Ordering::Relaxed) {
                return;
            }
            // poll 等 POLLIN（连接**到达即返回**——纯 sleep 轮询会给空闲期连接加
            // 平均 100ms 延迟；200ms 只是空闲醒来看停止位的节拍，评审 P7）
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            let r = unsafe { libc::poll(&mut pfd, 1, 200) };
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
            match ln.accept() {
                Ok((stream, _)) => {
                    let _ = stream.set_nonblocking(false);
                    let svc = Arc::clone(self);
                    std::thread::Builder::new()
                        .name("term-conn".into())
                        .spawn(move || svc.serve_conn(stream))
                        .ok();
                }
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
        // 尺寸归一（Go spawnLocked 的 80/24 缺省；HELLO 0x0 不产生退化几何——评审 L8）
        let (cols, rows) = (cols_or_default(cols), rows_or_default(rows));
        // 注册腿（全序：同实例替换 → 接管 → 腾位 → 入表即活动）
        let desc = LegDescriptor {
            client_id: ht.client_id.clone(),
            surface,
            raw_capable,
            cols,
            rows,
        };
        let (out, hs, reg, sess_gen) = {
            let mut guard = self.lock_state();
            let st = &mut *guard;
            // ATTACHED/回放计划用**选举前**的会话尺寸（Go registerLegLocked 同序）
            let prev_size = st.registry.session(&name).map(|s| (s.cols, s.rows)).unwrap_or((cols, rows));
            let Some(sess_gen) = st.sessions.get(&name).map(|rt| rt.gen) else {
                return;
            };
            let regres = st.registry.register_leg(&name, desc, takeover, now_ms());
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
                out: Arc::clone(&out),
                surface: surface.then(SurfaceLeg::new),
                theme_known: false,
                theme: ([0, 0, 0], [0, 0, 0]),
                clip_cache: None,
            });
            self.logf(&reg.log);
            // 接入即活动的尺寸应用（H2：注册表只改记账，PTY/vt/环 epoch 在这里跟上；
            // 哨兵不在注册路径注入——stream 统一一次）
            if let Some((c, r)) = reg.size_applied {
                apply_size_locked(rt, c, r);
            }
            // 活动切换的主题/剪贴板回落（评审 P3：注册即活动——Go noteActivityLocked
            // 内建；新腿无上报时把会话缓存清成它的（空）值）
            apply_active_theme_locked(rt, st.registry.session(&name).and_then(|sm| sm.active));
            // ATTACHED/握手计划（锁内构建；ATTACHED 先入队——此刻腿已在表内且锁在手）
            let attached =
                frames::enc_attached(prev_size.0, prev_size.1, rt.scan.modes(), rt.agent, rt.state_v2, &rt.name);
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
                    if let Some(hs) = hs {
                        svc.run_raw_writer(&tname, sess_gen, tkey, wio, tout, hs);
                    } else {
                        svc.run_surface_writer(&tname, sess_gen, tkey, wio, tout);
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
                            apply_size_locked(rt, sz.0, sz.1);
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
                    let mut st = self.lock_state();
                    let _ = st.registry.leg_resize(name, key, cols, rows);
                    let sz = st.registry.note_activity(name, key, now_ms());
                    let active = st.registry.session(name).and_then(|s| s.active);
                    if let Some(rt) = rt_of_mut(&mut st, name, sess_gen) {
                        apply_active_theme_locked(rt, active);
                        if let Some(sz) = sz {
                            apply_size_locked(rt, sz.0, sz.1);
                            self.sentinel_repaint_locked(rt);
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
                apply_size_locked(rt, sz.0, sz.1);
            }
            if rt.stopped.load(Ordering::Relaxed) {
                return;
            }
            let Some(vt) = rt.vt.as_ref() else { return };
            let enc = match &ev {
                frames::InputEvent::Key { key, mods, action, text } => {
                    let Some(action) = super::keyenc::KeyAction::from_wire(*action) else {
                        return;
                    };
                    vt.encode_key(&super::keyenc::KeyEvent {
                        key: super::keyenc::Key(*key),
                        action,
                        mods: super::keyenc::Mods(*mods),
                        text,
                        composing: false,
                    })
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
                let (cols, rows) = st.registry.session(name).map(|s| (s.cols, s.rows)).unwrap_or((80, 24));
                let rev = rt
                    .legs
                    .iter()
                    .find(|l| l.key == key)
                    .and_then(|l| l.surface.as_ref())
                    .map(|s| s.revision)
                    .unwrap_or(0);
                let rows_enc = super::codec::encode_rows(&vt.rows_at(req.from, req.count as usize));
                Some((cols, rows, rev, rows_enc))
            } else {
                None
            }
        };
        let Some((cols, rows, rev, rows_enc)) = built else {
            out.enqueue(
                WriteItem::new(Op::ERROR, frames::enc_error("surface_unavailable", "本会话没有服务端 vt")),
                0,
            );
            return;
        };
        let reply = super::codec::FetchRowsReply {
            revision: rev,
            cols,
            rows,
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
    /// spawn 失败不留半截状态）。
    fn attach_or_create(
        self: &Arc<Self>,
        name: &str,
        cols: u16,
        rows: u16,
        create: bool,
        only_if_absent: bool,
    ) -> Result<(), TermError> {
        let (cols, rows) = (cols_or_default(cols), rows_or_default(rows));
        let mut st = self.lock_state();
        if st.registry.session(name).is_none() && create {
            if st.registry.session_names().len() >= self.cfg.max_sessions {
                return Err(TermError::new(
                    super::session::TermErrorCode::TooMany,
                    format!("会话数已达上限 {}，请先关闭一些会话", self.cfg.max_sessions),
                ));
            }
            self.spawn_session_locked(&mut st, name, cols, rows)?;
            // 注册表建立（80x24 归一——Go spawnLocked 同款；此后腿接入尺寸归选举）
            return st.registry.attach_or_create(name, cols, rows, true, false).map(|_| ());
        }
        st.registry.attach_or_create(name, cols, rows, create, only_if_absent).map(|_| ())
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
        self.spawn_session_locked(&mut st, name, 0, 0)?;
        let r = st.registry.attach_or_create(name, 80, 24, true, false).map(|_| ());
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
        cols: u16,
        rows: u16,
    ) -> Result<(), TermError> {
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

        let (resp_tx, resp_rx) = std::sync::mpsc::sync_channel(RESP_QUEUE_LEN);
        let (clip_tx, clip_rx) = std::sync::mpsc::sync_channel::<String>(8);
        let vt = if vt_disabled_by_env() {
            self.logf(&format!("term: 会话 {name} 无服务端 vt（HOMEWAY_TERM_VT=off）→ 该会话仅 legacy 原始字节模式"));
            None
        } else {
            match SessionVt::new(cols, rows, self.cfg.scrollback_lines) {
                Ok(v) => Some(v),
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
            surf_wake: Arc::new((Mutex::new(0), Condvar::new())),
            clip_tx,
            stopped: Arc::new(AtomicBool::new(false)),
        };
        rt.ring.note_size(cols, rows);
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
            .spawn(move || svc.pump_loop(&tname, sess_gen, reader));
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
        if std::thread::Builder::new()
            .name("term-resp".into())
            .spawn(move || Self::response_writer_loop(resp_rx, resp_pty, resp_stopped))
            .is_err()
        {
            self.logf(&format!("term: 会话 {name} 应答写者线程起不来（查询应答将不代答）"));
        }
        let svc = Arc::clone(self);
        let tname = name.to_string();
        if std::thread::Builder::new()
            .name("term-surface".into())
            .spawn(move || svc.surface_loop(&tname, sess_gen, clip_rx, surf_wake, stopped))
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
        let (cols, rows) = rt.size();
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
            self.focus_nudge_locked(rt, false); // 末腿离开 → focus-out（TUI 停动画）
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
            apply_size_locked(rt, sz.0, sz.1);
            self.sentinel_repaint_locked(rt);
        }
    }

    /// 焦点事件注入（仅当 TUI 开了 ?1004——不开的程序读到这些字节只会当普通输入）。
    fn focus_nudge_locked(&self, rt: &SessRt, focus_in: bool) {
        if rt.stopped.load(Ordering::Relaxed) || rt.scan.modes() & super::codec::mode_bits::FOCUS == 0 {
            return;
        }
        let seq: &[u8] = if focus_in { b"\x1b[I" } else { b"\x1b[O" };
        if !rt.pty.write_input(seq) {
            self.logf(&format!("term: 会话 {} focus nudge 写入失败", rt.name));
        }
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
            let mut responses: Vec<Vec<u8>> = Vec::new();
            let mut clip_store: Option<String> = None;
            type ClipLoad = Arc<dyn Fn(&str) -> String + Send + Sync>;
            let mut clip_loads: Vec<ClipLoad> = Vec::new();
            #[allow(unused_assignments)]
            let mut clip_cache: Option<String> = None;
            let (clip_tx, resp_tx, resp_dropped): (SyncSender<String>, SyncSender<Vec<u8>>, Arc<AtomicU64>) = {
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
                let txs = (rt.clip_tx.clone(), rt.resp_tx.clone(), Arc::clone(&rt.resp_dropped));
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
            // 锁外投递：查询应答（FIX-25）+ 剪贴板读应答 + 剪贴板写转发
            let send = |bytes: Vec<u8>| match resp_tx.try_send(bytes) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    resp_dropped.fetch_add(1, Ordering::Relaxed);
                }
                Err(TrySendError::Disconnected(_)) => {}
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
                let _ = clip_tx.try_send(text); // 满/无消费者：宁可拒绝，也不阻塞 VT 流
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

    /// 应答写者（FIX-25）：独占消费应答队列写 PTY。退出 = 会话收工或 PTY 写失败。
    fn response_writer_loop(rx: Receiver<Vec<u8>>, pty: Arc<PtyShared>, stopped: Arc<AtomicBool>) {
        loop {
            match rx.recv_timeout(SAMPLE_PERIOD) {
                Ok(bytes) => {
                    if !pty.write_input(&bytes) {
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
            let geom = st.registry.session(name).map(|s| (s.cols, s.rows)).unwrap_or((80, 24));
            let Some(rt) = rt_of_mut(st, name, sess_gen) else { return };
            if !rt.any_surface() {
                return;
            }
            let Some(vt) = rt.vt.as_mut() else { return };
            let (cols, rows) = geom;
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
                        let grid = super::codec::encode_grid(cols, rows, &vt.rows());
                        let mirror_rows = vt.mirror_rows(rows as usize * MIRROR_VIEWPORTS);
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
            // 首腿：回放完成后注入 focus-in，逼 TUI 立即全屏重绘
            let mut st = self.lock_state();
            if let Some(rt) = rt_of_mut(&mut st, name, sess_gen) {
                self.focus_nudge_locked(rt, true);
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
                    cols: l.cols,
                    rows: l.rows,
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
                cols: reg.cols,
                rows: reg.rows,
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

    fn logf(&self, msg: &str) {
        (self.logf)(msg);
    }

    /// 服务锁获取（中毒恢复口径统一：任一持锁线程 panic 不该让整个 term 面殉葬——
    /// 数据可能不一致，但 term 是可选服务，锁内无跨调用不变量，恢复优于级联崩溃）。
    fn lock_state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ---- 采样：agent / 任务状态（Go sampleLoop/sampleOnce/sample）----

    fn sample_loop(&self) {
        loop {
            std::thread::sleep(SAMPLE_PERIOD);
            if self.stop.load(Ordering::Relaxed) {
                return;
            }
            if !self.cfg.detect {
                continue;
            }
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
                self.sample_once(&name, gen, &procs);
            }
        }
    }

    fn sample_once(&self, name: &str, sess_gen: u64, procs: &[ProcInfo]) {
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
fn apply_size_locked(rt: &mut SessRt, cols: u16, rows: u16) {
    if cols == 0 || rows == 0 || rt.stopped.load(Ordering::Relaxed) {
        return;
    }
    if rt.size() == (cols, rows) {
        return;
    }
    rt.pty.resize(cols, rows);
    rt.ring.note_size(cols, rows);
    if let Some(vt) = rt.vt.as_mut() {
        let _ = vt.resize(cols, rows);
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

/// HELLO 尺寸归一（Go spawnLocked 的 80/24 缺省——0x0 不产生退化几何）。
fn cols_or_default(c: u16) -> u16 {
    if c == 0 { 80 } else { c }
}

fn rows_or_default(r: u16) -> u16 {
    if r == 0 { 24 } else { r }
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
            let stream = UnixStream::connect(path).unwrap();
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
    #[test]
    fn attach_size_applies_to_pty() {
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

    /// P7：serve_stoppable 置停止位后监听线程在窗内退出（engine 实际用的路径）。
    #[test]
    fn serve_stoppable_exits_on_stop() {
        let lines = Arc::new(Mutex::new(Vec::new()));
        let svc = svc_with(TermConfig::default(), lines);
        let (ln, path) = start_listener();
        let stop = Arc::new(AtomicBool::new(false));
        let s2 = Arc::clone(&svc);
        let st2 = Arc::clone(&stop);
        let h = std::thread::spawn(move || s2.serve_stoppable(ln, st2));
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

    /// 找子串的窗口（回放流里 PTY 字节可能跨 DATA 帧分片——拼接后再找）。
    fn window_bytes(d: &[u8]) -> Vec<u8> {
        d.to_vec()
    }

    fn has_bytes(hay: &[u8], needle: &[u8]) -> bool {
        hay.windows(needle.len().max(1)).any(|w| w == needle)
    }
}
