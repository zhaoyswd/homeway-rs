//! 测速壳面（语义真源 `baseline:clientcore/cmd/clientcore/app_speedtest.go`；
//! 引擎 = `crate::speedtest`，轮级状态机/窗口对齐/期限与归因全在引擎——
//! 「口径与手机一致由共享实现保证」）。
//!
//! 本模块只做三件事（Go app_speedtest.go 同义）：
//! - 三导出（Start/Status/Cancel）与 JSON 信封逐字段对齐；
//! - 桥鉴权拨号（UDS 拨 + 鉴权首包）——`bridge_down`/`bridge_auth` 在这层产生；
//! - 壳层参数前置校验（invalid_arg）与结果信封映射。
//!
//! via/rtt 不在这层取：App 侧在开跑时从状态快照冻结。

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Map, Value};

use crate::speedtest::{Params, SpeedtestResult};

pub use crate::speedtest::Params as EngineParams;

use super::term_op::write_auth;

/// 出口测速服务端口（= 本仓 speedtest_server 的默认端口；桥拨号消费）。
///
/// **M3 口径**：QUIC 档不再拨它——桥按此端口选 `STREAM[tag=speedtest]`
/// （`facade/quic_stream.rs::tag_for_port`）；WG 档照旧拨端口。值/字段面不变（E1）。
pub const SPEEDTEST_SERVICE_PORT: u16 = 7803;

/// Start 的入参（JSON；字段名与迁移前逐字一致——camelCase，评审 r1-F08 整改：
/// 无 rename 时非 0 窗口值会被静默忽略）。
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SpeedParams {
    /// 桥鉴权 blob（状态 JSON 的 bridgeAuth）。
    #[serde(default)]
    pub auth: String,
    /// 测速桥 socket 路径（状态 JSON 的 bridgeSpeedSock）。
    #[serde(default)]
    pub sock: String,
    #[serde(default)]
    pub down_ms: i64,
    #[serde(default)]
    pub up_ms: i64,
    #[serde(default)]
    pub warmup_ms: i64,
    #[serde(default)]
    pub streams: i64,
}

/// 引擎测速回执的归一面（crate::speedtest 的 Result + 失败码 → 信封形态）。
pub enum SpeedOutcome {
    Ok(SpeedtestResult),
    Fail { reason: String, msg: String },
}

/// ClientCoreSpeedTestStart：同步跑完整轮（NAPI 在 async work 线程上；本面由调用方
/// 决定在哪个执行器上跑）。失败一律 `{"ok":false,"reason","msg"}`。
pub fn speed_start(raw: &str, run: impl FnOnce(Params) -> SpeedOutcome) -> String {
    let p: SpeedParams = match serde_json::from_str(raw) {
        Ok(p) => p,
        Err(e) => return speed_fail("invalid_arg", format!("参数不是合法 JSON：{e}")),
    };
    if p.auth.is_empty() || p.sock.is_empty() {
        return speed_fail(
            "bridge_down",
            "桥未就绪（缺 auth/sock：VPN 未连接且服务会话未就绪）",
        );
    }
    let params = Params {
        down: Duration::from_millis(p.down_ms.max(0) as u64),
        up: Duration::from_millis(p.up_ms.max(0) as u64),
        warmup: Duration::from_millis(p.warmup_ms.max(0) as u64),
        streams: p.streams.max(0) as usize,
    };
    let params = match params.normalized() {
        Ok(p) => p,
        Err(e) => return speed_fail("invalid_arg", e),
    };
    match run(params) {
        SpeedOutcome::Ok(res) => {
            let mut m = Map::new();
            m.insert("ok".into(), Value::from(true));
            m.insert("phase".into(), Value::String("done".into()));
            m.insert("downBps".into(), num(res.down_bps));
            m.insert("upBps".into(), num(res.up_bps));
            m.insert("usageDown".into(), Value::from(res.usage_down));
            m.insert("usageUp".into(), Value::from(res.usage_up));
            m.insert("wallMs".into(), Value::from(res.wall_ms));
            Value::Object(m).to_string()
        }
        SpeedOutcome::Fail { reason, msg } => speed_fail(&reason, msg),
    }
}

fn num(v: f64) -> Value {
    // Go json.Marshal 对 float64 产最短形态；serde_json 同策略（Number 保精度）。
    serde_json::Number::from_f64(v)
        .map(Value::Number)
        .unwrap_or(Value::from(0))
}

/// speedFail 业务失败的统一返回。
pub fn speed_fail(reason: &str, msg: impl Into<String>) -> String {
    let mut m = Map::new();
    m.insert("ok".into(), Value::from(false));
    m.insert("reason".into(), Value::String(reason.to_owned()));
    m.insert("msg".into(), Value::String(msg.into()));
    Value::Object(m).to_string()
}

/// ClientCoreSpeedTestStatus 的信封（快照输入由引擎侧持有方给；字段按需出现——
/// usage 随轮次、dir/bytes/instBps 随窗口、elapsedMs 随开跑）。
pub struct SpeedSnapshotIn {
    pub phase: &'static str,
    pub reason: String,
    pub usage: Option<(i64, i64)>,
    pub live: Option<(&'static str, i64, f64)>,
    pub elapsed_ms: i64,
}

/// Status 信封（`elapsedMs >= 0` 才出现——Go 同缺省条件）。
pub fn speed_status_json(s: &SpeedSnapshotIn) -> String {
    let mut m = Map::new();
    m.insert("phase".into(), Value::String(s.phase.to_owned()));
    m.insert("reason".into(), Value::String(s.reason.clone()));
    if let Some((d, u)) = s.usage {
        m.insert("usageDown".into(), Value::from(d));
        m.insert("usageUp".into(), Value::from(u));
    }
    if let Some((dir, bytes, inst)) = s.live {
        m.insert("dir".into(), Value::String(dir.to_owned()));
        m.insert("bytes".into(), Value::from(bytes));
        m.insert("instBps".into(), num(inst));
    }
    if s.elapsed_ms >= 0 {
        m.insert("elapsedMs".into(), Value::from(s.elapsed_ms));
    }
    Value::Object(m).to_string()
}

/// ClientCoreSpeedTestCancel：恒 `{"ok":true}`（取消动作由引擎侧持有方执行）。
pub fn speed_cancel_json() -> String {
    let mut m = Map::new();
    m.insert("ok".into(), Value::from(true));
    Value::Object(m).to_string()
}

/// App 形态的引擎承载（本机测速桥：UDS + 鉴权首包 → 拆半读写 + kill 面）。
/// `raw` 保留一份克隆专门给 kill（watchdog 到点 shutdown Both 打断在途读写——
/// 拆半后的 Box<dyn Read> 无法从外部关）。
pub struct BridgeSpeedConn {
    raw: UnixStream,
    r: Mutex<Box<dyn Read + Send>>,
    w: Mutex<Box<dyn super::bridge_host::WriteHalf + Send>>,
    /// **绝对期限**（评审 r1-F13：set_read/write_timeout 是 per-syscall 超时——
    /// 慢滴对端每次成功读写都给整份期限续命；Go SetDeadline 是绝对时刻。这里记
    /// deadline 时刻，每次读写前按剩余量收敛 per-op 超时，到点后读写立即失败——
    /// 复刻 speedtest_server 侧 set_io_deadline 的 M13 整改同款语义）。
    deadline: Mutex<Option<Instant>>,
}

impl BridgeSpeedConn {
    /// per-op 超时收敛（绝对期限的执行半边）。
    fn op_timeout(&self) -> std::io::Result<Option<std::time::Duration>> {
        let dl = self.deadline.lock().unwrap_or_else(|e| e.into_inner());
        match *dl {
            None => Ok(None),
            Some(t) => {
                let remain = t.saturating_duration_since(Instant::now());
                if remain.is_zero() {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "连接期限已到（绝对期限）",
                    ))
                } else {
                    Ok(Some(remain))
                }
            }
        }
    }
}

impl crate::speedtest::SpeedConn for BridgeSpeedConn {
    fn set_deadline(&self, d: Option<std::time::Duration>) {
        // 绝对期限语义（见字段注记）：记时刻；每次读写前按剩余收敛 per-op 超时。
        // SO_RCV/SNDTIMEO 是 socket 级选项（dup fd 共享）——一处设置全体生效。
        let mut dl = self.deadline.lock().unwrap_or_else(|e| e.into_inner());
        *dl = d.map(|dur| Instant::now() + dur);
        self.raw.set_read_timeout(d).ok();
        self.raw.set_write_timeout(d).ok();
    }

    fn write_frame(&self, data: &[u8]) -> Result<(), crate::speedtest::SpeedtestError> {
        use crate::speedtest::SpeedtestError;
        let mut w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let mut off = 0;
        while off < data.len() {
            let remain = self.op_timeout();
            if remain.is_err() {
                return Err(SpeedtestError::Conn(crate::wgcore::ConnErr::Timeout));
            }
            self.raw.set_write_timeout(remain.unwrap()).ok();
            match w.write(&data[off..]) {
                Ok(0) => {
                    return Err(SpeedtestError::Frame(
                        "测速桥写通道返回 0（对端已关）".into(),
                    ))
                }
                Ok(n) => off += n,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err(SpeedtestError::Conn(crate::wgcore::ConnErr::Timeout));
                }
                Err(e) => return Err(SpeedtestError::Frame(format!("测速桥写失败：{e}"))),
            }
        }
        Ok(())
    }
    fn read_some(&self) -> Result<Vec<u8>, crate::speedtest::SpeedtestError> {
        use crate::speedtest::SpeedtestError;
        let mut r = self.r.lock().unwrap_or_else(|e| e.into_inner());
        let mut buf = [0u8; 128 * 1024];
        let remain = self.op_timeout();
        if let Err(_e) = remain {
            return Err(SpeedtestError::Conn(crate::wgcore::ConnErr::Timeout));
        }
        self.raw.set_read_timeout(remain.unwrap()).ok();
        match r.read(&mut buf) {
            Ok(0) => Ok(Vec::new()), // EOF
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                Err(SpeedtestError::Conn(crate::wgcore::ConnErr::Timeout))
            }
            Err(e) => Err(SpeedtestError::Frame(format!("测速桥读失败：{e}"))),
        }
    }
    fn kill(&self) {
        let _ = self.raw.shutdown(std::net::Shutdown::Both);
    }
}

/// App 引擎入口：拨桥 + 鉴权 → `speedtest::run_dial`（同步跑完整轮——NAPI 壳在
/// async work 线程上跑；失败信封同 `SpeedOutcome::Fail`）。cancel 位经参数注入；
/// `live` = 进度出口（M-7：SpeedHost 的 Status 面消费——相位 + 字节）。
pub fn app_run(
    auth: &str,
    sock: &str,
    p: &crate::speedtest::Params,
    logf: &dyn Fn(&str),
    cancel: Option<&std::sync::atomic::AtomicBool>,
    live: Option<&std::sync::Arc<crate::speedtest::LiveProgress>>,
) -> SpeedOutcome {
    let dial = || -> Result<std::sync::Arc<dyn crate::speedtest::SpeedConn>, crate::speedtest::SpeedtestError> {
        match speed_dial_conn(auth, sock, Duration::from_secs(10)) {
            Ok(c) => Ok(std::sync::Arc::new(c)),
            Err((reason, msg)) => {
                // 拨号/鉴权失败 = 注入缝错误（bridge_down/bridge_auth——引擎透传面）
                Err(crate::speedtest::SpeedtestError::Bridge(reason, msg))
            }
        }
    };
    match crate::speedtest::run_dial(&dial, *p, logf, cancel, live) {
        Ok(res) => SpeedOutcome::Ok(res),
        Err(e) => SpeedOutcome::Fail {
            reason: e.reason().to_string(),
            msg: e.to_string(),
        },
    }
}

/// 拨测速桥 + 鉴权 + 拆半（SpeedConn 承载构造）。
pub fn speed_dial_conn(
    auth_hex: &str,
    sock: &str,
    budget: Duration,
) -> Result<BridgeSpeedConn, (&'static str, String)> {
    let mut conn =
        super::bridge_host::connect_budget(std::path::Path::new(sock), budget).map_err(|e| {
            (
                "bridge_down",
                format!("测速通道暂时不可用（桥未就绪或正在恢复）：{e}"),
            )
        })?;
    // 鉴权写先带拨号预算（R8-8c：复核 r3 对 M-8 兜底「部分失真」的修正——此前预算
    // 只罩 connect、auth 写挂 None 可无限等；write_auth 是纯写（评审 r1-F15：读半
    // 无对象）。请求发出后的硬期限由引擎 set_deadline 挂 warmup+window+15s——绝对
    // 期限语义（评审 r1-F13），窗口期读阻塞是常态不在此设短值）。
    conn.set_read_timeout(Some(budget)).ok();
    conn.set_write_timeout(Some(budget)).ok();
    write_auth(&mut conn, auth_hex)
        .map_err(|e| ("bridge_auth", format!("测速通道鉴权失败：{e}")))?;
    conn.set_read_timeout(None).ok();
    conn.set_write_timeout(None).ok();
    let raw = conn
        .try_clone()
        .map_err(|e| ("bridge_down", format!("测速桥 fd 复制失败：{e}")))?;
    let boxed: Box<dyn super::bridge_host::BridgeStream> = Box::new(conn);
    let (r, w) = boxed
        .into_halves()
        .map_err(|e| ("bridge_down", format!("测速桥拆半失败：{e}")))?;
    Ok(BridgeSpeedConn {
        raw,
        r: Mutex::new(r),
        w: Mutex::new(w),
        deadline: Mutex::new(None),
    })
}

/// 测速桥拨号 + 鉴权（引擎的 Dial 缝；ctx 语义由 UnixStream deadline 承载——
/// 拨号预算/取消要能打断在途拨号，FIX-39）。
pub fn speed_dial(
    auth_hex: &str,
    sock: &str,
    budget: Duration,
) -> Result<UnixStream, (&'static str, String)> {
    // 工单④：UDS 拨号加 connect 预算（拨号预算/取消要能打断在途拨号——FIX-39 面）
    let conn = super::bridge_host::connect_budget(
        std::path::Path::new(sock),
        budget.max(Duration::from_millis(500)),
    )
    .map_err(|e| {
        (
            "bridge_down",
            format!("测速通道暂时不可用（桥未就绪或正在恢复）：{e}"),
        )
    })?;
    conn.set_read_timeout(Some(budget)).ok();
    conn.set_write_timeout(Some(budget)).ok();
    let mut c = conn;
    write_auth(&mut c, auth_hex).map_err(|e| ("bridge_auth", format!("测速通道鉴权失败：{e}")))?;
    Ok(c)
}

/// 拨号 + 鉴权 + 写请求帧（一次性命令的开口；引擎语义由调用方组装，此处提供
/// 共享件——帧常量在 crate::speedtest）。
pub fn speed_dial_and_request(
    auth_hex: &str,
    sock: &str,
    request: &[u8],
    budget: Duration,
) -> Result<UnixStream, (&'static str, String)> {
    let mut c = speed_dial(auth_hex, sock, budget)?;
    c.write_all(request)
        .map_err(|e| ("bridge_down", format!("发测速请求失败：{e}")))?;
    Ok(c)
}

/// 桥接运转的辅助：一次性整读（Status 轮询面不需要流式）。
pub fn read_to_end_timeout(c: &UnixStream, budget: Duration) -> std::io::Result<Vec<u8>> {
    let mut c = c.try_clone()?;
    c.set_read_timeout(Some(budget)).ok();
    let mut out = Vec::new();
    let started = Instant::now();
    let mut buf = [0u8; 16 * 1024];
    loop {
        use std::io::Read;
        match c.read(&mut buf) {
            Ok(0) => return Ok(out),
            Ok(n) => {
                out.extend_from_slice(&buf[..n]);
                if started.elapsed() > budget {
                    return Ok(out); // 预算内尽力收（测速流的窗口语义由引擎管）
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut =>
            {
                return Ok(out);
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_param_gates() {
        // 非法 JSON → invalid_arg
        let v: Value = serde_json::from_str(&speed_start("{oops", |_| unreachable!())).unwrap();
        assert_eq!(v["reason"], "invalid_arg");
        // 缺 auth/sock → bridge_down
        let v: Value =
            serde_json::from_str(&speed_start(r#"{"auth":"","sock":""}"#, |_| unreachable!()))
                .unwrap();
        assert_eq!(v["reason"], "bridge_down");
        // streams 非法（Params.normalized 拒）→ invalid_arg
        let v: Value = serde_json::from_str(&speed_start(
            r#"{"auth":"a","sock":"s","streams":99}"#,
            |p| match p.normalized() {
                Err(e) => SpeedOutcome::Fail {
                    reason: "invalid_arg".into(),
                    msg: e,
                },
                Ok(_) => unreachable!(),
            },
        ))
        .unwrap();
        assert_eq!(v["reason"], "invalid_arg");
    }

    /// 成功信封键面与键序（ok,phase,downBps,upBps,usageDown,usageUp,wallMs 的字典序 =
    /// downBps,ok,phase,upBps,usageDown,usageUp,wallMs）。
    #[test]
    fn success_envelope() {
        let out = speed_start(r#"{"auth":"a","sock":"s"}"#, |_| {
            SpeedOutcome::Ok(SpeedtestResult {
                down_bps: 123.5,
                up_bps: 456.0,
                usage_down: 1000,
                usage_up: 2000,
                wall_ms: 25000,
            })
        });
        let v: Value = serde_json::from_str(&out).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "downBps",
                "ok",
                "phase",
                "upBps",
                "usageDown",
                "usageUp",
                "wallMs"
            ]
        );
        assert_eq!(v["phase"], "done");
        assert_eq!(v["downBps"], 123.5);
    }

    /// Status 信封的按需键。
    #[test]
    fn status_envelope_optionals() {
        let idle = SpeedSnapshotIn {
            phase: "idle",
            reason: String::new(),
            usage: None,
            live: None,
            elapsed_ms: -1,
        };
        let v: Value = serde_json::from_str(&speed_status_json(&idle)).unwrap();
        assert_eq!(v.as_object().unwrap().len(), 2); // phase + reason
        let live = SpeedSnapshotIn {
            phase: "down",
            reason: String::new(),
            usage: Some((100, 200)),
            live: Some(("down", 5000, 12.5)),
            elapsed_ms: 3000,
        };
        let v: Value = serde_json::from_str(&speed_status_json(&live)).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "bytes",
                "dir",
                "elapsedMs",
                "instBps",
                "phase",
                "reason",
                "usageDown",
                "usageUp"
            ]
        );
        assert_eq!(v["dir"], "down");
    }

    #[test]
    fn cancel_envelope() {
        assert_eq!(speed_cancel_json(), r#"{"ok":true}"#);
    }
}

// ---------------------------------------------------------------------------
// SpeedHost：App 形态的轮级状态机（busy 门 + Cancel 真取消 + Status 快照）
// 语义真源 `baseline:clientcore/cmd/clientcore/app_speedtest.go` 的轮管理面。
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicBool, Ordering};

/// 一轮的进度快照（Status 信封源；phase 缺省 = idle）。
#[derive(Clone)]
struct RoundState {
    phase: &'static str,
    reason: String,
    usage: Option<(i64, i64)>,
    live: Option<(&'static str, i64, f64)>,
    started_at: Option<Instant>,
    /// 上次 status 采样的 (字节, 时刻)——instBps 的差分基线（M-7）。
    sample: Option<(i64, Instant)>,
}

impl Default for RoundState {
    fn default() -> Self {
        RoundState {
            phase: "idle",
            reason: String::new(),
            usage: None,
            live: None,
            started_at: None,
            sample: None,
        }
    }
}

/// 轮级状态机（Start 同步跑完整轮〔NAPI async work 线程〕；Cancel 即时返回、
/// 在途轮以 cancelled 收场；Status 250ms 轮询面）。
pub struct SpeedHost {
    running: AtomicBool,
    cancel: std::sync::Arc<AtomicBool>,
    /// 引擎进度出口（M-7：live/dir 的数据源——相位 + 字节累计，instBps 差分算）。
    live: std::sync::Arc<crate::speedtest::LiveProgress>,
    state: Mutex<RoundState>,
}

impl Default for SpeedHost {
    fn default() -> Self {
        Self::new()
    }
}

impl SpeedHost {
    pub fn new() -> Self {
        SpeedHost {
            running: AtomicBool::new(false),
            cancel: std::sync::Arc::new(AtomicBool::new(false)),
            live: std::sync::Arc::new(crate::speedtest::LiveProgress::new()),
            state: Mutex::new(RoundState::default()),
        }
    }

    /// ClientCoreSpeedTestStart（同步完整轮；busy 门 = 并发轮拒绝）。
    pub fn start(&self, params_json: &str) -> String {
        if self.running.swap(true, Ordering::AcqRel) {
            return speed_fail("busy", "已有测速在跑（请等它完成或取消）");
        }
        self.cancel.store(false, Ordering::Release);
        {
            let mut st = lock_round(&self.state);
            *st = RoundState {
                phase: "connecting",
                started_at: Some(Instant::now()),
                ..RoundState::default()
            };
        }
        // 参数解析（引擎外的前置门——错参数不占轮）
        let p: SpeedParams = match serde_json::from_str(params_json) {
            Ok(p) => p,
            Err(e) => {
                self.running.store(false, Ordering::Release);
                return speed_fail("invalid_arg", format!("参数不是合法 JSON：{e}"));
            }
        };
        let raw = crate::speedtest::Params {
            down: Duration::from_millis(p.down_ms.max(0) as u64),
            up: Duration::from_millis(p.up_ms.max(0) as u64),
            warmup: Duration::from_millis(p.warmup_ms.max(0) as u64),
            streams: p.streams.max(0) as usize,
        };
        let params = match raw.normalized() {
            Ok(v) => v,
            Err(e) => {
                self.running.store(false, Ordering::Release);
                return speed_fail("invalid_arg", e);
            }
        };
        // R8-8b 临时排障面（收口前回退）：引擎轮内日志落文件（sock 同目录——上行
        // bulk 真机排障唯一证据源，App 形态无 stdout）。
        let logf = |s: &str| {
            // 轮内日志经 stderr？App 形态无 stdout 消费面——丢弃（判据行在 exit 侧）
            let _ = s;
        };
        let cancel = std::sync::Arc::clone(&self.cancel);
        let live = std::sync::Arc::clone(&self.live);
        let outcome = app_run(&p.auth, &p.sock, &params, &logf, Some(&cancel), Some(&live));
        let out = speed_start(params_json, |_| outcome);
        // 收尾快照（done/cancelled/failed + usage）
        {
            let mut st = lock_round(&self.state);
            let ok = serde_json::from_str::<serde_json::Value>(&out)
                .ok()
                .and_then(|v| v.get("ok").and_then(|o| o.as_bool()))
                .unwrap_or(false);
            if ok {
                st.phase = "done";
                let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
                st.usage = Some((
                    v.get("usageDown").and_then(|x| x.as_i64()).unwrap_or(0),
                    v.get("usageUp").and_then(|x| x.as_i64()).unwrap_or(0),
                ));
                st.reason = String::new();
            } else {
                let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
                st.phase = "failed";
                st.reason = v
                    .get("reason")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_owned();
                if st.reason == "cancelled" {
                    st.phase = "cancelled";
                }
            }
            st.live = None;
            st.sample = None;
            st.started_at = None;
        }
        self.running.store(false, Ordering::Release);
        out
    }

    /// ClientCoreSpeedTestStatus（250ms 轮询面；instBps = 进度字节对上次采样的差分）。
    pub fn status(&self) -> String {
        let mut st = lock_round(&self.state);
        let in_flight = st.phase == "connecting" || st.phase == "down" || st.phase == "up";
        if in_flight {
            let (dir, bytes) = self.live.snapshot();
            if let Some(dir) = dir {
                let now = Instant::now();
                let inst = match st.sample {
                    Some((prev_b, prev_t)) => {
                        let dt = now.duration_since(prev_t).as_secs_f64();
                        if dt > 0.0 {
                            ((bytes - prev_b) as f64 / dt).max(0.0)
                        } else {
                            0.0
                        }
                    }
                    None => 0.0,
                };
                st.live = Some((dir, bytes, inst));
                st.sample = Some((bytes, now));
                // 相位跟随引擎出口（down → up——M-7 的核心修复：此前恒 "down"）
                st.phase = dir;
            } else {
                st.live = None;
            }
        }
        let elapsed_ms = match st.started_at {
            Some(t) => t.elapsed().as_millis() as i64,
            None => -1,
        };
        let snap = SpeedSnapshotIn {
            phase: st.phase,
            reason: st.reason.clone(),
            usage: st.usage,
            live: st.live,
            elapsed_ms,
        };
        speed_status_json(&snap)
    }

    /// ClientCoreSpeedTestCancel（即时返回；在途轮由取消位收场——「已产生用量后
    /// 打断」归因 cancelled（M-8：类型化直达 reason 面）；本层恒 ok:true）。
    pub fn cancel(&self) -> String {
        self.cancel.store(true, Ordering::Release);
        speed_cancel_json()
    }
}

fn lock_round(m: &Mutex<RoundState>) -> std::sync::MutexGuard<'_, RoundState> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod host_tests {
    use super::*;

    /// busy 门：并发第二轮直接拒绝（不占轮——running 位原子交换）。
    #[test]
    fn busy_gate_and_idle_status() {
        let h = SpeedHost::new();
        let v: serde_json::Value = serde_json::from_str(&h.status()).unwrap();
        assert_eq!(v["phase"], "idle");
        assert_eq!(v.as_object().unwrap().len(), 2); // phase + reason（elapsedMs -1 不出现）
                                                     // 参数错不占轮（invalid_arg 即时回）
        let out = h.start("{oops");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["reason"], "invalid_arg");
        // running=false：取消恒 ok
        let v: serde_json::Value = serde_json::from_str(&h.cancel()).unwrap();
        assert_eq!(v["ok"], true);
    }
}
