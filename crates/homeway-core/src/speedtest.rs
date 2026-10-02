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
const MAGIC: [u8; 4] = *b"SPED";
const HEADER: usize = 15;
const TYPE_REQUEST: u8 = 1;
const TYPE_START: u8 = 2;
const TYPE_FINISH: u8 = 3;
const TYPE_DATA: u8 = 4;
const TYPE_REPORT: u8 = 5;
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

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SpeedtestError {
    #[error("测速连接失败：{0}")]
    Conn(#[from] ConnErr),
    #[error("帧协议错误：{0}")]
    Frame(String),
    #[error("服务端报告错误：{0}")]
    Report(String),
    #[error("参数非法：{0}")]
    InvalidArg(String),
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
fn crc32_ieee(buf: &[u8]) -> u32 {
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

fn zero_crc(n: usize) -> u32 {
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
        self.buf.clear();
        self.buf.reserve(HEADER + payload_len);
        self.buf.extend_from_slice(&MAGIC);
        self.buf.push(typ);
        self.buf.extend_from_slice(&seq.to_le_bytes());
        self.buf.extend_from_slice(&(payload_len as u16).to_le_bytes());
        self.buf.extend_from_slice(&crc.to_le_bytes());
        self.seq += 1;
    }

    /// 控制帧（request/start/finish；载荷进 crc；**seq 恒 0**——Go WriteControl 同义）。
    fn control(&mut self, typ: u8, payload: &[u8]) -> &[u8] {
        self.header(typ, 0, payload.len(), crc32_ieee(payload));
        self.buf.extend_from_slice(payload);
        &self.buf
    }

    /// data 帧（载荷全零，整帧拼好——单次写全，避免头/载荷拆包；seq 从 1 起递增，
    /// Go writeData 的 `*seq+1` 前置自增同义）。
    fn data(&mut self, payload_len: usize) -> &[u8] {
        self.seq += 1;
        self.header(TYPE_DATA, self.seq, payload_len, zero_crc(payload_len));
        let start = self.buf.len();
        self.buf.resize(start + payload_len, 0);
        &self.buf
    }
}

fn write_all(client: &Client, id: u64, data: &[u8]) -> Result<(), SpeedtestError> {
    let mut off = 0;
    while off < data.len() {
        let n = client.write(id, data[off..].to_vec())?;
        if n == 0 {
            return Err(SpeedtestError::Frame("写通道关闭".into()));
        }
        off += n;
    }
    Ok(())
}

enum FrameIn {
    /// data 帧：载荷已在读侧消耗丢弃，只回长度（下行零拷贝读路径）。
    Data { payload_len: usize },
    Other { typ: u8, payload: Vec<u8> },
}

/// 读一帧（按头分派：data 载荷零消耗丢弃；其它帧载荷带回并校验 crc）。
fn read_frame(client: &Client, id: u64) -> Result<FrameIn, SpeedtestError> {
    let header = read_exact::<HEADER>(client, id)?;
    if header[..4] != MAGIC {
        return Err(SpeedtestError::Frame(
            "帧魔数不符（流已错位或非 speedtest 服务）".into(),
        ));
    }
    let typ = header[4];
    let len = u16::from_le_bytes([header[9], header[10]]) as usize;
    let crc = u32::from_le_bytes([header[11], header[12], header[13], header[14]]);
    if typ == TYPE_DATA {
        let mut remain = len;
        while remain > 0 {
            let chunk = client.read(id)?;
            if chunk.is_empty() {
                return Err(SpeedtestError::Frame("data 载荷被截断".into()));
            }
            remain = remain.saturating_sub(chunk.len());
        }
        if crc != zero_crc(len) {
            return Err(SpeedtestError::Frame("data 帧 crc 不符".into()));
        }
        return Ok(FrameIn::Data { payload_len: len });
    }
    let mut payload = Vec::with_capacity(len);
    let mut remain = len;
    while remain > 0 {
        let chunk = client.read(id)?;
        if chunk.is_empty() {
            return Err(SpeedtestError::Frame("控制帧载荷被截断".into()));
        }
        remain -= chunk.len().min(remain);
        payload.extend_from_slice(&chunk);
    }
    if crc != crc32_ieee(&payload) {
        return Err(SpeedtestError::Frame("帧 crc 不符".into()));
    }
    Ok(FrameIn::Other { typ, payload })
}

fn read_exact<const N: usize>(client: &Client, id: u64) -> Result<[u8; N], SpeedtestError> {
    let mut out = [0u8; N];
    let mut off = 0;
    while off < N {
        let chunk = client.read(id)?;
        if chunk.is_empty() {
            return Err(SpeedtestError::Frame("流在帧中途关闭".into()));
        }
        let n = chunk.len().min(N - off);
        out[off..off + n].copy_from_slice(&chunk[..n]);
        off += n;
    }
    Ok(out)
}

/// 请求帧载荷（JSON，Go requestJSON 同形）。
fn request_payload(role: &str, warmup_ms: u64, window_ms: u64) -> Vec<u8> {
    format!(r#"{{"role":"{role}","warmup_ms":{warmup_ms},"window_ms":{window_ms}}}"#).into_bytes()
}

#[derive(Default, Clone, Copy)]
struct Report {
    bytes: i64,
    warmup_bytes: i64,
    wall_ms: i64,
}

fn parse_report(payload: &[u8]) -> Result<Report, SpeedtestError> {
    // 极小 JSON 面（三数字 + 可选 error 串）——手解避免 serde_json 进 core（依赖纪律）。
    let s = std::str::from_utf8(payload)
        .map_err(|_| SpeedtestError::Frame("report 非 UTF-8".into()))?;
    let mut r = Report::default();
    let mut error: Option<String> = None;
    for kv in s.trim_matches(['{', '}']).split(',') {
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
                let e = v.trim_matches('"');
                if !e.is_empty() && v != "null" {
                    error = Some(e.to_owned());
                }
            }
            _ => {}
        }
    }
    if let Some(e) = error {
        return Err(match e.as_str() {
            "busy" => SpeedtestError::Report("出口测速服务并发满员，请稍后再试".into()),
            "link_down" => SpeedtestError::Report("链路正在恢复".into()),
            other => SpeedtestError::Report(other.to_owned()),
        });
    }
    Ok(r)
}

/// 跑完整一轮（下行 → 上行；读数由服务端 report 报）。拨号目标恒隧道 IP:7803。
pub fn run(client: &Client, params: Params, logf: &dyn Fn(&str)) -> Result<SpeedtestResult, SpeedtestError> {
    let p = params.normalized().map_err(SpeedtestError::InvalidArg)?;
    let dst = SocketAddrV4::new(crate::wgcore::SERVER_TUNNEL_IP, SPEEDTEST_PORT);
    let t0 = Instant::now();
    logf(&format!(
        "speedtest: 开跑（down {}流 warmup={:?} window={:?}）",
        p.streams, p.warmup, p.down
    ));

    // ---- 下行：拨齐全部连接 → 统一发请求（各流窗口对齐，design D3）----
    let mut down_ids = Vec::with_capacity(p.streams);
    for _ in 0..p.streams {
        down_ids.push(client.connect(dst)?);
    }
    let mut w = Frame::new();
    for &id in &down_ids {
        let payload = request_payload("recv", p.warmup.as_millis() as u64, p.down.as_millis() as u64);
        write_all(client, id, w.control(TYPE_REQUEST, &payload))?;
    }
    let window_start = Instant::now() + p.warmup + PHASE_SLACK;
    // 每流一线程并发读（Go goroutine 同构；窗口内到达才计读数）
    let mut down_bytes: i64 = 0;
    let mut down_usage: i64 = 0;
    let mut srv_bytes: i64 = 0;
    let mut srv_warm: i64 = 0;
    let down_join: Result<(), SpeedtestError> = std::thread::scope(|s| {
        let handles: Vec<_> = down_ids
            .iter()
            .map(|&id| {
                s.spawn(move || -> Result<(i64, i64, i64, i64), SpeedtestError> {
                    let (mut got, mut used) = (0i64, 0i64);
                    let (sb, sw): (i64, i64);
                    loop {
                        match read_frame(client, id)? {
                            FrameIn::Data { payload_len } => {
                                if Instant::now() >= window_start {
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
    let mut up_ids = Vec::with_capacity(p.streams);
    for _ in 0..p.streams {
        up_ids.push(client.connect(dst)?);
    }
    for &id in &up_ids {
        let payload = request_payload("send", p.warmup.as_millis() as u64, p.up.as_millis() as u64);
        write_all(client, id, w.control(TYPE_REQUEST, &payload))?;
    }
    let mut up_bytes: i64 = 0;
    let mut up_usage: i64 = 0;
    let mut wall_sum: i64 = 0;
    let up_join: Result<(), SpeedtestError> = std::thread::scope(|s| {
        let handles: Vec<_> = up_ids
            .iter()
            .map(|&id| {
                s.spawn(move || -> Result<(i64, i64, i64), SpeedtestError> {
                    let mut f = Frame::new();
                    // 预热泵 → START → 窗口泵 → FINISH → report（接收端报数）
                    let warm = pump(client, id, &mut f, p.warmup)?;
                    write_all(client, id, f.control(TYPE_START, &[]))?;
                    let win = pump(client, id, &mut f, p.up)?;
                    write_all(client, id, f.control(TYPE_FINISH, &[]))?;
                    let (used, bytes, wall) = match read_frame(client, id)? {
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

    for &id in up_ids.iter().chain(down_ids.iter()) {
        let _ = client.close(id);
    }
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

/// 泵一段 data（整帧单次写全；返回本段 payload 字节数）。
fn pump(client: &Client, id: u64, f: &mut Frame, dur: Duration) -> Result<i64, SpeedtestError> {
    let deadline = Instant::now() + dur;
    let mut total: i64 = 0;
    while Instant::now() < deadline {
        write_all(client, id, f.data(BLOCK))?;
        total += BLOCK as i64;
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
        assert_eq!(u32::from_le_bytes(frame[5..9].try_into().unwrap()), 0, "控制帧 seq 恒 0（Go 同义）");
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
        // data 整帧形状：载荷区全零；seq 从 1 起递增
        let mut f2 = Frame::new();
        let d = f2.data(8).to_vec();
        assert_eq!(u32::from_le_bytes(d[5..9].try_into().unwrap()), 1);
        assert_eq!(d.len(), HEADER + 8);
        assert_eq!(&d[HEADER..], &[0u8; 8]);
    }

    #[test]
    fn params_validation_matches_go_bounds() {
        assert!(Params::default().normalized().is_ok());
        assert!(Params {
            down: Duration::from_secs(16),
            ..Default::default()
        }
        .normalized()
        .is_err(), "窗口超 15s 应拒绝");
        assert!(Params { streams: 7, ..Default::default() }.normalized().is_err());
        assert!(
            Params { warmup: Duration::from_secs(6), ..Default::default() }
                .normalized()
                .is_err()
        );
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

    #[test]
    fn report_parse_and_error_mapping() {
        let r = parse_report(br#"{"bytes":123,"warmup_bytes":45,"wall_ms":678}"#).unwrap();
        assert_eq!((r.bytes, r.warmup_bytes, r.wall_ms), (123, 45, 678));
        let r = parse_report(br#"{"bytes":0,"warmup_bytes":0,"wall_ms":0,"error":"busy"}"#);
        assert!(matches!(r, Err(SpeedtestError::Report(m)) if m.contains("满员")));
        let r = parse_report(br#"{"bytes":1,"warmup_bytes":0,"wall_ms":9,"error":"link_down"}"#);
        assert!(matches!(r, Err(SpeedtestError::Report(m)) if m.contains("链路")));
    }
}
