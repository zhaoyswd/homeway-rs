//! 服务会话真装配（语义真源 `baseline:clientcore/cmd/clientcore/app_service.go`
//! 的 Service 生命周期 + 桥挂接——openspec app-service-session）。
//!
//! rc 门与状态机在 [`super::service_op::ServiceDomain`]（纯逻辑）；本模块做接线：
//! - `start`：解析 cfg → 打开日志（-2）→ spawn 会话线程（`crate::session::Session`
//!   ——服务域形态：无 TUN、暖机 12s、60s 巡检、恢复阶梯、整会话重建）→ 桥三座
//!   （dial 经会话流——SessionConn）→ 状态机推进 starting→ready/failed；
//! - `stop`：桥停 → 会话停（≤6s）→ 端点缓存落盘由 Session 收尾 → idle；
//! - `status`：`crate::status_json::snapshot_json` + bridge 四键（`bridgeAuth`…
//!   ——Go serviceSnapshotJSON 的桥段）。
//!
//! App 进程内单例：与隧道会话**不得并发**（共用设备 WG 身份）——App 侧在连 VPN 前
//! 必调 stop 完整收工；核内 rc 门（stopping 期 start 恒 -1）是第二道防线。

use std::io;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::session::{Session, SessionConfig};
use crate::token;
use crate::Logf;

use super::bridge_host::{BridgeHost, BridgeStream};
use super::service_op::{ServiceDomain, ServiceState};
use super::tun_exec::SessionStream;

/// serviceConfig（app_service.go 同形；camelCase——App 实发键面）。
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceConfigJson {
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub identity_dir: String,
    #[serde(default)]
    pub endpoint_cache_dir: String,
    #[serde(default)]
    pub out: String,
}

/// 服务会话运行态（start 受理后持有；stop 完成后清槽）。
struct ServiceRun {
    /// Arc 形态（M-10②：桥的拨号闭包从锁里**克隆出来**再拨——此前持锁 15s 拨号，
    /// status() 同锁 ⇒ App 轮询面被同步阻塞）。
    session: Mutex<Option<Arc<Session>>>,
    bridge: Arc<BridgeHost>,
    /// 会话线程的状态推进面（快照读 + state 写回 ServiceDomain）。
    domain: Arc<ServiceDomain>,
    /// 受理时刻（starting 形态的 elapsedMs 源——L-11：Go serviceSnapshotJSON 的
    /// elapsedMs 随 Since 恒在，此前 starting 形态缺）。
    since: std::time::Instant,
}

/// 服务会话宿主（App 核单例）。
pub struct ServiceExec {
    /// Arc 包一层：会话线程失败时要能清槽（L-11②：failed 后 run 槽未清 ⇒ start 恒 -1）。
    run: Arc<Mutex<Option<Arc<ServiceRun>>>>,
}

impl Default for ServiceExec {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceExec {
    pub fn new() -> Self {
        ServiceExec { run: Arc::new(Mutex::new(None)) }
    }

    /// ClientCoreServiceStart：rc 契约见 service_op 模块头（0/−1/−2/−3/−4）。
    /// `domain` = ClientCore 的 rc 门载体（状态机推进共享）。
    pub fn start(&self, cfg_json: &str, domain: &Arc<ServiceDomain>) -> i32 {
        let cfg: ServiceConfigJson = match serde_json::from_str(cfg_json) {
            Ok(c) => c,
            Err(_) => return -3,
        };
        if cfg.token.is_empty() {
            return -4;
        }
        // 单飞：上一实例收工中 -1（rc 门先行——domain 的状态是唯一事实源）
        {
            let st = domain.state();
            match st {
                ServiceState::Starting | ServiceState::Ready => return 0, // 幂等
                ServiceState::Stopping => return -1,
                ServiceState::Idle | ServiceState::Failed => {}
            }
        }
        let mut guard = self.run.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_some() {
            // 域门说 Idle/Failed 但运行态还在 = 上次 stop 半途（等收工）——按 -1
            return -1;
        }        // token 解析（同步硬失败：参数面 -3 同源——Go Start 的 json.Unmarshal 后
        // token 装配失败即返回）
        let tk = match token::decode(&cfg.token) {
            Ok(t) => t,
            Err(e) => {
                domain.set_state(ServiceState::Failed);
                domain.set_reason(&format!("token 解析失败：{e}"));
                return -3;
            }
        };
        // 服务日志（-2：打不开即拒——与隧道域日志同一纪律）
        let logf = match open_service_log(&cfg.out) {
            Ok(l) => l,
            Err(_) => return -2,
        };
        domain.set_state(ServiceState::Starting);
        domain.set_reason("");

        let identity_dir = (!cfg.identity_dir.is_empty()).then(|| PathBuf::from(&cfg.identity_dir));
        let endpoint_cache_dir =
            (!cfg.endpoint_cache_dir.is_empty()).then(|| PathBuf::from(&cfg.endpoint_cache_dir));
        let dir_for_bridge = identity_dir.clone();

        // 桥先行构造（dial 经会话——会话起来前 dial 失败属预期：桥泵如实报）
        let logf2: Logf = Arc::clone(&logf);
        let bridge = Arc::new(BridgeHost::new(
            "服务桥",
            dir_for_bridge,
            logf2,
            Box::new(|_port, _budget| {
                // 拨号闭包经运行态取会话（会话在下方线程里建；建好前拨号失败）
                Err(io::Error::new(io::ErrorKind::NotConnected, "服务会话未就绪"))
            }),
        ));
        let run = Arc::new(ServiceRun {
            session: Mutex::new(None),
            bridge: Arc::clone(&bridge),
            domain: Arc::clone(domain),
            since: std::time::Instant::now(),
        });

        // 会话线程（Session::start 阻塞暖机 ~12s——不占调用线程；失败时清 run 槽
        // 〔L-11②：failed 后槽未清 ⇒ 下次 start 恒 -1〕+ 停桥）
        let run2 = Arc::clone(&run);
        let run_slot = Arc::clone(&self.run);
        let spawn = std::thread::Builder::new().name("homeway-svc".into()).spawn(move || {
            let sess = Session::start(SessionConfig {
                token: tk,
                identity_dir,
                endpoint_cache_dir,
                logf: Arc::clone(&logf),
                relay_only: false,
            });
            match sess {
                Ok(s) => {
                    // 桥的 dial 闭包拿不到会话句柄（构造在先）——桥 dial 经「运行态取
                    // 会话」由泵侧闭包完成：见下方 dial_via_run（桥重建成本高，改为
                    // 桥持有运行态：这里通过 set_dial 注入）。
                    *run2.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(s));
                    run2.domain.set_state(ServiceState::Ready);
                    run2.domain.set_reason("");
                }
                Err(e) => {
                    run2.domain.set_state(ServiceState::Failed);
                    run2.domain.set_reason(&e.to_string());
                    // 失败清槽（L-11②）+ 桥收工（受理侧起的桥随失败收掉）
                    run2.bridge.stop();
                    *run_slot.lock().unwrap_or_else(|e| e.into_inner()) = None;
                }
            }
        });
        if spawn.is_err() {
            domain.set_state(ServiceState::Failed);
            domain.set_reason("会话线程启动失败");
            return -1;
        }
        *guard = Some(Arc::clone(&run));
        drop(guard);
        // 桥在会话受理后启动（dial 经运行态会话——见 dial_via_run）
        let run3 = Arc::clone(&run);
        bridge.set_dial(Box::new(move |port, budget| dial_via_run(&run3, port, budget)));
        bridge.start();
        0
    }

    /// ClientCoreServiceStop：0 已收工 / -1 等待超时（调用方重试；期间 start 恒 -1）。
    /// 顺序 = 桥 → 会话 → （缓存落盘由 Session::stop 内完成）。
    /// **超时不提前清槽**（评审 r2-M-10①：旧实现超时前已 take ⇒ 第二次 stop 见空槽
    /// 直接置 Idle 返回 0 ⇒ start 放行 ⇒ 同钥双会话窗口；现 run 槽留到真正收完）。
    pub fn stop(&self, domain: &ServiceDomain) -> i32 {
        let mut guard = self.run.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = guard.clone() else {
            drop(guard);
            domain.set_state(ServiceState::Idle);
            return 0; // 本就没在跑
        };
        domain.set_state(ServiceState::Stopping);
        run.bridge.stop();
        // 会话句柄取走（重试 stop 时已取空 ⇒ 跳过；桥停幂等）
        let sess = run.session.lock().unwrap_or_else(|e| e.into_inner()).take();
        // 有界等待（Go serviceStopWait 的 6s；Session::stop 自身也 join 巡检）
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        // Session::stop 是同步收尾（join 巡检 ≤6s）——放线程跑并等
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let d2 = Arc::clone(&done);
        let h = std::thread::spawn(move || {
            if let Some(s) = sess.as_ref() {
                s.stop();
            }
            d2.store(true, std::sync::atomic::Ordering::Release);
        });
        while !done.load(std::sync::atomic::Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                // 收工超时：run 槽保留 + 域状态停在 stopping（start 恒 -1 防双持钥）；
                // 线程随后自行收尾——下一次 stop 重入走「会话已取空」路径等它结束
                return -1;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = h.join();
        *guard = None;
        drop(guard);
        domain.set_state(ServiceState::Idle);
        domain.set_reason("");
        0
    }

    /// ClientCoreServiceStatus：无实例 = idle 短路；有实例 = snapshot + bridge 四键。
    pub fn status(&self) -> String {
        let guard = self.run.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = guard.as_ref() else {
            return crate::status_json::idle_json().to_owned();
        };
        // 会话句柄**克隆出来**再快照（M-10② 的同款——status 轮询面不被在途拨号/
        // 收尾持锁阻塞）
        let sess = run.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(s) = sess else {
            // 会话线程还在装配：starting 形态（域状态的快照面 + elapsedMs——L-11①：
            // Go serviceSnapshotJSON 的 elapsedMs 随 Since 恒在）
            let mut m = serde_json::Map::new();
            m.insert("state".into(), serde_json::Value::String(run.domain.state().as_str().into()));
            m.insert(
                "reason".into(),
                serde_json::Value::String(run.domain.reason().to_owned()),
            );
            m.insert(
                "elapsedMs".into(),
                serde_json::Value::from(run.since.elapsed().as_millis() as i64),
            );
            return serde_json::Value::Object(m).to_string();
        };
        let snap = s.snapshot();
        let base = crate::status_json::snapshot_json(&snap);
        // bridge 四键并入（Go serviceSnapshotJSON 的桥段——桥未起全空串）
        let b = run.bridge.status();
        let v: serde_json::Value = serde_json::from_str(&base).unwrap_or_default();
        let mut m = v.as_object().cloned().unwrap_or_default();
        m.insert("bridgeAuth".into(), serde_json::Value::String(b.auth_hex));
        m.insert("bridgeFilesSock".into(), serde_json::Value::String(b.files_sock));
        m.insert("bridgeTermSock".into(), serde_json::Value::String(b.term_sock));
        m.insert("bridgeSpeedSock".into(), serde_json::Value::String(b.speed_sock));
        serde_json::Value::Object(m).to_string()
    }
}

/// 经运行态会话拨出口虚拟端口（服务会话形态的桥 dial：流 id 适配成 BridgeStream）。
/// 会话句柄从锁里克隆出来再拨（评审 r2-M-10②：此前持 `run.session` 锁做 15s 拨号
/// ⇒ status() 同锁被同步阻塞——App 轮询面卡住）。
fn dial_via_run(
    run: &Arc<ServiceRun>,
    port: u16,
    budget: Duration,
) -> io::Result<Box<dyn BridgeStream>> {
    let sess = run.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(s) = sess else {
        return Err(io::Error::new(io::ErrorKind::NotConnected, "服务会话未就绪"));
    };
    let id = s
        .healing_dial_port(port, budget)
        .map_err(|e| match e {
            crate::wgcore::ConnErr::Refused => {
                io::Error::new(io::ErrorKind::ConnectionRefused, e.to_string())
            }
            crate::wgcore::ConnErr::Timeout => {
                io::Error::new(io::ErrorKind::TimedOut, e.to_string())
            }
            other => io::Error::other(other.to_string()),
        })?;
    let client = s.client();
    Ok(Box::new(SessionStream::shared(client, id)))
}

fn open_service_log(out: &str) -> Result<Logf, String> {
    if out.is_empty() {
        return Ok(Arc::new(|_s: &str| {}));
    }
    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(out)
        .map_err(|e| e.to_string())?;
    let f = Arc::new(Mutex::new(f));
    Ok(Arc::new(move |s: &str| {
        use std::io::Write as _;
        let line = format!("{} 服务会话: {s}\n", super::tun_exec::local_ts());
        if let Ok(mut g) = f.lock() {
            let _ = g.write_all(line.as_bytes());
        }
    }))
}

/// 兼容桥 dial 闭包的 UDS 直拨形态（保留参考位：服务桥的远端恒为会话流）。
#[allow(dead_code)]
fn dial_unix_direct(_sock: &str) -> io::Result<UnixStream> {
    unreachable!("服务桥的远端恒为会话流（dial_via_run）")
}
