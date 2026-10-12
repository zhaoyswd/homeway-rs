//! runtime：app 主循环（tasks 4.6/4.7 + 任务组 3 的 I/O 装配）。
//!
//! 线程面（design D1）：
//! - **控制面循环**（backend::control_loop）：快照初值 + 事件增量 + 断线重拨；
//! - **LIST 轮询循环**（backend::list_poll_loop）：活动态 2s / 非活动 10s；
//! - **选中文件 watch**（300ms）：镜像他实例 + 活动态触发即时重取（r3 D）；
//! - **状态拍**（1s）：本机条目合成（serve.status + sock 可连）+ 流量速率差分；
//! - **腿线程**（选中会话一个）：LegSupervisor 驱动接入/判定/重拨，帧经有界
//!   通道给 UI（背压，tasks 3.7）。
//!
//! UI 主线程：crossterm 事件路由（双焦点，spec 需求 4）→ 模型更新/透传编码 →
//! ratatui 渲染。退出路径全部还原终端（4.7：可捕获退出 + panic hook）。

#![allow(dead_code)] // 少量装配面由集成验收（组 5）与 4.8 消费

use std::io::{self};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{
    self, DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
    EnableFocusChange, EnableMouseCapture, Event, KeyEventKind, MouseButton as CMouse,
    MouseEventKind,
};
use crossterm::cursor::SetCursorStyle;
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use homeway_core::daemon::client::ControlClient;
use homeway_core::daemon::proto::ServeStatusResult;
use homeway_core::daemon::vocab;
use homeway_core::term::frames::{self, Op};

use super::app::{route_key, AppModel, Focus, PanelCmd, Route};
use super::backend::{self, ControlConn, FeedSink, RealConn, StopFlag};
use super::chan::{input_frame, leg_channel, LegInput, LegMsg, RecvHalf, SendHalf};
use super::hosts::FeedInput;
use super::input;
use super::leg::{decode_frame, LegAction, LegEvent, LegSupervisor};
use super::local::{self, LOCAL_HOST_ID};
use super::render;
use super::selection::{SelectionFile, SelectionState};
use super::sessions::SessionListModel;
use super::termview::TermView;
use crate::unified_cli::default_state_dir;

// ---------------------------------------------------------------------------
// 信号面（r4 高4）
// ---------------------------------------------------------------------------

/// 可捕获信号旗标（SIGTERM/SIGHUP/SIGQUIT ⇒ UI 心跳检测后走还原路径）。
static SIGNALLED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_sig: libc::c_int) {
    SIGNALLED.store(true, Ordering::SeqCst);
}

// ---------------------------------------------------------------------------
// 共享状态
// ---------------------------------------------------------------------------

struct Shared {
    app: Mutex<AppModel>,
    sel_file: SelectionFile,
    state_dir: PathBuf,
    /// 活动态判据：最近一次本实例用户输入时刻。
    last_input: Mutex<Option<Instant>>,
    /// 外层终端焦点（DEC 1004；None = 终端不上报——按活动对待，r4 低2）。
    outer_focus: Mutex<Option<bool>>,
    /// 腿世代：选中变化 ⇒ +1；旧腿线程观察到世代不符自行收尾。
    leg_gen: AtomicU64,
    /// 当前腿的收口句柄（r4 F5：换代/退出主动 close——不再依赖对端 REPLACED）。
    leg_closer: Mutex<Option<crate::term_cli::TermCloser>>,
    /// 输入通路（UI → 腿线程 → PTY）：类型化消息经写者线程入 PTY（r4 高2）。
    input_tx: Mutex<Option<std::sync::mpsc::Sender<LegInput>>>,
    /// 当前腿接收半（UI 按世代取走）。
    leg_recv: Mutex<Option<RecvHalf>>,
    /// 腿监督相位（终端 pane 标题显示）。
    leg_phase: Mutex<String>,
    /// 最近渲染的终端区域（RESIZE 的 pane 尺寸来源 + 点击命中测试）。
    term_area: Mutex<Rect>,
    /// 流量速率差分簿记（逐主机，r5 验收批：主机条目第二行都要显示）(rx, tx, at_ms)。
    rates_prev: Mutex<std::collections::HashMap<String, (i64, i64, i64)>>,
    rates: Mutex<std::collections::HashMap<String, (f64, f64)>>,
    /// 恢复一次性哨兵（首个快照后执行持久化选中恢复）。
    restored: AtomicBool,
    /// 终端区尺寸变化待发（画后新 area 落位再发 RESIZE，r4 中4）。
    pending_resize: AtomicBool,
    /// 当前腿的挂接目标（host, session）——状态收敛的对照面（r4 高1）。
    attached: Mutex<Option<(String, String)>>,
}

impl Shared {
    fn new(state_dir: PathBuf) -> Self {
        Shared {
            app: Mutex::new(AppModel::new()),
            sel_file: SelectionFile::new(&state_dir),
            state_dir,
            last_input: Mutex::new(None),
            outer_focus: Mutex::new(None),
            leg_gen: AtomicU64::new(0),
            input_tx: Mutex::new(None),
            leg_recv: Mutex::new(None),
            leg_phase: Mutex::new(String::new()),
            term_area: Mutex::new(Rect::default()),
            rates_prev: Mutex::new(std::collections::HashMap::new()),
            rates: Mutex::new(std::collections::HashMap::new()),
            restored: AtomicBool::new(false),
            pending_resize: AtomicBool::new(false),
            attached: Mutex::new(None),
            leg_closer: Mutex::new(None),
        }
    }

    fn mark_input(&self) {
        *self.last_input.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn active(&self) -> bool {
        // spec 需求 3：最近 30s 有输入，**或**外层终端焦点位于面板（r4 低2 补全）；
        // 终端不上报焦点（1004 未启用/不支持）⇒ 按活动对待（保守快节奏）。
        let outer = *self.outer_focus.lock().unwrap_or_else(|e| e.into_inner());
        if outer == Some(true) {
            return true;
        }
        match self.last_input.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            Some(t) => t.elapsed() < Duration::from_secs(30),
            None => true, // 尚无输入 = 视为活动（首拍快节奏）
        }
    }

    /// 腿世代 +1 + 相位显示（旧腿线程见世代不符自退）。
    fn leg_reset(&self, phase: &str) {
        self.leg_gen.fetch_add(1, Ordering::SeqCst);
        *self.leg_phase.lock().unwrap_or_else(|e| e.into_inner()) = phase.to_owned();
    }

    fn leg_gen_now(&self) -> u64 {
        self.leg_gen.load(Ordering::SeqCst)
    }

    /// 相位显示（**不推世代**——世代由重挂路径专管，r4 F2：监督相位驱动显示）。
    fn set_phase(&self, phase: &str) {
        *self.leg_phase.lock().unwrap_or_else(|e| e.into_inner()) = phase.to_owned();
    }

    /// 用户发起的选中持久化（回落**不**走这里——回落不写回，r3 N-6）。
    fn persist_selection(&self, app: &AppModel) {
        static REV: AtomicU64 = AtomicU64::new(1);
        let rev = REV.fetch_add(1, Ordering::SeqCst);
        let sel = SelectionState {
            revision: rev,
            host_id: app.selected_host.clone(),
            session: app.sessions.selected().map(|s| s.to_owned()),
        };
        // 写失败 = 镜像退化为本机单实例，不影响本机体验。
        let _ = self.sel_file.store(&sel);
    }

    /// 当前选中会话的腿参数：None = 无选中（本机 = 哨兵，远端 = 主机 id）。
    fn leg_target(&self) -> Option<(PathBuf, String, String, u16, u16)> {
        let app = self.app.lock().unwrap_or_else(|e| e.into_inner());
        let host = app.selected_host.clone()?;
        let name = app.sessions.selected()?.to_owned();
        let (cols, rows) = app
            .sessions
            .sessions()
            .iter()
            .find(|s| s.name == name)
            .map(|s| (s.cols, s.rows))
            .unwrap_or((80, 24));
        Some((self.state_dir.clone(), host, name, cols, rows))
    }

    /// 选中主机的链路可用性（判定谓词的 link 信号；本机 = sock 可连）。
    fn link_up(&self, host: &str) -> bool {
        if host == LOCAL_HOST_ID {
            return local::term_sock_reachable(&self.state_dir.join("serve"));
        }
        let app = self.app.lock().unwrap_or_else(|e| e.into_inner());
        app.hosts
            .get(host)
            .map(|h| h.state == "ready" && h.via.is_some())
            .unwrap_or(false)
    }
}

/// 控制面喂入出口：模型更新 + 一次性持久化恢复 + 主机消失回落（可恢复异常留守）。
impl FeedSink for Shared {
    fn feed(&self, input: FeedInput<'_>) {
        let mut app = self.app.lock().unwrap_or_else(|e| e.into_inner());
        let first =
            matches!(&input, FeedInput::Snapshot { .. }) && !self.restored.swap(true, Ordering::SeqCst);
        app.hosts.feed(input);
        if first {
            if let Some(sel) = self.sel_file.load() {
                app.restore(&sel);
            }
        }
        app.on_hosts_updated();
    }
}

// ---------------------------------------------------------------------------
// 腿线程（tasks 3.4a/b 的运行面）
// ---------------------------------------------------------------------------

/// 腿满相位的占用明细文案（spec 需求 5：数量 + 类型构成 + 活跃标记，r4 F2）。
fn vacancy_text(found: Option<&super::sessions::SessionView>) -> String {
    let Some(s) = found else { return "腿满——等待空位".to_owned() };
    if s.clients.is_empty() {
        return format!("腿满（{}/?）——等待空位", s.legs);
    }
    let detail = s
        .clients
        .iter()
        .map(|(kind, active)| if *active { format!("{kind}*") } else { kind.clone() })
        .collect::<Vec<_>>()
        .join(",");
    format!("腿满（{}/{}）——等待空位 · 占用：{}", s.legs, s.clients.len(), detail)
}

/// 监督循环的控制流：继续（外层重走 Precheck）/ 退出（停止或世代更替）。
#[derive(Debug, PartialEq, Eq)]
enum LoopCtl {
    Continue,
    Exit,
}

fn leg_thread(gen: u64, shared: Arc<Shared>, stop: StopFlag, send: SendHalf) {

    let mut sup = LegSupervisor::new();
    let mut last_link_up = false;
    'outer: loop {
        if stop.stopped() || shared.leg_gen_now() != gen {
            return;
        }
        let Some((dir, host, session, mut cols, mut rows)) = shared.leg_target() else {
            return;
        };
        let local = host == LOCAL_HOST_ID;
        // 契约（r4 F1 修复）：list_once 恒传 **state 根**（本机分支内部自己 join serve）；
        // 拨号目录由 attach_and_read 自算（本机面 join serve）。
        let list_arg = if local { LOCAL_HOST_ID.to_owned() } else { host.clone() };
        // 接入前检查：LIST 快照 = 存在性 + 腿满 + 尺寸对齐输入（接入不改几何，r2 N-4）。
        match backend::list_once(&dir, &list_arg) {
            Ok(v) => {
                let found = v.iter().find(|s| s.name == session);
                let exists = found.is_some();
                let legs = found.map(|s| s.legs).unwrap_or(0);
                if let Some(s) = found {
                    cols = s.cols;
                    rows = s.rows;
                }
                let found_view = found.cloned();
                match sup.on(LegEvent::Precheck { exists, legs }) {
                    LegAction::Attach => {
                    }
                    LegAction::WaitVacancy { delay } => {
                        shared.set_phase(&vacancy_text(found_view.as_ref()));
                        let _ = delay;
                        if vacancy_wait(&mut sup, shared.as_ref(), &stop, &dir, &list_arg, &session, delay, gen)
                            == LoopCtl::Exit
                        {
                            return;
                        }
                        continue 'outer; // 空位出现 ⇒ 重走 Precheck（拿最新尺寸）
                    }
                    _ => {
                        // Missing ⇒ 终止（判定①：不重拨）。
                        shared.leg_reset("会话不存在");
                        let _ = send.send(LegMsg::Gone { reason: "会话不存在".into() });
                        return;
                    }
                }
            }
            Err(_) => {
                // 判定拍取不到 = 链路不可达：进判定循环（②）。
                sup.on(LegEvent::StreamEnd { ended: false });
                match judge_after_loss(
                    &mut sup, &shared, &stop, &send, &mut last_link_up, &host,
                    &session, &dir, local, gen,
                ) {
                    LoopCtl::Continue => continue 'outer,
                    LoopCtl::Exit => return,
                }
            }
        }
        match attach_and_read(
            &mut sup, &shared, &stop, &send, &mut last_link_up, &host, &session,
            &dir, local, cols, rows, gen,
        ) {
            LoopCtl::Continue => continue 'outer,
            LoopCtl::Exit => return,
        }
    }
}

/// 接入 + 读帧循环。任何损失 ⇒ 判定循环；判定要重接 ⇒ `Continue`（外层重走
/// Precheck），要退出 ⇒ `Exit`。
#[allow(clippy::too_many_arguments)]
fn attach_and_read(
    sup: &mut LegSupervisor,
    shared: &Arc<Shared>,
    stop: &StopFlag,
    send: &SendHalf,
    last_link_up: &mut bool,
    host: &str,
    session: &str,
    state_root: &std::path::Path,
    local: bool,
    cols: u16,
    rows: u16,
    gen: u64,
) -> LoopCtl {
    let host_ref = (!local).then(|| host.to_owned());
    let dial_dir = if local { state_root.join("serve") } else { state_root.to_owned() };
    let (mut conn, feats) =
        match crate::term_cli::dial_term_conn_impl(&dial_dir, host_ref.as_deref(), true) {
            Ok(x) => {
                x
            }
            Err(e) => {
                let _ = send.send(LegMsg::Gone { reason: format!("拨号失败：{e}") });
                return judge_after_loss(sup, shared, stop, send, last_link_up, host, session, state_root, local, gen);
            }
        };
    if stop.stopped() || shared.leg_gen_now() != gen {
        conn.close();
        return LoopCtl::Exit;
    }
    // split 前取收口句柄（conn 被 split 消费；closer 保留可靠逃生口）。
    let closer = conn.closer();
    // 注册到 Shared：换代/退出路径主动收口（r4 F5）。世代已换代时不注册——
    // 迟到注册会覆盖新腿的 closer（下次换代就会漏关这一条）。
    if shared.leg_gen_now() == gen {
        *shared.leg_closer.lock().unwrap_or_else(|e| e.into_inner()) = Some(closer.duplicate());
    }
    let (mut read_half, mut write_half) = match conn.split() {
        Ok(x) => x,
        Err(e) => {
            closer.close();
            let _ = send.send(LegMsg::Gone { reason: format!("连接分半失败：{e}") });
            return judge_after_loss(sup, shared, stop, send, last_link_up, host, session, state_root, local, gen);
        }
    };
    // HELLO：尺寸对齐 LIST 快照；caps 全零块保 client_id（r3 G）；flags=0 不创建不接管。
    let hello = super::leg::build_hello(session, cols, rows, feats, &crate::term_cli::client_id());
    if let Err(e) = write_half.write_frame(Op::HELLO, &hello) {
        closer.close();
        let _ = send.send(LegMsg::Gone { reason: format!("HELLO：{}", e.message(session)) });
        return judge_after_loss(sup, shared, stop, send, last_link_up, host, session, state_root, local, gen);
    }
    let f = match read_half.read_frame() {
        Ok(f) => f,
        Err(e) => {
            closer.close();
            let _ = send.send(LegMsg::Gone { reason: format!("接入应答：{}", e.message(session)) });
            return judge_after_loss(sup, shared, stop, send, last_link_up, host, session, state_root, local, gen);
        }
    };
    match f.op {
        Op::ATTACHED => {
            if send.send(LegMsg::Frame(frames::encode_frame(f.op, &f.payload))).is_err() {
                closer.close();
                return LoopCtl::Exit;
            }
            // 清相位但**不推世代**（leg_reset 会令本腿自毁——r4 验收期发现）。
            shared.set_phase("");
        }
        Op::ERROR => {
            let (code, msg) = frames::dec_error(&f.payload).unwrap_or_default();
            closer.close();
            // too_many_clients = 服务端权威兜底（r3 B）⇒ 判定循环接管（腿满分支）；
            // 不推世代（线程要在判定循环里等空位）。
            shared.set_phase(&format!("接入被拒（{code}）"));
            let _ = send.send(LegMsg::Gone { reason: format!("接入被拒：{code} {msg}") });
            sup.on(LegEvent::StreamEnd { ended: false });
            return judge_after_loss(sup, shared, stop, send, last_link_up, host, session, state_root, local, gen);
        }
        other => {
            closer.close();
            let _ = send.send(LegMsg::Gone { reason: format!("接入首帧异常 op={other:?}") });
            return judge_after_loss(sup, shared, stop, send, last_link_up, host, session, state_root, local, gen);
        }
    }
    // 输入通路（r4 F4 定稿）：通道**随接入建**——shared 槽替换后 UI 旧 Sender
    // 发送即 Err（旧写者随之收尾），无双消费者；写者持写半，UI 只产类型化消息。
    let (in_tx, in_rx) = std::sync::mpsc::channel::<LegInput>();
    *shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(in_tx);
    {
        let stop_w = stop.clone();
        // 写失败 = 流已死：此前静默 break ⇒ UI 继续把键入送进死写者（「敲命令没
        // 反应」且相位无提示）。现在主动收口令读循环立即退出进入判定重拨。
        let closer_w = closer.duplicate();
        std::thread::spawn(move || {
            for msg in in_rx {
                let (op, payload) = input_frame(&msg);
                if write_half.write_frame(op, &payload).is_err() || stop_w.stopped() {
                    let _ = closer_w.close();
                    break;
                }
            }
        });
    }
    // 读帧循环（帧原样转发；ENDED = 服务端明示终止）。
    loop {
        if stop.stopped() || shared.leg_gen_now() != gen {
            closer.close();
            return LoopCtl::Exit;
        }
        match read_half.read_frame() {
            Ok(f) => {
                let ended = f.op == Op::ENDED;
                if send.send(LegMsg::Frame(frames::encode_frame(f.op, &f.payload))).is_err() {
                    closer.close();
                    return LoopCtl::Exit;
                }
                if ended {
                    sup.on(LegEvent::StreamEnd { ended: true });
                    shared.leg_reset("会话已结束");
                    let _ = send.send(LegMsg::Gone { reason: "会话已结束".into() });
                    return LoopCtl::Exit;
                }
            }
            Err(e) => {
                closer.close();
                let _ = send.send(LegMsg::Gone { reason: e.message(session) });
                sup.on(LegEvent::StreamEnd { ended: false });
                return judge_after_loss(sup, shared, stop, send, last_link_up, host, session, state_root, local, gen);
            }
        }
    }
}

/// 判定循环（谓词四分支 + 退避 + 空位等待）。`Continue` = 要重接（外层重走
/// Precheck），`Exit` = 停止/世代更替/会话终止。
#[allow(clippy::too_many_arguments)]
fn judge_after_loss(
    sup: &mut LegSupervisor,
    shared: &Arc<Shared>,
    stop: &StopFlag,
    send: &SendHalf,
    last_link_up: &mut bool,
    host: &str,
    session: &str,
    state_root: &std::path::Path,
    local: bool,
    gen: u64,
) -> LoopCtl {
    let list_arg = if local { LOCAL_HOST_ID.to_owned() } else { host.to_owned() };
    loop {
        if stop.stopped() || shared.leg_gen_now() != gen {
            return LoopCtl::Exit;
        }
        let link_up_now = shared.link_up(host);
        // 链路恢复事件：退避重置、立即重附（spec：恢复即重置）。
        if !*last_link_up && link_up_now {
            *last_link_up = true;
            if matches!(sup.on(LegEvent::LinkUp), LegAction::Attach) {
                return LoopCtl::Continue;
            }
        }
        *last_link_up = link_up_now;
        let (exists, legs) = match backend::list_once(state_root, &list_arg) {
            Ok(v) => match v.iter().find(|s| s.name == session) {
                Some(s) => (true, s.legs),
                None => (false, 0),
            },
            // 判定拍取不到 LIST = 链路仍不可达（按 exists=true + link 信号判定）。
            Err(_) => (true, 0),
        };
        match sup.on(LegEvent::Judge { exists, legs, link_up: link_up_now }) {
            LegAction::Stopped => {
                shared.leg_reset("会话已结束");
                let _ = send.send(LegMsg::Gone { reason: "会话已结束".into() });
                return LoopCtl::Exit;
            }
            LegAction::Redial { delay } => {
                let mut left = delay;
                while left > Duration::ZERO && !stop.stopped() && shared.leg_gen_now() == gen {
                    let step = left.min(Duration::from_millis(100));
                    std::thread::sleep(step);
                    left = left.saturating_sub(step);
                }
                // 落回循环顶：重取链路/列表信号（恢复即重置由 LinkUp 分支承接）。
            }
            LegAction::WaitVacancy { delay } => {
                let mut left = delay;
                while left > Duration::ZERO && !stop.stopped() && shared.leg_gen_now() == gen {
                    let step = left.min(Duration::from_millis(500));
                    std::thread::sleep(step);
                    left = left.saturating_sub(step);
                }
                if let Ok(v) = backend::list_once(state_root, &list_arg) {
                    let legs = v.iter().find(|s| s.name == session).map(|s| s.legs).unwrap_or(0);
                    if matches!(sup.on(LegEvent::VacancyPoll { legs }), LegAction::Attach) {
                        return LoopCtl::Continue;
                    }
                }
            }
            _ => return LoopCtl::Continue,
        }
    }
}

/// 空位等待（Precheck::Full 入口）。
#[allow(clippy::too_many_arguments)]
fn vacancy_wait(
    sup: &mut LegSupervisor,
    shared: &Shared,
    stop: &StopFlag,
    state_root: &std::path::Path,
    list_arg: &str,
    session: &str,
    delay: Duration,
    gen: u64,
) -> LoopCtl {
    let mut left = delay;
    while left > Duration::ZERO && !stop.stopped() && shared.leg_gen_now() == gen {
        let step = left.min(Duration::from_millis(500));
        std::thread::sleep(step);
        left = left.saturating_sub(step);
    }
    if stop.stopped() || shared.leg_gen_now() != gen {
        return LoopCtl::Exit;
    }
    if let Ok(v) = backend::list_once(state_root, list_arg) {
        let legs = v.iter().find(|s| s.name == session).map(|s| s.legs).unwrap_or(0);
        if matches!(sup.on(LegEvent::VacancyPoll { legs }), LegAction::Attach) {
            return LoopCtl::Continue;
        }
    }
    LoopCtl::Continue
}

// ---------------------------------------------------------------------------
// 面板入口与线程装配
// ---------------------------------------------------------------------------

/// `homeway tui` 面板主入口（cmd_tui 调用）。返回进程退出码。
pub(crate) fn run(args: &[String]) -> i32 {
    let mut state_dir = default_state_dir();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--state" {
            match it.next() {
                Some(d) => state_dir = PathBuf::from(d),
                None => {
                    eprintln!("homeway tui：--state 需要目录参数");
                    return 2;
                }
            }
        } else if let Some(d) = a.strip_prefix("--state=") {
            state_dir = PathBuf::from(d);
        } else {
            eprintln!("homeway tui：不认识的参数 {a:?}（--help 查看用法）");
            return 2;
        }
    }

    // 4.7 终端环境边界：非 TTY / TERM 缺失或 dumb ⇒ 明确错误 + 非零退出。
    if unsafe { libc::isatty(0) } == 0 {
        eprintln!("homeway tui：需要交互终端（stdin 不是 TTY）");
        return 2;
    }
    let term = std::env::var("TERM").unwrap_or_default();
    if term.is_empty() || term == "dumb" {
        eprintln!("homeway tui：终端能力不足（TERM={term:?}）；请在支持 ANSI 的终端运行");
        return 2;
    }

    // 启动拨号（spec 需求 1：未运行按需拉起；失败 = 可行动错误 + 非零退出）。
    let client = match backend::dial_control(&state_dir) {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "homeway tui：控制面不可用——{e}\n（运行 homeway status 检查；面板启动需要 daemon 在位）"
            );
            return 1;
        }
    };

    let shared = Arc::new(Shared::new(state_dir.clone()));
    let stop = StopFlag::default();
    // 4.7 可捕获信号：TERM/HUP/QUIT 不再默认杀死进程——旗标 + UI 心跳还原后退出。
    unsafe {
        let h = on_signal as extern "C" fn(libc::c_int) as usize;
        libc::signal(libc::SIGTERM, h);
        libc::signal(libc::SIGHUP, h);
        libc::signal(libc::SIGQUIT, h);
    }
    // panic hook：任何 panic 路径还原终端（4.7）。
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            terminal::LeaveAlternateScreen,
            DisableMouseCapture,
            SetCursorStyle::DefaultUserShape,
            crossterm::style::ResetColor
        );
        prev_hook(info);
    }));

    // 控制面循环（首会话复用启动拨号；重连自拨）。
    {
        let shared = Arc::clone(&shared);
        let stop = stop.clone();
        let dir = state_dir.clone();
        let first = Mutex::new(Some(client));
        std::thread::spawn(move || {
            let first = first;
            backend::control_loop(
                move || {
                    if let Some(c) = first.lock().unwrap_or_else(|e| e.into_inner()).take() {
                        return Ok(Box::new(RealConn::new(c)) as Box<dyn ControlConn>);
                    }
                    backend::dial_control(&dir)
                        .map(|c| Box::new(RealConn::new(c)) as Box<dyn ControlConn>)
                },
                shared.as_ref(),
                &stop,
                &mut |d| std::thread::sleep(d),
            );
        });
    }
    // LIST 轮询循环。
    {
        let shared = Arc::clone(&shared);
        let stop = stop.clone();
        std::thread::spawn(move || {
            backend::list_poll_loop(
                || {
                    let (dir, host) = {
                        let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                        (shared.state_dir.clone(), app.selected_host.clone())
                    };
                    match host {
                        Some(h) => backend::list_once(&dir, &h),
                        None => Err("未选中主机".into()),
                    }
                },
                &|v| {
                    let mut app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                    app.sessions.replace(v.to_vec());
                    app.fallback_on_sessions();
                },
                &|| shared.active(),
                &stop,
                &mut |d| std::thread::sleep(d),
            );
        });
    }
    // 选中文件 watch（镜像 + 活动态触发即时重取 + 重挂腿，r3 D）。
    {
        let shared = Arc::clone(&shared);
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.stopped() {
                if let Some(sel) = shared.sel_file.poll() {
                    {
                        let mut app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                        app.restore(&sel);
                    }
                    list_beat(&shared);
                    // 重挂腿由 UI 收敛块接管（镜像只改模型，r4 高1）。
                }
                std::thread::sleep(Duration::from_millis(300));
            }
        });
    }
    // 状态拍（1s）：本机条目 + 流量速率差分。
    {
        let shared = Arc::clone(&shared);
        let stop = stop.clone();
        std::thread::spawn(move || {
            // r4 中6：控制面连接**复用**（此前每秒新建一条——污染 5.5 数值门）。
            let mut conn: Option<Arc<ControlClient>> = None;
            while !stop.stopped() {
                let (serve_state, sock_ok) = match serve_state_of(&mut conn, &shared.state_dir) {
                    Some(s) => (s, local::term_sock_reachable(&shared.state_dir.join("serve"))),
                    None => (local::ServeState::Absent, false),
                };
                {
                    let mut app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                    super::app::refresh_local(&mut app, &local::hostname(), serve_state, sock_ok);
                    app.on_hosts_updated();
                }
                let now = now_ms();
                // 逐主机差分（r5 验收批：主机条目第二行各自显示 ↑↓ 速率）。
                let cur: std::collections::HashMap<String, (i64, i64, i64)> = {
                    let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                    app.display_hosts()
                        .iter()
                        .filter_map(|h| {
                            h.rx_bytes.zip(h.tx_bytes).map(|(rx, tx)| (h.id.clone(), (rx, tx, now)))
                        })
                        .collect()
                };
                {
                    let mut prev = shared.rates_prev.lock().unwrap_or_else(|e| e.into_inner());
                    let mut rates = shared.rates.lock().unwrap_or_else(|e| e.into_inner());
                    for (id, c) in &cur {
                        if let Some(r) = AppModel::rate_of(prev.get(id).copied(), *c) {
                            rates.insert(id.clone(), r);
                        }
                    }
                    // 消失的主机（快照重取/删除）不再残留旧读数。
                    rates.retain(|id, _| cur.contains_key(id));
                    prev.retain(|id, _| cur.contains_key(id));
                    *prev = cur;
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        });
    }

    let code = ui_loop(&shared, &stop);
    stop.stop();
    code
}

/// serve.status → ServeState（None = 不可达；连接**复用**，失败重拨——r4 中6）。
fn serve_state_of(
    conn: &mut Option<Arc<ControlClient>>,
    state_dir: &std::path::Path,
) -> Option<local::ServeState> {
    if conn.is_none() {
        let sock = state_dir.join("control.sock");
        *conn = ControlClient::dial(&sock, "cli", "homeway-tui-beat").ok().map(|(c, _)| c);
    }
    let c = conn.as_ref()?;
    match c.request(vocab::OpName::ServeStatus.as_str(), None, Duration::from_secs(5)) {
        Ok(v) => {
            let st: ServeStatusResult = serde_json::from_value(v).ok()?;
            local::ServeState::from_wire(&st.state)
        }
        Err(_) => {
            *conn = None; // 断线/失效 ⇒ 下拍重拨
            None
        }
    }
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn list_beat(shared: &Shared) {
    let (dir, host) = {
        let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
        (shared.state_dir.clone(), app.selected_host.clone())
    };
    let Some(host) = host else { return };
    if let Ok(mut v) = backend::list_once(&dir, &host) {
        super::sessions::display_order(&mut v);
        let mut app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
        app.sessions.replace(v);
        app.fallback_on_sessions();
    }
}

/// 腿（重）挂：世代自增令旧线程自退 + 换通道 + 起腿线程。
fn spawn_leg(shared: &Arc<Shared>, stop: &StopFlag) {
    // 换代前先**主动收口上一条腿**（r5 实录泄漏修复）：旧腿读循环阻塞在
    // read_frame（空闲会话无帧）⇒ 世代检查永远跑不到；shared 槽里的旧 closer
    // 若只被覆盖不 close（duplicate 句柄 Drop 不关连接）＝空闲腿永久泄漏。
    if let Some(prev) = shared.leg_closer.lock().unwrap_or_else(|e| e.into_inner()).take() {
        prev.close();
    }
    shared.leg_reset("接入中…");
    let gen = shared.leg_gen_now();
    let (frame_send, frame_recv) = leg_channel(64);
    *shared.leg_recv.lock().unwrap_or_else(|e| e.into_inner()) = Some(frame_recv);
    let shared2 = Arc::clone(shared);
    let stop2 = stop.clone();
    std::thread::spawn(move || leg_thread(gen, shared2, stop2, frame_send));
}

// ---------------------------------------------------------------------------
// UI 主循环
// ---------------------------------------------------------------------------

/// UI 主循环。返回退出码。
fn ui_loop(shared: &Arc<Shared>, stop: &StopFlag) -> i32 {
    let mut leg_recv: Option<RecvHalf> = None;
    let mut termview: Option<TermView> = None;
    let mut ui_gen = shared.leg_gen_now();
    // 宿主光标样式切换沿（终端焦点 ↔ 面板）——DECSCUSR 只在沿上写。
    let mut last_focus: Option<Focus> = None;

    let mut out = io::stdout();
    if execute!(out, terminal::EnterAlternateScreen).is_err() {
        eprintln!("homeway tui：终端进入备用屏失败（能力不足）");
        return 2;
    }
    if terminal::enable_raw_mode().is_err() {
        let _ = execute!(out, terminal::LeaveAlternateScreen);
        eprintln!("homeway tui：终端 raw 模式启用失败");
        return 2;
    }
    // 输入协议面。鼠标捕获 = 「独占」的另一半（r5 验收实测）：不开则滚轮事件留在
    // 宿主终端——备用屏下 iTerm2/Terminal.app 照样滚 scrollback，把进面板前的历史
    // 滚出来；开了宿主才把滚轮/点击交给本面板（回看/点选路径 4.4b 早已就绪）。
    if execute!(out, EnableBracketedPaste, EnableFocusChange, EnableMouseCapture).is_err() {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(out, terminal::LeaveAlternateScreen, crossterm::style::ResetColor);
        eprintln!("homeway tui：终端输入模式启用失败");
        return 2;
    }
    let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut t = match Terminal::new(CrosstermBackend::new(io::stdout())) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("homeway tui：终端初始化失败：{e}");
                return 2;
            }
        };
        // 输入泵线程（r5 实录修复）：crossterm 0.29 的 try_read 在 tty EOF（master
        // 关闭/锁屏唤醒 fd 静默死亡——macOS 实证 slave 恒「可读」且 read 恒 Ok(0)）
        // 时内圈不 break，poll 里 100% CPU 自旋、主循环永困其中（stop 旗标都不再看）。
        // 读独立线程 + 有界通道：主循环永不困在 crossterm 里；EOF 自旋由看门狗识别。
        enum Pump {
            Ev(Event),
            Lost,
        }
        let (ev_tx, ev_rx) = std::sync::mpsc::sync_channel::<Pump>(256);
        let _ = std::thread::Builder::new()
            .name("tui-input".into())
            .spawn(move || loop {
                match event::poll(Duration::from_millis(200)) {
                    Ok(true) => match event::read() {
                        Ok(ev) => {
                            if ev_tx.send(Pump::Ev(ev)).is_err() {
                                return;
                            }
                        }
                        Err(_) => {
                            let _ = ev_tx.send(Pump::Lost);
                            return;
                        }
                    },
                    Ok(false) => {}
                    Err(_) => {
                        let _ = ev_tx.send(Pump::Lost);
                        return;
                    }
                }
            });
        let mut stuck_ticks = 0u32;
        let exit_code = loop {
            match ev_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Pump::Ev(ev)) => {
                    stuck_ticks = 0;
                    shared.mark_input();
                    if !handle_event(shared, stop, &mut termview, ev) {
                        break 0i32;
                    }
                }
                Ok(Pump::Lost) => {
                    eprintln!("homeway tui：终端输入面失联（EOF/IO 错误）");
                    break 2;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // 看门狗：fd 恒「可读」而泵 1s 零产出 ⇒ 泵已陷 crossterm EOF 自旋
                    // （tty 已死——正常路径下字节会被泵立刻吃掉，不会留在线程 fd 上）。
                    let mut fds = [libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 }];
                    let readable =
                        unsafe { libc::poll(fds.as_mut_ptr(), 1, 0) } > 0 && fds[0].revents & libc::POLLIN != 0;
                    stuck_ticks = if readable { stuck_ticks.saturating_add(1) } else { 0 };
                    if stuck_ticks >= 10 {
                        eprintln!("homeway tui：终端输入面失联（EOF 看门狗）");
                        break 2;
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break 2,
            }
            // —— 渲染心跳以下与事件处理同拍（每 ≤100ms 一轮）——
            if !ui_tick(shared, stop, &mut t, &mut termview, &mut leg_recv, &mut ui_gen, &mut last_focus) {
                break 0;
            }
            if stop.stopped() {
                break 0;
            }
        };
        exit_code
    }));
    // 4.7：任何可捕获退出路径还原终端。
    let code = code.unwrap_or(2);
    let _ = terminal::disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        terminal::LeaveAlternateScreen,
        DisableBracketedPaste,
        DisableFocusChange,
        DisableMouseCapture,
        SetCursorStyle::DefaultUserShape,
        crossterm::style::ResetColor
    );
    code
}

/// UI 事件分派（输入泵通道消费侧；r5 EOF 自旋修复时从主循环抽出）。
/// 返回 false = 退出（q / 终端失联路径外的主动退出）。
fn handle_event(shared: &Arc<Shared>, stop: &StopFlag, termview: &mut Option<TermView>, ev: Event) -> bool {
    match ev {
        Event::Key(k) => {
            let focus = shared.app.lock().unwrap_or_else(|e| e.into_inner()).focus;
            match route_key(&k, focus) {
                Route::Passthrough => {
                    // RESIZE 随窗口跟随（r5 实录）：首次交互必发（仅交互实例，镜像
                    // 实例无键入永不发——r2 N-4）；其后 pane 尺寸 ≠ 已发尺寸即重发
                    // （覆盖「面板焦点改窗 → 回同一会话」——世代未变、interacted 已
                    // 置位，此前永远不再发 ⇒ 会话几何冻结在旧尺寸）。
                    if let Some(tv) = termview.as_mut() {
                        let tx = shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
                        if let Some(tx) = tx {
                            let a = *shared.term_area.lock().unwrap_or_else(|e| e.into_inner());
                            let size = (a.width, a.height);
                            let fresh = !tv.interacted;
                            let follow = tv.interacted
                                && tv.last_sent != Some(size)
                                && a.width > 0
                                && a.height > 0;
                            if fresh {
                                tv.interacted = true;
                            }
                            if (fresh || follow) && a.width > 0 && a.height > 0 {
                                let _ = tx.send(LegInput::Resize(a.width, a.height));
                                tv.last_sent = Some(size);
                            }
                            if k.kind == KeyEventKind::Press || k.kind == KeyEventKind::Repeat {
                                if let Some(bytes) = input::encode_key_event(&k, tv.leg.vt()) {
                                    let _ = tx.send(LegInput::Data(bytes));
                                }
                            }
                        }
                    }
                }
                Route::Panel(cmd) => {
                    // (quit, changed) 双旗——quit 仅 Quit 命令；apply_panel 的返回是
                    // 「选中已变化」（bb8f90e 曾误当 quit ⇒ 任意改选中的键直接退出）。
                    let (quit, changed) = {
                        let mut app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                        match cmd {
                            PanelCmd::Quit => (true, false),
                            other => (false, app.apply_panel(other)),
                        }
                    };
                    if quit {
                        stop.stop();
                        return false;
                    }
                    // 用户发起 ⇒ 持久化 + 立即重取列表（重挂腿由收敛块接管，r4 高1）。
                    if changed {
                        {
                            let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                            shared.persist_selection(&app);
                        }
                        list_beat(shared);
                    }
                    // 进入终端焦点：交互实例把当前 pane 尺寸同步给会话（r5 实录——
                    // 面板焦点期间改窗口后 Enter，此前无任何路径补发 RESIZE）。
                    if shared.app.lock().unwrap_or_else(|e| e.into_inner()).focus == Focus::Terminal {
                        if let Some(tv) = termview.as_mut() {
                            let a = *shared.term_area.lock().unwrap_or_else(|e| e.into_inner());
                            let size = (a.width, a.height);
                            if tv.interacted && tv.last_sent != Some(size) && a.width > 0 && a.height > 0 {
                                if let Some(tx) = shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()).clone() {
                                    let _ = tx.send(LegInput::Resize(a.width, a.height));
                                    tv.last_sent = Some(size);
                                }
                            }
                        }
                    }
                }
            }
        }
        Event::Mouse(m) => {
            // 鼠标归属：终端焦点 + 会话启用上报 ⇒ 编码转发；否则面板点击。
            let (focus, tracking) = {
                let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                (app.focus, termview.as_ref().map(|tv| tv.leg.modes().mouse_tracking()).unwrap_or(false))
            };
            if focus == Focus::Terminal && tracking {
                if let Some(tv) = termview.as_mut() {
                    let tx = shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let pane = *shared.term_area.lock().unwrap_or_else(|e| e.into_inner());
                    if let (Some(tx), Some(ev)) = (tx, input::encode_mouse_event(&m, pane, tv.leg.vt())) {
                        let _ = tx.send(LegInput::Data(ev));
                    }
                }
            } else if focus == Focus::Terminal
                && matches!(m.kind, MouseEventKind::ScrollUp | MouseEventKind::ScrollDown)
            {
                // 滚轮回看（tasks 4.8）：会话未启用上报 ⇒ 面板内回看（spec 需求 4）。
                if let Some(tv) = termview.as_mut() {
                    match m.kind {
                        MouseEventKind::ScrollUp => tv.leg.scroll_display(3),
                        _ => tv.leg.scroll_display(-3),
                    }
                }
            } else if let MouseEventKind::Down(CMouse::Left) = m.kind {
                if click_select(shared, m.column, m.row) {
                    {
                        let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
                        shared.persist_selection(&app);
                    }
                    list_beat(shared);
                }
            }
        }
        Event::Paste(text) => {
            let focus = shared.app.lock().unwrap_or_else(|e| e.into_inner()).focus;
            if focus == Focus::Terminal {
                if let Some(tv) = termview.as_mut() {
                    let tx = shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let Some(tx) = tx {
                        for bytes in input::encode_paste_fragments(&text, tv.leg.vt()).unwrap_or_default() {
                            let _ = tx.send(LegInput::Data(bytes));
                        }
                    }
                }
            }
        }
        Event::FocusGained | Event::FocusLost => {
            // 外层 1004：活动态判据 + 终端焦点下按会话位注入（design D7）。
            let gained = matches!(ev, Event::FocusGained);
            *shared.outer_focus.lock().unwrap_or_else(|e| e.into_inner()) = Some(gained);
            let focus = shared.app.lock().unwrap_or_else(|e| e.into_inner()).focus;
            if focus == Focus::Terminal {
                if let Some(tv) = termview.as_mut() {
                    let tx = shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    if let (Some(tx), Some(bytes)) = (tx, input::encode_focus(gained, tv.leg.vt())) {
                        let _ = tx.send(LegInput::Data(bytes));
                    }
                }
            }
        }
        Event::Resize(_, _) => {
            // r4 中4：pane 尺寸变化不再丢弃——置旗，画后（新 area 落位）下发。
            if { shared.app.lock().unwrap_or_else(|e| e.into_inner()).focus } == Focus::Terminal {
                shared.pending_resize.store(true, Ordering::SeqCst);
            }
        }
    }
    true
}

/// 渲染心跳拍（每 ≤100ms 一轮）：挂腿收敛 / 世代接管 / 帧合流 / 焦点与光标样式 /
/// 绘制 / RESIZE 待发。返回 false = draw 失败（终端面坏）⇒ 退出。
#[allow(clippy::too_many_arguments)]
fn ui_tick(
    shared: &Arc<Shared>,
    stop: &StopFlag,
    t: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    termview: &mut Option<TermView>,
    leg_recv: &mut Option<RecvHalf>,
    ui_gen: &mut u64,
    last_focus: &mut Option<Focus>,
) -> bool {
    // 挂腿状态收敛（r4 高1）：期望目标 ≠ 已挂目标 ⇒ 重挂——覆盖冷启动、
    // 恢复、回落链换目标、镜像全部路径（不再依赖「选中变化」事件触发）。
    let desired = {
        let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
        app.selected_host
            .clone()
            .zip(app.sessions.selected().map(|s| s.to_owned()))
    };
    {
        let mut attached = shared.attached.lock().unwrap_or_else(|e| e.into_inner());
        if *attached != desired {
            if desired.is_some() {
                spawn_leg(shared, stop);
            } else {
                shared.leg_reset("未选中会话");
            }
            *attached = desired;
        }
    }
    // 腿世代推进（用户切换/镜像）⇒ 接手新通道 + 重建终端视图。
    let gen = shared.leg_gen_now();
    if gen != *ui_gen {
        *ui_gen = gen;
        if let Some(r) = shared.leg_recv.lock().unwrap_or_else(|e| e.into_inner()).take() {
            *leg_recv = Some(r);
        }
        let dims = {
            let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
            app.sessions
                .sessions()
                .iter()
                .find(|s| Some(s.name.as_str()) == app.sessions.selected())
                .map(|s| (s.cols, s.rows))
                .unwrap_or((80, 24))
        };
        *termview = TermView::new(dims.0, dims.1).ok();
    }
    // 腿帧 drain → 内嵌模拟器（合帧：丢渲染拍不丢字节）。
    if let (Some(tv), Some(rh)) = (termview.as_mut(), leg_recv.as_ref()) {
        for msg in rh.drain() {
            match msg {
                LegMsg::Frame(b) => {
                    if let Some(f) = decode_frame(&b) {
                        tv.leg.on_frame(&f);
                    }
                }
                LegMsg::Gone { reason } => {
                    // r4 中3：终结原因消费到相位显示（此前十处写零消费）。
                    *shared.leg_phase.lock().unwrap_or_else(|e| e.into_inner()) = reason;
                }
            }
        }
    }
    // 会话焦点跟随面板焦点切换（r4 高3：Enter=复得 / F12=失焦，去重）。
    let focus_now = shared.app.lock().unwrap_or_else(|e| e.into_inner()).focus;
    if focus_now == Focus::Terminal {
        push_focus(shared, termview, true);
    } else {
        push_focus(shared, termview, false);
    }
    // 宿主光标样式随焦点切换（r5 验收批：终端焦点 = 闪烁块；离开/退出还原
    // 缺省形状。DECSCUSR 只在切换沿写一次，不逐帧写）。
    if Some(focus_now) != *last_focus {
        let _ = match focus_now {
            Focus::Terminal => execute!(io::stdout(), SetCursorStyle::BlinkingBlock),
            _ => execute!(io::stdout(), SetCursorStyle::DefaultUserShape),
        };
        *last_focus = Some(focus_now);
    }
    // 渲染。
    let now = now_ms();
    let (status, rates) = {
        let app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
        (render::status_line(&app), shared.rates.lock().unwrap_or_else(|e| e.into_inner()).clone())
    };
    let phase = shared.leg_phase.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let mut status = status;
    if !phase.is_empty() {
        status.push_str(&format!(" · {phase}"));
    }
    let off = termview.as_ref().map(|tv| tv.leg.display_offset()).unwrap_or(0);
    if off > 0 {
        status.push_str(&format!(" · 回看中({off})"));
    }
    let area_store = &shared.term_area;
    let app_ref = &mut *shared.app.lock().unwrap_or_else(|e| e.into_inner());
    let tv = termview.as_mut();
    if t
        .draw(|f| {
            render::draw(f, app_ref, tv, &status, &rates, now);
            *area_store.lock().unwrap_or_else(|e| e.into_inner()) = render::layout(f.area()).2;
        })
        .is_err()
    {
        return false;
    }
    // 尺寸变化待发：新 area 落位后下发 RESIZE（r4 中4）。
    if shared.pending_resize.swap(false, Ordering::SeqCst) {
        if let Some(tx) = shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()).clone() {
            let a = *shared.term_area.lock().unwrap_or_else(|e| e.into_inner());
            if a.width > 0 && a.height > 0 {
                let _ = tx.send(LegInput::Resize(a.width, a.height));
                if let Some(tv) = termview.as_mut() {
                    tv.last_sent = Some((a.width, a.height));
                }
            }
        }
    }
    true
}

/// 会话焦点注入（r4 高3）：按会话 1004 位编码，**按最后注入态去重**——
/// 面板焦点切换（Enter/Exit）与外层焦点事件共用，避免双发。
fn push_focus(shared: &Shared, termview: &mut Option<TermView>, gained: bool) {
    let Some(tv) = termview.as_mut() else { return };
    if tv.session_focus == Some(gained) {
        return;
    }
    let tx = shared.input_tx.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(tx) = tx else { return };
    match input::encode_focus(gained, tv.leg.vt()) {
        Some(bytes) => {
            tv.session_focus = Some(gained);
            let _ = tx.send(LegInput::Data(bytes));
        }
        None => {
            tv.session_focus = None; // 会话未启用 1004：无可注入（注入态置未知）
        }
    }
}

/// 面板点击选中（命中测试：主机区/会话区行）。返回是否改变选中。
fn click_select(shared: &Arc<Shared>, col: u16, row: u16) -> bool {
    let area = *shared.term_area.lock().unwrap_or_else(|e| e.into_inner());
    let (hosts_a, sess_a, _, _) = render::layout(area);
    let mut app = shared.app.lock().unwrap_or_else(|e| e.into_inner());
    if hosts_a.contains((col, row).into()) {
        // 两行条目（r5 验收批）：● 名称行 + 副标题行 ⇒ 行号除 2 得条目下标。
        let idx = (row.saturating_sub(hosts_a.y + 1) / 2) as usize;
        let hosts = app.display_hosts();
        if let Some(h) = hosts.get(idx) {
            if app.selected_host.as_deref() != Some(h.id.as_str()) {
                app.selected_host = Some(h.id.clone());
                app.host_cursor = idx;
                app.sessions = SessionListModel::default();
                app.session_cursor = 0;
                return true;
            }
        }
        return false;
    }
    if sess_a.contains((col, row).into()) {
        let idx = row.saturating_sub(sess_a.y + 1) as usize;
        app.session_cursor = idx;
        if let Some(s) = app.sessions.sessions().get(idx) {
            let name = s.name.clone();
            return app.sessions.select(&name);
        }
    }
    false
}
