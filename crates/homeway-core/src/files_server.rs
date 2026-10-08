//! files 服务端（R3；语义真源 `pkg/files/server.go`——UDS 承载、根 = $HOME 恒读写、
//! 每命令一条流：问候行 → 请求行 → 响应/帧）。
//!
//! 行协议与 R2 客户端（`files.rs`）同源（Go `pkg/files/proto.go`）：JSON 行（`\n`
//! 结尾）+ 4B BE 长度前缀帧（len=0 = 终止/提交）。六动词全实现；路径沙箱 = 规整
//! 到根内相对路径、拒 `..`/控制字符，随后**逐分量复核**（`resolve_in_root`：对齐 Go
//! `os.Root` 的符号链接三规则；不存在的叶子绝不直返未复核候选）。
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
/// 逐分量复核的符号链接链深度上限（防循环链接；= Go `os.Root` 的 `rootMaxSymlinks = 8`，
/// `os/root.go:70`）。
const RESOLVE_MAX_DEPTH: usize = 8;
/// 上传磁盘水位：目标文件系统可用空间低于本保留量即拒收新上传（1 GiB）。
const UPLOAD_RESERVE_BYTES: u64 = 1 << 30;
/// 进行中水位复查的累计间隔（每写满 8 MiB 复查一次可用空间）。
const UPLOAD_RECHECK_BYTES: u64 = 8 << 20;

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

#[derive(Serialize, Default, Debug)]
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
#[derive(Serialize, Deserialize, Debug)]
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

/// 目标目录的可用字节数（`statvfs` 的 `f_bavail × f_frsize`；macOS/Linux/OHOS 同 API）。
/// 失败 → `None`（调用方 fail-open + 节流告警）。
fn avail_bytes(dir: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt as _;
    let c = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

/// 上传水位判定（纯函数——三态可测；`avail = None` = statvfs 失败 ⇒ fail-open 放行）。
///
/// 起始门（`recheck=false`）：剩余 < 保留量 ⇒ 拒；声明大小 > 0 时按「声明 + 保留量」
/// 走快路径（免走完流量再拒）。进行中门（`recheck=true`）：剩余跌破保留量 ⇒ 中止。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum QuotaVerdict {
    Allow,
    RejectStart { avail: u64 },
    AbortMidway { avail: u64, written: u64 },
}

pub(crate) fn quota_verdict(avail: Option<u64>, declared: i64, written: u64, recheck: bool) -> QuotaVerdict {
    let Some(avail) = avail else {
        return QuotaVerdict::Allow; // statvfs 失败：fail-open（登记项）
    };
    if recheck {
        return if avail < UPLOAD_RESERVE_BYTES {
            QuotaVerdict::AbortMidway { avail, written }
        } else {
            QuotaVerdict::Allow
        };
    }
    if avail < UPLOAD_RESERVE_BYTES || (declared > 0 && avail < declared as u64 + UPLOAD_RESERVE_BYTES) {
        return QuotaVerdict::RejectStart { avail };
    }
    QuotaVerdict::Allow
}

/// 水位拒绝文案（**既有错误码 `op_failed`**，不新增码——词表冻结）。
fn quota_msg(avail: u64, declared: i64, written: u64, midway: bool) -> String {
    let mib = avail >> 20;
    if midway {
        format!("出口磁盘可用空间不足（剩余 {mib} MiB < 保留 1 GiB，已收 {written} 字节）——上传已中止（临时文件已清理）")
    } else if declared > 0 {
        format!("出口磁盘可用空间不足（剩余 {mib} MiB < 保留 1 GiB + 声明 {declared} 字节）——上传被拒")
    } else {
        format!("出口磁盘可用空间不足（剩余 {mib} MiB < 保留 1 GiB）——上传被拒")
    }
}

/// 可用空间取数缝（注入 = 测试；缺省 = 真 `statvfs`）。
type AvailFn = std::sync::Arc<dyn Fn(&Path) -> Option<u64> + Send + Sync>;

/// 上传水位探针（`avail` 可注入——测试缝；`None` = 真 `statvfs`）。
#[derive(Clone, Default)]
struct DiskWatermark {
    avail: Option<AvailFn>,
    /// fail-open 告警节流计数（首 3 + 每 100）。
    fail_count: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

impl DiskWatermark {
    #[cfg(test)]
    fn fixed(v: Option<u64>) -> Self {
        Self {
            avail: Some(std::sync::Arc::new(move |_| v)),
            fail_count: Default::default(),
        }
    }

    /// 取可用空间（含 fail-open 节流告警）。
    fn probe(&self, dir: &Path, logf: &crate::Logf) -> Option<u64> {
        let v = match &self.avail {
            Some(f) => f(dir),
            None => avail_bytes(dir),
        };
        if v.is_none() {
            let n = self.fail_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            if n <= 3 || n.is_multiple_of(100) {
                (logf)(&format!(
                    "⚠️ files: 磁盘可用空间检查失败（statvfs {}）——fail-open 放行上传（第 {n} 次）",
                    dir.display()
                ));
            }
        }
        v
    }
}

/// files 服务（以某目录为根；根 = 后端用户主目录）。
pub struct FilesServer {
    root: PathBuf,
    logf: crate::Logf,
    /// 并发在册闸（FIX-36 语义：满 16 条回 busy——R3-G3 实装；跨 accept 副本共享）。
    conns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    /// 上传磁盘水位探针（F3a；可注入）。
    watermark: DiskWatermark,
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
        Ok(Self {
            root: abs,
            logf,
            conns: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            watermark: DiskWatermark::default(),
        })
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
    /// accept 错误分类/退避/节流日志走共享件 `serve_stoppable_accepts`（F5：瞬态错误
    /// 不再摘服务；只 `Fatal` 才退工并由调用方记行）。
    pub fn serve_stoppable(&self, ln: UnixListener, stop: std::sync::Arc<std::sync::atomic::AtomicBool>) -> std::io::Result<()> {
        ln.set_nonblocking(true)?;
        let accept = || {
            ln.accept().map(|(conn, _)| {
                let _ = conn.set_nonblocking(false);
                conn
            })
        };
        serve_stoppable_accepts(accept, |conn| self.spawn_conn(conn), stop, &self.logf, "files")
    }

    /// accept 后派发：在册 <16 走正常服务（结束时 release）；满则 busy 拒入
    ///（Go sem 同义——先发 greeting、吞掉一条请求再回 busy，客户端拿到的仍是
    ///「有根目录的服务端拒绝了我」，不是连接层失败）。
    fn spawn_conn(&self, conn: UnixStream) {
        let Some(res) = ConnReservation::acquire(&self.conns) else {
            self.spawn_busy(conn);
            return;
        };
        let server = FilesServer {
            root: self.root.clone(),
            logf: self.logf.clone(),
            conns: std::sync::Arc::clone(&self.conns),
            watermark: self.watermark.clone(),
        };
        // 守卫随闭包 move：`Builder::spawn` 失败时闭包被 drop ⇒ 名额自动回收
        //（修前 `fetch_add` 后 `.ok()` 吞掉失败 = 永久泄漏，16 次后恒 busy）；
        // 会话线程 panic 时守卫同样兜底回收。
        if let Err(e) = std::thread::Builder::new().name("homeway-files".into()).spawn(move || {
            server.serve_conn(conn);
            drop(res);
        }) {
            (self.logf)(&format!("files: 会话线程起不来（{e}）——已在册名额已回收"));
        }
    }

    fn spawn_busy(&self, conn: UnixStream) {
        let server = FilesServer {
            root: self.root.clone(),
            logf: self.logf.clone(),
            conns: std::sync::Arc::clone(&self.conns),
            watermark: self.watermark.clone(),
        };
        if let Err(e) = std::thread::Builder::new().name("homeway-files-busy".into()).spawn(move || server.serve_busy(conn)) {
            (self.logf)(&format!("files: busy 会话线程起不来（{e}）——连接直接收线"));
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
        // 吞掉请求行（客户端按「问候 → 请求 → 响应」节拍走——直接关会让它误判传输层错）。
        // F3b：读出错/超限 → 先回错误响应再收线（对齐 Go `ServeConn` 的
        // `WriteLine(errorResponse(rerr))`，`server.go:151-155`；超限 = invalid_arg、
        // IO 类 = op_failed）。
        if let Err(e) = read_line(&mut reader) {
            let _ = write_line(&mut writer, &line_err_response(&e));
            (self.logf)(&format!("files: busy 路径请求行读取失败（{e}）——回错误响应后收线"));
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
            Err(e) => {
                // F3b：超限（累积中判负，对齐 Go `readLineLimited`）⇒ 回 invalid_arg
                //「请求行超过 65536 字节」再收线；IO 类错误同 Go `errorResponse` 回
                // `op_failed`（对端已断时写失败无害）。
                let _ = write_line(&mut writer, &line_err_response(&e));
                return;
            }
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

    /// 规整成「相对根的路径」：空/./ 恒 "."；拒 NUL/控制字符与 `..` 段；随后
    /// **逐分量复核**（`resolve_in_root`，对齐 Go `os.Root` 的三条符号链接规则——
    /// 逃逸类统一 not_found 语义，不泄漏根外信息）。
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
        if trimmed.is_empty() || trimmed == "." {
            return Ok(self.root.clone()); // 前导 / 剥离 = 根内相对（既有语义）
        }
        self.resolve_in_root(trimmed, p)
    }

    /// 逐分量解析（P0-4 单点收口——六动词共用；**不存在的叶子绝不直返候选**）。
    ///
    /// 规则（`docs/reviews/QE-design.md` §0.4 对 Go 1.24 `os.Root` 的实测）：
    /// - 分量是**符号链接**：目标为**绝对路径** ⇒ `not_found`（即便落在根内，Go 同拒）；
    ///   相对目标按语义替换该分量继续逐分量走，目标里的 `..` 按词法弹栈、**弹出根外 ⇒
    ///   `not_found`**；`read_link` 失败（悬空/竞态）⇒ `not_found`；链接链深度上限 8
    ///   （= Go `rootMaxSymlinks`，防循环）超限 ⇒ `not_found`；
    /// - 分量不存在（ENOENT）⇒ 解析在该点终止（Go/POSIX 同语义）：剩余队列含 `..`
    ///   一律 `not_found`（否则词法回升会拼回未复核尾部 = 逃逸）；否则把剩余普通分量
    ///   并回**已复核的基点**（绝不返回未经复核的原候选）；
    /// - 其它 lstat 错误 ⇒ 交 `map_io_err` 归类（分量中途是普通文件 ⇒ ENOTDIR ⇒ op_failed）。
    ///
    /// 残余（D5，登记不修）：复核与真正的创建/打开分离 = TOCTOU 窗口未收口（威胁模型 =
    /// 经隧道的设备；本机 FS 写者不在模型内，macOS 无 `openat2`）。
    #[allow(clippy::result_large_err)]
    fn resolve_in_root(&self, trimmed: &str, p: &str) -> Result<PathBuf, Response> {
        let not_found = || err_response(crate::files::CODE_NOT_FOUND, "路径不在根内");
        let mut base = self.root.clone();
        let mut queue: std::collections::VecDeque<std::ffi::OsString> = Path::new(trimmed)
            .components()
            .map(|c| c.as_os_str().to_os_string())
            .collect();
        let mut links = 0usize;
        while let Some(comp) = queue.pop_front() {
            if comp == "." {
                continue; // 词法规整（符号链接目标可带）
            }
            if comp == ".." {
                // 词法弹栈；**弹出根外 ⇒ 拒**（规则 b）
                if base == self.root {
                    return Err(not_found());
                }
                base.pop();
                continue;
            }
            let next = base.join(&comp);
            match std::fs::symlink_metadata(&next) {
                Ok(md) => {
                    if md.file_type().is_symlink() {
                        links += 1;
                        if links > RESOLVE_MAX_DEPTH {
                            return Err(not_found()); // 链接链/循环：深度上限
                        }
                        let Ok(target) = std::fs::read_link(&next) else {
                            return Err(not_found()); // 规则 c（悬空/竞态）
                        };
                        if target.is_absolute() {
                            return Err(not_found()); // 规则 a：绝对目标一律拒
                        }
                        // 目标分量按序插回队列头（相对链接解析基点 = 链接所在目录）
                        let tcomps: Vec<std::ffi::OsString> = target
                            .components()
                            .map(|c| c.as_os_str().to_os_string())
                            .collect();
                        for c in tcomps.into_iter().rev() {
                            queue.push_front(c);
                        }
                        continue;
                    }
                    base = next;
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // 该分量不存在 ⇒ **整条解析在这一点终止**（POSIX/Go 同语义：内核逐分量
                    // 解析，缺失分量即 ENOENT，绝不会先词法消掉后面的 `..`）。剩余队列里
                    // 一旦出现 `..`（只能来自符号链接目标），词法回升会把**未复核**的
                    // 尾部直接拼回（`link -> gone/../evil/leaf` + `evil -> 根外` 可逃逸）
                    // ⇒ 一律 not_found；否则按词法并回已复核基点（剩余分量均为普通名，
                    // 缺失分量仍在路径里 ⇒ 内核同样在它处失败）。
                    if queue.iter().any(|c| c == "..") {
                        return Err(not_found());
                    }
                    let mut out = base.clone();
                    out.push(&comp);
                    for c in queue {
                        if c == "." {
                            continue;
                        }
                        out.push(c);
                    }
                    return Ok(out);
                }
                Err(e) => return Err(Self::map_io_err("路径", p, e)),
            }
        }
        Ok(base)
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
        // F3a 起始门（磁盘水位——创建 .tierpart 之前）：剩余 < 保留量或「声明 + 保留量」
        // 不足即拒（op_failed + 可行动文案；词表不新增码）。
        self.check_upload_quota(path.parent().unwrap_or(&self.root), req.size, 0, false)?;
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
        let dir = part.parent().unwrap_or(&self.root);
        let mut next_check = UPLOAD_RECHECK_BYTES;
        loop {
            match read_frame(r) {
                Ok(Some(payload)) => {
                    if f.write_all(&payload).is_err() {
                        return Err(Self::map_io_err("write", &req.path, std::io::Error::other("写临时文件失败")));
                    }
                    total += payload.len() as u64;
                    // F3a 进行中门：每累计 8 MiB 复查可用空间（跌破保留量即中止——
                    // 走既有 Err(resp) 清理路径删 .tierpart，目标文件不变）。
                    if total >= next_check {
                        next_check = total + UPLOAD_RECHECK_BYTES;
                        self.check_upload_quota(dir, req.size, total, true)?;
                    }
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

    /// F3a 水位门（起始 + 进行中共用）：不通过 ⇒ `op_failed`（既有码）+ 可行动文案。
    #[allow(clippy::result_large_err)]
    fn check_upload_quota(&self, dir: &Path, declared: i64, written: u64, recheck: bool) -> Result<(), Response> {
        match quota_verdict(self.watermark.probe(dir, &self.logf), declared, written, recheck) {
            QuotaVerdict::Allow => Ok(()),
            QuotaVerdict::RejectStart { avail } => Err(err_response(
                crate::files::CODE_OP_FAILED,
                quota_msg(avail, declared, written, false),
            )),
            QuotaVerdict::AbortMidway { avail, written } => {
                let msg = quota_msg(avail, declared, written, true);
                (self.logf)(&format!("files: 上传中止——{msg}"));
                Err(err_response(crate::files::CODE_OP_FAILED, msg))
            }
        }
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

/// 请求行读取失败（F3b）：超限要回**逐字对齐 Go** 的 invalid_arg 文案，IO 类错误按
/// 调用面各自处置（正常路径静默收线；busy 路径回 invalid_arg 再收线）。
#[derive(Debug)]
enum LineErr {
    /// 累积中超过 `MAX_REQUEST_LINE`（判点与 Go `readLineLimited` 同：含 EOL 累计）。
    OverLimit,
    Io(std::io::Error),
}

impl std::fmt::Display for LineErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LineErr::OverLimit => write!(f, "请求行超过 {} 字节", crate::files::MAX_REQUEST_LINE),
            LineErr::Io(e) => write!(f, "{e}"),
        }
    }
}

/// 请求行错误 → 响应。超限 = Go `Errf(CodeInvalidArg, "请求行超过 %d 字节", max)` 逐字
/// 对齐；IO 类错误 = Go `errorResponse`（非 `*Error` ⇒ `op_failed` + `err.Error()`）。
fn line_err_response(e: &LineErr) -> Response {
    match e {
        LineErr::OverLimit => err_response(
            crate::files::CODE_INVALID_ARG,
            format!("请求行超过 {} 字节", crate::files::MAX_REQUEST_LINE),
        ),
        LineErr::Io(io) => err_response(crate::files::CODE_OP_FAILED, format!("读请求行失败：{io}")),
    }
}

/// 读一行请求（F3b：`MAX_REQUEST_LINE` **累积中判负**——`read_until` 无界累积换成
/// `fill_buf` 逐块推进，超限即返 `OverLimit`，对齐 Go `readLineLimited`）。
fn read_line(r: &mut BufReader<UnixStream>) -> Result<Vec<u8>, LineErr> {
    let mut buf: Vec<u8> = Vec::new();
    loop {
        let mut chunk: Vec<u8> = Vec::new();
        {
            let avail = match r.fill_buf() {
                Ok(b) => b,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(LineErr::Io(e)),
            };
            if avail.is_empty() {
                return Err(LineErr::Io(std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "流关闭")));
            }
            match avail.iter().position(|&b| b == b'\n') {
                Some(i) => chunk.extend_from_slice(&avail[..=i]),
                None => chunk.extend_from_slice(avail),
            }
        }
        let has_eol = matches!(chunk.last(), Some(b'\n'));
        r.consume(chunk.len());
        buf.extend_from_slice(&chunk);
        if buf.len() > crate::files::MAX_REQUEST_LINE {
            return Err(LineErr::OverLimit);
        }
        if has_eol {
            while matches!(buf.last(), Some(b'\n') | Some(b'\r')) {
                buf.pop();
            }
            return Ok(buf);
        }
    }
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

// ---------- accept 错误分类 / 退避 / 名额回滚（F5：files 与 speedtest 共用） ----------

/// accept 错误的处置分类（纯函数，可测）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AcceptAction {
    /// 非阻塞监听的空转（200ms 轮询，既有形态）。
    Retry,
    /// 瞬态资源/连接类错误：**不退出**，退避重试 + 节流日志。
    Backoff,
    /// 监听面真没了（bad fd / 非 socket / 参数非法）：记行并退工。
    Fatal,
}

/// 归类 accept 错误。`EMFILE/ENFILE/ENOBUFS/ENOMEM/ECONNABORTED/EPROTO/EINTR` 都是
/// **瞬态**——修前一次瞬时错误就永久摘掉该服务（socket 文件还在 ⇒ 客户端恒
/// `ECONNREFUSED` 直到重启出口）。`ENETDOWN/EHOSTUNREACH/EOPNOTSUPP` 等会归 `Fatal`：
/// UDS 上基本不可达（无路由/网卡语义），真出现按监听面异常退工更诚实。
/// 注：`EMFILE` 正是「线程 spawn 也会失败」的时刻 ⇒ 退避与名额回滚（守卫）是一对。
pub(crate) fn classify_accept_err(e: &std::io::Error) -> AcceptAction {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        return AcceptAction::Retry;
    }
    // kind 面（`io::Error::from(kind)` 形态没有 raw errno；EINTR 常被 std 内部重试，
    // 纳入 Backoff 是保险）
    if matches!(e.kind(), std::io::ErrorKind::Interrupted | std::io::ErrorKind::ConnectionAborted) {
        return AcceptAction::Backoff;
    }
    match e.raw_os_error() {
        Some(c)
            if c == libc::EMFILE
                || c == libc::ENFILE
                || c == libc::ENOBUFS
                || c == libc::ENOMEM
                || c == libc::ECONNABORTED
                || c == libc::EPROTO
                || c == libc::EINTR =>
        {
            AcceptAction::Backoff
        }
        _ => AcceptAction::Fatal,
    }
}

/// 可停 accept 循环（files / speedtest 共用单一实现——错误分类、退避、节流日志、
/// stop 优先全在这里；`accept` 可注入 ⇒ 「一次 `EMFILE` 后服务仍受理下一条连接」
/// 可测）。返回 `Err` 仅限 `Fatal`（调用方按退工记行）。
pub(crate) fn serve_stoppable_accepts<A, H>(
    mut accept: A,
    mut on_conn: H,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    logf: &crate::Logf,
    tag: &'static str,
) -> std::io::Result<()>
where
    A: FnMut() -> std::io::Result<UnixStream>,
    H: FnMut(UnixStream),
{
    use std::sync::atomic::Ordering as OD;
    let mut backoff = Duration::from_millis(200);
    let mut transient: u64 = 0;
    loop {
        if stop.load(OD::Relaxed) {
            return Ok(());
        }
        match accept() {
            Ok(conn) => {
                backoff = Duration::from_millis(200);
                transient = 0;
                on_conn(conn);
            }
            Err(e) => match classify_accept_err(&e) {
                AcceptAction::Retry => std::thread::sleep(Duration::from_millis(200)),
                AcceptAction::Backoff => {
                    transient += 1;
                    if transient <= 3 || transient.is_multiple_of(100) {
                        (logf)(&format!("{tag}: accept 瞬态错误（{e}）——退避重试（累计 {transient} 次）"));
                    }
                    std::thread::sleep(backoff);
                    backoff = (backoff * 2).min(Duration::from_secs(1));
                }
                AcceptAction::Fatal => {
                    (logf)(&format!("{tag}: accept 致命错误（{e}）——监听面异常，退工"));
                    return Err(e);
                }
            },
        }
    }
}

/// 在册名额守卫（F5：**先建后 spawn**——`Builder::spawn` 失败时闭包被 drop ⇒ 守卫
/// Drop 自动回收，无需 error-path 补丁；会话线程 panic 时同样兜底）。
struct ConnReservation {
    conns: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ConnReservation {
    /// 尝试占一个名额（满 = `None`）。
    fn acquire(conns: &std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Option<Self> {
        use std::sync::atomic::Ordering as OD;
        if conns.fetch_add(1, OD::AcqRel) < MAX_CONNS {
            Some(Self { conns: std::sync::Arc::clone(conns) })
        } else {
            conns.fetch_sub(1, OD::AcqRel);
            None
        }
    }
}

impl Drop for ConnReservation {
    fn drop(&mut self) {
        self.conns.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// UDS 监听（exit-service-uds：死/活判别 + chmod 0600；路径 **> `SUN_PATH_MAX`** 拒绝）。
///
/// **契约（Q-G F4.2）**：`dir` 必须是**本仓 state 布局持有的目录**（三个生产调用点
/// 都传 `serve_dir`：`engine.rs` 的 files/speedtest/term 三处）——函数在 bind 之前
/// 会把 `dir` 收紧 0700（失败**告警不阻断**），这是 UDS 无法原子 0600 的补偿面
/// （`bind()` 无 mode 参数，跨用户暴露面由目录权限关闭；同用户窗口为已知残余）。
pub fn listen_local_service(dir: &Path, name: &str, logf: &crate::Logf) -> std::io::Result<UnixListener> {
    let sock = dir.join(name);
    // 量法统一为**字节口径**（`as_os_str().len()`——旧 `display()` 是 lossy 字符串，
    // 与判据不同源；Q-G F4.3）。
    let s_len = sock.as_os_str().len();
    if s_len > crate::sysfd::SUN_PATH_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!(
                "路径超长（{s_len} 字节 > {}，sun_path 上限〔平台值〕）",
                crate::sysfd::SUN_PATH_MAX
            ),
        ));
    }
    // 活实例占用判别：拨得通 = 有人（报占用）；ENOENT/ECONNREFUSED = 死残留（可清）
    if let Ok(c) = UnixStream::connect(&sock) {
        drop(c);
        return Err(std::io::Error::new(std::io::ErrorKind::AddrInUse, "socket 已被另一个活实例占用（同 state 双实例？）"));
    }
    let _ = std::fs::remove_file(&sock);
    // 目录先 0700（bind 之前；失败告警不阻断——socket 0600 那层还兜着）
    if let Err(e) = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)) {
        logf(&format!(
            "⚠️ state 目录 {} 收紧 0700 失败（{e}）——socket 0600 仍是边界",
            dir.display()
        ));
    }
    let ln = UnixListener::bind(&sock)?;
    // chmod 0600：**失败告警**（旧形态静默 `let _ =`；Go `chmodTighten` 会告警）
    if let Err(e) = std::fs::set_permissions(&sock, std::fs::Permissions::from_mode(0o600)) {
        logf(&format!(
            "⚠️ {} chmod 0600 失败（{e}）—— 纵深加固未生效，state 目录权限仍是边界",
            sock.display()
        ));
    }
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
        let srv = FilesServer { root: dir.canonicalize().unwrap(), logf: noop_logf(), conns: Default::default(), watermark: Default::default() };
        srv.conns.store(MAX_CONNS, std::sync::atomic::Ordering::Release);

        let (a, b) = UnixStream::pair().unwrap();
        let s2 = FilesServer { root: srv.root.clone(), logf: noop_logf(), conns: std::sync::Arc::clone(&srv.conns), watermark: Default::default() };
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
        let srv = FilesServer { root: dir.canonicalize().unwrap(), logf: noop_logf(), conns: Default::default(), watermark: Default::default() };
        std::fs::write(dir.join("hello.txt"), b"hello-files").unwrap();

        // 一条命令 = 起 pair + 服务端线程；返回（读半, 写半）
        fn call(srv: &FilesServer) -> (BufReader<UnixStream>, UnixStream) {
            let (a, b) = UnixStream::pair().unwrap();
            let s2 = FilesServer { root: srv.root.clone(), logf: noop_logf(), conns: std::sync::Arc::clone(&srv.conns), watermark: Default::default() };
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

    // ---------- P0-4 沙箱负例（F1：逐分量复核） ----------

    fn mkroot(name: &str) -> (PathBuf, FilesServer, PathBuf) {
        let root = tmpdir(name);
        let outside = tmpdir(&format!("{name}-out"));
        let s2 = FilesServer::open(Some(&root), noop_logf()).unwrap();
        (root, s2, outside)
    }

    fn symlink(target: &Path, at: &Path) {
        std::os::unix::fs::symlink(target, at).unwrap();
    }

    /// ① `link -> <根外>` + 不存在叶子：整个候选从未被解析 ⇒ 必须 not_found（**修前红**：
    /// canonicalize ENOENT ⇒ `Ok(candidate)` 原样放行）。
    #[test]
    fn rel_path_escape_symlink_with_missing_leaf_rejected() {
        let (root, srv, outside) = mkroot("esc-leaf");
        symlink(&outside, &root.join("link"));
        let e = srv.rel_path("link/newfile").expect_err("根外落文件必须被拒");
        assert_eq!(e.code, crate::files::CODE_NOT_FOUND);
        let e = srv.rel_path("link").expect_err("链接本身指向根外也必须拒");
        assert_eq!(e.code, crate::files::CODE_NOT_FOUND);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// ④ 绝对目标 + 落在根内：Go `os.Root` 一律拒（§0.4 实测 `path escapes from parent`）
    /// ⇒ `not_found`（**修前红**：canonicalize 成功且 starts_with(root) ⇒ 放行）。
    #[test]
    fn rel_path_absolute_target_even_inside_root_rejected() {
        let (root, srv, outside) = mkroot("esc-abs");
        std::fs::create_dir(root.join("sub")).unwrap();
        symlink(&root.join("sub"), &root.join("l_abs_in"));
        let e = srv.rel_path("l_abs_in/newleaf").expect_err("绝对目标链接一律拒（即便根内）");
        assert_eq!(e.code, crate::files::CODE_NOT_FOUND);
        let e = srv.rel_path("l_abs_in").expect_err("绝对目标链接本身也拒");
        assert_eq!(e.code, crate::files::CODE_NOT_FOUND);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// **H1（代码门）**：链接目标尾部含 `..` 且**缺失分量在前**（`l -> gone/../evil/leaf`）
    /// ⇒ 一律 not_found——修前实现会词法回升到根、把**未复核**的 `evil`（指向根外的
    /// 符号链接）直接拼回 ⇒ 根外落文件（真实 crate 已复现）。
    #[test]
    fn rel_path_dotdot_after_missing_component_rejected() {
        let (root, srv, outside) = mkroot("esc-dotdot");
        std::os::unix::fs::symlink(&outside, root.join("evil")).unwrap();
        symlink(Path::new("gone/../evil/leaf"), &root.join("l"));
        assert_eq!(srv.rel_path("l").unwrap_err().code, crate::files::CODE_NOT_FOUND);
        // 端到端：六动词打 `l` 一律失败，且根外零新增
        std::fs::write(outside.join("sentinel"), b"keep").unwrap();
        let before: Vec<String> = std::fs::read_dir(&outside)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        for req in [
            serde_json::json!({"op":"write","path":"l","size":3}),
            serde_json::json!({"op":"stat","path":"l"}),
            serde_json::json!({"op":"read","path":"l"}),
            serde_json::json!({"op":"list","path":"l"}),
        ] {
            let (a, b) = UnixStream::pair().unwrap();
            let s2 = FilesServer {
                root: srv.root.clone(),
                logf: noop_logf(),
                conns: Default::default(),
                watermark: Default::default(),
            };
            std::thread::spawn(move || s2.serve_conn(a));
            let mut w = b.try_clone().unwrap();
            let mut r = BufReader::new(b);
            let _ = read_line(&mut r); // 问候
            writeln!(w, "{req}").unwrap();
            let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
            assert_eq!(resp["ok"], serde_json::json!(false), "{req} 必须被拒：{resp}");
            assert_eq!(resp["code"], serde_json::json!("not_found"), "{req}：{resp}");
        }
        let after: Vec<String> = std::fs::read_dir(&outside)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(before, after, "根外不得新增：{before:?} → {after:?}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// ② 悬空链接（相对目标越界/或目标不存在）⇒ not_found（规则 c；Go 实测同级）。
    #[test]
    fn rel_path_dangling_symlink_rejected() {
        let (root, srv, outside) = mkroot("esc-dangle");
        symlink(&outside.join("notyet"), &root.join("l_dangle_out")); // 绝对悬空
        assert_eq!(srv.rel_path("l_dangle_out/new").unwrap_err().code, crate::files::CODE_NOT_FOUND);
        symlink(Path::new("../outside-dangling"), &root.join("l_rel_out")); // 相对越界
        assert_eq!(srv.rel_path("l_rel_out/x").unwrap_err().code, crate::files::CODE_NOT_FOUND);
        // 相对根内悬空（Go 实测回 ENOENT ⇒ 下游 not_found）：复核放行但叶子不存在
        std::fs::create_dir(root.join("sub")).unwrap();
        symlink(Path::new("sub/notyet"), &root.join("l_rel_dangle"));
        assert_eq!(srv.rel_path("l_rel_dangle/x").unwrap(), srv.root_dir().join("sub").join("notyet/x"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// ③ 相对根内链接（目标存在）+ 不存在叶子 ⇒ 放行且落在根内目标下。
    #[test]
    fn rel_path_in_root_relative_symlink_resolves() {
        let (root, srv, outside) = mkroot("esc-inrel");
        std::fs::create_dir(root.join("sub")).unwrap();
        symlink(Path::new("sub"), &root.join("l_rel_in"));
        assert_eq!(srv.rel_path("l_rel_in/newfile").unwrap(), srv.root_dir().join("sub").join("newfile"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// ⑤ 链接链（`l1 -> l2 -> <根外>`）⇒ not_found；⑥ 循环链接（深度上限）⇒ not_found。
    #[test]
    fn rel_path_link_chain_and_cycle_rejected() {
        let (root, srv, outside) = mkroot("esc-chain");
        symlink(Path::new("l2"), &root.join("l1"));
        symlink(&outside, &root.join("l2"));
        assert_eq!(srv.rel_path("l1/newfile").unwrap_err().code, crate::files::CODE_NOT_FOUND);
        // 循环：a -> b -> a（深度上限 8 = Go `rootMaxSymlinks`）
        symlink(Path::new("b"), &root.join("a"));
        symlink(Path::new("a"), &root.join("b"));
        assert_eq!(srv.rel_path("a/x").unwrap_err().code, crate::files::CODE_NOT_FOUND);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// ⑦ 分量中途是普通文件（`file/leaf`）⇒ `op_failed`（ENOTDIR，非 not_found）。
    #[test]
    fn rel_path_component_is_regular_file_maps_op_failed() {
        let (root, srv, outside) = mkroot("esc-notdir");
        std::fs::write(root.join("file"), b"x").unwrap();
        assert_eq!(srv.rel_path("file/leaf").unwrap_err().code, crate::files::CODE_OP_FAILED);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// ⑧ 深层不存在 ⇒ 放行（词法拼回，绝不返回未经复核的原候选）；⑨ 前导 `/` 视为根内相对。
    #[test]
    fn rel_path_missing_depth_ok_and_leading_slash_stays_in_root() {
        let (root, srv, outside) = mkroot("esc-miss");
        let p = srv.rel_path("x/y/z").unwrap();
        assert!(p.starts_with(srv.root_dir()), "{p:?}");
        assert!(p.ends_with("x/y/z"), "{p:?}");
        let p = srv.rel_path("/etc/passwd").unwrap();
        assert!(p.starts_with(srv.root_dir()), "前导 / 一律视为根内相对：{p:?}");
        assert!(p.ends_with("etc/passwd"), "{p:?}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    /// **端到端负例（审计点名）**：真 UDS 对 + 六动词打 `link/<新叶>` ⇒ 全 `ok=false`，
    /// 且根外目录内容**一字未增**（修前 write/mkdir 会在根外落文件）。
    #[test]
    fn symlink_escape_no_write_outside() {
        let (root, srv, outside) = mkroot("esc-e2e");
        std::fs::write(outside.join("sentinel"), b"keep").unwrap();
        symlink(&outside, &root.join("link"));
        let before: Vec<String> = std::fs::read_dir(&outside)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();

        fn call(srv: &FilesServer, req: serde_json::Value) -> serde_json::Value {
            let (a, b) = UnixStream::pair().unwrap();
            let s2 = FilesServer { root: srv.root.clone(), logf: noop_logf(), conns: Default::default(), watermark: Default::default() };
            std::thread::spawn(move || s2.serve_conn(a));
            let mut w = b.try_clone().unwrap();
            let mut r = BufReader::new(b);
            let _ = read_line(&mut r); // 问候
            writeln!(w, "{req}").unwrap();
            serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap()
        }

        // write：修前 = 根外真落文件（红）
        let resp = call(&srv, serde_json::json!({"op":"write","path":"link/pwn.bin","size":3}));
        if resp["ok"] == serde_json::json!(true) {
            // 备好 .tierpart ⇒ 发帧提交（把"写出根外"做实）
            // 这里协议上仍会走到写文件；直接断言失败更严：ok 必须 false
        }
        assert_eq!(resp["ok"], serde_json::json!(false), "write 越界必须被拒：{resp}");
        assert_eq!(resp["code"], serde_json::json!("not_found"), "{resp}");

        for req in [
            serde_json::json!({"op":"mkdir","path":"link/newdir"}),
            serde_json::json!({"op":"stat","path":"link/x"}),
            serde_json::json!({"op":"read","path":"link/x"}),
            serde_json::json!({"op":"download","path":"link/x"}),
            serde_json::json!({"op":"list","path":"link/x"}),
        ] {
            let resp = call(&srv, req.clone());
            assert_eq!(resp["ok"], serde_json::json!(false), "{req} 越界必须被拒：{resp}");
            assert_eq!(resp["code"], serde_json::json!("not_found"), "{req}：{resp}");
        }
        let after: Vec<String> = std::fs::read_dir(&outside)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(before, after, "根外目录内容一字不得增：{before:?} → {after:?}");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&outside);
    }

    // ---------- F3a 上传磁盘水位 ----------

    /// 三态纯函数 + fail-open。
    #[test]
    fn quota_verdict_three_states() {
        let reserve = UPLOAD_RESERVE_BYTES;
        // 放行（含声明大小快路径）
        assert_eq!(quota_verdict(Some(reserve + 1), 0, 0, false), QuotaVerdict::Allow);
        assert_eq!(quota_verdict(Some(reserve + 100), 100, 0, false), QuotaVerdict::Allow);
        // 起始拒：剩余 < 保留量 / 声明 + 保留量 > 剩余
        assert_eq!(quota_verdict(Some(reserve - 1), 0, 0, false), QuotaVerdict::RejectStart { avail: reserve - 1 });
        assert_eq!(quota_verdict(Some(reserve + 1), 2, 0, false), QuotaVerdict::RejectStart { avail: reserve + 1 });
        // 中途拒（已写入字节数带回）
        assert_eq!(
            quota_verdict(Some(reserve - 1), 0, 12_345, true),
            QuotaVerdict::AbortMidway { avail: reserve - 1, written: 12_345 }
        );
        assert_eq!(quota_verdict(Some(reserve), 0, 12_345, true), QuotaVerdict::Allow);
        // statvfs 失败 ⇒ fail-open（起始与中途都放行）
        assert_eq!(quota_verdict(None, 1 << 40, 0, false), QuotaVerdict::Allow);
        assert_eq!(quota_verdict(None, 0, 1 << 30, true), QuotaVerdict::Allow);
    }

    /// 真 `statvfs` 冒烟（临时目录应可取到可用空间）。
    #[test]
    fn avail_bytes_smoke() {
        let v = avail_bytes(&std::env::temp_dir());
        assert!(v.is_some(), "statvfs 在临时目录应可用");
        assert!(v.unwrap() > 0);
    }

    /// 起始门端到端（注入 avail=0）：`write` 回 op_failed、**无 .tierpart 残留**、目标未变；
    /// 中途门（注入值先足量后跌破）⇒ 中止 + 清理。
    #[test]
    fn upload_rejected_when_low_disk() {
        let dir = tmpdir("quota-low");
        let mut srv = FilesServer::open(Some(&dir), noop_logf()).unwrap();
        srv.watermark = DiskWatermark::fixed(Some(0));
        let s2 = FilesServer {
            root: srv.root.clone(),
            logf: noop_logf(),
            conns: std::sync::Arc::clone(&srv.conns),
            watermark: srv.watermark.clone(),
        };
        let (a, b) = UnixStream::pair().unwrap();
        std::thread::spawn(move || s2.serve_conn(a));
        let mut w = b.try_clone().unwrap();
        let mut r = BufReader::new(b);
        let _ = read_line(&mut r).unwrap(); // 问候
        writeln!(w, "{}", serde_json::json!({"op":"write","path":"big.bin","size":1048576})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(false), "{resp}");
        assert_eq!(resp["code"], serde_json::json!("op_failed"), "拒绝码用既有词表：{resp}");
        assert!(resp["msg"].as_str().unwrap().contains("出口磁盘可用空间不足"), "{resp}");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tierpart."))
            .collect();
        assert!(leftovers.is_empty(), "拒时不得留 .tierpart：{leftovers:?}");
        assert!(!dir.join("big.bin").exists(), "目标文件不得出现");
        // 中途门（注入值按每次 probe 变化：先足量、后跌破）——每 8MiB 复查
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let c2 = std::sync::Arc::clone(&calls);
        let mut srv2 = FilesServer::open(Some(&dir), noop_logf()).unwrap();
        srv2.watermark = DiskWatermark {
            avail: Some(std::sync::Arc::new(move |_| {
                let n = c2.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                if n == 0 {
                    Some(UPLOAD_RESERVE_BYTES + (1 << 20))
                } else {
                    Some(UPLOAD_RESERVE_BYTES - 1)
                }
            })),
            fail_count: Default::default(),
        };
        let s3 = FilesServer {
            root: srv2.root.clone(),
            logf: noop_logf(),
            conns: Default::default(),
            watermark: srv2.watermark.clone(),
        };
        let (a, b) = UnixStream::pair().unwrap();
        std::thread::spawn(move || s3.serve_conn(a));
        let mut w = b.try_clone().unwrap();
        let mut r = BufReader::new(b);
        let _ = read_line(&mut r).unwrap();
        writeln!(w, "{}", serde_json::json!({"op":"write","path":"mid.bin","size":0})).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(true), "起始门放行：{resp}");
        // 灌 8 MiB（= 复查点；帧载荷 ≤ MAX_CHUNK）⇒ 中途门中止；服务端在第 32 帧
        // 读完后即停读，客户端发完即止（不写第 33 帧——避免与已收线的对端竞写）。
        let block = vec![7u8; MAX_CHUNK];
        for _ in 0..(UPLOAD_RECHECK_BYTES / MAX_CHUNK as u64) {
            write_frame(&mut w, &block).unwrap();
        }
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(false), "中途门应中止：{resp}");
        assert_eq!(resp["code"], serde_json::json!("op_failed"), "{resp}");
        let leftovers: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".tierpart."))
            .collect();
        assert!(leftovers.is_empty(), "中止后不得留 .tierpart：{leftovers:?}");
        assert!(!dir.join("mid.bin").exists(), "中止后目标文件不得出现");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- F3b 服务端请求行上限（对齐 Go） ----------

    /// 66KB（> 65536）+ 换行 ⇒ `invalid_arg`（msg **逐字**对齐 Go「请求行超过 65536 字节」）
    /// 并收线；60KB 正常请求仍通。
    #[test]
    fn request_line_over_64k_rejected() {
        let dir = tmpdir("reqline");
        let srv = FilesServer::open(Some(&dir), noop_logf()).unwrap();
        let s2 = FilesServer {
            root: srv.root.clone(),
            logf: noop_logf(),
            conns: std::sync::Arc::clone(&srv.conns),
            watermark: Default::default(),
        };
        let (a, b) = UnixStream::pair().unwrap();
        std::thread::spawn(move || s2.serve_conn(a));
        let mut w = b.try_clone().unwrap();
        let mut r = BufReader::new(b);
        let _ = read_line(&mut r).unwrap(); // 问候
        // 66_000 字节（> 65536）+ 换行：累积中判负（不必等换行）
        let mut line = vec![b'x'; 66_000];
        line.push(b'\n');
        w.write_all(&line).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(false), "{resp}");
        assert_eq!(resp["code"], serde_json::json!("invalid_arg"), "{resp}");
        assert_eq!(resp["msg"], serde_json::json!("请求行超过 65536 字节"), "文案逐字对齐 Go：{resp}");
        // 收线：后续读应见 EOF（响应已到）
        let mut tail = Vec::new();
        let _ = r.read_to_end(&mut tail);
        assert!(tail.is_empty(), "超限后应收线（尾随字节 {tail:?}）");
        drop((r, w));

        // 60KB 合法请求（JSON 尾部空白合法）仍通
        let (a, b) = UnixStream::pair().unwrap();
        let s3 = FilesServer {
            root: srv.root.clone(),
            logf: noop_logf(),
            conns: std::sync::Arc::clone(&srv.conns),
            watermark: Default::default(),
        };
        std::thread::spawn(move || s3.serve_conn(a));
        let mut w = b.try_clone().unwrap();
        let mut r = BufReader::new(b);
        let _ = read_line(&mut r).unwrap();
        let mut req = serde_json::json!({"op":"list","path":""}).to_string();
        req.push_str(&" ".repeat(60_000));
        req.push('\n');
        w.write_all(req.as_bytes()).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["ok"], serde_json::json!(true), "60KB 请求仍通：{resp}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// busy 路径同口径：超长行 ⇒ 回 `invalid_arg`（**非** busy）再收线。
    #[test]
    fn busy_path_over_64k_line_replies_invalid_arg() {
        let dir = tmpdir("reqline-busy");
        let srv = FilesServer::open(Some(&dir), noop_logf()).unwrap();
        srv.conns.store(MAX_CONNS, std::sync::atomic::Ordering::Release);
        let s2 = FilesServer {
            root: srv.root.clone(),
            logf: noop_logf(),
            conns: std::sync::Arc::clone(&srv.conns),
            watermark: Default::default(),
        };
        let (a, b) = UnixStream::pair().unwrap();
        std::thread::spawn(move || s2.serve_busy(a));
        let mut w = b.try_clone().unwrap();
        let mut r = BufReader::new(b);
        let g: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(g["ok"], serde_json::json!(true), "问候照发");
        let mut line = vec![b'y'; 66_000];
        line.push(b'\n');
        w.write_all(&line).unwrap();
        let resp: serde_json::Value = serde_json::from_slice(&read_line(&mut r).unwrap()).unwrap();
        assert_eq!(resp["code"], serde_json::json!("invalid_arg"), "busy 路径也回 invalid_arg：{resp}");
        assert_eq!(resp["msg"], serde_json::json!("请求行超过 65536 字节"), "{resp}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- F5 accept 分类 / 退避 / 名额回滚 ----------

    /// 归类逐案（WouldBlock / EMFILE / ENFILE / ENOBUFS / ENOMEM / ECONNABORTED /
    /// EPROTO / EINTR / EBADF / InvalidInput）。
    #[test]
    fn classify_accept_err_cases() {
        use std::io::Error;
        assert_eq!(classify_accept_err(&Error::from(std::io::ErrorKind::WouldBlock)), AcceptAction::Retry);
        for code in [libc::EMFILE, libc::ENFILE, libc::ENOBUFS, libc::ENOMEM, libc::ECONNABORTED, libc::EPROTO, libc::EINTR] {
            assert_eq!(
                classify_accept_err(&Error::from_raw_os_error(code)),
                AcceptAction::Backoff,
                "errno {code} 应退避不退出"
            );
        }
        assert_eq!(classify_accept_err(&Error::from_raw_os_error(libc::EBADF)), AcceptAction::Fatal);
        assert_eq!(classify_accept_err(&Error::from(std::io::ErrorKind::InvalidInput)), AcceptAction::Fatal);
        assert_eq!(classify_accept_err(&Error::from(std::io::ErrorKind::Interrupted)), AcceptAction::Backoff);
    }

    /// 注入脚本化 accept：`EMFILE`×2 → 一条真连接 ⇒ 循环未退出、连接被受理、退避发生过
    /// （**修前红**：现状一次错误即 `return Ok(())` 摘服务）。
    #[test]
    fn serve_stoppable_accepts_survives_transient_error() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l2 = std::sync::Arc::clone(&logs);
        let logf: crate::Logf = std::sync::Arc::new(move |s: &str| {
            l2.lock().unwrap().push(s.to_owned());
        });
        let (tx, rx) = std::sync::mpsc::channel::<UnixStream>();
        let mut script = 0usize;
        let (keep_a, keep_b) = UnixStream::pair().unwrap();
        let mut keep = Some(keep_a);
        let stop2 = std::sync::Arc::clone(&stop);
        let t0 = std::time::Instant::now();
        let h = std::thread::spawn(move || {
            let accept = || {
                script += 1;
                match script {
                    1 | 2 => Err(std::io::Error::from_raw_os_error(libc::EMFILE)),
                    3 => Ok(keep.take().expect("连接只交付一次")),
                    _ => Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
                }
            };
            serve_stoppable_accepts(
                accept,
                move |c| {
                    let _ = tx.send(c);
                },
                stop2,
                &logf,
                "files",
            )
        });
        let got = rx.recv_timeout(Duration::from_secs(3)).expect("瞬态错误后仍须受理下一条连接");
        drop(got);
        drop(keep_b);
        // 停止位优先：置位后循环退出
        stop.store(true, std::sync::atomic::Ordering::Release);
        let r = h.join().unwrap();
        assert!(r.is_ok(), "退工只在 Fatal：{r:?}");
        assert!(t0.elapsed() < Duration::from_secs(5), "退避应在界内（{:?}）", t0.elapsed());
        let logs = logs.lock().unwrap();
        assert!(logs.iter().any(|l| l.contains("accept 瞬态错误")), "应有节流退避日志：{logs:?}");
        assert!(logs.len() <= 2, "节流（首 3 次内）：{logs:?}");
    }

    /// 停止位：置 stop 后循环在 200ms 轮询粒度内返回。
    #[test]
    fn serve_stoppable_accepts_honors_stop() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let logf: crate::Logf = std::sync::Arc::new(|_| {});
        let t0 = std::time::Instant::now();
        let r = serve_stoppable_accepts(
            || Err(std::io::Error::from(std::io::ErrorKind::WouldBlock)),
            |_| {},
            stop,
            &logf,
            "files",
        );
        assert!(r.is_ok());
        assert!(t0.elapsed() < Duration::from_millis(200), "stop 优先于 accept：{:?}", t0.elapsed());
    }

    /// Fatal ⇒ 退工并返回 Err（EBADF 形态）。
    #[test]
    fn serve_stoppable_accepts_fatal_returns_err() {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let logs = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let l2 = std::sync::Arc::clone(&logs);
        let logf: crate::Logf = std::sync::Arc::new(move |s: &str| {
            l2.lock().unwrap().push(s.to_owned());
        });
        let r = serve_stoppable_accepts(
            || Err(std::io::Error::from_raw_os_error(libc::EBADF)),
            |_| {},
            stop,
            &logf,
            "files",
        );
        assert_eq!(r.unwrap_err().raw_os_error(), Some(libc::EBADF));
        assert!(logs.lock().unwrap().iter().any(|l| l.contains("accept 致命错误")), "Fatal 必须记行");
    }

    /// 名额守卫：构造即占、drop 即回（等价 spawn 失败路径）；满员拒绝不越界。
    #[test]
    fn conn_reservation_rolls_back_on_drop() {
        let conns = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        {
            let _r = ConnReservation::acquire(&conns).expect("首个名额");
            assert_eq!(conns.load(std::sync::atomic::Ordering::Acquire), 1);
            // 模拟 spawn 失败：守卫被 drop（闭包被丢弃）⇒ 名额回滚
        }
        assert_eq!(conns.load(std::sync::atomic::Ordering::Acquire), 0, "drop 后名额归零");
        // 满员：不越界
        conns.store(MAX_CONNS, std::sync::atomic::Ordering::Release);
        assert!(ConnReservation::acquire(&conns).is_none());
        assert_eq!(conns.load(std::sync::atomic::Ordering::Acquire), MAX_CONNS);
        conns.store(0, std::sync::atomic::Ordering::Release);
    }

    /// UDS 监听面：bind + chmod 0600 + 活实例占用判别 + **目录先 0700**（Q-G F4）。
    /// **登记**：chmod 失败告警路径不可确定构造（chmod 只需属主身份、不需要目录写
    /// 权限 ⇒ 只读目录也成功；非属主构造在本仓测试不可行）——按设计 §2-F4 测试计划
    /// 如实降级为「代码面复核」。
    #[test]
    fn uds_listen_and_occupancy() {
        let dir = tmpdir("uds");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let logf = noop_logf();
        let ln = listen_local_service(&dir, "files.sock", &logf).unwrap();
        let sock = dir.join("files.sock");
        assert!(sock.exists());
        let mode = sock.metadata().unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "socket 权限应收紧 0600");
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "bind 之前目录已收紧 0700（F4：目录面是 UDS 的补偿边界）"
        );
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
