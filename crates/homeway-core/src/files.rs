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
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::session::Session;
use crate::wgcore::ConnErr;

/// files 服务端口（隧道 IP 上；出口按 LocalServices 转投 files.sock）。
pub const FILES_PORT: u16 = 7802;
/// 请求行上限（一行 JSON）。
pub const MAX_REQUEST_LINE: usize = 64 * 1024;
/// 单帧载荷上限（客户端应遵守）。
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
    sess: &'a Session,
    id: u64,
    buf: Vec<u8>,
    /// 读线程交付通道（线程在 EOF/错误/通道断开时退出）。
    rx: std::sync::mpsc::Receiver<Result<Vec<u8>, ConnErr>>,
    /// 问候帧带来的根与版本（诊断/展示用）。
    pub root: String,
    pub ver: i64,
}

impl Drop for Stream<'_> {
    fn drop(&mut self) {
        // 关流即语义：上传未提交（未发终止帧）= 取消（服务端删 .tierpart）。
        // 测试构造（id=u64::MAX）跳过——dangling Session 不可解引用。
        if self.id != u64::MAX {
            let _ = self.sess.client().close(self.id);
        }
    }
}

impl<'a> Stream<'a> {
    /// 测试构造：受控读通道驱动（不拨号；close 走真实路径——Session 由 PhantomData
    /// 借用形参规避）。
    #[cfg(test)]
    fn from_rx(rx: std::sync::mpsc::Receiver<Result<Vec<u8>, ConnErr>>) -> Stream<'static> {
        Stream {
            sess: unsafe { &*(std::ptr::NonNull::<Session>::dangling().as_ptr()) },
            id: u64::MAX,
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
        let mut s = Stream { sess, id, buf: Vec::with_capacity(16 * 1024), rx, root: String::new(), ver: 0 };
        let line = s.read_line().map_err(|e| FilesError::Code {
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

    /// 读一行（≤64KB；去 EOL）。
    fn read_line(&mut self) -> Result<Vec<u8>, FilesError> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                    line.pop();
                }
                if line.len() > MAX_REQUEST_LINE {
                    return Err(transport("行超过 64KB 上限"));
                }
                return Ok(line);
            }
            if self.buf.len() > MAX_REQUEST_LINE {
                return Err(transport("行超过 64KB 上限"));
            }
            let chunk = self.next_chunk()?;
            self.buf.extend_from_slice(&chunk);
        }
    }

    /// 写全部（部分写 + **Ok(0)=发送缓冲满的背压语义**：立即重试，RPC 往返驱动 ACK
    /// 排空——与 speedtest::write_all 同款；百万次零进展才判死）。
    fn write_all(&mut self, data: &[u8]) -> Result<(), FilesError> {
        let mut off = 0;
        let mut zero_streak = 0u32;
        while off < data.len() {
            let n = self.sess.client().write(self.id, data[off..].to_vec()).map_err(transport)?;
            if n == 0 {
                zero_streak += 1;
                if zero_streak > 1_000_000 {
                    return Err(transport("写通道长时间无进展"));
                }
                std::thread::yield_now();
                continue;
            }
            zero_streak = 0;
            off += n;
        }
        Ok(())
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
        let n = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        if n == 0 {
            self.buf.drain(..4);
            return Ok(None);
        }
        if n > MAX_CHUNK {
            return Err(transport(format!("帧长 {n} 超过上限 {MAX_CHUNK}")));
        }
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

// ---------- 动词（每命令一条流；Client 形态 = 拨号闭包由调用方给） ----------

/// 列目录。
pub fn list(sess: &Session, budget: Duration, path: &str) -> Result<Vec<Entry>, FilesError> {
    let mut s = Stream::open(sess, budget)?;
    let resp = s.call(&Request { op: "list", path, max_bytes: 0, mode: None, size: 0 })?;
    Ok(resp.entries)
}

/// 单条目信息。
pub fn stat(sess: &Session, budget: Duration, path: &str) -> Result<Entry, FilesError> {
    let mut s = Stream::open(sess, budget)?;
    let resp = s.call(&Request { op: "stat", path, max_bytes: 0, mode: None, size: 0 })?;
    resp.entry.ok_or_else(|| FilesError::Code { code: CODE_OP_FAILED.into(), msg: "响应缺 entry".into() })
}

/// 新建目录。
pub fn mkdir(sess: &Session, budget: Duration, path: &str) -> Result<(), FilesError> {
    let mut s = Stream::open(sess, budget)?;
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

/// 大文件下载：载荷帧原样写进 `w`，返回总字节数。`on_size` 收到响应行声明的
/// 大小（进度分母）。**断读收口**（FIX-40）：终止帧到达时对照声明——少收一律
/// 报错（提前终止被静默当成功会落半截文件）；多收容忍（下载生长中的文件合法）。
pub fn download<W, F>(sess: &Session, budget: Duration, path: &str, w: &mut W, on_size: F) -> Result<u64, FilesError>
where
    W: io::Write + ?Sized,
    F: FnOnce(i64),
{
    let mut s = Stream::open(sess, budget)?;
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

/// 大文件上传：请求 → 服务端 ready → 帧 → 终止帧（提交）→ 读提交结果。
/// **错误/提前 drop = 关流不发终止帧 ⇒ 服务端删 .tierpart**（目标不变）。
pub fn upload<R, F>(sess: &Session, budget: Duration, path: &str, r: &mut R, size: i64, on_progress: F) -> Result<u64, FilesError>
where
    R: io::Read + ?Sized,
    F: FnMut(u64),
{
    let mut s = Stream::open(sess, budget)?;
    s.call(&Request { op: "write", path, max_bytes: 0, mode: None, size })?;
    let mut buf = vec![0u8; MAX_CHUNK];
    let mut total: u64 = 0;
    let mut on_progress = on_progress;
    loop {
        let n = r.read(&mut buf).map_err(|e| FilesError::Code {
            code: CODE_OP_FAILED.into(),
            msg: format!("读本地失败：{e}"),
        })?;
        if n == 0 {
            break;
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
}
