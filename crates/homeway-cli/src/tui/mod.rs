//! `homeway tui`：既有 daemon/term 的交互客户端。
mod appearance;
mod backend;
mod clipboard;
mod input;
mod surface;
mod terminal;
mod transport;
mod view;

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use homeway_core::term::{
    codec,
    frames::{self, Op},
};
use ratatui::{backend::CrosstermBackend, Terminal};

use backend::{Backend, Command, Target};
use view::{Action, Model};

const CONNECTING_NOTICE: &str = "正在连接本机服务…";
const READING_SESSIONS_NOTICE: &str = "正在读取主机会话…";
const CONTROL_READY_NOTICE: &str = "本机服务已连接 · 可选择或创建会话";
const SESSIONS_READY_NOTICE: &str = "已就绪 · 选择会话，或点击 + 创建 shell";

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Backend(#[from] backend::Error),
    #[error("tui 需要交互式终端（stdin/stdout 均须为 TTY）")]
    NotTerminal,
    #[error("未知参数 {0}；用 homeway tui --help 查看用法")]
    Argument(String),
    #[error("{0} 后缺少参数")]
    MissingArgument(String),
}

struct Options {
    state: PathBuf,
}

fn options(args: &[String]) -> Result<Option<Options>, Error> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("用法：homeway tui [--state DIR]\n\n鼠标：左键选择；右键管理主机/会话；标题栏 + 添加。\n键盘：Ctrl+B 后 h/s/Enter 选主机/会话/终端；\n      Ctrl+B 后 a/n 添加主机/创建会话；m 操作菜单；q 分离退出。\n      侧栏中 F6 切焦点、F10 退出；Ctrl+B Ctrl+B 发送 Ctrl+B。\n历史：滚轮浏览；应用捕获鼠标时用 Shift+滚轮；Shift+PageUp/PageDown/Home/End。\n主题随终端背景自动识别。会话中的普通按键交给终端；退出后会话继续运行。");
        return Ok(None);
    }
    let mut parsed = Options {
        state: crate::unified_cli::default_state_dir(),
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--state" => {
                parsed.state = PathBuf::from(
                    iter.next()
                        .ok_or_else(|| Error::MissingArgument(arg.clone()))?,
                )
            }
            _ => return Err(Error::Argument(arg.clone())),
        }
    }
    Ok(Some(parsed))
}

pub(crate) fn cmd_tui(args: &[String]) {
    let result = options(args).and_then(|options| match options {
        Some(options) => run(options),
        None => Ok(()),
    });
    if let Err(error) = result {
        eprintln!("homeway tui：{error}");
        std::process::exit(1);
    }
}

fn run(options: Options) -> Result<(), Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::NotTerminal);
    }
    terminal::install_panic_hook();
    let _guard = terminal::TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new(options);
    let mut appearance =
        appearance::Appearance::new(Instant::now(), std::env::var("COLORFGBG").ok().as_deref());
    app.model.theme = appearance.theme();
    let mut pending_events = VecDeque::new();
    let mut geometry = view::Geometry::default();
    while !terminal::stopping() {
        app.process_backend();
        app.process_attach();
        app.flush_input();
        app.process_clipboard()?;
        app.poll_refresh();
        let now = Instant::now();
        if appearance.query_due(now) {
            io::stdout().write_all(appearance::QUERY)?;
            io::stdout().flush()?;
        }
        pending_events.extend(appearance.expire(now));
        if app.model.theme != appearance.theme() {
            app.model.theme = appearance.theme();
            app.render_dirty = true;
        }
        if app.render_dirty {
            if let Some(surface) = &mut app.surface {
                surface.prepare(app.size.1);
            }
            terminal.draw(|frame| {
                geometry = view::draw(
                    frame,
                    &mut app.model,
                    app.surface.as_ref().map(surface::Surface::view),
                );
            })?;
            app.render_dirty = false;
        }
        app.resize(geometry.terminal.width, geometry.terminal.height);
        app.poll_surface();
        // 输出每拍有预算；无论远端持续输出还是失联，输入/退出每 25ms 都有机会处理。
        if pending_events.is_empty() && event::poll(Duration::from_millis(25))? {
            pending_events.extend(appearance.filter(event::read()?, Instant::now()));
        }
        if let Some(event) = pending_events.pop_front() {
            if matches!(event, Event::FocusGained) {
                appearance.focus_gained(Instant::now());
            }
            app.render_dirty = true;
            let focused = app.model.focus == view::Focus::Terminal && !app.model.modal_open();
            if let Event::FocusGained | Event::FocusLost = event {
                if app
                    .surface
                    .as_ref()
                    .is_some_and(surface::Surface::focus_events)
                {
                    app.send_input(frames::InputEvent::Focus {
                        gained: matches!(event, Event::FocusGained),
                    });
                }
            }
            let action = view::handle_event(&mut app.model, event, &geometry);
            if app.action(action)? {
                break;
            }
            let focus_now = app.model.focus == view::Focus::Terminal && !app.model.modal_open();
            if focused != focus_now
                && app
                    .surface
                    .as_ref()
                    .is_some_and(surface::Surface::focus_events)
            {
                app.send_input(frames::InputEvent::Focus { gained: focus_now });
            }
        }
    }
    // App drop 只关闭 attach 连接，不发送 KILL；guard 最后恢复宿主终端。
    Ok(())
}

struct App {
    render_dirty: bool,
    model: Model,
    state_dir: PathBuf,
    backend: Backend,
    clipboard: clipboard::Clipboard,
    snapshot: backend::Snapshot,
    current_target: Target,
    active_session: Option<String>,
    create_pending: bool,
    operation_fact: Option<OperationFact>,
    operation_id: u64,
    operation_warning: Option<(backend::Confirmation, String)>,
    attach: Option<backend::Attach>,
    surface: Option<surface::Surface>,
    fragments: [codec::FragAssembler; 3],
    snapshot_request: Option<Instant>,
    generation: u64,
    connected: bool,
    input_queue: VecDeque<Vec<u8>>,
    input_bytes: usize,
    mouse_pressed: Vec<crossterm::event::MouseButton>,
    input_geometry: Option<InputGeometry>,
    size: (u16, u16),
    sent_size: Option<(u16, u16)>,
    sent_theme: Option<view::Theme>,
    refresh_pending: bool,
    sessions_pending: bool,
    sessions_epoch: u64,
    navigation_epoch: u64,
    next_refresh: Instant,
}

struct OperationFact {
    text: String,
    confirmation: backend::Confirmation,
}

struct InputGeometry {
    size: (u16, u16),
    requested: Option<Instant>,
}

impl App {
    fn new(options: Options) -> Self {
        let mut model = Model::default();
        model.hosts.push(local_host(false, false));
        model.notice = CONNECTING_NOTICE.into();
        Self {
            render_dirty: true,
            model,
            backend: Backend::start(options.state.clone()),
            state_dir: options.state,
            clipboard: clipboard::Clipboard::start(),
            snapshot: backend::Snapshot::default(),
            current_target: Target::Local,
            active_session: None,
            create_pending: false,
            operation_fact: None,
            operation_id: 0,
            operation_warning: None,
            attach: None,
            surface: None,
            fragments: std::array::from_fn(|_| codec::FragAssembler::default()),
            snapshot_request: None,
            generation: 0,
            connected: false,
            size: (80, 24),
            input_queue: VecDeque::new(),
            input_bytes: 0,
            mouse_pressed: Vec::new(),
            input_geometry: None,
            sent_size: None,
            sent_theme: None,
            refresh_pending: false,
            sessions_pending: false,
            sessions_epoch: 0,
            navigation_epoch: 0,
            next_refresh: Instant::now(),
        }
    }

    fn target(&self) -> Target {
        self.current_target.clone()
    }

    fn selected_name(&self) -> Option<String> {
        self.model
            .selected_session
            .and_then(|i| self.model.sessions.get(i))
            .map(|session| session.name.clone())
    }

    fn command(&mut self, command: Command) -> bool {
        match self.backend.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.model.notice = error.to_string();
                self.render_dirty = true;
                false
            }
        }
    }

    fn poll_refresh(&mut self) {
        if Instant::now() < self.next_refresh {
            return;
        }
        self.next_refresh = Instant::now() + Duration::from_secs(2);
        if !self.refresh_pending {
            self.refresh_pending = self.command(Command::Refresh);
        }
        self.refresh_sessions();
    }

    fn refresh_sessions(&mut self) {
        if !self.sessions_pending {
            self.sessions_epoch = self.sessions_epoch.wrapping_add(1);
            self.sessions_pending = self.command(Command::Sessions {
                target: self.target(),
                epoch: self.sessions_epoch,
            });
        }
    }

    fn detach(&mut self) {
        self.input_geometry = None;
        self.sent_theme = None;
        self.mouse_pressed.clear();
        self.operation_id = self.operation_id.wrapping_add(1);
        self.model.reset_terminal_input();
        self.generation = self.generation.wrapping_add(1);
        self.attach = None;
        self.surface = None;
        for assembler in &mut self.fragments {
            assembler.reset();
        }
        self.snapshot_request = None;
        self.connected = false;
        self.input_queue.clear();
        self.input_bytes = 0;
        self.sent_size = None;
        self.model.terminal_title.clear();
        self.active_session = None;
    }

    fn disconnect(&mut self) {
        self.input_geometry = None;
        self.sent_theme = None;
        self.mouse_pressed.clear();
        self.operation_id = self.operation_id.wrapping_add(1);
        self.model.reset_terminal_input();
        self.generation = self.generation.wrapping_add(1);
        self.attach = None;
        self.connected = false;
        self.input_queue.clear();
        self.input_bytes = 0;
        self.sent_size = None;
    }

    fn clear_operation(&mut self) {
        self.operation_id = self.operation_id.wrapping_add(1);
        self.operation_fact = None;
        self.operation_warning = None;
    }

    fn complete_progress(&mut self, expected: &[&str], ready: &str) {
        // Complete only a progress message still on screen. A concurrent action,
        // error or confirmed operation owns any other notice.
        if expected.contains(&self.model.notice.as_str()) {
            self.model.notice = ready.into();
        }
    }

    fn select_session(&mut self, index: usize) -> Result<(), Error> {
        let Some(session) = self.model.sessions.get(index) else {
            return Ok(());
        };
        if session.self_session {
            self.model.notice = "这是承载当前 TUI 的会话，请选择其它会话或新建".into();
            return Ok(());
        }
        let name = session.name.clone();
        let title = session.title.clone();
        if self.active_session.as_deref() == Some(name.as_str()) && self.attach.is_some() {
            return Ok(());
        }
        self.navigation_epoch = self.navigation_epoch.wrapping_add(1);
        self.detach();
        self.model.selected_session = Some(index);
        self.model.terminal_title = title;
        self.active_session = Some(name.clone());
        self.surface = Some(surface::Surface::default());
        self.attach = Some(backend::Attach::start(
            self.state_dir.clone(),
            self.target(),
            name,
            self.size.0.max(1),
            self.size.1.max(1),
            self.generation,
        ));
        self.sent_size = Some(self.size);
        self.model.notice = "正在接入会话…".into();
        Ok(())
    }

    fn process_backend(&mut self) {
        for _ in 0..32 {
            let Some(event) = self.backend.try_event() else {
                break;
            };
            self.backend_event(event);
        }
    }

    fn backend_event(&mut self, event: backend::Event) {
        self.render_dirty = true;
        match event {
            backend::Event::Snapshot(snapshot) => {
                self.refresh_pending = false;
                let selected = self.target();
                self.model.hosts =
                    std::iter::once(local_host(snapshot.serve_on, snapshot.relay_on))
                        .chain(snapshot.hosts.iter().map(host_item))
                        .collect();
                self.model.selected_host = match selected {
                    Target::Local => 0,
                    Target::Remote(ref id) => snapshot
                        .hosts
                        .iter()
                        .position(|host| &host.id == id)
                        .map(|i| i + 1)
                        .unwrap_or(0),
                };
                self.snapshot = snapshot;
                if matches!(&selected, Target::Remote(id) if !self.snapshot.hosts.iter().any(|host| &host.id == id))
                {
                    self.current_target = Target::Local;
                }
                if !self.online() {
                    self.input_queue.clear();
                    self.input_bytes = 0;
                }
                if self.target() != selected {
                    self.detach();
                    self.model.reset_sessions();
                    self.sessions_pending = false;
                }
                if self
                    .operation_warning
                    .as_ref()
                    .is_some_and(|(confirmation, _)| {
                        *confirmation == backend::Confirmation::Snapshot
                    })
                {
                    self.operation_warning = None;
                    self.model.notice = self
                        .operation_fact
                        .as_ref()
                        .map(|fact| fact.text.clone())
                        .unwrap_or_else(|| "主机状态已恢复".into());
                }
                if self
                    .operation_fact
                    .as_ref()
                    .is_some_and(|fact| fact.confirmation == backend::Confirmation::Snapshot)
                {
                    self.operation_fact = None;
                }
                self.complete_progress(&[CONNECTING_NOTICE], CONTROL_READY_NOTICE);
            }
            backend::Event::Sessions {
                target,
                epoch,
                result,
            } => {
                if target != self.target() || epoch != self.sessions_epoch {
                    return;
                }
                self.sessions_pending = false;
                match result {
                    Ok(sessions) => {
                        let selected = self.selected_name();
                        self.model.sessions = sessions.into_iter().map(session_item).collect();
                        self.model.selected_session = selected.as_ref().and_then(|name| {
                            self.model.sessions.iter().position(|s| &s.name == name)
                        });
                        if selected.is_some() && self.model.selected_session.is_none() {
                            self.detach();
                        }
                        if self
                            .operation_warning
                            .as_ref()
                            .is_some_and(|(confirmation, _)| {
                                *confirmation == backend::Confirmation::Sessions(target.clone())
                            })
                        {
                            self.operation_warning = None;
                            self.model.notice = self
                                .operation_fact
                                .as_ref()
                                .map(|fact| fact.text.clone())
                                .unwrap_or_else(|| "会话列表已恢复".into());
                        }
                        if self.operation_fact.as_ref().is_some_and(|fact| {
                            fact.confirmation == backend::Confirmation::Sessions(target.clone())
                        }) {
                            self.operation_fact = None;
                        }
                        self.complete_progress(
                            &[CONNECTING_NOTICE, READING_SESSIONS_NOTICE],
                            SESSIONS_READY_NOTICE,
                        );
                    }
                    Err(error) => {
                        let fact = self
                            .operation_fact
                            .as_ref()
                            .map(|fact| fact.text.as_str())
                            .unwrap_or("当前主机会话");
                        let warning = format!("会话列表刷新失败：{error}");
                        self.model.notice = format!("{fact}；{warning}");
                        self.operation_warning =
                            Some((backend::Confirmation::Sessions(target), warning));
                    }
                }
            }
            backend::Event::Created {
                target,
                name,
                epoch,
            } => {
                self.create_pending = false;
                if target != self.target() || epoch != self.navigation_epoch {
                    self.model.notice = format!("会话 {name} 已在原主机创建，选择该主机即可接入");
                    return;
                }
                let index = if let Some(index) = self
                    .model
                    .sessions
                    .iter()
                    .position(|session| session.name == name)
                {
                    index
                } else {
                    self.model.sessions.push(view::SessionItem {
                        title: name.clone(),
                        name,
                        agent: "shell".into(),
                        project: String::new(),
                        self_session: false,
                        state: view::SessionState::Unknown,
                    });
                    self.model.sessions.len() - 1
                };
                self.model.focus = view::Focus::Terminal;
                if let Err(error) = self.select_session(index) {
                    self.model.notice = error.to_string();
                }
                self.sessions_pending = false;
                self.refresh_sessions();
            }
            backend::Event::Notice {
                message,
                clipboard,
                confirmation,
            } => {
                if matches!(&confirmation, backend::Confirmation::Sessions(target) if target != &self.target())
                {
                    self.model.notice = format!("原主机：{message}");
                    return;
                }
                self.operation_id = self.operation_id.wrapping_add(1);
                self.operation_warning = None;
                self.operation_fact = Some(OperationFact {
                    text: message.clone(),
                    confirmation,
                });
                self.model.notice = message.clone();
                if let Some(value) = clipboard {
                    self.model.notice =
                        match self
                            .clipboard
                            .copy(value, self.operation_id, message.clone())
                        {
                            Ok(()) => format!("{message}，正在复制到剪贴板…"),
                            Err(error) => format!("{message}；{error}"),
                        };
                }
                self.sessions_pending = false;
                self.refresh_sessions();
            }
            backend::Event::Failed(error) => {
                self.clear_operation();
                self.refresh_pending = false;
                self.sessions_pending = false;
                self.model.notice = error.to_string();
            }
            backend::Event::CreateFailed {
                target,
                epoch,
                error,
            } => {
                self.clear_operation();
                self.create_pending = false;
                let current = target == self.target() && epoch == self.navigation_epoch;
                let context = if current {
                    "创建会话未确认（若已生效，会在列表中保留）"
                } else {
                    "原主机创建会话未确认，请查看该主机会话列表"
                };
                self.model.notice = format!("{context}：{error}");
                if current {
                    self.sessions_pending = false;
                    self.refresh_sessions();
                }
            }
            backend::Event::Warning {
                message,
                error,
                confirmation,
            } => {
                self.operation_id = self.operation_id.wrapping_add(1);
                self.operation_warning = None;
                self.model.notice = format!("{message}：{error}");
                self.operation_fact = if confirmation == backend::Confirmation::None {
                    None
                } else {
                    Some(OperationFact {
                        text: self.model.notice.clone(),
                        confirmation,
                    })
                };
            }
            backend::Event::RefreshFailed(error) => {
                self.refresh_pending = false;
                let fact = self
                    .operation_fact
                    .as_ref()
                    .map(|fact| fact.text.as_str())
                    .unwrap_or("主机状态");
                let warning = format!("状态刷新失败：{error}");
                self.model.notice = format!("{fact}；{warning}");
                self.operation_warning = Some((backend::Confirmation::Snapshot, warning));
            }
        }
    }

    fn process_attach(&mut self) {
        for _ in 0..32 {
            let Some(event) = self.attach.as_ref().and_then(|attach| attach.try_event()) else {
                break;
            };
            if event.generation != self.generation {
                continue;
            }
            self.render_dirty = true;
            let frame = match event.frame {
                Ok(frame) => frame,
                Err(error) => {
                    self.model.notice = error.to_string();
                    self.disconnect();
                    break;
                }
            };
            match frame.op {
                Op::ATTACHED => match frames::dec_attached(&frame.payload) {
                    Ok(_) => self.model.notice = "已连接，正在读取会话画面…".into(),
                    Err(error) => {
                        self.model.notice = error.to_string();
                        self.disconnect();
                        break;
                    }
                },
                Op::SNAPSHOT | Op::SURFACE_DIFF | Op::FETCH_ROWS => {
                    if let Err(error) = self.surface_frame(frame.op, &frame.payload) {
                        self.model.notice = error.to_string();
                        self.disconnect();
                        break;
                    }
                }
                Op::ENDED => {
                    let (code, reason) = frames::dec_ended(&frame.payload);
                    self.model.notice =
                        format!("会话已结束（{code}，{reason}），可选择其它会话或创建新会话");
                    self.disconnect();
                    self.sessions_pending = false;
                    self.refresh_sessions();
                    break;
                }
                Op::STATE => {
                    let (_, _, title) = frames::dec_state(&frame.payload);
                    if !title.is_empty() {
                        self.model.terminal_title = title;
                    }
                }
                Op::ERROR => self.term_error(&frame.payload),
                _ => {}
            }
        }
    }

    fn surface_frame(&mut self, op: Op, payload: &[u8]) -> Result<(), surface::Error> {
        let slot = if op == Op::SNAPSHOT {
            0
        } else if op == Op::SURFACE_DIFF {
            1
        } else {
            2
        };
        let Some(compressed) = self.fragments[slot].push(payload)? else {
            return Ok(());
        };
        let body = codec::gunzip_bytes(&compressed)?;
        let Some(surface) = &mut self.surface else {
            return Ok(());
        };
        let mut focus = false;
        if op == Op::SNAPSHOT {
            let body = codec::dec_snapshot_body(&body)?;
            let title = body.title.clone();
            if surface.snapshot(body)? {
                self.snapshot_request = None;
                let initial = !self.connected;
                self.connected = true;
                if !title.is_empty() {
                    self.model.terminal_title = title;
                }
                if initial {
                    self.model.notice = "已接入 · 滚轮浏览历史".into();
                    if self.model.focus == view::Focus::Terminal && surface.focus_events() {
                        focus = true;
                    }
                }
            }
        } else if op == Op::SURFACE_DIFF {
            if !surface.diff(codec::dec_diff_body(&body)?)? {
                self.connected = false;
                self.input_queue.clear();
                self.input_bytes = 0;
                self.model.reset_terminal_input();
                self.mouse_pressed.clear();
                self.input_geometry = None;
                self.snapshot_request = None;
                self.model.notice = "正在同步终端画面…".into();
            }
        } else {
            surface.fetch_reply(codec::dec_fetch_rows_reply(&body)?)?;
        }
        if self
            .input_geometry
            .as_ref()
            .is_some_and(|pending| surface.ready && pending.size == surface.geometry())
        {
            self.input_geometry = None;
        }
        if focus {
            self.send_input(frames::InputEvent::Focus { gained: true });
        }
        Ok(())
    }

    fn poll_surface(&mut self) {
        let Some(surface) = &mut self.surface else {
            return;
        };
        let Some(attach) = &self.attach else { return };
        let now = Instant::now();
        if !surface.ready {
            if self
                .snapshot_request
                .is_none_or(|sent| now.duration_since(sent) >= Duration::from_secs(2))
            {
                match attach.fetch_snapshot() {
                    Ok(()) => self.snapshot_request = Some(now),
                    Err(backend::Error::Busy) => {}
                    Err(error) => {
                        self.model.notice = error.to_string();
                        self.disconnect();
                        return;
                    }
                }
            }
        } else if let Some(request) = surface.fetch_request(self.size.1, now) {
            if let Err(error) = attach.fetch_rows(request) {
                surface.fetch_failed();
                if !matches!(error, backend::Error::Busy) {
                    self.model.notice = error.to_string();
                }
            }
        }
        if let Some(pending) = &mut self.input_geometry {
            if pending
                .requested
                .is_none_or(|sent| now.duration_since(sent) >= Duration::from_secs(2))
            {
                match attach.fetch_snapshot() {
                    Ok(()) => pending.requested = Some(now),
                    Err(backend::Error::Busy) => {}
                    Err(error) => {
                        self.model.notice = error.to_string();
                        self.disconnect();
                    }
                }
            }
        }
        self.sync_theme();
    }

    fn sync_theme(&mut self) {
        if !self.connected || self.sent_theme == Some(self.model.theme) {
            return;
        }
        let Some(attach) = &self.attach else { return };
        let (fg, bg, dark) = match self.model.theme {
            view::Theme::Dark => ([221, 226, 234], [18, 22, 29], true),
            view::Theme::Light => ([32, 43, 59], [248, 250, 253], false),
        };
        match attach.send_frame(Op::THEME, codec::enc_theme(fg, bg, dark)) {
            Ok(()) => self.sent_theme = Some(self.model.theme),
            Err(backend::Error::Busy) => {}
            Err(error) => {
                self.model.notice = error.to_string();
                self.disconnect();
            }
        }
    }

    fn term_error(&mut self, payload: &[u8]) {
        if let Some(surface) = &mut self.surface {
            surface.fetch_failed();
        }
        self.clear_operation();
        self.model.notice = match frames::dec_error(payload) {
            Ok((code, message)) => {
                if code == "bad_resize" {
                    self.sent_size = None;
                    self.input_queue.clear();
                    self.input_bytes = 0;
                }
                format!("终端操作未生效（{code}）：{message}")
            }
            Err(error) => error.to_string(),
        };
    }

    fn process_clipboard(&mut self) -> Result<(), Error> {
        while let Some(result) = self.clipboard.try_result() {
            self.clipboard_result(result)?;
        }
        Ok(())
    }

    fn clipboard_result(&mut self, result: clipboard::CopyResult) -> Result<(), Error> {
        self.render_dirty = true;
        let status = match result.status {
            clipboard::Copied::System => "已复制到系统剪贴板".into(),
            clipboard::Copied::Terminal(sequence) => {
                io::stdout().write_all(sequence.as_bytes())?;
                io::stdout().flush()?;
                "已发送剪贴板请求（终端需允许 OSC52）".into()
            }
            clipboard::Copied::Failed(error) => format!("操作完成，复制失败：{error}"),
        };
        if result.operation == self.operation_id {
            self.model.notice = format!("{}；{status}", result.fact);
            if let Some((_, warning)) = &self.operation_warning {
                self.model.notice.push_str(&format!("；{warning}"));
            }
        }
        Ok(())
    }

    fn send_input(&mut self, event: frames::InputEvent) {
        let release = matches!(event, frames::InputEvent::Key { action: 0, .. });
        if !self.connected || !self.online() || (self.size == (0, 0) && !release) {
            return;
        }
        let bytes = frames::enc_input(&event);
        if self.input_bytes + bytes.len() > 2 << 20 {
            self.model.notice = "输入队列已满，请等待远端接收后重试".into();
            return;
        }
        if matches!(
            event,
            frames::InputEvent::Key { action: 1 | 2, .. } | frames::InputEvent::Text { .. }
        ) {
            if let Some(surface) = &mut self.surface {
                surface.follow();
            }
        }
        self.input_bytes += bytes.len();
        self.input_queue.push_back(bytes);
        let activity = matches!(
            &event,
            frames::InputEvent::Key { action: 1 | 2, .. }
                | frames::InputEvent::Mouse {
                    action: 0,
                    button: 1..=11,
                    ..
                }
        ) || matches!(&event, frames::InputEvent::Text { text, .. } if !text.is_empty());
        if activity
            && self
                .surface
                .as_ref()
                .is_some_and(|surface| surface.geometry() != self.size)
        {
            self.input_geometry = Some(InputGeometry {
                size: self.size,
                requested: None,
            });
        }
    }

    fn flush_input(&mut self) {
        // A previously forwarded kitty press still needs its release when the pane
        // becomes hidden; fresh presses are rejected by send_input in that state.
        if !self.online() || (self.size != (0, 0) && self.sent_size != Some(self.size)) {
            return;
        }
        for _ in 0..16 {
            let Some(bytes) = self.input_queue.pop_front() else {
                break;
            };
            let Some(attach) = &self.attach else {
                self.input_queue.clear();
                self.input_bytes = 0;
                break;
            };
            match attach.send_frame(Op::INPUT, bytes.clone()) {
                Ok(()) => self.input_bytes -= bytes.len(),
                Err(backend::Error::Busy) => {
                    self.input_queue.push_front(bytes);
                    break;
                }
                Err(error) => {
                    self.model.notice = error.to_string();
                    self.disconnect();
                    break;
                }
            }
        }
    }

    fn online(&self) -> bool {
        self.model.selected_host == 0
            || self
                .snapshot
                .hosts
                .get(self.model.selected_host.wrapping_sub(1))
                .is_some_and(|host| host.state != backend::LinkState::Offline)
    }

    fn resize(&mut self, cols: u16, rows: u16) {
        if cols == 0 || rows == 0 {
            self.input_geometry = None;
            self.size = (0, 0);
            self.input_queue.clear();
            self.input_bytes = 0;
            return;
        }
        let size = homeway_core::term::size::Size::normalized(cols, rows);
        let (cols, rows) = (size.cols(), size.rows());
        if self.size != (cols, rows) {
            self.size = (cols, rows);
            self.render_dirty = true;
            if let Some(pending) = &mut self.input_geometry {
                pending.size = self.size;
                pending.requested = None;
            }
        }
        if self.sent_size == Some((cols, rows)) {
            return;
        }
        if let Some(attach) = &self.attach {
            match attach.resize(cols, rows) {
                Ok(()) => self.sent_size = Some((cols, rows)),
                Err(backend::Error::Busy) => {} // 同尺寸下一拍重试，不能把队列满当成已同步。
                Err(error) => {
                    self.model.notice = error.to_string();
                    self.connected = false;
                }
            }
        }
    }

    fn action(&mut self, action: Action) -> Result<bool, Error> {
        match action {
            Action::Quit => return Ok(true),
            Action::None => {}
            Action::SelectHost(index) => {
                let target = self
                    .snapshot
                    .hosts
                    .get(index.wrapping_sub(1))
                    .map(|host| Target::Remote(host.id.clone()))
                    .unwrap_or(Target::Local);
                if target == self.current_target {
                    return Ok(false);
                }
                self.clear_operation();
                self.navigation_epoch = self.navigation_epoch.wrapping_add(1);
                self.current_target = target;
                self.detach();
                self.model.selected_host = index;
                self.model.reset_sessions();
                self.sessions_pending = false;
                self.model.notice = READING_SESSIONS_NOTICE.into();
                self.refresh_sessions();
            }
            Action::SelectSession(index) => {
                self.clear_operation();
                self.select_session(index)?;
            }
            Action::AddHost { name, token } => {
                self.clear_operation();
                self.command(Command::AddHost { name, token });
            }
            Action::CreateSession => {
                if self.create_pending {
                    self.model.notice = "正在创建会话，请稍候…".into();
                    return Ok(false);
                }
                self.clear_operation();
                self.create_pending = self.command(Command::NewSession {
                    target: self.target(),
                    epoch: self.navigation_epoch,
                });
            }
            Action::ToggleExit => {
                self.clear_operation();
                self.command(Command::SetRole {
                    role: backend::Role::Exit,
                    on: !self.snapshot.serve_on,
                });
            }
            Action::ToggleRelay => {
                self.clear_operation();
                self.command(Command::SetRole {
                    role: backend::Role::Relay,
                    on: !self.snapshot.relay_on,
                });
            }
            Action::ToggleProxy => {
                self.clear_operation();
                if let Target::Remote(id) = self.target() {
                    let on = !self.model.hosts[self.model.selected_host].proxy_on;
                    self.command(Command::SetProxy { id, on });
                }
            }
            Action::DeleteHost => {
                self.clear_operation();
                if let Target::Remote(id) = self.target() {
                    self.navigation_epoch = self.navigation_epoch.wrapping_add(1);
                    self.command(Command::RemoveHost { id });
                }
            }
            Action::CloseSession => {
                if self
                    .model
                    .selected_session
                    .and_then(|index| self.model.sessions.get(index))
                    .is_some_and(|session| session.self_session)
                {
                    self.model.notice = "承载当前 TUI 的会话不能在此关闭；退出 TUI 后再关闭".into();
                    return Ok(false);
                }
                self.clear_operation();
                if let Some(name) = self.selected_name() {
                    self.navigation_epoch = self.navigation_epoch.wrapping_add(1);
                    self.sessions_epoch = self.sessions_epoch.wrapping_add(1);
                    self.sessions_pending = self.command(Command::KillSession {
                        target: self.target(),
                        name,
                    });
                }
            }
            Action::SendKey(event) => {
                if let Some(event) = input::key(event) {
                    self.send_input(event);
                }
            }
            Action::Paste(text) => {
                // A transaction is at most 1 MiB, as in the existing service.
                if text.len() > 1 << 20 {
                    self.model.notice = "粘贴超过 1 MiB，请分批粘贴".into();
                } else {
                    let chunks = input::paste(&text);
                    let bytes = text.len() + chunks.len() * 4;
                    if self.input_bytes + bytes > 2 << 20 {
                        self.model.notice = "输入队列忙，请稍后粘贴".into();
                    } else {
                        for event in chunks {
                            self.send_input(event);
                        }
                    }
                }
            }
            Action::TerminalMouse(mut event) => {
                use crossterm::event::MouseEventKind;
                let paired = match event.kind {
                    MouseEventKind::Up(button) | MouseEventKind::Drag(button) => {
                        self.mouse_pressed.contains(&button)
                    }
                    _ => false,
                };
                let activating = matches!(
                    event.kind,
                    MouseEventKind::Down(_) | MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                );
                // A key, paste or wheel may already have taken pane geometry on the
                // server while the old wide snapshot is still visible. Unpaired
                // passive events wait for an authoritative matching baseline.
                if self.input_geometry.is_some() && !activating && !paired {
                    return Ok(false);
                }
                if let Some((column, row)) = self.surface.as_ref().and_then(|surface| {
                    surface.mouse_position(event.column, event.row, activating || paired)
                }) {
                    event.column = column;
                    event.row = row;
                    if let Some(input) = input::mouse(event) {
                        let queued = self.input_queue.len();
                        self.send_input(input);
                        if self.input_queue.len() > queued {
                            match event.kind {
                                MouseEventKind::Down(button)
                                    if !self.mouse_pressed.contains(&button) =>
                                {
                                    self.mouse_pressed.push(button)
                                }
                                MouseEventKind::Up(button) => {
                                    self.mouse_pressed.retain(|pressed| *pressed != button)
                                }
                                _ => {}
                            }
                        }
                    }
                }
            }
            Action::Scroll(action) => {
                if self
                    .selected_name()
                    .is_some_and(|name| self.active_session.as_ref() == Some(&name))
                {
                    if let Some(surface) = &mut self.surface {
                        if surface.ready {
                            surface.scroll(action, self.size.1);
                        }
                    }
                }
                self.render_dirty = true;
            }
        }
        Ok(false)
    }
}

fn local_host(exit_on: bool, relay_on: bool) -> view::HostItem {
    view::HostItem {
        name: "我的电脑".into(),
        detail: String::new(),
        state: view::HostState::Local,
        exit_on,
        relay_on,
        proxy_on: false,
    }
}

fn host_item(host: &backend::Host) -> view::HostItem {
    let endpoint = host.endpoint.as_deref().unwrap_or("IP 未知");
    let delay = host
        .rtt_ms
        .map(|ms| format!("{ms} ms"))
        .unwrap_or_else(|| "-- ms".into());
    view::HostItem {
        name: host.name.clone(),
        detail: format!("{endpoint}  {delay}"),
        state: match host.state {
            backend::LinkState::Direct => view::HostState::Direct,
            backend::LinkState::Relay => view::HostState::Relay,
            backend::LinkState::Offline => view::HostState::Offline,
        },
        exit_on: false,
        relay_on: false,
        proxy_on: host.proxy.is_some(),
    }
}

fn session_item(session: backend::Session) -> view::SessionItem {
    let project = session
        .cwd
        .as_deref()
        .and_then(|cwd| Path::new(cwd).file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    view::SessionItem {
        title: if session.title.is_empty() {
            session.name.clone()
        } else {
            session.title
        },
        name: session.name,
        agent: session
            .agent_label
            .filter(|label| !label.is_empty())
            .unwrap_or(session.agent),
        project,
        self_session: session.self_session,
        state: match session.state {
            backend::SessionState::Working => view::SessionState::Working,
            backend::SessionState::Idle => view::SessionState::Idle,
            backend::SessionState::Blocked => view::SessionState::Blocked,
            backend::SessionState::Unknown => view::SessionState::Unknown,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(
        app: &mut App,
        terminal: &mut Terminal<ratatui::backend::TestBackend>,
    ) -> (view::Geometry, String) {
        if let Some(surface) = &mut app.surface {
            surface.prepare(app.size.1);
        }
        let mut geometry = view::Geometry::default();
        terminal
            .draw(|frame| {
                geometry = view::draw(
                    frame,
                    &mut app.model,
                    app.surface.as_ref().map(surface::Surface::view),
                )
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        let footer = (0..buffer.area.width)
            .map(|x| buffer[(x, buffer.area.height - 1)].symbol())
            .collect();
        (geometry, footer)
    }

    fn history_app() -> App {
        let mut vt = homeway_core::term::vt::SessionVt::new(66, 27, 100).unwrap();
        for line in 0..80 {
            vt.write(format!("LINE_{line:03}\r\n").as_bytes());
        }
        let mirror = vt.mirror_rows(100);
        let mut surface = surface::Surface::default();
        surface
            .snapshot(codec::SnapshotBody {
                cols: 66,
                rows: 27,
                seq: 1,
                revision: 1,
                cursor: codec::surface_cursor_of(&vt.cursor()),
                scroll: vt.scrollbar().into(),
                history_epoch: vt.history_epoch(),
                history_base: vt.history_base(),
                grid: codec::encode_grid(66, 27, &vt.rows()),
                mirror: codec::encode_grid(66, mirror.len() as u16, &mirror),
                ..Default::default()
            })
            .unwrap();
        let mut app = app();
        app.surface = Some(surface);
        app.size = (66, 27);
        app.active_session = Some("history".into());
        app.model.sessions.push(view::SessionItem {
            name: "history".into(),
            title: "history".into(),
            agent: "shell".into(),
            project: String::new(),
            state: view::SessionState::Idle,
            self_session: false,
        });
        app.model.selected_session = Some(0);
        app.model.focus = view::Focus::Terminal;
        app.model.notice = "已接入".into();
        app
    }

    #[test]
    fn empty_terminal_mouse_and_history_keys_preserve_notice_and_empty_footer() {
        use crossterm::event::{
            KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
        };
        let mut app = app();
        app.model.notice = "暂无会话，请创建 shell".into();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let (geometry, _) = screen(&mut app, &mut terminal);
        for kind in [
            MouseEventKind::Moved,
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::ScrollUp,
            MouseEventKind::ScrollDown,
        ] {
            let action = view::handle_event(
                &mut app.model,
                Event::Mouse(MouseEvent {
                    kind,
                    column: geometry.terminal.x + 2,
                    row: geometry.terminal.y + 2,
                    modifiers: KeyModifiers::NONE,
                }),
                &geometry,
            );
            app.action(action).unwrap();
        }
        // Even a queued historical shortcut from a formerly focused pane is harmless.
        app.model.focus = view::Focus::Terminal;
        for key in [
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Home,
            KeyCode::End,
        ] {
            let action = view::handle_event(
                &mut app.model,
                Event::Key(KeyEvent::new(key, KeyModifiers::SHIFT)),
                &geometry,
            );
            app.action(action).unwrap();
        }
        let (_, footer) = screen(&mut app, &mut terminal);
        assert_eq!(app.model.notice, "暂无会话，请创建 shell");
        assert!(!footer.contains('↑') && !footer.contains("⇧End"));
        assert!(app.surface.is_none() && app.active_session.is_none());
    }

    #[test]
    fn real_history_footer_returns_to_live_and_disappears_when_sessions_are_cleared() {
        let mut app = history_app();
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        assert!(!screen(&mut app, &mut terminal).1.contains('↑'));
        app.action(Action::Scroll(view::HistoryAction::Start))
            .unwrap();
        assert!(screen(&mut app, &mut terminal).1.contains('↑'));
        app.action(Action::TerminalMouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Moved,
            column: 2,
            row: 2,
            modifiers: crossterm::event::KeyModifiers::NONE,
        }))
        .unwrap();
        assert_eq!(
            app.model.notice, "已接入",
            "history guidance is derived, not a persistent error notice"
        );
        app.action(Action::Scroll(view::HistoryAction::End))
            .unwrap();
        let footer = screen(&mut app, &mut terminal).1;
        assert!(!footer.contains('↑') && !footer.contains("⇧End"));
        app.action(Action::Scroll(view::HistoryAction::Start))
            .unwrap();
        assert!(screen(&mut app, &mut terminal).1.contains('↑'));
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: app.sessions_epoch,
            result: Ok(vec![]),
        });
        assert!(app.surface.is_none() && app.model.selected_session.is_none());
        let footer = screen(&mut app, &mut terminal).1;
        assert!(!footer.contains('↑') && !footer.contains("⇧End"));
    }

    #[test]
    fn history_shortcuts_ignore_missing_selection_and_pending_surface() {
        let mut app = history_app();
        app.surface.as_mut().unwrap().prepare(27);
        let live_top = app.surface.as_ref().unwrap().view().top;
        app.model.selected_session = None;
        app.action(Action::Scroll(view::HistoryAction::Start))
            .unwrap();
        app.surface.as_mut().unwrap().prepare(27);
        assert_eq!(app.surface.as_ref().unwrap().view().top, live_top);
        app.model.selected_session = Some(0);
        app.surface.as_mut().unwrap().ready = false;
        app.action(Action::Scroll(view::HistoryAction::Start))
            .unwrap();
        app.surface.as_mut().unwrap().prepare(27);
        assert_eq!(app.surface.as_ref().unwrap().view().top, live_top);
        assert_eq!(app.model.notice, "已接入");
    }

    fn app() -> App {
        App::new(Options {
            state: std::env::temp_dir().join(format!("homeway-tui-events-{}", std::process::id())),
        })
    }

    #[test]
    fn initial_successful_snapshot_finishes_real_startup_notice_in_empty_view() {
        let mut app = app();
        assert_eq!(app.model.notice, CONNECTING_NOTICE);
        assert!(app.model.sessions.is_empty());
        app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
        assert_eq!(app.model.notice, CONTROL_READY_NOTICE);
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: app.sessions_epoch,
            result: Ok(vec![]),
        });
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(100, 30)).unwrap();
        let (_, footer) = screen(&mut app, &mut terminal);
        assert!(!footer.contains('…'));
        assert!(app.model.sessions.is_empty() && app.surface.is_none());
        assert!(
            terminal
                .backend()
                .buffer()
                .content
                .iter()
                .any(|cell| cell.symbol() == "创"),
            "empty view still guides shell creation"
        );
        // The independent session lane can also finish first.
        let mut app = self::app();
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: app.sessions_epoch,
            result: Ok(vec![]),
        });
        assert_eq!(app.model.notice, SESSIONS_READY_NOTICE);
        app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
        assert_eq!(app.model.notice, SESSIONS_READY_NOTICE);
    }

    #[test]
    fn host_reading_progress_finishes_only_for_current_host_and_query() {
        let mut app = app();
        app.current_target = Target::Remote("previous".into());
        app.action(Action::SelectHost(0)).unwrap();
        let epoch = app.sessions_epoch;
        assert_eq!(app.model.notice, READING_SESSIONS_NOTICE);
        app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
        assert_eq!(app.model.notice, READING_SESSIONS_NOTICE);
        for (target, response_epoch) in [
            (Target::Remote("previous".into()), epoch),
            (Target::Local, epoch.wrapping_sub(1)),
        ] {
            app.backend_event(backend::Event::Sessions {
                target,
                epoch: response_epoch,
                result: Ok(vec![]),
            });
            assert_eq!(app.model.notice, READING_SESSIONS_NOTICE);
        }
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch,
            result: Ok(vec![]),
        });
        assert_eq!(app.model.notice, SESSIONS_READY_NOTICE);
    }

    #[test]
    fn successful_background_refresh_preserves_errors_pending_creation_and_confirmed_results() {
        let complete = |app: &mut App| {
            app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
            app.backend_event(backend::Event::Sessions {
                target: Target::Local,
                epoch: app.sessions_epoch,
                result: Ok(vec![]),
            });
        };
        let mut app = app();
        app.backend_event(backend::Event::Failed(backend::Error::Busy));
        let error = app.model.notice.clone();
        complete(&mut app);
        assert_eq!(app.model.notice, error);
        app.create_pending = true;
        app.action(Action::CreateSession).unwrap();
        let creating = app.model.notice.clone();
        assert!(creating.contains("正在创建"));
        complete(&mut app);
        assert_eq!(app.model.notice, creating);
        app.backend_event(backend::Event::Notice {
            message: "主机已添加".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        complete(&mut app);
        assert_eq!(app.model.notice, "主机已添加");
    }

    #[test]
    fn created_session_invalidates_list_already_in_flight() {
        let mut app = app();
        app.sessions_epoch = 7;
        app.sessions_pending = true;
        app.backend_event(backend::Event::Created {
            target: Target::Local,
            name: "created".into(),
            epoch: 0,
        });
        assert_eq!(app.sessions_epoch, 8);
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: 7,
            result: Ok(vec![]),
        });
        assert_eq!(app.selected_name().as_deref(), Some("created"));
        assert_eq!(app.model.sessions.len(), 1);
    }

    #[test]
    fn late_create_cannot_change_new_navigation_context() {
        let mut app = app();
        app.navigation_epoch = 3;
        app.backend_event(backend::Event::Created {
            target: Target::Local,
            name: "old".into(),
            epoch: 2,
        });
        assert!(app.model.sessions.is_empty());
        assert!(app.attach.is_none());
        assert!(app.model.notice.contains("已在原主机创建"));
    }

    #[test]
    fn duplicate_create_is_single_flight_until_the_matching_result() {
        let mut app = app();
        app.create_pending = true;
        assert!(!app.action(Action::CreateSession).unwrap());
        assert!(app.create_pending);
        assert!(app.model.notice.contains("正在创建"));
        app.backend_event(backend::Event::CreateFailed {
            target: Target::Local,
            epoch: 0,
            error: backend::Error::Timeout,
        });
        assert!(!app.create_pending);
    }

    #[test]
    fn refresh_warning_keeps_the_success_fact_and_does_not_grow_unbounded() {
        let mut app = app();
        app.backend_event(backend::Event::Notice {
            message: "出口已启动".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        let first = app.model.notice.clone();
        assert!(first.contains("出口已启动") && first.contains("刷新失败"));
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        assert_eq!(app.model.notice, first);
        app.sessions_epoch = 4;
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: 4,
            result: Err(backend::Error::Timeout),
        });
        assert!(
            app.model.notice.contains("出口已启动")
                && app.model.notice.contains("会话列表刷新失败")
        );
        assert!(app.model.notice.len() < 200);
    }

    #[test]
    fn success_fact_expires_only_after_its_own_confirmation() {
        let mut app = app();
        app.backend_event(backend::Event::Notice {
            message: "会话已关闭".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Sessions(Target::Local),
        });
        app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
        assert!(
            app.operation_fact.is_some(),
            "主机快照不能确认KILL后的会话列表"
        );
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: app.sessions_epoch,
            result: Ok(vec![]),
        });
        assert!(app.operation_fact.is_none());
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: app.sessions_epoch,
            result: Err(backend::Error::Timeout),
        });
        assert!(
            !app.model.notice.contains("会话已关闭"),
            "旧成功事实不能串到后续失败"
        );
    }

    #[test]
    fn late_clipboard_result_keeps_its_original_operation_context() {
        let mut app = app();
        app.backend_event(backend::Event::Notice {
            message: "出口已启动".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        let previous = app.operation_id;
        app.backend_event(backend::Event::Notice {
            message: "会话已关闭".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Sessions(Target::Local),
        });
        app.clipboard_result(clipboard::CopyResult {
            operation: previous,
            fact: "出口已启动".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert_eq!(app.model.notice, "会话已关闭");
    }

    #[test]
    fn clipboard_success_does_not_hide_a_warning_from_the_same_operation() {
        let mut app = app();
        app.backend_event(backend::Event::Notice {
            message: "出口已启动".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        app.clipboard_result(clipboard::CopyResult {
            operation: app.operation_id,
            fact: "出口已启动".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert!(
            app.model.notice.contains("出口已启动")
                && app.model.notice.contains("已复制")
                && app.model.notice.contains("刷新失败")
        );
    }

    #[test]
    fn live_error_is_not_overwritten_by_an_older_clipboard_result() {
        let mut app = app();
        app.backend_event(backend::Event::Notice {
            message: "出口已启动".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        let previous = app.operation_id;
        app.term_error(&frames::enc_error("bad_input", "另一个客户端正在粘贴"));
        app.clipboard_result(clipboard::CopyResult {
            operation: previous,
            fact: "出口已启动".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert!(app.model.notice.contains("bad_input"));
    }

    #[test]
    fn another_notice_does_not_suppress_the_next_refresh_warning() {
        let mut app = app();
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        app.backend_event(backend::Event::Notice {
            message: "主机已添加".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        assert!(app.model.notice.contains("主机已添加") && app.model.notice.contains("刷新失败"));
    }

    #[test]
    fn session_completion_on_another_host_is_not_a_current_host_fact() {
        let mut app = app();
        app.backend_event(backend::Event::Notice {
            message: "会话已关闭".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Sessions(Target::Remote("old-host".into())),
        });
        assert!(app.model.notice.contains("原主机"));
        assert!(app.operation_fact.is_none());
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: 0,
            result: Err(backend::Error::Timeout),
        });
        assert!(!app.model.notice.contains("会话已关闭"));
    }

    #[test]
    fn create_failure_invalidates_feedback_from_a_previous_clipboard_operation() {
        let mut app = app();
        app.backend_event(backend::Event::Notice {
            message: "出口已启动".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        let previous = app.operation_id;
        app.backend_event(backend::Event::CreateFailed {
            target: Target::Local,
            epoch: 0,
            error: backend::Error::Timeout,
        });
        app.clipboard_result(clipboard::CopyResult {
            operation: previous,
            fact: "出口已启动".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert!(app.model.notice.contains("创建会话未确认"));
    }

    #[test]
    fn connection_end_resets_prefix_and_pending_input_without_erasing_screen() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        app.model.focus = view::Focus::Terminal;
        let mut geometry = view::Geometry::default();
        geometry.terminal = ratatui::layout::Rect::new(0, 0, 80, 24);
        view::handle_event(
            &mut app.model,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            &geometry,
        );
        let mut vt = homeway_core::term::vt::SessionVt::new(80, 24, 0).unwrap();
        vt.write(b"KEEP");
        let mut surface = surface::Surface::default();
        surface
            .snapshot(codec::SnapshotBody {
                seq: 1,
                cols: 80,
                rows: 24,
                grid: codec::encode_grid(80, 24, &vt.rows()),
                scroll: codec::ScrollbarWire {
                    total: 24,
                    offset: 0,
                    len: 24,
                },
                ..Default::default()
            })
            .unwrap();
        surface.prepare(24);
        app.surface = Some(surface);
        app.input_queue.push_back(b"old-input".to_vec());
        app.disconnect();
        assert!(matches!(
            view::handle_event(
                &mut app.model,
                Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
                &geometry
            ),
            Action::SendKey(_)
        ));
        assert!(app.input_queue.is_empty());
        assert_eq!(
            app.surface.as_ref().unwrap().view().rows[0].cells[0].symbol,
            "K"
        );
    }

    #[test]
    fn session_label_prefers_real_manifest_identity_and_cwd_basename() {
        let session = backend::Session {
            name: "s".into(),
            agent: "other".into(),
            agent_label: Some("pi".into()),
            cwd: Some("/tmp/project".into()),
            ..Default::default()
        };
        let item = session_item(session);
        assert_eq!(item.agent, "pi");
        assert_eq!(item.project, "project");
    }

    #[test]
    fn hidden_pane_rejects_new_input_but_keeps_a_previously_forwarded_release() {
        let mut app = app();
        app.connected = true;
        app.resize(0, 0);
        app.send_input(frames::InputEvent::Key {
            key: 20,
            mods: 0,
            action: 1,
            text: "a".into(),
        });
        assert!(app.input_queue.is_empty());
        app.send_input(frames::InputEvent::Key {
            key: 20,
            mods: 0,
            action: 0,
            text: "a".into(),
        });
        assert_eq!(app.input_queue.len(), 1);
        assert!(matches!(
            frames::dec_input(app.input_queue.front().unwrap()).unwrap(),
            frames::InputEvent::Key { action: 0, .. }
        ));
    }

    #[test]
    fn self_session_is_rejected_before_detaching_the_existing_screen() {
        let mut app = app();
        app.model.sessions.push(view::SessionItem {
            name: "carrier".into(),
            title: "carrier".into(),
            agent: "shell".into(),
            project: String::new(),
            self_session: true,
            state: view::SessionState::Idle,
        });
        app.surface = Some(surface::Surface::default());
        app.active_session = Some("other".into());
        let generation = app.generation;
        app.select_session(0).unwrap();
        assert_eq!(app.generation, generation);
        assert_eq!(app.active_session.as_deref(), Some("other"));
        assert!(app.surface.is_some());
        assert!(app.model.notice.contains("承载当前"));
        app.model.selected_session = Some(0); // Defense even if a caller bypasses view selection.
        let navigation = app.navigation_epoch;
        app.action(Action::CloseSession).unwrap();
        assert_eq!(app.navigation_epoch, navigation);
        assert!(app.model.notice.contains("退出 TUI 后再关闭"));
    }

    #[test]
    fn mouse_press_release_pair_uses_new_pane_coordinates_before_snapshot_arrives() {
        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
        let mut vt = homeway_core::term::vt::SessionVt::new(120, 40, 0).unwrap();
        vt.write(b"\x1b[35;1HLOW_CURSOR");
        let mut surface = surface::Surface::default();
        surface
            .snapshot(codec::SnapshotBody {
                cols: 120,
                rows: 40,
                seq: 1,
                cursor: codec::surface_cursor_of(&vt.cursor()),
                scroll: vt.scrollbar().into(),
                grid: codec::encode_grid(120, 40, &vt.rows()),
                ..Default::default()
            })
            .unwrap();
        surface.prepare(25);
        let mut app = app();
        app.surface = Some(surface);
        app.connected = true;
        app.size = (60, 25);
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.action(Action::TerminalMouse(MouseEvent {
                kind,
                column: 2,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
        }
        assert_eq!(app.input_queue.len(), 2);
        for bytes in &app.input_queue {
            assert!(matches!(
                frames::dec_input(bytes).unwrap(),
                frames::InputEvent::Mouse { x: 2, y: 3, .. }
            ));
        }
        assert!(app.mouse_pressed.is_empty());
        app.action(Action::TerminalMouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 2,
            row: 3,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
        assert_eq!(
            app.input_queue.len(),
            2,
            "unpaired release waits for the new pane baseline instead of using the old crop offset"
        );
        app.detach();
        assert!(app.mouse_pressed.is_empty());
    }

    #[test]
    fn keys_paste_and_wheel_hold_passive_mouse_until_pane_baseline_arrives() {
        use crossterm::event::{KeyModifiers, MouseEvent, MouseEventKind};
        for input in [
            frames::InputEvent::Key {
                key: 20,
                mods: 0,
                action: 1,
                text: "a".into(),
            },
            frames::InputEvent::Text {
                flags: frames::text_bits::PASTE,
                text: "paste".into(),
            },
            frames::InputEvent::Mouse {
                action: 0,
                button: 4,
                mods: 0,
                x: 2,
                y: 3,
            },
        ] {
            let mut vt = homeway_core::term::vt::SessionVt::new(120, 40, 0).unwrap();
            vt.write(b"\x1b[35;1HLOW");
            let mut surface = surface::Surface::default();
            surface
                .snapshot(codec::SnapshotBody {
                    cols: 120,
                    rows: 40,
                    seq: 1,
                    cursor: codec::surface_cursor_of(&vt.cursor()),
                    scroll: vt.scrollbar().into(),
                    grid: codec::encode_grid(120, 40, &vt.rows()),
                    ..Default::default()
                })
                .unwrap();
            surface.prepare(25);
            let mut app = app();
            app.surface = Some(surface);
            app.connected = true;
            app.size = (60, 25);
            app.send_input(input);
            assert_eq!(app.input_geometry.as_ref().unwrap().size, (60, 25));
            let queued = app.input_queue.len();
            app.action(Action::TerminalMouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 2,
                row: 3,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
            assert_eq!(
                app.input_queue.len(),
                queued,
                "passive mouse cannot address old wide geometry after real input"
            );
            vt.resize(60, 25).unwrap();
            let body = codec::SnapshotBody {
                cols: 60,
                rows: 25,
                seq: 2,
                revision: 2,
                history_epoch: vt.history_epoch(),
                history_base: vt.history_base(),
                cursor: codec::surface_cursor_of(&vt.cursor()),
                scroll: vt.scrollbar().into(),
                grid: codec::encode_grid(60, 25, &vt.rows()),
                ..Default::default()
            };
            for fragment in
                codec::fragment_payload(&codec::gzip_bytes(&codec::enc_snapshot_body(&body)))
            {
                app.surface_frame(Op::SNAPSHOT, &fragment).unwrap();
            }
            assert!(app.input_geometry.is_none());
        }
    }
}
