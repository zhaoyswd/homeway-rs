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
        return speed_fail("bridge_down", "桥未就绪（缺 auth/sock：VPN 未连接且服务会话未就绪）");
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
    serde_json::Number::from_f64(v).map(Value::Number).unwrap_or(Value::from(0))
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
}

impl crate::speedtest::SpeedConn for BridgeSpeedConn {
    fn write_frame(&self, data: &[u8]) -> Result<(), crate::speedtest::SpeedtestError> {
        use crate::speedtest::SpeedtestError;
        let mut w = self.w.lock().unwrap_or_else(|e| e.into_inner());
        let mut off = 0;
        while off < data.len() {
            match w.write(&data[off..]) {
                Ok(0) => return Err(SpeedtestError::Frame("测速桥写通道返回 0（对端已关）".into())),
                Ok(n) => off += n,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
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
        match r.read(&mut buf) {
            Ok(0) => Ok(Vec::new()), // EOF
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
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
/// async work 线程上跑；失败信封同 `SpeedOutcome::Fail`）。
pub fn app_run(auth: &str, sock: &str, p: &crate::speedtest::Params, logf: &dyn Fn(&str)) -> SpeedOutcome {
    let dial = || -> Result<std::sync::Arc<dyn crate::speedtest::SpeedConn>, crate::speedtest::SpeedtestError> {
        match speed_dial_conn(auth, sock, Duration::from_secs(10)) {
            Ok(c) => Ok(std::sync::Arc::new(c)),
            Err((reason, msg)) => {
                // 拨号/鉴权失败 = 注入缝错误（bridge_down/bridge_auth——引擎透传面）
                Err(crate::speedtest::SpeedtestError::Bridge(reason, msg))
            }
        }
    };
    match crate::speedtest::run_dial(&dial, *p, logf) {
        Ok(res) => SpeedOutcome::Ok(res),
        Err(e) => SpeedOutcome::Fail { reason: e.reason().to_string(), msg: e.to_string() },
    }
}

/// 拨测速桥 + 鉴权 + 拆半（SpeedConn 承载构造）。
pub fn speed_dial_conn(
    auth_hex: &str,
    sock: &str,
    budget: Duration,
) -> Result<BridgeSpeedConn, (&'static str, String)> {
    let mut conn = super::bridge_host::connect_budget(std::path::Path::new(sock), budget)
        .map_err(|e| ("bridge_down", format!("测速通道暂时不可用（桥未就绪或正在恢复）：{e}")))?;
    // 长任务连接不设逐操作超时（窗口期读阻塞是常态；中断由引擎看门狗 kill 承担）
    conn.set_read_timeout(None).ok();
    conn.set_write_timeout(None).ok();
    write_auth(&mut conn, auth_hex).map_err(|e| ("bridge_auth", format!("测速通道鉴权失败：{e}")))?;
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
    })
}

/// 测速桥拨号 + 鉴权（引擎的 Dial 缝；ctx 语义由 UnixStream deadline 承载——
/// 拨号预算/取消要能打断在途拨号，FIX-39）。
pub fn speed_dial(auth_hex: &str, sock: &str, budget: Duration) -> Result<UnixStream, (&'static str, String)> {
    // 工单④：UDS 拨号加 connect 预算（拨号预算/取消要能打断在途拨号——FIX-39 面）
    let conn = super::bridge_host::connect_budget(
        std::path::Path::new(sock),
        budget.max(Duration::from_millis(500)),
    )
        .map_err(|e| ("bridge_down", format!("测速通道暂时不可用（桥未就绪或正在恢复）：{e}")))?;
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
    c.write_all(request).map_err(|e| ("bridge_down", format!("发测速请求失败：{e}")))?;
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
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
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
            serde_json::from_str(&speed_start(r#"{"auth":"","sock":""}"#, |_| unreachable!())).unwrap();
        assert_eq!(v["reason"], "bridge_down");
        // streams 非法（Params.normalized 拒）→ invalid_arg
        let v: Value = serde_json::from_str(&speed_start(
            r#"{"auth":"a","sock":"s","streams":99}"#,
            |p| match p.normalized() {
                Err(e) => SpeedOutcome::Fail { reason: "invalid_arg".into(), msg: e },
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
        let out = speed_start(r#"{"auth":"a","sock":"s"}"#, |_| SpeedOutcome::Ok(SpeedtestResult {
            down_bps: 123.5,
            up_bps: 456.0,
            usage_down: 1000,
            usage_up: 2000,
            wall_ms: 25000,
        }));
        let v: Value = serde_json::from_str(&out).unwrap();
        let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
        assert_eq!(keys, vec!["downBps", "ok", "phase", "upBps", "usageDown", "usageUp", "wallMs"]);
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
        assert_eq!(keys, vec!["bytes", "dir", "elapsedMs", "instBps", "phase", "reason", "usageDown", "usageUp"]);
        assert_eq!(v["dir"], "down");
    }

    #[test]
    fn cancel_envelope() {
        assert_eq!(speed_cancel_json(), r#"{"ok":true}"#);
    }
}
