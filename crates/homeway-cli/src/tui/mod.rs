//! `homeway tui`：既有 daemon/term 的交互客户端。
mod backend;
mod clipboard;
mod input;
mod terminal;
mod transport;
mod view;

use std::collections::VecDeque;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event};
use homeway_core::term::{
    frames::{self, Op},
    keyenc,
    vt::SessionVt,
};
use ratatui::{backend::CrosstermBackend, Terminal};

use backend::{Backend, Command, Target};
use view::{Action, Model, Theme};

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Vt(#[from] homeway_core::term::vt::VtError),
    #[error("{0}")]
    Backend(#[from] backend::Error),
    #[error("tui 需要交互式终端（stdin/stdout 均须为 TTY）")]
    NotTerminal,
    #[error("未知参数 {0}；用 homeway tui --help 查看用法")]
    Argument(String),
    #[error("{0} 后缺少参数")]
    MissingArgument(String),
    #[error("主题应为 dark 或 light")]
    Theme,
}

struct Options {
    state: PathBuf,
    theme: Option<Theme>,
}

fn options(args: &[String]) -> Result<Option<Options>, Error> {
    if args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--help" | "-h"))
    {
        println!("用法：homeway tui [--theme dark|light] [--state DIR]\n\n鼠标：左键选择；右键管理主机/会话；标题栏 + 添加。\n键盘：Ctrl+B 后 h/s/Enter 选主机/会话/终端；\n      Ctrl+B 后 a/n 添加主机/创建会话；m 操作菜单；t 切主题；q 分离退出。\n      侧栏中 F6 切焦点、F10 退出；Ctrl+B Ctrl+B 发送 Ctrl+B。\n会话中的普通按键（包括 Ctrl+C、Tab、q、功能键）交给终端。退出后会话继续运行。");
        return Ok(None);
    }
    let mut parsed = Options {
        state: crate::unified_cli::default_state_dir(),
        theme: None,
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
            "--theme" => {
                parsed.theme = Some(
                    match iter
                        .next()
                        .ok_or_else(|| Error::MissingArgument(arg.clone()))?
                        .as_str()
                    {
                        "dark" => Theme::Dark,
                        "light" => Theme::Light,
                        _ => return Err(Error::Theme),
                    },
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

fn load_theme(state: &Path) -> Theme {
    match std::fs::read_to_string(state.join("tui-theme")) {
        Ok(value) if value.trim() == "light" => Theme::Light,
        _ => Theme::Dark,
    }
}

fn save_theme(state: &Path, theme: Theme) -> io::Result<()> {
    std::fs::create_dir_all(state)?;
    let temporary = state.join(format!(".tui-theme-{}", std::process::id()));
    std::fs::write(
        &temporary,
        match theme {
            Theme::Dark => "dark\n",
            Theme::Light => "light\n",
        },
    )?;
    std::fs::rename(temporary, state.join("tui-theme"))
}

fn run(options: Options) -> Result<(), Error> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(Error::NotTerminal);
    }
    terminal::install_panic_hook();
    let _guard = terminal::TerminalGuard::enter()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App::new(options);
    let mut geometry = view::Geometry::default();
    while !terminal::stopping() {
        app.process_backend();
        app.process_attach();
        app.flush_input();
        app.process_clipboard()?;
        app.poll_refresh();
        if app.render_dirty {
            terminal.draw(|frame| {
                geometry = view::draw(frame, &mut app.model, app.vt.as_mut());
            })?;
            app.render_dirty = false;
        }
        app.resize(geometry.terminal.width, geometry.terminal.height);
        // 输出每拍有预算；无论远端持续输出还是失联，输入/退出每 25ms 都有机会处理。
        if event::poll(Duration::from_millis(25))? {
            let event = event::read()?;
            app.render_dirty = true;
            let focused = app.model.focus == view::Focus::Terminal && !app.model.modal_open();
            if let Event::FocusGained | Event::FocusLost = event {
                if app.vt.as_ref().is_some_and(|vt| vt.modes().focus_events) {
                    app.send_input(keyenc::encode_focus(matches!(event, Event::FocusGained)));
                }
            }
            let action = view::handle_event(&mut app.model, event, &geometry);
            if app.action(action)? {
                break;
            }
            let focus_now = app.model.focus == view::Focus::Terminal && !app.model.modal_open();
            if focused != focus_now && app.vt.as_ref().is_some_and(|vt| vt.modes().focus_events) {
                app.send_input(keyenc::encode_focus(focus_now));
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
    vt: Option<SessionVt>,
    generation: u64,
    connected: bool,
    replaying: bool,
    input_queue: VecDeque<Vec<u8>>,
    input_bytes: usize,
    size: (u16, u16),
    sent_size: Option<(u16, u16)>,
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

impl App {
    fn new(options: Options) -> Self {
        let mut model = Model::default();
        model.theme = options.theme.unwrap_or_else(|| load_theme(&options.state));
        model.hosts.push(local_host(false, false));
        model.notice = "正在连接本机服务…".into();
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
            vt: None,
            generation: 0,
            connected: false,
            size: (80, 24),
            replaying: true,
            input_queue: VecDeque::new(),
            input_bytes: 0,
            sent_size: None,
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
        self.operation_id = self.operation_id.wrapping_add(1);
        self.model.reset_terminal_input();
        self.generation = self.generation.wrapping_add(1);
        self.attach = None;
        self.vt = None;
        self.connected = false;
        self.replaying = true;
        self.input_queue.clear();
        self.input_bytes = 0;
        self.sent_size = None;
        self.model.terminal_title.clear();
        self.active_session = None;
    }

    fn disconnect(&mut self) {
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

    fn select_session(&mut self, index: usize) -> Result<(), Error> {
        let Some(session) = self.model.sessions.get(index) else {
            return Ok(());
        };
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
        self.vt = Some(SessionVt::new(
            self.size.0.max(1),
            self.size.1.max(1),
            10_000,
        )?);
        self.apply_theme();
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
                    Ok((_, _, modes, _, _, _)) => {
                        if let Some(vt) = &mut self.vt {
                            input::attached_modes(vt, modes);
                        }
                        self.model.notice = "已连接，正在回放会话…".into();
                    }
                    Err(error) => {
                        self.model.notice = error.to_string();
                        self.disconnect();
                        break;
                    }
                },
                Op::REPLAY_DONE => {
                    self.replaying = false;
                    self.connected = true;
                    self.model.notice = match frames::dec_replay_done(&frame.payload) {
                        Ok((_, flags)) if flags & frames::replay_flags::TRUNCATED != 0 => {
                            "已接入（早期回放已裁剪） · Ctrl+B q 分离".into()
                        }
                        Ok((_, flags)) if flags & frames::replay_flags::SIZE_CHANGE != 0 => {
                            "已接入（历史包含尺寸变化） · Ctrl+B q 分离".into()
                        }
                        _ => "已接入 · Ctrl+B q 分离退出；右键管理会话".into(),
                    };
                    if self.model.focus == view::Focus::Terminal
                        && self.vt.as_ref().is_some_and(|vt| vt.modes().focus_events)
                    {
                        self.send_input(keyenc::encode_focus(true));
                    }
                }
                Op::DATA => {
                    let mut replies = Vec::new();
                    if let Some(vt) = &mut self.vt {
                        vt.write_collecting(&frame.payload, &mut |reply| {
                            replies.push(reply.to_vec())
                        });
                    }
                    // 历史查询只恢复屏态，不把过期应答重新注入正在运行的程序。
                    if !self.replaying {
                        for reply in replies {
                            self.send_input(reply);
                        }
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
        let mut replies = Vec::new();
        if let Some(vt) = &mut self.vt {
            self.render_dirty |= vt.flush_sync_timeout(&mut |reply| replies.push(reply.to_vec()));
        }
        if !self.replaying {
            for reply in replies {
                self.send_input(reply);
            }
        }
    }

    fn term_error(&mut self, payload: &[u8]) {
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

    fn send_input(&mut self, bytes: Vec<u8>) {
        if bytes.is_empty() || !self.connected || !self.online() {
            return;
        }
        if self.input_bytes + bytes.len() > 2 << 20 {
            self.model.notice = "输入队列已满，请等待远端接收后重试".into();
            return;
        }
        for chunk in bytes.chunks(frames::DATA_CHUNK) {
            self.input_bytes += chunk.len();
            self.input_queue.push_back(chunk.to_vec());
        }
    }

    fn flush_input(&mut self) {
        if self.sent_size != Some(self.size) || !self.online() {
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
            match attach.send_data(bytes.clone()) {
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
            return;
        }
        let size = homeway_core::term::size::Size::normalized(cols, rows);
        let (cols, rows) = (size.cols(), size.rows());
        if self.size != (cols, rows) {
            self.size = (cols, rows);
            self.render_dirty = true;
            if let Some(vt) = &mut self.vt {
                if let Err(error) = vt.resize(cols, rows) {
                    self.model.notice = error.to_string();
                    self.connected = false;
                    return;
                }
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

    fn apply_theme(&mut self) {
        if let Some(vt) = &mut self.vt {
            let (fg, bg) = match self.model.theme {
                Theme::Dark => ([221, 226, 234], [18, 22, 29]),
                Theme::Light => ([32, 43, 59], [248, 250, 253]),
            };
            vt.set_default_colors(fg, bg);
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
                self.model.notice = "正在读取主机会话…".into();
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
                if let Some(vt) = &self.vt {
                    self.send_input(input::key(vt, event));
                }
            }
            Action::Paste(text) => {
                // 单个事务≤1MiB，与既有服务端粘贴事务上界一致。
                if text.len() > 1 << 20 {
                    self.model.notice = "粘贴超过 1 MiB，请分批粘贴".into();
                } else if self.input_bytes + text.len() + 12 > 2 << 20 {
                    self.model.notice = "输入队列忙，请稍后粘贴".into();
                } else if let Some(vt) = &self.vt {
                    let bracketed = vt.modes().bracketed_paste;
                    let chunks = text.as_bytes().chunks(8 << 10);
                    let count = chunks.len();
                    for (i, bytes) in chunks.enumerate() {
                        if let Some(bytes) =
                            keyenc::encode_paste_part(bytes, bracketed, i == 0, i + 1 == count)
                        {
                            self.send_input(bytes);
                        }
                    }
                }
            }
            Action::TerminalMouse(event) => {
                if let Some(vt) = &self.vt {
                    self.send_input(input::mouse(vt, event));
                }
            }
            Action::ThemeChanged => {
                self.clear_operation();
                self.apply_theme();
                self.model.notice = match save_theme(&self.state_dir, self.model.theme) {
                    Ok(()) => "主题已保存".into(),
                    Err(error) => format!("主题已切换，保存失败：{error}"),
                };
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

    fn app() -> App {
        App::new(Options {
            state: std::env::temp_dir().join(format!("homeway-tui-events-{}", std::process::id())),
            theme: None,
        })
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
        app.vt = Some(SessionVt::new(80, 24, 0).unwrap());
        app.vt.as_mut().unwrap().write(b"KEEP");
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
        assert!(app.vt.as_mut().unwrap().screen_text().contains("KEEP"));
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
}
