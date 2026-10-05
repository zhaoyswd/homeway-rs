//! speedtest 客户端引擎（隧道内测速，服务端 = 出口拦截层转投 `<state>/speedtest.sock`）。
//!
//! 语义真源 `baseline:pkg/speedtest/{speedtest.go,engine.go}` 客户端侧（R1 子集）：
//! - 线协议：统一帧 `[magic "SPED"][type u8][seq u32 LE][len u16 LE][crc32 IEEE LE][payload]`；
//!   type：1=Request（恒第一帧，客户端先写）2=Start 3=Finish 4=Data（载荷全零）5=Report；
//!   **LE**（与 proto 帧的 BE 相反——评审 S4 钉住）；载荷 crc 盖 IEEE crc32。
//! - 会话：每连接一角色；下行 = N 条 role=recv（**拨齐后统一发请求**对齐各流窗口 t0 +
//!   100ms 滞后余量；每流一线程并发读，窗口内 data payload 才计读数，收 report 收场）；
//!   上行 = N 条 role=send（每流一线程：预热泵 → START → 窗口泵（250ms 分片）→ FINISH →
//!   服务端 report 报数——**速率读数永远由接收端报**）。
//! - 参数默认 = Go 手机口径（down/up 10s、warmup 2s、4 流）；上限校验同 Go
//!   （窗口 ≤15s、预热 ≤5s、流数 ≤6）。
//! - **与 proto 帧语义相反的三处**（评审 S4）：未知帧类型 ⇒ interrupted（不忽略）；
//!   长度端序 LE；上行收口只认 report 帧。

use std::collections::HashMap;
use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

use crate::wgcore::{Client, ConnErr};

/// speedtest 服务端口（隧道 IP 上；出口按 LocalServices 转投）。
pub const SPEEDTEST_PORT: u16 = 7803;
pub(crate) const MAGIC: [u8; 4] = *b"SPED";
pub(crate) const HEADER: usize = 15;
pub(crate) const TYPE_REQUEST: u8 = 1;
pub(crate) const TYPE_START: u8 = 2;
pub(crate) const TYPE_FINISH: u8 = 3;
pub(crate) const TYPE_DATA: u8 = 4;
pub(crate) const TYPE_REPORT: u8 = 5;
/// 发送块（= Go blockBytes：u16 长度场硬顶 64KB-1）。
const BLOCK: usize = 64 * 1024 - 1;
/// 客户端窗口起点对服务端的滞后余量（phaseSlack）。
const PHASE_SLACK: Duration = Duration::from_millis(100);

pub const MAX_STREAMS: usize = 6;
pub const MAX_WINDOW: Duration = Duration::from_secs(15);
pub const MAX_WARMUP: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Params {
    pub down: Duration,
    pub up: Duration,
    pub warmup: Duration,
    pub streams: usize,
}

impl Default for Params {
    /// 手机口径默认（Go DefaultParams）。
    fn default() -> Self {
        Self {
            down: Duration::from_secs(10),
            up: Duration::from_secs(10),
            warmup: Duration::from_secs(2),
            streams: 4,
        }
    }
}

impl Params {
    pub fn normalized(self) -> Result<Self, String> {
        let d = Params::default();
        let mut p = self;
        if p.down.is_zero() {
            p.down = d.down;
        }
        if p.up.is_zero() {
            p.up = d.up;
        }
        if p.warmup.is_zero() {
            p.warmup = d.warmup;
        }
        if p.streams == 0 {
            p.streams = d.streams;
        }
        if p.down > MAX_WINDOW || p.up > MAX_WINDOW {
            return Err(format!("窗口超上限（≤ {MAX_WINDOW:?}）"));
        }
        if p.warmup > MAX_WARMUP {
            return Err(format!("预热超上限（≤ {MAX_WARMUP:?}）"));
        }
        if !(1..=MAX_STREAMS).contains(&p.streams) {
            return Err(format!("并行流数 {} 超出 [1,{MAX_STREAMS}]", p.streams));
        }
        Ok(p)
    }
}

/// 归因词表（Go engine.go:52-63 Reason* 同串；手机 SpeedTestRules.ets 短因分派依赖取值）。
pub const REASON_BUSY: &str = "busy";
pub const REASON_LINK_DOWN: &str = "link_down";
pub const REASON_NOT_SUPPORTED: &str = "not_supported";
pub const REASON_INTERRUPTED: &str = "interrupted";
pub const REASON_TIMEOUT: &str = "timeout";
pub const REASON_CANCELLED: &str = "cancelled";
pub const REASON_INVALID_ARG: &str = "invalid_arg";

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SpeedtestError {
    #[error("测速连接失败：{0}")]
    Conn(#[from] ConnErr),
    #[error("帧协议错误：{0}")]
    Frame(String),
    #[error("服务端报告错误：{0}")]
    Report(String),
    /// 出口测速服务并发满员（服务端 report 的 error=busy——评审 r2-M8/M-9：类型化，
    /// 归因不再走字符串嗅探 contains("满员")）。
    #[error("出口测速服务并发满员，请稍后再试")]
    Busy,
    /// 链路正在恢复（服务端 report 的 error=link_down——同上类型化）。
    #[error("链路正在恢复")]
    LinkDown,
    /// 用户取消（评审 r2-M8：取消此前被归因成 interrupted——REASON_CANCELLED 死
    /// 常量、tier「已取消」分支不可达；类型化后 reason 面直达）。
    #[error("已取消")]
    Cancelled,
    #[error("参数非法：{0}")]
    InvalidArg(String),
    /// 出口没有测速服务（首帧前 EOF/复位——Go :625-643 的 not_supported 判据）。
    #[error("not_supported: 出口没有测速服务")]
    NotSupported,
    /// 注入缝（拨号闭包）错误透传：bridge_down / bridge_auth——App 形态的桥鉴权
    /// 拨号面产生，引擎原样透传（Go *speedtest.DialError{Code} 同义）。
    #[error("{0}: {1}")]
    Bridge(&'static str, String),
}

impl SpeedtestError {
    /// 归因码（App 短因分派面；Go Result.Reason 同串）。
    pub fn reason(&self) -> &'static str {
        match self {
            SpeedtestError::Busy => REASON_BUSY,
            SpeedtestError::LinkDown => REASON_LINK_DOWN,
            SpeedtestError::Cancelled => REASON_CANCELLED,
            SpeedtestError::Report(_) => REASON_INTERRUPTED,
            SpeedtestError::NotSupported => REASON_NOT_SUPPORTED,
            SpeedtestError::Frame(_) => REASON_INTERRUPTED,
            SpeedtestError::Conn(ConnErr::Timeout) => REASON_TIMEOUT,
            SpeedtestError::Conn(_) => REASON_INTERRUPTED,
            SpeedtestError::InvalidArg(_) => REASON_INVALID_ARG,
            SpeedtestError::Bridge(code, _) => code,
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SpeedtestResult {
    pub down_bps: f64,
    pub up_bps: f64,
    pub usage_down: i64,
    pub usage_up: i64,
    pub wall_ms: u64,
}

/// crc32（IEEE，反射式）——Go `crc32.ChecksumIEEE` 同义（逐位小实现；data 帧载荷全零
/// 按长度缓存，热路径零重复计算）。
pub(crate) fn crc32_ieee(buf: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &b in buf {
        crc ^= b as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xEDB8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

pub(crate) fn zero_crc(n: usize) -> u32 {
    std::thread_local! {
        static CACHE: std::cell::RefCell<HashMap<usize, u32>> =
            std::cell::RefCell::new(HashMap::new());
    }
    CACHE.with(|c| {
        *c.borrow_mut()
            .entry(n)
            .or_insert_with(|| crc32_ieee(&vec![0u8; n]))
    })
}

/// 一条待写帧（头 + 载荷拼好；跨 write_all 部分写复用同一 Vec）。
struct Frame {
    buf: Vec<u8>,
    seq: u32,
}

impl Frame {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(HEADER + BLOCK),
            seq: 0,
        }
    }

    fn header(&mut self, typ: u8, seq: u32, payload_len: usize, crc: u32) {
        // seq 由调用方决定（控制帧恒 0 不推进；data 帧前置自增——Go WriteControl/
        // writeData 同义；评审高-1：此处不得自增，否则 data seq 变 1,3,5…）
        self.buf.clear();
        self.buf.reserve(HEADER + payload_len);
        self.buf.extend_from_slice(&MAGIC);
        self.buf.push(typ);
        self.buf.extend_from_slice(&seq.to_le_bytes());
        self.buf
            .extend_from_slice(&(payload_len as u16).to_le_bytes());
        self.buf.extend_from_slice(&crc.to_le_bytes());
    }

    /// 控制帧（request/start/finish；载荷进 crc；**seq 恒 0**——Go WriteControl 同义）。
    fn control(&mut self, typ: u8, payload: &[u8]) -> &[u8] {
        self.header(typ, 0, payload.len(), crc32_ieee(payload));
        self.buf.extend_from_slice(payload);
        &self.buf
    }

    /// data 帧（载荷全零，整帧拼好——单次写全，避免头/载荷拆包；seq 从 1 起递增，
    /// Go writeData 的 `*seq+1` 前置自增同义）。载荷必须 ≤ u16 上限（Go writeData
    /// 对超限报错而非静默截断——低-12）。
    fn data(&mut self, payload_len: usize) -> Result<&[u8], SpeedtestError> {
        if payload_len > u16::MAX as usize {
            return Err(SpeedtestError::Frame(format!(
                "data 帧载荷 {payload_len} 超过 u16 上限 {}",
                u16::MAX
            )));
        }
        self.seq += 1;
        self.header(TYPE_DATA, self.seq, payload_len, zero_crc(payload_len));
        let start = self.buf.len();
        self.buf.resize(start + payload_len, 0);
        Ok(&self.buf)
    }
}

/// 测速连接抽象（App 形态经本机测速桥〔UDS + 鉴权〕、CLI 形态直连引擎——
/// 工单⑤ speedtest 接桥：引擎只依赖本面，两种承载各自实现）。
pub trait SpeedConn: Send + Sync {
    /// 全量写（发送缓冲满时的部分写语义由实现侧处理——重试/背压）。
    fn write_frame(&self, data: &[u8]) -> Result<(), SpeedtestError>;
    /// 阻塞读一段（空 Vec = EOF——映射 Frame("流在帧中途关闭") 语义）。
    fn read_some(&self) -> Result<Vec<u8>, SpeedtestError>;
    /// 关闭（收口/看门狗中断用；幂等）。
    fn kill(&self);
    /// 请求发出后的**每连接读写硬期限**（Go `SetDeadline(now+warmup+window+
    /// connBudget)` 同义——R8-8c 处置 F8 登记项：桥接形态在窗口期读阻塞时按
    /// 期限打断，不挂到引擎看门狗；CLI/栈内形态缺省 no-op（看门狗已覆盖）。
    fn set_deadline(&self, _d: Option<Duration>) {}
}

/// 请求发出后每连接的读写硬期限裕量（Go connBudget = 15s，engine.go:72 同值）。
const CONN_BUDGET: Duration = Duration::from_secs(15);

/// CLI 形态承载（Arc<Client> + 流 id）。
struct ClientConn {
    client: std::sync::Arc<Client>,
    id: u64,
}

/// 由既有引擎连接构造 SpeedConn（daemon 承载面 speedtest runner 的拨号腿——
/// 会话侧 healing 拨号拿到 conn id 后经本面交给引擎；与 CLI 形态同一承载实现）。
pub fn engine_conn(client: std::sync::Arc<Client>, id: u64) -> std::sync::Arc<dyn SpeedConn> {
    std::sync::Arc::new(ClientConn { client, id })
}

impl SpeedConn for ClientConn {
    fn write_frame(&self, data: &[u8]) -> Result<(), SpeedtestError> {
        let mut off = 0;
        let mut zero_streak = 0u32;
        // R8-3 F12：零接纳时引擎带回原 Vec——重试环不重拷（与 SessionWriteHalf 同款）。
        let mut pending: Option<Vec<u8>> = None;
        while off < data.len() {
            let chunk = match pending.take() {
                Some(v) => v,
                None => data[off..].to_vec(),
            };
            let w = self.client.write(self.id, chunk).map_err(SpeedtestError::Conn)?;
            if w.n == 0 {
                pending = w.back;
                zero_streak += 1;
                if zero_streak > 1_000_000 {
                    return Err(SpeedtestError::Frame("写通道长时间无进展".into()));
                }
                std::thread::yield_now();
                continue;
            }
            zero_streak = 0;
            off += w.n;
        }
        Ok(())
    }
    fn read_some(&self) -> Result<Vec<u8>, SpeedtestError> {
        match self.client.read(self.id) {
            Ok(v) => Ok(v),
            Err(ConnErr::Closed) => Ok(Vec::new()), // EOF
            Err(e) => Err(SpeedtestError::Conn(e)),
        }
    }
    fn kill(&self) {
        let _ = self.client.close(self.id);
    }
}

enum FrameIn {
    /// data 帧：载荷已在读侧消耗丢弃，只回长度（下行零拷贝读路径）。
    Data {
        payload_len: usize,
    },
    Other {
        typ: u8,
        payload: Vec<u8>,
    },
}

/// 流式帧读取器（TCP 是字节流：块边界 ≠ 帧边界——读进内部缓冲后按帧解析；
/// data 帧载荷不拷出（下行计数按头长度），控制帧载荷带回）。
struct FrameReader {
    buf: Vec<u8>,
}

impl FrameReader {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(128 * 1024),
        }
    }

    /// 读一帧：内部缓冲不足时从连接补读（SpeedConn 面——桥/直连同构）。
    fn read_frame(&mut self, conn: &dyn SpeedConn) -> Result<FrameIn, SpeedtestError> {
        loop {
            if let Some(f) = self.try_parse()? {
                return Ok(f);
            }
            let chunk = conn.read_some()?;
            if chunk.is_empty() {
                return Err(SpeedtestError::Frame("流在帧中途关闭".into()));
            }
            self.buf.extend_from_slice(&chunk);
        }
    }

    /// 缓冲够一帧则解析并消费；否则 None。
    fn try_parse(&mut self) -> Result<Option<FrameIn>, SpeedtestError> {
        let Some((head, payload)) = decode_frame(&self.buf)? else {
            return Ok(None); // 帧未到齐
        };
        let n = HEADER + head.len;
        if head.typ == TYPE_DATA {
            self.buf.drain(..n);
            return Ok(Some(FrameIn::Data {
                payload_len: head.len,
            }));
        }
        let payload = payload.to_vec();
        self.buf.drain(..n);
        Ok(Some(FrameIn::Other {
            typ: head.typ,
            payload,
        }))
    }
}

/// 构造一帧 report 控制帧（Go WriteControl(TypeReport, payload) 的公共件——
/// 桥宿主 link_down 回复面消费；error 形如 `{"error":"link_down"}`）。
pub fn make_report_frame(error_json: &str) -> Vec<u8> {
    let mut w = Frame::new();
    w.control(TYPE_REPORT, error_json.as_bytes()).to_vec()
}

/// 帧头（纯数据视图——`decode_frame` 的产物；fuzz/向量对照的公共面）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameHead {
    pub typ: u8,
    pub seq: u32,
    pub len: usize,
    pub crc: u32,
}

/// 纯头解析（魔数 + 字段视图，无 IO/无消费）。**服务端读循环与 `decode_frame`
/// 共用的单一真源**（R5 第二道门 高-4 整改：服务端 reader 原为同语义重实现）。
pub fn decode_head(hdr: &[u8]) -> Result<FrameHead, SpeedtestError> {
    if hdr.len() < HEADER {
        return Err(SpeedtestError::Frame("帧头未到齐（<15B）".into()));
    }
    if hdr[..4] != MAGIC {
        return Err(SpeedtestError::Frame(
            "帧魔数不符（流已错位或非 speedtest 服务）".into(),
        ));
    }
    Ok(FrameHead {
        typ: hdr[4],
        seq: u32::from_le_bytes([hdr[5], hdr[6], hdr[7], hdr[8]]),
        len: u16::from_le_bytes([hdr[9], hdr[10]]) as usize,
        crc: u32::from_le_bytes(hdr[11..15].try_into().expect("定长")),
    })
}

/// 纯解析（无 IO/无消费）：缓冲里的帧头 + 载荷切片。`Ok(None)` = 帧未到齐。
/// 魔数/crc 校验在此层（与 Go readFrame 同判位）。**pub 是测试/fuzz 可达面**
///（R5 评审 ②-1 前置：IO 留在 FrameReader 薄封装）。
pub fn decode_frame(buf: &[u8]) -> Result<Option<(FrameHead, &[u8])>, SpeedtestError> {
    if buf.len() < HEADER {
        return Ok(None);
    }
    let head = decode_head(buf)?;
    if buf.len() < HEADER + head.len {
        return Ok(None); // 帧未到齐
    }
    let payload = &buf[HEADER..HEADER + head.len];
    if head.typ == TYPE_DATA {
        // crc 盖全零载荷（按长度缓存）
        if head.crc != zero_crc(head.len) {
            return Err(SpeedtestError::Frame("data 帧 crc 不符".into()));
        }
    } else if head.crc != crc32_ieee(payload) {
        return Err(SpeedtestError::Frame("帧 crc 不符".into()));
    }
    Ok(Some((head, payload)))
}

/// 请求帧载荷（JSON，Go requestJSON 同形）。
fn request_payload(role: &str, warmup_ms: u64, window_ms: u64) -> Vec<u8> {
    format!(r#"{{"role":"{role}","warmup_ms":{warmup_ms},"window_ms":{window_ms}}}"#).into_bytes()
}

#[derive(Default, Clone, Copy, Debug)]
pub struct Report {
    pub bytes: i64,
    pub warmup_bytes: i64,
    pub wall_ms: i64,
}

/// 顶层逗号切分（引号感知，R2 低-4 补修）：只在双引号外把 `,` 当分隔符——字符串值
/// 内的逗号/`}` 不再错分（error 值是自由文案面：窗口期已知不含 ASCII 逗号，但 fuzz
/// 对抗输入与后续文案演进不能依赖这一点）。`\"` 转义连下一字节跳过防提前闭合。
pub(crate) fn split_top_level(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut in_str = false;
    let bytes = s.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' if in_str => {
                i += 1; // 转义：跳过下一个字节（`\,`/`\"` 都不参与分隔/闭合判定）
            }
            b'"' => in_str = !in_str,
            b',' if !in_str => {
                out.push(&s[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&s[start..]);
    out
}

/// 极小 JSON unescape（error 值保真）：`\"`→`"`、`\\`→`\`；其余转义序列原样保留
///（error 值下游只做码匹配与展示，码值不含转义面——够用且不扩大手解面）。
pub(crate) fn unescape_minimal(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// report 帧载荷的手解（pub = fuzz 可达面——服务端回包的自由 JSON 文本面）。
pub fn parse_report(payload: &[u8]) -> Result<Report, SpeedtestError> {
    // 极小 JSON 面（三数字 + 可选 error 串）——手解避免 serde_json 进 core（依赖纪律）。
    let s = std::str::from_utf8(payload)
        .map_err(|_| SpeedtestError::Frame("report 非 UTF-8".into()))?;
    let mut r = Report::default();
    let mut error: Option<String> = None;
    for kv in split_top_level(s.trim_matches(['{', '}'])) {
        let Some((k, v)) = kv.split_once(':') else {
            continue;
        };
        let k = k.trim().trim_matches('"');
        let v = v.trim();
        match k {
            "bytes" => {
                r.bytes = v
                    .parse()
                    .map_err(|_| SpeedtestError::Frame("report bytes 非法".into()))?
            }
            "warmup_bytes" => {
                r.warmup_bytes = v
                    .parse()
                    .map_err(|_| SpeedtestError::Frame("report warmup_bytes 非法".into()))?
            }
            "wall_ms" => {
                r.wall_ms = v
                    .parse()
                    .map_err(|_| SpeedtestError::Frame("report wall_ms 非法".into()))?
            }
            "error" => {
                let e = unescape_minimal(v.trim_matches('"'));
                if !e.is_empty() && v != "null" {
                    error = Some(e);
                }
            }
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(match e.as_str() {
            "busy" => SpeedtestError::Busy,
            "link_down" => SpeedtestError::LinkDown,
            other => SpeedtestError::Report(other.to_owned()),
        });
    }
    Ok(r)
}

/// 轮内连接守卫：任何退出路径（含 `?` 提前返回）关闭全部已拨连接（Go closeAll 同义，
/// 评审高-2/中-12；SpeedConn 面——kill 语义由承载实现〔引擎 close / 桥 shutdown〕）。
struct ConnGuard {
    conns: std::sync::Arc<std::sync::Mutex<Vec<std::sync::Arc<dyn SpeedConn>>>>,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        let conns = self.conns.lock().expect("连接表锁中毒").clone();
        for c in conns {
            c.kill();
        }
    }
}

/// 引擎进度出口（评审 r2-M7：SpeedHost 的 live/dir 数据源——相位切换 + 字节累计
/// 的原子面；status 侧按轮询间隔差分算 instBps）。phase：0=无 1=down 2=up。
#[derive(Debug, Default)]
pub struct LiveProgress {
    phase: std::sync::atomic::AtomicU8,
    bytes: std::sync::atomic::AtomicI64,
}

impl LiveProgress {
    pub fn new() -> Self {
        Self::default()
    }

    fn set_phase(&self, p: u8) {
        self.phase.store(p, std::sync::atomic::Ordering::Release);
    }

    fn add_bytes(&self, n: i64) {
        self.bytes
            .fetch_add(n, std::sync::atomic::Ordering::Relaxed);
    }

    fn reset(&self) {
        self.phase.store(0, std::sync::atomic::Ordering::Release);
        self.bytes.store(0, std::sync::atomic::Ordering::Relaxed);
    }

    /// (phase, 累计字节)——status 面消费；phase 词面 "down"/"up"/None。
    pub fn snapshot(&self) -> (Option<&'static str>, i64) {
        match self.phase.load(std::sync::atomic::Ordering::Acquire) {
            1 => (
                Some("down"),
                self.bytes.load(std::sync::atomic::Ordering::Relaxed),
            ),
            2 => (
                Some("up"),
                self.bytes.load(std::sync::atomic::Ordering::Relaxed),
            ),
            _ => (None, 0),
        }
    }
}

/// 总预算看门狗（Go watchdogFix 口径）：到点关全部连接——把可能卡死的读/泵线程
/// 从待决 RPC 里解出来（close ⇒ 引擎结算 EOF/Closed）。到点先置 timed_out 旗标
/// （R3-M24：后续读线程收到的 Closed/Frame 错误统一归因 timeout——「窗口没跑完」
/// 与「链路死」是同一个根因面，不应报 interrupted）。
fn watchdog_cancellable(
    conns: std::sync::Arc<std::sync::Mutex<Vec<std::sync::Arc<dyn SpeedConn>>>>,
    budget: Duration,
    done: std::sync::mpsc::Receiver<()>,
    timed_out: &std::sync::atomic::AtomicBool,
    cancelled: &std::sync::atomic::AtomicBool,
    external_cancel: Option<&std::sync::atomic::AtomicBool>,
) {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(cx) = external_cancel {
            if cx.load(std::sync::atomic::Ordering::Acquire) {
                cancelled.store(true, std::sync::atomic::Ordering::Release);
                let first = conns.lock().expect("连接表锁中毒").clone();
                for c in first {
                    c.kill();
                }
                // 复核 r3-F8：取消后**不退出**——继续守到 done/父预算烧尽，周期性
                // 再杀迟登记的连接（在途拨号成功入表后无人杀 ⇒ 读线程无期限阻塞 ⇒
                // SpeedHost 恒 busy；timeout 分支预算已尽，维持原退出语义〔SpeedConn
                // 的 Go SetDeadline 同义期限面登记 R8〕）
                loop {
                    match done.recv_timeout(Duration::from_millis(200)) {
                        Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            let late = conns.lock().expect("连接表锁中毒").clone();
                            for c in late {
                                c.kill();
                            }
                            if Instant::now() >= deadline {
                                return;
                            }
                        }
                    }
                }
            }
        }
        match done.recv_timeout(Duration::from_millis(200)) {
            Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if Instant::now() >= deadline {
                    timed_out.store(true, std::sync::atomic::Ordering::Release);
                    let conns = conns.lock().expect("连接表锁中毒").clone();
                    for c in conns {
                        c.kill();
                    }
                    return;
                }
            }
        }
    }
}

/// 跑完整一轮（下行 → 上行；读数由服务端 report 报）。拨号目标恒隧道 IP:7803。
pub fn run(
    client: &std::sync::Arc<Client>,
    params: Params,
    logf: &dyn Fn(&str),
) -> Result<SpeedtestResult, SpeedtestError> {
    // CLI 形态：拨号闭包 = ClientConn（每流一连接，直连引擎）；无进度出口消费方
    let dial = || -> Result<std::sync::Arc<dyn SpeedConn>, SpeedtestError> {
        let id = client
            .connect(SocketAddrV4::new(
                crate::wgcore::SERVER_TUNNEL_IP,
                SPEEDTEST_PORT,
            ))
            .map_err(SpeedtestError::Conn)?;
        Ok(std::sync::Arc::new(ClientConn {
            client: std::sync::Arc::clone(client),
            id,
        }))
    };
    run_dial(&dial, params, logf, None, None)
}

/// App/CLI 双入口的引擎主体（拨号闭包形态——Go speed.Start(ctx, dial, params) 同构；
/// 工单⑤ speedtest 接桥：App 形态的 dial = 本机测速桥〔UDS + 鉴权首包〕）。
/// `cancel`：外部取消位（Cancel 导出面置位 → 看门狗分片轮询发现 → kill 全部连接，
/// 本轮以 `cancelled` 收场——Go 的 cancel 关连接同义）。
/// `live`：进度出口（M-7：相位切换 + 字节累计——SpeedHost 的 Status 面消费）。
pub fn run_dial(
    dial: &dyn Fn() -> Result<std::sync::Arc<dyn SpeedConn>, SpeedtestError>,
    params: Params,
    logf: &dyn Fn(&str),
    cancel: Option<&std::sync::atomic::AtomicBool>,
    live: Option<&std::sync::Arc<LiveProgress>>,
) -> Result<SpeedtestResult, SpeedtestError> {
    let p = params.normalized().map_err(SpeedtestError::InvalidArg)?;
    if let Some(l) = live {
        l.reset();
    }
    let conns = std::sync::Arc::new(std::sync::Mutex::new(
        Vec::<std::sync::Arc<dyn SpeedConn>>::new(),
    ));
    let _guard = ConnGuard {
        conns: std::sync::Arc::clone(&conns),
    };
    let (wd_tx, wd_rx) = std::sync::mpsc::channel::<()>();
    let budget = Duration::from_secs(60) + p.warmup + p.down + p.up;
    let wd_conns = std::sync::Arc::clone(&conns);
    let timed_out = std::sync::atomic::AtomicBool::new(false);
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    let wd_flag = &timed_out;
    let wd_cancelled = &cancelled;
    let body = std::thread::scope(|scope| {
        scope.spawn(move || {
            watchdog_cancellable(wd_conns, budget, wd_rx, wd_flag, wd_cancelled, cancel)
        });
        let r = run_phases(dial, &p, logf, &conns, cancel, live);
        // R8-3 尾账（F14 测试写出）：收工信号必须在 **scope join 之前** 发——
        // std::thread::scope 返回即 join 看门狗线程，而它只认 done/预算烧尽；
        // 旧位（scope 之后）发 = 永远晚于 join ⇒ 每轮白等满 60s 基础预算
        // （CLI 实测 3 轮 246s = 3×(22s 相位 + 60s 空等)；App 形态同罪）。
        let _ = wd_tx.send(());
        r
    });
    let _ = wd_tx.send(()); // 幂等兜底（recv 侧单消费即退）
                            // 取消旗标**优先**于一切归因（复核 r3-F7：Go engine.go finish 先判 isCancelled，
                            // 任何已归因错误被取消覆盖——此前白名单外的 NotSupported/Report 会抢先，
                            // 下行首帧前取消会误报「出口需升级」）。
    if cancelled.load(std::sync::atomic::Ordering::Acquire) && body.is_err() {
        return Err(SpeedtestError::Cancelled);
    }
    // M24：看门狗触发（预算烧满）时，读/泵线程带出的连接级错误统一归因 timeout
    //（窗口没跑完 = 链路死/服务端卡，不是 interrupted）。Report/InvalidArg/NotSupported
    // 等服务端语义错误不改写。
    if body.is_err() {
        if let Some(l) = live {
            l.reset();
        }
    }
    if timed_out.load(std::sync::atomic::Ordering::Acquire) {
        if let Err(e) = &body {
            if matches!(e, SpeedtestError::Conn(_) | SpeedtestError::Frame(_)) {
                return Err(SpeedtestError::Conn(ConnErr::Timeout));
            }
        }
    }
    body
}

/// 看门狗（可取消形态）：分片轮询取消位（200ms）——取消触发 = kill 全部连接 +
/// cancelled 旗标（归因面在 run_dial 尾部）。
fn run_phases(
    dial: &dyn Fn() -> Result<std::sync::Arc<dyn SpeedConn>, SpeedtestError>,
    p: &Params,
    logf: &dyn Fn(&str),
    conns: &std::sync::Arc<std::sync::Mutex<Vec<std::sync::Arc<dyn SpeedConn>>>>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    live: Option<&std::sync::Arc<LiveProgress>>,
) -> Result<SpeedtestResult, SpeedtestError> {
    let t0 = Instant::now();
    logf(&format!(
        "speedtest: 开跑（down {}流 warmup={} window={}）",
        p.streams,
        crate::go_fmt::fmt_duration_go_ms(p.warmup),
        crate::go_fmt::fmt_duration_go_ms(p.down)
    ));
    // 评审 r2-M8：拨号间隙查取消位（在途单次拨号自带 ≤10s 预算兜底；Go 用 ctx
    // 即刻打断——差值是「取消生效点延到拨号预算边界」，登记）
    let cancel_hit = || cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Acquire));

    // ---- 下行：拨齐全部连接 → 统一发请求（各流窗口对齐，design D3）----
    let mut down_conns = Vec::with_capacity(p.streams);
    for _ in 0..p.streams {
        if cancel_hit() {
            return Err(SpeedtestError::Cancelled);
        }
        let c = dial()?;
        conns
            .lock()
            .expect("连接表锁中毒")
            .push(std::sync::Arc::clone(&c));
        down_conns.push(c);
    }
    if let Some(l) = live {
        l.set_phase(1); // down
    }
    let mut w = Frame::new();
    for c in &down_conns {
        let payload = request_payload(
            "recv",
            p.warmup.as_millis() as u64,
            p.down.as_millis() as u64,
        );
        c.write_frame(w.control(TYPE_REQUEST, &payload))?;
        // Go SetDeadline 同义（engine.go:573）：请求已发出——本连接后续读写按
        // warmup+window+15s 硬期限收口（R8-8c：F8 登记的 timeout 分支极端形态）。
        c.set_deadline(Some(p.warmup + p.down + CONN_BUDGET));
    }
    let window_start = Instant::now() + p.warmup + PHASE_SLACK;
    // 每流一线程并发读（Go goroutine 同构；窗口内到达才计读数）
    let mut down_bytes: i64 = 0;
    let mut down_usage: i64 = 0;
    let mut srv_bytes: i64 = 0;
    let mut srv_warm: i64 = 0;
    let down_join: Result<(), SpeedtestError> = std::thread::scope(|s| {
        let live2 = live.cloned();
        let handles: Vec<_> = down_conns
            .iter()
            .map(|c| {
                let c = std::sync::Arc::clone(c);
                let live2 = live2.clone();
                s.spawn(move || -> Result<(i64, i64, i64, i64), SpeedtestError> {
                    let (mut got, mut used) = (0i64, 0i64);
                    let (sb, sw): (i64, i64);
                    let mut fr = FrameReader::new();
                    // L10 修复（R3-design §6 风险表）：first_frame 此前恒 true ⇒ 窗口期
                    // 任何帧错误都被降级 not_supported——成功读到首帧后必须复位。
                    let mut first_frame = true;
                    loop {
                        let fin = match fr.read_frame(c.as_ref()).map_err(|e| {
                            // 首帧前 EOF/通道关 = 出口没有测速服务（连接被出口侧立即收流）
                            if first_frame
                                && matches!(
                                    e,
                                    SpeedtestError::Conn(ConnErr::Closed)
                                        | SpeedtestError::Frame(_)
                                )
                            {
                                SpeedtestError::NotSupported
                            } else {
                                e
                            }
                        }) {
                            Ok(f) => f,
                            Err(e) => return Err(e),
                        };
                        first_frame = false;
                        match fin {
                            FrameIn::Data { payload_len } => {
                                let now = Instant::now();
                                if let Some(l) = live2.as_ref() {
                                    l.add_bytes(payload_len as i64);
                                }
                                if now >= window_start && now < window_start + p.down {
                                    got += payload_len as i64;
                                } else {
                                    used += payload_len as i64;
                                }
                            }
                            FrameIn::Other { typ, payload } if typ == TYPE_REPORT => {
                                let rep = parse_report(&payload)?;
                                sb = rep.bytes;
                                sw = rep.warmup_bytes;
                                break;
                            }
                            FrameIn::Other { typ, .. } => {
                                return Err(SpeedtestError::Frame(format!("窗口期收到类型 {typ}")))
                            }
                        }
                    }
                    Ok((got, used, sb, sw))
                })
            })
            .collect();
        for h in handles {
            let (g, u, sb, sw) = h
                .join()
                .map_err(|_| SpeedtestError::Frame("下行读线程 panic".into()))??;
            down_bytes += g;
            down_usage += g + u;
            srv_bytes += sb;
            srv_warm += sw;
        }
        Ok(())
    });
    down_join?;
    if srv_bytes > 0 {
        let dev = (srv_bytes - down_bytes) as f64 / srv_bytes as f64 * 100.0;
        logf(&format!(
            "speedtest: 下行对账（接收端窗内={down_bytes}B 服务端窗内={srv_bytes}B 预热={srv_warm}B 偏差={dev:.2}%）"
        ));
    }
    let down_bps = down_bytes as f64 / p.down.as_secs_f64();

    // ---- 上行：新连接（每流一角色不复用）；每流一线程泵送 ----
    // 拨号窗相位回空档（复核 r3-F13：Go 相位回「建立连接」——Status 面对应
    // connecting，不带着 down 误报）
    if let Some(l) = live {
        l.set_phase(0);
    }
    let mut up_conns = Vec::with_capacity(p.streams);
    for _ in 0..p.streams {
        if cancel_hit() {
            return Err(SpeedtestError::Cancelled);
        }
        let c = dial()?;
        conns
            .lock()
            .expect("连接表锁中毒")
            .push(std::sync::Arc::clone(&c));
        up_conns.push(c);
    }
    if let Some(l) = live {
        l.set_phase(2); // up（相位切换点——字节计数器连续累计〔含预热字节：Rust
                        // 进度面口径，Go 的 liveBytes 只计窗内——登记为已知口径差〕）
    }
    for c in &up_conns {
        let payload = request_payload("send", p.warmup.as_millis() as u64, p.up.as_millis() as u64);
        c.write_frame(w.control(TYPE_REQUEST, &payload))?;
        // 同下行：Go SetDeadline 同义的硬期限（pump 的收口 report 读也罩在内）。
        c.set_deadline(Some(p.warmup + p.up + CONN_BUDGET));
    }
    let mut up_bytes: i64 = 0;
    let mut up_usage: i64 = 0;
    let mut wall_sum: i64 = 0;
    let up_join: Result<(), SpeedtestError> = std::thread::scope(|s| {
        let live2 = live.cloned();
        let handles: Vec<_> = up_conns
            .iter()
            .map(|c| {
                let c = std::sync::Arc::clone(c);
                let live2 = live2.clone();
                s.spawn(move || -> Result<(i64, i64, i64), SpeedtestError> {
                    let mut f = Frame::new();
                    // 预热泵 → START → 窗口泵 → FINISH → report（接收端报数）
                    let warm = pump(c.as_ref(), &mut f, p.warmup, live2.as_deref())?;
                    c.write_frame(f.control(TYPE_START, &[]))?;
                    let win = pump(c.as_ref(), &mut f, p.up, live2.as_deref())?;
                    c.write_frame(f.control(TYPE_FINISH, &[]))?;
                    let (used, bytes, wall) = {
                        let mut fr = FrameReader::new();
                        match fr.read_frame(c.as_ref())? {
                            FrameIn::Other { typ, payload } if typ == TYPE_REPORT => {
                                let rep = parse_report(&payload)?;
                                (warm + win, rep.bytes, rep.wall_ms)
                            }
                            FrameIn::Other { typ, .. } => {
                                return Err(SpeedtestError::Frame(format!("窗口期收到类型 {typ}")))
                            }
                            FrameIn::Data { .. } => {
                                return Err(SpeedtestError::Frame("收口期收到 data 帧".into()))
                            }
                        }
                    };
                    Ok((used, bytes, wall))
                })
            })
            .collect();
        for h in handles {
            let (used, bytes, wall) = h
                .join()
                .map_err(|_| SpeedtestError::Frame("上行泵线程 panic".into()))??;
            up_usage += used;
            up_bytes += bytes;
            wall_sum += wall;
        }
        Ok(())
    });
    up_join?;
    // 上行分母 = 服务端实测墙钟均值（名义 10s 有 ~1% 虚高，Go 同口径）
    let denom = if wall_sum > 0 {
        Duration::from_millis((wall_sum / p.streams as i64).max(1) as u64)
    } else {
        p.up
    };
    let up_bps = up_bytes as f64 / denom.as_secs_f64();

    let wall_ms = t0.elapsed().as_millis() as u64;
    logf(&format!(
        "speedtest: 完成（down={:.0}B/s（{:.2}Mbps） up={:.0}B/s（{:.2}Mbps） 用量={}MB 用时={}s）",
        down_bps,
        down_bps * 8.0 / 1e6,
        up_bps,
        up_bps * 8.0 / 1e6,
        (down_usage + up_usage) / (1 << 20),
        wall_ms / 1000
    ));
    Ok(SpeedtestResult {
        down_bps,
        up_bps,
        usage_down: down_usage,
        usage_up: up_usage,
        wall_ms,
    })
}

/// 泵一段 data（250ms 分片节奏——Go PumpDataChunk 形态：分片边界 = live 字节更新点
/// 与取消检查点；整帧单次写全；返回本段 payload 字节数）。
fn pump(
    conn: &dyn SpeedConn,
    f: &mut Frame,
    dur: Duration,
    live: Option<&LiveProgress>,
) -> Result<i64, SpeedtestError> {
    const SLICE: Duration = Duration::from_millis(250);
    let deadline = Instant::now() + dur;
    let mut total: i64 = 0;
    let mut reported: i64 = 0;
    while Instant::now() < deadline {
        let slice_end =
            Instant::now() + SLICE.min(deadline.saturating_duration_since(Instant::now()));
        while Instant::now() < slice_end {
            conn.write_frame(f.data(BLOCK)?)?;
            total += BLOCK as i64;
        }
        // 分片边界：live 字节上报**本片增量**（复核 r3-F4：此前每片加运行累计 ⇒
        // n 片后 ≈ 真值 ×(n+1)/2 虚高；Go engine.go「字节数必须用分片泵的返回值」）
        if let Some(l) = live {
            l.add_bytes(total - reported);
            reported = total;
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 帧编解码往返 + LE/crc 语义（对拍 Go speedtest.go 的字节形状）。
    #[test]
    fn frame_shape_and_crc_semantics() {
        let mut f = Frame::new();
        let payload = request_payload("recv", 2000, 10000);
        let frame = f.control(TYPE_REQUEST, &payload).to_vec();
        assert_eq!(&frame[..4], b"SPED");
        assert_eq!(frame[4], TYPE_REQUEST);
        assert_eq!(
            u32::from_le_bytes(frame[5..9].try_into().unwrap()),
            0,
            "控制帧 seq 恒 0（Go 同义）"
        );
        assert_eq!(
            u16::from_le_bytes(frame[9..11].try_into().unwrap()) as usize,
            payload.len()
        );
        let crc = u32::from_le_bytes(frame[11..15].try_into().unwrap());
        assert_eq!(crc, crc32_ieee(&payload), "crc 盖 payload（LE 放置）");
        assert_eq!(&frame[15..], &payload[..]);

        // crc32 IEEE 自证（"123456789" ⇒ 0xCBF43926）
        assert_eq!(crc32_ieee(b"123456789"), 0xCBF4_3926);

        // data 帧：全零载荷 crc 按长度缓存（与逐位计算一致）
        assert_eq!(zero_crc(16), crc32_ieee(&[0u8; 16]));
        assert_eq!(zero_crc(BLOCK), crc32_ieee(&vec![0u8; BLOCK]));
        // data 帧 seq 从 1 起逐帧 +1；控制帧不推进计数（评审高-1 断言）
        let mut f2 = Frame::new();
        let d1 = f2.data(8).unwrap().to_vec();
        assert_eq!(u32::from_le_bytes(d1[5..9].try_into().unwrap()), 1);
        let _ = f2.control(TYPE_START, &[]);
        let d2 = f2.data(8).unwrap().to_vec();
        assert_eq!(
            u32::from_le_bytes(d2[5..9].try_into().unwrap()),
            2,
            "控制帧不得推进 seq"
        );
        assert_eq!(d2.len(), HEADER + 8);
        assert_eq!(&d2[HEADER..], &[0u8; 8]);
    }

    #[test]
    fn data_payload_over_u16_rejected() {
        let mut f = Frame::new();
        assert!(f.data(70_000).is_err(), "超 u16 上限必须报错（低-12）");
        assert!(f.data(BLOCK).is_ok(), "常量块（64KB-1）必须可发");
    }

    #[test]
    fn params_validation_matches_go_bounds() {
        assert!(Params::default().normalized().is_ok());
        assert!(
            Params {
                down: Duration::from_secs(16),
                ..Default::default()
            }
            .normalized()
            .is_err(),
            "窗口超 15s 应拒绝"
        );
        assert!(Params {
            streams: 7,
            ..Default::default()
        }
        .normalized()
        .is_err());
        assert!(Params {
            warmup: Duration::from_secs(6),
            ..Default::default()
        }
        .normalized()
        .is_err());
        let n = Params {
            down: Duration::ZERO,
            up: Duration::ZERO,
            warmup: Duration::ZERO,
            streams: 0,
        }
        .normalized()
        .unwrap();
        assert_eq!(n, Params::default(), "零值取默认");
    }

    /// F14（R8-3 尾账）：读期限到点的归因回归——Go 对照 `TestEngineReadDeadline
    /// Interrupted`（exec-r1 L4：期限到点 ≠ 判据②，不得冒充 not_supported）。**口径
    /// 注记**：Go 侧到点按 interrupted 呈现；Rust 侧 R3-M24 既定口径 = 窗口没跑完
    /// 与链路死同根因，统一 **timeout**——本测试钉死的是「到点绝不冒充
    /// not_supported（出口没有测速服务）」这个两侧共有的不变量 + 我方口径本身。
    /// 两形态与 Go 同构：①hold（请求后永无应答——下行首帧前期限到点）；②halfhold
    /// （下行相位正常收场、上行收口 report 缺席到点）。
    #[test]
    fn read_deadline_expiry_attribution() {
        use std::sync::Mutex;
        /// 钳制期限的假连接：`set_deadline` 把引擎给的期限钳到 ≤300ms（Go clampConn
        /// 同义）；read_some hold 到期限后以 Conn(Timeout) 失败。`serve_report` 形态
        /// 的连接在首读即回一帧合法 REPORT（下行相位秒过），其后 hold。
        struct ClampConn {
            deadline: Mutex<Option<Instant>>,
            serve_report: bool,
            served: std::sync::atomic::AtomicBool,
        }
        impl SpeedConn for ClampConn {
            fn write_frame(&self, _data: &[u8]) -> Result<(), SpeedtestError> {
                Ok(()) // 请求/START/FINISH 全成功（写不设障）
            }
            fn read_some(&self) -> Result<Vec<u8>, SpeedtestError> {
                if self.serve_report
                    && !self.served.swap(true, std::sync::atomic::Ordering::AcqRel)
                {
                    let mut f = Frame::new();
                    return Ok(f
                        .control(TYPE_REPORT, b"{\"bytes\":0,\"warmup_bytes\":0,\"wall_ms\":1}")
                        .to_vec());
                }
                loop {
                    let dl = *self.deadline.lock().unwrap();
                    match dl {
                        Some(t) => {
                            let now = Instant::now();
                            if now >= t {
                                return Err(SpeedtestError::Conn(ConnErr::Timeout));
                            }
                            std::thread::sleep(((t - now) / 4).max(Duration::from_millis(5)));
                        }
                        None => std::thread::sleep(Duration::from_millis(5)),
                    }
                }
            }
            fn kill(&self) {}
            fn set_deadline(&self, d: Option<Duration>) {
                // 钳制：把 warmup+window+15s connBudget 收到 ≤300ms
                let clamped = d.map(|x| x.min(Duration::from_millis(300)));
                *self.deadline.lock().unwrap() = clamped.map(|x| Instant::now() + x);
            }
        }
        let p = Params {
            warmup: Duration::from_millis(50),
            down: Duration::from_millis(200),
            up: Duration::from_millis(200),
            streams: 1,
        }
        .normalized()
        .unwrap();

        // 形态一 hold：首帧前期限到点。
        let hold = std::sync::Arc::new(ClampConn {
            deadline: Mutex::new(None),
            serve_report: false,
            served: std::sync::atomic::AtomicBool::new(false),
        });
        let h2 = std::sync::Arc::clone(&hold);
        let r = run_dial(
            &|| Ok(h2.clone()),
            p,
            &|_| {},
            None,
            None,
        );
        match r {
            Err(SpeedtestError::Conn(ConnErr::Timeout)) => {}
            Err(SpeedtestError::NotSupported) => {
                panic!("期限到点冒充 not_supported（Go exec-r1 L4 同款 bug）")
            }
            other => panic!("形态一 hold：期望 Conn(Timeout)，实得 {other:?}"),
        }

        // 形态二 halfhold：下行正常收场（REPORT 即回），上行收口 report 缺席到点。
        // dial 计数：第 1 个 = 下行连接（serve_report），其后 = 上行连接（纯 hold）。
        let n = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let r2 = {
            let n = std::sync::Arc::clone(&n);
            run_dial(
                &move || {
                    let i = n.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
                    Ok(std::sync::Arc::new(ClampConn {
                        deadline: Mutex::new(None),
                        serve_report: i == 0,
                        served: std::sync::atomic::AtomicBool::new(false),
                    }))
                },
                p,
                &|_| {},
                None,
                None,
            )
        };
        match r2 {
            Err(SpeedtestError::Conn(ConnErr::Timeout)) => {}
            Err(SpeedtestError::NotSupported) => {
                panic!("收口期限到点冒充 not_supported（Go exec-r1 L4 同款 bug）")
            }
            other => panic!("形态二 halfhold：期望 Conn(Timeout)，实得 {other:?}"),
        }
    }

    /// 泵分片上报增量（复核 r3-F4 的回归钉）：累计面不重复累加。
    #[test]
    fn live_progress_delta_accounting() {
        let l = LiveProgress::new();
        l.set_phase(1);
        // 模拟 pump 的分片上报形状：每片加「本片增量」
        let mut total = 0i64;
        let mut reported = 0i64;
        for slice_bytes in [1000i64, 1000, 500, 700] {
            total += slice_bytes;
            l.add_bytes(total - reported);
            reported = total;
        }
        assert_eq!(
            l.snapshot(),
            (Some("down"), 3200),
            "增量上报 ⇒ bytes == total"
        );
    }

    /// LiveProgress（M-7）：相位切换 + 字节累计的原子面——Status 面的数据源。
    #[test]
    fn live_progress_phase_and_bytes() {
        let l = LiveProgress::new();
        assert_eq!(l.snapshot(), (None, 0));
        l.set_phase(1);
        l.add_bytes(100);
        l.add_bytes(23);
        assert_eq!(l.snapshot(), (Some("down"), 123));
        l.set_phase(2);
        l.add_bytes(1);
        assert_eq!(l.snapshot(), (Some("up"), 124));
        l.reset();
        assert_eq!(l.snapshot(), (None, 0));
    }

    #[test]
    fn report_parse_and_error_mapping() {
        let r = parse_report(br#"{"bytes":123,"warmup_bytes":45,"wall_ms":678}"#).unwrap();
        assert_eq!((r.bytes, r.warmup_bytes, r.wall_ms), (123, 45, 678));
        // 评审 r2-M8/M-9：busy/link_down 类型化（归因不再字符串嗅探）
        let r = parse_report(br#"{"bytes":0,"warmup_bytes":0,"wall_ms":0,"error":"busy"}"#);
        assert!(matches!(r, Err(SpeedtestError::Busy)), "busy 类型化：{r:?}");
        assert_eq!(r.unwrap_err().reason(), REASON_BUSY);
        let r = parse_report(br#"{"bytes":1,"warmup_bytes":0,"wall_ms":9,"error":"link_down"}"#);
        assert!(
            matches!(r, Err(SpeedtestError::LinkDown)),
            "link_down 类型化：{r:?}"
        );
        assert_eq!(r.unwrap_err().reason(), REASON_LINK_DOWN);
        // 取消归因可达（评审 r2-M8：REASON_CANCELLED 此前是死常量）
        assert_eq!(SpeedtestError::Cancelled.reason(), REASON_CANCELLED);
    }

    #[test]
    fn report_parse_quote_aware() {
        // 低-4：error 文案含 ASCII 逗号/引号/`}`/转义时不再错分（引号感知顶层切分）
        let r =
            parse_report(br#"{"bytes":7,"warmup_bytes":2,"wall_ms":33,"error":"a,b \"x\", c}d"}"#);
        assert!(
            matches!(&r, Err(SpeedtestError::Report(m)) if m == "a,b \"x\", c}d"),
            "含逗号/引号/大括号的 error 值必须整段保真，得 {r:?}"
        );
        // error 在前、数字在后的键序形态（切分不依赖键序）
        let r = parse_report(br#"{"error":"x,y","bytes":8,"warmup_bytes":1,"wall_ms":5}"#);
        assert!(matches!(&r, Err(SpeedtestError::Report(m)) if m == "x,y"));
        // 值内 `\,`（转义逗号）不切分
        let r = parse_report(br#"{"bytes":1,"warmup_bytes":0,"wall_ms":1,"error":"a\,b"}"#);
        assert!(matches!(&r, Err(SpeedtestError::Report(m)) if m == "a\\,b"));
    }

    #[test]
    fn split_top_level_edges() {
        assert_eq!(split_top_level(""), vec![""]);
        assert_eq!(split_top_level("a,b"), vec!["a", "b"]);
        assert_eq!(split_top_level(r#""a,b",c"#), vec![r#""a,b""#, "c"]);
        assert_eq!(split_top_level(r#""a\"b""#), vec![r#""a\"b""#]);
        // 不平衡引号 = 对抗输入：按扫描尾态整体返回（解析侧后续步骤自然报错/忽略）
        assert_eq!(split_top_level(r#""a,b"#), vec![r#""a,b"#]);
    }
}
