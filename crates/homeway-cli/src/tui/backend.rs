//! backend：I/O 接线层。
//!
//! - **控制面喂入循环**（tasks 3.1）：拨号（daemon 未运行按需拉起，spec 需求 1）→
//!   `snapshot.get` 全量初值 → `events.subscribe`（cursor = 快照 seq，generation 门）
//!   → 事件循环；断开 = [`FeedInput::ControlDown`] + 退避重拨（0.5s 起 ×2 至 10s
//!   上限，r3 F——防紧循环）；重连 = [`FeedInput::ControlUp`] ⇒ 循环总以全量快照
//!   开场（重取即模型要求的 Resnapshot）；`cursor_stale` = 立即重拨拿新代（不退避、
//!   不下发 ControlDown——连接从未确立，r2 N-12③：不复刻 watch 的 exit(1)）。
//! - **LIST 轮询器**（tasks 3.3）：短连接一拍 LIST（不占 8 腿预算），活动态 ≤2s /
//!   非活动态 10s（design D3）；失败丢拍、列表保持上一拍。
//!
//! 与 I/O 解耦的缝：[`ControlConn`]（真实面 = [`RealConn`]，测试面 = mock）、
//! sleep 注入（退避时序可断言）、`dial`/`fetch`/`sink`/`is_active` 全部闭包注入。
//! 消费方 = app 主循环（任务组 4）——期间豁免 dead_code。

#![allow(dead_code)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use homeway_core::daemon::client::ControlClient;
use homeway_core::daemon::proto::{EventBody, HostState};
use homeway_core::daemon::vocab;

use super::hosts::{FeedInput, HostsModel};
use super::sessions::SessionView;

// ---------------------------------------------------------------------------
// 控制面喂入循环（tasks 3.1）
// ---------------------------------------------------------------------------

/// 停止哨兵（app 退出时置位；循环在拍边界退出）。
#[derive(Default, Clone)]
pub(crate) struct StopFlag(Arc<AtomicBool>);

impl StopFlag {
    pub fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }

    pub fn stopped(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// 控制面连接抽象：真实面 = [`RealConn`]（ControlClient），测试面 = mock。
pub(crate) trait ControlConn {
    /// 全量快照：(generation, seq, hosts)。
    fn snapshot(&mut self) -> Result<(String, u64, Vec<HostState>), String>;
    /// 订阅（cursor 续播 + generation 门）。
    fn subscribe(&mut self, cursor: u64, generation: &str) -> Result<(), SubErr>;
    /// 收事件（带超时；None = 超时无事件）。
    fn recv_event(&mut self, timeout: Duration) -> Option<EventBody>;
    /// 连接是否已被服务端终结（goodbye/关停面）。
    fn closed(&self) -> bool;
    fn close(&mut self);
}

/// 订阅失败的两类面：代际失配（立即重拨拿新代）/ 其它。
#[derive(Debug)]
pub(crate) enum SubErr {
    CursorStale,
    Other(String),
}

/// 真实连接（ControlClient 适配）。`events` 槽在 dial 时建立，`take_events` 一次性取走。
pub(crate) struct RealConn {
    c: Arc<ControlClient>,
    events: Option<std::sync::mpsc::Receiver<EventBody>>,
}

impl RealConn {
    pub(crate) fn new(c: Arc<ControlClient>) -> Self {
        RealConn { events: c.take_events(), c }
    }
}

impl ControlConn for RealConn {
    fn snapshot(&mut self) -> Result<(String, u64, Vec<HostState>), String> {
        let v = self
            .c
            .request(vocab::OpName::SnapshotGet.as_str(), None, Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        let generation = v["generation"].as_str().ok_or("快照缺 generation")?.to_owned();
        let seq = v["seq"].as_u64().unwrap_or(0);
        let mut hosts = Vec::new();
        for h in v["hosts"].as_array().into_iter().flatten() {
            hosts.push(serde_json::from_value(h.clone()).map_err(|e| format!("快照主机解析失败：{e}"))?);
        }
        Ok((generation, seq, hosts))
    }

    fn subscribe(&mut self, cursor: u64, generation: &str) -> Result<(), SubErr> {
        let domains = [vocab::DOMAIN_LINK.to_owned(), vocab::DOMAIN_SESSION.to_owned()];
        match self
            .c
            .subscribe(&domains, Some(cursor), "", generation, Duration::from_secs(10))
        {
            Ok(_) => Ok(()),
            Err(e) if e.code == vocab::CODE_CURSOR_STALE => Err(SubErr::CursorStale),
            Err(e) => Err(SubErr::Other(e.to_string())),
        }
    }

    fn recv_event(&mut self, timeout: Duration) -> Option<EventBody> {
        self.events.as_ref()?.recv_timeout(timeout).ok()
    }

    fn closed(&self) -> bool {
        self.c.is_closed()
    }

    fn close(&mut self) {
        self.c.close();
    }
}

/// TUI 控制面拨号（spec 需求 1：未运行族按需拉起；frontend = homeway-tui）。
pub(crate) fn dial_control(state_dir: &Path) -> Result<Arc<ControlClient>, String> {
    let sock = state_dir.join("control.sock");
    match ControlClient::dial(&sock, "cli", "homeway-tui") {
        Ok((c, _)) => Ok(c),
        Err(e) => {
            if crate::daemon_cli::sock_not_running(&sock) {
                crate::daemon_cli::dial_control_spawn(state_dir, "homeway-tui", false)
            } else {
                Err(e.to_string())
            }
        }
    }
}

/// 喂入出口：真实面 = `Mutex<HostsModel>`（app 层持有），测试面 = 记录器。
pub(crate) trait FeedSink {
    fn feed(&self, input: FeedInput<'_>);
}

impl FeedSink for Mutex<HostsModel> {
    fn feed(&self, input: FeedInput<'_>) {
        self.lock().unwrap_or_else(|e| e.into_inner()).feed(input);
    }
}

/// 控制面喂入循环（阻塞；放后台线程跑）。会话确立（订阅成功）即重置退避。
pub(crate) fn control_loop(
    mut dial: impl FnMut() -> Result<Box<dyn ControlConn>, String>,
    sink: &dyn FeedSink,
    stop: &StopFlag,
    sleep: &mut dyn FnMut(Duration),
) {
    let (init, max) = (Duration::from_millis(500), Duration::from_secs(10));
    let mut backoff = init;
    'session: loop {
        if stop.stopped() {
            return;
        }
        let mut conn = match dial() {
            Ok(c) => c,
            Err(_) => {
                sink.feed(FeedInput::ControlDown);
                sleep(backoff);
                backoff = (backoff * 2).min(max);
                continue;
            }
        };
        let (gen, seq, hosts) = match conn.snapshot() {
            Ok(x) => x,
            Err(_) => {
                conn.close();
                sink.feed(FeedInput::ControlDown);
                sleep(backoff);
                backoff = (backoff * 2).min(max);
                continue;
            }
        };
        sink.feed(FeedInput::ControlUp);
        sink.feed(FeedInput::Snapshot { generation: &gen, hosts: &hosts });
        match conn.subscribe(seq, &gen) {
            Ok(()) => backoff = init,
            Err(SubErr::CursorStale) => {
                // 代际失配：连接从未确立——不下发 ControlDown、不退避，立即重拨。
                conn.close();
                continue;
            }
            Err(SubErr::Other(_)) => {
                conn.close();
                sink.feed(FeedInput::ControlDown);
                sleep(backoff);
                backoff = (backoff * 2).min(max);
                continue;
            }
        }
        loop {
            if stop.stopped() {
                conn.close();
                return;
            }
            match conn.recv_event(Duration::from_millis(500)) {
                Some(ev) => sink.feed(FeedInput::Event(&ev)),
                None => {
                    if conn.closed() {
                        conn.close();
                        sink.feed(FeedInput::ControlDown);
                        sleep(backoff);
                        backoff = (backoff * 2).min(max);
                        continue 'session;
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// LIST 轮询器（tasks 3.3）
// ---------------------------------------------------------------------------

/// LIST 一拍：拨 term 连接（**no_spawn**——拉起职责在控制面循环/面板启动，轮询拍
/// 只读不拉）→ GREETING → LIST 往返 → 解析。远端 = 控制面 stream.open；本机 = 直连
/// `<state>/serve/term.sock`（host_ref 传哨兵 [`super::local::LOCAL_HOST_ID`] 时
/// 走本地面——注意目录语义：本地面要传 `<state>/serve` 子目录，r1 D-14）。
pub(crate) fn list_once(state_dir: &Path, host_ref: &str) -> Result<Vec<SessionView>, String> {
    if host_ref == super::local::LOCAL_HOST_ID {
        // 本机：serve 子目录（design D5；term_cli 的本地面 --state 语义即 serve 目录）。
        let (mut conn, _) =
            crate::term_cli::dial_term_conn_impl(state_dir.join("serve").as_path(), None, true)?;
        let payload = crate::term_cli::round_trip(&mut conn, homeway_core::term::frames::Op::LIST, &[])?;
        conn.close();
        return super::sessions::parse_list_reply(&payload).ok_or_else(|| "LIST 应答解析失败".to_owned());
    }
    let (mut conn, _) = crate::term_cli::dial_term_conn_impl(state_dir, Some(host_ref), true)?;
    let payload = crate::term_cli::round_trip(&mut conn, homeway_core::term::frames::Op::LIST, &[])?;
    conn.close();
    super::sessions::parse_list_reply(&payload).ok_or_else(|| "LIST 应答解析失败".to_owned())
}

/// LIST 轮询循环：fetch → sink → 按活动态拍长睡眠（分片以便及时响应停止）。
/// fetch 失败静默丢拍。
pub(crate) fn list_poll_loop(
    mut fetch: impl FnMut() -> Result<Vec<SessionView>, String>,
    sink: &dyn Fn(&[SessionView]),
    is_active: &dyn Fn() -> bool,
    stop: &StopFlag,
    sleep: &mut dyn FnMut(Duration),
) {
    while !stop.stopped() {
        if let Ok(v) = fetch() {
            sink(&v);
        }
        let mut left = if is_active() {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(10)
        };
        while left > Duration::ZERO && !stop.stopped() {
            let step = left.min(Duration::from_millis(100));
            sleep(step);
            left = left.saturating_sub(step);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // ---- mock 面 ----------------------------------------------------------

    struct MockConn {
        gen: String,
        seq: u64,
        hosts: Vec<HostState>,
        events: std::collections::VecDeque<EventBody>,
        /// 事件排空后即「被服务端终结」——驱动循环走重连分支。
        close_after_drain: bool,
        closed: bool,
        subscribe_err: Option<SubErr>,
    }

    impl MockConn {
        fn new(gen: &str, seq: u64, hosts: Vec<HostState>, events: Vec<EventBody>) -> Self {
            MockConn { gen: gen.into(), seq, hosts, events: events.into(), close_after_drain: true, closed: false, subscribe_err: None }
        }
        fn with_sub_err(mut self, e: SubErr) -> Self {
            self.subscribe_err = Some(e);
            self
        }
    }

    impl ControlConn for MockConn {
        fn snapshot(&mut self) -> Result<(String, u64, Vec<HostState>), String> {
            Ok((self.gen.clone(), self.seq, self.hosts.clone()))
        }
        fn subscribe(&mut self, _cursor: u64, _generation: &str) -> Result<(), SubErr> {
            match self.subscribe_err.take() {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
        fn recv_event(&mut self, _timeout: Duration) -> Option<EventBody> {
            let ev = self.events.pop_front();
            if ev.is_none() && self.close_after_drain {
                self.closed = true;
            }
            ev
        }
        fn closed(&self) -> bool {
            self.closed
        }
        fn close(&mut self) {
            self.closed = true;
        }
    }

    /// 记录型 sink + sleep 序列（退避时序断言）。
    #[derive(Default)]
    struct Recorder {
        inputs: std::sync::Mutex<Vec<String>>,
        sleeps: std::sync::Mutex<Vec<Duration>>,
    }
    impl Recorder {
        fn log(&self, s: &str) {
            self.inputs.lock().unwrap().push(s.to_owned());
        }
    }
    impl FeedSink for Recorder {
        fn feed(&self, input: FeedInput<'_>) {
            let tag = match input {
                FeedInput::Snapshot { generation, .. } => format!("snap({generation})"),
                FeedInput::Event(ev) => format!("event({})", ev.kind),
                FeedInput::ControlDown => "down".into(),
                FeedInput::ControlUp => "up".into(),
            };
            self.log(&tag);
        }
    }

    fn host(id: &str, state: &str) -> HostState {
        HostState {
            id: id.into(),
            name: Some(id.into()),
            state: state.into(),
            reason: None,
            link: None,
            stats: None,
            added_at: None,
        }
    }

    fn ev(kind: &str, payload: serde_json::Value) -> EventBody {
        EventBody {
            seq: 1,
            domain: vocab::kind_domain(kind).unwrap().into(),
            kind: kind.into(),
            payload: Some(payload),
        }
    }

    // ---- 用例 --------------------------------------------------------------

    #[test]
    fn control_loop_reconnect_resnapshot_and_backoff_sequence() {
        let rec = Arc::new(Recorder::default());
        let stop = StopFlag::default();
        let sessions = Mutex::new(0u32);
        let dial = move || {
            let n = *sessions.lock().unwrap();
            *sessions.lock().unwrap() += 1;
            Ok(match n {
                0 => Box::new(MockConn::new(
                    "g1",
                    5,
                    vec![host("peer-a", "ready")],
                    vec![ev(vocab::KIND_LINK_CHANGED, json!({"host":"peer-a","via":"direct","ep":"e","rttMs":9,"at":1}))],
                )) as Box<dyn ControlConn>,
                1 => Box::new(MockConn::new("g2", 6, vec![host("peer-b", "ready")], vec![]))
                    as Box<dyn ControlConn>,
                // 第三次拨号失败 ⇒ 走退避；sleep 闭包在首个退避处置位 stop。
                _ => {
                    return Err("daemon down".into());
                }
            })
        };
        {
            let rec_sleep = Arc::clone(&rec);
            let stop = stop.clone();
            let mut sleeps: Vec<Duration> = Vec::new();
            control_loop(
                dial,
                rec.as_ref(),
                &stop,
                &mut |d: Duration| {
                    sleeps.push(d);
                    rec_sleep.log(&format!("sleep({d:?})"));
                    if sleeps.len() == 3 {
                        stop.stop();
                    }
                },
            );
            // 断言 sleep 序列：重连退避 0.5s（确立后重置）→ 0.5s → 1s → 拨号失败 2s? 
            // 时序：会话 1（确立，backoff 重置 init）→ 排空 closed ⇒ down+sleep(0.5)
            // ⇒ 会话 2（确立，重置 init）⇒ 排空 ⇒ down+sleep(0.5) ⇒ 拨号失败 ⇒
            // down+sleep(0.5)〔此刻 stop〕。
            // 时序：会话 1 确立（退避重置 init）→ 排空 closed ⇒ down+sleep(500)
            // ⇒ 会话 2 确立（重置）⇒ 排空 ⇒ down+sleep(500) ⇒ 拨号失败 ⇒
            // down+sleep(1s)〔此刻 stop〕——退避确立即重置、失败倍增（r3 F）。
            assert_eq!(sleeps, vec![
                Duration::from_millis(500),
                Duration::from_millis(500),
                Duration::from_secs(1)
            ]);
        }
        let inputs = rec.inputs.lock().unwrap().clone();
        assert_eq!(
            inputs,
            vec![
                "up", "snap(g1)", "event(link.changed)", "down", "sleep(500ms)", "up",
                "snap(g2)", "down", "sleep(500ms)", "down", "sleep(1s)",
            ],
            "重连 = ControlUp ⇒ 全量重取；断开 = ControlDown 保留行（第三处 down = 拨号失败）"
        );
    }

    #[test]
    fn cursor_stale_redials_immediately_without_down() {
        let rec = Arc::new(Recorder::default());
        let stop = StopFlag::default();
        let n = Mutex::new(0u32);
        let dial = move || {
            let k = *n.lock().unwrap();
            *n.lock().unwrap() += 1;
            Ok(match k {
                0 => Box::new(MockConn::new("g-old", 1, vec![host("peer-a", "ready")], vec![])
                    .with_sub_err(SubErr::CursorStale)) as Box<dyn ControlConn>,
                1 => Box::new(MockConn::new("g-new", 2, vec![host("peer-a", "ready")], vec![]))
                    as Box<dyn ControlConn>,
                _ => {
                    return Err("stop".into());
                }
            })
        };
        let rec_sleep = Arc::clone(&rec);
        control_loop(
            dial,
            rec.as_ref(),
            &stop,
            &mut |d: Duration| {
                rec_sleep.log(&format!("sleep({d:?})"));
                stop.stop();
            },
        );
        let inputs = rec.inputs.lock().unwrap().clone();
        // cursor_stale：连接从未确立 ⇒ 两次 up/snap 之间无 down、无退避
        //（首个 sleep 出现在 conn2 排空之后的正常重连退避，而非 cursor_stale 处）。
        assert_eq!(
            inputs,
            vec!["up", "snap(g-old)", "up", "snap(g-new)", "down", "sleep(500ms)"]
        );
    }

    #[test]
    fn list_poll_cadence_active_and_idle() {
        let stop = StopFlag::default();
        let beats = Arc::new(Mutex::new(0u32));
        let delivered = Mutex::new(0u32);
        let sleep_total = Mutex::new(Duration::ZERO);
        let beats_fetch = Arc::clone(&beats);
        let mut fetch = move || {
            let mut n = beats_fetch.lock().unwrap();
            let b = *n;
            *n += 1;
            drop(n);
            match b {
                0 | 1 => Ok(vec![]),
                _ => Err("daemon gone".into()), // 失败丢拍
            }
        };
        {
            let stop = stop.clone();
            let beats_active = Arc::clone(&beats);
            list_poll_loop(
                &mut fetch,
                &|_v| {
                    *delivered.lock().unwrap() += 1;
                },
                // 拍 0 活动（2s）、拍 1 非活动（10s）、拍 2 活动
                &move || (*beats_active.lock().unwrap()).is_multiple_of(2),
                &stop,
                &mut |d: Duration| {
                    let mut t = sleep_total.lock().unwrap();
                    *t += d;
                    if *t >= Duration::from_secs(12) {
                        stop.stop();
                    }
                },
            );
        }
        assert_eq!(*delivered.lock().unwrap(), 2, "失败拍不投递");
        let total = *sleep_total.lock().unwrap();
        assert!(total >= Duration::from_secs(12), "两拍活动(2s)+一拍非活动(10s) 全部睡满：{total:?}");
    }
}
