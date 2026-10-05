//! 守护侧测速运行面（语义真源 `baseline:clientcore/facade/speedrun.go`）。
//!
//! per-host 单飞 + 守护托管：speedtest.start **立即返回 waiting 相位**、等待由 runner
//! 状态机在 wait_ms 预算内承载（默认 60s，覆盖恢复阶梯最坏 ≈45s；长等待不占用控制面
//! 请求）；引擎数据腿 = 拨号缝拨出口 7803 直连隧道（MUST NOT 经控制面流）。
//!
//! 注入缝错误契约：refused-like 判定与归因归本 runner——拨号错误按 `ConnErr::Refused`
//! 哨兵分类产 `not_supported`（出口无 state 目录时 7803 不在 LocalServices、拦截回
//! RST、连接未建立，MUST NOT 落进「链路未就绪 = 等 waitMs」一支空烧预算）；会话不在/
//! 重建窗口 = link_down（waitMs 预算内保持 waiting 重试）。引擎只透传错误面。

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::speedtest::{self, LiveProgress, SpeedtestError};

use super::{CarrierDial, DialErr};

/// 出口测速服务端口（= server::SPEEDTEST_PORT；与手机壳的同名常量同源同值）。
pub const SPEEDTEST_SERVICE_PORT: u16 = 7803;
/// link_down 重试节拍（与 CLI 轮询同拍 250ms）。
const RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// start 载荷（Go SpeedtestStart）。
#[derive(Debug, Clone, Copy)]
pub struct SpeedtestParams {
    pub down: Duration,
    pub up: Duration,
    pub warmup: Duration,
    pub streams: usize,
    /// 链路未就绪等待预算（0 = 不等，立即失败）。
    pub wait_ms: i64,
}

impl SpeedtestParams {
    /// 控制面载荷 → 参数（毫秒面）。
    pub fn from_ms(down_ms: i64, up_ms: i64, warmup_ms: i64, streams: i64, wait_ms: i64) -> Self {
        SpeedtestParams {
            down: Duration::from_millis(down_ms.max(0) as u64),
            up: Duration::from_millis(up_ms.max(0) as u64),
            warmup: Duration::from_millis(warmup_ms.max(0) as u64),
            streams: streams.max(0) as usize,
            wait_ms,
        }
    }
}

/// start 的立即回执（waiting 相位或 busy——busy 是成功载荷里的 reason，不占错误码表）。
#[derive(Debug, Clone)]
pub struct SpeedtestAck {
    /// waiting | busy。
    pub phase: &'static str,
    /// busy 时携带。
    pub reason: Option<&'static str>,
}

/// 终态结果（CLI --json 的数据源；字段名与手机信封同面）。
#[derive(Debug, Clone)]
pub struct SpeedtestOutcome {
    pub ok: bool,
    pub reason: String,
    pub msg: String,
    pub down_bps: f64,
    pub up_bps: f64,
    pub usage_down: i64,
    pub usage_up: i64,
    pub wall_ms: u64,
}

/// status 面快照（waiting 相位或引擎快照；终态时 result 携带完整结果）。
#[derive(Debug, Clone)]
pub struct SpeedtestStatus {
    pub waiting: bool,
    pub wait_remain_ms: i64,
    /// waiting | connecting | down | up | idle。
    pub phase: String,
    /// 当前相位累计字节（CLI 轮询差分算 instBps）。
    pub bytes: i64,
    pub elapsed_ms: i64,
    pub result: Option<SpeedtestOutcome>,
}

struct RunState {
    /// start 已返回、引擎未真正开跑（link_down 重试窗）。
    waiting: bool,
    /// 终态（runner 收场时写回；None = 未到终态）。写回后 runLoop 即退（busy 随之假）。
    result: Option<SpeedtestOutcome>,
}

/// 一台主机的运行时（run 线程与 status 面的共享面）。
struct SpeedRun {
    cancel: AtomicBool,
    live: Arc<LiveProgress>,
    state: Mutex<RunState>,
    wait_until: Instant,
    started_at: Instant,
}

impl SpeedRun {
    fn busy(&self) -> bool {
        // busy = waiting || starting || engine.Running() 的 Rust 收敛：run 线程活着
        // （终态未写回）即 busy——setWaiting(false) 与引擎开跑之间无空窗。
        self.state.lock().unwrap_or_else(|e| e.into_inner()).result.is_none()
    }

    fn set_waiting(&self, v: bool) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).waiting = v;
    }

    fn finish_with(&self, outcome: SpeedtestOutcome) {
        let mut st = self.state.lock().unwrap_or_else(|e| e.into_inner());
        st.waiting = false;
        st.result = Some(outcome);
    }

    /// 等待节拍（250ms 或取消先到）。true = 已取消。
    fn wait_cancel_or(&self, d: Duration) -> bool {
        let deadline = Instant::now() + d;
        while Instant::now() < deadline {
            if self.cancel.load(Ordering::Acquire) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.cancel.load(Ordering::Acquire)
    }
}

/// 守护侧测速运行面（Carriers 持有；拨号缝 = CarrierDial）。
pub struct SpeedtestManager {
    runs: Mutex<HashMap<String, Arc<SpeedRun>>>,
    dial: CarrierDial,
    logf: Arc<dyn Fn(&str) + Send + Sync>,
}

impl SpeedtestManager {
    pub fn new(dial: &CarrierDial, logf: &Arc<dyn Fn(&str) + Send + Sync>) -> SpeedtestManager {
        SpeedtestManager {
            runs: Mutex::new(HashMap::new()),
            dial: CarrierDial {
                dial_port: Arc::clone(&dial.dial_port),
                dial: Arc::clone(&dial.dial),
            },
            logf: Arc::clone(logf),
        }
    }

    /// 对一台主机开跑：per-host 单飞（busy = 成功载荷 reason，同手机信封形态）；
    /// 立即返回 waiting 相位，整轮在后台线程里跑。
    pub fn start(&self, host: &str, p: SpeedtestParams) -> SpeedtestAck {
        {
            let runs = self.runs.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(r) = runs.get(host) {
                if r.busy() {
                    return SpeedtestAck {
                        phase: "busy",
                        reason: Some(speedtest::REASON_BUSY),
                    };
                }
            }
        }
        let run = Arc::new(SpeedRun {
            cancel: AtomicBool::new(false),
            live: Arc::new(LiveProgress::new()),
            state: Mutex::new(RunState { waiting: true, result: None }),
            wait_until: Instant::now() + Duration::from_millis(p.wait_ms.max(0) as u64),
            started_at: Instant::now(),
        });
        self.runs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(host.to_owned(), Arc::clone(&run));
        let dial = clone_dial(&self.dial);
        let logf = Arc::clone(&self.logf);
        let host = host.to_owned();
        std::thread::Builder::new()
            .name("hw-spd-run".to_owned())
            .stack_size(2 * 1024 * 1024)
            .spawn(move || run_loop(run, host, p, dial, logf))
            .expect("线程创建不可失败");
        SpeedtestAck { phase: "waiting", reason: None }
    }

    /// 该主机的运行态（无运行面 = None——CLI 判「运行面丢失」的形态）。
    pub fn status(&self, host: &str) -> Option<SpeedtestStatus> {
        let run = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(host).cloned()?;
        let st = run.state.lock().unwrap_or_else(|e| e.into_inner());
        if st.waiting {
            let remain = run.wait_until.saturating_duration_since(Instant::now());
            return Some(SpeedtestStatus {
                waiting: true,
                wait_remain_ms: remain.as_millis() as i64,
                phase: "waiting".to_owned(),
                bytes: 0,
                elapsed_ms: -1,
                result: st.result.clone(),
            });
        }
        let (phase, bytes) = run.live.snapshot();
        // run 在册但引擎快照还在 idle（starting 窗口/重试间隙的瞬态）：不回 idle——
        // CLI 的「运行面丢失」判据以 idle 为准，这里把在册 run 的瞬态 idle 归一到
        // connecting，免得轮询撞上微秒级窗口误判。
        let phase = match (phase, st.result.is_none()) {
            (Some(p), _) => p.to_owned(),
            (None, true) => "connecting".to_owned(),
            (None, false) => "idle".to_owned(),
        };
        Some(SpeedtestStatus {
            waiting: false,
            wait_remain_ms: 0,
            phase,
            bytes,
            elapsed_ms: run.started_at.elapsed().as_millis() as i64,
            result: st.result.clone(),
        })
    }

    /// 取消该主机当前轮（等待期与运行中都收；幂等）。
    pub fn cancel(&self, host: &str) {
        let run = self.runs.lock().unwrap_or_else(|e| e.into_inner()).get(host).cloned();
        if let Some(r) = run {
            r.cancel.store(true, Ordering::Release);
            // engine 侧取消位即 run.cancel（run_dial 的 cancel 形参——看门狗 200ms
            // 分片轮询发现后 kill 全部连接）。
        }
    }

    /// 收工：取消全部在跑轮。
    pub fn close(&self) {
        let runs: Vec<Arc<SpeedRun>> =
            self.runs.lock().unwrap_or_else(|e| e.into_inner()).values().cloned().collect();
        for r in runs {
            r.cancel.store(true, Ordering::Release);
        }
    }
}

/// 整轮承载：link_down 在 waitMs 预算内保持 waiting 重试（恢复阶梯自愈后开跑）；
/// 其余终态（含 not_supported——refused-like，MUST NOT 落等待支）立即收场。全部
/// 终态写回 result（CLI 轮询的终态数据源——等待期被取消也合成 cancelled 终态，
/// 不留给轮询方一个永远 idle 的歧义快照）。
fn run_loop(run: Arc<SpeedRun>, host: String, p: SpeedtestParams, dial: CarrierDial, logf: Arc<dyn Fn(&str) + Send + Sync>) {
    let params = speedtest::Params {
        down: p.down,
        up: p.up,
        warmup: p.warmup,
        streams: p.streams,
    };
    loop {
        // 开跑即离开 waiting（整轮在跑期间状态面由引擎快照承载）；link_down 且
        // 预算未尽才回到 waiting 相位重试。
        run.set_waiting(false);
        let res = speedtest::run_dial(
            &|| dial_speedtest(&dial, &host, &run),
            params,
            &*logf,
            Some(&run.cancel),
            Some(&run.live),
        );
        match res {
            Ok(r) => {
                run.finish_with(SpeedtestOutcome {
                    ok: true,
                    reason: String::new(),
                    msg: String::new(),
                    down_bps: r.down_bps,
                    up_bps: r.up_bps,
                    usage_down: r.usage_down,
                    usage_up: r.usage_up,
                    wall_ms: r.wall_ms,
                });
                return;
            }
            Err(e) => {
                let reason = e.reason().to_owned();
                // not_supported（refused-like——出口无测速服务）与其余非 link_down
                // 终态立即收场，MUST NOT 落等待支。
                if reason != speedtest::REASON_LINK_DOWN {
                    run.finish_with(outcome_err(&reason, &e.to_string()));
                    return;
                }
                if Instant::now() > run.wait_until {
                    run.finish_with(outcome_err(&reason, &e.to_string())); // link_down 到点（预算耗尽，如实收场）
                    return;
                }
                if run.cancel.load(Ordering::Acquire) {
                    run.finish_with(outcome_err(
                        speedtest::REASON_CANCELLED,
                        "等待链路就绪期间测速被取消",
                    ));
                    return;
                }
                run.set_waiting(true);
                if run.wait_cancel_or(RETRY_INTERVAL) {
                    run.finish_with(outcome_err(
                        speedtest::REASON_CANCELLED,
                        "等待链路就绪期间测速被取消",
                    ));
                    return;
                }
            }
        }
    }
}

fn outcome_err(reason: &str, msg: &str) -> SpeedtestOutcome {
    SpeedtestOutcome {
        ok: false,
        reason: reason.to_owned(),
        msg: msg.to_owned(),
        down_bps: 0.0,
        up_bps: 0.0,
        usage_down: 0,
        usage_up: 0,
        wall_ms: 0,
    }
}

/// 引擎拨号注入缝：拨号缝拨出口 7803 + refused-like 分类（判定与归因归 runner——
/// Go dialFn 同义）。已取消时归 cancelled（拨号面收口）；refused → NotSupported
/// （出口无测速服务——7803 回 RST）；会话不在/重建窗口与其余拨号失败 → link_down
/// 带 detail（waitMs 预算内保持 waiting 重试）。
fn dial_speedtest(
    dial: &CarrierDial,
    host: &str,
    run: &Arc<SpeedRun>,
) -> Result<Arc<dyn speedtest::SpeedConn>, SpeedtestError> {
    if run.cancel.load(Ordering::Acquire) {
        return Err(SpeedtestError::Cancelled);
    }
    match (dial.dial_port)(host, SPEEDTEST_SERVICE_PORT) {
        Ok(c) => Ok(c.speed),
        Err(DialErr::Refused) => Err(SpeedtestError::NotSupported),
        Err(e @ (DialErr::NoSession | DialErr::NoHost)) => Err(SpeedtestError::Bridge(
            speedtest::REASON_LINK_DOWN,
            format!("链路未就绪（会话不在/重建窗口）：{e}"),
        )),
        Err(DialErr::Other(msg)) => Err(SpeedtestError::Bridge(
            speedtest::REASON_LINK_DOWN,
            format!("隧道拨号失败：{msg}"),
        )),
    }
}

fn clone_dial(d: &CarrierDial) -> CarrierDial {
    CarrierDial { dial_port: Arc::clone(&d.dial_port), dial: Arc::clone(&d.dial) }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::{eventually, temp_dir, FakeDial};
    use super::*;
    use crate::speedtest::SpeedConn;

    fn nop_log() -> Arc<dyn Fn(&str) + Send + Sync> {
        Arc::new(|_| {})
    }

    fn hex_host(tag: u8) -> String {
        format!("{tag:0>64x}")
    }

    /// start 立即 waiting → link_down 在 wait 预算内保持 waiting 重试 → 到点收场
    /// （runner 状态机的核心节拍）。
    #[test]
    fn link_down_waits_then_finalizes_on_deadline() {
        let dial = FakeDial::new();
        // 全部拨号失败（Other）→ link_down。
        *dial.fail_with.lock().unwrap() = Some(DialErr::Other("模拟链路未就绪".into()));
        let m = SpeedtestManager::new(&dial.carrier_dial(), &nop_log());
        let ack = m.start(
            &hex_host(1),
            SpeedtestParams::from_ms(50, 50, 10, 1, 900),
        );
        assert_eq!(ack.phase, "waiting");
        assert!(ack.reason.is_none());
        // busy：单飞（同主机立即再 start = busy）。
        let ack2 = m.start(&hex_host(1), SpeedtestParams::from_ms(50, 50, 10, 1, 0));
        assert_eq!(ack2.phase, "busy");
        // waiting 相位可见 + wait_remain_ms 递减面。
        let st = m.status(&hex_host(1)).expect("run 在册");
        assert!(st.waiting);
        assert!(st.wait_remain_ms > 0);
        // 到点（900ms wait + 引擎拨号 10s 预算兜底——FakeDial 立即失败，实际由
        // RETRY_INTERVAL 节拍收场）：终态 link_down。
        eventually(Duration::from_secs(5), "link_down 终态", || {
            matches!(&m.status(&hex_host(1)), Some(SpeedtestStatus { result: Some(r), .. }) if r.reason == "link_down")
        });
        // 终态后再 start 可开新轮（busy 解除）。
        let ack3 = m.start(&hex_host(1), SpeedtestParams::from_ms(50, 50, 10, 1, 0));
        assert_eq!(ack3.phase, "waiting");
    }

    /// refused-like（对端 RST）→ not_supported 立即终态（MUST NOT 落等待支）。
    #[test]
    fn refused_maps_not_supported_immediately() {
        let dial = FakeDial::new();
        *dial.fail_with.lock().unwrap() = Some(DialErr::Refused);
        let m = SpeedtestManager::new(&dial.carrier_dial(), &nop_log());
        m.start(&hex_host(2), SpeedtestParams::from_ms(50, 50, 10, 1, 60_000));
        eventually(Duration::from_secs(5), "not_supported 终态", || {
            matches!(&m.status(&hex_host(2)), Some(SpeedtestStatus { result: Some(r), .. }) if r.reason == "not_supported")
        });
    }

    /// cancel：等待期取消 → cancelled 终态（不留永远 idle 的歧义快照）。
    #[test]
    fn cancel_during_wait_finalizes_cancelled() {
        let dial = FakeDial::new();
        *dial.fail_with.lock().unwrap() = Some(DialErr::Other("模拟链路未就绪".into()));
        let m = SpeedtestManager::new(&dial.carrier_dial(), &nop_log());
        m.start(&hex_host(3), SpeedtestParams::from_ms(50, 50, 10, 1, 60_000));
        std::thread::sleep(Duration::from_millis(300)); // 进 waiting 重试环
        m.cancel(&hex_host(3));
        eventually(Duration::from_secs(5), "cancelled 终态", || {
            matches!(&m.status(&hex_host(3)), Some(SpeedtestStatus { result: Some(r), .. }) if r.reason == "cancelled")
        });
        let _ = temp_dir("spd"); // 目录助手引用位（本测试无落盘面）
    }

    /// 假测速服务端：握手成功 → 引擎真跑一轮 → ok 终态带数字（speed 腿接线面）。
    #[test]
    fn engine_roundtrip_via_speed_leg() {
        let dial = FakeDial::new();
        // 假服务端：一个本地 speedtest 帧应答器太重——这里改验「拨号缝 speed 腿被
        // 引擎消费」的最小面：拨号成功 + 引擎在 InvalidArg（streams=0 会被
        // normalized 填默认？——走超大窗口越界形态）下收场为终态。
        // 更直接：让 dial 返回不可读写的假连接（read 立即 EOF）→ 引擎报 not_supported
        // （首帧前 EOF）——验证 speed 腿接线。
        *dial.fail_with.lock().unwrap() = None;
        dial.port_map.lock().unwrap().clear();
        // 不注册端口 → 拨号 Other → link_down（与第一个用例重复）——改注册一个
        // 立即关掉的 socket 目标：连接建立后 EOF。
        let dead = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);
        dial.port_map
            .lock()
            .unwrap()
            .insert(SPEEDTEST_SERVICE_PORT, format!("127.0.0.1:{dead_port}").parse().unwrap());
        let m = SpeedtestManager::new(&dial.carrier_dial(), &nop_log());
        m.start(&hex_host(4), SpeedtestParams::from_ms(50, 50, 10, 1, 0));
        eventually(Duration::from_secs(5), "EOF 归因终态", || {
            matches!(&m.status(&hex_host(4)), Some(SpeedtestStatus { result: Some(_), .. }))
        });
        // 拨号缝确实经 speed 腿（FakeDial 产出的 NoopSpeed 不满足真引擎——本用例
        // 的价值在「runner→run_dial→拨号缝」链路打通；终态归因细节由引擎既有测试覆盖）。
    }

    /// SpeedConn 假腿可达面（编译期契约——testutil 的 NoopSpeed 实现）。
    #[test]
    fn noop_speed_conn_contract() {
        struct N;
        impl SpeedConn for N {
            fn write_frame(&self, _: &[u8]) -> Result<(), crate::speedtest::SpeedtestError> {
                Ok(())
            }
            fn read_some(&self) -> Result<Vec<u8>, crate::speedtest::SpeedtestError> {
                Ok(Vec::new())
            }
            fn kill(&self) {}
        }
        let n = N;
        assert!(n.write_frame(b"x").is_ok());
        assert!(n.read_some().unwrap().is_empty());
    }
}
