//! 控制面客户端（语义真源 `baseline:internal/control/client.go`）：握手/请求/
//! 订阅/流的客户端 API——CLI（host/status/serve/term --host 族）与控制面集成测试
//! 共用（真实消费者路径：每个测试经本客户端走完整 UDS + 帧 + JSON 协议）。
//!
//! Rust 形态：一客户端一 reader 线程（分发 rsp/evt/流帧/生命周期帧），请求走
//! corr 关联的应答通道（带超时预算）。流上行按 16KiB 自动分片（单帧 body 超
//! 256KiB-4 会被服务端按帧长上限拒——大块上行必须分片）；下行 [`ClientStream`]
//! 有界 64 条 + 阻塞投递（慢消费背压沿链传导；连接关闭解阻塞）。

use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::frame::{self, Op};
use super::proto::*;
use super::server::{read_body_buf, read_head};
use super::vocab;

/// 流上行分片（与服务端下行读块同款常量）。
const STREAM_CHUNK_SIZE: usize = 16 << 10;
/// 下行队列条数（单流）。
const STREAM_RECV_ITEMS: usize = 64;

/// 控制面错误：稳定码 + 服务端可行动归因。
pub use super::proto::OpError;

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ClientErr {
    #[error("控制面连接失败（{0}）——守护进程未运行？")]
    Dial(String),
    #[error("服务端要求 reload（{0}）")]
    Reload(String),
    #[error("控制面连接已关闭")]
    ConnClosed,
    #[error("等待应答超时")]
    Timeout,
    #[error("流已终结（{0}）")]
    StreamEnded(String),
    #[error("IO：{0}")]
    Io(String),
}

struct RecvQueue {
    items: VecDeque<Vec<u8>>,
    closed: bool,
}

/// 前端侧的一条流。
pub struct ClientStream {
    id: AtomicU32,
    recv: Mutex<RecvQueue>,
    recv_cv: Condvar,
    /// 终结状态（closed|gone|conn）：置位后 [`ControlClient::stream_send`] 恒报流终结错误。
    ended: Mutex<Option<String>>,
}

impl ClientStream {
    pub fn stream_id(&self) -> u32 {
        self.id.load(Ordering::SeqCst)
    }

    fn mark_ended(&self, reason: &str) {
        let mut e = self.ended.lock().unwrap_or_else(|e| e.into_inner());
        if e.is_none() {
            *e = Some(reason.to_owned());
        }
    }

    /// 终结置位（close_stream 成功后的本端闸；幂等、首个原因生效）。
    fn mark_ended_pub(&self, reason: &str) {
        self.mark_ended(reason);
        self.unblock_recv();
    }

    /// 终结原因（未终结 = None；closed|gone|conn 三态——conn 为连接级断开的本地值）。
    pub fn end_reason(&self) -> Option<String> {
        self.ended.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn blocking_push(self: &Arc<Self>, payload: Vec<u8>) {
        let mut q = self.recv.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if q.closed || self.end_reason().is_some() {
                return;
            }
            if q.items.len() < STREAM_RECV_ITEMS {
                q.items.push_back(payload);
                return;
            }
            let (guard, _) = self
                .recv_cv
                .wait_timeout(q, Duration::from_millis(200))
                .unwrap_or_else(|e| e.into_inner());
            q = guard;
        }
    }

    fn unblock_recv(&self) {
        let mut q = self.recv.lock().unwrap_or_else(|e| e.into_inner());
        q.closed = true;
        drop(q);
        self.recv_cv.notify_all();
    }

    /// 下行一条（预算内阻塞；Ok(None) = 流已终结或连接断开——终因查 [`Self::end_reason`]）。
    pub fn recv_timeout(&self, timeout: Duration) -> Option<Vec<u8>> {
        let deadline = Instant::now() + timeout;
        let mut q = self.recv.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(v) = q.items.pop_front() {
                return Some(v);
            }
            if q.closed || self.end_reason().is_some() {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = self
                .recv_cv
                .wait_timeout(q, deadline - now)
                .unwrap_or_else(|e| e.into_inner());
            q = guard;
        }
    }

    /// 阻塞收一条下行（无预算——term CLI 长连接消费面；终结/连接断开 = None，
    /// 终因查 [`Self::end_reason`]）。与 [`Self::recv_timeout`] 同锁同队列。
    pub fn recv_wait(&self) -> Option<Vec<u8>> {
        let mut q = self.recv.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(v) = q.items.pop_front() {
                return Some(v);
            }
            if q.closed || self.end_reason().is_some() {
                return None;
            }
            q = self.recv_cv.wait(q).unwrap_or_else(|e| e.into_inner());
        }
    }

    // 上行/关闭面见 ControlClient::stream_send / stream_close——流对象不持客户端
    // 引用（写出面归客户端，调用方两者都持有）。
}

/// 控制面客户端（一连接一客户端）。
pub struct ControlClient {
    sock: PathBuf,
    write_sock: Mutex<UnixStream>,
    corr: AtomicU64,
    pending: Mutex<HashMap<u64, std::sync::mpsc::Sender<ResponseBody>>>,
    streams: Mutex<HashMap<u32, Arc<ClientStream>>>,
    pending_streams: Mutex<HashMap<u64, Arc<ClientStream>>>,
    closed: AtomicBool,
    goodbye: Mutex<Option<GoodbyeBody>>,
    reload: Mutex<Option<ReloadBody>>,
    notify_rx: Mutex<Option<std::sync::mpsc::Receiver<ResponseBody>>>,
    events_rx: Mutex<Option<std::sync::mpsc::Receiver<EventBody>>>,
    notify_tx: std::sync::mpsc::Sender<ResponseBody>,
    events_tx: std::sync::mpsc::SyncSender<EventBody>,
    ctl_sock: Mutex<UnixStream>,
}

impl ControlClient {
    /// 连接 control.sock 并完成握手；返回客户端与 welcome。
    pub fn dial(
        sock: &Path,
        frontend_kind: &str,
        frontend_name: &str,
    ) -> Result<(Arc<ControlClient>, WelcomeBody), ClientErr> {
        let stream = UnixStream::connect(sock).map_err(|e| ClientErr::Dial(e.to_string()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
        let writer = stream.try_clone().map_err(|e| ClientErr::Io(e.to_string()))?;
        let ctl = stream.try_clone().map_err(|e| ClientErr::Io(e.to_string()))?;
        let (notify_tx, notify_rx) = std::sync::mpsc::channel();
        let (events_tx, events_rx) = std::sync::mpsc::sync_channel(1024);
        let (welcome_tx, welcome_rx) = std::sync::mpsc::channel();
        let c = Arc::new(ControlClient {
            sock: sock.to_owned(),
            write_sock: Mutex::new(writer),
            corr: AtomicU64::new(0),
            pending: Mutex::new(HashMap::new()),
            streams: Mutex::new(HashMap::new()),
            pending_streams: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
            goodbye: Mutex::new(None),
            reload: Mutex::new(None),
            notify_rx: Mutex::new(Some(notify_rx)),
            events_rx: Mutex::new(Some(events_rx)),
            notify_tx,
            events_tx,
            ctl_sock: Mutex::new(ctl),
        });
        let rc = Arc::clone(&c);
        std::thread::Builder::new()
            .name("hw-cctl-reader".to_owned())
            .spawn(move || rc.reader_main(stream, welcome_tx))
            .expect("线程创建不可失败");
        // hello。
        let hello = HelloBody {
            proto_version: frame::PROTO_VERSION,
            frontend: FrontendInfo {
                kind: frontend_kind.to_owned(),
                name: frontend_name.to_owned(),
                version: format!("homeway-rs/{}", env!("CARGO_PKG_VERSION")),
            },
        };
        c.write_frame(encode_json_frame(Op::Hello, &hello)).map_err(ClientErr::Io)?;
        // 等 welcome（或 reload——版本不匹配时服务端回 reload 后关闭连接）。
        match welcome_rx.recv_timeout(Duration::from_secs(5)) {
            Ok(w) => Ok((c, w)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                c.close();
                Err(ClientErr::Timeout)
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let r = c.reload.lock().unwrap_or_else(|e| e.into_inner()).clone();
                c.close();
                match r {
                    Some(r) => Err(ClientErr::Reload(r.reason)),
                    None => Err(ClientErr::ConnClosed),
                }
            }
        }
    }

    pub fn sock_path(&self) -> &Path {
        &self.sock
    }

    /// 抽一条 corr=0 通知（非阻塞；如对未知流 stream.data 的 no_stream 回执）。
    pub fn try_notify(&self) -> Option<ResponseBody> {
        self.notify_rx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
    }

    /// 服务端告别（overrun/bad_frame/bad_json/shutting_down…；None = 未见）。
    pub fn goodbye(&self) -> Option<GoodbyeBody> {
        self.goodbye.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// 事件流（含订阅回放；通道单接收者——首个调用者拿走，后续调用得 None）。
    pub fn take_events(&self) -> Option<std::sync::mpsc::Receiver<EventBody>> {
        self.events_rx.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    /// 发请求等响应（corr 关联；args None = 无载荷）。错误码响应返回 [`OpError`]
    /// （码 = 服务端稳定错误码；CLI 侧映射人类可读文案，文案不进契约）。
    pub fn request(
        &self,
        op: &str,
        args: Option<serde_json::Value>,
        timeout: Duration,
    ) -> Result<serde_json::Value, OpError> {
        let corr = self.corr.fetch_add(1, Ordering::SeqCst) + 1;
        let req = RequestBody { corr, op: op.to_owned(), args };
        self.roundtrip(corr, req, timeout)
    }

    fn roundtrip(&self, corr: u64, req: RequestBody, timeout: Duration) -> Result<serde_json::Value, OpError> {
        let (tx, rx) = std::sync::mpsc::channel();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).insert(corr, tx);
        let r = (|| {
            self.write_frame(encode_json_frame(Op::Req, &req))
                .map_err(|e| OpError::with_detail("io_error", e))?;
            match rx.recv_timeout(timeout) {
                Ok(rsp) => {
                    if rsp.ok {
                        Ok(rsp.result.unwrap_or(serde_json::Value::Null))
                    } else {
                        let code = rsp.error.unwrap_or_else(|| "bad_request".to_owned());
                        Err(match rsp.detail {
                            Some(d) => OpError::with_detail(&code, d),
                            None => OpError::code(&code),
                        })
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    Err(OpError::with_detail("timeout", "等待应答超时"))
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    Err(OpError::with_detail("conn_closed", "控制面连接已关闭"))
                }
            }
        })();
        self.pending.lock().unwrap_or_else(|e| e.into_inner()).remove(&corr);
        r
    }

    /// stream.open{kind, host} → 流句柄。流对象在请求发出前经 pending_streams
    /// 预登记、由 reader 在响应帧上同步注册（消除「rsp 处理与调用方注册流之间」
    /// 的首帧竞态：紧随 rsp 的第一帧流数据必须在流已在册时才到达 reader）。
    pub fn open_stream(
        &self,
        kind: &str,
        host: &str,
        timeout: Duration,
    ) -> Result<Arc<ClientStream>, OpError> {
        let corr = self.corr.fetch_add(1, Ordering::SeqCst) + 1;
        let st = Arc::new(ClientStream {
            id: AtomicU32::new(0),
            recv: Mutex::new(RecvQueue { items: VecDeque::new(), closed: false }),
            recv_cv: Condvar::new(),
            ended: Mutex::new(None),
        });
        self.pending_streams.lock().unwrap_or_else(|e| e.into_inner()).insert(corr, Arc::clone(&st));
        let args = serde_json::to_value(StreamOpenArgs { kind: kind.to_owned(), host: host.to_owned() })
            .unwrap();
        let req = RequestBody { corr, op: vocab::OpName::StreamOpen.as_str().to_owned(), args: Some(args) };
        let r = self.roundtrip(corr, req, timeout);
        self.pending_streams.lock().unwrap_or_else(|e| e.into_inner()).remove(&corr);
        match r {
            Ok(v) => {
                let sid: StreamOpenResult =
                    serde_json::from_value(v).map_err(|_| OpError::code("bad_request"))?;
                if st.stream_id() == 0 {
                    st.id.store(sid.stream_id, Ordering::SeqCst);
                }
                self.streams.lock().unwrap_or_else(|e| e.into_inner()).insert(sid.stream_id, Arc::clone(&st));
                Ok(st)
            }
            Err(e) => Err(e),
        }
    }

    /// 上行数据（stream.data 帧；透传原始字节，按 16KiB 自动分片——流 = 字节流
    /// 语义，不承诺帧边界；单帧 body 超 256KiB-4 会被服务端按帧长上限拒，大块
    /// 上行必须分片）。分片循环逐帧复查终结状态，流中途死即停发。
    pub fn stream_send(&self, st: &ClientStream, data: &[u8]) -> Result<(), ClientErr> {
        if data.is_empty() {
            return Ok(()); // 空载荷显式不发（字节流语义下无实害）
        }
        if let Some(r) = st.end_reason() {
            return Err(ClientErr::StreamEnded(r));
        }
        let id = st.stream_id();
        let mut off = 0usize;
        while off < data.len() {
            let end = (off + STREAM_CHUNK_SIZE).min(data.len());
            let f = frame::encode_frame(Op::StreamData, &frame::encode_stream_body(id, &data[off..end]));
            self.write_frame(f).map_err(ClientErr::Io)?;
            if let Some(r) = st.end_reason() {
                return Err(ClientErr::StreamEnded(r));
            }
            off = end;
        }
        Ok(())
    }

    /// 前端主动关流（stream.close 操作，reason=closed）。成功即置终结位（此后
    /// stream_send 不再静默成功写进死流）。
    pub fn stream_close(&self, st: &ClientStream, timeout: Duration) -> Result<(), OpError> {
        let id = st.stream_id();
        self.request(
            vocab::OpName::StreamClose.as_str(),
            Some(serde_json::to_value(StreamCloseArgs { stream_id: id }).unwrap()),
            timeout,
        )?;
        st.mark_ended_pub(vocab::STREAM_END_CLOSED);
        Ok(())
    }

    /// 订阅事件（游标续播；generation = 游标所属代际——非空且失配时服务端回
    /// cursor_stale）。订阅确认后回放事件先于在线事件进入事件通道（服务端写出
    /// 次序保证）。
    pub fn subscribe(
        &self,
        domains: &[String],
        cursor: Option<u64>,
        view: &str,
        generation: &str,
        timeout: Duration,
    ) -> Result<SubscribeResult, OpError> {
        let args = serde_json::to_value(SubscribeArgs {
            domains: Some(domains.to_vec()),
            cursor,
            view: (!view.is_empty()).then(|| view.to_owned()),
            generation: Some(generation.to_owned()),
        })
        .unwrap();
        let v = self.request(vocab::OpName::EventsSubscribe.as_str(), Some(args), timeout)?;
        serde_json::from_value(v).map_err(|_| OpError::code("bad_request"))
    }

    /// 关闭客户端连接（幂等；在册流全部终结状态化——此后 send 报错而非静默丢）。
    pub fn close(&self) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        // 先标在册流 ended（conn），再关 socket——反序时「Closed 已关、流还没标」
        // 的窗口里 send 会返回写错误而非流终结错误。
        self.terminate_streams();
        let _ = self
            .ctl_sock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown(std::net::Shutdown::Both);
    }

    fn terminate_streams(&self) {
        let streams: Vec<Arc<ClientStream>> = self
            .streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, v)| v)
            .collect();
        for s in streams {
            s.mark_ended("conn");
            s.unblock_recv();
        }
    }

    fn write_frame(&self, f: Vec<u8>) -> Result<(), String> {
        let mut w = self.write_sock.lock().unwrap_or_else(|e| e.into_inner());
        w.write_all(&f).map_err(|e| e.to_string())
    }

    fn reader_main(&self, mut sock: UnixStream, welcome_tx: std::sync::mpsc::Sender<WelcomeBody>) {
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let head = match read_head(&mut sock, &mut buf) {
                Ok(Some(h)) => h,
                Ok(None) => {
                    if self.is_closed() {
                        break;
                    }
                    continue;
                }
                Err(_) => break, // EOF/IO 错误 = 连接结束
            };
            let body = match read_body_buf(&mut sock, &mut buf, head) {
                Ok(b) => b,
                Err(_) => break,
            };
            let Some(op) = Op::from_code(head.op) else { break };
            match op {
                Op::Welcome => {
                    if let Ok(w) = serde_json::from_slice::<WelcomeBody>(&body) {
                        let _ = welcome_tx.send(w);
                    }
                }
                Op::Rsp => {
                    let Ok(rsp) = serde_json::from_slice::<ResponseBody>(&body) else { continue };
                    if rsp.corr == 0 {
                        let _ = self.notify_tx.send(rsp);
                        continue;
                    }
                    // stream.open 响应：先于调用方注册流（首帧数据可能紧随本帧到达）。
                    if rsp.ok {
                        let pst = self
                            .pending_streams
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .get(&rsp.corr)
                            .cloned();
                        if let Some(pst) = pst {
                            if let Ok(r) = serde_json::from_value::<StreamOpenResult>(
                                rsp.result.clone().unwrap_or(serde_json::Value::Null),
                            ) {
                                if r.stream_id != 0 {
                                    pst.id.store(r.stream_id, Ordering::SeqCst);
                                    self.streams
                                        .lock()
                                        .unwrap_or_else(|e| e.into_inner())
                                        .insert(r.stream_id, pst);
                                }
                            }
                        }
                    }
                    if let Some(tx) = self.pending.lock().unwrap_or_else(|e| e.into_inner()).get(&rsp.corr) {
                        let _ = tx.send(rsp);
                    }
                }
                Op::Evt => {
                    if let Ok(ev) = serde_json::from_slice::<EventBody>(&body) {
                        // 客户端侧缓冲满（消费者太慢）：丢弃（真实前端应有自己的有界
                        // 缓冲与重订阅策略；服务端侧 overrun 断连是最终兜底）。
                        let _ = self.events_tx.try_send(ev);
                    }
                }
                Op::Goodbye => {
                    if let Ok(g) = serde_json::from_slice::<GoodbyeBody>(&body) {
                        *self.goodbye.lock().unwrap_or_else(|e| e.into_inner()) = Some(g);
                    }
                    break;
                }
                Op::Reload => {
                    if let Ok(r) = serde_json::from_slice::<ReloadBody>(&body) {
                        *self.reload.lock().unwrap_or_else(|e| e.into_inner()) = Some(r);
                    }
                    break;
                }
                Op::StreamData => {
                    let Ok((id, payload)) = frame::decode_stream_body(&body) else { continue };
                    let st = self.streams.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned();
                    if let Some(st) = st {
                        // 下行背压：满槽阻塞投递而非丢弃（term 丢帧 = 无从感知的静默
                        // 画面损坏）；逃生口 = 连接关闭（close 先解阻塞）。
                        st.blocking_push(payload.to_vec());
                    }
                }
                Op::StreamEnd => {
                    let Ok(e) = serde_json::from_slice::<StreamEndBody>(&body) else { continue };
                    let st = self.streams.lock().unwrap_or_else(|e| e.into_inner()).remove(&e.stream_id);
                    if let Some(st) = st {
                        st.mark_ended(&e.reason);
                        st.unblock_recv();
                    }
                }
                Op::Hello | Op::Req => break, // 客户端方向帧从服务端来：次序错乱
            }
        }
        // 连接级断开：在册流全部终结状态化（conn）。
        self.terminate_streams();
        self.closed.store(true, Ordering::SeqCst);
    }
}

