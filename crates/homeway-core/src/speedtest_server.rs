//! speedtest 服务端（R3；语义真源 `pkg/speedtest/speedtest.go` 的 Server 半边）。
//!
//! 客户端（手机核 / 测试 CLI）经隧道拨隧道IP:7803，拦截层按 LocalServices 映射转投到
//! 本服务（`<state>/speedtest.sock`，UDS 承载）。会话数据只在内存收发、不落盘；
//! 每连接一角色：role=recv（供下行：服务端自驱泵送）role=send（收上行：客户端泵、
//! START 后计窗内字节、FINISH 触发 report）。**速率读数永远由接收端报**。
//!
//! 线协议与客户端（`speedtest.rs`）同源：`[magic "SPED"][type u8][seq u32 LE]
//! [len u16 LE][crc32 IEEE LE][payload]`；受理/结算判据行（E13）逐串对齐。
//!
//! 限额（Go Limits.withDefaults）：并发 12（客户端上下行各 4 流 + 摘表时差余量）、
//! 单连接硬超时 30s、预热 ≤5s、窗口 ∈[100ms,15s]、data 载荷 ≤65535。

use std::io::{BufReader, BufWriter, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};

use homeway_quic::ServiceIntake;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::go_fmt::fmt_duration_go_ms;
use crate::speedtest::{
    crc32_ieee, zero_crc, HEADER, MAGIC, TYPE_DATA, TYPE_FINISH, TYPE_REPORT, TYPE_REQUEST,
    TYPE_START,
};

/// 并发测速连接上限（客户端 4+4 条流，留出下行摘表时差的余量——Go MaxConns 缺省）。
pub const MAX_CONNS: usize = 12;
/// 单连接硬超时缺省（覆盖 5s 预热 + 15s 窗口 + 控制帧余量）。
const CONN_TIMEOUT: Duration = Duration::from_secs(30);
/// 预热时长上限缺省。
const MAX_WARMUP: Duration = Duration::from_secs(5);
/// 窗口时长区间（下限是 Rust 侧既有约束，Go Limits 只有上限）。
const MIN_WINDOW: Duration = Duration::from_millis(100);
const MAX_WINDOW: Duration = Duration::from_secs(15);
/// data 载荷上限（u16 长度场）+ 服务端发送块（Go SendBlock）。
const MAX_BLOCK: usize = 65535;
/// 注入限额的硬上界（防 `Instant::now() + Duration` 溢出 panic——`Limits` 是 pub 面；
/// Go `time.Now().Add` 不 panic，但超长限额也没有实际用途）。
const LIMITS_MAX_TIMEOUT: Duration = Duration::from_secs(3600);
/// busy 拒绝路径的帧吞窗（Go `ReplyThenClose`：`now+2s` **绝对**期限）。
const BUSY_FRAME_BUDGET: Duration = Duration::from_secs(2);
/// busy 拒绝路径吞输入窗（Go `speedtest.go:434-440`：`now+1s` **绝对**期限——
/// per-syscall 续命会让滴流连接永久占住一条线程）。
const BUSY_DRAIN_BUDGET: Duration = Duration::from_secs(1);

/// 服务端限额（移植 Go `Limits` + `SetLimits`——硬超时/上限**可注入** ⇒ 可测；
/// 缺省值与生产常量同源）。`send_block` = 下行泵送块（Go `SendBlock`）。
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_conns: usize,
    pub conn_timeout: Duration,
    pub max_warmup: Duration,
    pub max_window: Duration,
    pub max_block: usize,
    pub send_block: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_conns: MAX_CONNS,
            conn_timeout: CONN_TIMEOUT,
            max_warmup: MAX_WARMUP,
            max_window: MAX_WINDOW,
            max_block: MAX_BLOCK,
            send_block: MAX_BLOCK,
        }
    }
}

impl Limits {
    /// Go `withDefaults` 同判点（0/越界回缺省；send_block 不得超过 max_block）。
    fn filled(mut self) -> Self {
        if self.max_conns == 0 {
            self.max_conns = MAX_CONNS;
        }
        if self.conn_timeout.is_zero() {
            self.conn_timeout = CONN_TIMEOUT;
        }
        if self.max_warmup.is_zero() {
            self.max_warmup = MAX_WARMUP;
        }
        if self.max_window.is_zero() {
            self.max_window = MAX_WINDOW;
        }
        if self.max_block == 0 || self.max_block > MAX_BLOCK {
            self.max_block = MAX_BLOCK;
        }
        if self.send_block == 0 || self.send_block > self.max_block {
            self.send_block = self.max_block;
        }
        // 防溢出 panic（注入面是 pub）
        for t in [&mut self.conn_timeout, &mut self.max_warmup, &mut self.max_window] {
            if *t > LIMITS_MAX_TIMEOUT {
                *t = LIMITS_MAX_TIMEOUT;
            }
        }
        self
    }
}

/// 会话计数（受理/拒绝——验收对账面；total 只在登记成功时 +1）。`next` = 会话号
/// 分配器（**单调自增、可 >MAX_CONNS**——Go connreg.Add 语义，评审 H3：此前用
/// 「当前在册数」冒充会话号，串行两轮都打 #1，判据行错位）。
#[derive(Default)]
pub struct SessStats {
    pub total: AtomicU64,
    pub rejected: AtomicU64,
    next: AtomicU64,
}

/// 测速服务（UDS 承载；Serve 由装配层在 listener 上拉起——照 files 的形状）。
pub struct SpeedtestServer {
    logf: crate::Logf,
    /// 限额（可注入——F6a；生产 = 缺省常量）。
    limits: Limits,
    /// 在册连接表（上限判定与登记同锁——受理/摘除的原子面）。
    conns: Arc<Mutex<CountingRegistry>>,
    pub stats: Arc<SessStats>,
}

struct CountingRegistry {
    live: usize,
    /// 在册会话句柄（会话号 → dup 句柄；F6b：`release(id)` 按会话收口——修前
    /// `release()` 清整表，任一会话结束会把其它在跑会话的句柄从账上抹掉）。
    conns: Vec<(u64, UnixStream)>,
}

impl CountingRegistry {
    /// 受理：满员 None；否则在册 +1 并返回**单调会话号**（评审 H3——Go connreg
    /// 单调自增语义，可 >MAX_CONNS）。
    fn admit(&mut self, id_alloc: &AtomicU64, conn: &UnixStream, max_conns: usize) -> Option<u64> {
        if self.live >= max_conns {
            return None;
        }
        let id = id_alloc.fetch_add(1, Ordering::Relaxed) + 1;
        self.live += 1;
        if let Ok(c) = conn.try_clone() {
            self.conns.push((id, c));
        }
        Some(id)
    }

    /// 按会话号收口（只摘自己那条；在册计数减一）。
    fn release(&mut self, id: u64) {
        self.live = self.live.saturating_sub(1);
        self.conns.retain(|(i, _)| *i != id);
    }

    /// 收工：断开全部在跑会话（Go connreg.CloseAll 同义，评审 M12）。`live = 0` 对**已
    /// 登记**的会话是事实；窄窗残余：stop 前已 accept、尚未 admit 的会话不在表内
    /// （其线程照常跑完 30s 硬超时），`try_clone` 失败时也不登记句柄（EMFILE/ENFILE 恰是
    /// accept 退避场景——该会话只能等硬超时自收）。
    fn close_all(&mut self) {
        for (_, c) in self.conns.drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        self.live = 0;
    }
}

impl SpeedtestServer {
    pub fn new(logf: crate::Logf) -> Self {
        Self::with_limits(logf, Limits::default())
    }

    /// 注入限额（Go `SetLimits`；0/越界走缺省——`Limits::filled`）。
    pub fn with_limits(logf: crate::Logf, limits: Limits) -> Self {
        Self {
            logf,
            limits: limits.filled(),
            conns: Arc::new(Mutex::new(CountingRegistry { live: 0, conns: Vec::new() })),
            stats: Arc::new(SessStats::default()),
        }
    }

    /// 受理循环（每连接一线程；accept 出错即返回——listener 由持有方关）。
    pub fn serve(self: &Arc<Self>, ln: UnixListener) -> std::io::Result<()> {
        for conn in ln.incoming() {
            let conn = conn?;
            let srv = Arc::clone(self);
            std::thread::Builder::new()
                .name("homeway-speedtest".into())
                .stack_size(512 * 1024)
                .spawn(move || srv.serve_conn(conn))
                .ok();
        }
        Ok(())
    }

    /// 可停形态（引擎收工面）。accept 错误分类/退避/节流日志走共享件
    /// `files_server::serve_stoppable_accepts`（F5：瞬态错误不再摘服务）。
    ///
    /// **形参 M3 S2 起 = [`ServiceIntake`]**（设计 §2.2 方案 B′；同 `FilesServer`）：
    /// 两源合一（UDS + QUIC stream 队列〔tag=3〕）；`ServiceIntake::from_listener` = 改前的
    /// 单 UDS 形态。**本函数以下的服务本体一行不改**（应用层帧逐字节不变，§1.2）。
    pub fn serve_stoppable(
        self: &Arc<Self>,
        intake: ServiceIntake,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> std::io::Result<()> {
        let logf = self.logf.clone();
        let r = crate::files_server::serve_stoppable_accepts(
            || intake.accept(),
            move |conn| {
                let srv = Arc::clone(self);
                let logf = logf.clone();
                if let Err(e) = std::thread::Builder::new()
                    .name("homeway-speedtest".into())
                    .stack_size(512 * 1024)
                    .spawn(move || srv.serve_conn(conn))
                {
                    (logf)(&format!("speedtest: 会话线程起不来（{e}）——连接直接收线"));
                }
            },
            stop,
            &self.logf,
            "speedtest",
        );
        // 收工：断开全部在跑会话（Go connreg.CloseAll 同义，评审 M12）
        if let Ok(mut reg) = self.conns.lock() {
            reg.close_all();
        }
        r
    }

    /// 在册会话数（诊断面）。
    pub fn live(&self) -> usize {
        self.conns.lock().map(|c| c.live).unwrap_or(0)
    }

    fn serve_conn(self: Arc<Self>, conn: UnixStream) {
        // 上限判定与登记同临界区；拒绝路径先有界读掉请求帧再回帧（r1 中-1②：回帧后
        // 立刻关会走 RST 路径、已发出的 report 可能被对端丢弃）。
        let conn_id = {
            let mut reg = self.conns.lock().expect("会话表锁中毒");
            match reg.admit(&self.stats.next, &conn, self.limits.max_conns) {
                Some(id) => {
                    self.stats.total.fetch_add(1, Ordering::Relaxed);
                    id
                }
                None => {
                    drop(reg);
                    self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    (self.logf)(&format!("speedtest: 会话拒绝（并发上限 {}）", self.limits.max_conns));
                    reply_then_close(conn, &report_json(0, 0, 0, Some("busy")));
                    return;
                }
            }
        };
        // 守卫按会话号收口（F6b）：任何退出路径只摘自己那条（修前清整表）。
        let _guard = ConnGuard { conns: Arc::clone(&self.conns), id: conn_id };

        // 单连接硬超时（**绝对期限**——F6a：每次触达 socket 的读写前 `arm_io` 按剩余
        // 收敛设置；`SO_RCVTIMEO` 是 per-syscall，只在会话开头设一次会被慢滴客户端
        // 每次成功读续命 30s）。
        let deadline = Instant::now() + self.limits.conn_timeout;
        let w = match conn.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut r = BufReader::with_capacity(128 * 1024, DeadlineIo { sock: &conn, deadline });

        // 恒第一帧：会话请求（客户端先写、服务端后答）
        let (typ, payload) = match read_frame(&mut r) {
            Ok(v) => v,
            Err(e) => {
                (self.logf)(&format!("speedtest: 会话异常（读请求帧：{e}）"));
                return;
            }
        };
        if typ != TYPE_REQUEST {
            (self.logf)(&format!("speedtest: 会话异常（首帧类型 {typ} 非 request）"));
            return;
        }
        let Some(req) = parse_request(&payload) else {
            (self.logf)("speedtest: 会话异常（请求解析：JSON 形状不符）");
            return;
        };
        let warmup = Duration::from_millis(req.warmup_ms);
        let window = Duration::from_millis(req.window_ms);
        if req.role != "recv" && req.role != "send" {
            finish_with_error(&w, &format!("未知角色 {:?}", req.role), deadline);
            return;
        }
        if warmup > self.limits.max_warmup {
            finish_with_error(
                &w,
                &format!(
                    "预热 {} 超上限 {}",
                    fmt_duration_go_ms(warmup),
                    fmt_duration_go_ms(self.limits.max_warmup)
                ),
                deadline,
            );
            return;
        }
        if window < MIN_WINDOW || window > self.limits.max_window {
            finish_with_error(
                &w,
                &format!(
                    "窗口 {} 超出 [{}, {}]",
                    fmt_duration_go_ms(window),
                    fmt_duration_go_ms(MIN_WINDOW),
                    fmt_duration_go_ms(self.limits.max_window)
                ),
                deadline,
            );
            return;
        }
        // E13 受理行
        (self.logf)(&format!(
            "speedtest: 会话 #{conn_id} role={} warmup={} window={}",
            req.role,
            fmt_duration_go_ms(warmup),
            fmt_duration_go_ms(window)
        ));
        if req.role == "recv" {
            self.serve_recv(conn_id, &w, warmup, window, deadline);
        } else {
            self.serve_send(conn_id, &mut r, &w, deadline);
        }
    }

    /// role=recv：预热发 → 窗口发 → report。发送侧按接收方 TCP 背压自然限速。
    ///
    /// R8-2 归因插桩：窗口段按 1s 切片泵送（共享同一截止时刻——墙钟行为与整段
    /// pump_data 等价，切片只用于计量），末尾多打一行逐秒字节序列 + 尾 3s 速率
    /// ——真机下行「爬坡支配 vs 稳态封顶」的判别面（窗口均值会掩盖前者）。
    fn serve_recv(&self, id: u64, w: &UnixStream, warmup: Duration, window: Duration, deadline: Instant) {
        let mut bw = BufWriter::new(DeadlineIo { sock: w, deadline });
        let block = vec![0u8; self.limits.send_block];
        let mut seq: u32 = 0;
        let t0 = Instant::now();
        let warmup_bytes = pump_data(&mut bw, &block, &mut seq, warmup);
        if bw.flush().is_err() {
            (self.logf)(&format!("speedtest: 会话 #{id} 异常（下行发送：flush 失败）"));
            return;
        }
        let window_deadline = (Instant::now() + window).min(deadline);
        // 片 = (载荷字节, 实际时长)——末片常为残片（<1s），尾速率按真实时长折算
        // （评审 r1-F4：片数当秒数会把尾速率系统性低报至多 ~1/3）。
        let mut per_sec: Vec<(i64, f64)> = Vec::with_capacity(window.as_secs() as usize + 1);
        loop {
            let now = Instant::now();
            if now >= window_deadline {
                break;
            }
            let slice = (window_deadline - now).min(Duration::from_secs(1));
            let slice_t0 = Instant::now();
            let b = pump_data(&mut bw, &block, &mut seq, slice);
            per_sec.push((b, slice_t0.elapsed().as_secs_f64().min(1.0)));
            // 评审 r1-F1：写错误早退——pump_data 写失败时本片快速返回，若仍剩窗口
            // 时间继续空转（64KB 分配+立即失败的写）最长 MAX_WINDOW。0 字节且未到
            // 截止 ⇒ 通道已断，跳出走 flush 的错误路径记行。
            if b == 0 && Instant::now() < window_deadline {
                break;
            }
        }
        let window_bytes: i64 = per_sec.iter().map(|(b, _)| b).sum();
        if bw.flush().is_err() {
            (self.logf)(&format!("speedtest: 会话 #{id} 异常（下行发送：flush 失败）"));
            return;
        }
        let rep = report_json(window_bytes, warmup_bytes, t0.elapsed().as_millis() as i64, None);
        if write_control(&mut bw, TYPE_REPORT, rep.as_bytes()).is_err() || bw.flush().is_err() {
            (self.logf)(&format!("speedtest: 会话 #{id} 异常（回报告：写失败）"));
            return;
        }
        // 尾 3s 速率（不足 3s 取全部，按各片真实时长折算）：末段均值——与窗口均值
        // 对比即可判别「窗口内仍在爬坡」（尾 ≫ 均值）还是「稳态封顶」（尾 ≈ 均值）。
        let tail_secs = per_sec.len().min(3);
        let tail: Vec<&(i64, f64)> = per_sec[per_sec.len() - tail_secs..].iter().collect();
        let tail_span: f64 = tail.iter().map(|(_, d)| d).sum();
        let tail_bytes: i64 = tail.iter().map(|(b, _)| b).sum();
        let tail_mbps = if tail_span > 0.0 {
            tail_bytes as f64 * 8.0 / (tail_span * 1_000_000.0)
        } else {
            0.0
        };
        // E13 结算行（role=recv）
        (self.logf)(&format!(
            "speedtest: 会话 #{id} role=recv bytes={window_bytes}（含预热 {warmup_bytes}）用时={}ms",
            t0.elapsed().as_millis()
        ));
        // R8-2 归因行（词面与判据行区分——评审 r1-F13：`grep role=recv` 的会话计数
        // 不受本行污染）。
        (self.logf)(&format!(
            "speedtest: 归因 #{id} 下行逐秒MB=[{}] 尾{}片({tail_span:.1}s)={tail_mbps:.0}Mbps",
            per_sec
                .iter()
                .map(|(b, d)| format!("{:.1}/{:.0}s", *b as f64 / (1024.0 * 1024.0), d))
                .collect::<Vec<_>>()
                .join(","),
            tail_secs,
        ));
    }

    /// role=send：读帧计数——START 前 = 预热，START 后 = 窗口；FINISH 触发 report。
    fn serve_send(&self, id: u64, r: &mut BufReader<DeadlineIo<'_>>, w: &UnixStream, deadline: Instant) {
        let mut window_bytes: i64 = 0;
        let mut warmup_bytes: i64 = 0;
        let mut started = false;
        let mut start_at = Instant::now();
        loop {
            let (typ, payload_len) = match read_frame_header(r) {
                Ok(v) => v,
                Err(e) => {
                    (self.logf)(&format!("speedtest: 会话 #{id} 异常（读上行帧：{e}）"));
                    return;
                }
            };
            if typ != TYPE_DATA && payload_len > 0 {
                // 控制帧按约定空载荷；非空按载荷丢弃（协议向前兼容）
                if discard_payload(r, payload_len).is_err() {
                    (self.logf)(&format!("speedtest: 会话 #{id} 异常（读控制载荷：EOF）"));
                    return;
                }
            }
            match typ {
                TYPE_DATA => {
                    if payload_len > self.limits.max_block {
                        finish_with_error(w, &format!("data 载荷 {payload_len} 超上限 {}", self.limits.max_block), deadline);
                        return;
                    }
                    if discard_payload(r, payload_len).is_err() {
                        (self.logf)(&format!("speedtest: 会话 #{id} 异常（读上行载荷：EOF）"));
                        return;
                    }
                    if started {
                        window_bytes += payload_len as i64;
                    } else {
                        warmup_bytes += payload_len as i64;
                    }
                }
                TYPE_START => {
                    if started {
                        finish_with_error(w, "重复 START", deadline);
                        return;
                    }
                    started = true;
                    start_at = Instant::now();
                }
                TYPE_FINISH => {
                    if !started {
                        finish_with_error(w, "FINISH 前无 START", deadline);
                        return;
                    }
                    let rep = report_json(
                        window_bytes,
                        warmup_bytes,
                        start_at.elapsed().as_millis() as i64,
                        None,
                    );
                    let mut bw = BufWriter::new(DeadlineIo { sock: w, deadline });
                    if write_control(&mut bw, TYPE_REPORT, rep.as_bytes()).is_err() || bw.flush().is_err() {
                        (self.logf)(&format!("speedtest: 会话 #{id} 异常（回报告：写失败）"));
                        return;
                    }
                    // E13 结算行（role=send）
                    (self.logf)(&format!(
                        "speedtest: 会话 #{id} role=send bytes={window_bytes}（含预热 {warmup_bytes}）用时={}ms",
                        start_at.elapsed().as_millis()
                    ));
                    return;
                }
                other => {
                    finish_with_error(w, &format!("窗口期收到类型 {other}"), deadline);
                    return;
                }
            }
        }
    }
}

/// 按**绝对期限**收敛设置读写超时（每次触达 socket 的读写前调用——`SO_RCVTIMEO`
/// 是 per-syscall，只在会话开头设一次会被慢滴客户端每次成功读续命；零即设 1ms 立即失败）。
fn arm_io(conn: &UnixStream, deadline: Instant) {
    let remain = deadline.saturating_duration_since(Instant::now());
    if remain.is_zero() {
        let _ = conn.set_read_timeout(Some(Duration::from_millis(1)));
        let _ = conn.set_write_timeout(Some(Duration::from_millis(1)));
        return;
    }
    let _ = conn.set_read_timeout(Some(remain));
    let _ = conn.set_write_timeout(Some(remain));
}

/// 期限感知的 IO 适配器（F6a 的收口形态）：**每次 syscall 前**按绝对期限重设超时，
/// 于是 `read_exact`/`BufWriter` 的内部循环也受同一绝对期限约束（逐字节滴流无法靠
/// 「每次成功读续命」把会话拖过期限）。设计 R8 的三个 arm 点（读前 / flush 前 /
/// 直写前）由本适配器统一覆盖。
struct DeadlineIo<'a> {
    sock: &'a UnixStream,
    deadline: Instant,
}

impl std::io::Read for DeadlineIo<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        arm_io(self.sock, self.deadline);
        self.sock.read(buf)
    }
}

impl std::io::Write for DeadlineIo<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        arm_io(self.sock, self.deadline);
        self.sock.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        arm_io(self.sock, self.deadline);
        self.sock.flush()
    }
}

/// 受理计数守卫（按**会话号**收口——F6b；任何退出路径只摘自己那条，panic 兜底同）。
struct ConnGuard {
    conns: Arc<Mutex<CountingRegistry>>,
    id: u64,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        if let Ok(mut c) = self.conns.lock() {
            c.release(self.id);
        }
    }
}

/// 请求帧载荷（`{"role":"recv","warmup_ms":2000,"window_ms":10000}`——手解极小 JSON 面）。
/// **pub 纯函数 = fuzz/测试可达面**（R5 第二道门 高-4 整改：服务端半边原不可达）。
pub struct RequestJson {
    pub role: String,
    pub warmup_ms: u64,
    pub window_ms: u64,
}

pub fn parse_request(payload: &[u8]) -> Option<RequestJson> {
    let s = std::str::from_utf8(payload).ok()?;
    let inner = s.trim_start_matches('{').trim_end_matches('}');
    let mut role = String::new();
    let mut warmup = 0u64;
    let mut window = 0u64;
    // 引号感知切分（低-4 同款，服务端面）：role 值是客户端自由串，含 `,`/`"` 时
    // 裸 split 会错分——与客户端 parse_report 共用同一切分/反转义语义。
    for kv in crate::speedtest::split_top_level(inner) {
        let Some((k, v)) = kv.split_once(':') else { continue };
        match k.trim().trim_matches('"') {
            "role" => role = crate::speedtest::unescape_minimal(v.trim().trim_matches('"')),
            "warmup_ms" => warmup = v.trim().parse().ok()?,
            "window_ms" => window = v.trim().parse().ok()?,
            _ => {}
        }
    }
    if role.is_empty() {
        return None;
    }
    Some(RequestJson { role, warmup_ms: warmup, window_ms: window })
}

/// report 帧载荷（error 文案经最小 JSON 转义——评审 M15：`未知角色 "xyz"` 的引号
/// 不转义会产出非法 JSON，对端解析歧义）。
fn report_json(bytes: i64, warmup: i64, wall_ms: i64, error: Option<&str>) -> String {
    // Go json.Marshal 字段序：bytes/warmup_bytes/wall_ms/error(omitempty)
    match error {
        Some(e) => format!(
            r#"{{"bytes":{bytes},"warmup_bytes":{warmup},"wall_ms":{wall_ms},"error":"{}"}}"#,
            json_escape(e)
        ),
        None => format!(r#"{{"bytes":{bytes},"warmup_bytes":{warmup},"wall_ms":{wall_ms}}}"#),
    }
}

/// 最小 JSON 字符串转义（`"`/`\`/控制字符——判据面文案不含其余特殊字符）。
fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// 失败收场：回 report（error 形态）后冲刷（写前按绝对期限 arm）。
fn finish_with_error(w: &UnixStream, msg: &str, deadline: Instant) {
    let mut bw = BufWriter::new(DeadlineIo { sock: w, deadline });
    let rep = report_json(0, 0, 0, Some(msg));
    let _ = write_control(&mut bw, TYPE_REPORT, rep.as_bytes());
    let _ = bw.flush();
}

/// busy/link_down 类拒绝回帧的有序收口（Go ReplyThenClose）：先有界吞一帧（请求）→
/// 回 report 并冲刷 → 短窗继续吞输入（send 角色在回帧抵达前还在泵）→ 关。
/// 两段各自**绝对期限**（F6c：帧吞 `now+2s`、循环 `now+1s`——per-syscall 续命会让
/// 滴流连接永久占住一条线程）。
fn reply_then_close(conn: UnixStream, error_report: &str) {
    let Ok(r_stream) = conn.try_clone() else { return };
    {
        let frame_deadline = Instant::now() + BUSY_FRAME_BUDGET;
        // 两腿都走 `DeadlineIo`（逐 syscall 按剩余重设）——只 arm 一次的话，
        // `read_exact`/`BufWriter` 的内部循环会被「每 <2s 送 1 字节」的滴流无限续命
        // （声明长度 ≤65535 ⇒ 单条 busy 连接可占线程数小时；Go 是 SetReadDeadline 绝对语义）
        let mut r = BufReader::new(DeadlineIo { sock: &r_stream, deadline: frame_deadline });
        let _ = drain_one_frame(&mut r, frame_deadline);
    }
    let mut w = conn;
    let write_deadline = Instant::now() + BUSY_FRAME_BUDGET;
    let mut bw = BufWriter::new(DeadlineIo { sock: &w, deadline: write_deadline });
    let _ = write_control(&mut bw, TYPE_REPORT, error_report.as_bytes());
    let _ = bw.flush();
    drop(bw);
    // 继续吞输入一小窗（期限内对端先关则自然提前结束）
    let drain_deadline = Instant::now() + BUSY_DRAIN_BUDGET;
    let mut discard = [0u8; 4096];
    loop {
        if Instant::now() >= drain_deadline {
            break;
        }
        arm_io(&w, drain_deadline);
        match w.read(&mut discard) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
    }
}

/// 有界吞一帧（拒绝路径用）：**类型字节已由头读消费，载荷必须流式吞完**——否则载荷
/// 留在流里，后续吞输入从载荷中间对帧（错位）；声明载荷超 `MAX_BLOCK` 即收线。
/// 失败也继续回帧（连接本就异常，尽力而为）。
fn drain_one_frame(r: &mut BufReader<DeadlineIo<'_>>, deadline: Instant) -> std::io::Result<()> {
    let (_typ, n) = read_frame_header(r)?;
    if n > MAX_BLOCK {
        // u16 长度场 ⇒ 恒不触发（保留语义位：MAX_BLOCK 若调小即成真门）
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "帧超长"));
    }
    let mut buf = [0u8; 8192];
    let mut left = n;
    while left > 0 {
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "帧吞窗耗尽"));
        }
        let c = left.min(buf.len());
        r.read_exact(&mut buf[..c])?; // 块内每次 syscall 由 DeadlineIo 收敛（滴流不续命）
        left -= c;
    }
    Ok(())
}

// ---------- 帧编解码（BufReader 面；头解析复用 speedtest::decode_head 单一真源——
// R5 第二道门 高-4 整改：原为客户端 decode_frame 的同语义重实现） ----------

/// 读一帧（控制帧路径：连同 crc 一起校验，载荷带回）。
fn read_frame(r: &mut impl Read) -> std::io::Result<(u8, Vec<u8>)> {
    let mut hdr = [0u8; HEADER];
    r.read_exact(&mut hdr)?;
    let head = crate::speedtest::decode_head(&hdr)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let mut payload = vec![0u8; head.len];
    r.read_exact(&mut payload)?;
    let want = if head.typ == TYPE_DATA { zero_crc(head.len) } else { crc32_ieee(&payload) };
    if head.crc != want {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "帧 crc 不符"));
    }
    Ok((head.typ, payload))
}

/// 只读帧头不进载荷（data 计数路径：payload 是零填充，只需长度）。
fn read_frame_header(r: &mut impl Read) -> std::io::Result<(u8, usize)> {
    let mut hdr = [0u8; HEADER];
    r.read_exact(&mut hdr)?;
    let head = crate::speedtest::decode_head(&hdr)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    if head.typ == TYPE_DATA && head.crc != zero_crc(head.len) {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "data 帧 crc 不符"));
    }
    Ok((head.typ, head.len))
}

/// 载荷丢弃（64KB 整块读——热路径每 64KB 帧降到 ~2-3 次系统调用）。
fn discard_payload(r: &mut impl Read, mut n: usize) -> std::io::Result<()> {
    let mut buf = vec![0u8; 64 * 1024];
    while n > 0 {
        let c = n.min(buf.len());
        r.read_exact(&mut buf[..c])?;
        n -= c;
    }
    Ok(())
}

/// 写一个控制帧（request/start/finish/report；seq 恒 0）。
fn write_control(w: &mut impl Write, typ: u8, payload: &[u8]) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(HEADER + payload.len());
    out.extend_from_slice(&MAGIC);
    out.push(typ);
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    out.extend_from_slice(&crc32_ieee(payload).to_le_bytes());
    out.extend_from_slice(payload);
    w.write_all(&out)
}

/// 在 dur 内持续写 data 帧（载荷全零、seq 递增——前置自增从 1 起），返回 payload 总字节。
/// 整帧（头+载荷）拼进一块 scratch 一次写出；不 flush——调用方控制冲刷时机。
fn pump_data(w: &mut impl Write, block: &[u8], seq: &mut u32, dur: Duration) -> i64 {
    let deadline = Instant::now() + dur;
    let mut scratch = vec![0u8; HEADER + block.len()];
    let mut total: i64 = 0;
    while Instant::now() < deadline {
        *seq += 1;
        scratch[..4].copy_from_slice(&MAGIC);
        scratch[4] = TYPE_DATA;
        scratch[5..9].copy_from_slice(&seq.to_le_bytes());
        scratch[9..11].copy_from_slice(&(block.len() as u16).to_le_bytes());
        scratch[11..15].copy_from_slice(&zero_crc(block.len()).to_le_bytes());
        if w.write_all(&scratch).is_err() {
            return total;
        }
        total += block.len() as i64;
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// UDS 对拍：role=recv（服务端泵送 + report + E13 结算行）——客户端形态极简
    /// （读帧计长直到 report）。
    #[test]
    fn serve_recv_end_to_end() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-sprv-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });

        let mut c = UnixStream::connect(&sock).unwrap();
        // 请求：role=recv、warmup 100ms、window 300ms
        let req = br#"{"role":"recv","warmup_ms":100,"window_ms":300}"#;
        write_control(&mut c, TYPE_REQUEST, req).unwrap();

        // 读到 report：累计 data payload
        let mut r = BufReader::new(c.try_clone().unwrap());
        let mut got: i64 = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut rep = None;
        while Instant::now() < deadline {
            match read_frame_header(&mut r) {
                Ok((TYPE_DATA, n)) => {
                    discard_payload(&mut r, n).unwrap();
                    got += n as i64;
                }
                Ok((typ, n)) => {
                    assert_eq!(typ, TYPE_REPORT);
                    let mut buf = vec![0u8; n];
                    r.read_exact(&mut buf).unwrap();
                    rep = Some(String::from_utf8(buf).unwrap());
                    break;
                }
                Err(e) => panic!("读帧失败：{e}"),
            }
        }
        let rep = rep.expect("report 应到达");
        assert!(rep.contains("\"bytes\":"), "report 形态：{rep}");
        // 窗口 300ms × ~600Mbps 量级（回环 UDS）：至少应有可观字节
        assert!(got > 1_000_000, "300ms 窗口应收可观载荷（实收 {got}B）");
        assert_eq!(srv.stats.total.load(Ordering::Relaxed), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// role=send：客户端泵 → START → 窗口泵 → FINISH → 服务端报数。
    #[test]
    fn serve_send_end_to_end() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spsd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let l2 = Arc::clone(&lines);
        let logf: crate::Logf = Arc::new(move |s: &str| {
            l2.lock().unwrap().push(s.to_owned());
        });
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });

        let mut c = UnixStream::connect(&sock).unwrap();
        let req = br#"{"role":"send","warmup_ms":50,"window_ms":200}"#;
        write_control(&mut c, TYPE_REQUEST, req).unwrap();
        // 预热泵 3 帧 → START → 窗口泵 5 帧 → FINISH
        let block = vec![0u8; 4096];
        let mut seq = 0u32;
        let warm = pump_data(&mut c, &block, &mut seq, Duration::ZERO);
        let _ = warm;
        for _ in 0..3 {
            let mut f = vec![0u8; HEADER + 4096];
            f[..4].copy_from_slice(&MAGIC);
            f[4] = TYPE_DATA;
            seq += 1;
            f[5..9].copy_from_slice(&seq.to_le_bytes());
            f[9..11].copy_from_slice(&4096u16.to_le_bytes());
            f[11..15].copy_from_slice(&zero_crc(4096).to_le_bytes());
            c.write_all(&f).unwrap();
        }
        write_control(&mut c, TYPE_START, &[]).unwrap();
        for _ in 0..5 {
            let mut f = vec![0u8; HEADER + 4096];
            f[..4].copy_from_slice(&MAGIC);
            f[4] = TYPE_DATA;
            seq += 1;
            f[5..9].copy_from_slice(&seq.to_le_bytes());
            f[9..11].copy_from_slice(&4096u16.to_le_bytes());
            f[11..15].copy_from_slice(&zero_crc(4096).to_le_bytes());
            c.write_all(&f).unwrap();
        }
        write_control(&mut c, TYPE_FINISH, &[]).unwrap();
        // report
        let mut r = BufReader::new(c);
        let (rt, payload) = read_frame(&mut r).expect("report 应到达");
        assert_eq!(rt, TYPE_REPORT);
        let rep = String::from_utf8(payload).unwrap();
        assert!(rep.contains("\"bytes\":20480"), "窗口 5×4096（实得 {rep}）");
        assert!(rep.contains("\"warmup_bytes\":12288"), "预热 3×4096");
        // E13 判据行（受理 + 结算）
        let lines = lines.lock().unwrap();
        assert!(lines.iter().any(|l| l.starts_with("speedtest: 会话 #1 role=send warmup=50ms window=200ms")), "受理行：{lines:?}");
        assert!(
            lines.iter().any(|l| l.starts_with("speedtest: 会话 #1 role=send bytes=20480（含预热 12288）用时=")),
            "结算行：{lines:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话号单调回归（评审 H3）：串行两轮会话的受理行号必须递增（此前用「当前
    /// 在册数」冒充会话号——两轮都打 #1）。
    #[test]
    fn session_ids_monotonic_across_rounds() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spid-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let lines = Arc::new(Mutex::new(Vec::<String>::new()));
        let l2 = Arc::clone(&lines);
        let logf: crate::Logf = Arc::new(move |s: &str| {
            l2.lock().unwrap().push(s.to_owned());
        });
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });
        // 两轮 role=send（收口即退——每轮一个号）
        for _ in 0..2 {
            let mut c = UnixStream::connect(&sock).unwrap();
            write_control(&mut c, TYPE_REQUEST, br#"{"role":"send","warmup_ms":10,"window_ms":100}"#).unwrap();
            write_control(&mut c, TYPE_START, &[]).unwrap();
            write_control(&mut c, TYPE_FINISH, &[]).unwrap();
            let mut r = BufReader::new(c);
            let _ = read_frame(&mut r);
        }
        let lines = lines.lock().unwrap();
        let ids: Vec<u64> = lines
            .iter()
            .filter_map(|l| l.strip_prefix("speedtest: 会话 #"))
            .filter_map(|r| r.split(' ').next().and_then(|n| n.parse().ok()))
            .collect();
        assert!(ids.len() >= 4, "两轮应各有受理+结算行：{lines:?}");
        assert!(ids[0] < ids[2], "第二轮会话号必须大于第一轮（单调）：{ids:?}");
        assert_eq!(ids[0], ids[1], "同会话受理/结算同号：{ids:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **M3 S2 判据（服务入口承载面）**：① 同一请求（未知角色 ⇒ 定型错误帧）经**两条入口**
    /// （UDS / QUIC socketpair）的应答**逐字节相同**（「应用层零改动」的构造性钉法）；
    /// ② 在册满 ⇒ **应用层 busy 报表在 intake 面仍可达**（第 13 条拿到 `"error":"busy"`）；
    /// ③ 入口容量 = 在册上限 + K（装配点的算式与常量一致）。
    #[test]
    fn intake_two_sources_serve_identical_bytes_and_busy_stays_reachable() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spintake-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));

        let capacity = homeway_quic::tuning::service_defaults::intake_capacity(MAX_CONNS);
        assert_eq!(capacity, 16, "speedtest 入口容量 = 12 + 4（§8.2-16）");
        let uds_intake = homeway_quic::ServiceIntake::from_listener(ln).unwrap();
        let (quic_intake, tx) = homeway_quic::ServiceIntake::quic_only(capacity).unwrap();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        for intake in [uds_intake, quic_intake] {
            let s2 = Arc::clone(&srv);
            let st2 = Arc::clone(&stop);
            std::thread::spawn(move || {
                let _ = s2.serve_stoppable(intake, st2);
            });
        }

        // 同一请求（未知角色 ⇒ `finish_with_error` 定型帧）在两条入口上跑：字节对比
        let bad = br#"{"role":"bogus","warmup_ms":0,"window_ms":100}"#;
        let run = |mut c: UnixStream| -> Vec<u8> {
            write_control(&mut c, TYPE_REQUEST, bad).unwrap();
            let mut r = BufReader::new(c);
            let f = read_frame(&mut r).expect("错误报表帧");
            assert_eq!(f.0, TYPE_REPORT);
            f.1
        };
        let c_uds = UnixStream::connect(&sock).unwrap();
        let rep_uds = run(c_uds);
        let (svc_end, cli_end) = UnixStream::pair().unwrap();
        tx.try_enqueue(svc_end).expect("入队");
        let rep_quic = run(cli_end);
        assert_eq!(rep_quic, rep_uds, "两源同协议面：应答帧逐字节相同");
        assert!(
            String::from_utf8_lossy(&rep_quic).contains("未知角色"),
            "内容面（防两源都回了空壳）：{}",
            String::from_utf8_lossy(&rep_quic)
        );

        // ② 在册满（12 条经 intake 的会话挂着）⇒ 第 13 条走应用层 busy
        let mut holders = Vec::new();
        for _ in 0..MAX_CONNS {
            let (svc_end, cli_end) = UnixStream::pair().unwrap();
            tx.try_enqueue(svc_end).expect("入队");
            holders.push(cli_end); // 不写请求：会话线程挂在读请求帧上（名额持住）
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while srv.live() < MAX_CONNS {
            assert!(Instant::now() < deadline, "在册数未达上限：{}", srv.live());
            std::thread::sleep(Duration::from_millis(20));
        }
        let (svc_end, cli_end) = UnixStream::pair().unwrap();
        tx.try_enqueue(svc_end).expect("第 13 条入队（入口未满——在册闸先触发）");
        let mut c = cli_end;
        write_control(&mut c, TYPE_REQUEST, br#"{"role":"recv","warmup_ms":100,"window_ms":200}"#).unwrap();
        let mut r = BufReader::new(c);
        let rep = read_frame(&mut r).expect("busy 报表应到达");
        assert_eq!(rep.0, TYPE_REPORT);
        assert_eq!(
            String::from_utf8(rep.1).unwrap(),
            r#"{"bytes":0,"warmup_bytes":0,"wall_ms":0,"error":"busy"}"#,
            "busy 报表逐字节（Go 字段序）"
        );
        assert_eq!(srv.stats.rejected.load(Ordering::Relaxed), 1, "计拒 1 条");
        drop(holders);
        stop.store(true, Ordering::Relaxed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// busy 路径：占满并发后新连接回 error=busy（reply_then_close 形态）。
    #[test]
    fn busy_rejection() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spbusy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });
        // 占满：12 条并发 recv 会话（每条 30s 超时内不退出）
        let mut holders = Vec::new();
        for _ in 0..MAX_CONNS {
            let mut c = UnixStream::connect(&sock).unwrap();
            write_control(&mut c, TYPE_REQUEST, br#"{"role":"recv","warmup_ms":1000,"window_ms":5000}"#).unwrap();
            holders.push(c);
        }
        std::thread::sleep(Duration::from_millis(200));
        // 第 13 条：busy 回帧
        let mut c = UnixStream::connect(&sock).unwrap();
        write_control(&mut c, TYPE_REQUEST, br#"{"role":"recv","warmup_ms":100,"window_ms":200}"#).unwrap();
        let mut r = BufReader::new(c);
        let rep = read_frame(&mut r).expect("busy report 应到达");
        assert_eq!(rep.0, TYPE_REPORT);
        assert_eq!(String::from_utf8(rep.1).unwrap(), r#"{"bytes":0,"warmup_bytes":0,"wall_ms":0,"error":"busy"}"#);
        assert_eq!(srv.stats.rejected.load(Ordering::Relaxed), 1);
        drop(holders);
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- F6a 绝对硬超时（可注入 Limits） ----------

    /// 滴流客户端（每 100ms 1 字节）在注入的 `conn_timeout = 300ms` 下必须按期收线
    /// （修前：per-syscall 期限每次成功读续命 ⇒ 会话一直活着；本用例断言 ≤1s = ≥3× 余量）。
    #[test]
    fn conn_timeout_is_absolute_under_dribble() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spdrib-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::with_limits(
            Arc::clone(&logf),
            Limits { conn_timeout: Duration::from_millis(300), ..Default::default() },
        ));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });
        let mut c = UnixStream::connect(&sock).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        write_control(&mut c, TYPE_REQUEST, br#"{"role":"send","warmup_ms":0,"window_ms":100}"#).unwrap();
        // 滴流：每 100ms 1 字节（对端逐 syscall 续命即永远读不完帧头）
        let t0 = Instant::now();
        let mut closed = false;
        let mut byte = 0u8;
        while t0.elapsed() < Duration::from_secs(3) {
            std::thread::sleep(Duration::from_millis(100));
            if c.write_all(&[byte]).is_err() {
                closed = true;
                break;
            }
            byte = byte.wrapping_add(1);
            let mut b = [0u8; 64];
            match c.read(&mut b) {
                Ok(0) => {
                    closed = true;
                    break;
                }
                Ok(_) => {} // 服务端可能回错误帧（也是收线前的正常产物）
                Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
                Err(_) => {
                    closed = true;
                    break;
                }
            }
        }
        let dt = t0.elapsed();
        assert!(closed, "滴流会话必须被绝对硬超时收线（{dt:?}）");
        assert!(dt < Duration::from_secs(1), "上界 = conn_timeout(300ms) + 余量（实 {dt:?}）");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 正例：正常短会话在宽松 timeout 内完成（不贴目标值）。
    #[test]
    fn normal_session_completes_within_loose_timeout() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spok-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::with_limits(
            Arc::clone(&logf),
            Limits { conn_timeout: Duration::from_secs(2), ..Default::default() },
        ));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });
        let mut c = UnixStream::connect(&sock).unwrap();
        write_control(&mut c, TYPE_REQUEST, br#"{"role":"send","warmup_ms":10,"window_ms":100}"#).unwrap();
        write_control(&mut c, TYPE_START, &[]).unwrap();
        write_control(&mut c, TYPE_FINISH, &[]).unwrap();
        let mut r = BufReader::new(c);
        let (rt, payload) = read_frame(&mut r).expect("report 应到达");
        assert_eq!(rt, TYPE_REPORT);
        assert!(String::from_utf8(payload).unwrap().contains("\"bytes\":0"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- F6b release(id) 按会话收口 / close_all ----------

    /// 在册表：`release(id)` 只摘自己那条（修前清整表）；`close_all` 切断全部在跑会话
    /// 且 `live = 0` 是事实。
    #[test]
    fn release_by_session_id_and_close_all() {
        let (a1, a2) = UnixStream::pair().unwrap();
        let (b1, mut b2) = UnixStream::pair().unwrap();
        let mut reg = CountingRegistry { live: 0, conns: Vec::new() };
        let alloc = AtomicU64::new(0);
        let id1 = reg.admit(&alloc, &a1, MAX_CONNS).expect("会话 1");
        let id2 = reg.admit(&alloc, &b1, MAX_CONNS).expect("会话 2");
        assert_eq!((id1, id2), (1, 2));
        assert_eq!(reg.live, 2);
        reg.release(id1);
        assert_eq!(reg.live, 1, "只减一条");
        assert_eq!(reg.conns.len(), 1, "只摘自己那条（修前清整表）");
        assert_eq!(reg.conns[0].0, id2);
        // close_all：切断仍注册的会话 2（dup 句柄 shutdown ⇒ 对端读到 EOF/错误）
        reg.close_all();
        assert_eq!(reg.live, 0);
        assert!(reg.conns.is_empty());
        let mut buf = [0u8; 8];
        b2.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        match b2.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => panic!("close_all 后不应有数据（{n}B）"),
            Err(e) => assert!(
                !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
                "应是 EOF/连接类错误而非超时：{e}"
            ),
        }
        drop((a1, a2, b1));
    }

    /// 真会话端到端：两条 role=send 挂住 → 会话 1 FINISH 收口后 `live()==1` →
    /// 停止位置位（serve_stoppable 收工）⇒ 会话 2 被切断。
    #[test]
    fn stop_cuts_remaining_sessions_after_release() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-sprel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let srv2 = Arc::clone(&srv);
            let stop2 = Arc::clone(&stop);
            std::thread::spawn(move || {
                // M3 S2：形参换成两源入口；`from_listener` = 改前的单 UDS 形态（语义零改）
                let intake = ServiceIntake::from_listener(ln).expect("入口");
                let _ = srv2.serve_stoppable(intake, stop2);
            });
        }
        let mut c1 = UnixStream::connect(&sock).unwrap();
        c1.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write_control(&mut c1, TYPE_REQUEST, br#"{"role":"send","warmup_ms":0,"window_ms":100}"#).unwrap();
        let mut c2 = UnixStream::connect(&sock).unwrap();
        c2.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        write_control(&mut c2, TYPE_REQUEST, br#"{"role":"send","warmup_ms":0,"window_ms":100}"#).unwrap();
        let wait_live = |want: usize| {
            let t0 = Instant::now();
            while t0.elapsed() < Duration::from_secs(3) {
                if srv.live() == want {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            false
        };
        assert!(wait_live(2), "两条会话应在册（实 {}）", srv.live());
        // 会话 1 收口
        write_control(&mut c1, TYPE_START, &[]).unwrap();
        write_control(&mut c1, TYPE_FINISH, &[]).unwrap();
        let mut r1 = BufReader::new(c1);
        let (rt, _) = read_frame(&mut r1).expect("report 应到达");
        assert_eq!(rt, TYPE_REPORT);
        assert!(wait_live(1), "会话 1 收口后应在册 1（实 {}）", srv.live());
        // 收工：close_all 切断仍在跑的会话 2
        stop.store(true, Ordering::Release);
        let mut buf = [0u8; 8];
        match c2.read(&mut buf) {
            Ok(0) => {}
            Ok(n) => panic!("收工后不应有数据（{n}B）"),
            Err(e) => assert!(
                !matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut),
                "应是 EOF/连接类错误而非超时：{e}"
            ),
        }
        assert_eq!(srv.live(), 0, "收工后在册归零");
        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---------- F6c busy 路径帧吞 / 绝对窗 ----------

    /// **滴流 busy 连接**（H2 回归）：占满并发后第 13 条按 400ms/字节滴 15B 帧头 ⇒
    /// 帧吞窗（2s **绝对**期限）到点即回 busy report 并释放线程（修前 per-syscall
    /// 续命 ⇒ 每字节都重置 2s，单连接可挂数小时）。
    #[test]
    fn busy_path_dribble_released_by_absolute_window() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spbd2-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });
        let mut holders = Vec::new();
        for _ in 0..MAX_CONNS {
            let mut c = UnixStream::connect(&sock).unwrap();
            write_control(&mut c, TYPE_REQUEST, br#"{"role":"recv","warmup_ms":1000,"window_ms":5000}"#).unwrap();
            holders.push(c);
        }
        std::thread::sleep(Duration::from_millis(200));
        // 第 13 条：滴流（每 700ms 1 字节，共 15B 帧头；帧头 10.5s 才凑齐——修前
        // per-syscall 续命 ⇒ 回帧要等帧头凑齐/对端停发）
        let mut c = UnixStream::connect(&sock).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let rd = c.try_clone().unwrap();
        rd.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let hdr = [0u8; 15]; // 全是 0：magic 不符，读满 15B 后 read_frame_header 会报错
        let writer = std::thread::spawn(move || {
            for b in hdr {
                if c.write_all(&[b]).is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(700));
            }
        });
        let t0 = Instant::now();
        let mut r = BufReader::new(rd);
        let rep = read_frame(&mut r).expect("滴流 busy 连接仍须按期拿到 report");
        let dt = t0.elapsed();
        assert_eq!(rep.0, TYPE_REPORT);
        assert!(String::from_utf8(rep.1).unwrap().contains("\"error\":\"busy\""));
        // 最坏墙钟 = 2s（帧）+ 2s（写）+ 1s（吞输入）= 5s ⇒ 阈值 8s 留 ≥1.6× 余量；
        // 修前形态（单次 arm）实测 ≈9.9s ⇒ 仍必红。
        assert!(dt < Duration::from_secs(8), "帧吞窗绝对期限 2s（+吞输入窗）应到点放行（实 {dt:?}）");
        let _ = writer.join();
        drop(holders);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// busy 路径喂「非 request 首帧 + 大载荷」⇒ 帧边界不错位、回帧到达、按期返回
    /// （≤ 帧吞 2s + 吞输入 1s + 余量）。
    #[test]
    fn busy_path_drains_frame_and_bounded() {
        let dir = std::env::temp_dir().join(format!("homeway-rs-spbd-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("speedtest.sock");
        let _ = std::fs::remove_file(&sock);
        let ln = UnixListener::bind(&sock).unwrap();
        let logf: crate::Logf = Arc::new(|_| {});
        let srv = Arc::new(SpeedtestServer::new(Arc::clone(&logf)));
        let srv2 = Arc::clone(&srv);
        std::thread::spawn(move || {
            let _ = srv2.serve(ln);
        });
        // 占满并发
        let mut holders = Vec::new();
        for _ in 0..MAX_CONNS {
            let mut c = UnixStream::connect(&sock).unwrap();
            write_control(&mut c, TYPE_REQUEST, br#"{"role":"recv","warmup_ms":1000,"window_ms":5000}"#).unwrap();
            holders.push(c);
        }
        std::thread::sleep(Duration::from_millis(200));
        // 第 13 条：首帧是 data 大载荷（非 request）+ 后续垃圾滴流
        let mut c = UnixStream::connect(&sock).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let big = vec![0u8; 60 * 1024];
        write_control(&mut c, TYPE_DATA, &big).unwrap();
        let t0 = Instant::now();
        let mut r = BufReader::new(c);
        let rep = read_frame(&mut r).expect("busy report 应到达（帧吞不错位）");
        assert_eq!(rep.0, TYPE_REPORT);
        assert!(String::from_utf8(rep.1).unwrap().contains("\"error\":\"busy\""));
        assert!(t0.elapsed() < Duration::from_secs(4), "帧吞 + 吞输入窗应受界（实 {:?}）", t0.elapsed());
        assert_eq!(srv.stats.rejected.load(Ordering::Relaxed), 1);
        drop(holders);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
