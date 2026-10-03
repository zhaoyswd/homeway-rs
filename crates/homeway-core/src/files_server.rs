//! files 服务端（R3；语义真源 `pkg/files/server.go`——UDS 承载、根 = $HOME 恒读写、
//! 每命令一条流：问候行 → 请求行 → 响应/帧）。
//!
//! 行协议与 R2 客户端（`files.rs`）同源（Go `pkg/files/proto.go`）：JSON 行（`\n`
//! 结尾）+ 4B BE 长度前缀帧（len=0 = 终止/提交）。六动词全实现；路径沙箱 = 规整
//! 到根内相对路径、拒 `..`/控制字符、符号链接不跟随（canonicalize 复核在根内）。
//!
//! 上传原语：`<名>.tierpart.<8hex>` 临时文件 + rename 提交（**无 fsync**——Go 同无）；
//! 提前关流/出错只删**自己创建的**临时文件；写同路径前顺手回收超 24h 的陈旧残留。

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};

use crate::files::{Entry, MAX_CHUNK};

/// 与 App 侧 NAPI 契约对齐的默认值（Go 同源）。
const DEFAULT_TEXT_MAX_BYTES: i64 = 512 * 1024;
const DEFAULT_IMAGE_MAX_BYTES: i64 = 8 * 1024 * 1024;
const MAX_INLINE_READ: i64 = 16 * 1024 * 1024;
/// 并发在册流上限（超限回 busy——有可行动文案，不静默挂起）。
pub const MAX_CONNS: usize = 16;
/// 每流空闲期限。
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// 陈旧 .tierpart 的回收阈值。
const STALE_PART_AGE: Duration = Duration::from_secs(24 * 3600);
/// 每流读块（读文件喂帧）。
const READ_BLOCK: usize = 64 * 1024;

#[derive(Debug, Deserialize)]
struct Request {
    op: String,
    #[serde(default)]
    path: String,
    #[serde(default)]
    max_bytes: i64,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    size: i64,
}

#[derive(Serialize, Default)]
struct Response {
    ok: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    code: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    msg: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entries: Vec<SerdeEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    entry: Option<SerdeEntry>,
    #[serde(skip_serializing_if = "is_zero")]
    size: i64,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    #[serde(skip_serializing_if = "String::is_empty")]
    text: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    base64: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    root: String,
    #[serde(skip_serializing_if = "is_zero")]
    ver: i64,
}

fn is_zero(v: &i64) -> bool {
    *v == 0
}

/// Entry 的序列化形态（字段名与 Go/客户端一致）。
#[derive(Serialize, Deserialize)]
struct SerdeEntry {
    name: String,
    #[serde(rename = "isDir")]
    is_dir: bool,
    size: i64,
    #[serde(rename = "mtimeMs")]
    mtime_ms: i64,
    #[serde(default)]
    mode: u32,
}

impl From<&Entry> for SerdeEntry {
    fn from(e: &Entry) -> Self {
        Self {
            name: e.name.clone(),
            is_dir: e.is_dir,
            size: e.size,
            mtime_ms: e.mtime_ms,
            mode: e.mode,
        }
    }
}

fn err_response(code: &str, msg: impl Into<String>) -> Response {
    Response { ok: false, code: code.to_string(), msg: msg.into(), ..Default::default() }
}

fn ok_response() -> Response {
    Response { ok: true, ..Default::default() }
}

/// files 服务（以某目录为根；根 = 后端用户主目录）。
pub struct FilesServer {
    root: PathBuf,
    logf: crate::Logf,
    /// 并发在册闸（FIX-36 语义：满 16 条回 busy——R3-G3 实装；跨 accept 副本共享）。
    conns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl FilesServer {
    /// 打开根目录（rootDir 空 = 用户主目录）。不存在/不是目录 → 报错。
    pub fn open(root: Option<&Path>, logf: crate::Logf) -> std::io::Result<Self> {
        let root = match root {
            Some(p) => p.to_path_buf(),
            None => std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "无 HOME"))?,
        };
        let abs = root.canonicalize().unwrap_or(root);
        if !abs.is_dir() {
            return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} 不是目录", abs.display())));
        }
        Ok(Self { root: abs, logf, conns: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)) })
    }

    pub fn root_dir(&self) -> &Path {
        &self.root
    }

    /// 在 UDS 上服务（每连接一线程；accept 出错即返回）。
    pub fn serve(&self, ln: UnixListener) -> std::io::Result<()> {
        for conn in ln.incoming() {
            let conn = conn?;
            self.spawn_conn(conn);
        }
        Ok(())
    }

    /// 可停形态（引擎收工：非阻塞 accept + stop 轮询——② 关 UDS listeners 的执行面）。
    pub fn serve_stoppable(&self, ln: UnixListener, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::io::Result<()> {
        use std::sync::atomic::Ordering as OD;
        ln.set_nonblocking(true)?;
        loop {
            if stop.load(OD::Relaxed) {
                return Ok(());
            }
            match ln.accept() {
                Ok((conn, _)) => {
                    let _ = conn.set_nonblocking(false);
                    self.spawn_conn(conn);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(_) => return Ok(()), // listener 已关/异常：摘监听退出
            }
        }
    }

    /// accept 后派发：在册 <16 走正常服务（结束时 release）；满则 busy 拒入
    ///（Go sem 同义——先发 greeting、吞掉一条请求再回 busy，客户端拿到的仍是
    ///「有根目录的服务端拒绝了我」，不是连接层失败）。
    fn spawn_conn(&self, conn: UnixStream) {
        use std::sync::atomic::Ordering as OD;
        if self.conns.fetch_add(1, OD::AcqRel) < MAX_CONNS {
            let server = FilesServer {
                root: self.root.clone(),
                logf: self.logf.clone(),
                conns: std::sync::Arc::clone(&self.conns),
            };
            std::thread::Builder::new()
                .name("homeway-files".into())
                .spawn(move || {
                    let conns = std::sync::Arc::clone(&server.conns);
                    server.serve_conn(conn);
                    conns.fetch_sub(1, OD::AcqRel);
                })
                .ok();
        } else {
            self.conns.fetch_sub(1, OD::AcqRel);
            let server = FilesServer {
                root: self.root.clone(),
                logf: self.logf.clone(),
                conns: std::sync::Arc::clone(&self.conns),
            };
            std::thread::Builder::new()
                .name("homeway-files-busy".into())
                .spawn(move || server.serve_busy(conn))
                .ok();
        }
    }

    /// 满员拒入路径（Go server.go:141-156 同序）。
    fn serve_busy(self, conn: UnixStream) {
        let _ = conn.set_read_timeout(Some(IDLE_TIMEOUT));
        let _ = conn.set_write_timeout(Some(IDLE_TIMEOUT));
        let mut reader = BufReader::new(match conn.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        });
        let mut writer = conn;
        let greeting = Response { ok: true, root: self.root.display().to_string(), ver: crate::files::VERSION as i64, ..Default::default() };
        if write_line(&mut writer, &greeting).is_err() {
            return;
        }
        // 吞掉请求行（客户端按「问候 → 请求 → 响应」节拍走——直接关会让它误判传输层错）
        if read_line(&mut reader).is_err() {
            return;
        }
        let _ = write_line(
            &mut writer,
            &err_response(crate::files::CODE_SERVER_BUSY, format!("服务端并发流已满（{MAX_CONNS}），请稍后重试")),
        );
        (self.logf)(&format!("files: 并发流已满（{MAX_CONNS}）——回 busy 拒入"));
    }

    fn serve_conn(self, conn: UnixStream) {
        let _ = conn.set_read_timeout(Some(IDLE_TIMEOUT));
        let _ = conn.set_write_timeout(Some(IDLE_TIMEOUT));
        let mut reader = BufReader::new(match conn.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        });
        let mut writer = conn;
        // 问候行恒第一个发（即使请求行非法，客户端也能先拿到 root/ver）
        let greeting = Response { ok: true, root: self.root.display().to_string(), ver: crate::files::VERSION as i64, ..Default::default() };
        if write_line(&mut writer, &greeting).is_err() {
            return;
        }
        let line = match read_line(&mut reader) {
            Ok(l) => l,
            Err(_) => return,
        };
        let req: Request = match serde_json::from_slice(&line) {
            Ok(r) => r,
            Err(e) => {
                let _ = write_line(&mut writer, &err_response(crate::files::CODE_INVALID_ARG, format!("请求不是 JSON：{e}")));
                return;
            }
        };
        match self.dispatch(&mut writer, &mut reader, &req) {
            Ok(()) => {}
            Err(resp) => {
                let _ = write_line(&mut writer, &resp);
                (self.logf)(&format!("files: {} {:?} 失败：{}", req.op, req.path, resp.msg));
            }
        }
    }

    #[allow(clippy::result_large_err)]
    fn dispatch(&self, w: &mut UnixStream, r: &mut BufReader<UnixStream>, req: &Request) -> Result<(), Response> {
        match req.op.as_str() {
            "list" => {
                let ents = self.list(&req.path)?;
                let mut resp = ok_response();
                resp.entries = ents;
                write_line(w, &resp).map_err(|e| err_response(crate::files::CODE_OP_FAILED, format!("写响应失败：{e}")))
            }
            "stat" => {
                let e = self.stat(&req.path)?;
                let mut resp = ok_response();
                resp.entry = Some(e);
                write_line(w, &resp).map_err(|e| err_response(crate::files::CODE_OP_FAILED, format!("写响应失败：{e}")))
            }
            "mkdir" => {
                self.mkdir(&req.path)?;
                write_line(w, &ok_response()).map_err(|e| err_response(crate::files::CODE_OP_FAILED, format!("写响应失败：{e}")))
            }
            "read" => self.read(w, req),
            "download" => self.download(w, req),
            "write" => self.write(w, r, req),
            other => Err(err_response(crate::files::CODE_INVALID_ARG, format!("未知操作 {other:?}"))),
        }
    }

    // ---------- 路径沙箱 ----------

    /// 规整成「相对根的路径」：空/./ 恒 "."；拒 NUL/控制字符与 .. 段；canonicalize
    /// 复核仍在根内（符号链接逃逸拒之——not_found 语义，不泄漏根外信息）。
    #[allow(clippy::result_large_err)]
    fn rel_path(&self, p: &str) -> Result<PathBuf, Response> {
        if p.is_empty() {
            return Ok(self.root.clone()); // 根目录（"." 语义——不是进程 CWD！）
        }
        if p.bytes().any(|b| b == 0 || b < 0x20 || b == 0x7f) {
            return Err(err_response(crate::files::CODE_INVALID_ARG, "路径含非法字符"));
        }
        for seg in p.split('/') {
            if seg == ".." {
                return Err(err_response(crate::files::CODE_INVALID_ARG, format!("路径越界：{p:?}")));
            }
        }
        let trimmed = p.trim_start_matches('/');
        let candidate = self.root.join(trimmed);
        if trimmed.is_empty() || trimmed == "." {
            return Ok(self.root.clone());
        }
        // 符号链接逃逸检查（存在性不影响不存在类错误——与 Go os.Root 语义对齐：
        // 逃逸类错误统一按 not_found）
        match std::fs::canonicalize(&candidate) {
            Ok(real) => {
                if !real.starts_with(&self.root) {
                    return Err(err_response(crate::files::CODE_NOT_FOUND, "路径不在根内"));
                }
                Ok(real)
            }
            Err(_) => Ok(candidate), // 不存在：由各动词按 not_found 处理
        }
    }

    fn map_io_err(op: &str, p: &str, e: std::io::Error) -> Response {
        match e.kind() {
            std::io::ErrorKind::NotFound => err_response(crate::files::CODE_NOT_FOUND, format!("{op} {p}：不存在")),
            std::io::ErrorKind::PermissionDenied => err_response(crate::files::CODE_PERMISSION, format!("{op} {p}：拒绝访问")),
            _ => err_response(crate::files::CODE_OP_FAILED, format!("{op} {p}：{e}")),
        }
    }

    fn entry_of(md: &std::fs::Metadata, name: String) -> SerdeEntry {
        SerdeEntry {
            name,
            is_dir: md.is_dir(),
            size: md.len() as i64,
            mtime_ms: md.modified().ok()
                .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0),
            mode: md.permissions().mode() & 0o777,
        }
    }

    // ---------- 六动词 ----------

    #[allow(clippy::result_large_err)]
    fn list(&self, p: &str) -> Result<Vec<SerdeEntry>, Response> {
        let dir = self.rel_path(p)?;
        let rd = std::fs::read_dir(&dir).map_err(|e| Self::map_io_err("list", p, e))?;
        let mut out = Vec::new();
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if let Ok(md) = ent.metadata() {
                out.push(Self::entry_of(&md, name));
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(out)
    }

    #[allow(clippy::result_large_err)]
    fn stat(&self, p: &str) -> Result<SerdeEntry, Response> {
        let path = self.rel_path(p)?;
        let md = std::fs::metadata(&path).map_err(|e| Self::map_io_err("stat", p, e))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        Ok(Self::entry_of(&md, name))
    }

    #[allow(clippy::result_large_err)]
    fn mkdir(&self, p: &str) -> Result<(), Response> {
        let path = self.rel_path(p)?;
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name.is_empty() || name == "." || name.contains('/') {
            return Err(err_response(crate::files::CODE_INVALID_NAME, format!("目录名不合法：{name:?}")));
        }
        if path.exists() {
            return Err(err_response(crate::files::CODE_ALREADY_EXISTS, "同名条目已存在"));
        }
        std::fs::create_dir(&path).map_err(|e| Self::map_io_err("mkdir", p, e))
    }

    #[allow(clippy::result_large_err)]
    fn read(&self, w: &mut UnixStream, req: &Request) -> Result<(), Response> {
        let path = self.rel_path(&req.path)?;
        let md = std::fs::metadata(&path).map_err(|e| Self::map_io_err("read", &req.path, e))?;
        if md.is_dir() {
            return Err(err_response(crate::files::CODE_IS_DIR, "是目录，不是文件"));
        }
        let mut max_bytes = req.max_bytes;
        if max_bytes <= 0 {
            max_bytes = if req.mode.as_deref() == Some("image") { DEFAULT_IMAGE_MAX_BYTES } else { DEFAULT_TEXT_MAX_BYTES };
        }
        max_bytes = max_bytes.min(MAX_INLINE_READ);
        let mut f = std::fs::File::open(&path).map_err(|e| Self::map_io_err("read", &req.path, e))?;
        let mut buf = vec![0u8; max_bytes as usize];
        let mut n = 0usize;
        while n < buf.len() {
            match f.read(&mut buf[n..]) {
                Ok(0) => break,
                Ok(k) => n += k,
                Err(e) => return Err(Self::map_io_err("read", &req.path, e)),
            }
        }
        let mut resp = ok_response();
        resp.size = md.len() as i64;
        resp.truncated = (md.len() as i64) > n as i64;
        if req.mode.as_deref() == Some("image") {
            use base64::Engine as _;
            resp.base64 = base64::engine::general_purpose::STANDARD.encode(&buf[..n]);
        } else {
            resp.text = String::from_utf8_lossy(&buf[..n]).into_owned();
        }
        write_line(w, &resp).map_err(|e| err_response(crate::files::CODE_OP_FAILED, format!("写响应失败：{e}")))
    }

    #[allow(clippy::result_large_err)]
    fn download(&self, w: &mut UnixStream, req: &Request) -> Result<(), Response> {
        let path = self.rel_path(&req.path)?;
        let md = std::fs::metadata(&path).map_err(|e| Self::map_io_err("download", &req.path, e))?;
        if md.is_dir() {
            return Err(err_response(crate::files::CODE_IS_DIR, "是目录，不是文件"));
        }
        let mut resp = ok_response();
        resp.size = md.len() as i64;
        write_line(w, &resp).map_err(|e| err_response(crate::files::CODE_OP_FAILED, format!("写响应失败：{e}")))?;
        let mut f = std::fs::File::open(&path).map_err(|e| Self::map_io_err("download", &req.path, e))?;
        let mut buf = vec![0u8; READ_BLOCK.min(MAX_CHUNK)];
        loop {
            let n = f.read(&mut buf).map_err(|e| Self::map_io_err("download", &req.path, e))?;
            if n == 0 {
                break;
            }
            if write_frame(w, &buf[..n]).is_err() {
                return Ok(()); // 对端断了：静默收工
            }
        }
        let _ = write_frame(w, &[]); // 终止帧
        Ok(())
    }

    #[allow(clippy::result_large_err)]
    fn write(&self, w: &mut UnixStream, r: &mut BufReader<UnixStream>, req: &Request) -> Result<(), Response> {
        let path = self.rel_path(&req.path)?;
        if path == self.root {
            return Err(err_response(crate::files::CODE_INVALID_NAME, "不能写入根目录本身"));
        }
        self.clean_stale_parts(&path);
        // 随机临时名：并发写同一路径不互踩；只删自己创建的那个
        let mut rnd = [0u8; 4];
        getrandom::getrandom(&mut rnd).expect("系统随机源不可用");
        let part = path.with_file_name(format!(
            "{}.tierpart.{}",
            path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
            rnd.iter().map(|b| format!("{b:02x}")).collect::<String>()
        ));
        if let Err(e) = std::fs::File::create(&part) {
            return Err(Self::map_io_err("write", &req.path, e));
        }
        let committed = self.receive_upload(w, r, &part, &path, req);
        match committed {
            Ok(total) => {
                let mut resp = ok_response();
                resp.size = total as i64;
                let _ = write_line(w, &resp);
                if req.size > 0 && req.size != total as i64 {
                    (self.logf)(&format!("files: 上传 {:?} 声明 {} 字节，实收 {total}（已提交）", req.path, req.size));
                }
                Ok(())
            }
            Err(resp) => {
                let _ = std::fs::remove_file(&part); // 取消/中断：只删自己的临时文件
                Err(resp)
            }
        }
    }

    #[allow(clippy::result_large_err)]
    fn receive_upload(
        &self,
        w: &mut UnixStream,
        r: &mut BufReader<UnixStream>,
        part: &Path,
        target: &Path,
        req: &Request,
    ) -> Result<u64, Response> {
        // 回 {"ok":true} 表示备好 .tierpart
        if write_line(w, &ok_response()).is_err() {
            return Err(err_response(crate::files::CODE_CANCELED, "对端已断"));
        }
        let mut f = match std::fs::OpenOptions::new().write(true).open(part) {
            Ok(f) => f,
            Err(e) => return Err(Self::map_io_err("write", &req.path, e)),
        };
        let mut total = 0u64;
        loop {
            match read_frame(r) {
                Ok(Some(payload)) => {
                    if f.write_all(&payload).is_err() {
                        return Err(Self::map_io_err("write", &req.path, std::io::Error::other("写临时文件失败")));
                    }
                    total += payload.len() as u64;
                }
                Ok(None) => break, // 终止帧 = 提交
                Err(_) => {
                    return Err(err_response(crate::files::CODE_CANCELED, format!("上传中断（已收到 {total} 字节，已清理临时文件）")));
                }
            }
        }
        drop(f); // 句柄先关，rename 才在 Unix 上语义干净
        if let Err(e) = std::fs::rename(part, target) {
            let _ = std::fs::remove_file(part);
            return Err(Self::map_io_err("write", &req.path, e));
        }
        Ok(total)
    }

    /// 顺手回收同目录下陈旧的 .tierpart 残留（超 24h；绝不碰正在用的）。
    fn clean_stale_parts(&self, target: &Path) {
        let Some(dir) = target.parent() else { return };
        let Some(base) = target.file_name().and_then(|n| n.to_str()) else { return };
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        let prefix = format!("{base}.tierpart.");
        let cutoff = SystemTime::now() - STALE_PART_AGE;
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().into_owned();
            if !name.starts_with(&prefix) {
                continue;
            }
            let Ok(md) = ent.metadata() else { continue };
            if md.modified().map(|t| t > cutoff).unwrap_or(true) {
                continue;
            }
            let p = dir.join(&name);
            if std::fs::remove_file(&p).is_ok() {
                (self.logf)(&format!("files: 回收陈旧临时文件 {}（超 24h 未提交）", p.display()));
            }
        }
    }
}

// ---------- 线协议 ----------

fn read_line(r: &mut BufReader<UnixStream>) -> std::io::Result<Vec<u8>> {
    let mut buf = Vec::new();
    let n = r.read_until(b'\n', &mut buf)?;
    if n == 0 {
        return Err(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "流关闭"));
    }
    while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
        buf.pop();
    }
    Ok(buf)
}

fn write_line(w: &mut UnixStream, resp: &Response) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(resp).map_err(std::io::Error::other)?;
    line.push(b'\n');
    w.write_all(&line)
}

fn write_frame(w: &mut UnixStream, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&(payload.len() as u32).to_be_bytes())?;
    w.write_all(payload)
}

fn read_frame(r: &mut BufReader<UnixStream>) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    // 前缀判定与客户端共用单一真源（files::decode_prefix——R5 第二道门 高-4 整改：
    // 原为同语义重实现，fuzz 抽取面只盖了客户端半边）
    match crate::files::decode_prefix(&len) {
        Ok(crate::files::Prefix::Terminated) => Ok(None),
        Ok(crate::files::Prefix::Frame { len: n }) => {
            let mut buf = vec![0u8; n];
            r.read_exact(&mut buf)?;
            Ok(Some(buf))
        }
        Err(e) => Err(std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string())),
    }
}

/// UDS 监听（exit-service-uds：死/活判别 + chmod 0600；路径 ≥100B 拒绝）。
pub fn listen_local_service(dir: &Path, name: &str, logf: &crate::Logf) -> std::io::Result<UnixListener> {
    let sock = dir.join(name);
    let s = sock.display().to_string();
    if s.len() >= 100 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("路径超长（{} 字节 ≥ 100，sun_path 上限）", s.len())));
    }
    // 活实例占用判别：拨得通 = 有人（报占用）；ENOENT/ECONNREFUSED = 死残留（可清）
    if let Ok(c) = UnixStream::connect(&sock) {
        drop(c);
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "socket 已被另一个活实例占用（同 state 双实例？）"));
    }
    let _ = std::fs::remove_file(&sock);
    let ln = UnixListener::bind(&sock)?;
    let _ = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600));
    let _ = logf;
    Ok(ln)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "homeway-rs-filesrv-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap().subsec_nanos()
        ));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn noop_logf() -> crate::Logf {
        std::sync::Arc::new(|_| {})
    }

    /// G3：满员拒入（Go server.go:140-156 同序）——在册置满后第 17 条流拿到
    /// greeting → 回 server_busy 错误响应（不是连接层失败）。
    #[test]
    fn busy_rejection_when_conns_full() {
        let dir = tmpdir("busy");
        let srv = FilesServer { root: dir.canonicalize().unwrap(), logf: noop_logf(), conns: Default::default() };
        srv.conns.store(MAX_CONNS, std::sync::atomic::Ordering::Release);

        let (a, b) = UnixStream::pair().unwrap();
        let s2 = FilesServer { root: srv.root.clone(), logf: noop_logf(), conns: std::sync::Arc::clone(&srv.conns) };
        std::thread::spawn(move || s2.serve_busy(a));
        let mut r = BufReader::new(b.try_clone().unwrap());
        let mut w = b;
        // 问候行照发（客户端先拿到 root/ver）
        let g: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(g["ok"], serde_json::json!(true));
        // 发一条请求 → busy 响应
        writeln!(w, "{}", serde_json::json!({"op":"list","path":""})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["code"], serde_json::json!("server_busy"));
        assert!(resp["msg"].as_str().unwrap().contains("并发流已满"));
    }

    /// 内存对拍：客户端请求（R2 同协议形态）→ 服务端响应。**每命令一对 pair**
    /// （协议即「每命令一流」——Go serveConn 同义）。
    #[test]
    fn six_verbs_over_ud_pair() {
        let dir = tmpdir("verbs");
        let srv = FilesServer { root: dir.canonicalize().unwrap(), logf: noop_logf(), conns: Default::default() };
        std::fs::write(dir.join("hello.txt"), b"hello-files").unwrap();

        // 一条命令 = 起 pair + 服务端线程；返回（读半, 写半）
        fn call(srv: &FilesServer) -> (BufReader<UnixStream>, UnixStream) {
            let (a, b) = UnixStream::pair().unwrap();
            let s2 = FilesServer { root: srv.root.clone(), logf: noop_logf(), conns: std::sync::Arc::clone(&srv.conns) };
            std::thread::spawn(move || s2.serve_conn(a));
            let w = b.try_clone().unwrap();
            (BufReader::new(b), w)
        }

        // 问候行（每条流的第一个响应）
        let (mut r, w) = call(&srv);
        let g: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(g["ok"], serde_json::json!(true));
        assert_eq!(g["ver"], serde_json::json!(1));
        drop((r, w));

        // list
        let (mut r, mut w) = call(&srv);
        let _ = read_line(&mut r); // 问候
        writeln!(w, "{}", serde_json::json!({"op":"list","path":""})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert!(resp["entries"].as_array().unwrap().iter().any(|e| e["name"] == "hello.txt"));
        drop((r, w));

        // download（响应行 + 帧 + 终止帧）
        let (mut r, mut w) = call(&srv);
        let _ = read_line(&mut r);
        writeln!(w, "{}", serde_json::json!({"op":"download","path":"hello.txt"})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["size"], serde_json::json!(11));
        let frame = read_frame(&mut r).unwrap().unwrap();
        assert_eq!(frame, b"hello-files");
        assert!(read_frame(&mut r).unwrap().is_none(), "终止帧");
        drop((r, w));

        // upload（回 ok → 帧 → 终止帧 → 回 ok+size）
        let (mut r, mut w) = call(&srv);
        let _ = read_line(&mut r);
        writeln!(w, "{}", serde_json::json!({"op":"write","path":"up.bin","size":5})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(true), "备好 .tierpart");
        write_frame(&mut w, b"ABCDE").unwrap();
        write_frame(&mut w, &[]).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["size"], serde_json::json!(5));
        assert_eq!(std::fs::read(dir.join("up.bin")).unwrap(), b"ABCDE");
        drop((r, w));

        // read（文本形态 + truncated 语义）
        let (mut r, mut w) = call(&srv);
        let _ = read_line(&mut r);
        writeln!(w, "{}", serde_json::json!({"op":"read","path":"hello.txt"})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["text"], serde_json::json!("hello-files"));
        // truncated=false 按 Go omitempty 语义省略（缺省即 false）
        assert!(resp.get("truncated").is_none() || resp["truncated"] == serde_json::json!(false));
        drop((r, w));

        // stat + mkdir
        let (mut r, mut w) = call(&srv);
        let _ = read_line(&mut r);
        writeln!(w, "{}", serde_json::json!({"op":"stat","path":"up.bin"})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["entry"]["size"], serde_json::json!(5));
        drop((r, w));
        let (mut r, mut w) = call(&srv);
        let _ = read_line(&mut r);
        writeln!(w, "{}", serde_json::json!({"op":"mkdir","path":"sub"})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(true));
        assert!(dir.join("sub").is_dir());
        drop((r, w));

        // 路径越界拒绝
        let (mut r, mut w) = call(&srv);
        let _ = read_line(&mut r);
        writeln!(w, "{}", serde_json::json!({"op":"list","path":"../escape"})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(false));
        assert_eq!(resp["code"], serde_json::json!("invalid_arg"));
        drop((r, w));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// UDS 监听面：bind + chmod 0600 + 活实例占用判别。
    #[test]
    fn uds_listen_and_occupancy() {
        let dir = tmpdir("uds");
        let logf = noop_logf();
        let ln = listen_local_service(&dir, "files.sock", &logf).unwrap();
        let sock = dir.join("files.sock");
        assert!(sock.exists());
        let mode = sock.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket 权限应收紧 0600");
        // 活占用：一个 listener 在听 → 第二个 bind 报占用
        let r = listen_local_service(&dir, "files.sock", &logf);
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::AddrInUse);
        drop(ln);
        // 死残留（listener 已关、文件还在）→ 可清重建
        assert!(sock.exists());
        let ln2 = listen_local_service(&dir, "files.sock", &logf);
        assert!(ln2.is_ok(), "死残留应可清重建");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
