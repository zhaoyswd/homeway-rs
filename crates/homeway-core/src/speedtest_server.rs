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
/// 单连接硬超时（覆盖 5s 预热 + 15s 窗口 + 控制帧余量）。
const CONN_TIMEOUT: Duration = Duration::from_secs(30);
/// 预热时长上限。
const MAX_WARMUP: Duration = Duration::from_secs(5);
/// 窗口时长区间。
const MIN_WINDOW: Duration = Duration::from_millis(100);
const MAX_WINDOW: Duration = Duration::from_secs(15);
/// data 载荷上限（u16 长度场）+ 服务端发送块（Go SendBlock）。
const MAX_BLOCK: usize = 65535;

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
    /// 在册连接表（上限判定与登记同锁——受理/摘除的原子面）。
    conns: Arc<Mutex<CountingRegistry>>,
    pub stats: Arc<SessStats>,
}

struct CountingRegistry {
    live: usize,
    /// 在册会话句柄（收工 CloseAll——Go speedSrv.Close() 语义，评审 M12）。
    conns: Vec<UnixStream>,
}

impl CountingRegistry {
    /// 受理：满员 None；否则在册 +1 并返回**单调会话号**（评审 H3——Go connreg
    /// 单调自增语义，可 >MAX_CONNS）。
    fn admit(&mut self, id_alloc: &AtomicU64, conn: &UnixStream) -> Option<u64> {
        if self.live >= MAX_CONNS {
            return None;
        }
        self.live += 1;
        if let Ok(c) = conn.try_clone() {
            self.conns.push(c);
        }
        Some(id_alloc.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn release(&mut self) {
        self.live = self.live.saturating_sub(1);
        self.conns.clear(); // 会话收线由各线程自行收（守卫侧只做簿记）
    }

    /// 收工：断开全部在跑会话（Go connreg.CloseAll 同义，评审 M12）。
    fn close_all(&mut self) {
        for c in self.conns.drain(..) {
            let _ = c.shutdown(std::net::Shutdown::Both);
        }
        self.live = 0;
    }
}

impl SpeedtestServer {
    pub fn new(logf: crate::Logf) -> Self {
        Self {
            logf,
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

    /// 可停形态（引擎收工面——files::serve_stoppable 同构；停时断开在跑会话，M12）。
    pub fn serve_stoppable(self: &Arc<Self>, ln: UnixListener, stop: Arc<std::sync::atomic::AtomicBool>) -> std::io::Result<()> {
        use std::sync::atomic::Ordering as OD;
        ln.set_nonblocking(true)?;
        loop {
            if stop.load(OD::Relaxed) {
                if let Ok(mut reg) = self.conns.lock() {
                    reg.close_all();
                }
                return Ok(());
            }
            match ln.accept() {
                Ok((conn, _)) => {
                    let _ = conn.set_nonblocking(false);
                    let srv = Arc::clone(self);
                    std::thread::Builder::new()
                        .name("homeway-speedtest".into())
                        .stack_size(512 * 1024)
                        .spawn(move || srv.serve_conn(conn))
                        .ok();
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(200));
                }
                Err(_) => return Ok(()),
            }
        }
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
            match reg.admit(&self.stats.next, &conn) {
                Some(id) => {
                    self.stats.total.fetch_add(1, Ordering::Relaxed);
                    id
                }
                None => {
                    drop(reg);
                    self.stats.rejected.fetch_add(1, Ordering::Relaxed);
                    (self.logf)(&format!("speedtest: 会话拒绝（并发上限 {MAX_CONNS}）"));
                    reply_then_close(conn, &report_json(0, 0, 0, Some("busy")));
                    return;
                }
            }
        };
        let _guard = ConnGuard { conns: Arc::clone(&self.conns) };

        // 单连接硬超时（**绝对期限**——评审 M13：set_read_timeout 是 per-syscall，
        // 慢滴客户端每次成功读都续命 30s；每次读写前按剩余时间收敛设置）
        let deadline = Instant::now() + CONN_TIMEOUT;
        set_io_deadline(&conn, deadline);
        let w = match conn.try_clone() {
            Ok(c) => c,
            Err(_) => return,
        };
        let mut r = BufReader::with_capacity(128 * 1024, conn);

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
            finish_with_error(&w, &format!("未知角色 {:?}", req.role));
            return;
        }
        if warmup > MAX_WARMUP {
            finish_with_error(
                &w,
                &format!("预热 {} 超上限 {}", fmt_duration_go_ms(warmup), fmt_duration_go_ms(MAX_WARMUP)),
            );
            return;
        }
        if window < MIN_WINDOW || window > MAX_WINDOW {
            finish_with_error(
                &w,
                &format!(
                    "窗口 {} 超出 [{}, {}]",
                    fmt_duration_go_ms(window),
                    fmt_duration_go_ms(MIN_WINDOW),
                    fmt_duration_go_ms(MAX_WINDOW)
                ),
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
            self.serve_recv(conn_id, &w, warmup, window);
        } else {
            self.serve_send(conn_id, &mut r, &w);
        }
    }

    /// role=recv：预热发 → 窗口发 → report。发送侧按接收方 TCP 背压自然限速。
    ///
    /// R8-2 归因插桩：窗口段按 1s 切片泵送（共享同一截止时刻——墙钟行为与整段
    /// pump_data 等价，切片只用于计量），末尾多打一行逐秒字节序列 + 尾 3s 速率
    /// ——真机下行「爬坡支配 vs 稳态封顶」的判别面（窗口均值会掩盖前者）。
    fn serve_recv(&self, id: u64, w: &UnixStream, warmup: Duration, window: Duration) {
        let mut bw = BufWriter::new(w);
        let block = vec![0u8; MAX_BLOCK];
        let mut seq: u32 = 0;
        let t0 = Instant::now();
        let warmup_bytes = pump_data(&mut bw, &block, &mut seq, warmup);
        if bw.flush().is_err() {
            (self.logf)(&format!("speedtest: 会话 #{id} 异常（下行发送：flush 失败）"));
            return;
        }
        let window_deadline = Instant::now() + window;
        let mut per_sec: Vec<i64> = Vec::with_capacity(window.as_secs() as usize + 1);
        loop {
            let now = Instant::now();
            if now >= window_deadline {
                break;
            }
            let slice = (window_deadline - now).min(Duration::from_secs(1));
            per_sec.push(pump_data(&mut bw, &block, &mut seq, slice));
        }
        let window_bytes: i64 = per_sec.iter().sum();
        if bw.flush().is_err() {
            (self.logf)(&format!("speedtest: 会话 #{id} 异常（下行发送：flush 失败）"));
            return;
        }
        let rep = report_json(window_bytes, warmup_bytes, t0.elapsed().as_millis() as i64, None);
        if write_control(&mut bw, TYPE_REPORT, rep.as_bytes()).is_err() || bw.flush().is_err() {
            (self.logf)(&format!("speedtest: 会话 #{id} 异常（回报告：写失败）"));
            return;
        }
        // 尾 3s 速率（不足 3s 取全部）：末段均值——与窗口均值对比即可判别
        // 「窗口内仍在爬坡」（尾 ≫ 均值）还是「稳态封顶」（尾 ≈ 均值）。
        let tail_secs = per_sec.len().min(3);
        let tail_bytes: i64 = per_sec[per_sec.len() - tail_secs..].iter().sum();
        let tail_mbps = if tail_secs > 0 {
            tail_bytes as f64 * 8.0 / (tail_secs as f64 * 1_000_000.0)
        } else {
            0.0
        };
        // E13 结算行（role=recv）
        (self.logf)(&format!(
            "speedtest: 会话 #{id} role=recv bytes={window_bytes}（含预热 {warmup_bytes}）用时={}ms",
            t0.elapsed().as_millis()
        ));
        // R8-2 归因行（逐秒载荷 MB + 尾 3s Mbps）
        (self.logf)(&format!(
            "speedtest: 会话 #{id} role=recv 逐秒MB=[{}] 尾{}s={tail_mbps:.0}Mbps",
            per_sec
                .iter()
                .map(|b| format!("{:.1}", *b as f64 / (1024.0 * 1024.0)))
                .collect::<Vec<_>>()
                .join(","),
            tail_secs,
        ));
    }

    /// role=send：读帧计数——START 前 = 预热，START 后 = 窗口；FINISH 触发 report。
    fn serve_send(&self, id: u64, r: &mut BufReader<UnixStream>, w: &UnixStream) {
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
                    if payload_len > MAX_BLOCK {
                        finish_with_error(w, &format!("data 载荷 {payload_len} 超上限 {MAX_BLOCK}"));
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
                        finish_with_error(w, "重复 START");
                        return;
                    }
                    started = true;
                    start_at = Instant::now();
                }
                TYPE_FINISH => {
                    if !started {
                        finish_with_error(w, "FINISH 前无 START");
                        return;
                    }
                    let rep = report_json(
                        window_bytes,
                        warmup_bytes,
                        start_at.elapsed().as_millis() as i64,
                        None,
                    );
                    let mut bw = BufWriter::new(w);
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
                    finish_with_error(w, &format!("窗口期收到类型 {other}"));
                    return;
                }
            }
        }
    }
}

/// 按绝对期限收敛设置读写超时（评审 M13）。
fn set_io_deadline(conn: &UnixStream, deadline: Instant) {
    let remain = deadline.saturating_duration_since(Instant::now());
    if remain.is_zero() {
        let _ = conn.set_read_timeout(Some(Duration::from_millis(1)));
        let _ = conn.set_write_timeout(Some(Duration::from_millis(1)));
        return;
    }
    let _ = conn.set_read_timeout(Some(remain));
    let _ = conn.set_write_timeout(Some(remain));
}

/// 受理计数守卫（任何退出路径摘除在册）。
struct ConnGuard {
    conns: Arc<Mutex<CountingRegistry>>,
}

impl Drop for ConnGuard {
    fn drop(&mut self) {
        if let Ok(mut c) = self.conns.lock() {
            c.release();
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

/// 失败收场：回 report（error 形态）后冲刷。
fn finish_with_error(w: &UnixStream, msg: &str) {
    let mut bw = BufWriter::new(w);
    let rep = report_json(0, 0, 0, Some(msg));
    let _ = write_control(&mut bw, TYPE_REPORT, rep.as_bytes());
    let _ = bw.flush();
}

/// busy/link_down 类拒绝回帧的有序收口（Go ReplyThenClose）：先有界吞一帧（请求）→
/// 回 report 并冲刷 → 短窗继续吞输入（send 角色在回帧抵达前还在泵）→ 关。
fn reply_then_close(conn: UnixStream, error_report: &str) {
    let Ok(r_stream) = conn.try_clone() else { return };
    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
    let mut r = BufReader::new(r_stream);
    {
        let mut sink = [0u8; 512];
        let _ = read_frame_bounded(&mut r, &mut sink);
    }
    let mut w = conn;
    let _ = w.set_write_timeout(Some(Duration::from_secs(2)));
    let mut bw = BufWriter::new(&w);
    let _ = write_control(&mut bw, TYPE_REPORT, error_report.as_bytes());
    let _ = bw.flush();
    drop(bw);
    // 继续吞输入一小窗（期限内对端先关则自然提前结束）
    let _ = w.set_read_timeout(Some(Duration::from_secs(1)));
    let mut discard = [0u8; 4096];
    while let Ok(n) = w.read(&mut discard) {
        if n == 0 {
            break;
        }
    }
}

/// 有界吞一帧（拒绝路径用；失败也继续回帧——连接本就异常，尽力而为）。
fn read_frame_bounded(r: &mut BufReader<UnixStream>, buf: &mut [u8]) -> std::io::Result<()> {
    let (typ, n) = read_frame_header(r)?;
    let _ = typ;
    if n > buf.len() {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "帧超长"));
    }
    r.read_exact(&mut buf[..n])
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
}
