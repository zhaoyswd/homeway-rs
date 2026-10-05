//! 控制面服务器（语义真源 `baseline:internal/control/server.go` + `stream.go`，
//! spec daemon-control-plane §3.2/§3.3/§3.5）。
//!
//! ## 并发模型（B0-2b 第 1 棒拍板，记录见 docs/reviews/B0-2b.md）
//!
//! Go 侧一连接多 goroutine（reader/writer/dispatcher + 每流三泵）。Rust 侧沿
//! R1 决议「自管线程 + 独占 fd」的自然延伸，**每连接三线程 + 每流两线程**：
//!
//! - **reader 线程**：独占 socket 读半（500ms 读超时节拍 + 半帧缓冲），读帧 →
//!   握手/请求入队（在途有界 32，超界 `goodbye(overrun)`）/流上行非阻塞转投；
//! - **dispatcher 线程**：固定请求工位，逐条执行（`stream.open` 的 30s 级拨号移交
//!   独立线程——「有界在途 = 契约、串行不是契约」同义）；
//! - **writer 线程**：**唯一 socket 写者**。出队面 = `Mutex<OutQueue>` + Condvar，
//!   优先级排空：high（welcome/rsp/goodbye/reload/订阅确认）> 事件（订阅者在线
//!   队列）> 流（公平轮询）。写超时 30s（写停滞看门狗：前端整体不读的病态兜底）。
//!
//! **订阅原子交付（Go B3 的 Rust 收敛）**：Go 因 goroutine channel 语义需要
//! heldFrames 暂存 + 复检 + 按 corr 清门闩；本实现里 writer 是事件与回放的唯一
//! 消费者——订阅确认作为**复合出队项**（`SubConfirm{确认帧, 订阅者}`）走 high
//! 队列，writer 写出确认帧后**同一线程**先清门闩、再写回放段、再恢复在线消费
//! （「确认 → 回放 → 在线」三段次序由单一写者天然保证）。门闩期在线事件堆在
//! 订阅者 512 有界队列（满 = overrun 断连——与 Go 门闩期 heldFrames 触顶同款终态）。
//!
//! 协议次序错乱（握手前非 hello、握手后再 hello、前端方向出现服务端帧）= 无词表值
//! 的连接级硬错误：直接关闭连接、不发帧。流式通道纯透传——term 协议语义端到端归
//! 前端，守护进程 MUST NOT 解析/改写。

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixListener;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::{Duration, Instant};

use super::bus::{Bus, SubscribeErr, Subscriber};
use super::frame::{self, FrameError, Op};
use super::listen::ListenError;
use super::proto::*;
use super::vocab;
use super::{Backend, RoleOp, RoleOpOut};

// 默认连接参数（Go server.go design D5；不改契约形状）。
/// 每连接在册流上限（超限 stream_refused）。
pub const DEFAULT_MAX_STREAMS: usize = 8;
/// 连接写停滞看门狗（前端整体不读的病态兜底）。
pub const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(30);
/// 控制帧队列容量（在途请求 + 生命周期帧；告别帧不受此界——尽力送达语义）。
const HIGH_QUEUE: usize = 64;
/// 每连接在途请求上限（在途〔含工位执行中〕≤ 32；超界 goodbye(overrun) 断连）。
const MAX_INFLIGHT: i32 = 32;

// 流参数（Go stream.go design D5）。
const STREAM_QUEUE_ITEMS: usize = 16; // 下行队列条数（≈256KiB/流有界缓冲）
const UP_WORKER_ITEMS: usize = 32; // 上行队列条数界
const UP_WORKER_BYTES: i64 = 512 << 10; // 上行字节界（双界取先到）
/// 全连接（server 汇总）上行工位总量上限（防「每流一工位」的资源放大——超总量
/// 拒开新流，复用既有 stream_refused）。
pub const DEFAULT_MAX_UP_WORKERS: i64 = 64;

/// 服务器装配项。
pub struct ServerConfig {
    pub server_version: String,
    pub bus: Arc<Bus>,
    pub backend: Arc<dyn Backend>,
    pub logf: Arc<dyn Fn(&str) + Send + Sync>,
}

/// 控制面服务器。`serve()` 阻塞跑接入循环（瞬态 accept 错误有界退避重试、永久
/// 错误上抛走角色重建）；`shutdown()` 收工（调用方随后 drop listener 令 accept
/// 返回）。
pub struct ControlServer {
    version: String,
    bus: Arc<Bus>,
    backend: Arc<dyn Backend>,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
    conns: Mutex<Vec<Arc<ConnShared>>>,
    conn_threads: Mutex<Vec<std::thread::JoinHandle<()>>>,
    shutdown_flag: AtomicBool,
    up_workers: AtomicI64,
}

impl ControlServer {
    pub fn new(cfg: ServerConfig) -> Arc<ControlServer> {
        Arc::new(ControlServer {
            version: cfg.server_version,
            bus: cfg.bus,
            backend: cfg.backend,
            logf: cfg.logf,
            conns: Mutex::new(Vec::new()),
            conn_threads: Mutex::new(Vec::new()),
            shutdown_flag: AtomicBool::new(false),
            up_workers: AtomicI64::new(0),
        })
    }

    pub fn generation(&self) -> &str {
        self.bus.generation()
    }

    pub fn shutting_down(&self) -> bool {
        self.shutdown_flag.load(Ordering::SeqCst)
    }

    /// 接入循环（阻塞；正常 shutdown 后返回 Ok）。瞬态 accept 错误（连接中断/
    /// fd 短缺类）同一 listener 有界退避重试；其余（listener 失效类）返回错误由
    /// 调用方（控制角色）上抛走退避重建。
    pub fn serve(self: &Arc<Self>, ln: UnixListener) -> Result<(), ListenError> {
        // 非阻塞 accept + 节拍轮询：listener 所有权在本线程（shutdown 侧无法关它），
        // 轮询 shutting_down 才能保证 serve() 在收工后可返回（角色线程可 join）。
        ln.set_nonblocking(true).map_err(|e| ListenError::Bind(e.to_string()))?;
        let mut retry = Duration::ZERO;
        loop {
            match ln.accept() {
                Ok((stream, _)) => {
                    retry = Duration::ZERO;
                    let peer = self.spawn_conn(stream);
                    self.conns.lock().unwrap_or_else(|e| e.into_inner()).push(peer);
                }
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    // 空转拍：收工检查（50ms 节拍）。
                    if self.shutting_down() {
                        return Ok(());
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
                Err(e) => {
                    if self.shutting_down() {
                        return Ok(());
                    }
                    if !transient_accept_error(&e) {
                        return Err(ListenError::Bind(format!("accept：{e}")));
                    }
                    retry = if retry.is_zero() {
                        Duration::from_millis(5)
                    } else {
                        (retry * 2).min(Duration::from_secs(1))
                    };
                    (self.logf)(&format!(
                        "control: accept 瞬态错误（{e}）——退避 {}ms 后同 listener 重试（不重建）",
                        retry.as_millis()
                    ));
                    std::thread::sleep(retry);
                }
            }
        }
    }

    fn spawn_conn(self: &Arc<Self>, stream: std::os::unix::net::UnixStream) -> Arc<ConnShared> {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(WRITE_STALL_TIMEOUT));
        let writer_sock = stream.try_clone().expect("UDS fd 克隆不可失败");
        let ctl_sock = stream.try_clone().expect("UDS fd 克隆不可失败");
        let (req_tx, req_rx) = std::sync::mpsc::channel::<RequestBody>();
        let conn = Arc::new(ConnShared {
            srv: Arc::clone(self),
            ctl_sock: Mutex::new(ctl_sock),
            closed: AtomicBool::new(false),
            handshook: AtomicBool::new(false),
            inflight: AtomicI32::new(0),
            req_tx,
            out: Mutex::new(OutQueue { high: VecDeque::new(), done: false }),
            out_cv: Condvar::new(),
            sub: Mutex::new(None),
            streams: Mutex::new(StreamTable { next_id: 0, map: HashMap::new(), closed: false }),
        });
        let r_conn = Arc::clone(&conn);
        let reader = std::thread::Builder::new()
            .name("hw-ctl-reader".to_owned())
            .spawn(move || r_conn.reader_main(stream))
            .expect("线程创建不可失败");
        let w_conn = Arc::clone(&conn);
        let writer = std::thread::Builder::new()
            .name("hw-ctl-writer".to_owned())
            .spawn(move || w_conn.writer_main(writer_sock))
            .expect("线程创建不可失败");
        let d_conn = Arc::clone(&conn);
        // dispatcher 句柄即弃（分离线程）：joiner 不 join 它（慢请求不绑架收工——
        // Go 同款纪律；经 recv_timeout 节拍 + is_closed 自终止）。
        let _dispatcher = std::thread::Builder::new()
            .name("hw-ctl-disp".to_owned())
            .spawn(move || d_conn.dispatcher_main(req_rx))
            .expect("线程创建不可失败");
        // reader 退出即整连接收工（等 writer；**不 join dispatcher**——工位可能仍在
        // 执行在途慢请求（host.add 探测等有界慢操作），close 后经 closed 分支自终止；
        // 等它会让收工被一条慢请求绑架——Go 同款纪律）。
        let j_conn = Arc::clone(&conn);
        let j_srv = Arc::clone(self);
        let joiner = std::thread::Builder::new()
            .name("hw-ctl-join".to_owned())
            .spawn(move || {
                let _ = reader.join();
                j_conn.close("");
                let _ = writer.join();
                // 自摘（并发-1：conns 只在 shutdown drain——正常结束的连接若不自摘，
                // 每条泄漏一个 Arc<ConnShared>（连带 ctl_sock 的 fd），长跑必炸 EMFILE）。
                j_srv.conns.lock().unwrap_or_else(|e| e.into_inner()).retain(|c| !Arc::ptr_eq(c, &j_conn));
            })
            .expect("线程创建不可失败");
        self.conn_threads.lock().unwrap_or_else(|e| e.into_inner()).push(joiner);
        conn
    }

    /// 收工：停接入、断开全部连接（在途请求由 shutting_down 错误码路径承接；
    /// `goodbye(shutting_down)` 对还写得出的连接尽力送达）。调用方随后 drop
    /// listener，`serve()` 随之返回。
    pub fn shutdown(&self) {
        self.shutdown_flag.store(true, Ordering::SeqCst);
        let conns: Vec<Arc<ConnShared>> =
            self.conns.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for c in &conns {
            c.close(vocab::CODE_SHUTTING_DOWN);
        }
        let threads: Vec<_> =
            self.conn_threads.lock().unwrap_or_else(|e| e.into_inner()).drain(..).collect();
        for t in threads {
            let _ = t.join();
        }
    }
}

/// accept 错误的瞬态/永久二分（对齐 Go transientAcceptError）：瞬态 = 连接中断类 +
/// fd/内存短缺类 + 非阻塞无待收；其余（listener 已关闭/失效类）= 永久。
fn transient_accept_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionReset
            | ErrorKind::Interrupted
            | ErrorKind::WouldBlock
    ) || matches!(e.raw_os_error(), Some(libc::EMFILE) | Some(libc::ENFILE) | Some(libc::ENOMEM))
}

// ---------- 连接 ----------

enum HighItem {
    /// 普通控制帧（welcome/rsp/goodbye/reload）。
    Frame(Vec<u8>),
    /// 订阅确认（成功或错误应答）：写出确认帧后清门闩 + 先写回放段（B3 三段次序
    /// 的单 writer 落点）。
    SubConfirm { frame: Vec<u8>, sub: Arc<Subscriber> },
    /// 告别/重载帧（带「真写出」回执——close 路径有界等待用，FIX-97 同义）。
    Bye { frame: Vec<u8>, ack: std::sync::mpsc::Sender<()> },
}

struct OutQueue {
    high: VecDeque<HighItem>,
    done: bool,
}

struct StreamTable {
    next_id: u32,
    map: HashMap<u32, Arc<Stream>>,
    closed: bool,
}

struct ConnShared {
    srv: Arc<ControlServer>,
    /// 收工用的第三句柄（shutdown 解阻塞 reader 的阻塞读）。
    ctl_sock: Mutex<std::os::unix::net::UnixStream>,
    closed: AtomicBool,
    handshook: AtomicBool,
    inflight: AtomicI32,
    req_tx: std::sync::mpsc::Sender<RequestBody>,
    out: Mutex<OutQueue>,
    out_cv: Condvar,
    sub: Mutex<Option<Arc<Subscriber>>>,
    streams: Mutex<StreamTable>,
}

impl ConnShared {
    /// 控制帧入 high 队列（有界阻塞：队列满 = 写停滞/前端不读病态——等 writer 消化
    /// 或连接收工；**不丢帧**——丢 rsp = 前端永久等 corr，丢 SubConfirm = 门闩永不清
    /// 〔影子订阅队列堆到 512 → overrun 断连〕。Go sendHigh/sendConfirm 同款语义；
    /// 写停滞看门狗（30s 写超时断连）是最终兜底）。
    fn push_high(&self, item: HighItem) {
        let mut q = self.out.lock().unwrap_or_else(|e| e.into_inner());
        while q.high.len() >= HIGH_QUEUE && !q.done && !matches!(item, HighItem::Bye { .. }) {
            q = self.out_cv.wait_timeout(q, Duration::from_millis(100)).unwrap_or_else(|e| e.into_inner()).0;
        }
        if q.high.len() >= HIGH_QUEUE && q.done && !matches!(item, HighItem::Bye { .. }) {
            return; // 收工中且队列仍满：尽力语义放弃（Bye 不放弃）
        }
        q.high.push_back(item);
        drop(q);
        self.out_cv.notify_all();
    }

    /// 收工连接（幂等）。reason 非空时尽力先发 goodbye（有界等待真写出 ≤500ms）。
    fn close(&self, reason: &str) {
        if self.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        if !reason.is_empty() {
            let (ack_tx, ack_rx) = std::sync::mpsc::channel();
            self.push_high(HighItem::Bye {
                frame: encode_json_frame(Op::Goodbye, &GoodbyeBody { reason: reason.to_owned() }),
                ack: ack_tx,
            });
            let _ = ack_rx.recv_timeout(Duration::from_millis(500));
        }
        {
            let mut q = self.out.lock().unwrap_or_else(|e| e.into_inner());
            q.done = true;
            drop(q);
            self.out_cv.notify_all();
        }
        // shutdown socket：解阻塞 reader 的阻塞读（读超时只是节拍兜底）。
        let _ = self
            .ctl_sock
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .shutdown(std::net::Shutdown::Both);
        // 流面收口：先置 closed 拦下拨号窗口内的晚注册，再逐流 teardown（不发
        // end——连接级断开与流级 end 可区分）。
        {
            let mut st = self.streams.lock().unwrap_or_else(|e| e.into_inner());
            st.closed = true;
            for (_, s) in st.map.drain() {
                s.teardown();
            }
        }
        let sub = self.sub.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(sub) = sub {
            self.srv.bus.unsubscribe(&sub);
            sub.set_latched(false);
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::SeqCst)
    }

    fn fatal(&self, code: &str) {
        self.close(code);
    }

    fn wake_writer(&self) {
        self.out_cv.notify_all();
    }

    // ---------- reader ----------

    fn reader_main(&self, mut sock: std::os::unix::net::UnixStream) {
        let mut buf: Vec<u8> = Vec::new(); // 半帧缓冲（读超时中断后的续读面）
        loop {
            let head = match read_head(&mut sock, &mut buf) {
                Ok(Some(h)) => h,
                Ok(None) => {
                    // 读超时节拍（500ms）：复检收工，否则继续。
                    if self.is_closed() {
                        return;
                    }
                    continue;
                }
                Err(FrameError::Overlong { .. }) => {
                    self.fatal(vocab::CODE_BAD_FRAME); // 长度超限（读 body 前拒绝）
                    return;
                }
                Err(_) => return, // EOF/IO 错误 = 连接结束
            };
            let body = match read_body_buf(&mut sock, &mut buf, head) {
                Ok(b) => b,
                Err(_) => return, // 半帧（声明了 body 却断流）= 连接结束
            };
            let Some(op) = Op::from_code(head.op) else {
                self.fatal(vocab::CODE_BAD_FRAME); // 非法 op（预留段/未知值）
                return;
            };
            match op {
                Op::Hello => {
                    if self.handshook.load(Ordering::SeqCst) {
                        return; // 协议次序错乱：无词表值，裸断
                    }
                    if !self.handle_hello(&body) {
                        return;
                    }
                }
                Op::Goodbye => return, // 前端礼貌告别：正常收工
                Op::Req => {
                    if !self.handshook.load(Ordering::SeqCst) {
                        return;
                    }
                    self.handle_request_frame(&body);
                }
                Op::StreamData => {
                    if !self.handshook.load(Ordering::SeqCst) {
                        return;
                    }
                    self.handle_stream_data(&body);
                }
                // 服务端方向的帧从前端来：次序错乱，裸断。
                Op::Welcome | Op::Reload | Op::Rsp | Op::Evt | Op::StreamEnd => return,
            }
            if self.is_closed() {
                return;
            }
        }
    }

    /// 握手：正常 → welcome；版本不匹配 → reload(proto_mismatch) 后关闭（锁步
    /// 哲学：唯一协商位，无降级路径）。返回 false = reader 收工。
    fn handle_hello(&self, body: &[u8]) -> bool {
        let hello: HelloBody = match serde_json::from_slice(body) {
            Ok(h) => h,
            Err(_) => {
                self.fatal(vocab::CODE_BAD_JSON);
                return false;
            }
        };
        if hello.frontend.kind.is_empty() || hello.frontend.name.is_empty() {
            self.fatal(vocab::CODE_BAD_REQUEST); // 前端标识缺失（握手无 rsp 通道）
            return false;
        }
        if hello.proto_version != frame::PROTO_VERSION {
            (self.srv.logf)(&format!(
                "control: 前端 {}/{} 协议版本 {} 不匹配（本端 {}）——reload",
                hello.frontend.kind, hello.frontend.name, hello.proto_version, frame::PROTO_VERSION
            ));
            let (ack_tx, ack_rx) = std::sync::mpsc::channel();
            self.push_high(HighItem::Bye {
                frame: encode_json_frame(
                    Op::Reload,
                    &ReloadBody { reason: vocab::RELOAD_PROTO_MISMATCH.to_owned() },
                ),
                ack: ack_tx,
            });
            let _ = ack_rx.recv_timeout(Duration::from_millis(500));
            self.close("");
            return false;
        }
        self.push_high(HighItem::Frame(encode_json_frame(
            Op::Welcome,
            &WelcomeBody {
                server_version: self.srv.version.clone(),
                generation: self.srv.generation().to_owned(),
                server_seq: self.srv.bus.current_seq(),
            },
        )));
        self.handshook.store(true, Ordering::SeqCst);
        (self.srv.logf)(&format!(
            "control: 前端已接入（{}/{} {}）",
            hello.frontend.kind, hello.frontend.name, hello.frontend.version
        ));
        true
    }

    /// 解请求帧：控制类 body 非法 JSON → bad_json 断连；合法 → 入每连接有界请求
    /// 队列（在途〔含工位执行中〕超 32 = 前端失控流水线 → goodbye(overrun) 断连）。
    fn handle_request_frame(&self, body: &[u8]) {
        let req: RequestBody = match serde_json::from_slice(body) {
            Ok(r) => r,
            Err(_) => {
                self.fatal(vocab::CODE_BAD_JSON);
                return;
            }
        };
        if self.inflight.fetch_add(1, Ordering::SeqCst) + 1 > MAX_INFLIGHT {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            self.fatal(vocab::GOODBYE_OVERRUN);
            return;
        }
        if self.req_tx.send(req).is_err() {
            self.inflight.fetch_sub(1, Ordering::SeqCst); // 工位已退（连接收工中）
        }
    }

    /// 上行数据帧（前端→后端透传）。未知/已关闭流：回执 no_stream（corr=0 保留值
    /// = 服务端主动通知）；连接不断连、其它流不受影响。在册流：非阻塞转投进每流
    /// 上行有界队列（32 帧 / 512KiB 双界取先到，超界收流 gone——「慢流 MUST NOT
    /// 阻塞控制帧/事件/其它流」在上行方向同样成立）。
    fn handle_stream_data(&self, body: &[u8]) {
        let (id, payload) = match frame::decode_stream_body(body) {
            Ok(v) => v,
            Err(_) => {
                self.fatal(vocab::CODE_BAD_FRAME);
                return;
            }
        };
        let Some(st) = self.lookup_stream(id) else {
            self.reply_json(
                0,
                Some(serde_json::json!({
                    "op": Op::StreamData.code(),
                    "streamId": id,
                    "error": vocab::CODE_NO_STREAM,
                })),
                None,
            );
            (self.srv.logf)(&format!(
                "control: 流 {id} 不在册（已关？），上行 {} 字节被拒",
                payload.len()
            ));
            return;
        };
        let over = {
            let mut up = st.up_buf.lock().unwrap_or_else(|e| e.into_inner());
            if st.done.load(Ordering::SeqCst) {
                false
            } else if up.len() >= UP_WORKER_ITEMS {
                true
            } else {
                up.push_back(payload.to_vec());
                st.up_cv.notify_all();
                false
            }
        };
        if over {
            st.finish(vocab::STREAM_END_GONE);
            return;
        }
        // 字节界（双界取先到）：入队后核对（超界收流——已入队部分由收流机器统一兜底）。
        if st.up_bytes.fetch_add(payload.len() as i64, Ordering::SeqCst) + payload.len() as i64
            > UP_WORKER_BYTES
        {
            st.finish(vocab::STREAM_END_GONE);
        }
    }

    fn lookup_stream(&self, id: u32) -> Option<Arc<Stream>> {
        self.streams.lock().unwrap_or_else(|e| e.into_inner()).map.get(&id).cloned()
    }

    // ---------- dispatcher（固定请求工位） ----------

    fn dispatcher_main(self: &Arc<Self>, req_rx: std::sync::mpsc::Receiver<RequestBody>) {
        loop {
            let req = match req_rx.recv_timeout(Duration::from_millis(250)) {
                Ok(r) => r,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if self.is_closed() {
                        return;
                    }
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
            };
            if req.op == vocab::OpName::StreamOpen.as_str() {
                // stream.open：整条移交拨号执行体（30s 级拨号不占工位串行位——在途
                // 计数随本调用原子转移：dispatch 不减、拨号收尾减）。
                let conn = Arc::clone(self);
                let _ = std::thread::Builder::new()
                    .name("hw-ctl-open".to_owned())
                    .spawn(move || conn.run_stream_open(req));
                continue;
            }
            self.dispatch(req);
            self.inflight.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// 工位执行一条请求（非 stream.open）。
    fn dispatch(&self, req: RequestBody) {
        use vocab::OpName as O;
        if self.srv.shutting_down() {
            self.reply_err(req.corr, OpError::code(vocab::CODE_SHUTTING_DOWN));
            return;
        }
        let Some(op) = O::parse(&req.op) else {
            // 未知操作名：稳定错误码、连接不中断。
            self.reply_err(req.corr, OpError::code(vocab::CODE_UNKNOWN_OP));
            return;
        };
        let args = req.args.clone().unwrap_or(serde_json::Value::Null);
        match op {
            O::DaemonStatus => self.op_daemon_status(req.corr),
            O::HostAdd => self.op_host_add(req.corr, &args),
            O::HostRemove => self.op_host_remove(req.corr, &args),
            O::HostList => self.op_host_list(req.corr),
            O::SnapshotGet => self.op_snapshot_get(req.corr),
            O::EventsSubscribe => self.op_subscribe(req.corr, &args),
            O::EventsUnsubscribe => self.op_unsubscribe(req.corr, &args),
            O::StreamOpen => {} // dispatcher 已分流到 run_stream_open（理论不可达）
            O::StreamClose => self.op_stream_close(req.corr, &args),
            // 承载面 9 op（forward/socks/speedtest 托管——D-1 实装；语义在 carriers，
            // 本层只做载荷解析 + not_ready 门 + 错误映射）。
            O::ForwardAdd => self.op_forward_add(req.corr, &args),
            O::ForwardRemove => self.op_forward_remove(req.corr, &args),
            O::ForwardList => self.op_forward_list(req.corr, &args),
            O::SocksOn => self.op_socks_on(req.corr, &args),
            O::SocksOff => self.op_socks_off(req.corr, &args),
            O::SocksStatus => self.op_socks_status(req.corr),
            O::SpeedtestStart => self.op_speedtest_start(req.corr, &args),
            O::SpeedtestStatus => self.op_speedtest_status(req.corr, &args),
            O::SpeedtestCancel => self.op_speedtest_cancel(req.corr, &args),
            O::ServeStart => self.role_op(req.corr, RoleOp::ServeStart),
            O::ServeStop => self.role_op(req.corr, RoleOp::ServeStop),
            O::ServeRestart => self.role_op(req.corr, RoleOp::ServeRestart),
            O::ServeStatus => self.role_op(req.corr, RoleOp::ServeStatus),
            O::ServeToken => self.role_op(req.corr, RoleOp::ServeToken),
            O::RelayStart => self.role_op(req.corr, RoleOp::RelayStart),
            O::RelayStop => self.role_op(req.corr, RoleOp::RelayStop),
            O::RelayRestart => self.role_op(req.corr, RoleOp::RelayRestart),
            O::RelayStatus => self.role_op(req.corr, RoleOp::RelayStatus),
            O::RelayToken => self.role_op(req.corr, RoleOp::RelayToken),
        }
    }

    // ---------- op 实现 ----------

    /// parseArgs 同义：载荷字段缺失/类型不符 → bad_request（不断连）。
    fn parse_args<T: serde::de::DeserializeOwned>(
        &self,
        args: &serde_json::Value,
    ) -> Result<T, OpError> {
        serde_json::from_value(args.clone()).map_err(|_| OpError::code(vocab::CODE_BAD_REQUEST))
    }

    fn gate_not_ready(&self, corr: u64) -> bool {
        if self.srv.backend.not_ready() {
            self.reply_err(corr, OpError::code(vocab::CODE_NOT_READY));
            true
        } else {
            false
        }
    }

    fn op_daemon_status(&self, corr: u64) {
        // daemon.status 不受 not_ready 挡：版本/代际/角色面是骨架信息（CLI「守护
        // 进程未运行时报可行动错误」依赖骨架可应答）。
        self.reply_json(
            corr,
            Some(
                serde_json::to_value(DaemonStatusResult {
                    server_version: self.srv.backend.server_version().to_owned(),
                    generation: self.srv.generation().to_owned(),
                    seq: self.srv.bus.current_seq(),
                    pid: Some(std::process::id() as i32),
                    roles: self.srv.backend.roles_status(),
                    hosts: self.srv.backend.host_states(),
                    demand: None,
                })
                .expect("普通值类型序列化不可失败"),
            ),
            None,
        );
    }

    fn op_host_add(&self, corr: u64, args: &serde_json::Value) {
        let a: HostAddArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.token.is_empty() {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        match self.srv.backend.add_host(a.name.as_deref().unwrap_or(""), &a.token, a.force) {
            Ok(res) => self.reply_json(corr, Some(serde_json::to_value(&res).unwrap()), None),
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    fn op_host_remove(&self, corr: u64, args: &serde_json::Value) {
        let a: HostRemoveArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        match self.srv.backend.remove_host(&a.host) {
            Ok(()) => self.reply_json(corr, Some(serde_json::json!({"removed": true})), None),
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    fn op_host_list(&self, corr: u64) {
        if self.gate_not_ready(corr) {
            return;
        }
        let r = HostListResult { hosts: self.srv.backend.host_briefs() };
        self.reply_json(corr, Some(serde_json::to_value(&r).unwrap()), None);
    }

    /// 快照：**先读总线当前 seq、后读宿主快照**（FIX-03 次序——seq 先读的失败模式
    /// 是事件 seq ≤ 游标且不在快照里 ⇒ 永久陈旧；seq 先读则该窗口内事件必回放，
    /// 可能重复、由同键幂等覆盖消化——at-least-once 口径）。
    fn op_snapshot_get(&self, corr: u64) {
        if self.gate_not_ready(corr) {
            return;
        }
        let seq = self.srv.bus.current_seq(); // 先取号（保「不漏」）
        let hosts = self.srv.backend.host_states();
        self.reply_json(
            corr,
            Some(
                serde_json::to_value(SnapshotResult {
                    seq,
                    generation: self.srv.generation().to_owned(),
                    hosts,
                })
                .unwrap(),
            ),
            None,
        );
    }

    fn op_subscribe(&self, corr: u64, args: &serde_json::Value) {
        let a: SubscribeArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        let Some(domains) = a.domains else {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        };
        let sub = {
            let mut s = self.sub.lock().unwrap_or_else(|e| e.into_inner());
            s.get_or_insert_with(|| self.srv.bus.new_subscriber()).clone()
        };
        // 门闩置位在 bus.subscribe 之前（在线投递从 Subscribe 前开始就堆队列）。
        sub.set_latched(true);
        let r = self.srv.bus.subscribe(
            &sub,
            &domains,
            a.cursor,
            a.generation.as_deref().unwrap_or(""),
            a.view.as_deref().unwrap_or(""),
        );
        let rsp = match r {
            Ok(()) => {
                let result = SubscribeResult {
                    domains: domains.clone(), // 替换语义下生效集合恰 = 本次声明（回显即生效域集合）
                    cursor: self.srv.bus.current_seq(),
                    view: a.view.clone(),
                    generation: self.srv.generation().to_owned(),
                };
                response_body(corr, Some(serde_json::to_value(&result).unwrap()), None)
            }
            Err(SubscribeErr::Stale(_)) => {
                response_body(corr, None, Some(&OpError::code(vocab::CODE_CURSOR_STALE)))
            }
            // 超前游标 / 词表外域 / 代际缺失 → bad_request。
            Err(SubscribeErr::Future) | Err(SubscribeErr::Other(_)) => {
                response_body(corr, None, Some(&OpError::code(vocab::CODE_BAD_REQUEST)))
            }
        };
        // 确认（成功或错误）一律走 SubConfirm 复合项——错误应答同样清门闩（Go r2
        // 新-1：否则订阅队列无人消费，一次 cursor_stale 被放大成 overrun 断连）。
        self.push_high(HighItem::SubConfirm { frame: encode_json_frame(Op::Rsp, &rsp), sub });
    }

    fn op_unsubscribe(&self, corr: u64, args: &serde_json::Value) {
        let a: UnsubscribeArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        let Some(domains) = a.domains else {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        };
        let sub = self.sub.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(sub) = sub {
            self.srv.bus.unsubscribe_domains(&sub, &domains);
        }
        let r = UnsubscribeResult { domains };
        self.reply_json(corr, Some(serde_json::to_value(&r).unwrap()), None);
    }

    fn op_stream_close(&self, corr: u64, args: &serde_json::Value) {
        let a: StreamCloseArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        match self.lookup_stream(a.stream_id) {
            None => self.reply_err(corr, OpError::code(vocab::CODE_NO_STREAM)),
            Some(st) => {
                st.finish(vocab::STREAM_END_CLOSED);
                self.reply_json(corr, Some(serde_json::json!({"closed": true})), None);
            }
        }
    }

    // ---------- 承载面 9 op（语义在 Backend/carriers；本层 = parseArgs + 门 + 映射） ----------

    fn op_forward_add(&self, corr: u64, args: &serde_json::Value) {
        let a: ForwardAddArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() || a.listen == 0 {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        let rule = super::ForwardRule {
            host: a.host,
            listen: a.listen,
            target_ip: a.target_ip,
            target_port: a.target_port,
        };
        match self.srv.backend.forward_add(rule) {
            Ok(state) => {
                let brief = forward_brief_of(&state);
                self.reply_json(
                    corr,
                    Some(serde_json::to_value(ForwardAddResult { rule: brief }).unwrap()),
                    None,
                )
            }
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    fn op_forward_remove(&self, corr: u64, args: &serde_json::Value) {
        let a: ForwardRemoveArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() || a.listen == 0 {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        match self.srv.backend.forward_remove(&a.host, a.listen) {
            Ok(()) => self.reply_json(
                corr,
                Some(serde_json::to_value(ForwardRemoveResult { removed: true }).unwrap()),
                None,
            ),
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    fn op_forward_list(&self, corr: u64, args: &serde_json::Value) {
        let a: ForwardListArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if self.gate_not_ready(corr) {
            return;
        }
        let forwards: Vec<ForwardRuleBrief> =
            self.srv.backend.forward_list(&a.host).iter().map(forward_brief_of).collect();
        self.reply_json(
            corr,
            Some(serde_json::to_value(ForwardListResult { forwards }).unwrap()),
            None,
        );
    }

    fn op_socks_on(&self, corr: u64, args: &serde_json::Value) {
        let a: SocksOnArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        match self.srv.backend.socks_on(&a.host, a.listen) {
            Ok(listen) => self.reply_json(
                corr,
                Some(serde_json::to_value(SocksOnResult { listen }).unwrap()),
                None,
            ),
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    fn op_socks_off(&self, corr: u64, args: &serde_json::Value) {
        let a: SocksOffArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        match self.srv.backend.socks_off(&a.host) {
            Ok(listen) => self.reply_json(
                corr,
                Some(serde_json::to_value(SocksOffResult { listen }).unwrap()),
                None,
            ),
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    fn op_socks_status(&self, corr: u64) {
        if self.gate_not_ready(corr) {
            return;
        }
        let socks: Vec<SocksBrief> = self
            .srv
            .backend
            .socks_states()
            .into_iter()
            .map(|s| SocksBrief {
                host: s.host,
                on: s.on,
                listen: s.listen,
                conns: s.conns,
                err: s.err,
            })
            .collect();
        self.reply_json(corr, Some(serde_json::to_value(SocksStatusResult { socks }).unwrap()), None);
    }

    fn op_speedtest_start(&self, corr: u64, args: &serde_json::Value) {
        let a: SpeedtestStartArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        let p = super::SpeedtestParams::from_ms(a.down_ms, a.up_ms, a.warmup_ms, a.streams, a.wait_ms);
        match self.srv.backend.speedtest_start(&a.host, p) {
            Ok(ack) => self.reply_json(
                corr,
                Some(
                    serde_json::to_value(SpeedtestStartAck {
                        phase: ack.phase.to_owned(),
                        reason: ack.reason.unwrap_or_default().to_owned(),
                    })
                    .unwrap(),
                ),
                None,
            ),
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    fn op_speedtest_status(&self, corr: u64, args: &serde_json::Value) {
        let a: SpeedtestStatusArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        // 无运行面（从未 start / 守护进程重启后）= idle 形态（Go 同义——CLI 的
        // 「运行面丢失」判据以 idle + 非 waiting + 无终态为准）。
        let mut res = SpeedtestStatusResult {
            host: a.host.clone(),
            waiting: false,
            wait_remain_ms: 0,
            phase: "idle".to_owned(),
            reason: String::new(),
            bytes: 0,
            elapsed_ms: 0,
            result: None,
        };
        match self.srv.backend.speedtest_status(&a.host) {
            Err(e) => return self.reply_err(corr, map_backend_err(e)),
            Ok(None) => {}
            Ok(Some(st)) => {
                res.waiting = st.waiting;
                res.wait_remain_ms = st.wait_remain_ms;
                res.phase = st.phase;
                res.bytes = st.bytes;
                res.elapsed_ms = st.elapsed_ms;
                if let Some(r) = st.result {
                    res.result = Some(SpeedtestResultBrief {
                        ok: r.ok,
                        reason: r.reason,
                        msg: r.msg,
                        down_bps: r.down_bps,
                        up_bps: r.up_bps,
                        usage_down: r.usage_down,
                        usage_up: r.usage_up,
                        wall_ms: r.wall_ms as i64,
                    });
                }
            }
        }
        self.reply_json(corr, Some(serde_json::to_value(&res).unwrap()), None);
    }

    fn op_speedtest_cancel(&self, corr: u64, args: &serde_json::Value) {
        let a: SpeedtestCancelArgs = match self.parse_args(args) {
            Ok(a) => a,
            Err(e) => return self.reply_err(corr, e),
        };
        if a.host.is_empty() {
            return self.reply_err(corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.gate_not_ready(corr) {
            return;
        }
        match self.srv.backend.speedtest_cancel(&a.host) {
            Ok(()) => self.reply_json(
                corr,
                Some(serde_json::to_value(SpeedtestCancelResult { cancelled: true }).unwrap()),
                None,
            ),
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    // ---------- serve/relay 角色管理 op（幂等语义在成功载荷呈现，不借道错误码） ----------

    fn role_op(&self, corr: u64, op: RoleOp) {
        match self.srv.backend.role_op(op) {
            Ok(RoleOpOut::Action(r)) => {
                self.reply_json(corr, Some(serde_json::to_value(&r).unwrap()), None)
            }
            Ok(RoleOpOut::Status(v)) => self.reply_json(corr, Some(v), None),
            Ok(RoleOpOut::Token(r)) => {
                self.reply_json(corr, Some(serde_json::to_value(&r).unwrap()), None)
            }
            Err(e) => self.reply_err(corr, map_backend_err(e)),
        }
    }

    // ---------- stream.open（独立拨号执行体） ----------

    fn run_stream_open(self: &Arc<Self>, req: RequestBody) {
        // 在途计数随 dispatcher 原子转移到本执行体，**出口**才减（Go defer 同义）——
        // 30s 级拨号全程占一计（「不无界 spawn」的立目不因拨号豁免而退化）。
        let _inflight_guard = InflightGuard(self);
        if self.srv.shutting_down() {
            self.reply_err(req.corr, OpError::code(vocab::CODE_SHUTTING_DOWN));
            return;
        }
        let args: StreamOpenArgs =
            match serde_json::from_value(req.args.clone().unwrap_or(serde_json::Value::Null)) {
                Ok(a) => a,
                Err(_) => return self.reply_err(req.corr, OpError::code(vocab::CODE_BAD_REQUEST)),
            };
        if args.kind != vocab::STREAM_KIND_TERM && args.kind != vocab::STREAM_KIND_FILES {
            return self.reply_err(req.corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if args.host.is_empty() {
            return self.reply_err(req.corr, OpError::code(vocab::CODE_BAD_REQUEST));
        }
        if self.srv.backend.not_ready() {
            return self.reply_err(req.corr, OpError::code(vocab::CODE_NOT_READY));
        }
        {
            let st = self.streams.lock().unwrap_or_else(|e| e.into_inner());
            if st.map.len() >= DEFAULT_MAX_STREAMS {
                return self.reply_err(req.corr, OpError::code(vocab::CODE_STREAM_REFUSED));
            }
        }
        let backend = match self.srv.backend.dial_stream(&args.kind, &args.host) {
            Ok(b) => b,
            Err(e) => {
                let code = match e {
                    super::proto::BackendErr::NoHost => vocab::CODE_NO_HOST,
                    super::proto::BackendErr::NoSession => vocab::CODE_STREAM_REFUSED,
                    ref other => {
                        (self.srv.logf)(&format!(
                            "control: stream.open({}/{}) 拨号失败：{other}",
                            args.kind, args.host
                        ));
                        vocab::CODE_STREAM_REFUSED // 主机不可达等 = 被拒
                    }
                };
                return self.reply_err(req.corr, OpError::code(code));
            }
        };
        // 拨号完成后的注册窗口：工位配额 → 连接收工/上限竞争三闸（晚注册的流不在
        // close 的 teardown 清单里会成孤儿——先闸后插表）。
        let quota = self.srv.up_workers.fetch_add(1, Ordering::SeqCst) + 1;
        let stream = {
            let mut st = self.streams.lock().unwrap_or_else(|e| e.into_inner());
            if st.closed || st.map.len() >= DEFAULT_MAX_STREAMS || quota > DEFAULT_MAX_UP_WORKERS {
                drop(st);
                self.srv.up_workers.fetch_sub(1, Ordering::SeqCst);
                backend.close();
                return self.reply_err(req.corr, OpError::code(vocab::CODE_STREAM_REFUSED));
            }
            st.next_id += 1;
            let id = st.next_id;
            let s = Arc::new(Stream::new(id, Arc::downgrade(self) as Weak<ConnShared>, backend));
            st.map.insert(id, Arc::clone(&s));
            s
        };
        self.reply_json(
            req.corr,
            Some(serde_json::to_value(StreamOpenResult { stream_id: stream.id }).unwrap()),
            None,
        );
        stream.spawn_pumps();
    }

    // ---------- 应答 ----------

    fn reply_json(&self, corr: u64, result: Option<serde_json::Value>, err: Option<&OpError>) {
        let body = response_body(corr, result, err);
        self.push_high(HighItem::Frame(encode_json_frame(Op::Rsp, &body)));
    }

    fn reply_err(&self, corr: u64, e: OpError) {
        self.reply_json(corr, None, Some(&e));
    }

    // ---------- writer（唯一 socket 写者，优先级排空） ----------

    fn writer_main(&self, mut sock: std::os::unix::net::UnixStream) {
        loop {
            if self.is_closed() {
                // 收工：high 队列里可能还有告别帧——排空一次后退出。
                let q = self.out.lock().unwrap_or_else(|e| e.into_inner());
                if q.high.is_empty() {
                    return;
                }
            }
            // ① 控制（最高优先级）。
            if !self.drain_high(&mut sock) {
                return;
            }
            // ② 一条事件（订阅者在线队列；门闩期跳过——回放段在 SubConfirm 侧）。
            match self.write_one_event(&mut sock) {
                EventStep::Wrote => continue, // 回循环头（控制优先）
                EventStep::Fatal => return,
                EventStep::Idle => {}
            }
            // ③ 流公平轮询一条。
            if self.write_one_stream_item(&mut sock) {
                continue;
            }
            // 无消费来源：阻塞等待（100ms 节拍 = 事件面轮询 + overrun/closed 复检——
            // 事件来自订阅者队列，无入队通知面，节拍轮询是刻意取舍）。
            {
                let q = self.out.lock().unwrap_or_else(|e| e.into_inner());
                if !q.high.is_empty() || q.done {
                    continue;
                }
                let _unused = self
                    .out_cv
                    .wait_timeout(q, Duration::from_millis(100))
                    .unwrap_or_else(|e| e.into_inner());
            }
        }
    }

    /// 排空 high 队列；false = 写失败（连接收工）。
    fn drain_high(&self, sock: &mut std::os::unix::net::UnixStream) -> bool {
        loop {
            let item = {
                let mut q = self.out.lock().unwrap_or_else(|e| e.into_inner());
                match q.high.pop_front() {
                    Some(i) => i,
                    None => return true,
                }
            };
            match item {
                HighItem::Frame(f) => {
                    if !self.write_bytes(sock, &f) {
                        return false;
                    }
                }
                HighItem::SubConfirm { frame, sub } => {
                    // 「确认 → 回放 → 在线」三段次序（单 writer 天然保序）：写确认 →
                    // 清门闩 → 写回放段 → 恢复在线消费。
                    if !self.write_bytes(sock, &frame) {
                        return false;
                    }
                    sub.set_latched(false);
                    for ev in sub.take_replay() {
                        let f = encode_json_frame(
                            Op::Evt,
                            &EventBody {
                                seq: ev.seq,
                                domain: ev.domain,
                                kind: ev.kind,
                                payload: Some(ev.payload),
                            },
                        );
                        if !self.write_bytes(sock, &f) {
                            return false;
                        }
                    }
                }
                HighItem::Bye { frame, ack } => {
                    let ok = self.write_bytes(sock, &frame);
                    let _ = ack.send(());
                    if !ok {
                        return false;
                    }
                }
            }
        }
    }

    /// 事件非阻塞一条。门闩置位 = 跳过；overrun = `goodbye(overrun)` 断连（不静默
    /// 丢事件）。
    fn write_one_event(&self, sock: &mut std::os::unix::net::UnixStream) -> EventStep {
        let sub = self.sub.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(sub) = sub else { return EventStep::Idle };
        if sub.overrun() {
            // 订阅 overrun（总线停投）：goodbye(overrun) 断连——慢消费者被断连重同步。
            self.fatal_from_writer(sock, vocab::GOODBYE_OVERRUN);
            return EventStep::Fatal;
        }
        if sub.latched() {
            return EventStep::Idle;
        }
        match sub.try_pop() {
            Some(ev) => {
                let f = encode_json_frame(
                    Op::Evt,
                    &EventBody {
                        seq: ev.seq,
                        domain: ev.domain,
                        kind: ev.kind,
                        payload: Some(ev.payload),
                    },
                );
                if !self.write_bytes(sock, &f) {
                    return EventStep::Fatal;
                }
                EventStep::Wrote
            }
            None => EventStep::Idle,
        }
    }

    /// 流公平轮询一条（data 或 end 标记——end 与该流数据同队列 FIFO：「end 之前的
    /// 数据先送达、end 之后绝无该流数据」）。返回 true = 有产出。
    fn write_one_stream_item(&self, sock: &mut std::os::unix::net::UnixStream) -> bool {
        let streams: Vec<Arc<Stream>> = {
            let st = self.streams.lock().unwrap_or_else(|e| e.into_inner());
            st.map.values().cloned().collect()
        };
        for st in streams {
            let item = st.down.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
            if let Some(item) = item {
                match item {
                    StreamOut::Data(d) => {
                        let _ = self.write_bytes(
                            sock,
                            &frame::encode_frame(
                                Op::StreamData,
                                &frame::encode_stream_body(st.id, &d),
                            ),
                        );
                    }
                    StreamOut::End(reason) => {
                        let _ = self.write_bytes(
                            sock,
                            &encode_json_frame(
                                Op::StreamEnd,
                                &StreamEndBody { stream_id: st.id, reason },
                            ),
                        );
                        self.streams.lock().unwrap_or_else(|e| e.into_inner()).map.remove(&st.id);
                    }
                }
                return true;
            }
        }
        false
    }

    /// 写一帧字节（写停滞看门狗 = socket 写超时 30s；失败 = 连接收工）。
    fn write_bytes(&self, sock: &mut std::os::unix::net::UnixStream, f: &[u8]) -> bool {
        match sock.write_all(f) {
            Ok(()) => true,
            Err(_) => {
                self.close("");
                false
            }
        }
    }

    /// writer 侧致命路径（overrun：告别帧同步写出后断连——走 high 队列已无意义）。
    fn fatal_from_writer(&self, sock: &mut std::os::unix::net::UnixStream, reason: &str) {
        let _ = sock.set_write_timeout(Some(Duration::from_secs(2)));
        let f = encode_json_frame(Op::Goodbye, &GoodbyeBody { reason: reason.to_owned() });
        let _ = sock.write_all(&f);
        self.close("");
    }
}

enum EventStep {
    Wrote,
    Fatal,
    Idle,
}

/// stream.open 拨号执行体的在途计数守卫（所有出口恰一次递减）。
struct InflightGuard<'a>(&'a ConnShared);

impl Drop for InflightGuard<'_> {
    fn drop(&mut self) {
        self.0.inflight.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---------- 流 ----------

enum StreamOut {
    Data(Vec<u8>),
    End(String),
}

struct Stream {
    id: u32,
    /// 连接弱引用（避免 Stream ←→ ConnShared 的 Arc 环；连接收工后 upgrade 失败 =
    /// 只做本地收尾）。writer 消费 down 队列不依赖本引用——背压通知走 10ms 节拍。
    conn: Weak<ConnShared>,
    backend: Arc<dyn super::StreamConn>,
    /// 后端→前端（writer 消费；有界 STREAM_QUEUE_ITEMS——满则 backendPump 暂停读
    /// 后端 = 背压传导到后端 TCP，绝不阻塞控制帧/事件/其它流）。
    down: Mutex<VecDeque<StreamOut>>,
    /// 前端→后端（up 线程消费；双界 32 帧 / 512KiB）。
    up_buf: Mutex<VecDeque<Vec<u8>>>,
    up_bytes: AtomicI64,
    up_cv: Condvar,
    done: AtomicBool,
    /// 0 = 未收尾；1 = finish（发 end）；2 = teardown（不发 end）。CAS 保证恰一次。
    finish_once: AtomicU32,
}

use std::sync::atomic::AtomicU32;

impl Stream {
    fn new(id: u32, conn: Weak<ConnShared>, backend: Arc<dyn super::StreamConn>) -> Stream {
        Stream {
            id,
            conn,
            backend,
            down: Mutex::new(VecDeque::new()),
            up_buf: Mutex::new(VecDeque::new()),
            up_bytes: AtomicI64::new(0),
            up_cv: Condvar::new(),
            done: AtomicBool::new(false),
            finish_once: AtomicU32::new(0),
        }
    }

    /// 流终结（幂等，首个路径生效）：停泵（done + 关后端连接）→ end 标记尾入 down
    /// 队列（等积压排空——「先送数据后送 end」）。上行工位配额在此释放。
    fn finish(&self, reason: &str) {
        if self.finish_once.compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return;
        }
        self.release_quota();
        self.done.store(true, Ordering::SeqCst);
        self.up_cv.notify_all();
        self.backend.close();
        // end 尾入 down 队列：积压满则有界等排空（连接断开即弃）。
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            {
                let mut q = self.down.lock().unwrap_or_else(|e| e.into_inner());
                if q.len() < STREAM_QUEUE_ITEMS {
                    q.push_back(StreamOut::End(reason.to_owned()));
                    break;
                }
            }
            let timed_out = Instant::now() > deadline;
            let conn_gone = !self.conn.upgrade().is_some_and(|c| !c.is_closed());
            if conn_gone || timed_out {
                if timed_out {
                    // 死线触发（前端持续不读、积压排不动）：end 发不出也不许留僵尸
                    // 流占 DEFAULT_MAX_STREAMS 配额——摘出表（并发-7）。
                    if let Some(c) = self.conn.upgrade() {
                        c.streams.lock().unwrap_or_else(|e| e.into_inner()).map.remove(&self.id);
                    }
                }
                break; // 连接已断（end 发不出）：无意义等待，直接弃
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if let Some(c) = self.conn.upgrade() {
            c.wake_writer();
        }
    }

    /// 连接级收口：只停泵关后端，**不发 end**（连接没了；连接级断开与流级 end 可区分）。
    fn teardown(&self) {
        if self.finish_once.compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst).is_err() {
            return;
        }
        self.release_quota();
        self.done.store(true, Ordering::SeqCst);
        self.up_cv.notify_all();
        self.backend.close();
    }

    fn release_quota(&self) {
        if let Some(c) = self.conn.upgrade() {
            c.srv.up_workers.fetch_sub(1, Ordering::SeqCst);
        }
    }

    fn spawn_pumps(self: &Arc<Self>) {
        let down_s = Arc::clone(self);
        std::thread::Builder::new()
            .name("hw-ctl-strm-d".to_owned())
            .spawn(move || down_s.backend_pump())
            .expect("线程创建不可失败");
        let up_s = Arc::clone(self);
        std::thread::Builder::new()
            .name("hw-ctl-strm-u".to_owned())
            .spawn(move || up_s.upstream_pump())
            .expect("线程创建不可失败");
    }

    /// 后端→前端：读后端 → down 队列（满则暂停读后端 = 慢流背压，10ms 节拍复查
    /// done）；EOF → closed；读错 → gone；done → 流已终结。
    fn backend_pump(&self) {
        loop {
            let chunk = match self.backend.read_chunk() {
                Ok(v) if v.is_empty() => {
                    self.finish(vocab::STREAM_END_CLOSED); // 对端 EOF
                    return;
                }
                Ok(v) => v,
                Err(_) => {
                    self.finish(vocab::STREAM_END_GONE); // 后端连接错误（会话收工会体现为这里）
                    return;
                }
            };
            loop {
                if self.done.load(Ordering::SeqCst) {
                    return;
                }
                let mut q = self.down.lock().unwrap_or_else(|e| e.into_inner());
                if q.len() < STREAM_QUEUE_ITEMS {
                    q.push_back(StreamOut::Data(chunk));
                    break;
                }
                // 背压：等 writer 消化（节拍复查 done）。
                drop(q);
                std::thread::sleep(Duration::from_millis(10));
            }
            if let Some(c) = self.conn.upgrade() {
                c.wake_writer();
            }
            if self.done.load(Ordering::SeqCst) {
                return;
            }
        }
    }

    /// 前端→后端：up_buf → 写后端（写停滞/错误 = 对端不消费 → 收流 gone——停滞
    /// 预算在 StreamConn::write_chunk 内部承载，Go streamUpTimeout/upWorker 停滞
    /// 两预算在此合并为单写预算，同款终态）。
    fn upstream_pump(&self) {
        loop {
            let payload = {
                let mut up = self.up_buf.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    if let Some(p) = up.pop_front() {
                        break p;
                    }
                    if self.done.load(Ordering::SeqCst) {
                        return;
                    }
                    let (guard, _tmo) = self
                        .up_cv
                        .wait_timeout(up, Duration::from_millis(500))
                        .unwrap_or_else(|e| e.into_inner());
                    up = guard;
                }
            };
            self.up_bytes.fetch_sub(payload.len() as i64, Ordering::SeqCst);
            let mut off = 0;
            while off < payload.len() {
                match self.backend.write_chunk(&payload[off..]) {
                    Ok(n) => off += n,
                    Err(e) => {
                        if e.kind() == ErrorKind::TimedOut || e.kind() == ErrorKind::WouldBlock {
                            if let Some(c) = self.conn.upgrade() {
                                (c.srv.logf)(&format!(
                                    "control: 流 {} 上行停滞（后端不读）——收流（gone）",
                                    self.id
                                ));
                            }
                        }
                        self.finish(vocab::STREAM_END_GONE);
                        return;
                    }
                }
            }
        }
    }
}

// ---------- Backend 错误 → 错误码映射 ----------

/// ForwardState → wire brief（字段名/omitempty 与 Go ForwardRuleBrief 对齐）。
fn forward_brief_of(st: &super::ForwardState) -> ForwardRuleBrief {
    ForwardRuleBrief {
        host: st.rule.host.clone(),
        listen: st.rule.listen,
        target_ip: st.rule.target_ip.clone(),
        target_port: st.rule.target_port,
        state: st.state.clone(),
        err: st.err.clone(),
        conns: st.conns,
        rejected: st.rejected,
    }
}

fn map_backend_err(e: super::proto::BackendErr) -> OpError {
    match e {
        super::proto::BackendErr::HostExists => OpError::code(vocab::CODE_HOST_EXISTS),
        super::proto::BackendErr::BadToken(d) => OpError::with_detail(vocab::CODE_BAD_TOKEN, d),
        super::proto::BackendErr::NoHost => OpError::code(vocab::CODE_NO_HOST),
        super::proto::BackendErr::HostUnreachable => OpError::code(vocab::CODE_HOST_UNREACHABLE),
        super::proto::BackendErr::NoSession => OpError::code(vocab::CODE_STREAM_REFUSED),
        super::proto::BackendErr::RoleStopped => OpError::code(vocab::CODE_BAD_REQUEST),
        super::proto::BackendErr::BadStreamKind(_) => OpError::code(vocab::CODE_BAD_REQUEST),
        super::proto::BackendErr::Other(d) => OpError::with_detail(vocab::CODE_BAD_REQUEST, d),
    }
}

// ---------- 半帧缓冲读写（reader 的读超时节拍与 read_exact 组合；client 复用） ----------

/// 从 socket + 残余缓冲读 5 字节帧头。Ok(None) = 读超时节拍（无进展）。
pub(super) fn read_head(
    sock: &mut std::os::unix::net::UnixStream,
    buf: &mut Vec<u8>,
) -> Result<Option<frame::FrameHead>, FrameError> {
    while buf.len() < 5 {
        if !fill(sock, buf)? {
            return Ok(None);
        }
    }
    let head_bytes: [u8; 5] = buf.drain(..5).collect::<Vec<u8>>().try_into().unwrap();
    let fh = frame::FrameHead {
        op: head_bytes[0],
        n: u32::from_be_bytes([head_bytes[1], head_bytes[2], head_bytes[3], head_bytes[4]]),
    };
    let max = if fh.op == Op::StreamData.code() { frame::MAX_STREAM_BODY } else { frame::MAX_CONTROL_BODY };
    if fh.n as u64 > max as u64 {
        return Err(FrameError::Overlong { op: fh.op, len: fh.n, max });
    }
    Ok(Some(fh))
}

pub(super) fn read_body_buf(
    sock: &mut std::os::unix::net::UnixStream,
    buf: &mut Vec<u8>,
    head: frame::FrameHead,
) -> Result<Vec<u8>, FrameError> {
    while (buf.len() as u64) < head.n as u64 {
        if !fill(sock, buf)? {
            // 读超时不算断流（等下一拍）；EOF（真断）在 fill 里报错。
        }
    }
    Ok(buf.drain(..head.n as usize).collect())
}

/// 填一块；false = 本拍无数据（读超时节拍——10ms 缓冲后返回，防忙等）。
pub(super) fn fill(
    sock: &mut std::os::unix::net::UnixStream,
    buf: &mut Vec<u8>,
) -> Result<bool, FrameError> {
    let mut tmp = [0u8; 16 * 1024];
    loop {
        match sock.read(&mut tmp) {
            Ok(0) => {
                return Err(FrameError::Io(std::io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "对端关闭",
                )))
            }
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                return Ok(true);
            }
            Err(e) if e.kind() == ErrorKind::WouldBlock || e.kind() == ErrorKind::TimedOut => {
                std::thread::sleep(Duration::from_millis(10));
                return Ok(false);
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => return Err(FrameError::Io(e)),
        }
    }
}
