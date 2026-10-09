//! 服务会话真装配（语义真源 `baseline:clientcore/cmd/clientcore/app_service.go`
//! 的 Service 生命周期 + 桥挂接——openspec app-service-session）。
//!
//! rc 门与状态机在 [`super::service_op::ServiceDomain`]（纯逻辑）；本模块做接线：
//! - `start`：解析 cfg → 打开日志（-2）→ spawn 会话线程（**宿主会话**
//!   [`crate::facade::host_session::HostSession`]——M5 C2 换源：QUIC 岛承接的
//!   无 TUN 服务会话，暖机/巡检/岛内阶梯/失败终态语义逐条同形）→ 桥三座
//!   （dial 经会话流——`HostStream`）→ 状态机推进 starting→ready/failed；
//! - `stop`：桥停 → 会话停（≤6s；岛收工在 `HostSession::stop` 内有界收口）→ idle；
//! - `status`：`crate::status_json::snapshot_json` + bridge 四键（`bridgeAuth`…
//!   ——Go serviceSnapshotJSON 的桥段）。
//!
//! App 进程内单例：与隧道会话**不得并发**（共用设备 WG 身份）——App 侧在连 VPN 前
//! 必调 stop 完整收工；核内 rc 门（stopping 期 start 恒 -1）是第二道防线。

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Deserialize;

use crate::facade::host_session::{HostErr, HostSession, HostSessionConfig, ServicePort, SessState};
use crate::token;
use crate::Logf;

use super::bridge_host::{BridgeHost, BridgeStream};
use super::host_session::NO_SUCH_SERVICE;
use super::service_op::{ServiceDomain, ServiceState};

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
    session: Mutex<Option<Arc<HostSession>>>,
    bridge: Arc<BridgeHost>,
    /// 会话线程的状态推进面（快照读 + state 写回 ServiceDomain）。
    domain: Arc<ServiceDomain>,
    /// 受理时刻（starting 形态的 elapsedMs 源——L-11：Go serviceSnapshotJSON 的
    /// elapsedMs 随 Since 恒在，此前 starting 形态缺）。
    since: std::time::Instant,
    /// 收工请求位（复核 r3-F6③：warmup 期 stop——会话线程建好后不得再发布 Ready）。
    stopping: std::sync::atomic::AtomicBool,
    /// 日志面（与会话同一个 Arc；F2 的「暖机硬失败」行经它打——`open_service_log`
    /// 已带 `服务会话: ` 前缀）。
    logf: Logf,
    /// 收尾**完成**信号（复核 r3-F6①：会话 stop 真正收完才置位——重试 stop 等
    /// 它而不是等「空转线程」，同钥双会话窗口真正闭合；Go 等的是会话自己的 done）。
    ///
    /// Q-F F2/D10：本位同时是「运行槽可替换」的门（对齐 Go `svcStateFailed &&
    /// isDone()` ⇒ 换新会话）。**弱化登记（D13）**：F6 的 detach 让本位弱于 Go 的
    /// `isDone()`（引擎线程可能仍在收尾）——见设计 §5.3/§7-6。
    fully_stopped: Arc<(Mutex<bool>, std::sync::Condvar)>,
}

impl ServiceRun {
    /// 置「已收尾」位（F2：失败实例入槽后立刻置位 ⇒ 下一次 start 可替换）。
    fn mark_fully_stopped(&self) {
        let (lock, cv) = &*self.fully_stopped;
        *lock.lock().unwrap_or_else(|e| e.into_inner()) = true;
        cv.notify_all();
    }

    /// 是否已收尾（运行槽替换门；Go `isDone()` 的等价位）。
    fn is_settled(&self) -> bool {
        let (lock, _cv) = &*self.fully_stopped;
        *lock.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// 服务会话宿主（App 核单例）。
pub struct ServiceExec {
    /// Arc 包一层：会话线程失败时要能清槽（L-11②：failed 后 run 槽未清 ⇒ start 恒 -1）。
    run: Arc<Mutex<Option<Arc<ServiceRun>>>>,
    /// 【test-seams】会话工厂（Q-F F2 测试缝：`start()` 今日硬编码 `HostSession::start`——
    /// 不加工厂就无法注入合成会话）。**不改生产 API**：`new()` 留空 = 走真
    /// `HostSession::start`。
    #[cfg(test)]
    session_factory: Option<SessionFactory>,
}

/// 会话工厂形参（测试缝的类型别名——type_complexity 收口；生产恒 None ⇒ 走真
/// `HostSession::start`，`ServiceExec::new()` 的公开签名不变）。
type SessionFactory =
    Arc<dyn Fn(HostSessionConfig) -> Result<HostSession, HostErr> + Send + Sync>;

impl Default for ServiceExec {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceExec {
    pub fn new() -> Self {
        ServiceExec {
            run: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            session_factory: None,
        }
    }

    /// 【test-seams】注入会话工厂（仅测试可见；生产构造面零改动）。
    #[cfg(test)]
    pub(crate) fn with_session_factory(
        f: SessionFactory,
    ) -> Self {
        ServiceExec {
            run: Arc::new(Mutex::new(None)),
            session_factory: Some(f),
        }
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
        if let Some(prev) = guard.as_ref() {
            // Q-F F2-2（对齐 Go `svcStateFailed && isDone()` ⇒ 换新会话）：槽内实例
            // **已收尾** ⇒ 允许替换（置空后按新 start 走）；否则 = 上次 stop 半途
            // （等收工）——按 -1。
            // 双持钥第二道防线不受影响：域门（Starting/Ready→0、Stopping→-1）在槽门
            // 之前；stop 超时保留槽 + 域 Stopping 时 start 仍被域门挡住。
            if prev.is_settled() {
                *guard = None;
            } else {
                return -1;
            }
        } // token 解析（同步硬失败：参数面 -3 同源——Go Start 的 json.Unmarshal 后
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
            Arc::new(|_port, _budget| {
                // 拨号闭包经运行态取会话（会话在下方线程里建；建好前拨号失败）
                Err(io::Error::new(
                    io::ErrorKind::NotConnected,
                    "服务会话未就绪",
                ))
            }),
        ));
        let run = Arc::new(ServiceRun {
            session: Mutex::new(None),
            bridge: Arc::clone(&bridge),
            domain: Arc::clone(domain),
            since: std::time::Instant::now(),
            stopping: std::sync::atomic::AtomicBool::new(false),
            logf: Arc::clone(&logf),
            fully_stopped: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
        });

        // 会话线程（HostSession::start 阻塞暖机 ~12s——不占调用线程；失败时清 run 槽
        // 〔L-11②：failed 后槽未清 ⇒ 下次 start 恒 -1〕+ 停桥）。
        // **线程体套 catch_unwind**（Q-F F7 同族：panic 不穿透——否则线程死掉后域态
        // 永久停在 Starting，`service_start` 恒 0、status 恒 starting；落空 = 记行 +
        // 桥停 + 清槽 + 域 Failed，与 `Err(e)` 分支同形）。
        let run2 = Arc::clone(&run);
        let run_slot = Arc::clone(&self.run);
        #[cfg(test)]
        let factory = self.session_factory.clone();
        #[cfg(not(test))]
        let factory: Option<SessionFactory> = None;
        let spawn = std::thread::Builder::new()
            .name("homeway-svc".into())
            .spawn(move || {
                let run3 = Arc::clone(&run2);
                let slot3 = Arc::clone(&run_slot);
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    session_thread_body(
                        run2,
                        run_slot,
                        tk,
                        identity_dir,
                        endpoint_cache_dir,
                        logf,
                        factory,
                    );
                }));
                if r.is_err() {
                    (run3.logf)("会话线程 panic（已兜住）—— 会话失败收工（清槽 + 域 Failed）");
                    // 顺序与 `Err(e)` 分支**逐字同形**（代码门 r15 新增 3：写域态 →
                    // 停桥 → 清槽——倒序会在「清槽」与「写域态」之间留出被新 start
                    // 抢占的窗口）
                    run3.domain.set_state(ServiceState::Failed);
                    run3.domain.set_reason("会话线程 panic（已兜住）");
                    run3.bridge.stop();
                    clear_slot_if_same(&slot3, &run3);
                }
            });
        if spawn.is_err() {
            domain.set_state(ServiceState::Failed);
            domain.set_reason("会话线程启动失败");
            return -1;
        }
        *guard = Some(Arc::clone(&run));
        drop(guard);
        // 桥在会话受理后启动（dial 经运行态会话——见 dial_via_run）。
        // R8-8c（F15 处置）：闭包持 **Weak**——ServiceRun.bridge → BridgeHost →
        // 闭包 → Arc<ServiceRun> 的引用环每 start/stop 周期泄漏一套对象；降 Weak
        // 后强引用只剩 run 槽 + 会话线程（stop 清槽 + 线程退出即整组释放）。
        // upgrade 失败（正在收工）= 桥拨号「服务会话未就绪」同语义。
        bridge.set_dial(bridge_dial_closure(run, dial_via_run));
        bridge.start();
        0
    }

    /// ClientCoreServiceStop：0 已收工 / -1 等待超时（调用方重试；期间 start 恒 -1）。
    /// 顺序 = 桥 → 会话 → （岛收尾由 `HostSession::stop` 内完成）。
    /// **超时不提前清槽**（评审 r2-M-10①：旧实现超时前已 take ⇒ 第二次 stop 见空槽
    /// 直接置 Idle 返回 0 ⇒ start 放行 ⇒ 同钥双会话窗口；现 run 槽留到真正收完）。
    pub fn stop(&self, domain: &ServiceDomain) -> i32 {
        let run = {
            let guard = self.run.lock().unwrap_or_else(|e| e.into_inner());
            guard.clone()
        }; // 锁只护取句柄——后续全程无锁（复核 r3-F6②：此前持锁跨 bridge.stop(≤2s)
           // + 6s 轮询 ⇒ status/start 被阻塞 ≤8s，与「轮询面不再被阻塞」的宣称相反）
        let Some(run) = run else {
            domain.set_state(ServiceState::Idle);
            return 0; // 本就没在跑
        };
        domain.set_state(ServiceState::Stopping);
        run.stopping
            .store(true, std::sync::atomic::Ordering::Release);
        run.bridge.stop();
        // 会话句柄取走（重试 stop 时已取空 ⇒ 只等完成信号；桥停幂等）
        let sess = run.session.lock().unwrap_or_else(|e| e.into_inner()).take();
        // warmup 期 sess=None ⇒ 先等会话线程过 stopping 检查点（它建好后自查自收），
        // 上界 2s——超界按已收尾处理（HostSession::start 仍在跑的极端形态交给进程收口）
        if sess.is_none() {
            let warm_deadline = std::time::Instant::now() + Duration::from_secs(2);
            while run
                .session
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_none()
                && std::time::Instant::now() < warm_deadline
            {
                std::thread::sleep(Duration::from_millis(50));
            }
            let s2 = run.session.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(s) = s2.as_ref() {
                s.stop();
            }
        }
        // 会话收尾 + 置完成信号（Q-F F7-2/N6：`std::thread::spawn` 失败 = panic，且
        // 会把服务会话打进**永久 Stopping**（`fully_stopped` 永不置位 + 槽不清 ⇒
        // 此后 start 恒 -1）——改 `Builder::spawn` 检 Result，失败**就地同步收尾**）
        let done_sig = Arc::clone(&run.fully_stopped);
        let logf2 = Arc::clone(&run.logf);
        let sess_for_reap = sess.clone();
        let reaped = std::thread::Builder::new()
            .name("homeway-svc-reap".into())
            .spawn(move || {
                if let Some(s) = sess_for_reap.as_ref() {
                    s.stop();
                }
                let (lock, cv) = &*done_sig;
                let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
                *g = true;
                cv.notify_all();
            });
        let h = match reaped {
            Ok(h) => h,
            Err(e) => {
                crate::syncutil::log_spawn_failed(
                    &logf2,
                    "homeway-svc-reap",
                    &e,
                    "收尾线程起不来——本次 stop 就地同步收尾",
                );
                settle_inline(&run, sess.as_ref(), &self.run, domain);
                return 0;
            }
        };
        // 有界等待真正的收尾完成（复核 r3-F6①：Go serviceStopWait 等会话自己的 done）
        let deadline = std::time::Instant::now() + Duration::from_secs(6);
        {
            let (lock, cv) = &*run.fully_stopped;
            let mut g = lock.lock().unwrap_or_else(|e| e.into_inner());
            while !*g {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    // 收工超时：run 槽保留 + 域状态停在 stopping（start 恒 -1 防双持钥）；
                    // 收尾线程随后自行完成——下一次 stop 重入等同一个信号（真闭合）
                    return -1;
                }
                let (g2, _to) = cv.wait_timeout(g, left).unwrap_or_else(|e| e.into_inner());
                g = g2;
            }
        }
        let _ = h.join();
        clear_slot_if_same(&self.run, &run);
        domain.set_state(ServiceState::Idle);
        domain.set_reason("");
        0
    }

    /// ClientCoreServiceStatus：无实例 = idle 短路（**域态 Failed 时改报 failed+原因**
    /// ——Q-F F2-3 可选小修，覆盖 `HostSession::start` 返 Err 的清槽路径：此前槽空即
    /// idle，而域态是 Failed+原因 ⇒ 状态面与 rc 门自相矛盾）；有实例 = snapshot + bridge 四键。
    pub fn status(&self, domain: &ServiceDomain) -> String {
        let guard = self.run.lock().unwrap_or_else(|e| e.into_inner());
        let Some(run) = guard.as_ref() else {
            // 槽空：failed 域态照实输出（其余 = idle 短路逐字节不变）
            if domain.state() == ServiceState::Failed {
                let mut m = serde_json::Map::new();
                m.insert(
                    "state".into(),
                    serde_json::Value::String(ServiceState::Failed.as_str().into()),
                );
                m.insert(
                    "reason".into(),
                    serde_json::Value::String(domain.reason().to_owned()),
                );
                return serde_json::Value::Object(m).to_string();
            }
            return crate::status_json::idle_json().to_owned();
        };
        // 会话句柄**克隆出来**再快照（M-10② 的同款——status 轮询面不被在途拨号/
        // 收尾持锁阻塞）
        let sess = run
            .session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let Some(s) = sess else {
            // 会话线程还在装配：starting 形态（域状态的快照面 + elapsedMs——L-11①：
            // Go serviceSnapshotJSON 的 elapsedMs 随 Since 恒在）
            let mut m = serde_json::Map::new();
            m.insert(
                "state".into(),
                serde_json::Value::String(run.domain.state().as_str().into()),
            );
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
        m.insert(
            "bridgeFilesSock".into(),
            serde_json::Value::String(b.files_sock),
        );
        m.insert(
            "bridgeTermSock".into(),
            serde_json::Value::String(b.term_sock),
        );
        m.insert(
            "bridgeSpeedSock".into(),
            serde_json::Value::String(b.speed_sock),
        );
        serde_json::Value::Object(m).to_string()
    }
}

/// 槽清理的 ptr_eq 身份校验（复核 r3-F6④：只清自己——防旧线程清掉新 start 的 runB）。
fn clear_slot_if_same(slot: &Arc<Mutex<Option<Arc<ServiceRun>>>>, expect: &Arc<ServiceRun>) {
    let mut g = slot.lock().unwrap_or_else(|e| e.into_inner());
    if g.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, expect)) {
        *g = None;
    }
}

/// 槽身份判定（F2 域态写回守卫：仅当槽仍指向本 run 才写域态——与并发 stop 分裂防护）。
fn slot_is_same(slot: &Arc<Mutex<Option<Arc<ServiceRun>>>>, expect: &Arc<ServiceRun>) -> bool {
    slot.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|cur| Arc::ptr_eq(cur, expect))
}

/// 发布判定纯函数（Q-F F2-4）：`HostSession::start` 返回 Ok 后**只有快照已 ready**
/// 才发布；`stopping` 竞态由外层分支处理（不并入本函数）。
fn publish_ready(state: SessState) -> bool {
    state == SessState::Ready
}

/// 会话线程体（`HostSession::start` + Q-F F2-1 三分支；由 `catch_unwind` 包裹）。
fn session_thread_body(
    run2: Arc<ServiceRun>,
    run_slot: Arc<Mutex<Option<Arc<ServiceRun>>>>,
    tk: crate::token::Token,
    identity_dir: Option<PathBuf>,
    _endpoint_cache_dir: Option<PathBuf>,
    logf: Logf,
    factory: Option<SessionFactory>,
) {
    // M5 C2 换源：宿主会话配置（端点缓存面随 WG 档退役——§1.6-G-1；`ServiceConfigJson`
    // 的 `endpointCacheDir` 键面保留但不再被消费）。
    let sess_cfg = HostSessionConfig {
        token: tk,
        identity_dir,
        logf: Arc::clone(&logf),
    };
    // 生产 = 真 HostSession::start；测试可注入合成会话（cfg(test) 工厂）
    let sess = match &factory {
        Some(f) => f(sess_cfg),
        None => HostSession::start(sess_cfg),
    };
    match sess {
        Ok(s) => {
            // ---- 三分支显式处置（Q-F F2-1；顺序即优先级）----
            if run2.stopping.load(std::sync::atomic::Ordering::Acquire) {
                // ① stopping 竞态（复核 r3-F6③，语义逐字保留）：stop 在暖机期
                //    到达 ⇒ 不发布 Ready，就地停会话 + 桥 + 清槽 + Idle
                s.stop();
                run2.bridge.stop();
                clear_slot_if_same(&run_slot, &run2);
                run2.domain.set_state(ServiceState::Idle);
                run2.domain.set_reason("");
                return;
            }
            let snap = s.snapshot();
            if publish_ready(snap.state) {
                // ② 就绪发布（既有行为）：桥 dial 经运行态取会话（见 dial_via_run）
                *run2.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::new(s));
                run2.domain.set_state(ServiceState::Ready);
                run2.domain.set_reason("");
            } else {
                // ③ 硬失败（Ok-but-failed，Q-F F2/D10）：**不发布就绪**——
                //    桥先停（Go finish 同序）→ 会话内收尾 → 失败实例**入槽**
                //    （status 继续走会话快照 = failed+原因）→ 置「已收尾」位
                //    （= Go isDone() 等价位 ⇒ 下次 start 可替换）
                let reason = if snap.reason.is_empty() {
                    "出口不可达".to_owned()
                } else {
                    snap.reason.clone()
                };
                (run2.logf)(&format!(
                    "服务会话暖机硬失败（原因={reason}）——不发布就绪"
                ));
                run2.bridge.stop();
                let s = Arc::new(s);
                s.stop();
                *run2.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&s));
                run2.mark_fully_stopped();
                // 域态写回加身份守卫（照 clear_slot_if_same 范式）：仅当槽仍是本 run
                // 才写（与并发 stop 分裂防护）
                if slot_is_same(&run_slot, &run2) {
                    run2.domain.set_state(ServiceState::Failed);
                    run2.domain.set_reason(&reason);
                }
                // 保留 run 槽：service_status 继续报 failed+原因（Go m.cur 语义）
            }
        }
        Err(e) => {
            run2.domain.set_state(ServiceState::Failed);
            run2.domain.set_reason(&e.to_string());
            // 失败清槽（L-11②；ptr_eq 防清新 start 的 runB——复核 r3-F6④）+
            // 桥收工（受理侧起的桥随失败收掉）
            run2.bridge.stop();
            clear_slot_if_same(&run_slot, &run2);
        }
    }
}

/// 收尾线程起不来时的**就地同步收尾**（Q-F F7-2/N6）：会话停 + 置「已收尾」位 +
/// 清槽 + 域 Idle——不留**永久 Stopping**（否则此后 `service_start` 恒 -1）。
fn settle_inline(
    run: &Arc<ServiceRun>,
    sess: Option<&Arc<HostSession>>,
    slot: &Arc<Mutex<Option<Arc<ServiceRun>>>>,
    domain: &ServiceDomain,
) {
    if let Some(s) = sess {
        s.stop();
    }
    run.mark_fully_stopped();
    clear_slot_if_same(slot, run);
    domain.set_state(ServiceState::Idle);
    domain.set_reason("");
}

type BridgeDialFn = crate::facade::bridge_host::DialFn;
/// 闭包工厂的拨号实现面（dial_via_run 同形——T = ServiceRun）。
type BridgeDialImpl<T> = fn(&Arc<T>, u16, Duration) -> io::Result<Box<dyn crate::facade::bridge_host::BridgeStream>>;

fn bridge_dial_closure<T: Send + Sync + 'static>(run: Arc<T>, dial: BridgeDialImpl<T>) -> BridgeDialFn {
    let weak = Arc::downgrade(&run);
    Arc::new(move |port, budget| match weak.upgrade() {
        Some(r) => dial(&r, port, budget),
        None => Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "服务会话未就绪",
        )),
    })
}

/// 经运行态会话拨出口虚拟端口（服务会话形态的桥 dial：流 id 适配成 BridgeStream）。
/// 会话句柄从锁里克隆出来再拨（评审 r2-M-10②：此前持 `run.session` 锁做 15s 拨号
/// ⇒ status() 同锁被同步阻塞——App 轮询面卡住）。
/// 桥拨号闭包工厂（R8-3 F16 可测面）：闭包持 **Weak**——ServiceRun.bridge →
/// BridgeHost → 闭包 → Arc<ServiceRun> 的引用环每 start/stop 周期泄漏一套对象
/// （R8-8c F15 整改的原始动机）；降 Weak 后强引用只剩 run 槽 + 会话线程（stop
/// 清槽 + 线程退出即整组释放）。upgrade 失败（正在收工）= 桥拨号「服务会话未
/// 就绪」同语义。泛型 `T` + 函数指针只为单测能以轻量替身钉死两点：闭包不抬
/// 强计数 / 收工后拨号报未就绪（见 tests::bridge_dial_closure_weak_only）。
/// 桥拨号闭包的产出类型（set_dial 形参同形——type_complexity 收口）。
fn dial_via_run(
    run: &Arc<ServiceRun>,
    port: u16,
    budget: Duration,
) -> io::Result<Box<dyn BridgeStream>> {
    let sess = run
        .session
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    let Some(s) = sess else {
        return Err(io::Error::new(
            io::ErrorKind::NotConnected,
            "服务会话未就绪",
        ));
    };
    // 端口 → 服务 tag 单源（未知端口 = 归因「无此服务」——拨号前）；
    // 错误面：`HostErr` → `io::Error`（§2.7 R-4 的单处映射）。
    let sp = ServicePort::from_bridge_port(port)
        .ok_or_else(|| io::Error::new(io::ErrorKind::ConnectionRefused, NO_SUCH_SERVICE))?;
    let stream = s.connect(sp, budget).map_err(io::Error::from)?;
    Ok(Box::new(stream))
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

// （Q-F F6-3：`dial_unix_direct` 的死函数已删——`unreachable!` 是 Drop/收工链上的
// panic 面，服务桥的远端恒为会话流，保留参考位无收益。）

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// 合法 token 的 start 配置（`start()` 会真解 token——占位串必 -3）。
    fn cfg_json() -> String {
        use crate::token::{EndpointRef, PeerId, Secret, TokenSpec};
        let peer = PeerId::from([1u8; 32]);
        let secret = Secret::from([2u8; 32]);
        let eps: [EndpointRef; 0] = [];
        let tok = crate::token::encode(&TokenSpec {
            peer_id: &peer,
            secret: &secret,
            endpoints: &eps,
            rpk: None,
        })
        .expect("空端点 token 可编码");
        format!(r#"{{"token":"{tok}"}}"#)
    }

    /// 轮询等条件成立（或超时 panic）。
    fn wait_for(what: &str, budget: Duration, f: impl Fn() -> bool) {
        let deadline = std::time::Instant::now() + budget;
        while !f() {
            assert!(std::time::Instant::now() < deadline, "等 {what} 超时");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// F2-4：发布判定真值表（只有 Ready 发布）。
    #[test]
    fn publish_ready_truth_table() {
        use crate::facade::host_session::SessState;
        assert!(publish_ready(SessState::Ready));
        for s in [
            SessState::Starting,
            SessState::Failed,
            SessState::Stopping,
            SessState::Idle,
        ] {
            assert!(!publish_ready(s), "{s:?} 不得发布就绪");
        }
    }

    /// F2 三分支之③（硬失败）：不发布就绪 + 失败实例入槽 + 域 Failed + 状态面报
    /// failed+原因 + **可重建**（Go `svcStateFailed && isDone()`）+ stop 无 2s 空等。
    #[test]
    fn warm_hard_failure_keeps_failed_and_replaceable() {
        let domain = Arc::new(ServiceDomain::new());
        let calls = Arc::new(AtomicUsize::new(0));
        let c2 = Arc::clone(&calls);
        let exec = ServiceExec::with_session_factory(Arc::new(move |_cfg| {
            c2.fetch_add(1, Ordering::SeqCst);
            Ok(HostSession::synthetic_for_test(
                SessState::Failed,
                "出口不可达：测试硬失败",
            ))
        }));
        assert_eq!(exec.start(&cfg_json(), &domain), 0);
        wait_for("域态 Failed", Duration::from_secs(5), || {
            domain.state() == ServiceState::Failed
        });
        assert_eq!(domain.reason(), "出口不可达：测试硬失败");
        // 状态面：走会话快照 = failed + 原因（不是 idle、不是 ready）
        let st = exec.status(&domain);
        assert!(st.contains("\"state\":\"failed\""), "{st}");
        assert!(st.contains("出口不可达：测试硬失败"), "{st}");
        // 失败实例入槽 + 「已收尾」位（替换门）
        let run = exec.run.lock().unwrap().clone().expect("失败实例保留在槽");
        assert!(run.session.lock().unwrap().is_some(), "失败实例入槽");
        assert!(run.is_settled(), "fully_stopped 置位（= Go isDone() 等价位）");
        // 可重建：失败实例已收尾 ⇒ start 受理（替换）
        assert_eq!(exec.start(&cfg_json(), &domain), 0);
        wait_for("第二次会话起", Duration::from_secs(5), || {
            calls.load(Ordering::SeqCst) >= 2
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2, "失败实例可替换（不得恒 -1）");
        wait_for("域态再 Failed", Duration::from_secs(5), || {
            domain.state() == ServiceState::Failed
        });
        // stop：会话在槽 ⇒ 不走 warmup 2s 轮询（无空等）
        let t0 = std::time::Instant::now();
        assert_eq!(exec.stop(&domain), 0);
        assert!(
            t0.elapsed() < Duration::from_millis(1500),
            "stop 不得出现 2s 空等（实耗 {:?}）",
            t0.elapsed()
        );
        assert_eq!(domain.state(), ServiceState::Idle);
        assert!(exec.run.lock().unwrap().is_none(), "收工后清槽");
    }

    /// F2 三分支之①（stopping 竞态，回归 C3a）：暖机期 stop ⇒ 不发布就绪、清槽、域 Idle。
    #[test]
    fn warm_stopping_race_clears_slot_and_goes_idle() {
        let domain = Arc::new(ServiceDomain::new());
        let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
        let gate2 = Arc::clone(&gate);
        let exec = Arc::new(ServiceExec::with_session_factory(Arc::new(move |_cfg| {
            // 挂到测试放行（模拟「暖机中」）
            let (l, cv) = &*gate2;
            let mut g = l.lock().unwrap_or_else(|e| e.into_inner());
            while !*g {
                g = cv.wait(g).unwrap_or_else(|e| e.into_inner());
            }
            Ok(HostSession::synthetic_for_test(SessState::Ready, ""))
        })));
        assert_eq!(exec.start(&cfg_json(), &domain), 0);
        std::thread::sleep(Duration::from_millis(100));
        let exec2 = Arc::clone(&exec);
        let domain2 = Arc::clone(&domain);
        let stopper = std::thread::spawn(move || exec2.stop(&domain2));
        std::thread::sleep(Duration::from_millis(150)); // 等 stopping 置位
        {
            // 放行工厂：会话线程应在 stopping 分支收口
            let (l, cv) = &*gate;
            *l.lock().unwrap_or_else(|e| e.into_inner()) = true;
            cv.notify_all();
        }
        wait_for("域态 Idle", Duration::from_secs(5), || {
            domain.state() == ServiceState::Idle
        });
        assert!(exec.run.lock().unwrap().is_none(), "stopping 分支清槽");
        assert_eq!(stopper.join().unwrap(), 0, "stop 收口 0");
    }

    /// F7-2/N6：收尾线程起不来 ⇒ 就地同步收尾（不留永久 Stopping）。
    #[test]
    fn settle_inline_never_leaves_stopping() {
        let domain = Arc::new(ServiceDomain::new());
        let bridge = Arc::new(BridgeHost::new(
            "测试桥",
            None,
            Arc::new(|_s: &str| {}),
            Arc::new(|_, _| Err(io::Error::new(io::ErrorKind::NotConnected, "无会话"))),
        ));
        let run = Arc::new(ServiceRun {
            session: Mutex::new(None),
            bridge,
            domain: Arc::clone(&domain),
            since: std::time::Instant::now(),
            stopping: std::sync::atomic::AtomicBool::new(true),
            logf: Arc::new(|_s: &str| {}),
            fully_stopped: Arc::new((Mutex::new(false), std::sync::Condvar::new())),
        });
        let slot: Arc<Mutex<Option<Arc<ServiceRun>>>> =
            Arc::new(Mutex::new(Some(Arc::clone(&run))));
        domain.set_state(ServiceState::Stopping);
        settle_inline(&run, None, &slot, &domain);
        assert_eq!(domain.state(), ServiceState::Idle, "不得停在 Stopping");
        assert!(run.is_settled());
        assert!(slot.lock().unwrap().is_none(), "槽已清（此后 start 可受理）");
    }

    /// F7 同族（代码门 ①-1）：会话线程体 panic 被兜住 ⇒ 记行 + 清槽 + 域 Failed
    /// （不得留在永久 Starting：否则 `service_start` 恒 0、status 恒 starting）。
    #[test]
    fn session_thread_panic_is_caught_and_domain_failed() {
        let domain = Arc::new(ServiceDomain::new());
        let exec = ServiceExec::with_session_factory(Arc::new(|_cfg| {
            panic!("注入：会话线程体炸了");
        }));
        assert_eq!(exec.start(&cfg_json(), &domain), 0);
        wait_for("域态 Failed", Duration::from_secs(5), || {
            domain.state() == ServiceState::Failed
        });
        assert!(
            domain.reason().contains("panic"),
            "落空归因：{}",
            domain.reason()
        );
        assert!(exec.run.lock().unwrap().is_none(), "落空后清槽");
        // 槽已清 + 域 Failed ⇒ 下一次 start 可受理
        assert_eq!(exec.start(&cfg_json(), &domain), 0);
        wait_for("第二次域态 Failed", Duration::from_secs(5), || {
            domain.state() == ServiceState::Failed
        });
        let _ = exec.stop(&domain);
    }

    /// F2-3（可选小修，已采）：槽空 + 域 Failed ⇒ status 报 failed+原因（不再谎报 idle）。
    #[test]
    fn status_reports_failed_when_slot_empty() {
        let domain = Arc::new(ServiceDomain::new());
        let exec = ServiceExec::new();
        assert_eq!(
            exec.status(&domain),
            "{\"state\":\"idle\"}",
            "Idle 短路逐字节不变"
        );
        domain.set_state(ServiceState::Failed);
        domain.set_reason("token 解析失败：坏串");
        let st = exec.status(&domain);
        assert!(st.contains("\"state\":\"failed\""), "{st}");
        assert!(st.contains("token 解析失败：坏串"), "{st}");
    }

    /// F16（R8-3 尾账）：桥拨号闭包**只持 Weak**——不抬强计数（R8-8c F15 的
    /// 引用环整改点），run 释放后闭包不复活对象、拨号报「服务会话未就绪」
    /// （NotConnected——与 dial_via_run 会话缺席分支同词面同语义）。
    #[test]
    fn bridge_dial_closure_weak_only() {
        #[derive(Default)]
        struct Dummy {
            called: AtomicBool,
        }
        fn fake_dial(
            r: &Arc<Dummy>,
            _port: u16,
            _budget: Duration,
        ) -> io::Result<Box<dyn crate::facade::bridge_host::BridgeStream>> {
            r.called.store(true, Ordering::Release);
            Err(io::Error::new(io::ErrorKind::AddrInUse, "替身不产流"))
        }
        let run = Arc::new(Dummy::default());
        let dial = bridge_dial_closure(Arc::clone(&run), fake_dial);
        // ① 闭包持 Weak：本测试的 run 强引用只有这里的一个（工厂已 downgrade）
        assert_eq!(
            Arc::strong_count(&run),
            1,
            "闭包不得抬强计数（引用环回归——每 start/stop 周期泄漏一套对象）"
        );
        // ② run 在世：拨号抵达真实现（替身被调 = upgrade 成功路径）
        let r1 = match dial(7802, Duration::from_secs(1)) {
            Err(e) => e,
            Ok(_) => panic!("替身不产流——不应成功"),
        };
        assert_eq!(r1.kind(), io::ErrorKind::AddrInUse, "替身透传");
        assert!(run.called.load(Ordering::Acquire), "upgrade 成功应抵达 dial 实现");
        // ③ run 收工（强引用清零）：闭包不复活对象，拨号 = NotConnected + 同词面
        let weak = Arc::downgrade(&run);
        drop(run);
        assert!(weak.upgrade().is_none(), "前置：对象确已释放");
        let err = match dial(7802, Duration::from_secs(1)) {
            Err(e) => e,
            Ok(_) => panic!("run 已释放——不应拨通"),
        };
        assert_eq!(err.kind(), io::ErrorKind::NotConnected);
        assert_eq!(err.to_string(), "服务会话未就绪");
    }
}
