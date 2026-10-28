//! `homeway tui`：既有 daemon/term 的交互客户端。
mod appearance;
mod backend;
mod clipboard;
mod input;
mod selection;
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
use ratatui::{backend::CrosstermBackend, layout::Rect, Terminal};

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
        app.expire_notice(Instant::now());
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
            app.validate_selection(geometry.terminal);
            app.model.terminal_present = app.active_session.is_some();
            app.model.terminal_close = app.terminal_close_target();
            if let Some(surface) = &mut app.surface {
                surface.prepare(app.size.1);
            }
            terminal.draw(|frame| {
                app.model.selecting = app.selection.is_some();
                app.model.selection_spans = app
                    .selection
                    .as_ref()
                    .map_or_else(Vec::new, |drag| drag.value.spans());
                let displayed = app
                    .selection
                    .as_ref()
                    .map(|drag| drag.value.snapshot().view())
                    .or_else(|| app.surface.as_ref().map(surface::Surface::view));
                geometry = view::draw(frame, &mut app.model, displayed);
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
            if app.selection_event(&event, &geometry)? {
                continue;
            }
            let commands_before = app.model.commands_open();
            let focused = app.model.focus == view::Focus::Terminal && !app.model.modal_open();
            if let Event::FocusGained | Event::FocusLost = event {
                if !app.model.modal_open()
                    && app
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
                && !commands_before
                && !app.model.commands_open()
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
    create_target: Option<Target>,
    close_pending: Option<ClosingSession>,
    selection: Option<DragSelection>,
    local_left: bool,
    selection_copy: Option<SelectionCopy>,
    selection_copy_ticket: u64,
    operation_fact: Option<OperationFact>,
    operation_target: Option<view::NoticeTarget>,
    operation_copy: Option<OperationCopy>,
    notice_life: NoticeLife,
    operation_id: u64,
    operation_warning: Option<(backend::Confirmation, String)>,
    warning_notice: Option<view::ItemNotice>,
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

struct OperationCopy {
    operation: u64,
    notice: view::ItemNotice,
}

enum NoticeLife {
    Persistent,
    Progress,
    Brief {
        until: Instant,
        owner: view::ItemNotice,
    },
}

#[derive(PartialEq, Eq)]
struct ClosingSession {
    target: Target,
    name: String,
}

struct SelectionContext {
    generation: u64,
    target: Target,
    name: String,
    identity: selection::Identity,
    pane: Rect,
}

struct DragSelection {
    value: selection::Selection,
    context: SelectionContext,
}

struct SelectionCopy {
    ticket: u64,
    operation: u64,
    notice: String,
    context: SelectionContext,
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
        model.item_notice = Some(view::ItemNotice {
            target: view::NoticeTarget::Host(Target::Local),
            summary: "正在连接…".into(),
            detail: CONNECTING_NOTICE.into(),
        });
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
            create_target: None,
            close_pending: None,
            selection: None,
            local_left: false,
            selection_copy: None,
            selection_copy_ticket: 0,
            operation_fact: None,
            operation_target: None,
            operation_copy: None,
            notice_life: NoticeLife::Progress,
            operation_id: 0,
            operation_warning: None,
            warning_notice: None,
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

    fn session_notice_target(&self) -> view::NoticeTarget {
        self.active_session
            .as_ref()
            .or_else(|| {
                self.model
                    .selected_session
                    .and_then(|index| self.model.sessions.get(index))
                    .map(|session| &session.name)
            })
            .map_or_else(
                || view::NoticeTarget::Host(self.target()),
                |name| view::NoticeTarget::Session {
                    target: self.target(),
                    name: name.clone(),
                },
            )
    }

    fn show_notice(
        &mut self,
        target: view::NoticeTarget,
        summary: impl Into<String>,
        detail: impl Into<String>,
        life: NoticeLife,
    ) {
        self.selection_copy = None;
        self.operation_copy = None;
        self.warning_notice = None;
        let detail = detail.into();
        self.model.notice = detail.clone();
        self.model.item_notice = Some(view::ItemNotice {
            target,
            summary: summary.into(),
            detail,
        });
        self.notice_life = life;
        self.render_dirty = true;
    }

    fn show_error(&mut self, target: view::NoticeTarget, summary: &str, detail: impl Into<String>) {
        self.show_notice(target, summary, detail, NoticeLife::Persistent);
    }

    fn show_progress(
        &mut self,
        target: view::NoticeTarget,
        summary: &str,
        detail: impl Into<String>,
    ) {
        self.show_notice(target, summary, detail, NoticeLife::Progress);
    }

    fn show_success(
        &mut self,
        target: view::NoticeTarget,
        summary: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let summary = summary.into();
        let detail = detail.into();
        let owner = view::ItemNotice {
            target: target.clone(),
            summary: summary.clone(),
            detail: detail.clone(),
        };
        self.show_notice(
            target,
            summary,
            detail,
            NoticeLife::Brief {
                until: Instant::now() + Duration::from_secs(3),
                owner,
            },
        );
    }

    fn expire_notice(&mut self, now: Instant) {
        if self.model.commands_open() {
            return;
        }
        if let NoticeLife::Brief { until, owner } = &self.notice_life {
            if now < *until {
                return;
            }
            if self.model.item_notice.as_ref() == Some(owner) {
                self.model.item_notice = None;
                self.render_dirty = true;
            }
            self.notice_life = NoticeLife::Persistent;
        }
    }

    fn complete_session_progress(&mut self) {
        if matches!(self.notice_life, NoticeLife::Progress)
            && self
                .model
                .item_notice
                .as_ref()
                .is_some_and(|notice| notice.target == self.session_notice_target())
        {
            self.model.item_notice = None;
            self.notice_life = NoticeLife::Persistent;
        }
    }

    fn operation_host_matches(&self, host: &Target) -> bool {
        self.operation_target
            .as_ref()
            .is_some_and(|target| match target {
                view::NoticeTarget::Host(target) | view::NoticeTarget::Session { target, .. } => {
                    target == host
                }
            })
    }

    fn show_operation_update(
        &mut self,
        target: view::NoticeTarget,
        summary: &str,
        detail: String,
        success: bool,
    ) {
        let waiting = self.operation_copy.take().filter(|copy| {
            copy.operation == self.operation_id
                && self.model.item_notice.as_ref() == Some(&copy.notice)
        });
        if success && waiting.is_some() {
            self.show_progress(target, "正在复制…", format!("{detail}，正在复制到剪贴板…"));
        } else if success {
            self.show_success(target, summary, detail);
        } else {
            self.show_error(target, summary, detail);
        }
        if let Some(copy) = waiting {
            self.operation_copy = self.model.item_notice.clone().map(|notice| OperationCopy {
                operation: copy.operation,
                notice,
            });
        }
    }

    fn selected_name(&self) -> Option<String> {
        self.model
            .selected_session
            .and_then(|i| self.model.sessions.get(i))
            .map(|session| session.name.clone())
    }

    fn terminal_close_target(&self) -> Option<view::TerminalClose> {
        if self.create_pending || self.close_pending.is_some() {
            return None;
        }
        let session = self
            .model
            .selected_session
            .and_then(|index| self.model.sessions.get(index))?;
        (!session.self_session && self.active_session.as_deref() == Some(session.name.as_str()))
            .then(|| view::TerminalClose {
                name: session.name.clone(),
                navigation_epoch: self.navigation_epoch,
            })
    }

    fn selection_context_matches(&self, context: &SelectionContext) -> bool {
        self.generation == context.generation
            && self.target() == context.target
            && self.active_session.as_deref() == Some(context.name.as_str())
            && self.selected_name().as_deref() == Some(context.name.as_str())
            && self
                .surface
                .as_ref()
                .and_then(surface::Surface::selection_identity)
                == Some(context.identity)
            && self.size == (context.pane.width, context.pane.height)
    }

    fn cancel_selection(&mut self) {
        self.selection_copy = None;
        if self.selection.take().is_some() {
            self.render_dirty = true;
        }
        self.model.selecting = false;
        self.model.selection_spans.clear();
        // Keep local_left until its release; cancellation must not leak an Up.
    }

    fn validate_selection(&mut self, pane: Rect) {
        if self.selection.as_ref().is_some_and(|drag| {
            drag.context.pane != pane || !self.selection_context_matches(&drag.context)
        }) {
            self.cancel_selection();
        }
    }

    fn selection_point(mouse: crossterm::event::MouseEvent, pane: Rect) -> selection::Point {
        selection::Point {
            column: mouse
                .column
                .saturating_sub(pane.x)
                .min(pane.width.saturating_sub(1)),
            row: mouse
                .row
                .saturating_sub(pane.y)
                .min(pane.height.saturating_sub(1)),
        }
    }

    fn selection_event(&mut self, event: &Event, geometry: &view::Geometry) -> Result<bool, Error> {
        use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEventKind};
        match event {
            Event::Resize(_, _) | Event::FocusLost => self.cancel_selection(),
            Event::Paste(_) => self.cancel_selection(),
            Event::Key(key)
                if key.kind != KeyEventKind::Release
                    && !matches!(key.code, KeyCode::Modifier(_)) =>
            {
                let selecting = self.selection.is_some();
                self.cancel_selection();
                if selecting && key.code == KeyCode::Esc {
                    return Ok(true);
                }
            }
            _ => {}
        }
        let Event::Mouse(mouse) = event else {
            return Ok(false);
        };
        if matches!(mouse.kind, MouseEventKind::Down(_)) {
            self.selection_copy = None;
        }
        self.validate_selection(geometry.terminal);
        if self.local_left {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
                    let point = Self::selection_point(*mouse, geometry.terminal);
                    if self
                        .selection
                        .as_mut()
                        .is_some_and(|drag| drag.value.update(point).is_err())
                    {
                        self.cancel_selection();
                    }
                    if matches!(mouse.kind, MouseEventKind::Up(_)) {
                        self.local_left = false;
                        if let Some(drag) = self.selection.take() {
                            self.model.selecting = false;
                            self.model.selection_spans.clear();
                            if self.selection_context_matches(&drag.context) {
                                match drag.value.text(clipboard::MAX_BYTES) {
                                    Ok(Some(value)) => {
                                        self.selection_copy_ticket =
                                            self.selection_copy_ticket.wrapping_add(1);
                                        let ticket = self.selection_copy_ticket;
                                        self.selection_copy = Some(SelectionCopy {
                                            ticket,
                                            operation: self.operation_id,
                                            notice: self.model.notice.clone(),
                                            context: drag.context,
                                        });
                                        if let Err(error) =
                                            self.clipboard.copy_selection(value, ticket)
                                        {
                                            self.selection_copy = None;
                                            self.show_error(
                                                self.session_notice_target(),
                                                "复制失败",
                                                format!("复制失败：{error}，请重新拖选"),
                                            );
                                        }
                                    }
                                    Ok(None) => {}
                                    Err(error) => self.show_error(
                                        self.session_notice_target(),
                                        "复制失败",
                                        error.to_string(),
                                    ),
                                }
                            }
                        }
                    }
                    self.render_dirty = true;
                    return Ok(true);
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    self.cancel_selection();
                    self.local_left = false;
                }
                MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => self.cancel_selection(),
                _ => return Ok(true),
            }
        }
        if let MouseEventKind::Drag(button) | MouseEventKind::Up(button) = mouse.kind {
            if self.mouse_pressed.contains(&button) {
                if self.model.modal_open() && matches!(mouse.kind, MouseEventKind::Drag(_)) {
                    return Ok(true);
                }
                let pane = geometry.terminal;
                if pane.width > 0 && pane.height > 0 {
                    let point = Self::selection_point(*mouse, pane);
                    let mut relative = *mouse;
                    relative.column = point.column;
                    relative.row = point.row;
                    self.action(Action::TerminalMouse(relative))?;
                }
                return Ok(true);
            }
        }
        let pane = geometry.terminal;
        let inside = pane.width > 0
            && pane.height > 0
            && mouse.column >= pane.x
            && mouse.column < pane.right()
            && mouse.row >= pane.y
            && mouse.row < pane.bottom();
        if mouse.kind == MouseEventKind::Down(MouseButton::Left)
            && inside
            && !self.model.modal_open()
            && !self.mouse_pressed.contains(&MouseButton::Left)
        {
            if let Some(surface) = self.surface.as_ref().filter(|surface| surface.ready) {
                if let Some(name) = self.selected_name().filter(|name| {
                    Some(name) == self.active_session.as_ref()
                        && (!surface.view().mouse_reporting
                            || mouse.modifiers.contains(KeyModifiers::SHIFT))
                }) {
                    self.selection_copy = None;
                    self.model.focus = view::Focus::Terminal;
                    self.local_left = true;
                    let point = Self::selection_point(*mouse, pane);
                    if point.column < surface.geometry().0
                        && usize::from(point.row) < surface.view().rows.len()
                    {
                        if let Some(snapshot) = surface.selection_snapshot(pane.width, pane.height)
                        {
                            let identity = snapshot.identity();
                            if let Ok(value) = selection::Selection::begin(snapshot, point) {
                                self.selection = Some(DragSelection {
                                    value,
                                    context: SelectionContext {
                                        generation: self.generation,
                                        target: self.target(),
                                        name,
                                        identity,
                                        pane,
                                    },
                                });
                            }
                        }
                    }
                    self.render_dirty = true;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    fn command(&mut self, command: Command) -> bool {
        let target = match &command {
            Command::KillSession { target, name } => view::NoticeTarget::Session {
                target: target.clone(),
                name: name.clone(),
            },
            _ => view::NoticeTarget::Host(command.target()),
        };
        match self.backend.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.selection_copy = None;
                self.show_error(target, "请求失败", error.to_string());
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
        self.cancel_selection();
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
        self.cancel_selection();
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
        self.selection_copy = None;
        self.operation_id = self.operation_id.wrapping_add(1);
        self.operation_fact = None;
        self.operation_target = None;
        self.operation_copy = None;
        self.operation_warning = None;
        self.warning_notice = None;
    }

    fn complete_progress(&mut self, expected: &[&str], ready: &str) {
        // Complete only a progress message still on screen. A concurrent action,
        // error or confirmed operation owns any other notice.
        if expected.contains(&self.model.notice.as_str()) {
            self.model.notice = ready.into();
            if matches!(self.notice_life, NoticeLife::Progress) {
                self.model.item_notice = None;
                self.notice_life = NoticeLife::Persistent;
            }
        }
    }

    fn select_session(&mut self, index: usize) -> Result<(), Error> {
        let Some(session) = self.model.sessions.get(index) else {
            return Ok(());
        };
        if session.self_session {
            self.show_error(
                view::NoticeTarget::Session {
                    target: self.target(),
                    name: session.name.clone(),
                },
                "当前承载会话",
                "这是承载当前 TUI 的会话，请选择其它会话或新建",
            );
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
        self.show_progress(self.session_notice_target(), "正在接入…", "正在接入会话…");
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
        // Passive successful polls keep a copy's ownership. Any new result or
        // error invalidates it even when the replacement notice has equal text.
        if !matches!(
            &event,
            backend::Event::Snapshot(_) | backend::Event::Sessions { result: Ok(_), .. }
        ) {
            self.selection_copy = None;
        }
        self.render_dirty = true;
        match event {
            backend::Event::SessionClosed {
                target,
                name,
                result,
            } => {
                let closed = ClosingSession {
                    target: target.clone(),
                    name: name.clone(),
                };
                if self.close_pending.as_ref() != Some(&closed) {
                    return;
                }
                self.close_pending = None;
                let succeeded = result.is_ok();
                match result {
                    Ok(()) => self.backend_event(backend::Event::Notice {
                        target: target.clone(),
                        message: format!("会话 {name} 已关闭"),
                        clipboard: None,
                        confirmation: backend::Confirmation::Sessions(target.clone()),
                    }),
                    Err(error) => {
                        let context = if target == self.target() {
                            "会话关闭失败"
                        } else {
                            "原主机会话关闭失败"
                        };
                        self.show_error(
                            view::NoticeTarget::Session {
                                target: target.clone(),
                                name: name.clone(),
                            },
                            "关闭失败",
                            format!("{context}：{error}"),
                        );
                    }
                }
                let target = view::NoticeTarget::Session { target, name };
                if succeeded {
                    self.show_success(target.clone(), "会话已关闭", self.model.notice.clone());
                }
                self.operation_target = Some(target);
            }
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
                    let owned = self
                        .warning_notice
                        .as_ref()
                        .is_some_and(|owner| self.model.item_notice.as_ref() == Some(owner));
                    self.selection_copy = None;
                    self.operation_warning = None;
                    self.warning_notice = None;
                    let detail = self
                        .operation_fact
                        .as_ref()
                        .map(|fact| fact.text.clone())
                        .unwrap_or_else(|| "主机状态已恢复".into());
                    let target = self
                        .operation_target
                        .clone()
                        .unwrap_or(view::NoticeTarget::Host(Target::Local));
                    if owned {
                        self.show_operation_update(target, "状态已恢复", detail, true);
                    }
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
                            let owned = self.warning_notice.as_ref().is_some_and(|owner| {
                                self.model.item_notice.as_ref() == Some(owner)
                            });
                            self.selection_copy = None;
                            self.operation_warning = None;
                            self.warning_notice = None;
                            let related = self.operation_host_matches(&target);
                            let detail = self
                                .operation_fact
                                .as_ref()
                                .filter(|_| related)
                                .map(|fact| fact.text.clone())
                                .unwrap_or_else(|| "会话列表已恢复".into());
                            let owner = self
                                .operation_target
                                .clone()
                                .filter(|_| related)
                                .unwrap_or_else(|| view::NoticeTarget::Host(target.clone()));
                            if owned {
                                self.show_operation_update(owner, "状态已恢复", detail, true);
                            }
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
                        let related = self.operation_host_matches(&target);
                        let fact = self
                            .operation_fact
                            .as_ref()
                            .filter(|_| related)
                            .map(|fact| fact.text.as_str())
                            .unwrap_or("当前主机会话");
                        let warning = format!("会话列表刷新失败：{error}");
                        let detail = format!("{fact}；{warning}");
                        if related {
                            self.show_operation_update(
                                view::NoticeTarget::Host(target.clone()),
                                "会话读取失败",
                                detail,
                                false,
                            );
                        } else {
                            self.show_error(
                                view::NoticeTarget::Host(target.clone()),
                                "会话读取失败",
                                detail,
                            );
                        }
                        self.operation_warning =
                            Some((backend::Confirmation::Sessions(target), warning));
                        self.warning_notice = self.model.item_notice.clone();
                    }
                }
            }
            backend::Event::Created {
                target,
                name,
                epoch,
            } => {
                self.create_pending = false;
                self.create_target = None;
                if target != self.target() || epoch != self.navigation_epoch {
                    self.show_success(
                        view::NoticeTarget::Session {
                            target,
                            name: name.clone(),
                        },
                        "会话已创建",
                        format!("会话 {name} 已在原主机创建，选择该主机即可接入"),
                    );
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
                    self.show_error(self.session_notice_target(), "接入失败", error.to_string());
                }
                self.sessions_pending = false;
                self.refresh_sessions();
            }
            backend::Event::Notice {
                target,
                message,
                clipboard,
                confirmation,
            } => {
                if matches!(&confirmation, backend::Confirmation::Sessions(target) if target != &self.target())
                {
                    self.show_success(
                        view::NoticeTarget::Host(target),
                        message.clone(),
                        format!("原主机：{message}"),
                    );
                    return;
                }
                self.operation_id = self.operation_id.wrapping_add(1);
                self.operation_warning = None;
                self.operation_target = Some(view::NoticeTarget::Host(target.clone()));
                self.operation_fact = Some(OperationFact {
                    text: message.clone(),
                    confirmation,
                });
                self.show_success(
                    view::NoticeTarget::Host(target.clone()),
                    message.clone(),
                    message.clone(),
                );
                if let Some(value) = clipboard {
                    match self
                        .clipboard
                        .copy(value, self.operation_id, message.clone())
                    {
                        Ok(()) => {
                            self.show_progress(
                                view::NoticeTarget::Host(target),
                                "正在复制…",
                                format!("{message}，正在复制到剪贴板…"),
                            );
                            self.operation_copy =
                                self.model.item_notice.clone().map(|notice| OperationCopy {
                                    operation: self.operation_id,
                                    notice,
                                });
                        }
                        Err(error) => self.show_error(
                            view::NoticeTarget::Host(target),
                            "复制失败",
                            format!("{message}；{error}"),
                        ),
                    }
                }
                self.sessions_pending = false;
                self.refresh_sessions();
            }
            backend::Event::Failed { target, error } => {
                self.clear_operation();
                self.refresh_pending = false;
                self.sessions_pending = false;
                self.show_error(
                    view::NoticeTarget::Host(target),
                    "操作失败",
                    error.to_string(),
                );
            }
            backend::Event::CreateFailed {
                target,
                epoch,
                error,
            } => {
                self.clear_operation();
                self.create_pending = false;
                self.create_target = None;
                let current = target == self.target() && epoch == self.navigation_epoch;
                let context = if current {
                    "创建会话未确认（若已生效，会在列表中保留）"
                } else {
                    "原主机创建会话未确认，请查看该主机会话列表"
                };
                self.show_error(
                    view::NoticeTarget::Host(target),
                    "创建未确认",
                    format!("{context}：{error}"),
                );
                if current {
                    self.sessions_pending = false;
                    self.refresh_sessions();
                }
            }
            backend::Event::Warning {
                target,
                message,
                error,
                confirmation,
            } => {
                self.operation_id = self.operation_id.wrapping_add(1);
                self.operation_warning = None;
                self.operation_target = Some(view::NoticeTarget::Host(target.clone()));
                let summary = if confirmation == backend::Confirmation::None {
                    "服务未就绪"
                } else {
                    "操作部分完成"
                };
                self.show_error(
                    view::NoticeTarget::Host(target),
                    summary,
                    format!("{message}：{error}"),
                );
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
                let detail = format!("{fact}；{warning}");
                let target = self
                    .operation_target
                    .clone()
                    .unwrap_or(view::NoticeTarget::Host(Target::Local));
                self.show_operation_update(target, "状态刷新失败", detail, false);
                self.operation_warning = Some((backend::Confirmation::Snapshot, warning));
                self.warning_notice = self.model.item_notice.clone();
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
                    self.show_error(self.session_notice_target(), "终端错误", error.to_string());
                    self.disconnect();
                    break;
                }
            };
            match frame.op {
                Op::ATTACHED => {
                    match frames::dec_attached(&frame.payload) {
                        Ok(_) => {
                            if matches!(self.notice_life, NoticeLife::Progress)
                                && self.model.item_notice.as_ref().is_some_and(|notice| {
                                    notice.target == self.session_notice_target()
                                })
                            {
                                self.show_progress(
                                    self.session_notice_target(),
                                    "正在读取画面…",
                                    "已连接，正在读取会话画面…",
                                );
                            }
                        }
                        Err(error) => {
                            self.show_error(
                                self.session_notice_target(),
                                "终端错误",
                                error.to_string(),
                            );
                            self.disconnect();
                            break;
                        }
                    }
                }
                Op::SNAPSHOT | Op::SURFACE_DIFF | Op::FETCH_ROWS => {
                    if let Err(error) = self.surface_frame(frame.op, &frame.payload) {
                        self.show_error(
                            self.session_notice_target(),
                            "终端错误",
                            error.to_string(),
                        );
                        self.disconnect();
                        break;
                    }
                }
                Op::ENDED => {
                    let (code, reason) = frames::dec_ended(&frame.payload);
                    self.show_error(
                        self.session_notice_target(),
                        "会话已结束",
                        format!("会话已结束（{code}，{reason}），可选择其它会话或创建新会话"),
                    );
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
        let mut initial_ready = false;
        let mut resync = false;
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
                    self.selection_copy = None;
                    initial_ready = true;
                    if self.model.focus == view::Focus::Terminal
                        && !self.model.modal_open()
                        && surface.focus_events()
                    {
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
                self.selection_copy = None;
                resync = true;
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
        if initial_ready {
            self.complete_session_progress();
            // Kept internally for request ownership; healthy attachment has no row notice.
            if self.model.item_notice.is_none() {
                self.model.notice = "已接入 · 滚轮浏览历史".into();
            }
        }
        if resync {
            self.show_progress(
                self.session_notice_target(),
                "正在同步…",
                "正在同步终端画面…",
            );
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
        let mut fetch_error = None;
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
                        self.show_error(
                            self.session_notice_target(),
                            "终端错误",
                            error.to_string(),
                        );
                        self.disconnect();
                        return;
                    }
                }
            }
        } else if let Some(request) = surface.fetch_request(self.size.1, now) {
            if let Err(error) = attach.fetch_rows(request) {
                surface.fetch_failed();
                if !matches!(error, backend::Error::Busy) {
                    self.selection_copy = None;
                    fetch_error = Some(error.to_string());
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
                        self.show_error(
                            self.session_notice_target(),
                            "终端错误",
                            error.to_string(),
                        );
                        self.disconnect();
                    }
                }
            }
        }
        if let Some(error) = fetch_error {
            self.show_error(self.session_notice_target(), "历史读取失败", error);
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
                self.show_error(self.session_notice_target(), "终端错误", error.to_string());
                self.disconnect();
            }
        }
    }

    fn term_error(&mut self, payload: &[u8]) {
        if let Some(surface) = &mut self.surface {
            surface.fetch_failed();
        }
        self.clear_operation();
        let detail = match frames::dec_error(payload) {
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
        self.show_error(self.session_notice_target(), "终端操作失败", detail);
    }

    fn process_clipboard(&mut self) -> Result<(), Error> {
        while let Some(result) = self.clipboard.try_result() {
            self.clipboard_result(result)?;
        }
        Ok(())
    }

    fn clipboard_result(&mut self, result: clipboard::CopyResult) -> Result<(), Error> {
        self.render_dirty = true;
        if result.purpose == clipboard::Purpose::Selection {
            let Some(copy) = self
                .selection_copy
                .as_ref()
                .filter(|copy| copy.ticket == result.operation)
            else {
                return Ok(());
            };
            let owned = copy.operation == self.operation_id
                && copy.notice == self.model.notice
                && self.selection_context_matches(&copy.context);
            let target = view::NoticeTarget::Session {
                target: copy.context.target.clone(),
                name: copy.context.name.clone(),
            };
            self.selection_copy = None;
            if owned {
                match result.status {
                    clipboard::Copied::System => {
                        self.show_success(target, "已复制", "选中内容已复制到系统剪贴板")
                    }
                    clipboard::Copied::Terminal(sequence) => {
                        io::stdout().write_all(sequence.as_bytes())?;
                        io::stdout().flush()?;
                        self.show_success(target, "已发复制请求", "已提交终端剪贴板请求");
                    }
                    clipboard::Copied::Failed(error) => self.show_error(
                        target,
                        "复制失败",
                        format!("复制失败：{error}，请重新拖选"),
                    ),
                }
            }
            return Ok(());
        }
        // Reject stale operation output before emitting an OSC52 sequence, too.
        let Some(copy) = self.operation_copy.as_ref().filter(|copy| {
            result.operation == self.operation_id
                && copy.operation == result.operation
                && self.model.item_notice.as_ref() == Some(&copy.notice)
        }) else {
            return Ok(());
        };
        let target = copy.notice.target.clone();
        self.operation_copy = None;
        let (summary, status, failed) = match result.status {
            clipboard::Copied::System => ("已复制", "已复制到系统剪贴板".into(), false),
            clipboard::Copied::Terminal(sequence) => {
                io::stdout().write_all(sequence.as_bytes())?;
                io::stdout().flush()?;
                (
                    "已发复制请求",
                    "已发送剪贴板请求（终端需允许 OSC52）".into(),
                    false,
                )
            }
            clipboard::Copied::Failed(error) => {
                ("复制失败", format!("操作完成，复制失败：{error}"), true)
            }
        };
        let mut detail = format!("{}；{status}", result.fact);
        if let Some((_, warning)) = &self.operation_warning {
            detail.push_str(&format!("；{warning}"));
        }
        if failed || self.operation_warning.is_some() {
            self.show_error(target, summary, detail);
            if self.operation_warning.is_some() {
                self.warning_notice = self.model.item_notice.clone();
            }
        } else {
            self.show_success(target, summary, detail);
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
            self.selection_copy = None;
            self.show_error(
                self.session_notice_target(),
                "输入队列已满",
                "输入队列已满，请等待远端接收后重试",
            );
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
                    self.show_error(self.session_notice_target(), "终端错误", error.to_string());
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
        if self.size != (cols, rows) {
            self.cancel_selection();
        }
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
                    self.selection_copy = None;
                    self.show_error(self.session_notice_target(), "终端错误", error.to_string());
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
                self.show_progress(
                    view::NoticeTarget::Host(self.target()),
                    "正在读取会话…",
                    READING_SESSIONS_NOTICE,
                );
                self.refresh_sessions();
            }
            Action::SelectSession(index) => {
                self.clear_operation();
                self.select_session(index)?;
            }
            Action::AddHost { name, token } => {
                self.clear_operation();
                self.show_progress(
                    view::NoticeTarget::Host(Target::Local),
                    "正在添加…",
                    "正在添加主机…",
                );
                self.command(Command::AddHost { name, token });
            }
            Action::CreateSession => {
                if self.create_pending {
                    self.show_progress(
                        view::NoticeTarget::Host(
                            self.create_target.clone().unwrap_or_else(|| self.target()),
                        ),
                        "正在创建…",
                        "正在创建会话，请稍候…",
                    );
                    return Ok(false);
                }
                self.clear_operation();
                let target = self.target();
                self.show_progress(
                    view::NoticeTarget::Host(target.clone()),
                    "正在创建…",
                    "正在创建会话，请稍候…",
                );
                self.create_pending = self.command(Command::NewSession {
                    target: target.clone(),
                    epoch: self.navigation_epoch,
                });
                self.create_target = self.create_pending.then_some(target);
            }
            Action::ToggleExit => {
                self.clear_operation();
                self.show_progress(
                    view::NoticeTarget::Host(Target::Local),
                    "正在更新出口…",
                    "正在更新出口状态…",
                );
                self.command(Command::SetRole {
                    role: backend::Role::Exit,
                    on: !self.snapshot.serve_on,
                });
            }
            Action::ToggleRelay => {
                self.clear_operation();
                self.show_progress(
                    view::NoticeTarget::Host(Target::Local),
                    "正在更新中继…",
                    "正在更新中继状态…",
                );
                self.command(Command::SetRole {
                    role: backend::Role::Relay,
                    on: !self.snapshot.relay_on,
                });
            }
            Action::ToggleProxy => {
                self.clear_operation();
                if let Target::Remote(id) = self.target() {
                    let on = !self.model.hosts[self.model.selected_host].proxy_on;
                    self.show_progress(
                        view::NoticeTarget::Host(Target::Remote(id.clone())),
                        "正在更新代理…",
                        "正在更新代理状态…",
                    );
                    self.command(Command::SetProxy { id, on });
                }
            }
            Action::DeleteHost => {
                self.clear_operation();
                if let Target::Remote(id) = self.target() {
                    self.navigation_epoch = self.navigation_epoch.wrapping_add(1);
                    self.show_progress(
                        view::NoticeTarget::Host(Target::Remote(id.clone())),
                        "正在删除…",
                        "正在删除主机…",
                    );
                    self.command(Command::RemoveHost { id });
                }
            }
            Action::CloseSession => {
                if self.close_pending.is_some() {
                    let target = self
                        .close_pending
                        .as_ref()
                        .map(|closing| view::NoticeTarget::Session {
                            target: closing.target.clone(),
                            name: closing.name.clone(),
                        })
                        .unwrap_or_else(|| self.session_notice_target());
                    self.show_progress(target, "正在关闭…", "正在关闭会话，请稍候…");
                    return Ok(false);
                }
                if self
                    .model
                    .selected_session
                    .and_then(|index| self.model.sessions.get(index))
                    .is_some_and(|session| session.self_session)
                {
                    self.show_error(
                        self.session_notice_target(),
                        "当前承载会话",
                        "承载当前 TUI 的会话不能在此关闭；退出 TUI 后再关闭",
                    );
                    return Ok(false);
                }
                self.clear_operation();
                if let Some(name) = self.selected_name() {
                    self.navigation_epoch = self.navigation_epoch.wrapping_add(1);
                    self.sessions_epoch = self.sessions_epoch.wrapping_add(1);
                    let target = self.target();
                    self.show_progress(
                        view::NoticeTarget::Session {
                            target: target.clone(),
                            name: name.clone(),
                        },
                        "正在关闭…",
                        "正在关闭会话，请稍候…",
                    );
                    self.sessions_pending = self.command(Command::KillSession {
                        target: target.clone(),
                        name: name.clone(),
                    });
                    if self.sessions_pending {
                        self.close_pending = Some(ClosingSession { target, name });
                    }
                }
            }
            Action::CloseTerminalSession(target) => {
                if self.terminal_close_target().as_ref() == Some(&target) {
                    return self.action(Action::CloseSession);
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
                    self.show_error(
                        self.session_notice_target(),
                        "粘贴内容过长",
                        "粘贴超过 1 MiB，请分批粘贴",
                    );
                } else {
                    let chunks = input::paste(&text);
                    let bytes = text.len() + chunks.len() * 4;
                    if self.input_bytes + bytes > 2 << 20 {
                        self.show_error(
                            self.session_notice_target(),
                            "输入队列忙",
                            "输入队列忙，请稍后粘贴",
                        );
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
                if matches!(event.kind, MouseEventKind::Up(_) | MouseEventKind::Drag(_)) && !paired
                {
                    return Ok(false);
                }
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
        target: Target::Local,
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
        target: Target::Remote(host.id.clone()),
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
        .map(Path::new)
        .map(|cwd| cwd.file_name().unwrap_or(cwd.as_os_str()))
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
    use ratatui::buffer::CellWidth;

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
        let footer_area = geometry.footer_area();
        let mut footer = String::new();
        let mut column = footer_area.x;
        while column < footer_area.right() {
            let cell = &buffer[(column, footer_area.y)];
            footer.push_str(cell.symbol());
            column += cell.cell_width().max(1);
        }
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

    fn selection_app() -> (App, view::Geometry) {
        let mut app = history_app();
        app.connected = true;
        app.surface.as_mut().unwrap().prepare(app.size.1);
        let mut geometry = view::Geometry::default();
        geometry.terminal = Rect::new(30, 1, 66, 27);
        (app, geometry)
    }

    fn left(kind: crossterm::event::MouseEventKind, x: u16, y: u16, shift: bool) -> Event {
        Event::Mouse(crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: if shift {
                crossterm::event::KeyModifiers::SHIFT
            } else {
                crossterm::event::KeyModifiers::NONE
            },
        })
    }

    #[test]
    fn local_selection_click_freeze_and_cancellation_never_send_input() {
        use crossterm::event::{
            MouseButton::Left,
            MouseEventKind::{Down, Drag, Up},
        };
        let (mut app, geometry) = selection_app();
        let notice = app.model.notice.clone();
        app.model.focus = view::Focus::Sessions;
        assert!(app
            .selection_event(&left(Down(Left), 30, 1, false), &geometry)
            .unwrap());
        assert!(app
            .selection_event(&left(Up(Left), 30, 1, false), &geometry)
            .unwrap());
        assert_eq!(app.model.focus, view::Focus::Terminal);
        assert!(
            app.selection_copy.is_none(),
            "a click must not request a clipboard write"
        );
        assert!(app
            .selection_event(&left(Down(Left), 30, 1, false), &geometry)
            .unwrap());
        app.selection_event(&left(Drag(Left), 37, 1, true), &geometry)
            .unwrap();
        let text = app
            .selection
            .as_ref()
            .unwrap()
            .value
            .text(clipboard::MAX_BYTES)
            .unwrap();
        assert!(text.as_ref().is_some_and(|text| text.starts_with("LINE_")));
        // Passive output can move the live viewport without changing the frozen rows.
        app.surface
            .as_mut()
            .unwrap()
            .scroll(view::HistoryAction::Start, 27);
        app.surface.as_mut().unwrap().prepare(27);
        assert_eq!(
            app.selection
                .as_ref()
                .unwrap()
                .value
                .text(clipboard::MAX_BYTES)
                .unwrap()
                .as_deref(),
            text.as_deref()
        );
        app.selection_event(&Event::Resize(80, 24), &geometry)
            .unwrap();
        assert!(app.selection.is_none());
        assert!(
            app.selection_event(&left(Up(Left), 200, 200, false), &geometry)
                .unwrap(),
            "cancelled local release stays local outside the pane"
        );
        assert!(!app.local_left);
        assert!(app.input_queue.is_empty());
        assert!(app.input_geometry.is_none());
        assert_eq!(app.model.notice, notice);
    }

    #[test]
    fn local_release_copies_exact_frozen_text_without_activating_observer() {
        use crossterm::event::{
            MouseButton::Left,
            MouseEventKind::{Down, Drag, Up},
        };
        let (mut app, geometry) = selection_app();
        let (clipboard, recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        let notice = app.model.notice.clone();
        app.selection_event(&left(Down(Left), 30, 1, false), &geometry)
            .unwrap();
        app.selection_event(&left(Drag(Left), 37, 1, false), &geometry)
            .unwrap();
        let expected = app
            .selection
            .as_ref()
            .unwrap()
            .value
            .text(clipboard::MAX_BYTES)
            .unwrap()
            .unwrap();
        app.selection_event(&left(Up(Left), 37, 1, false), &geometry)
            .unwrap();
        let (actual, ticket) = recorded.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(actual, expected);
        assert_eq!(ticket, app.selection_copy.as_ref().unwrap().ticket);
        assert!(app.selection.is_none());
        assert!(!app.local_left);
        assert!(app.input_queue.is_empty());
        assert!(
            app.input_geometry.is_none(),
            "copy never activates an observer's pane geometry"
        );
        assert_eq!(
            app.model.notice, notice,
            "requesting a copy does not create a sticky progress notice"
        );
    }

    #[test]
    fn application_mouse_route_is_locked_at_press_and_shift_can_start_local() {
        use crossterm::event::{
            MouseButton::Left,
            MouseEventKind::{Down, Drag, Up},
        };
        let (mut app, geometry) = selection_app();
        let mut vt = homeway_core::term::vt::SessionVt::new(66, 27, 0).unwrap();
        vt.write(b"\x1b[?1000h\x1b[?1006h");
        app.surface.as_mut().unwrap().modes = codec::surface_modes_of(&vt.modes()).0;
        let down = left(Down(Left), 32, 4, false);
        assert!(!app.selection_event(&down, &geometry).unwrap());
        let Event::Mouse(mut mouse) = down else {
            unreachable!()
        };
        mouse.column -= geometry.terminal.x;
        mouse.row -= geometry.terminal.y;
        app.action(Action::TerminalMouse(mouse)).unwrap();
        assert!(app
            .selection_event(&left(Drag(Left), 200, 100, true), &geometry)
            .unwrap());
        assert!(app
            .selection_event(&left(Up(Left), 200, 100, true), &geometry)
            .unwrap());
        assert_eq!(app.input_queue.len(), 3);
        assert!(app.mouse_pressed.is_empty());
        assert!(app.selection.is_none());
        let queued = app.input_queue.len();
        assert!(app
            .selection_event(&left(Down(Left), 32, 4, true), &geometry)
            .unwrap());
        app.selection_event(&left(Drag(Left), 36, 4, false), &geometry)
            .unwrap();
        assert!(
            app.selection.is_some(),
            "modifier changes cannot transfer a local gesture to the application"
        );
        app.cancel_selection();
        assert!(app
            .selection_event(&left(Up(Left), 200, 100, false), &geometry)
            .unwrap());
        assert_eq!(app.input_queue.len(), queued);
    }

    fn pending_selection_copy(app: &mut App, geometry: &view::Geometry, ticket: u64) {
        app.selection_copy = Some(SelectionCopy {
            ticket,
            operation: app.operation_id,
            notice: app.model.notice.clone(),
            context: SelectionContext {
                generation: app.generation,
                target: app.target(),
                name: app.active_session.clone().unwrap(),
                identity: app.surface.as_ref().unwrap().selection_identity().unwrap(),
                pane: geometry.terminal,
            },
        });
    }

    #[test]
    fn selection_clipboard_result_requires_current_context_and_notice_owner() {
        let (mut app, geometry) = selection_app();
        pending_selection_copy(&mut app, &geometry, 1);
        app.clipboard_result(clipboard::CopyResult {
            operation: 1,
            purpose: clipboard::Purpose::Selection,
            fact: "unused".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert_eq!(app.model.notice, "选中内容已复制到系统剪贴板");
        pending_selection_copy(&mut app, &geometry, 2);
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: app.model.notice.clone(),
            clipboard: None,
            confirmation: backend::Confirmation::None,
        });
        assert!(
            app.selection_copy.is_none(),
            "a new same-text notice still owns a new result"
        );
        app.clipboard_result(clipboard::CopyResult {
            operation: 2,
            purpose: clipboard::Purpose::Selection,
            fact: "unused".into(),
            status: clipboard::Copied::Terminal("MUST_NOT_WRITE".into()),
        })
        .unwrap();
        assert_eq!(app.model.notice, "选中内容已复制到系统剪贴板");
        pending_selection_copy(&mut app, &geometry, 3);
        app.detach();
        let notice = app.model.notice.clone();
        app.clipboard_result(clipboard::CopyResult {
            operation: 3,
            purpose: clipboard::Purpose::Selection,
            fact: "unused".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert_eq!(app.model.notice, notice);
    }

    #[test]
    fn same_text_error_and_new_mouse_down_invalidate_pending_selection_copy() {
        let (mut app, geometry) = selection_app();
        app.model.notice = backend::Error::Busy.to_string();
        pending_selection_copy(&mut app, &geometry, 1);
        app.backend_event(backend::Event::Failed {
            target: Target::Local,
            error: backend::Error::Busy,
        });
        let notice = app.model.notice.clone();
        app.clipboard_result(clipboard::CopyResult {
            operation: 1,
            purpose: clipboard::Purpose::Selection,
            fact: String::new(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert_eq!(app.model.notice, notice);
        pending_selection_copy(&mut app, &geometry, 2);
        assert!(!app
            .selection_event(
                &left(
                    crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
                    1,
                    1,
                    false
                ),
                &geometry
            )
            .unwrap());
        assert!(
            app.selection_copy.is_none(),
            "even a non-terminal click owns a newer interaction"
        );
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
        assert_eq!(screen(&mut app, &mut terminal).1.trim(), "^B 快捷指令");
        app.action(Action::Scroll(view::HistoryAction::Start))
            .unwrap();
        let footer = screen(&mut app, &mut terminal).1;
        let view = app.surface.as_ref().unwrap().view();
        let distance = view.scrollbar.offset.saturating_sub(view.top);
        assert!(distance > 0);
        assert_eq!(footer.trim(), format!("{distance}行 ⇧End回底"));
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
        assert_eq!(footer.trim(), "^B 快捷指令");
        app.action(Action::Scroll(view::HistoryAction::Start))
            .unwrap();
        let footer = screen(&mut app, &mut terminal).1;
        let view = app.surface.as_ref().unwrap().view();
        let distance = view.scrollbar.offset.saturating_sub(view.top);
        assert!(distance > 0);
        assert_eq!(footer.trim(), format!("{distance}行 ⇧End回底"));
        app.backend_event(backend::Event::Sessions {
            target: Target::Local,
            epoch: app.sessions_epoch,
            result: Ok(vec![]),
        });
        assert!(app.surface.is_none() && app.model.selected_session.is_none());
        let footer = screen(&mut app, &mut terminal).1;
        assert_eq!(footer.trim(), "^B 快捷指令");
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
    fn status_rows_keep_original_host_and_do_not_merge_a_different_hosts_fact() {
        let mut app = app();
        let remote = Target::Remote("remote".into());
        app.current_target = remote.clone();
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "出口已启动".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Snapshot,
        });
        assert_eq!(
            app.model.item_notice.as_ref().unwrap().target,
            view::NoticeTarget::Host(Target::Local)
        );
        app.backend_event(backend::Event::Sessions {
            target: remote.clone(),
            epoch: app.sessions_epoch,
            result: Err(backend::Error::Timeout),
        });
        let notice = app.model.item_notice.as_ref().unwrap();
        assert_eq!(notice.target, view::NoticeTarget::Host(remote));
        assert!(!notice.detail.contains("出口已启动"));
    }

    #[test]
    fn brief_notice_waits_for_popup_and_expires_only_its_owned_row() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app();
        let owner = view::NoticeTarget::Host(Target::Local);
        app.show_success(owner.clone(), "已复制", "已复制到系统剪贴板");
        let later = Instant::now() + Duration::from_secs(4);
        let geometry = view::Geometry::default();
        view::handle_event(
            &mut app.model,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            &geometry,
        );
        app.expire_notice(later);
        assert!(
            app.model.item_notice.is_some(),
            "reading full details must not expire the row"
        );
        view::handle_event(
            &mut app.model,
            Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            &geometry,
        );
        app.expire_notice(later);
        assert!(
            app.model.item_notice.is_none(),
            "metadata returns after the brief result"
        );
        app.show_success(owner.clone(), "已复制", "已复制到系统剪贴板");
        app.model.item_notice = Some(view::ItemNotice {
            target: owner,
            summary: "当前承载会话".into(),
            detail: "新说明".into(),
        });
        app.expire_notice(Instant::now() + Duration::from_secs(4));
        assert_eq!(app.model.item_notice.as_ref().unwrap().detail, "新说明");
    }

    #[test]
    fn operation_copy_cannot_overwrite_a_new_same_text_error_or_emit_stale_osc52() {
        let mut app = app();
        let (clipboard, _recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "出口已启动".into(),
            clipboard: Some("token".into()),
            confirmation: backend::Confirmation::Snapshot,
        });
        let operation = app.operation_id;
        let detail = app.model.notice.clone();
        app.show_error(
            view::NoticeTarget::Host(Target::Local),
            "新错误",
            detail.clone(),
        );
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
            operation,
            fact: "出口已启动".into(),
            status: clipboard::Copied::Terminal("MUST_NOT_WRITE".into()),
        })
        .unwrap();
        assert_eq!(app.model.notice, detail);
        assert_eq!(app.model.item_notice.as_ref().unwrap().summary, "新错误");
    }

    #[test]
    fn older_operation_copy_result_keeps_the_newer_pending_copy() {
        let mut app = app();
        let (clipboard, _recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        let notice = |message: &str| backend::Event::Notice {
            target: Target::Local,
            message: message.into(),
            clipboard: Some("token".into()),
            confirmation: backend::Confirmation::Snapshot,
        };
        app.backend_event(notice("出口已启动"));
        let old = app.operation_id;
        app.backend_event(notice("中继已启动"));
        let new = app.operation_id;
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
            operation: old,
            fact: "出口已启动".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert_eq!(app.operation_copy.as_ref().unwrap().operation, new);
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
            operation: new,
            fact: "中继已启动".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert!(app.operation_copy.is_none());
        assert_eq!(app.model.item_notice.as_ref().unwrap().summary, "已复制");
        assert!(app.model.notice.contains("中继已启动") && app.model.notice.contains("已复制"));
    }

    #[test]
    fn successful_poll_does_not_replace_a_newer_view_explanation() {
        let mut app = app();
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        let newer = view::ItemNotice {
            target: view::NoticeTarget::Session {
                target: Target::Local,
                name: "self".into(),
            },
            summary: "当前承载会话".into(),
            detail: "请使用其它会话".into(),
        };
        app.model.notice = newer.detail.clone();
        app.model.item_notice = Some(newer.clone());
        app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
        assert_eq!(app.model.item_notice, Some(newer));
        assert!(app.operation_warning.is_none());
    }

    #[test]
    fn refresh_recovery_keeps_a_pending_copy_until_its_real_result() {
        let mut app = app();
        let (clipboard, _recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "出口已启动".into(),
            clipboard: Some("token".into()),
            confirmation: backend::Confirmation::Snapshot,
        });
        let operation = app.operation_id;
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
        assert!(matches!(app.notice_life, NoticeLife::Progress));
        app.expire_notice(Instant::now() + Duration::from_secs(30));
        assert!(app.model.item_notice.is_some() && app.operation_copy.is_some());
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
            operation,
            fact: "出口已启动".into(),
            status: clipboard::Copied::System,
        })
        .unwrap();
        assert_eq!(app.model.item_notice.as_ref().unwrap().summary, "已复制");
    }

    #[test]
    fn closed_session_brief_uses_its_final_target_and_expires() {
        let mut app = app();
        app.close_pending = Some(ClosingSession {
            target: Target::Local,
            name: "gone".into(),
        });
        app.backend_event(backend::Event::SessionClosed {
            target: Target::Local,
            name: "gone".into(),
            result: Ok(()),
        });
        assert_eq!(
            app.model.item_notice.as_ref().unwrap().target,
            view::NoticeTarget::Session {
                target: Target::Local,
                name: "gone".into()
            }
        );
        app.expire_notice(Instant::now() + Duration::from_secs(4));
        assert!(
            app.model.item_notice.is_none(),
            "closed-row fallback must restore host metadata too"
        );
    }

    #[test]
    fn duplicate_creation_status_stays_on_its_original_host() {
        let mut app = app();
        app.create_pending = true;
        app.create_target = Some(Target::Local);
        app.current_target = Target::Remote("new-host".into());
        app.action(Action::CreateSession).unwrap();
        assert_eq!(
            app.model.item_notice.as_ref().unwrap().target,
            view::NoticeTarget::Host(Target::Local)
        );
    }

    #[test]
    fn terminal_close_rechecks_displayed_context_and_matching_completion() {
        let mut app = app();
        app.model.sessions = vec![session_item(backend::Session {
            name: "a".into(),
            ..Default::default()
        })];
        app.model.selected_session = Some(0);
        app.active_session = Some("a".into());
        let displayed = app.terminal_close_target().unwrap();
        app.create_pending = true;
        assert!(app.terminal_close_target().is_none());
        app.create_pending = false;
        app.action(Action::CloseTerminalSession(displayed.clone()))
            .unwrap();
        assert_eq!(app.close_pending.as_ref().unwrap().name, "a");
        let epoch = app.navigation_epoch;
        app.action(Action::CloseTerminalSession(displayed.clone()))
            .unwrap();
        assert_eq!(
            app.navigation_epoch, epoch,
            "duplicate press cannot enqueue a second close"
        );
        app.backend_event(backend::Event::Failed {
            target: Target::Local,
            error: backend::Error::Busy,
        });
        assert!(
            app.close_pending.is_some(),
            "unrelated failure cannot complete close"
        );
        app.backend_event(backend::Event::SessionClosed {
            target: Target::Local,
            name: "different".into(),
            result: Ok(()),
        });
        assert!(app.close_pending.is_some());
        app.current_target = Target::Remote("other-host".into());
        app.model.sessions[0].name = "new".into();
        app.active_session = Some("new".into());
        app.backend_event(backend::Event::SessionClosed {
            target: Target::Local,
            name: "a".into(),
            result: Err(backend::Error::Busy),
        });
        assert!(app.close_pending.is_none());
        assert_eq!(app.selected_name().as_deref(), Some("new"));
        assert_eq!(app.active_session.as_deref(), Some("new"));
        assert!(app.model.notice.contains("原主机"));
        app.navigation_epoch = app.navigation_epoch.wrapping_add(1);
        app.model.sessions[0].name = "a".into();
        app.active_session = Some("a".into());
        app.action(Action::CloseTerminalSession(displayed)).unwrap();
        assert!(
            app.close_pending.is_none(),
            "old same-name target cannot close a new host context"
        );
        let current = app.terminal_close_target().unwrap();
        app.action(Action::CloseTerminalSession(current)).unwrap();
        assert_eq!(
            app.close_pending.as_ref().unwrap().target,
            Target::Remote("other-host".into())
        );
    }

    #[test]
    fn context_menu_close_still_accepts_an_unshown_selection() {
        let mut app = app();
        app.model.sessions = vec![session_item(backend::Session {
            name: "unshown".into(),
            ..Default::default()
        })];
        app.model.selected_session = Some(0);
        app.active_session = Some("shown".into());
        assert!(app.terminal_close_target().is_none());
        app.action(Action::CloseSession).unwrap();
        assert_eq!(app.close_pending.as_ref().unwrap().name, "unshown");
        assert_eq!(app.active_session.as_deref(), Some("shown"));
    }

    #[test]
    fn initial_successful_snapshot_finishes_real_startup_notice_in_empty_view() {
        let mut app = app();
        assert_eq!(app.model.notice, CONNECTING_NOTICE);
        assert!(app.model.sessions.is_empty());
        app.backend_event(backend::Event::Snapshot(backend::Snapshot::default()));
        assert_eq!(app.model.notice, CONTROL_READY_NOTICE);
        assert!(
            app.model.item_notice.is_none(),
            "healthy control status has no row notification"
        );
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
        app.backend_event(backend::Event::Failed {
            target: Target::Local,
            error: backend::Error::Busy,
        });
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
            target: Target::Local,
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
            target: Target::Local,
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
            target: Target::Local,
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
        let (clipboard, _recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "出口已启动".into(),
            clipboard: Some("token".into()),
            confirmation: backend::Confirmation::Snapshot,
        });
        let previous = app.operation_id;
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "会话已关闭".into(),
            clipboard: None,
            confirmation: backend::Confirmation::Sessions(Target::Local),
        });
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
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
        let (clipboard, _recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "出口已启动".into(),
            clipboard: Some("token".into()),
            confirmation: backend::Confirmation::Snapshot,
        });
        app.backend_event(backend::Event::RefreshFailed(backend::Error::Timeout));
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
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
        let (clipboard, _recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "出口已启动".into(),
            clipboard: Some("token".into()),
            confirmation: backend::Confirmation::Snapshot,
        });
        let previous = app.operation_id;
        app.term_error(&frames::enc_error("bad_input", "另一个客户端正在粘贴"));
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
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
            target: Target::Local,
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
            target: Target::Remote("old-host".into()),
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
        let (clipboard, _recorded) = clipboard::Clipboard::recording();
        app.clipboard = clipboard;
        app.backend_event(backend::Event::Notice {
            target: Target::Local,
            message: "出口已启动".into(),
            clipboard: Some("token".into()),
            confirmation: backend::Confirmation::Snapshot,
        });
        let previous = app.operation_id;
        app.backend_event(backend::Event::CreateFailed {
            target: Target::Local,
            epoch: 0,
            error: backend::Error::Timeout,
        });
        app.clipboard_result(clipboard::CopyResult {
            purpose: clipboard::Purpose::Operation,
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
        for (cwd, project) in [("/", "/"), ("/tmp/my project", "my project")] {
            let item = session_item(backend::Session {
                cwd: Some(cwd.into()),
                ..Default::default()
            });
            assert_eq!(item.project, project);
        }
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
