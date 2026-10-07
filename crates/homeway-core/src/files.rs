//! files 原生协议客户端（语义真源 `baseline:pkg/files/{proto,client}.go`）。
//!
//! 承载：隧道内 TCP 流上**每命令一条流**（拨隧道 IP:7802，出口豁免规则转投本机
//! files.sock）。线上格式：
//!
//! ```text
//! 服务端 → 客户端（流的第一个东西，恒有）：
//!   {"ok":true,"root":"/Users/xx","ver":1}\n               ← 问候帧
//! 客户端 → 服务端（一行 JSON 请求，≤64KB）：
//!   {"op":"list","path":"photos"}\n
//! 服务端 → 客户端（一行 JSON 响应）：
//!   {"ok":true,"entries":[…}\n  或  {"ok":false,"code":"not_found","msg":"…"}\n
//! 流式（大文件）用 [4B BE len][payload] 帧，len=0 是终止帧：
//!   download：响应行给出 size → 若干帧 → 终止帧；
//!   write（上传）：请求行 → {"ok":true} → 帧 → 终止帧 = 提交；提前关流 = 取消
//!   （服务端删 .tierpart，目标不变）。
//! ```
//!
//! 动词六枚：list / stat / mkdir / read / download / write。错误码词表 9 枚
//! （7 稳定码 + canceled + server_busy）+ stream_open（流开场失败——**唯一可安全
//! 重放的阶段**：请求未送达）。

use std::io;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::session::Session;
use crate::wgcore::ConnErr;

/// files 服务端口（隧道 IP 上；出口按 LocalServices 转投 files.sock）。
pub const FILES_PORT: u16 = 7802;
/// 请求行上限（一行 JSON）。
pub const MAX_REQUEST_LINE: usize = 64 * 1024;
/// 单帧载荷上限（客户端应遵守）。
/// 流式帧载荷上限（pub = fuzz 断言可达面）。
pub const MAX_CHUNK: usize = 256 * 1024;
/// 协议版本（问候帧 ver）。
pub const VERSION: i32 = 1;

// ---------- 错误码词表（contract-ledger 族④；ArkTS filesErrorMessage 表同源） ----------

pub const CODE_INVALID_ARG: &str = "invalid_arg";
pub const CODE_INVALID_NAME: &str = "invalid_name";
pub const CODE_NOT_FOUND: &str = "not_found";
pub const CODE_PERMISSION: &str = "permission";
pub const CODE_IS_DIR: &str = "is_dir";
pub const CODE_ALREADY_EXISTS: &str = "already_exists";
pub const CODE_OP_FAILED: &str = "op_failed";
pub const CODE_CANCELED: &str = "canceled";
pub const CODE_SERVER_BUSY: &str = "server_busy";
/// 流开场失败（拨号之后读问候那一段；Go CodeStreamOpen）。
pub const CODE_STREAM_OPEN: &str = "stream_open";

/// files 错误（带稳定错误码 + 阶段信息）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FilesError {
    /// 服务端拒绝/操作失败（code = 稳定码词表）。
    #[error("{code}: {msg}")]
    Code { code: String, msg: String },
    /// 传输/协议层失败（连接、帧、JSON）。
    #[error("files 传输失败：{0}")]
    Transport(String),
    /// 连接建立失败（healing dial 用尽预算）。
    #[error(transparent)]
    Conn(#[from] ConnErr),
}

impl FilesError {
    /// 稳定错误码（App 按码分派；`stream_open` = 请求未送达、可安全重放）。
    pub fn code(&self) -> &str {
        match self {
            FilesError::Code { code, .. } => code,
            FilesError::Transport(_) => CODE_OP_FAILED,
            FilesError::Conn(_) => CODE_OP_FAILED,
        }
    }
}

fn transport<E: std::fmt::Display>(e: E) -> FilesError {
    FilesError::Transport(e.to_string())
}

// ---------- 线上结构（字段序 = Go 声明序；omitempty 对齐） ----------

#[derive(Debug, Serialize)]
struct Request<'a> {
    op: &'a str,
    path: &'a str,
    #[serde(skip_serializing_if = "opty_is_zero")]
    #[serde(rename = "maxBytes")]
    max_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
    #[serde(skip_serializing_if = "opty_is_zero")]
    size: i64,
}

fn opty_is_zero(v: &i64) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Entry {
    pub name: String,
    #[serde(rename = "isDir")]
    pub is_dir: bool,
    pub size: i64,
    #[serde(rename = "mtimeMs")]
    pub mtime_ms: i64,
    #[serde(default)]
    pub mode: u32,
}

#[derive(Debug, Deserialize)]
struct Greeting {
    ok: bool,
    #[serde(default)]
    root: String,
    ver: i64,
}

#[derive(Debug, Deserialize, Default)]
struct Response {
    ok: bool,
    #[serde(default)]
    code: String,
    #[serde(default)]
    msg: String,
    #[serde(default)]
    entries: Vec<Entry>,
    entry: Option<Entry>,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    truncated: bool,
    #[serde(default)]
    text: String,
    #[serde(default)]
    base64: String,
}

// ---------- 流（每命令一条；引擎 RPC 之上的行/帧读写） ----------

/// 一条已建立（问候帧已读）的命令流。
///
/// **读线程解耦**：引擎读由独立线程持续推进（channel 交付）——帧消费侧在帧边界
/// 暂停（写盘等）不再暂停隧道读。暂停会让出口侧 gVisor 写阻塞升级为连接拆毁
/// （实测：帧边界暂停的下载在第一帧后即被 FIN；连续读形态全量收满——Go 客户端
/// 的 net.Conn 读由运行时推进，同模型）。
pub struct Stream<'a> {
    io: StreamIo<'a>,
    buf: Vec<u8>,
    /// 读线程交付通道（线程在 EOF/错误/通道断开时退出）。
    rx: std::sync::mpsc::Receiver<Result<Vec<u8>, ConnErr>>,
    /// 问候帧带来的根与版本（诊断/展示用）。
    pub root: String,
    pub ver: i64,
}

/// 一条命令流的承载（Go pkg/files 的 net.Conn 注入缝——本地 = Session 隧道拨号；
/// 远程 = daemon 控制面 stream.open{kind:files} 的透传腿，files 协议端到端原样承载）。
enum StreamIo<'a> {
    Local { sess: &'a Session, id: u64 },
    Remote {
        client: std::sync::Arc<crate::daemon::client::ControlClient>,
        st: std::sync::Arc<crate::daemon::client::ClientStream>,
    },
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        // 关流即语义：上传未提交（未发终止帧）= 取消（服务端删 .tierpart）。
        // 远程形态的收口由调用方关 ControlClient 承载（整连接关 = 流终结）。
        if let StreamIo::Local { sess, id } = &self.io {
            // 测试构造（id=u64::MAX）跳过——dangling Session 不可解引用。
            if *id != u64::MAX {
                let _ = sess.client().close(*id);
            }
        }
    }
}

impl<'a> Stream<'a> {
    /// 测试构造：受控读通道驱动（不拨号；close 走真实路径——Session 由 PhantomData
    /// 借用形参规避）。
    #[cfg(test)]
    fn from_rx(rx: std::sync::mpsc::Receiver<Result<Vec<u8>, ConnErr>>) -> Stream<'static> {
        Stream {
            io: StreamIo::Local {
                sess: unsafe { &*(std::ptr::NonNull::<Session>::dangling().as_ptr()) },
                id: u64::MAX,
            },
            buf: Vec::new(),
            rx,
            root: String::new(),
            ver: 0,
        }
    }

    /// 起一条流并读问候帧（`stream_open` 只在这一段——此后请求可能已送达，
    /// 重放有重复副作用风险）。拨号走 healing（4s 首试 → 阶梯 → 重试）。
    pub fn open(sess: &'a Session, budget: Duration) -> Result<Stream<'a>, FilesError> {
        let id = sess.healing_dial_port(FILES_PORT, budget)?;
        let client = sess.client();
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, ConnErr>>();
        std::thread::Builder::new()
            .name("homeway-files-rd".into())
            .spawn(move || {
                loop {
                    match client.read(id) {
                        Ok(chunk) if !chunk.is_empty() => {
                            if tx.send(Ok(chunk)).is_err() {
                                break; // 消费侧 drop
                            }
                        }
                        r => {
                            // EOF/错误：交付一次后退出（评审中-7——连接已被引擎回收，
                            // 继续循环只会紧转 + 无界通道堆积）
                            let _ = tx.send(r);
                            break;
                        }
                    }
                }
            })
            .ok();
        // 问候帧看门（评审中-7 → D-2 收口）：首响应段带预算（与拨号同一段预算——
        // Go files-cli「连接+首响应两段各 --timeout」同义）；到点关流打断挂死的读。
        Stream::handshake(
            Stream {
                io: StreamIo::Local { sess, id },
                buf: Vec::with_capacity(16 * 1024),
                rx,
                root: String::new(),
                ver: 0,
            },
            budget,
        )
    }

    /// 远程形态：已打开的控制面流腿上读问候帧（`--host` 模式——files 协议经
    /// stream.open 纯透传，零 wire 改动）。流终结三态折 `stream_open`（Go
    /// filesStreamOpenErr 的 bad_request 代际文案由 CLI 侧翻译，本层给中性归因）。
    pub fn open_remote(
        client: std::sync::Arc<crate::daemon::client::ControlClient>,
        st: std::sync::Arc<crate::daemon::client::ClientStream>,
        greet_budget: Duration,
    ) -> Result<Stream<'static>, FilesError> {
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, ConnErr>>();
        let rd_st = std::sync::Arc::clone(&st);
        std::thread::Builder::new()
            .name("homeway-files-rr".into())
            .spawn(move || {
                loop {
                    match rd_st.recv_wait() {
                        Some(chunk) => {
                            if tx.send(Ok(chunk)).is_err() {
                                break; // 消费侧 drop
                            }
                        }
                        None => {
                            // 流终结（closed|gone|conn）——映射引擎 Closed 面（EOF 语义）。
                            let _ = tx.send(Err(ConnErr::Closed));
                            break;
                        }
                    }
                }
            })
            .ok();
        // 问候帧看门（本地/远程一次收两面——D-1 评审中-7 的 D-2 收口项）：远程腿
        // 挂死（出口 files.sock 无应答/腿停滞）不再永久挂 CLI。
        Stream::handshake(
            Stream {
                io: StreamIo::Remote { client, st },
                buf: Vec::with_capacity(16 * 1024),
                rx,
                root: String::new(),
                ver: 0,
            },
            greet_budget,
        )
    }

    /// 问候帧公共段（两种承载共用）。首响应带预算（Go armWatchdog 同义：**绝对
    /// 期限**（dial 返回起一次性），到点关流打断阻塞中的问候帧读——对端不应答时
    /// 不会永久挂死；问候帧返回后不再受此预算约束，传输期无期限——传输中的流
    /// 不会被首响应预算误杀）。
    fn handshake(mut s: Stream<'_>, budget: Duration) -> Result<Stream<'_>, FilesError> {
        let deadline = Instant::now() + budget;
        let line = s.read_line_deadline(deadline).map_err(|e| FilesError::Code {
            code: CODE_STREAM_OPEN.to_owned(),
            msg: format!("读问候帧失败：{e}"),
        })?;
        let g: Greeting = serde_json::from_slice(&line)
            .map_err(|e| FilesError::Code { code: CODE_STREAM_OPEN.to_owned(), msg: format!("问候帧不是 JSON：{e}") })?;
        if !g.ok {
            return Err(FilesError::Code { code: CODE_STREAM_OPEN.to_owned(), msg: "问候帧失败".into() });
        }
        s.root = g.root;
        s.ver = g.ver;
        Ok(s)
    }

    /// 从读线程取一块（EOF/错误 = Err；空块视作 EOF）。
    fn next_chunk(&mut self) -> Result<Vec<u8>, FilesError> {
        match self.rx.recv() {
            Ok(Ok(chunk)) if !chunk.is_empty() => Ok(chunk),
            Ok(_) => Err(transport("流已到尾（EOF）")),
            Err(_) => Err(transport("读线程已退出")),
        }
    }

    /// 读一行（去 EOL；**无上限——响应面**：客户端只读响应行/问候帧，请求行上限
    /// 是服务端的事（见 `files_server::read_line`）；口径与 `facade/files_op.rs`
    /// 的 `read_line_capped(false)` 一致——服务端内联上限 16MB 与 list 大目录 JSON
    /// 都可超 64KB，卡上限会把合法响应判死。无期限——传输期）。
    fn read_line(&mut self) -> Result<Vec<u8>, FilesError> {
        self.read_line_opt(None)
    }

    /// 读一行（带首响应**绝对期限**的问候帧形态：到点关流 + 超预算归因）。
    fn read_line_deadline(&mut self, deadline: Instant) -> Result<Vec<u8>, FilesError> {
        self.read_line_opt(Some(deadline))
    }

    fn read_line_opt(&mut self, deadline: Option<Instant>) -> Result<Vec<u8>, FilesError> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                    line.pop();
                }
                return Ok(line);
            }
            let chunk = match deadline {
                Some(d) => self.next_chunk_timeout(d)?,
                None => self.next_chunk()?,
            };
            self.buf.extend_from_slice(&chunk);
        }
    }

    /// 带期限取一块（问候帧看门面；**绝对期限**——每轮按剩余时间收窄，评审 6.1：
    /// 按块重置的滑动期限会被「滴灌对端」拖成 块数×预算，Go armWatchdog 是 dial
    /// 返回起一次性定时器）：到点**关流**（本地 = 引擎关连接——读线程的阻塞 read
    /// 随连接回收退出；远程 = stream_close——腿终结）再报超预算——不关流的话读
    /// 线程与消费侧通道都悬着（drop 时 Local 走 close 幂等无害）。
    fn next_chunk_timeout(&mut self, deadline: Instant) -> Result<Vec<u8>, FilesError> {
        let remain = deadline.saturating_duration_since(Instant::now());
        if remain.is_zero() {
            self.abort_stream();
            return Err(transport("首响应（问候帧）超预算"));
        }
        match self.rx.recv_timeout(remain) {
            Ok(Ok(chunk)) if !chunk.is_empty() => Ok(chunk),
            Ok(_) => Err(transport("流已到尾（EOF）")),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                self.abort_stream();
                Err(transport("首响应（问候帧）超预算".to_owned()))
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err(transport("读线程已退出"))
            }
        }
    }

    /// 看门到点的关流动作（两面统一；幂等——drop 的收口路径重复关无害）。
    fn abort_stream(&self) {
        match &self.io {
            StreamIo::Local { sess, id } => {
                if *id != u64::MAX {
                    let _ = sess.client().close(*id);
                }
            }
            StreamIo::Remote { client, st } => {
                let _ = client.stream_close(st, Duration::from_secs(3));
            }
        }
    }

    /// 写全部（本地：部分写 + **Ok(0)=发送缓冲满的背压语义**——立即重试，RPC 往返
    /// 驱动 ACK 排空，百万次零进展才判死；远程：整帧 stream_send，流终结三态折
    /// 传输错误）。
    fn write_all(&mut self, data: &[u8]) -> Result<(), FilesError> {
        match &self.io {
            StreamIo::Local { sess, id } => {
                let client = sess.client();
                let mut off = 0;
                let mut zero_streak = 0u32;
                // R8-3 F12：零接纳时引擎带回原 Vec——重试环不重拷。
                let mut pending: Option<Vec<u8>> = None;
                while off < data.len() {
                    let chunk = match pending.take() {
                        Some(v) => v,
                        None => data[off..].to_vec(),
                    };
                    let w = client.write(*id, chunk).map_err(transport)?;
                    if w.n == 0 {
                        pending = w.back;
                        zero_streak += 1;
                        if zero_streak > 1_000_000 {
                            return Err(transport("写通道长时间无进展"));
                        }
                        std::thread::yield_now();
                        continue;
                    }
                    zero_streak = 0;
                    off += w.n;
                }
                Ok(())
            }
            StreamIo::Remote { client, st } => client
                .stream_send(st, data)
                .map_err(|e| transport(e.to_string())),
        }
    }

    /// 发一条命令并读响应；ok=false ⇒ `FilesError::Code`（缺 code 归 op_failed）。
    fn call(&mut self, req: &Request) -> Result<Response, FilesError> {
        let mut line = serde_json::to_vec(req).map_err(transport)?;
        line.push(b'\n');
        self.write_all(&line)?;
        let resp_line = self.read_line()?;
        let mut resp: Response = serde_json::from_slice(&resp_line).map_err(|e| transport(format!("响应不是 JSON：{e}")))?;
        if !resp.ok {
            let code = if resp.code.is_empty() { CODE_OP_FAILED.to_owned() } else { std::mem::take(&mut resp.code) };
            return Err(FilesError::Code { code, msg: resp.msg });
        }
        Ok(resp)
    }

    /// 读一个数据帧：`Ok(Some(payload))`；终止帧 = `Ok(None)`。
    fn read_frame(&mut self) -> Result<Option<Vec<u8>>, FilesError> {
        while self.buf.len() < 4 {
            let chunk = self.next_chunk()?;
            self.buf.extend_from_slice(&chunk);
        }
        let pre = decode_prefix(&self.buf)?;
        let n = match pre {
            Prefix::Terminated => {
                self.buf.drain(..4);
                return Ok(None);
            }
            Prefix::Frame { len } => len,
        };
        while self.buf.len() < 4 + n {
            let chunk = self.next_chunk()?;
            self.buf.extend_from_slice(&chunk);
        }
        let payload = self.buf[4..4 + n].to_vec();
        self.buf.drain(..4 + n);
        Ok(Some(payload))
    }

    /// 写一个数据帧（len=0 = 终止/提交帧）。
    fn write_frame(&mut self, payload: &[u8]) -> Result<(), FilesError> {
        let mut frame = Vec::with_capacity(4 + payload.len());
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        self.write_all(&frame)
    }
}

/// 4B 前缀帧的纯解析产物（无 IO；fuzz/测试可达面，IO 留在 Stream::read_frame）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Prefix {
    /// len=0 终止帧。
    Terminated,
    /// 数据帧的载荷长度（到齐判定 = `buf.len() >= 4 + len`）。
    Frame { len: usize },
}

/// 解析 4B BE 前缀：`<4B` 报错（调用方补读）；超 `MAX_CHUNK` 报错（Go files 协议
/// 同上限——对端异常长度的防御面）。
pub fn decode_prefix(buf: &[u8]) -> Result<Prefix, FilesError> {
    if buf.len() < 4 {
        return Err(transport("前缀未到齐（<4B）"));
    }
    let n = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if n == 0 {
        return Ok(Prefix::Terminated);
    }
    if n > MAX_CHUNK {
        return Err(transport(format!("帧长 {n} 超过上限 {MAX_CHUNK}")));
    }
    Ok(Prefix::Frame { len: n })
}

// ---------- 动词（每命令一条流；承载 = 本地 Session 拨号或远程控制面流腿） ----------

/// 列目录。
pub fn list(sess: &Session, budget: Duration, path: &str) -> Result<Vec<Entry>, FilesError> {
    let mut s = Stream::open(sess, budget)?;
    list_at(&mut s, path)
}

/// 单条目信息。
pub fn stat(sess: &Session, budget: Duration, path: &str) -> Result<Entry, FilesError> {
    let mut s = Stream::open(sess, budget)?;
    stat_at(&mut s, path)
}

/// 新建目录。
pub fn mkdir(sess: &Session, budget: Duration, path: &str) -> Result<(), FilesError> {
    let mut s = Stream::open(sess, budget)?;
    mkdir_at(&mut s, path)
}

// ——`--host` 远程形态（Go internal/daemon/files_remote.go 的 DialFiles 缝：控制面
// stream.open{kind:files} 透传腿上跑同一套动词，零 wire 改动）——

pub fn list_remote(
    client: std::sync::Arc<crate::daemon::client::ControlClient>,
    st: std::sync::Arc<crate::daemon::client::ClientStream>,
    greet_budget: Duration,
    path: &str,
) -> Result<Vec<Entry>, FilesError> {
    let mut s = Stream::open_remote(client, st, greet_budget)?;
    list_at(&mut s, path)
}

pub fn stat_remote(
    client: std::sync::Arc<crate::daemon::client::ControlClient>,
    st: std::sync::Arc<crate::daemon::client::ClientStream>,
    greet_budget: Duration,
    path: &str,
) -> Result<Entry, FilesError> {
    let mut s = Stream::open_remote(client, st, greet_budget)?;
    stat_at(&mut s, path)
}

pub fn mkdir_remote(
    client: std::sync::Arc<crate::daemon::client::ControlClient>,
    st: std::sync::Arc<crate::daemon::client::ClientStream>,
    greet_budget: Duration,
    path: &str,
) -> Result<(), FilesError> {
    let mut s = Stream::open_remote(client, st, greet_budget)?;
    mkdir_at(&mut s, path)
}

pub fn read_remote(
    client: std::sync::Arc<crate::daemon::client::ControlClient>,
    st: std::sync::Arc<crate::daemon::client::ClientStream>,
    greet_budget: Duration,
    path: &str,
    mode: &str,
    max_bytes: i64,
) -> Result<ReadResult, FilesError> {
    let mut s = Stream::open_remote(client, st, greet_budget)?;
    let resp = s.call(&Request { op: "read", path, max_bytes, mode: Some(mode), size: 0 })?;
    Ok(ReadResult { text: resp.text, base64: resp.base64, truncated: resp.truncated })
}

pub fn download_remote<W, F>(
    client: std::sync::Arc<crate::daemon::client::ControlClient>,
    st: std::sync::Arc<crate::daemon::client::ClientStream>,
    greet_budget: Duration,
    path: &str,
    w: &mut W,
    on_size: F,
) -> Result<u64, FilesError>
where
    W: io::Write + ?Sized,
    F: FnOnce(i64),
{
    let mut s = Stream::open_remote(client, st, greet_budget)?;
    download_at(&mut s, path, w, on_size)
}

// 8 参 = client/st/greet_budget 三承载参 + 协议四参 + 限速器（Go 同族函数的
// 参数面；承载三参已在 6 个远程动词间同构——收敛为结构体的收益不抵本批侵入面）。
#[allow(clippy::too_many_arguments)]
pub fn upload_remote<R, F>(
    client: std::sync::Arc<crate::daemon::client::ControlClient>,
    st: std::sync::Arc<crate::daemon::client::ClientStream>,
    greet_budget: Duration,
    path: &str,
    r: &mut R,
    size: i64,
    on_progress: F,
    limiter: Option<&mut UploadLimiter>,
) -> Result<u64, FilesError>
where
    R: io::Read + ?Sized,
    F: FnMut(u64),
{
    let mut s = Stream::open_remote(client, st, greet_budget)?;
    upload_at(&mut s, path, r, size, on_progress, limiter)
}

// ——已建立流上的动词主体（本地/远程共用）——

pub(crate) fn list_at(s: &mut Stream<'_>, path: &str) -> Result<Vec<Entry>, FilesError> {
    let resp = s.call(&Request { op: "list", path, max_bytes: 0, mode: None, size: 0 })?;
    Ok(resp.entries)
}

pub(crate) fn stat_at(s: &mut Stream<'_>, path: &str) -> Result<Entry, FilesError> {
    let resp = s.call(&Request { op: "stat", path, max_bytes: 0, mode: None, size: 0 })?;
    resp.entry.ok_or_else(|| FilesError::Code { code: CODE_OP_FAILED.into(), msg: "响应缺 entry".into() })
}

pub(crate) fn mkdir_at(s: &mut Stream<'_>, path: &str) -> Result<(), FilesError> {
    s.call(&Request { op: "mkdir", path, max_bytes: 0, mode: None, size: 0 })?;
    Ok(())
}

/// 内联读取（mode="" 文本、mode="image" 回 base64；`truncated` = 服务端按 maxBytes
/// 截断的标记——Go Response.Truncated 透传，App 侧据此提示）。
pub struct ReadResult {
    pub text: String,
    pub base64: String,
    pub truncated: bool,
}

pub fn read(sess: &Session, budget: Duration, path: &str, mode: &str, max_bytes: i64) -> Result<ReadResult, FilesError> {
    let mut s = Stream::open(sess, budget)?;
    let resp = s.call(&Request { op: "read", path, max_bytes, mode: Some(mode), size: 0 })?;
    Ok(ReadResult { text: resp.text, base64: resp.base64, truncated: resp.truncated })
}

pub(crate) fn download_at<W, F>(
    s: &mut Stream<'_>,
    path: &str,
    w: &mut W,
    on_size: F,
) -> Result<u64, FilesError>
where
    W: io::Write + ?Sized,
    F: FnOnce(i64),
{
    let resp = s.call(&Request { op: "download", path, max_bytes: 0, mode: None, size: 0 })?;
    on_size(resp.size);
    let mut total: u64 = 0;
    loop {
        match s.read_frame()? {
            Some(payload) => {
                w.write_all(&payload).map_err(|e| FilesError::Code {
                    code: CODE_OP_FAILED.into(),
                    msg: format!("写本地失败：{e}"),
                })?;
                total += payload.len() as u64;
            }
            None => {
                if resp.size > 0 && (total as i64) < resp.size {
                    return Err(FilesError::Code {
                        code: CODE_OP_FAILED.into(),
                        msg: format!("下载不完整：服务端声明 {} 字节，实收 {total}", resp.size),
                    });
                }
                return Ok(total);
            }
        }
    }
}

pub(crate) fn upload_at<R, F>(
    s: &mut Stream<'_>,
    path: &str,
    r: &mut R,
    size: i64,
    mut on_progress: F,
    mut limiter: Option<&mut UploadLimiter>,
) -> Result<u64, FilesError>
where
    R: io::Read + ?Sized,
    F: FnMut(u64),
{
    s.call(&Request { op: "write", path, max_bytes: 0, mode: None, size })?;
    let block = limiter.as_ref().map_or(MAX_CHUNK, |l| l.block_hint());
    let mut buf = vec![0u8; block];
    let mut total: u64 = 0;
    loop {
        let n = r.read(&mut buf).map_err(|e| FilesError::Code {
            code: CODE_OP_FAILED.into(),
            msg: format!("读本地失败：{e}"),
        })?;
        if n == 0 {
            break;
        }
        if let Some(l) = limiter.as_deref_mut() {
            l.await_quota(n);
        }
        s.write_frame(&buf[..n])?;
        total += n as u64;
        on_progress(total);
    }
    s.write_frame(&[])?; // 终止帧 = 提交
    let line = s.read_line()?;
    let resp: Response = serde_json::from_slice(&line).map_err(|e| transport(format!("提交结果不是 JSON：{e}")))?;
    if !resp.ok {
        return Err(FilesError::Code { code: resp.code, msg: resp.msg });
    }
    Ok(total)
}

/// 大文件下载：载荷帧原样写进 `w`，返回总字节数。`on_size` 收到响应行声明的
/// 大小（进度分母）。**断读收口**（FIX-40）：终止帧到达时对照声明——少收一律
/// 报错（提前终止被静默当成功会落半截文件）；多收容忍（下载生长中的文件合法）。
pub fn download<W, F>(sess: &Session, budget: Duration, path: &str, w: &mut W, on_size: F) -> Result<u64, FilesError>
where
    W: io::Write + ?Sized,
    F: FnOnce(i64),
{
    let mut s = Stream::open(sess, budget)?;
    download_at(&mut s, path, w, on_size)
}

/// 上行限速缺省（bytes/s；Go files-cli 1.4「发送端速率义务」同值：保守起步 2MiB/s，
/// design D5「不可判定时取保守值」）。0 = 不限、风险自担（越界被对端收流属可预期
/// 边界）。
pub const DEFAULT_RATE_LIMIT: i64 = 2 << 20;

/// 令牌桶补充上限（停顿后削峰）：独立于 rate 的常量，取对端每流窗口
/// （640KiB = 40 帧 × 16KiB）的一半以下——「任意时刻可立即灌入的增量 ≤ burstCap」
/// 恒真，稳态吞吐不受影响（稳态 tokens≈0，只在读发停顿后削峰）。Go tokenBurstCap
/// 同值同理由。
const TOKEN_BURST_CAP: i64 = 256 << 10;

/// 发送端速率义务的载体：**无 ack/credit 下的盲节流**（协议 write 方向无回压信号；
/// 对端每流窗口有限，越界即时收流）。安全条件 = 发送速率 ≤ 隧道+远端的排空速率；
/// 固定默认值必须取最慢预期腿之下（快腿绿不能当安全速率证据）。
///
/// 与 Go tokenBucket 逐语义对齐（files-cli 1.4 + exec-r1 F2/v0.12.1 空桶起步 +
/// exec-r2 N1 整块放行）：
///   - **空桶起步**（tokens = 0）：满桶起步的首秒突刺远超对端窗口（真机实测
///     100MiB put 对慢腿 0.04s 内 gone）；首帧只多等 MAX_CHUNK/rate（默认 ~128ms）；
///   - 补充上限 = min(rate, 256KiB)——小速率下不超过 rate 本身（否则节拍失真）；
///   - 单块配额 > burst（0 < rate < 块大小）时**不走高水位攒额**：按 n/rate 等满
///     整块配额后清零放行（否则 tokens 恒被截在 burst 以下、永攒不够一块）。
///
/// sleep 按 ≤250ms 分片（小速率下单块等待可达秒级——分片让进程收尾/信号响应
/// 不被单次长睡拖住；Go 侧走 ctx 取消，本侧无 ctx 面，进程信号即取消）。
pub struct UploadLimiter {
    rate: i64,
    burst: i64,
    tokens: f64,
    /// 上次结算时刻（**字段而非调用局部**——跨调用计息：写 socket/读本地的耗时
    /// 都在攒令牌，稳态严格 ≈ rate；Go tokenBucket.last 同义）。
    last: std::time::Instant,
}

impl UploadLimiter {
    /// rate ≤ 0 = 不限（调用方以 None 语义处理）。tokens 空桶起步。
    pub fn new(rate: i64) -> Option<Self> {
        if rate <= 0 {
            return None;
        }
        Some(Self { rate, burst: rate.min(TOKEN_BURST_CAP), tokens: 0.0, last: std::time::Instant::now() })
    }

    /// 一次配额结算的纯计算（测试可达面）：给定距上次结算的秒数与本次需发字节数，
    /// 返回（结算后的 tokens 余量, 需等待的秒数）。不改时钟状态。
    fn settle(elapsed_s: f64, tokens: f64, rate: i64, burst: i64, n: usize) -> (f64, f64) {
        let mut t = tokens + elapsed_s * rate as f64;
        let n = n as f64;
        if n > burst as f64 {
            // 整块配额放行 = 积累作废（等待时间已花在上一块上，不得凭积累立刻放行
            // 整块；burst 仍约束可立即灌入的增量）
            return (0.0, n / rate as f64);
        }
        if t > burst as f64 {
            t = burst as f64; // 停顿后削峰
        }
        if n <= t {
            (t - n, 0.0)
        } else {
            (t, (n - t) / rate as f64)
        }
    }

    /// 为 n 字节的发送配额等待（上传循环按本地读块调用，配额单位 = 读块
    /// （≤ MAX_CHUNK），不是 16KiB 线帧）。**整块配额 > burst（0 < rate < 块大小）
    /// 时睡 n/rate 一次即返**（Go sleepCtx 单次语义——放进循环会因该分支恒成立
    /// 而永不返回，第二道门 高-1 钉死的形态）；普通差额睡完回环重结算（睡眠期
    /// 计息）。
    pub fn await_quota(&mut self, n: usize) {
        if n as f64 > self.burst as f64 {
            self.tokens = 0.0; // 积累作废；last 不动（Go 同形：睡眠期下次调用计息）
            sleep_sliced(n as f64 / self.rate as f64);
            return;
        }
        loop {
            let now = std::time::Instant::now();
            let (tokens, wait_s) = Self::settle(
                now.duration_since(self.last).as_secs_f64(),
                self.tokens,
                self.rate,
                self.burst,
                n,
            );
            self.last = now;
            self.tokens = tokens;
            if wait_s <= 0.0 {
                return;
            }
            sleep_sliced(wait_s);
        }
    }
}

impl UploadLimiter {
    /// 读块粒度建议（发送平滑用）：`rate/8` 钳在 [16KiB, MAX_CHUNK]——缺省 2MiB/s
    /// 恰为 MAX_CHUNK（Go 同读块形态，零行为差异）；低速率下自动细化为包级平滑
    /// （R5 终验轮 3' 实证：250KB/s × 256KiB 整块放行在 200pps 准入闸同机拓扑下
    /// 瞬时突发贴闸——每秒 8 个小块把突发压到 ~13 包/次）。
    pub fn block_hint(&self) -> usize {
        (self.rate as usize / 8).clamp(16 << 10, MAX_CHUNK)
    }
}

/// ≤250ms 分片的 sleep（见 UploadLimiter 文档）。
fn sleep_sliced(mut secs: f64) {
    while secs > 0.0 {
        let slice = secs.min(0.25);
        std::thread::sleep(std::time::Duration::from_secs_f64(slice));
        secs -= slice;
    }
}

/// 大文件上传：请求 → 服务端 ready → 帧 → 终止帧（提交）→ 读提交结果。
/// **错误/提前 drop = 关流不发终止帧 ⇒ 服务端删 .tierpart**（目标不变）。
/// `limiter` = 发送端速率义务（None = 不限；CLI 缺省 2MiB/s——对齐 Go files-cli）。
pub fn upload<R, F>(sess: &Session, budget: Duration, path: &str, r: &mut R, size: i64, on_progress: F, limiter: Option<&mut UploadLimiter>) -> Result<u64, FilesError>
where
    R: io::Read + ?Sized,
    F: FnMut(u64),
{
    let mut s = Stream::open(sess, budget)?;
    upload_at(&mut s, path, r, size, on_progress, limiter)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 合成流：问候+响应+3 帧（256KB 全帧）+终止帧，按任意切块喂给**真 Stream**——
    /// 真实下载曾在帧边界出现 4B 前缀混入载荷的事故（字节对账抓出）。
    #[test]
    fn frame_parse_boundaries_with_arbitrary_chunking() {
        let payload1: Vec<u8> = (0..262144u32).map(|i| (i % 251) as u8).collect();
        let payload2: Vec<u8> = (0..262144u32).map(|i| ((i * 7) % 253) as u8).collect();
        let payload3: Vec<u8> = vec![0xAB; 262132];
        let mut stream: Vec<u8> = Vec::new();
        stream.extend_from_slice(b"{\"ok\":true,\"root\":\"/Users/zhaozhe\",\"ver\":1}\n");
        stream.extend_from_slice(b"{\"ok\":true,\"size\":786420}\n");
        for p in [&payload1, &payload2, &payload3] {
            stream.extend_from_slice(&(p.len() as u32).to_be_bytes());
            stream.extend_from_slice(p);
        }
        stream.extend_from_slice(&0u32.to_be_bytes());
        for chunk_size in [1usize, 7, 64, 1024, 65536] {
            let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, ConnErr>>();
            let chunks: Vec<Vec<u8>> = stream.chunks(chunk_size).map(|c| c.to_vec()).collect();
            std::thread::spawn(move || {
                for c in chunks {
                    if tx.send(Ok(c)).is_err() {
                        break;
                    }
                }
            });
            let mut s = Stream::from_rx(rx);
            let g = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _greeting = s.read_line().expect("问候行");
                let _resp = s.read_line().expect("响应行");
                assert_eq!(s.read_frame().expect("帧1").unwrap(), payload1, "chunk={chunk_size}");
                assert_eq!(s.read_frame().expect("帧2").unwrap(), payload2, "chunk={chunk_size}");
                assert_eq!(s.read_frame().expect("帧3").unwrap(), payload3, "chunk={chunk_size}");
                assert!(s.read_frame().expect("终止帧").is_none(), "chunk={chunk_size}");
            }));
            std::mem::forget(s); // 测试构造不 drop
            if let Err(e) = g {
                std::panic::resume_unwind(e);
            }
        }
    }

    /// F2：客户端**响应行不设上限**（对齐 `facade/files_op.rs` 的 `read_line_capped(false)`；
    /// 原 64KB 门是服务端请求行上限的**误移植**）。200KB 的 list 响应行 + 12MB 的 text
    /// 响应行都必须读通——修前第二条必报「行超过 64KB 上限」。
    /// 注：直接打 `read_line`（= `call()` 的读侧；写请求侧不涉及本上限，且测试构造的
    /// dangling Session 不可走写路径）。
    #[test]
    fn response_line_over_64k_accepted() {
        let big = format!(
            r#"{{"ok":true,"entries":[{}]}}"#,
            (0..2000)
                .map(|i| format!(
                    r#"{{"name":"file-{i:04}-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx","isDir":false,"size":1,"mtimeMs":1}}"#
                ))
                .collect::<Vec<_>>()
                .join(",")
        );
        assert!(big.len() > 3 * 64 * 1024, "构造 64KB 门 3 倍以上的响应行（实 {}B）", big.len());
        let text_line = format!(r#"{{"ok":true,"text":"{}"}}"#, "a".repeat(12 << 20));
        for line in [big, text_line] {
            let mut stream = line.into_bytes();
            stream.push(b'\n');
            let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, ConnErr>>();
            std::thread::spawn(move || {
                let _ = tx.send(Ok(stream));
            });
            let mut s = Stream::from_rx(rx);
            let got = s.read_line().expect(">64KB 响应行必须读通（F2 去掉误移植的 64KB 门）");
            assert!(got.len() > 64 * 1024, "实收 {}B", got.len());
            std::mem::forget(s); // 测试构造不 drop
        }
    }

    /// 请求行序列化：字段序 + omitempty（对拍 Go json.Marshal(Request)）。
    #[test]
    fn request_line_shape() {
        let r = Request { op: "list", path: "photos", max_bytes: 0, mode: None, size: 0 };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"op":"list","path":"photos"}"#);
        let r = Request { op: "read", path: "a.txt", max_bytes: 1024, mode: Some("image"), size: 0 };
        assert_eq!(
            serde_json::to_string(&r).unwrap(),
            r#"{"op":"read","path":"a.txt","maxBytes":1024,"mode":"image"}"#
        );
        let r = Request { op: "write", path: "b.bin", max_bytes: 0, mode: None, size: 12345 };
        assert_eq!(serde_json::to_string(&r).unwrap(), r#"{"op":"write","path":"b.bin","size":12345}"#);
    }

    /// 响应/问候/条目解析（Go 字段名 camelCase）。
    #[test]
    fn response_parse() {
        let g: Greeting = serde_json::from_str(r#"{"ok":true,"root":"/Users/x","ver":1}"#).unwrap();
        assert!(g.ok && g.ver == 1 && g.root == "/Users/x");
        let r: Response = serde_json::from_str(
            r#"{"ok":false,"code":"not_found","msg":"没有这个文件"}"#,
        )
        .unwrap();
        assert!(!r.ok && r.code == "not_found");
        let r: Response = serde_json::from_str(
            r#"{"ok":true,"entries":[{"name":"a","isDir":false,"size":3,"mtimeMs":1700000000000,"mode":420}]}"#,
        )
        .unwrap();
        assert_eq!(r.entries.len(), 1);
        assert_eq!(r.entries[0].name, "a");
        assert_eq!(r.entries[0].mtime_ms, 1700000000000);
        assert_eq!(r.entries[0].mode, 420);
    }

    // ---------- UploadLimiter（发送端速率义务；Go files-cli tokenBucket 同语义） ----------

    #[test]
    fn upload_limiter_bounds() {
        assert!(UploadLimiter::new(0).is_none(), "0 = 不限");
        assert!(UploadLimiter::new(-1).is_none(), "负值 = 不限");
        let l = UploadLimiter::new(DEFAULT_RATE_LIMIT).unwrap();
        // 缺省 2MiB/s：burst = min(rate, 256KiB) = 256KiB（对端窗口一半以下）
        assert_eq!(l.burst, 256 << 10);
        // 小速率：补充上限不超过 rate 本身（否则节拍失真）
        let small = UploadLimiter::new(1024).unwrap();
        assert_eq!(small.burst, 1024);
        assert_eq!(small.rate, 1024);
        // 空桶起步（v0.12.1 形态：满桶首秒突刺会打爆对端窗口）
        assert_eq!(l.tokens, 0.0);
    }

    #[test]
    fn upload_limiter_settle_three_regimes() {
        let rate = 1000i64;
        let burst = 1000i64; // rate < TOKEN_BURST_CAP ⇒ burst = rate
        // ① 令牌足够：立即放行，扣除 n
        let (t, w) = UploadLimiter::settle(1.0, 500.0, rate, burst, 300);
        assert_eq!(w, 0.0);
        assert_eq!(t, 700.0);
        // ② 停顿后削峰：tokens 高水位恒 ≤ burst（补充 10000 → 截在 1000）
        let (t, w) = UploadLimiter::settle(10.0, 0.0, rate, burst, 0);
        assert_eq!(t, 1000.0, "停顿后补充上限 = burst");
        assert_eq!(w, 0.0);
        // ③ 令牌不足：等待 = 差额/rate，tokens 不动
        let (t, w) = UploadLimiter::settle(0.0, 100.0, rate, burst, 300);
        assert_eq!(t, 100.0);
        assert!((w - 0.2).abs() < 1e-9, "等待 = (300-100)/1000 = 0.2s（得 {w}）");
        // ④ 整块配额 > burst：积累作废、按 n/rate 等满（exec-r2 N1 形态）
        let (t, w) = UploadLimiter::settle(100.0, 900.0, 1_000_000, 256 << 10, 300_000);
        assert_eq!(t, 0.0, "整块放行 = 积累作废");
        assert!((w - 0.3).abs() < 1e-9, "等待 = 300000/1000000 = 0.3s（得 {w}）");
    }

    #[test]
    fn upload_limiter_pacing_shapes() {
        // 节拍不快于 rate：连续结算 10 块（每块 100B，rate=1000）在零 elapsed 下
        // 第 2 块起必产生等待（空桶起步——首块也没有白给的令牌）
        let rate = 1000i64;
        let burst = 1000i64;
        let mut tokens = 0.0f64;
        let mut total_wait = 0.0f64;
        for _ in 0..10 {
            let (t, w) = UploadLimiter::settle(0.0, tokens, rate, burst, 100);
            tokens = t;
            total_wait += w;
        }
        assert!(total_wait >= 0.9, "10×100B @1000B/s 至少要 ~1s 配额（得 {total_wait}）");
    }

    #[test]
    fn upload_limiter_block_hint() {
        // 缺省 2MiB/s：rate/8 = 256KiB = MAX_CHUNK（Go 同读块形态，零差异）
        assert_eq!(UploadLimiter::new(DEFAULT_RATE_LIMIT).unwrap().block_hint(), MAX_CHUNK);
        // 低速率：钳到 16KiB 下限（包级平滑——终验轮 3' 的贴闸形态整改）
        assert_eq!(UploadLimiter::new(120_000).unwrap().block_hint(), 16 << 10);
        // 250KB/s：rate/8 = 31250B（高于下限，原值通过——~25 包/块的平滑度）
        assert_eq!(UploadLimiter::new(250_000).unwrap().block_hint(), 31_250);
        // 中间速率：rate/8 原值（如 2MiB/s 的 1/4 → 64KiB）
        assert_eq!(UploadLimiter::new(512 << 10).unwrap().block_hint(), 64 << 10);
    }

    #[test]
    fn upload_limiter_await_quota_no_hang() {
        // 真调有状态循环体的看门狗测试（第二道门 高-1：整块配额 > burst 分支曾
        // 死循环——settle 恒返回正等待、loop 永不退出；矩阵 --rate-limit 250000 的
        // 5MB 首块 100% 命中）。节拍 = n/rate，睡一次即返。
        let mut l = UploadLimiter::new(250_000).unwrap(); // burst=250000 < MAX_CHUNK
        let t0 = std::time::Instant::now();
        l.await_quota(MAX_CHUNK); // 262144B 首块：n > burst ⇒ 整块路径
        let dt = t0.elapsed();
        assert!(
            dt >= std::time::Duration::from_millis(1000) && dt < std::time::Duration::from_secs(5),
            "整块路径应按 n/rate ≈ 1.05s 返回（得 {dt:?}）"
        );
        assert_eq!(l.tokens, 0.0, "整块放行后积累作废");
        // 普通差额路径也必须能返回（第二块小块走 settle 循环）
        let t1 = std::time::Instant::now();
        l.await_quota(1000);
        assert!(t1.elapsed() < std::time::Duration::from_secs(3), "差额路径不应久等");
    }
}

#[cfg(test)]
mod greeting_watchdog_tests {
    use super::*;

    /// 问候帧看门（D-1 中-7 → D-2 收口）：对端不应答时首响应段按预算到点退出
    /// （不永久挂死），错误归因带「首响应（问候帧）超预算」。
    #[test]
    fn greeting_deadline_fires_on_silent_peer() {
        // 静默对端的等价形态：读线程从未投递（通道有发送方但从不发、也未断开——
        // Disconnected 走另一分支，这里钉 Timeout 分支）。
        let (_tx_keepalive, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, ConnErr>>();
        let mut s = Stream::from_rx(rx);
        let t0 = std::time::Instant::now();
        let r = s.read_line_deadline(std::time::Instant::now() + Duration::from_millis(200));
        let dt = t0.elapsed();
        assert!(r.is_err(), "静默对端必须按预算失败");
        let msg = match &r.unwrap_err() {
            FilesError::Code { msg, .. } => msg.clone(),
            other => format!("{other:?}"),
        };
        assert!(msg.contains("首响应（问候帧）超预算"), "归因文案（实收 {msg:?}）");
        assert!(dt < Duration::from_secs(2), "不应等到天荒地老（{dt:?}）");
    }

    /// 问候帧正常返回的路径不受预算影响（预算内完成 handshake）。
    #[test]
    fn greeting_arrives_within_budget() {
        let (tx, rx) = std::sync::mpsc::channel::<Result<Vec<u8>, ConnErr>>();
        tx.send(Ok(br#"{"ok":true,"root":"/tmp","ver":1}
"#.to_vec())).unwrap();
        let s = Stream::from_rx(rx);
        let s = Stream::handshake(s, Duration::from_secs(2)).expect("预算内问候");
        assert_eq!(s.root, "/tmp");
        assert_eq!(s.ver, 1);
        let _ = rx; // 通道保活（读线程形态在测试构造下不存在）
    }
}
