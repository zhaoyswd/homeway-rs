//! 文件管理派发面（语义真源 `baseline:clientcore/cmd/clientcore/app_files.go` +
//! `app_files_native.go`——files 原生协议，wg-native-stack 4.2）。
//!
//! 出入口只有 `files_call` 一个：JSON 进、JSON 出，操作名分发——
//! connect/close/list/stat/mkdir/readText/readImage/download/upload/transfers/cancel。
//! 会话制：connect 拿 handle，其它操作带 handle；页面退出调 close。会话表 cap=4、
//! 驱逐最久未用（防「页面忘了 close」漏会话），淘汰/关闭/取消都要打断在跑传输（FIX-42）。
//!
//! 传输承载 = 桥 UDS（`<filesDir>/bridge/files.sock` → 隧道 → 出口 7802）；
//! auth/sock 来自状态 JSON 的 bridgeAuth/bridgeFilesSock。线协议与 `crate::files`
//! 同源（鉴权 blob + 问候 JSON 行 + 命令行 + 4B BE 帧）；`crate::files` 的动词面绑定
//! 引擎 Session（CLI 形态），本模块自带 UDS 流——两个消费方两种承载，协议件
//! （Entry/前缀/码表）复用不重抄。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use crate::files::{decode_prefix, Prefix, MAX_REQUEST_LINE, CODE_STREAM_OPEN};

use super::term_op::write_auth;

/// 一次性命令的期限（Go filesConnectTimeout 同值；读预算落到 conn deadline——
/// 对端「连着但不说话」时不得无限挂死）。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// 同时存在的文件会话上限（超限驱逐最久未用）。
const MAX_SESSIONS: usize = 4;
/// 文本/图片内联读的默认上限（Go pkg/files 同值）。
const DEFAULT_TEXT_MAX_BYTES: i64 = 512 * 1024;
const DEFAULT_IMAGE_MAX_BYTES: i64 = 8 * 1024 * 1024;
/// 传输开场警戒：第一个进度回调前断读收口（Go transferOpenBudget 同源）。
const TRANSFER_OPEN_BUDGET: Duration = CONNECT_TIMEOUT;

/// files 桥层归一码词表（contract-ledger 台账族⑥——与 App 的 FilesRules.ets 对账、
/// 只增不改；与 pkg/files 协议码〔族④〕是两个独立冻结空间，值交集之外的同名仅为
/// 透传巧合）。
pub mod code {
    pub const BRIDGE_DOWN: &str = "bridge_down";
    pub const BRIDGE_AUTH: &str = "bridge_auth";
    pub const NO_SESSION: &str = "no_session";
    pub const INVALID_ARG: &str = "invalid_arg";
    pub const OP_FAILED: &str = "op_failed";
    pub const BUSY: &str = "busy";
    pub const MARSHAL: &str = "marshal";
}

/// filesError 的 Rust 面（稳定码 + 文案）。
#[derive(Debug, Clone)]
pub struct FilesOpError {
    pub code: String,
    pub msg: String,
}

impl FilesOpError {
    fn new(code: &str, msg: impl Into<String>) -> Self {
        FilesOpError { code: code.to_owned(), msg: msg.into() }
    }
}

// ---------------------------------------------------------------------------
// 桥 UDS 流（线协议 = crate::files 同源：鉴权 blob + 问候行 + 命令行 + 4B BE 帧）
// ---------------------------------------------------------------------------

/// 一条桥上的命令流（连上 + 已鉴权）。
struct BridgeStream {
    conn: UnixStream,
    buf: Vec<u8>,
}

impl BridgeStream {
    /// 拨桥 + 鉴权 + 读问候帧（**每命令一条流，每条流都以问候开场**——Go Client.Open
    /// 同形：连接后服务端立即送问候行，无请求）。问候失败 = `stream_open`（族④码，
    /// 请求未送达、可安全重放；connect 路径透传、操作路径归一 bridge_down）。
    fn open(sock: &str, auth_hex: &str, budget: Duration) -> Result<(Self, String, i64), FilesOpError> {
        if sock.is_empty() {
            return Err(FilesOpError::new(
                code::BRIDGE_DOWN,
                "文件通道暂时不可用（桥未就绪：VPN 未连接且服务会话未就绪，或正在恢复）",
            ));
        }
        let mut conn = UnixStream::connect(sock)
            .map_err(|e| FilesOpError::new(code::BRIDGE_DOWN, format!("文件通道暂时不可用（桥未就绪或正在恢复）：{e}")))?;
        conn.set_read_timeout(Some(budget)).ok();
        conn.set_write_timeout(Some(budget)).ok();
        write_auth(&mut conn, auth_hex)
            .map_err(|e| FilesOpError::new(code::BRIDGE_AUTH, format!("文件通道鉴权失败：{e}")))?;
        let mut s = BridgeStream { conn, buf: Vec::with_capacity(16 * 1024) };
        let line = s
            .read_line_capped(false)
            .map_err(|e| FilesOpError::new(CODE_STREAM_OPEN, format!("读问候帧失败：{}", e.msg)))?;
        let g: Value = serde_json::from_slice(&line)
            .map_err(|e| FilesOpError::new(CODE_STREAM_OPEN, format!("问候帧不是 JSON：{e}")))?;
        if !g.get("ok").and_then(Value::as_bool).unwrap_or(false) {
            return Err(FilesOpError::new(CODE_STREAM_OPEN, "问候帧失败"));
        }
        let root = g.get("root").and_then(Value::as_str).unwrap_or("").to_owned();
        let ver = g.get("ver").and_then(Value::as_i64).unwrap_or(0);
        Ok((s, root, ver))
    }

    fn read_some(&mut self) -> Result<Vec<u8>, FilesOpError> {
        let mut tmp = [0u8; 16 * 1024];
        let n = self.conn.read(&mut tmp).map_err(op_failed_io)?;
        Ok(tmp[..n].to_vec())
    }

    /// 读一行（去 EOL）。上限只约束**入向请求行**（镜像 crate::files 的请求面上限）；
    /// 响应行（问候/回执/内联读结果）不设 64KB 上限——Go 客户端 bufio.ReadBytes 无
    /// 上限、服务端内联上限 16MB，App 默认 512KB 的 readText 会超 64KB（评审 r1-C2）。
    /// `cap_request`：是否套 64KB 请求面上限（响应行 false）。
    fn read_line_capped(&mut self, cap_request: bool) -> Result<Vec<u8>, FilesOpError> {
        loop {
            if let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
                let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
                while matches!(line.last(), Some(b'\n') | Some(b'\r')) {
                    line.pop();
                }
                if cap_request && line.len() > MAX_REQUEST_LINE {
                    return Err(FilesOpError::new(code::OP_FAILED, "行超过 64KB 上限"));
                }
                return Ok(line);
            }
            if cap_request && self.buf.len() > MAX_REQUEST_LINE {
                return Err(FilesOpError::new(code::OP_FAILED, "行超过 64KB 上限"));
            }
            let chunk = self.read_some()?;
            if chunk.is_empty() {
                return Err(FilesOpError::new(code::OP_FAILED, "流已到尾（EOF）"));
            }
            self.buf.extend_from_slice(&chunk);
        }
    }

    fn write_all(&mut self, data: &[u8]) -> Result<(), FilesOpError> {
        self.conn.write_all(data).map_err(op_failed_io)
    }

    /// 发命令行并读响应行（响应行不设上限——见 read_line_capped）。
    fn call(&mut self, line: &str) -> Result<Value, FilesOpError> {
        self.write_all(line.as_bytes())?;
        self.write_all(b"\n")?;
        let resp = self.read_line_capped(false)?;
        serde_json::from_slice(&resp)
            .map_err(|e| FilesOpError::new(code::OP_FAILED, format!("响应行不是 JSON：{e}")))
    }

    /// 读一个 4B 前缀帧（None = len=0 终止帧）。
    fn read_frame(&mut self) -> Result<Option<Vec<u8>>, FilesOpError> {
        while self.buf.len() < 4 {
            let chunk = self.read_some()?;
            if chunk.is_empty() {
                return Err(FilesOpError::new(code::OP_FAILED, "帧前缀未到齐（EOF）"));
            }
            self.buf.extend_from_slice(&chunk);
        }
        match decode_prefix(&self.buf).map_err(|e| FilesOpError::new(code::OP_FAILED, e.to_string()))? {
            Prefix::Terminated => {
                self.buf.drain(..4);
                Ok(None)
            }
            Prefix::Frame { len } => {
                while self.buf.len() < 4 + len {
                    let chunk = self.read_some()?;
                    if chunk.is_empty() {
                        return Err(FilesOpError::new(code::OP_FAILED, "帧载荷未到齐（EOF）"));
                    }
                    self.buf.extend_from_slice(&chunk);
                }
                let payload: Vec<u8> = self.buf.drain(..4 + len).skip(4).collect();
                Ok(Some(payload))
            }
        }
    }

    /// 写一个 4B 前缀帧。
    fn write_frame(&mut self, payload: &[u8]) -> Result<(), FilesOpError> {
        let mut out = Vec::with_capacity(4 + payload.len());
        out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        out.extend_from_slice(payload);
        self.write_all(&out)
    }

    /// 断 I/O（取消路径：对 dup 的 fd shutdown，阻塞中的读写立即以错误返回）。
    fn cut(&self) {
        if let Ok(c) = self.conn.try_clone() {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
    }
}

fn op_failed_io(e: std::io::Error) -> FilesOpError {
    FilesOpError::new(code::OP_FAILED, e.to_string())
}

/// 请求行构造（`crate::files::Request` 的线上同形：**声明序**输出 + omitempty 对齐——
/// Go Request 是 struct，字段序 = 声明序而非 map 字典序）。
fn request_line(op: &str, path: &str, max_bytes: i64, mode: Option<&str>, size: i64) -> String {
    #[derive(serde::Serialize)]
    struct Req<'a> {
        op: &'a str,
        path: &'a str,
        #[serde(skip_serializing_if = "is_zero")]
        #[serde(rename = "maxBytes")]
        max_bytes: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        mode: Option<&'a str>,
        #[serde(skip_serializing_if = "is_zero")]
        size: i64,
    }
    fn is_zero(v: &i64) -> bool {
        *v == 0
    }
    serde_json::to_string(&Req { op, path, max_bytes, mode, size }).expect("请求行序列化不会失败")
}

// ---------------------------------------------------------------------------
// 会话表 + 传输
// ---------------------------------------------------------------------------

/// 一次传输的运行态（transfers 轮询的数据源；interior-mutable——传输线程写、派发面读，
/// 同一 `Arc<Transfer>` 共享）。
struct Transfer {
    id: i64,
    direction: &'static str,
    remote_path: String,
    local_path: String,
    bytes: AtomicI64,
    total: AtomicI64,
    err: Mutex<String>,
    done: AtomicBool,
    /// 归因旗标（FIX-42 配套）：取消/收工**先置旗标再断 I/O**——只按超时归因会落成
    /// op_failed（负载下实测）。
    cancelled: AtomicBool,
    /// 断在途 I/O 的出口（传输线程拨号成功后登记；未登记 = 还在拨号/未开跑，no-op）。
    cut: Mutex<Option<Box<dyn Fn() + Send>>>,
}

impl Transfer {
    fn snapshot_json(&self) -> Value {
        let mut m = Map::new();
        m.insert("id".into(), Value::from(self.id));
        m.insert("direction".into(), Value::String(self.direction.to_owned()));
        m.insert("remotePath".into(), Value::String(self.remote_path.clone()));
        m.insert("bytes".into(), Value::from(self.bytes.load(Ordering::Relaxed)));
        m.insert("total".into(), Value::from(self.total.load(Ordering::Relaxed)));
        m.insert("done".into(), Value::from(self.done.load(Ordering::Relaxed)));
        m.insert("err".into(), Value::String(self.err.lock().expect("err 锁中毒").clone()));
        Value::Object(m)
    }

    /// 断在途 I/O（幂等）。
    fn cut_io(&self) {
        if let Some(f) = self.cut.lock().expect("cut 锁中毒").take() {
            f();
        }
    }

    fn set_done(&self, err: &str) {
        *self.err.lock().expect("err 锁中毒") = err.to_owned();
        self.done.store(true, Ordering::Relaxed);
    }

    fn mark(&self, bytes: i64, total: i64) {
        self.bytes.store(bytes, Ordering::Relaxed);
        self.total.store(total, Ordering::Relaxed);
    }
}

/// 一个文件会话（connect 建立；auth/sock 绑在会话上，后续命令逐条新流）。
struct FilesSession {
    id: i64,
    auth: String,
    sock: String,
    last_used: Mutex<Instant>,
    txs: Mutex<HashMap<i64, Arc<Transfer>>>,
    tx_next: Mutex<i64>,
}

impl FilesSession {
    /// 收工：打断全部在跑传输（FIX-42——被淘汰/关闭的会话不得留孤儿 I/O）。
    fn shutdown(&self) {
        let txs: Vec<Arc<Transfer>> = self.txs.lock().expect("tx 锁中毒").values().cloned().collect();
        for tx in txs {
            tx.cancelled.store(true, Ordering::Relaxed);
            tx.cut_io();
        }
    }
}

/// files 派发器（会话表 + 单调 handle 计数；线程安全——NAPI 面可在多线程进）。
#[derive(Default)]
pub struct FilesOps {
    sessions: Mutex<HashMap<i64, Arc<FilesSession>>>,
    next: AtomicI64,
}

impl FilesOps {
    pub fn new() -> Self {
        Self::default()
    }

    /// ClientCoreFilesCall：opJson 进、JSON 出（信封 = 成功 `{"ok":true,…}` /
    /// 失败 `{"error":{"code","msg"}}`——键序字典序同 Go json.Marshal(map)）。
    pub fn files_call(&self, op_json: &str) -> String {
        let op: Value = match serde_json::from_str(op_json) {
            Ok(v) => v,
            Err(e) => {
                return marshal(Err(FilesOpError::new(code::INVALID_ARG, format!("参数不是合法 JSON：{e}"))))
            }
        };
        let name = op.get("op").and_then(Value::as_str).unwrap_or("");
        let handle = op.get("handle").and_then(Value::as_i64).unwrap_or(0);
        let strv = |k: &str| op.get(k).and_then(Value::as_str).unwrap_or("").to_owned();
        let intv = |k: &str, d: i64| op.get(k).and_then(Value::as_i64).filter(|v| *v > 0).unwrap_or(d);

        if name == "connect" {
            let timeout = Duration::from_millis(intv("timeoutMs", 15000) as u64);
            return marshal(self.op_connect(&strv("auth"), &strv("sock"), timeout));
        }

        let Ok(sess) = self.get_session(handle) else {
            return marshal(Err(FilesOpError::new(code::NO_SESSION, "会话不存在或已关闭")));
        };
        let res = match name {
            "close" => self.op_close(&sess),
            "list" => Self::op_list(&sess, &strv("path")),
            "stat" => Self::op_stat(&sess, &strv("path")),
            "mkdir" => Self::op_mkdir(&sess, &strv("path")),
            "readText" => Self::op_read(&sess, &strv("path"), "", intv("maxBytes", DEFAULT_TEXT_MAX_BYTES)),
            "readImage" => Self::op_read(&sess, &strv("path"), "image", intv("maxBytes", DEFAULT_IMAGE_MAX_BYTES)),
            "download" => Self::op_transfer(&sess, "download", &strv("remotePath"), &strv("localPath")),
            "upload" => Self::op_transfer(&sess, "upload", &strv("remotePath"), &strv("localPath")),
            "transfers" => {
                let txs = sess.txs.lock().expect("tx 锁中毒");
                let next = *sess.tx_next.lock().expect("txNext 锁中毒");
                let out: Vec<Value> = (1..=next).filter_map(|id| txs.get(&id).map(|t| t.snapshot_json())).collect();
                let mut m = Map::new();
                m.insert("transfers".into(), Value::Array(out));
                Ok(Value::Object(m))
            }
            "cancel" => Self::op_cancel(&sess, intv("transferId", 0)),
            other => Err(FilesOpError::new(code::INVALID_ARG, format!("未知操作 {other:?}"))),
        };
        marshal(res)
    }

    fn get_session(&self, handle: i64) -> Result<Arc<FilesSession>, FilesOpError> {
        let guard = self.sessions.lock().expect("会话锁中毒");
        let Some(s) = guard.get(&handle) else {
            return Err(FilesOpError::new(code::NO_SESSION, "会话不存在或已关闭"));
        };
        *s.last_used.lock().expect("lastUsed 锁中毒") = Instant::now();
        Ok(Arc::clone(s))
    }

    fn op_connect(&self, auth: &str, sock: &str, timeout: Duration) -> Result<Value, FilesOpError> {
        let (stream, root, ver) = BridgeStream::open(sock, auth, timeout).map_err(|e| {
            // 问候阶段非 files 协议稳定码（files_not_enabled/other_service/stream_open 透传）；
            // op_failed 一律归一 bridge_down：触发 App 侧自动重试骑过恢复窗口
            //（Go nativeFilesConnect 同义——恢复阶梯已在首拨触发，第二次进来即用）。
            if e.code == code::OP_FAILED {
                FilesOpError::new(code::BRIDGE_DOWN, format!("文件通道当时不可用：{}", e.msg))
            } else {
                e
            }
        })?;
        // 问候流用完即关（协议契约 = 每命令一条流；不关会打满桥并发闸）
        drop(stream);

        let (id, evicted) = {
            let mut guard = self.sessions.lock().expect("会话锁中毒");
            let evicted = if guard.len() >= MAX_SESSIONS {
                guard
                    .iter()
                    .min_by_key(|(_, s)| *s.last_used.lock().expect("lastUsed 锁中毒"))
                    .map(|(id, _)| *id)
                    .and_then(|id| guard.remove(&id))
            } else {
                None
            };
            let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
            guard.insert(
                id,
                Arc::new(FilesSession {
                    id,
                    auth: auth.to_owned(),
                    sock: sock.to_owned(),
                    last_used: Mutex::new(Instant::now()),
                    txs: Mutex::new(HashMap::new()),
                    tx_next: Mutex::new(0),
                }),
            );
            (id, evicted)
        };
        // 淘汰收工在锁外（shutdown 取 tx 锁，与会话锁无嵌套）
        if let Some(old) = evicted {
            old.shutdown();
        }
        let mut m = Map::new();
        m.insert("handle".into(), Value::from(id));
        m.insert("root".into(), Value::String(root));
        m.insert("ver".into(), Value::from(ver));
        Ok(Value::Object(m))
    }

    fn op_close(&self, sess: &Arc<FilesSession>) -> Result<Value, FilesOpError> {
        self.sessions.lock().expect("会话锁中毒").remove(&sess.id);
        sess.shutdown();
        let mut m = Map::new();
        m.insert("ok".into(), Value::from(true));
        Ok(Value::Object(m))
    }

    /// 操作路径的流开场（open + 归一：`stream_open` ⇒ bridge_down 触发通道重试——
    /// Go mapOpErr/normalizeOpStreamErr 同义。只认开场不认 call 阶段：请求可能已
    /// 送达，重放会有重复副作用）。
    fn op_stream(sess: &Arc<FilesSession>) -> Result<BridgeStream, FilesOpError> {
        BridgeStream::open(&sess.sock, &sess.auth, CONNECT_TIMEOUT)
            .map(|(s, _, _)| s)
            .map_err(|e| {
                if e.code == CODE_STREAM_OPEN {
                    FilesOpError::new(code::BRIDGE_DOWN, format!("文件通道当时不可用：{}", e.msg))
                } else {
                    e
                }
            })
    }

    fn op_list(sess: &Arc<FilesSession>, path: &str) -> Result<Value, FilesOpError> {
        let mut s = Self::op_stream(sess)?;
        let resp = s.call(&request_line("list", path, 0, None, 0))?;
        check_ok(&resp)?;
        let mut m = Map::new();
        m.insert(
            "entries".into(),
            resp.get("entries").cloned().unwrap_or(Value::Array(vec![])),
        );
        Ok(Value::Object(m))
    }

    fn op_stat(sess: &Arc<FilesSession>, path: &str) -> Result<Value, FilesOpError> {
        let mut s = Self::op_stream(sess)?;
        let resp = s.call(&request_line("stat", path, 0, None, 0))?;
        check_ok(&resp)?;
        let mut m = Map::new();
        m.insert("entry".into(), resp.get("entry").cloned().unwrap_or(Value::Null));
        Ok(Value::Object(m))
    }

    fn op_mkdir(sess: &Arc<FilesSession>, path: &str) -> Result<Value, FilesOpError> {
        let mut s = Self::op_stream(sess)?;
        check_ok(&s.call(&request_line("mkdir", path, 0, None, 0))?)?;
        let mut m = Map::new();
        m.insert("ok".into(), Value::from(true));
        Ok(Value::Object(m))
    }

    fn op_read(sess: &Arc<FilesSession>, path: &str, mode: &str, max_bytes: i64) -> Result<Value, FilesOpError> {
        let mut s = Self::op_stream(sess)?;
        let resp = s.call(&request_line("read", path, max_bytes, Some(mode), 0))?;
        check_ok(&resp)?;
        let mut m = Map::new();
        m.insert("size".into(), Value::from(resp.get("size").and_then(Value::as_i64).unwrap_or(0)));
        m.insert(
            "truncated".into(),
            Value::from(resp.get("truncated").and_then(Value::as_bool).unwrap_or(false)),
        );
        if mode == "image" {
            m.insert("base64".into(), Value::String(resp.get("base64").and_then(Value::as_str).unwrap_or("").to_owned()));
        } else {
            m.insert("text".into(), Value::String(resp.get("text").and_then(Value::as_str).unwrap_or("").to_owned()));
        }
        Ok(Value::Object(m))
    }

    /// 起一次传输（busy 闸：任一未完成传输在场即拒绝）。
    fn op_transfer(
        sess: &Arc<FilesSession>,
        direction: &'static str,
        remote_path: &str,
        local_path: &str,
    ) -> Result<Value, FilesOpError> {
        if remote_path.is_empty() || local_path.is_empty() {
            return Err(FilesOpError::new(code::INVALID_ARG, "remotePath 与 localPath 必填"));
        }
        let mut txs = sess.txs.lock().expect("tx 锁中毒");
        if txs.values().any(|t| !t.done.load(Ordering::Relaxed)) {
            return Err(FilesOpError::new(code::BUSY, "已有传输进行中，请等它完成或取消"));
        }
        let mut next = sess.tx_next.lock().expect("txNext 锁中毒");
        *next += 1;
        let tx = Arc::new(Transfer {
            id: *next,
            direction,
            remote_path: remote_path.to_owned(),
            local_path: local_path.to_owned(),
            bytes: AtomicI64::new(0),
            total: AtomicI64::new(0),
            err: Mutex::new(String::new()),
            done: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
            cut: Mutex::new(None),
        });
        let id = tx.id;
        txs.insert(id, Arc::clone(&tx));
        drop(txs);
        let sess2 = Arc::clone(sess);
        let tx2 = Arc::clone(&tx);
        std::thread::Builder::new()
            .name("hw-files-tx".into())
            .spawn(move || run_transfer(sess2, tx2))
            .ok();
        let mut m = Map::new();
        m.insert("transferId".into(), Value::from(id));
        Ok(Value::Object(m))
    }

    fn op_cancel(sess: &Arc<FilesSession>, id: i64) -> Result<Value, FilesOpError> {
        let txs = sess.txs.lock().expect("tx 锁中毒");
        if let Some(tx) = txs.get(&id) {
            // 先归因后断 I/O（Go cancelled 旗标时序）
            tx.cancelled.store(true, Ordering::Relaxed);
            tx.cut_io();
        }
        let mut m = Map::new();
        m.insert("ok".into(), Value::from(true));
        Ok(Value::Object(m))
    }
}

/// 响应行 ok=false ⇒ 稳定码错误（缺 code 归 op_failed）。
fn check_ok(resp: &Value) -> Result<(), FilesOpError> {
    if resp.get("ok").and_then(Value::as_bool).unwrap_or(false) {
        return Ok(());
    }
    let c = resp.get("code").and_then(Value::as_str).unwrap_or(code::OP_FAILED);
    let m = resp.get("msg").and_then(Value::as_str).unwrap_or("");
    Err(FilesOpError::new(c, m))
}

/// 传输线程（下载/上传各一形态；开场警戒 = 第一个进度前 TRANSFER_OPEN_BUDGET 断读）。
fn run_transfer(sess: Arc<FilesSession>, tx: Arc<Transfer>) {
    let cancelled = || tx.cancelled.load(Ordering::Relaxed);
    let mut stream = match BridgeStream::open(&sess.sock, &sess.auth, CONNECT_TIMEOUT) {
        Ok((s, _, _)) => s,
        Err(e) => {
            tx.set_done(&e.msg);
            return;
        }
    };
    // 登记断 I/O 出口（拨号成功后；重复登记以最后一次为准——本形态每传输一条流）
    *tx.cut.lock().expect("cut 锁中毒") = Some(Box::new({
        let conn = stream.conn.try_clone().ok();
        move || {
            if let Some(c) = &conn {
                let _ = c.shutdown(std::net::Shutdown::Both);
            }
        }
    }));

    let started = Instant::now();
    let mut progressed = false;
    // 开场警戒：预算内无正文字节即断读收口（Go transferOpenBudget 语义；正文一旦
    // 流动即解除——大文件不受限）。
    let open_overdue = |progressed: bool| !progressed && started.elapsed() > TRANSFER_OPEN_BUDGET;

    let result: Result<(), String> = (|| {
        match tx.direction {
            "download" => {
                let resp = stream.call(&request_line("download", &tx.remote_path, 0, None, 0)).map_err(|e| e.msg)?;
                check_ok(&resp).map_err(|e| e.msg)?;
                let declared = resp.get("size").and_then(Value::as_i64).unwrap_or(0);
                tx.mark(0, declared);
                let mut file = std::fs::File::create(&tx.local_path).map_err(|e| format!("创建本地文件：{e}"))?;
                let mut total: i64 = 0;
                loop {
                    if cancelled() {
                        return Err("canceled: 已取消".to_owned());
                    }
                    if open_overdue(progressed) {
                        return Err("传输开场超时（无正文字节）：断读收口".to_owned());
                    }
                    match stream.read_frame().map_err(|e| e.msg)? {
                        Some(payload) => {
                            file.write_all(&payload).map_err(|e| format!("写本地失败：{e}"))?;
                            total += payload.len() as i64;
                            progressed = true;
                            tx.mark(total, declared);
                        }
                        None => {
                            // 断读收口（FIX-40）：少收一律报错；多收容忍（下载生长中的文件合法）
                            if declared > 0 && total < declared {
                                return Err(format!("下载提前终止（{total} < {declared}）"));
                            }
                            if let Ok(md) = file.metadata() {
                                tx.mark(md.len() as i64, md.len() as i64);
                            }
                            return Ok(());
                        }
                    }
                }
            }
            "upload" => {
                let mut file = std::fs::File::open(&tx.local_path).map_err(|e| format!("打开本地文件：{e}"))?;
                let size = file.metadata().map(|m| m.len() as i64).map_err(|e| format!("读本地大小：{e}"))?;
                tx.mark(0, size);
                // 上传动词 = "write"（Go Request{Op:"write",Size} 同源）；请求行先收一
                // 应答行（受理），终止帧后再收一应答行（提交结果）。
                let resp = stream.call(&request_line("write", &tx.remote_path, 0, None, size)).map_err(|e| e.msg)?;
                check_ok(&resp).map_err(|e| e.msg)?;
                let mut sent: i64 = 0;
                let mut buf = [0u8; 128 * 1024];
                loop {
                    if cancelled() {
                        // 取消 = 未发终止帧直接断读（服务端收 .tierpart 残片自清）
                        stream.cut();
                        return Err("canceled: 已取消".to_owned());
                    }
                    if open_overdue(progressed) {
                        return Err("传输开场超时（无正文字节）：断读收口".to_owned());
                    }
                    let n = file.read(&mut buf).map_err(|e| format!("读本地失败：{e}"))?;
                    if n == 0 {
                        break;
                    }
                    stream.write_frame(&buf[..n]).map_err(|e| e.msg)?;
                    sent += n as i64;
                    progressed = true;
                    tx.mark(sent, size);
                }
                // 终止帧（len=0）= 提交；读提交结果行（响应面，不设上限）
                stream.write_frame(&[]).map_err(|e| e.msg)?;
                let line = stream.read_line_capped(false).map_err(|e| e.msg)?;
                let resp: Value = serde_json::from_slice(&line).map_err(|e| format!("上传回执不是 JSON：{e}"))?;
                check_ok(&resp).map_err(|e| e.msg)?;
                tx.mark(sent, size);
                Ok(())
            }
            _ => unreachable!("方向在 start 处已校验"),
        }
    })();

    match result {
        Ok(()) => tx.set_done(""),
        Err(_) if cancelled() => tx.set_done("canceled: 已取消"),
        Err(e) => tx.set_done(&e),
    }
}

/// marshalFiles：成功合并载荷 + `"ok":true`；失败 `"error":{code,msg}`（键序字典序）。
fn marshal(res: Result<Value, FilesOpError>) -> String {
    let mut out = Map::new();
    match res {
        Ok(v) => {
            if let Some(obj) = v.as_object() {
                for (k, val) in obj {
                    out.insert(k.clone(), val.clone());
                }
            }
            out.insert("ok".into(), Value::from(true));
        }
        Err(e) => {
            let mut em = Map::new();
            em.insert("code".into(), Value::String(e.code));
            em.insert("msg".into(), Value::String(e.msg));
            out.insert("error".into(), Value::Object(em));
        }
    }
    Value::Object(out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    fn tmp_sock(name: &str) -> (UnixListener, String) {
        let dir = std::env::temp_dir().join(format!("hwfilesop-{:?}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("files.sock");
        let _ = std::fs::remove_file(&path);
        let ln = UnixListener::bind(&path).unwrap();
        (ln, path.to_str().unwrap().to_owned())
    }

    fn auth_hex() -> String {
        let mut blob = [0u8; 48];
        blob[..16].copy_from_slice(b"TIERBRIDGEAUTH01");
        blob.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 桥桩：auth → **先送问候行**（每命令一条流、每条流以问候开场）→ 读一行命令行、
    /// 按命令回行。
    fn serve(ln: UnixListener, resp_of: fn(&str) -> String) {
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader};
            for conn in ln.incoming() {
                let Ok(conn) = conn else { break };
                std::thread::spawn(move || {
                    let (mut r, mut w) = (conn.try_clone().unwrap(), conn);
                    let mut authbuf = [0u8; 48];
                    r.read_exact(&mut authbuf).unwrap();
                    w.write_all(br#"{"ok":true,"root":"/r","ver":1}"#).unwrap();
                    w.write_all(b"\n").unwrap();
                    let mut line = String::new();
                    let mut reader = BufReader::new(r);
                    if reader.read_line(&mut line).unwrap() == 0 {
                        return;
                    }
                    w.write_all(resp_of(&line).as_bytes()).unwrap();
                    w.write_all(b"\n").unwrap();
                });
            }
        });
    }

    #[test]
    fn connect_list_close_flow() {
        let (ln, sock) = tmp_sock("flow");
        serve(
            ln,
            |line| {
                if line.contains("\"list\"") {
                    r#"{"ok":true,"entries":[{"name":"a.bin","isDir":false,"size":10,"mtimeMs":1696000000000}]}"#.to_owned()
                } else {
                    r#"{"ok":true}"#.to_owned()
                }
            },
        );
        let ops = FilesOps::new();
        let auth = auth_hex();
        // connect
        let out = ops.files_call(&format!(r#"{{"op":"connect","auth":"{auth}","sock":"{sock}"}}"#));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert!(v["ok"].as_bool().unwrap());
        assert_eq!(v["root"], "/r");
        assert_eq!(v["ver"], 1);
        let handle = v["handle"].as_i64().unwrap();
        // list
        let out = ops.files_call(&format!(r#"{{"op":"list","handle":{handle},"path":"/"}}"#));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["entries"][0]["name"], "a.bin");
        assert_eq!(v["entries"][0]["mtimeMs"], 1696000000000i64);
        // close → ok；关后再用 → no_session
        let out = ops.files_call(&format!(r#"{{"op":"close","handle":{handle}}}"#));
        assert!(serde_json::from_str::<Value>(&out).unwrap()["ok"].as_bool().unwrap());
        let out = ops.files_call(&format!(r#"{{"op":"list","handle":{handle},"path":"/"}}"#));
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["error"]["code"], "no_session");
    }

    #[test]
    fn envelopes() {
        let ops = FilesOps::new();
        let v: Value = serde_json::from_str(&ops.files_call("{oops")).unwrap();
        assert_eq!(v["error"]["code"], "invalid_arg");
        // handle 不存在先拦（no_session 优先于未知操作——Go getSession 在 switch 前）
        let v: Value = serde_json::from_str(&ops.files_call(r#"{"op":"nope","handle":1}"#)).unwrap();
        assert_eq!(v["error"]["code"], "no_session");
        // 空 sock → bridge_down
        let v: Value = serde_json::from_str(&ops.files_call(r#"{"op":"connect","auth":"","sock":""}"#)).unwrap();
        assert_eq!(v["error"]["code"], "bridge_down");
        // 坏 auth → bridge_auth（listener 必须活着——drop 即拔 socket 文件，connect 会
        // 先以 bridge_down 失败）
        let (_ln_alive, sock) = tmp_sock("auth");
        let v: Value =
            serde_json::from_str(&ops.files_call(&format!(r#"{{"op":"connect","auth":"zz","sock":"{sock}"}}"#))).unwrap();
        assert_eq!(v["error"]["code"], "bridge_auth");
    }

    /// 会话驱逐：cap=4，第 5 次 connect 挤掉最久未用。
    #[test]
    fn session_eviction() {
        let (ln, sock) = tmp_sock("evict");
        serve(ln, |_| r#"{"ok":true}"#.to_owned());
        let ops = FilesOps::new();
        let auth = auth_hex();
        let mut handles = vec![];
        for _ in 0..5 {
            let out = ops.files_call(&format!(r#"{{"op":"connect","auth":"{auth}","sock":"{sock}"}}"#));
            handles.push(serde_json::from_str::<Value>(&out).unwrap()["handle"].as_i64().unwrap());
        }
        // 第一个被挤掉（最久未用）
        let out = ops.files_call(&format!(r#"{{"op":"list","handle":{},"path":"/"}}"#, handles[0]));
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["error"]["code"], "no_session");
        // 后四个在（list 走到桥面成功而非 no_session）
        for h in &handles[1..] {
            let out = ops.files_call(&format!(r#"{{"op":"list","handle":{h},"path":"/"}}"#));
            let v: Value = serde_json::from_str(&out).unwrap();
            assert!(v["ok"].as_bool().unwrap());
        }
    }

    /// 传输：下载端到端（桥桩回声明大小 + 一帧 + 终止帧）→ transfers 轮询 → done。
    #[test]
    fn download_transfer_flow() {
        let (ln, sock) = tmp_sock("dl");
        std::thread::spawn(move || {
            use std::io::{BufRead, BufReader, Write};
            for conn in ln.incoming() {
                let Ok(conn) = conn else { break };
                std::thread::spawn(move || {
                    let (mut r, mut w) = (conn.try_clone().unwrap(), conn);
                    let mut authbuf = [0u8; 48];
                    r.read_exact(&mut authbuf).unwrap();
                    w.write_all(br#"{"ok":true,"root":"/r","ver":1}"#).unwrap();
                    w.write_all(b"\n").unwrap();
                    let mut line = String::new();
                    let mut reader = BufReader::new(r);
                    if reader.read_line(&mut line).unwrap() == 0 {
                        return;
                    }
                    if line.contains("\"download\"") {
                        w.write_all(br#"{"ok":true,"size":5}"#).unwrap();
                        w.write_all(b"\n").unwrap();
                        w.write_all(&5u32.to_be_bytes()).unwrap();
                        w.write_all(b"hello").unwrap();
                        w.write_all(&0u32.to_be_bytes()).unwrap();
                    }
                });
            }
        });
        let ops = FilesOps::new();
        let auth = auth_hex();
        let out = ops.files_call(&format!(r#"{{"op":"connect","auth":"{auth}","sock":"{sock}"}}"#));
        let h = serde_json::from_str::<Value>(&out).unwrap()["handle"].as_i64().unwrap();
        let local = std::env::temp_dir().join(format!("hwfilesop-dl-{}.bin", std::process::id()));
        let out = ops.files_call(&format!(
            r#"{{"op":"download","handle":{h},"remotePath":"/r/f.bin","localPath":"{}"}}"#,
            local.display()
        ));
        let v: Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["transferId"], 1);
        // 等传输完成（轮询 transfers）
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut done = false;
        while Instant::now() < deadline {
            let out = ops.files_call(&format!(r#"{{"op":"transfers","handle":{h}}}"#));
            let v: Value = serde_json::from_str(&out).unwrap();
            let tx = &v["transfers"][0];
            if tx["done"].as_bool().unwrap() {
                assert_eq!(tx["err"], "");
                assert_eq!(tx["bytes"], 5);
                assert_eq!(tx["total"], 5);
                assert_eq!(tx["direction"], "download");
                done = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(30));
        }
        assert!(done, "5s 内传输未完成");
        assert_eq!(std::fs::read(&local).unwrap(), b"hello");
        let _ = std::fs::remove_file(&local);
        // busy 闸：done 后可再起
        let local2 = local.with_extension("2.bin");
        let out = ops.files_call(&format!(
            r#"{{"op":"download","handle":{h},"remotePath":"/r/f.bin","localPath":"{}"}}"#,
            local2.display()
        ));
        assert_eq!(serde_json::from_str::<Value>(&out).unwrap()["transferId"], 2);
        let _ = std::fs::remove_file(&local2);
    }

    #[test]
    fn request_line_omitempty() {
        assert_eq!(request_line("list", "/", 0, None, 0), r#"{"op":"list","path":"/"}"#);
        assert_eq!(
            request_line("read", "/f", 512, Some("image"), 0),
            r#"{"op":"read","path":"/f","maxBytes":512,"mode":"image"}"#
        );
        assert_eq!(
            request_line("upload", "/f", 0, None, 1024),
            r#"{"op":"upload","path":"/f","size":1024}"#
        );
    }
}
