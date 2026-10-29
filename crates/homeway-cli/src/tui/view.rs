//! TUI 的纯显示与交互状态；网络与 PTY 生命周期由主循环管理。

use std::borrow::Cow;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use homeway_core::term::vt;
#[cfg(test)]
use homeway_core::term::vt::SessionVt;
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph, Wrap},
    Frame,
};

use super::{
    backend::Target,
    selection::{self, projected_symbol as terminal_symbol},
};

pub(super) const COMMAND_SHORTCUT: &str = "Ctrl+B";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    fn palette(self) -> Palette {
        match self {
            Self::Dark => Palette {
                bg: Color::Rgb(18, 22, 29),
                fg: Color::Rgb(221, 226, 234),
                muted: Color::Rgb(139, 151, 169),
                border: Color::Rgb(53, 64, 80),
                accent: Color::Rgb(109, 185, 255),
                selected: Color::Rgb(35, 58, 83),
                green: Color::Rgb(112, 205, 162),
                amber: Color::Rgb(242, 194, 107),
                red: Color::Rgb(234, 130, 135),
            },
            Self::Light => Palette {
                bg: Color::Rgb(248, 250, 253),
                fg: Color::Rgb(32, 43, 59),
                muted: Color::Rgb(98, 112, 131),
                border: Color::Rgb(192, 203, 217),
                accent: Color::Rgb(24, 102, 173),
                selected: Color::Rgb(220, 235, 252),
                green: Color::Rgb(25, 127, 86),
                amber: Color::Rgb(155, 102, 5),
                red: Color::Rgb(182, 48, 64),
            },
        }
    }
}

#[derive(Clone, Copy)]
struct Palette {
    bg: Color,
    fg: Color,
    muted: Color,
    border: Color,
    accent: Color,
    selected: Color,
    green: Color,
    amber: Color,
    red: Color,
}

const ITEM_HEIGHT: u16 = 2;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Focus {
    #[default]
    Hosts,
    Sessions,
    Terminal,
}

impl Focus {
    fn next(self) -> Self {
        match self {
            Self::Hosts => Self::Sessions,
            Self::Sessions => Self::Terminal,
            Self::Terminal => Self::Hosts,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HostState {
    Local,
    Direct,
    Relay,
    Offline,
}

#[derive(Debug, Clone)]
pub(super) struct HostItem {
    pub target: Target,
    pub name: String,
    pub rtt_ms: Option<i64>,
    pub state: HostState,
    pub exit_on: bool,
    pub relay_on: bool,
    pub proxy_on: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SessionState {
    Working,
    Idle,
    Blocked,
    Unknown,
}

#[derive(Debug, Clone)]
pub(super) struct SessionItem {
    pub name: String,
    pub title: String,
    pub agent: String,
    pub project: String,
    pub state: SessionState,
    pub self_session: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NoticeTarget {
    Host(Target),
    Session { target: Target, name: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ItemNotice {
    pub target: NoticeTarget,
    pub summary: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct TerminalClose {
    pub name: String,
    pub navigation_epoch: u64,
}

/// The caller supplies an authoritative, ordered window of physical terminal rows.
/// FETCH row.y is local to the payload, so display positions use slice order.
#[derive(Clone, Copy)]
pub(super) struct TerminalView<'a> {
    pub rows: &'a [vt::Row],
    pub cursor: Option<vt::Cursor>,
    pub mouse_reporting: bool,
    pub scrollbar: vt::Scrollbar,
    pub top: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HistoryAction {
    Lines(i32),
    PageUp,
    PageDown,
    Start,
    End,
}

#[derive(Default)]
pub(super) struct Model {
    pub hosts: Vec<HostItem>,
    pub sessions: Vec<SessionItem>,
    pub selected_host: usize,
    pub selected_session: Option<usize>,
    pub theme: Theme,
    pub notice: String,
    pub item_notice: Option<ItemNotice>,
    pub focus: Focus,
    pub terminal_title: String,
    pub terminal_present: bool,
    pub terminal_close: Option<TerminalClose>,
    pub selection_spans: Vec<selection::Span>,
    pub selecting: bool,
    prefix: bool,
    command_detail_offset: u16,
    hosts_offset: usize,
    sessions_offset: usize,
    suppressed_releases: Vec<KeyCode>,
    forwarded_keys: Vec<KeyCode>,
    mouse_reporting: bool,
    history_behind: u64,
    menu: Option<Menu>,
    add_host: Option<AddHost>,
}

impl Model {
    pub(super) fn reset_sessions(&mut self) {
        self.reset_terminal_input();
        self.sessions.clear();
        self.selected_session = None;
        self.sessions_offset = 0;
    }

    pub(super) fn modal_open(&self) -> bool {
        self.prefix || self.menu.is_some() || self.add_host.is_some()
    }

    pub(super) fn commands_open(&self) -> bool {
        self.prefix
    }

    pub(super) fn clear_prefix(&mut self) {
        self.prefix = false;
        self.command_detail_offset = 0;
    }

    pub(super) fn reset_terminal_input(&mut self) {
        self.clear_prefix();
        self.forwarded_keys.clear();
        self.suppressed_releases.clear();
        self.mouse_reporting = false;
        self.history_behind = 0;
        self.terminal_close = None;
        self.terminal_present = false;
        self.selection_spans.clear();
        self.selecting = false;
    }

    fn close_target(&self) -> Option<&TerminalClose> {
        if !self.terminal_present {
            return None;
        }
        let session = self
            .selected_session
            .and_then(|index| self.sessions.get(index))?;
        self.terminal_close
            .as_ref()
            .filter(|target| !session.self_session && target.name == session.name)
    }

    fn next_focus(&mut self) {
        self.focus = self.focus.next();
        if self.focus == Focus::Terminal && self.selected_session.is_none() {
            self.focus = Focus::Hosts;
        }
    }
}

#[derive(Debug)]
pub(super) enum Action {
    None,
    Quit,
    SelectHost(usize),
    SelectSession(usize),
    AddHost { name: String, token: String },
    CreateSession,
    ToggleExit,
    ToggleRelay,
    ToggleProxy,
    DeleteHost,
    CloseSession,
    CloseTerminalSession(TerminalClose),
    SendKey(KeyEvent),
    Paste(String),
    TerminalMouse(MouseEvent),
    Scroll(HistoryAction),
}

#[derive(Default, Debug, Clone, Copy)]
pub(super) struct Geometry {
    pub terminal: Rect,
    footer: Rect,
    hosts: Rect,
    sessions: Rect,
    host_add: Rect,
    session_add: Rect,
    terminal_close: Rect,
    create: Rect,
    menu: Rect,
    dialog: Rect,
    name_input: Rect,
    token_input: Rect,
    submit: Rect,
}

#[cfg(test)]
impl Geometry {
    pub(super) fn footer_area(&self) -> Rect {
        self.footer
    }
}

#[derive(Debug, Clone, Copy)]
enum MenuKind {
    Local,
    Host,
    Session,
    SelfInfo,
}

struct Menu {
    kind: MenuKind,
    x: u16,
    y: u16,
    selected: usize,
}

#[derive(Default)]
struct AddHost {
    name: String,
    token: String,
    field: usize,
    error: String,
}

fn inner(rect: Rect) -> Rect {
    Rect::new(
        rect.x.saturating_add(1),
        rect.y.saturating_add(1),
        rect.width.saturating_sub(2),
        rect.height.saturating_sub(2),
    )
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && x >= rect.x
        && x < rect.right()
        && y >= rect.y
        && y < rect.bottom()
}

fn text(frame: &mut Frame<'_>, area: Rect, value: impl Into<Line<'static>>, style: Style) {
    if area.width > 0 && area.height > 0 {
        frame.render_widget(Paragraph::new(value.into()).style(style), area);
    }
}

fn clean(value: &str) -> String {
    value.chars().filter(|c| !c.is_control()).collect()
}

fn ensure_visible(selected: usize, len: usize, offset: &mut usize, slots: usize) {
    if slots == 0 || len == 0 {
        *offset = 0;
        return;
    }
    *offset = (*offset).min(len.saturating_sub(slots));
    if selected < *offset {
        *offset = selected;
    }
    if selected >= offset.saturating_add(slots) {
        *offset = selected + 1 - slots;
    }
}

fn item_index(area: Rect, y: u16, offset: usize) -> Option<usize> {
    let slot = y.checked_sub(area.y)? / ITEM_HEIGHT;
    (slot < area.height / ITEM_HEIGHT).then_some(offset + usize::from(slot))
}

fn panel(frame: &mut Frame<'_>, rect: Rect, title: String, active: bool, p: Palette) {
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if active { p.accent } else { p.border }))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(if active { p.accent } else { p.fg })
                    .add_modifier(Modifier::BOLD),
            )),
        rect,
    );
}

/// 几何与鼠标命中使用同一份结果，滚动时通过实际 offset 映射列表行。
pub(super) fn draw(
    frame: &mut Frame<'_>,
    model: &mut Model,
    terminal: Option<TerminalView<'_>>,
) -> Geometry {
    let area = frame.area();
    let p = model.theme.palette();
    frame.render_widget(
        Block::default().style(Style::default().fg(p.fg).bg(p.bg)),
        area,
    );
    let mut geometry = Geometry::default();
    model.mouse_reporting = terminal.is_some_and(|view| view.mouse_reporting);
    model.history_behind =
        terminal.map_or(0, |view| view.scrollbar.offset.saturating_sub(view.top));
    if area.width < 40 || area.height < 10 {
        frame.render_widget(
            Paragraph::new(format!("窗口太小，至少 40×10 · {COMMAND_SHORTCUT} q 退出"))
                .alignment(Alignment::Center)
                .style(Style::default().fg(p.muted)),
            area,
        );
        if model.prefix {
            draw_commands(frame, model, p);
        }
        return geometry;
    }
    let sidebar = ((u32::from(area.width) * 3 / 10) as u16).clamp(18, 34) * 4 / 5;
    let sidebar = sidebar.min(area.width.saturating_sub(18));
    let body = area;
    let host_height = model
        .hosts
        .len()
        .saturating_add(1)
        .saturating_mul(usize::from(ITEM_HEIGHT))
        // One additional two-line item is blank content; the frame is separate.
        .saturating_add(2)
        .min(usize::from(body.height / 2)) as u16;
    let split = host_height.saturating_sub(1);
    let host_panel = Rect::new(body.x, body.y, sidebar, host_height);
    let session_panel = Rect::new(body.x, body.y + split, sidebar, body.height - split);
    let terminal_panel = Rect::new(
        body.x + sidebar - 1,
        body.y,
        body.width - sidebar + 1,
        body.height,
    );
    geometry.hosts = inner(host_panel);
    geometry.sessions = inner(session_panel);
    let footer_area = Rect::new(
        geometry.sessions.x + 1,
        geometry.sessions.bottom().saturating_sub(1),
        geometry.sessions.width.saturating_sub(1),
        1,
    );
    geometry.footer = footer_area;
    geometry.sessions.height = geometry.sessions.height.saturating_sub(1);
    geometry.terminal = inner(terminal_panel);
    geometry.host_add = Rect::new(host_panel.right() - 6, host_panel.y, 5, 1);
    geometry.session_add = Rect::new(session_panel.right() - 6, session_panel.y, 5, 1);
    if model.terminal_present {
        geometry.terminal_close = Rect::new(terminal_panel.right() - 6, terminal_panel.y, 5, 1);
    }
    panel(
        frame,
        host_panel,
        " 主机 ".into(),
        model.focus == Focus::Hosts,
        p,
    );
    panel(
        frame,
        session_panel,
        " 会话 ".into(),
        model.focus == Focus::Sessions,
        p,
    );
    panel(
        frame,
        terminal_panel,
        String::new(),
        model.focus == Focus::Terminal,
        p,
    );
    text(
        frame,
        Rect::new(
            terminal_panel.x + 1,
            terminal_panel.y,
            terminal_panel
                .width
                .saturating_sub(if model.terminal_present { 8 } else { 2 }),
            1,
        ),
        Line::from(Span::styled(
            format!(
                " {} ",
                if model.terminal_title.is_empty() {
                    "终端".to_owned()
                } else {
                    clean(&model.terminal_title)
                }
            ),
            Style::default()
                .fg(if model.focus == Focus::Terminal {
                    p.accent
                } else {
                    p.fg
                })
                .add_modifier(Modifier::BOLD),
        )),
        Style::default(),
    );
    text(
        frame,
        geometry.host_add,
        "[ + ]",
        Style::default().fg(p.accent),
    );
    text(
        frame,
        geometry.session_add,
        "[ + ]",
        Style::default().fg(p.accent),
    );
    text(
        frame,
        geometry.terminal_close,
        "[ X ]",
        Style::default().fg(if model.close_target().is_some() {
            p.accent
        } else {
            p.muted
        }),
    );
    // 三个面板共用一条竖线和一条左侧横线。
    text(
        frame,
        Rect::new(body.x + sidebar - 1, body.y + split, 1, 1),
        "┤",
        Style::default().fg(p.border),
    );
    let host_slots = usize::from(geometry.hosts.height / ITEM_HEIGHT);
    ensure_visible(
        model.selected_host,
        model.hosts.len(),
        &mut model.hosts_offset,
        host_slots,
    );
    for (slot, (index, host)) in model
        .hosts
        .iter()
        .enumerate()
        .skip(model.hosts_offset)
        .take(host_slots)
        .enumerate()
    {
        let y = geometry.hosts.y + slot as u16 * ITEM_HEIGHT;
        if y >= geometry.hosts.bottom() {
            break;
        }
        let selected = index == model.selected_host;
        let bg = if selected { p.selected } else { p.bg };
        let row = Rect::new(
            geometry.hosts.x,
            y,
            geometry.hosts.width,
            (geometry.hosts.bottom() - y).min(2),
        );
        frame.render_widget(Block::default().style(Style::default().bg(bg)), row);
        let (label, color) = match host.state {
            HostState::Local => ("local", p.accent),
            HostState::Direct => ("direct", p.green),
            HostState::Relay => ("relay", p.amber),
            HostState::Offline => ("offline", p.muted),
        };
        text(
            frame,
            Rect::new(row.x + 1, y, row.width.saturating_sub(1), 1),
            Line::from(vec![
                Span::styled("● ", Style::default().fg(color)),
                Span::raw(clean(&host.name)),
            ]),
            Style::default().fg(p.fg).bg(bg).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        );
        if row.height > 1 {
            let notice = host_notice(model, host);
            let detail = if let Some(notice) = notice {
                format!("  {}", clean(&notice.summary))
            } else if host.state == HostState::Local {
                format!(
                    "  exit:{} relay:{}",
                    if host.exit_on { "on" } else { "off" },
                    if host.relay_on { "on" } else { "off" }
                )
            } else {
                format!(
                    "  {label} {} proxy:{}",
                    host.rtt_ms
                        .map(|ms| format!("{ms}ms"))
                        .unwrap_or_else(|| "--ms".into()),
                    if host.proxy_on { "on" } else { "off" }
                )
            };
            let detail_area = Rect::new(row.x + 1, y + 1, row.width.saturating_sub(1), 1);
            text(
                frame,
                detail_area,
                detail,
                Style::default()
                    .fg(if notice.is_some() { p.amber } else { p.muted })
                    .bg(bg),
            );
        }
    }
    let session_slots = usize::from(geometry.sessions.height / ITEM_HEIGHT);
    ensure_visible(
        model.selected_session.unwrap_or(0),
        model.sessions.len(),
        &mut model.sessions_offset,
        session_slots,
    );
    for (slot, (index, session)) in model
        .sessions
        .iter()
        .enumerate()
        .skip(model.sessions_offset)
        .take(session_slots)
        .enumerate()
    {
        let y = geometry.sessions.y + slot as u16 * ITEM_HEIGHT;
        if y >= geometry.sessions.bottom() {
            break;
        }
        let selected = model.selected_session == Some(index);
        let bg = if selected { p.selected } else { p.bg };
        let row = Rect::new(
            geometry.sessions.x,
            y,
            geometry.sessions.width,
            (geometry.sessions.bottom() - y).min(2),
        );
        let (label, color) = match session.state {
            SessionState::Working => ("working", p.green),
            SessionState::Idle => ("idle", p.muted),
            SessionState::Blocked => ("blocking", p.amber),
            SessionState::Unknown => ("unknown", p.muted),
        };
        frame.render_widget(
            Block::default().style(Style::default().fg(color).bg(bg)),
            row,
        );
        let title = if session.title.is_empty() {
            &session.name
        } else {
            &session.title
        };
        text(
            frame,
            Rect::new(row.x + 1, y, row.width.saturating_sub(1), 1),
            Line::from(vec![
                Span::styled("● ", Style::default().fg(color)),
                Span::raw(format!(
                    "{}{}",
                    if session.self_session {
                        "·自身 "
                    } else {
                        ""
                    },
                    clean(title),
                )),
            ]),
            Style::default().fg(p.fg).bg(bg).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        );
        if row.height > 1 {
            let agent = clean(&session.agent);
            let project = clean(&session.project);
            let normal = format!(
                "{label} {} {}",
                if agent.is_empty() { "unknown" } else { &agent },
                if project.is_empty() { "—" } else { &project }
            );
            let notice = session_notice(model, session);
            let detail = if model.selecting && selected {
                "拖选中，松开复制".to_owned()
            } else {
                notice.map_or(normal, |notice| clean(&notice.summary))
            };
            text(
                frame,
                Rect::new(row.x + 1, y + 1, row.width.saturating_sub(1), 1),
                format!("  {detail}"),
                Style::default()
                    .fg(if notice.is_some() || (model.selecting && selected) {
                        p.amber
                    } else {
                        color
                    })
                    .bg(bg),
            );
        }
    }
    if model.sessions.is_empty() {
        text(
            frame,
            Rect::new(
                geometry.sessions.x + 1,
                geometry.sessions.y,
                geometry.sessions.width.saturating_sub(1),
                geometry.sessions.height,
            ),
            "暂无会话 · 点击 + 创建",
            Style::default().fg(p.muted),
        );
    }
    if let Some(terminal) = terminal {
        draw_terminal(
            frame,
            geometry.terminal,
            terminal,
            p,
            model.focus == Focus::Terminal && !model.modal_open(),
        );
        highlight_selection(frame, geometry.terminal, &model.selection_spans, p);
    } else if model.selected_session.is_none()
        || model
            .selected_session
            .and_then(|index| model.sessions.get(index))
            .is_some_and(|session| session.self_session)
    {
        let selected_self = (model.selected_session.is_none()
            && model.sessions.len() == 1
            && model.sessions[0].self_session)
            || model
                .selected_session
                .and_then(|index| model.sessions.get(index))
                .is_some_and(|session| session.self_session);
        let terminal = geometry.terminal;
        let center_y = terminal.y + terminal.height.saturating_sub(5) / 2;
        text(
            frame,
            Rect::new(terminal.x, center_y, terminal.width, 1),
            Line::from(if selected_self {
                "当前界面运行在该会话中"
            } else {
                "想在这台主机上做些什么？"
            })
            .alignment(Alignment::Center),
            Style::default().fg(p.fg),
        );
        text(
            frame,
            Rect::new(terminal.x, center_y + 2, terminal.width, 1),
            Line::from(if selected_self {
                "请选择其它会话，或创建 shell"
            } else {
                "创建 shell 会话，开始工作"
            })
            .alignment(Alignment::Center),
            Style::default().fg(p.muted),
        );
        geometry.create = Rect::new(
            terminal.x + terminal.width.saturating_sub(18) / 2,
            center_y + 4,
            18.min(terminal.width),
            1,
        );
        text(
            frame,
            geometry.create,
            "[ + 创建 shell ]",
            Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
        );
    } else {
        let terminal = geometry.terminal;
        text(
            frame,
            Rect::new(
                terminal.x,
                terminal.y + terminal.height / 2,
                terminal.width,
                1,
            ),
            Line::from("正在接入会话…").alignment(Alignment::Center),
            Style::default().fg(p.muted),
        );
    }
    text(
        frame,
        footer_area,
        footer(model, footer_area.width),
        Style::default().fg(p.muted),
    );
    if model.menu.is_some() {
        draw_menu(frame, model, &mut geometry, p);
    }
    if model.add_host.is_some() {
        draw_add_host(frame, model, &mut geometry, p);
    }
    if model.prefix {
        draw_commands(frame, model, p);
    }
    geometry
}

fn terminal_color(color: vt::Color, default: Color) -> Color {
    match color {
        vt::Color::None => default,
        vt::Color::Palette(index) => Color::Indexed(index),
        vt::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn footer(model: &Model, width: u16) -> String {
    if model.history_behind > 0 {
        let count = model.history_behind.to_string();
        if count.len() + 11 <= usize::from(width) {
            format!("{count}行 ⇧End回底")
        } else if count.len() + 10 <= usize::from(width) {
            format!("{count}行⇧End回底")
        } else if count.len() + 7 <= usize::from(width) {
            format!("{count}行 ⇧End")
        } else {
            let (scale, unit) = [
                (1_000_000_000_000_000_000u64, "E"),
                (1_000_000_000_000_000, "P"),
                (1_000_000_000_000, "T"),
                (1_000_000_000, "G"),
                (1_000_000, "M"),
                (1_000, "k"),
            ]
            .into_iter()
            .find(|(scale, _)| model.history_behind >= *scale)
            .unwrap_or((1, ""));
            format!("~{}{unit}行 ⇧End", model.history_behind / scale)
        }
    } else {
        let hint = format!("{COMMAND_SHORTCUT} 快捷指令");
        if Line::from(hint.as_str()).width() <= usize::from(width) {
            hint
        } else {
            format!("{COMMAND_SHORTCUT} 指令")
        }
    }
}

fn session_notice<'a>(model: &'a Model, session: &SessionItem) -> Option<&'a ItemNotice> {
    let notice = model.item_notice.as_ref()?;
    match &notice.target {
        NoticeTarget::Session { target, name }
            if name == &session.name
                && model
                    .hosts
                    .get(model.selected_host)
                    .is_some_and(|host| &host.target == target) =>
        {
            Some(notice)
        }
        _ => None,
    }
}

fn host_notice<'a>(model: &'a Model, host: &HostItem) -> Option<&'a ItemNotice> {
    let notice = model.item_notice.as_ref()?;
    match &notice.target {
        NoticeTarget::Host(target) if target == &host.target => Some(notice),
        NoticeTarget::Session { target, name } if target == &host.target => {
            let current = model
                .hosts
                .get(model.selected_host)
                .is_some_and(|selected| &selected.target == target);
            (!current || !model.sessions.iter().any(|session| &session.name == name))
                .then_some(notice)
        }
        _ => None,
    }
}

fn draw_commands(frame: &mut Frame<'_>, model: &mut Model, p: Palette) {
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(64);
    let height = area.height.saturating_sub(2).min(16);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .style(Style::default().bg(p.bg).fg(p.fg))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(p.accent))
            .title(format!(" 快捷指令 · {COMMAND_SHORTCUT} ")),
        rect,
    );
    let content = inner(rect);
    if content.is_empty() {
        model.command_detail_offset = 0;
        return;
    }
    let commands = [
        "h 主机  s 会话  Enter 终端",
        "n 新建会话  a 添加主机",
        "m 操作菜单  q 分离退出",
        "Ctrl+B 连按发送 Ctrl+B  Esc 取消",
    ];
    let history = if model.history_behind > 0 {
        format!("历史：{}行；Shift+End 回到底部。", model.history_behind)
    } else {
        String::new()
    };
    let notice = model
        .item_notice
        .as_ref()
        .map_or(String::new(), |notice| clean(&notice.detail));
    let detail = match (history.is_empty(), notice.is_empty()) {
        (false, false) => format!("{history}\n{notice}"),
        (false, true) => history,
        (_, _) => notice,
    };
    let show_detail = content.height > 5 && !detail.is_empty();
    let detail_heading = "详情 · ↑↓滚动";
    let width = commands
        .iter()
        .map(|line| Line::from(*line).width())
        .chain(show_detail.then(|| Line::from(detail_heading).width()))
        .chain(
            detail
                .lines()
                .filter(|_| show_detail)
                .map(|line| Line::from(line).width()),
        )
        .max()
        .unwrap_or(0)
        .min(usize::from(content.width)) as u16;
    let paragraph = Paragraph::new(detail).wrap(Wrap { trim: false });
    let detail_lines = if show_detail {
        paragraph.line_count(width)
    } else {
        0
    };
    let detail_height = detail_lines.min(usize::from(content.height.saturating_sub(5))) as u16;
    let height =
        content.height.min(commands.len() as u16) + if show_detail { 1 + detail_height } else { 0 };
    let group = Rect::new(
        content.x + (content.width - width) / 2,
        content.y + (content.height - height) / 2,
        width,
        height,
    );
    for (index, command) in commands
        .into_iter()
        .enumerate()
        .take(usize::from(group.height))
    {
        text(
            frame,
            Rect::new(group.x, group.y + index as u16, group.width, 1),
            command,
            Style::default().fg(p.fg).bg(p.bg),
        );
    }
    model.command_detail_offset = model.command_detail_offset.min(
        u16::try_from(detail_lines.saturating_sub(usize::from(detail_height))).unwrap_or(u16::MAX),
    );
    if show_detail {
        text(
            frame,
            Rect::new(group.x, group.y + 4, group.width, 1),
            detail_heading,
            Style::default().fg(p.muted).bg(p.bg),
        );
        frame.render_widget(
            paragraph
                .style(Style::default().fg(p.amber).bg(p.bg))
                .scroll((model.command_detail_offset, 0)),
            Rect::new(group.x, group.y + 5, group.width, detail_height),
        );
    }
}

fn highlight_selection(frame: &mut Frame<'_>, area: Rect, spans: &[selection::Span], p: Palette) {
    let style = Style::default()
        .fg(p.bg)
        .bg(p.accent)
        .remove_modifier(Modifier::REVERSED);
    for span in spans {
        if span.row >= area.height {
            continue;
        }
        for column in span.start.min(area.width)..span.end.min(area.width) {
            if let Some(cell) = frame
                .buffer_mut()
                .cell_mut((area.x + column, area.y + span.row))
            {
                // Keep HIDDEN and other application attributes intact. Selection
                // changes presentation colors, never the underlying cell text.
                cell.set_style(style);
            }
        }
    }
}

fn draw_terminal(
    frame: &mut Frame<'_>,
    area: Rect,
    terminal: TerminalView<'_>,
    p: Palette,
    show_cursor: bool,
) {
    for (row_index, row) in terminal
        .rows
        .iter()
        .take(usize::from(area.height))
        .enumerate()
    {
        for (x, cell) in row.cells.iter().enumerate().take(usize::from(area.width)) {
            if cell.skip || cell.width == 0 {
                continue;
            }
            let clipped_wide = cell.width == 2 && x + 1 >= usize::from(area.width);
            let mut style = Style::default()
                .fg(terminal_color(cell.fg, p.fg))
                .bg(terminal_color(cell.bg, p.bg));
            for (bit, modifier) in [
                (vt::attr::BOLD, Modifier::BOLD),
                (vt::attr::ITALIC, Modifier::ITALIC),
                (vt::attr::FAINT, Modifier::DIM),
                (vt::attr::INVERSE, Modifier::REVERSED),
                (vt::attr::INVISIBLE, Modifier::HIDDEN),
                (vt::attr::STRIKETHROUGH, Modifier::CROSSED_OUT),
                (vt::attr::BLINK, Modifier::SLOW_BLINK),
            ] {
                if cell.attr & bit != 0 {
                    style = style.add_modifier(modifier);
                }
            }
            if cell.attr & vt::attr::UNDERLINE_MASK != 0 {
                style = style.add_modifier(Modifier::UNDERLINED);
            }
            let x = area.x + x as u16;
            let y = area.y + row_index as u16;
            if let Some(target) = frame.buffer_mut().cell_mut((x, y)) {
                let symbol = if clipped_wide {
                    Cow::Borrowed(" ")
                } else {
                    terminal_symbol(&cell.symbol)
                };
                target.set_symbol(symbol.as_ref()).set_style(style);
            }
            if cell.width == 2 && !clipped_wide {
                if let Some(target) = frame.buffer_mut().cell_mut((x + 1, y)) {
                    target.set_symbol(" ").set_style(style);
                }
            }
        }
    }
    if let Some(cursor) = terminal.cursor {
        if show_cursor && cursor.visible && cursor.x < area.width && cursor.y < area.height {
            frame.set_cursor_position((area.x + cursor.x, area.y + cursor.y));
        }
    }
}

fn menu_labels(model: &Model, kind: MenuKind) -> Vec<&'static str> {
    match kind {
        MenuKind::Local => {
            let host = model.hosts.get(model.selected_host);
            vec![
                if host.is_some_and(|h| h.exit_on) {
                    "关闭出口"
                } else {
                    "启动出口"
                },
                if host.is_some_and(|h| h.relay_on) {
                    "关闭中继"
                } else {
                    "启动中继"
                },
            ]
        }
        MenuKind::Host => vec![
            if model
                .hosts
                .get(model.selected_host)
                .is_some_and(|h| h.proxy_on)
            {
                "关闭代理"
            } else {
                "启动代理"
            },
            "删除主机",
        ],
        MenuKind::Session => vec!["关闭会话"],
        MenuKind::SelfInfo => vec!["承载当前TUI，退出后再关闭"],
    }
}

fn draw_menu(frame: &mut Frame<'_>, model: &Model, geometry: &mut Geometry, p: Palette) {
    let Some(menu) = &model.menu else {
        return;
    };
    let labels = menu_labels(model, menu.kind);
    let area = frame.area();
    let width = 22.min(area.width);
    let height = (labels.len() as u16 + 2).min(area.height);
    let rect = Rect::new(
        menu.x.min(area.right().saturating_sub(width)),
        menu.y.min(area.bottom().saturating_sub(height)),
        width,
        height,
    );
    geometry.menu = rect;
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .style(Style::default().fg(p.fg).bg(p.bg))
            .border_style(Style::default().fg(p.accent)),
        rect,
    );
    for (index, label) in labels.iter().enumerate() {
        text(
            frame,
            Rect::new(rect.x + 1, rect.y + 1 + index as u16, rect.width - 2, 1),
            format!(" {label}"),
            Style::default().fg(p.fg).bg(if index == menu.selected {
                p.selected
            } else {
                p.bg
            }),
        );
    }
}

fn draw_add_host(frame: &mut Frame<'_>, model: &Model, geometry: &mut Geometry, p: Palette) {
    let Some(dialog) = &model.add_host else {
        return;
    };
    let area = frame.area();
    let width = area.width.saturating_sub(4).min(64);
    let height = 11.min(area.height);
    let rect = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    geometry.dialog = rect;
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Block::default()
            .title(" 添加主机 ")
            .borders(Borders::ALL)
            .style(Style::default().fg(p.fg).bg(p.bg))
            .border_style(Style::default().fg(p.accent)),
        rect,
    );
    text(
        frame,
        Rect::new(rect.x + 2, rect.y + 1, width - 4, 1),
        "主机名",
        Style::default().fg(p.muted),
    );
    geometry.name_input = Rect::new(rect.x + 2, rect.y + 2, width - 4, 1);
    text(
        frame,
        geometry.name_input,
        if dialog.name.is_empty() {
            "输入主机名".to_owned()
        } else {
            clean(&dialog.name)
        },
        Style::default()
            .fg(if dialog.name.is_empty() {
                p.muted
            } else {
                p.fg
            })
            .bg(if dialog.field == 0 { p.selected } else { p.bg }),
    );
    text(
        frame,
        Rect::new(rect.x + 2, rect.y + 4, width - 4, 1),
        "Token",
        Style::default().fg(p.muted),
    );
    geometry.token_input = Rect::new(rect.x + 2, rect.y + 5, width - 4, 1);
    let masked = if dialog.token.is_empty() {
        "粘贴 homeway token".to_owned()
    } else {
        format!(
            "{} ({} 字符)",
            "•".repeat(dialog.token.chars().count().min(16)),
            dialog.token.chars().count()
        )
    };
    text(
        frame,
        geometry.token_input,
        masked,
        Style::default()
            .fg(if dialog.token.is_empty() {
                p.muted
            } else {
                p.fg
            })
            .bg(if dialog.field == 1 { p.selected } else { p.bg }),
    );
    geometry.submit = Rect::new(rect.x + 2, rect.y + 7, 12, 1);
    text(
        frame,
        geometry.submit,
        "[ 添加主机 ]",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    );
    text(
        frame,
        Rect::new(rect.x + 16, rect.y + 7, width - 18, 1),
        "Enter 添加 · Esc 取消",
        Style::default().fg(p.muted),
    );
    text(
        frame,
        Rect::new(rect.x + 2, rect.y + 9, width - 4, 1),
        dialog.error.clone(),
        Style::default().fg(p.red),
    );
    let active = if dialog.field == 0 {
        geometry.name_input
    } else {
        geometry.token_input
    };
    let cursor = if dialog.field == 0 {
        Line::from(clean(&dialog.name)).width()
    } else {
        0
    };
    if active.width > 0 {
        frame.set_cursor_position((active.x + (cursor as u16).min(active.width - 1), active.y));
    }
}

fn submit_host(model: &mut Model) -> Action {
    let Some(dialog) = &mut model.add_host else {
        return Action::None;
    };
    if dialog.name.trim().is_empty() {
        dialog.error = "请输入主机名".into();
        dialog.field = 0;
        return Action::None;
    }
    if dialog.token.trim().is_empty() {
        dialog.error = "请输入 token".into();
        dialog.field = 1;
        return Action::None;
    }
    let Some(dialog) = model.add_host.take() else {
        return Action::None;
    };
    Action::AddHost {
        name: dialog.name.trim().to_owned(),
        token: dialog.token.trim().to_owned(),
    }
}

fn activate_menu(model: &mut Model) -> Action {
    let Some(menu) = model.menu.take() else {
        return Action::None;
    };
    match (menu.kind, menu.selected) {
        (MenuKind::Local, 0) => Action::ToggleExit,
        (MenuKind::Local, _) => Action::ToggleRelay,
        (MenuKind::Host, 0) => Action::ToggleProxy,
        (MenuKind::Host, _) => Action::DeleteHost,
        (MenuKind::Session, _) => Action::CloseSession,
        (MenuKind::SelfInfo, _) => Action::None,
    }
}

fn menu_for_focus(model: &mut Model, geometry: &Geometry) {
    let (kind, area) = match model.focus {
        Focus::Hosts
            if model
                .hosts
                .get(model.selected_host)
                .is_some_and(|h| h.state == HostState::Local) =>
        {
            (MenuKind::Local, geometry.hosts)
        }
        Focus::Hosts if !model.hosts.is_empty() => (MenuKind::Host, geometry.hosts),
        Focus::Sessions if model.selected_session.is_some() => (
            if model
                .selected_session
                .and_then(|index| model.sessions.get(index))
                .is_some_and(|session| session.self_session)
            {
                MenuKind::SelfInfo
            } else {
                MenuKind::Session
            },
            geometry.sessions,
        ),
        _ => return,
    };
    model.menu = Some(Menu {
        kind,
        x: area.x + 2,
        y: area.y + 1,
        selected: 0,
    });
}

fn is_command_shortcut(key: KeyEvent) -> bool {
    key.code == KeyCode::Char('b')
        && (key.modifiers.contains(KeyModifiers::CONTROL) || key.modifiers == KeyModifiers::SUPER)
}

/// 终端内只保留快捷指令前缀；功能键仍交给被管理的程序。
pub(super) fn handle_event(model: &mut Model, event: Event, geometry: &Geometry) -> Action {
    let key = match &event {
        Event::Key(key) => Some(*key),
        _ => None,
    };
    if let Some(key) = key {
        if key.kind == KeyEventKind::Release {
            let suppressed = model.suppressed_releases.contains(&key.code);
            model.suppressed_releases.retain(|code| *code != key.code);
            let forwarded = model.forwarded_keys.contains(&key.code);
            model.forwarded_keys.retain(|code| *code != key.code);
            // A release belongs to the terminal that received its press, even
            // when navigation temporarily owns focus or the viewport is hidden.
            // detach resets this ledger before a different attach can receive it.
            return if suppressed || !forwarded {
                Action::None
            } else {
                Action::SendKey(key)
            };
        }
    }
    if model.menu.is_some() || model.add_host.is_some() {
        model.clear_prefix();
    }
    let action = handle_event_inner(model, event, geometry);
    if matches!(action, Action::SelectHost(_) | Action::SelectSession(_)) {
        model.clear_prefix();
    }
    if let Some(key) = key {
        if key.kind != KeyEventKind::Release {
            if matches!(action, Action::SendKey(_)) {
                model.suppressed_releases.retain(|code| *code != key.code);
                remember_key(&mut model.forwarded_keys, key.code);
            } else {
                model.forwarded_keys.retain(|code| *code != key.code);
                remember_key(&mut model.suppressed_releases, key.code);
            }
        }
    }
    action
}

fn remember_key(keys: &mut Vec<KeyCode>, key: KeyCode) {
    if keys.contains(&key) {
        return;
    }
    // Legacy terminals do not report releases. Arbitrary sidebar text must not
    // grow this bookkeeping without a bound; a later forwarded press supersedes it.
    if keys.len() == 64 {
        keys.remove(0);
    }
    keys.push(key);
}

fn handle_event_inner(model: &mut Model, event: Event, geometry: &Geometry) -> Action {
    if model.prefix && !matches!(event, Event::Key(_)) {
        return Action::None;
    }
    if let Event::Key(key) = &event {
        if key.code == KeyCode::F(10) && model.focus != Focus::Terminal && !model.modal_open() {
            return Action::Quit;
        }
    }
    if model.add_host.is_some() {
        return handle_dialog(model, event, geometry);
    }
    if model.menu.is_some() {
        return handle_menu(model, event, geometry);
    }
    match event {
        Event::Key(key) => {
            if model.prefix {
                if key.code == KeyCode::Up {
                    model.command_detail_offset = model.command_detail_offset.saturating_sub(1);
                    return Action::None;
                }
                if key.code == KeyCode::Down {
                    model.command_detail_offset = model.command_detail_offset.saturating_add(1);
                    return Action::None;
                }
                model.clear_prefix();
                return match key.code {
                    KeyCode::Char('q') => Action::Quit,
                    KeyCode::Char('h') => {
                        model.focus = Focus::Hosts;
                        Action::None
                    }
                    KeyCode::Char('s') => {
                        model.focus = Focus::Sessions;
                        Action::None
                    }
                    KeyCode::Enter if model.selected_session.is_some() => {
                        if model
                            .selected_session
                            .and_then(|index| model.sessions.get(index))
                            .is_some_and(|session| session.self_session)
                        {
                            explain_self(model, None);
                        } else {
                            model.focus = Focus::Terminal;
                        }
                        Action::None
                    }
                    KeyCode::Char('n') => Action::CreateSession,
                    KeyCode::Char('a') => {
                        model.add_host = Some(AddHost::default());
                        Action::None
                    }
                    KeyCode::Char('m') => {
                        menu_for_focus(model, geometry);
                        Action::None
                    }
                    KeyCode::Char('b')
                        if is_command_shortcut(key)
                            && geometry.terminal.width > 0
                            && geometry.terminal.height > 0 =>
                    {
                        Action::SendKey(key)
                    }
                    _ => Action::None,
                };
            }
            if is_command_shortcut(key) {
                model.prefix = true;
                model.command_detail_offset = 0;
                return Action::None;
            }
            if model.focus == Focus::Terminal {
                return if geometry.terminal.width > 0 && geometry.terminal.height > 0 {
                    match (key.modifiers.contains(KeyModifiers::SHIFT), key.code) {
                        (true, KeyCode::PageUp) => Action::Scroll(HistoryAction::PageUp),
                        (true, KeyCode::PageDown) => Action::Scroll(HistoryAction::PageDown),
                        (true, KeyCode::Home) => Action::Scroll(HistoryAction::Start),
                        (true, KeyCode::End) => Action::Scroll(HistoryAction::End),
                        _ => Action::SendKey(key),
                    }
                } else {
                    Action::None
                };
            }
            if key.code == KeyCode::F(6) {
                model.next_focus();
                return Action::None;
            }
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    model.next_focus();
                    Action::None
                }
                KeyCode::Char('+') => {
                    if model.focus == Focus::Hosts {
                        model.add_host = Some(AddHost::default());
                        Action::None
                    } else {
                        Action::CreateSession
                    }
                }
                KeyCode::Char('m') | KeyCode::F(2) => {
                    menu_for_focus(model, geometry);
                    Action::None
                }
                KeyCode::Up | KeyCode::Char('k') => move_selection(model, -1),
                KeyCode::Down | KeyCode::Char('j') => move_selection(model, 1),
                KeyCode::Enter if model.focus == Focus::Sessions => {
                    if let Some(index) = model.selected_session {
                        select_session_row(model, index, true)
                    } else if model.sessions.iter().any(|session| session.self_session) {
                        explain_self(model, None);
                        Action::None
                    } else {
                        Action::CreateSession
                    }
                }
                _ => Action::None,
            }
        }
        Event::Paste(value)
            if model.focus == Focus::Terminal
                && geometry.terminal.width > 0
                && geometry.terminal.height > 0 =>
        {
            Action::Paste(value)
        }
        Event::Mouse(mouse) => handle_mouse(model, mouse, geometry),
        _ => Action::None,
    }
}

fn move_selection(model: &mut Model, delta: isize) -> Action {
    match model.focus {
        Focus::Hosts if !model.hosts.is_empty() => {
            let index = model
                .selected_host
                .saturating_add_signed(delta)
                .min(model.hosts.len() - 1);
            model.selected_host = index;
            Action::SelectHost(index)
        }
        Focus::Sessions if !model.sessions.is_empty() => {
            let mut index = model
                .selected_session
                .map_or(0, |selected| selected.saturating_add_signed(delta))
                .min(model.sessions.len() - 1);
            loop {
                if !model.sessions[index].self_session {
                    return select_session_row(model, index, false);
                }
                let Some(next) = index
                    .checked_add_signed(if delta < 0 { -1 } else { 1 })
                    .filter(|next| *next < model.sessions.len())
                else {
                    break;
                };
                index = next;
            }
            explain_self(model, Some(index));
            Action::None
        }
        _ => Action::None,
    }
}

fn explain_self(model: &mut Model, index: Option<usize>) {
    model.focus = Focus::Sessions;
    model.notice = "承载当前TUI，不能重复接入；退出后再关闭".into();
    if let (Some(host), Some(session)) = (
        model.hosts.get(model.selected_host),
        index
            .or(model.selected_session)
            .and_then(|index| model.sessions.get(index))
            .filter(|session| session.self_session)
            .or_else(|| model.sessions.iter().find(|session| session.self_session)),
    ) {
        model.item_notice = Some(ItemNotice {
            target: NoticeTarget::Session {
                target: host.target.clone(),
                name: session.name.clone(),
            },
            summary: "当前界面承载会话".into(),
            detail: model.notice.clone(),
        });
    }
}

fn select_session_row(model: &mut Model, index: usize, enter_terminal: bool) -> Action {
    if model
        .sessions
        .get(index)
        .is_some_and(|session| session.self_session)
    {
        explain_self(model, Some(index));
        Action::None
    } else {
        model.selected_session = Some(index);
        if enter_terminal {
            model.focus = Focus::Terminal;
        }
        Action::SelectSession(index)
    }
}

fn handle_mouse(model: &mut Model, mouse: MouseEvent, geometry: &Geometry) -> Action {
    let (x, y) = (mouse.column, mouse.row);
    if contains(geometry.terminal_close, x, y) {
        return if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
            model
                .close_target()
                .cloned()
                .map(Action::CloseTerminalSession)
                .unwrap_or(Action::None)
        } else {
            Action::None
        };
    }
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        if contains(geometry.host_add, x, y) {
            model.add_host = Some(AddHost::default());
            return Action::None;
        }
        if contains(geometry.session_add, x, y) || contains(geometry.create, x, y) {
            return Action::CreateSession;
        }
    }
    if contains(geometry.hosts, x, y) {
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
        ) {
            model.focus = Focus::Hosts;
        }
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return move_selection(
                model,
                if mouse.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                },
            );
        }
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
        ) {
            let Some(index) = item_index(geometry.hosts, y, model.hosts_offset) else {
                return Action::None;
            };
            if let Some(host) = model.hosts.get(index) {
                model.selected_host = index;
                if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                    model.menu = Some(Menu {
                        kind: if host.state == HostState::Local {
                            MenuKind::Local
                        } else {
                            MenuKind::Host
                        },
                        x,
                        y,
                        selected: 0,
                    });
                }
                return Action::SelectHost(index);
            }
        }
    }
    if contains(geometry.sessions, x, y) {
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
        ) {
            model.focus = Focus::Sessions;
        }
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) {
            return move_selection(
                model,
                if mouse.kind == MouseEventKind::ScrollUp {
                    -1
                } else {
                    1
                },
            );
        }
        if matches!(
            mouse.kind,
            MouseEventKind::Down(MouseButton::Left | MouseButton::Right)
        ) {
            let Some(index) = item_index(geometry.sessions, y, model.sessions_offset) else {
                return Action::None;
            };
            if index < model.sessions.len() {
                if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                    model.menu = Some(Menu {
                        kind: if model.sessions[index].self_session {
                            MenuKind::SelfInfo
                        } else {
                            MenuKind::Session
                        },
                        x,
                        y,
                        selected: 0,
                    });
                }
                return select_session_row(
                    model,
                    index,
                    mouse.kind == MouseEventKind::Down(MouseButton::Left),
                );
            }
        }
    }
    if contains(geometry.terminal, x, y) {
        if matches!(mouse.kind, MouseEventKind::Down(_)) && model.selected_session.is_some() {
            model.focus = Focus::Terminal;
        }
        let mut relative = mouse;
        relative.column -= geometry.terminal.x;
        relative.row -= geometry.terminal.y;
        if matches!(
            mouse.kind,
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
        ) && (mouse.modifiers.contains(KeyModifiers::SHIFT) || !model.mouse_reporting)
        {
            return Action::Scroll(HistoryAction::Lines(
                if mouse.kind == MouseEventKind::ScrollUp {
                    -3
                } else {
                    3
                },
            ));
        }
        return Action::TerminalMouse(relative);
    }
    Action::None
}

fn handle_menu(model: &mut Model, event: Event, geometry: &Geometry) -> Action {
    match event {
        Event::Key(key) => match key.code {
            KeyCode::Esc => {
                model.menu = None;
                Action::None
            }
            KeyCode::Enter => activate_menu(model),
            KeyCode::Up | KeyCode::Down => {
                let Some(menu) = &model.menu else {
                    return Action::None;
                };
                let count = menu_labels(model, menu.kind).len();
                if let Some(menu) = &mut model.menu {
                    menu.selected = if key.code == KeyCode::Up {
                        menu.selected.saturating_sub(1)
                    } else {
                        (menu.selected + 1).min(count - 1)
                    };
                }
                Action::None
            }
            _ => Action::None,
        },
        Event::Mouse(mouse) if matches!(mouse.kind, MouseEventKind::Down(_)) => {
            let content = inner(geometry.menu);
            if contains(content, mouse.column, mouse.row)
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
            {
                if let Some(menu) = &mut model.menu {
                    menu.selected = usize::from(mouse.row - content.y);
                }
                activate_menu(model)
            } else {
                model.menu = None;
                Action::None
            }
        }
        _ => Action::None,
    }
}

fn handle_dialog(model: &mut Model, event: Event, geometry: &Geometry) -> Action {
    match event {
        Event::Key(key) => match key.code {
            KeyCode::Esc => {
                model.add_host = None;
                Action::None
            }
            KeyCode::Enter => submit_host(model),
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Up | KeyCode::Down => {
                if let Some(dialog) = &mut model.add_host {
                    dialog.field = 1 - dialog.field;
                }
                Action::None
            }
            KeyCode::Backspace => {
                if let Some(dialog) = &mut model.add_host {
                    if dialog.field == 0 {
                        dialog.name.pop();
                    } else {
                        dialog.token.pop();
                    }
                }
                Action::None
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(dialog) = &mut model.add_host {
                    if dialog.field == 0 {
                        dialog.name.clear();
                    } else {
                        dialog.token.clear();
                    }
                }
                Action::None
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && !c.is_control() =>
            {
                if let Some(dialog) = &mut model.add_host {
                    let value = if dialog.field == 0 {
                        &mut dialog.name
                    } else {
                        &mut dialog.token
                    };
                    if value.len() < 65536 {
                        value.push(c);
                    }
                }
                Action::None
            }
            _ => Action::None,
        },
        Event::Paste(value) => {
            if let Some(dialog) = &mut model.add_host {
                let target = if dialog.field == 0 {
                    &mut dialog.name
                } else {
                    &mut dialog.token
                };
                for c in value.chars().filter(|c| !c.is_control()) {
                    if target.len() + c.len_utf8() > 65536 {
                        break;
                    }
                    target.push(c);
                }
            }
            Action::None
        }
        Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
            if contains(geometry.submit, mouse.column, mouse.row) {
                return submit_host(model);
            }
            if let Some(dialog) = &mut model.add_host {
                if contains(geometry.name_input, mouse.column, mouse.row) {
                    dialog.field = 0;
                }
                if contains(geometry.token_input, mouse.column, mouse.row) {
                    dialog.field = 1;
                }
            }
            Action::None
        }
        _ => Action::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{
        backend::TestBackend,
        buffer::{Buffer, CellWidth},
        Terminal,
    };

    fn buffer_line(buffer: &Buffer, y: u16) -> String {
        let mut text = String::new();
        let mut x = buffer.area.x;
        while x < buffer.area.right() {
            let cell = &buffer[(x, y)];
            text.push_str(cell.symbol());
            x += cell.cell_width().max(1);
        }
        text
    }

    fn draw(
        frame: &mut Frame<'_>,
        model: &mut Model,
        terminal: Option<&mut SessionVt>,
    ) -> Geometry {
        if let Some(vt) = terminal {
            let rows = vt.rows();
            let cursor = vt.cursor();
            let scrollbar = vt.scrollbar();
            super::draw(
                frame,
                model,
                Some(TerminalView {
                    rows: &rows,
                    cursor: Some(cursor),
                    mouse_reporting: vt.modes().mouse_tracking(),
                    scrollbar,
                    top: scrollbar.offset,
                }),
            )
        } else {
            super::draw(frame, model, None)
        }
    }

    fn draw_vt_terminal(
        frame: &mut Frame<'_>,
        area: Rect,
        vt: &mut SessionVt,
        palette: Palette,
        cursor: bool,
    ) {
        let rows = vt.rows();
        let scrollbar = vt.scrollbar();
        super::draw_terminal(
            frame,
            area,
            TerminalView {
                rows: &rows,
                cursor: Some(vt.cursor()),
                mouse_reporting: vt.modes().mouse_tracking(),
                scrollbar,
                top: scrollbar.offset,
            },
            palette,
            cursor,
        );
    }

    #[test]
    fn authoritative_rows_use_window_order_and_clip_wide_right_edge_without_reflow() {
        let mut terminal = Terminal::new(TestBackend::new(4, 2)).unwrap();
        let wide = |symbol: &str| vt::Cell {
            symbol: symbol.into(),
            width: 2,
            bg: vt::Color::Palette(4),
            ..Default::default()
        };
        let ascii = |symbol: &str| vt::Cell {
            symbol: symbol.into(),
            width: 1,
            ..Default::default()
        };
        let rows = vec![
            vt::Row {
                y: 0,
                cells: vec![
                    wide("中"),
                    vt::Cell {
                        width: 0,
                        skip: true,
                        ..Default::default()
                    },
                    ascii("X"),
                    wide("文"),
                ],
                ..Default::default()
            },
            // FETCH windows may report local y=0 for every materialized row.
            vt::Row {
                y: 0,
                cells: vec![ascii("B")],
                ..Default::default()
            },
        ];
        terminal
            .draw(|frame| {
                let area = frame.area();
                super::draw_terminal(
                    frame,
                    area,
                    TerminalView {
                        rows: &rows,
                        cursor: None,
                        mouse_reporting: false,
                        scrollbar: vt::Scrollbar {
                            total: 20,
                            offset: 18,
                            len: 2,
                        },
                        top: 5,
                    },
                    Theme::Dark.palette(),
                    true,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "中");
        assert_eq!(buffer[(2, 0)].symbol(), "X");
        assert_eq!(buffer[(3, 0)].symbol(), " ");
        assert_eq!(buffer[(3, 0)].bg, Color::Indexed(4));
        assert_eq!(
            buffer[(0, 1)].symbol(),
            "B",
            "history rows must not overwrite row zero or reflow"
        );
    }

    #[test]
    fn layout_reserves_only_sidebar_footer_and_uses_full_terminal_height() {
        for (width, height) in [(40, 10), (48, 12), (100, 30)] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut model = Model {
                notice: "READY".into(),
                ..Default::default()
            };
            let mut geometry = Geometry::default();
            terminal
                .draw(|frame| geometry = super::draw(frame, &mut model, None))
                .unwrap();
            assert_eq!(geometry.terminal.y, 1);
            assert_eq!(geometry.terminal.height, height - 2);
            let buffer = terminal.backend().buffer();
            let line = |y| buffer_line(buffer, y);
            assert!(line(0).contains("主机") && line(0).contains("终端"));
            assert!(!line(0).contains("homeway") && !line(0).contains("主题"));
            assert!(line(height - 2).contains(COMMAND_SHORTCUT));
            assert!(line(height - 2).contains("指令"));
            assert!(!line(height - 2).contains("READY"));
            assert!(line(height - 1).contains('┘'));
            assert!(!line(height - 1).contains("快捷指令"));
            let title_x = (0..width)
                .find(|&x| buffer[(x, geometry.sessions.y - 1)].symbol() == "会")
                .unwrap();
            assert_eq!(buffer[(title_x, geometry.sessions.y)].symbol(), "暂");
            assert_eq!(buffer[(title_x, geometry.footer.y)].symbol(), "C");
            assert_eq!(geometry.footer.x, title_x);
            assert!(
                Line::from(footer(&model, geometry.footer.width)).width()
                    <= usize::from(geometry.footer.width)
            );
            for theme in [Theme::Dark, Theme::Light] {
                for focus in [Focus::Hosts, Focus::Terminal] {
                    for present in [false, true] {
                        model.theme = theme;
                        model.focus = focus;
                        model.terminal_present = present;
                        terminal
                            .draw(|frame| geometry = super::draw(frame, &mut model, None))
                            .unwrap();
                        let buffer = terminal.backend().buffer();
                        let mut compared = 0;
                        for x in geometry.terminal.x..geometry.terminal.right() {
                            let top = &buffer[(x, geometry.terminal.y - 1)];
                            if top.symbol() != "─" {
                                continue;
                            }
                            let bottom = &buffer[(x, geometry.terminal.bottom())];
                            assert_eq!(top.symbol(), bottom.symbol());
                            assert_eq!(
                                top.style(),
                                bottom.style(),
                                "{theme:?} {focus:?} present={present} x={x}"
                            );
                            assert!(!top.modifier.intersects(Modifier::BOLD | Modifier::DIM));
                            compared += 1;
                        }
                        assert!(compared > 0, "must compare non-title border cells");
                        assert!(
                            buffer[(geometry.terminal.x + 1, 0)]
                                .modifier
                                .contains(Modifier::BOLD),
                            "title itself remains bold"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn terminal_close_button_reserves_header_space_and_only_handles_left_press() {
        for theme in [Theme::Dark, Theme::Light] {
            for (width, height) in [(40, 10), (100, 30)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                let target = TerminalClose {
                    name: "session".into(),
                    navigation_epoch: 7,
                };
                let mut model = Model {
                    hosts: vec![host("我的电脑", HostState::Local)],
                    sessions: vec![SessionItem {
                        name: target.name.clone(),
                        title: "session".into(),
                        agent: "shell".into(),
                        project: "project".into(),
                        state: SessionState::Idle,
                        self_session: false,
                    }],
                    selected_session: Some(0),
                    terminal_title: "中文title".repeat(40),
                    terminal_present: true,
                    terminal_close: Some(target.clone()),
                    theme,
                    ..Default::default()
                };
                let mut geometry = Geometry::default();
                terminal
                    .draw(|frame| geometry = draw(frame, &mut model, None))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let close = geometry.terminal_close;
                assert_eq!(close.width, geometry.host_add.width);
                assert_eq!(
                    (close.x..close.right())
                        .map(|x| buffer[(x, close.y)].symbol())
                        .collect::<String>(),
                    "[ X ]"
                );
                assert_eq!(
                    buffer[(close.x, close.y)].fg,
                    buffer[(geometry.host_add.x, geometry.host_add.y)].fg
                );
                assert_eq!(
                    buffer[(close.x - 1, close.y)].symbol(),
                    "─",
                    "title must leave a gap before X"
                );
                assert_eq!(buffer[(close.right(), close.y)].symbol(), "┐");
                assert!(close.bottom() <= geometry.terminal.y);
                assert!(
                    matches!(handle_event(&mut model,mouse(close.x+2,close.y,MouseButton::Left),&geometry),
                    Action::CloseTerminalSession(value) if value==target)
                );
                for kind in [
                    MouseEventKind::Up(MouseButton::Left),
                    MouseEventKind::Down(MouseButton::Right),
                    MouseEventKind::Moved,
                ] {
                    assert!(matches!(
                        handle_event(
                            &mut model,
                            Event::Mouse(MouseEvent {
                                kind,
                                column: close.x + 2,
                                row: close.y,
                                modifiers: KeyModifiers::NONE
                            }),
                            &geometry
                        ),
                        Action::None
                    ));
                }
                for invalid in ["self", "missing", "no target"] {
                    model.sessions[0].self_session = invalid == "self";
                    model.terminal_close = if invalid == "no target" {
                        None
                    } else {
                        Some(TerminalClose {
                            name: if invalid == "self" {
                                target.name.clone()
                            } else {
                                invalid.into()
                            },
                            navigation_epoch: 7,
                        })
                    };
                    terminal
                        .draw(|frame| geometry = draw(frame, &mut model, None))
                        .unwrap();
                    assert_eq!(
                        terminal.backend().buffer()[(close.x, close.y)].fg,
                        theme.palette().muted
                    );
                    assert!(matches!(
                        handle_event(
                            &mut model,
                            mouse(close.x + 2, close.y, MouseButton::Left),
                            &geometry
                        ),
                        Action::None
                    ));
                }
                model.sessions[0].self_session = false;
                model.terminal_present = false;
                model.terminal_close = Some(target.clone()); // A stale payload cannot restore a hidden hitbox.
                terminal
                    .draw(|frame| geometry = draw(frame, &mut model, None))
                    .unwrap();
                assert!(geometry.terminal_close.is_empty());
                assert!(!buffer_line(terminal.backend().buffer(), 0).contains("[ X ]"));
                assert_ne!(
                    terminal.backend().buffer()[(close.x - 1, close.y)].symbol(),
                    "─",
                    "empty view returns the reserved header space to the title"
                );
                assert!(matches!(
                    handle_event(
                        &mut model,
                        mouse(close.x + 2, close.y, MouseButton::Left),
                        &geometry
                    ),
                    Action::None
                ));
            }
        }
    }

    #[test]
    fn sidebar_fits_host_rows_and_aligns_dots_with_panel_titles() {
        for (count, height) in [(1, 6), (2, 8), (20, 15)] {
            let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
            let mut model = Model {
                hosts: std::iter::once(host("我的电脑", HostState::Local))
                    .chain((1..count).map(|_| host("remote", HostState::Direct)))
                    .collect(),
                ..Default::default()
            };
            let mut geometry = Geometry::default();
            terminal
                .draw(|frame| geometry = draw(frame, &mut model, None))
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(geometry.hosts.height + 2, height);
            assert_eq!(geometry.sessions.y - 1, geometry.hosts.bottom());
            assert_eq!(geometry.sessions.bottom(), 28);
            assert_eq!(geometry.terminal.x, 24);
            assert_eq!(buffer[(2, 0)].symbol(), "主");
            assert_eq!(buffer[(2, 1)].symbol(), "●");
            assert_eq!(buffer[(2, height - 1)].symbol(), "会");
            assert!(!buffer_line(buffer, 0).contains(&format!("主机 {count}")));
            let first = buffer_line(buffer, 1);
            assert!(first.contains("● 我的电脑"));
            assert!(!first.contains("【本地】"));
            if count == 2 {
                assert!(buffer_line(buffer, 3).contains("● remote"));
                assert!(!buffer_line(buffer, 3).contains("【"));
            }
            if count <= 2 {
                for y in count * 2 + 1..count * 2 + 3 {
                    for x in geometry.hosts.x..geometry.hosts.right() {
                        assert_eq!(buffer[(x, y)].symbol(), " ");
                    }
                }
                assert!(matches!(
                    handle_event(
                        &mut model,
                        mouse(2, height - 2, MouseButton::Left),
                        &geometry
                    ),
                    Action::None
                ));
            }
        }
        // Host details align with the title text and session details; long
        // fields are clipped at the existing sidebar boundary.
        for theme in [Theme::Dark, Theme::Light] {
            for (state, rtt_ms, proxy_on, exit_on, relay_on, expected) in [
                (
                    HostState::Local,
                    None,
                    false,
                    true,
                    false,
                    "exit:on relay:off",
                ),
                (
                    HostState::Local,
                    None,
                    false,
                    false,
                    true,
                    "exit:off relay:on",
                ),
                (
                    HostState::Direct,
                    Some(12),
                    true,
                    false,
                    false,
                    "direct 12ms proxy:on",
                ),
                (
                    HostState::Direct,
                    Some(0),
                    false,
                    false,
                    false,
                    "direct 0ms proxy:off",
                ),
                (
                    HostState::Relay,
                    None,
                    false,
                    false,
                    false,
                    "relay --ms proxy:off",
                ),
                (
                    HostState::Offline,
                    None,
                    false,
                    false,
                    false,
                    "offline --ms proxy:off",
                ),
            ] {
                let mut item = host("主机", state);
                item.rtt_ms = rtt_ms;
                item.proxy_on = proxy_on;
                item.exit_on = exit_on;
                item.relay_on = relay_on;
                let mut model = Model {
                    hosts: vec![item],
                    sessions: vec![SessionItem {
                        name: "session".into(),
                        title: "会话".into(),
                        agent: "shell".into(),
                        project: "project".into(),
                        state: SessionState::Idle,
                        self_session: false,
                    }],
                    theme,
                    ..Default::default()
                };
                let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
                let mut geometry = Geometry::default();
                terminal
                    .draw(|frame| geometry = draw(frame, &mut model, None))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let row = geometry.hosts;
                let detail = (row.x..row.right())
                    .map(|x| buffer[(x, row.y + 1)].symbol())
                    .collect::<String>();
                let visible = expected
                    .chars()
                    .take(usize::from(row.width.saturating_sub(3)))
                    .collect::<String>();
                assert_eq!(detail.trim_end(), format!("   {visible}"));
                let title_x = (row.x..row.right())
                    .find(|&x| buffer[(x, row.y)].symbol() == "主")
                    .unwrap();
                let detail_x = (row.x..row.right())
                    .find(|&x| buffer[(x, row.y + 1)].symbol() != " ")
                    .unwrap();
                let session_x = (geometry.sessions.x..geometry.sessions.right())
                    .find(|&x| buffer[(x, geometry.sessions.y + 1)].symbol() != " ")
                    .unwrap();
                assert_eq!(detail_x, title_x);
                assert_eq!(detail_x, session_x);
                assert_eq!(detail_x, row.x + 3);
                assert!(buffer_line(buffer, row.y).contains("● 主机"));
                assert!(!buffer_line(buffer, row.y).contains("【"));
                assert_eq!(buffer[(row.x - 1, row.y + 1)].symbol(), "│");
                assert_eq!(buffer[(row.right(), row.y + 1)].symbol(), "│");
                assert_eq!(buffer[(row.x, row.y + 1)].bg, theme.palette().selected);
                assert_eq!(
                    buffer[(row.x + 1, row.y)].fg,
                    match state {
                        HostState::Local => theme.palette().accent,
                        HostState::Direct => theme.palette().green,
                        HostState::Relay => theme.palette().amber,
                        HostState::Offline => theme.palette().muted,
                    }
                );
                model.item_notice = Some(ItemNotice {
                    target: NoticeTarget::Host(model.hosts[0].target.clone()),
                    summary: "提示".into(),
                    detail: "完整提示".into(),
                });
                terminal
                    .draw(|frame| {
                        draw(frame, &mut model, None);
                    })
                    .unwrap();
                assert_eq!(
                    terminal.backend().buffer()[(row.x + 3, row.y + 1)].symbol(),
                    "提"
                );
                assert_eq!(
                    terminal.backend().buffer()[(row.x + 3, row.y + 1)].fg,
                    theme.palette().amber
                );
            }
        }
        let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
        let mut model = Model {
            hosts: (0..20).map(|_| host("remote", HostState::Relay)).collect(),
            ..Default::default()
        };
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        assert_eq!(geometry.hosts.height + 2, 5);
        assert_eq!(geometry.sessions.height, 3);
        assert_eq!(geometry.terminal.x, 14);
        assert!(geometry.host_add.right() < geometry.terminal.x);
    }

    #[test]
    fn session_status_moves_to_colored_detail_without_changing_selected_background() {
        for theme in [Theme::Dark, Theme::Light] {
            for (state, label) in [
                (SessionState::Working, "working"),
                (SessionState::Idle, "idle"),
                (SessionState::Blocked, "blocking"),
                (SessionState::Unknown, "unknown"),
            ] {
                let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
                let mut model = Model {
                    hosts: vec![host("我的电脑", HostState::Local)],
                    sessions: vec![SessionItem {
                        name: "session".into(),
                        title: "[my title]".into(),
                        agent: "codex".into(),
                        project: "project".into(),
                        state,
                        self_session: false,
                    }],
                    selected_session: Some(0),
                    theme,
                    ..Default::default()
                };
                let mut geometry = Geometry::default();
                terminal
                    .draw(|frame| geometry = draw(frame, &mut model, None))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let first = buffer_line(buffer, geometry.sessions.y);
                assert!(first.contains("● [my title]"));
                assert!(!first.contains(label) && !first.contains('【'));
                let detail = buffer_line(buffer, geometry.sessions.y + 1);
                assert!(detail.contains(&format!("{label} codex project")));
                assert!(!detail.contains(&format!("{label}  codex")));
                let dot = &buffer[(2, geometry.sessions.y)];
                assert_eq!(dot.symbol(), "●");
                for x in geometry.sessions.x..geometry.sessions.right() {
                    let cell = &buffer[(x, geometry.sessions.y + 1)];
                    assert_eq!(cell.fg, dot.fg, "{theme:?} {label} column {x}");
                    assert_eq!(cell.bg, theme.palette().selected);
                }
            }
        }
    }

    #[test]
    fn session_detail_keeps_three_fields_and_legal_directory_spaces() {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        let mut model = Model {
            hosts: vec![host("我的电脑", HostState::Local)],
            sessions: vec![SessionItem {
                name: "session".into(),
                title: "terminal".into(),
                agent: "codex".into(),
                project: String::new(),
                state: SessionState::Idle,
                self_session: false,
            }],
            ..Default::default()
        };
        for (project, expected) in [
            ("", "idle codex —"),
            ("my project", "idle codex my project"),
        ] {
            model.sessions[0].project = project.into();
            let mut geometry = Geometry::default();
            terminal
                .draw(|frame| geometry = draw(frame, &mut model, None))
                .unwrap();
            let line = buffer_line(terminal.backend().buffer(), geometry.sessions.y + 1);
            assert!(line.contains(expected));
            assert!(!line.contains("idle  codex") && !line.contains("codex  "));
        }
    }

    #[test]
    #[ignore = "writes sidebar preview fixtures under /tmp"]
    fn sidebar_layout_preview_artifacts() {
        let directory = std::path::Path::new("/tmp/homeway-tui-sidebar-previews");
        std::fs::create_dir_all(directory).unwrap();
        for (name, theme, many, width, height) in [
            ("one-dark", Theme::Dark, false, 100, 30),
            ("one-light", Theme::Light, false, 100, 30),
            ("many-dark", Theme::Dark, true, 100, 30),
            ("many-light", Theme::Light, true, 100, 30),
            ("small-light", Theme::Light, true, 40, 10),
            ("commands-dark", Theme::Dark, false, 100, 30),
            ("commands-light", Theme::Light, false, 100, 30),
            ("commands-long-light", Theme::Light, false, 100, 30),
            ("commands-small-light", Theme::Light, false, 40, 10),
            ("empty-light", Theme::Light, false, 100, 30),
            ("status-light", Theme::Light, false, 100, 30),
        ] {
            let mut hosts = vec![host("我的电脑", HostState::Local)];
            if many {
                hosts.extend([
                    host("办公室", HostState::Direct),
                    host("家里", HostState::Relay),
                    host("笔记本", HostState::Offline),
                ]);
            }
            hosts[0].exit_on = true;
            if many {
                hosts[1].proxy_on = true;
                hosts[2].rtt_ms = Some(48);
            }
            let mut model = Model {
                hosts,
                sessions: [
                    ("修改界面", SessionState::Working, "codex"),
                    ("开发终端", SessionState::Idle, "shell"),
                    ("等待确认", SessionState::Blocked, "pi"),
                    ("当前终端", SessionState::Working, "opencode"),
                ]
                .into_iter()
                .enumerate()
                .map(|(index, (title, state, agent))| SessionItem {
                    name: format!("session-{index}"),
                    title: title.into(),
                    agent: agent.into(),
                    project: "homeway".into(),
                    state,
                    self_session: index == 3,
                })
                .collect(),
                selected_session: Some(1),
                terminal_present: true,
                terminal_close: Some(TerminalClose {
                    name: "session-1".into(),
                    navigation_epoch: 0,
                }),
                focus: Focus::Sessions,
                theme,
                notice: "已就绪 · 选择会话，或点击 + 创建 shell".into(),
                ..Default::default()
            };
            if name.starts_with("empty") {
                model.sessions.clear();
                model.selected_session = None;
                model.terminal_present = false;
            }
            if name.starts_with("commands") || name == "status-light" {
                model.item_notice = Some(ItemNotice {
                    target: NoticeTarget::Session {
                        target: Target::Local,
                        name: "session-1".into(),
                    },
                    summary: "已发复制请求".into(),
                    detail: if name == "commands-long-light" {
                        (0..160).map(|index| format!("详情{index:03} ")).collect()
                    } else if name.starts_with("commands") {
                        "已提交终端剪贴板请求。".into()
                    } else {
                        "选中内容已提交终端剪贴板请求；请在承载终端中允许写入剪贴板。".into()
                    },
                });
                model.prefix = name.starts_with("commands");
            }
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    draw(frame, &mut model, None);
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            let color = |value| match value {
                Color::Rgb(r, g, b) => format!("{r:02x}{g:02x}{b:02x}"),
                _ => "default".into(),
            };
            let rows: Vec<Vec<_>> = (0..height).map(|y| (0..width).map(|x| {
                let cell = &buffer[(x, y)];
                serde_json::json!({"data": cell.symbol(), "fg": color(cell.fg), "bg": color(cell.bg)})
            }).collect()).collect();
            std::fs::write(directory.join(format!("{name}.json")),
                serde_json::to_vec(&serde_json::json!({"width":width,"height":height,"light":theme==Theme::Light,"rows":rows})).unwrap()).unwrap();
            std::fs::write(
                directory.join(format!("{name}.txt")),
                (0..height)
                    .map(|y| buffer_line(buffer, y))
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .unwrap();
        }
    }

    #[test]
    fn history_routing_respects_application_mouse_reporting_and_shift_override() {
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let geometry = Geometry {
            terminal: Rect::new(30, 1, 66, 27),
            ..Geometry::default()
        };
        let wheel = |mods| {
            Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollUp,
                column: 34,
                row: 4,
                modifiers: mods,
            })
        };
        assert!(matches!(
            handle_event(&mut model, wheel(KeyModifiers::NONE), &geometry),
            Action::Scroll(HistoryAction::Lines(-3))
        ));
        model.mouse_reporting = true;
        assert!(
            matches!(handle_event(&mut model,wheel(KeyModifiers::NONE),&geometry),Action::TerminalMouse(event) if event.column==4 && event.row==3)
        );
        assert!(matches!(
            handle_event(&mut model, wheel(KeyModifiers::SHIFT), &geometry),
            Action::Scroll(HistoryAction::Lines(-3))
        ));
        for (key, history) in [
            (KeyCode::PageUp, HistoryAction::PageUp),
            (KeyCode::PageDown, HistoryAction::PageDown),
            (KeyCode::Home, HistoryAction::Start),
            (KeyCode::End, HistoryAction::End),
        ] {
            assert!(matches!(
                handle_event(
                    &mut model,
                    Event::Key(KeyEvent::new(key, KeyModifiers::NONE)),
                    &geometry
                ),
                Action::SendKey(_)
            ));
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new_with_kind(
                    key,
                    KeyModifiers::NONE,
                    KeyEventKind::Release,
                )),
                &geometry,
            );
            assert!(
                matches!(handle_event(&mut model,Event::Key(KeyEvent::new(key,KeyModifiers::SHIFT)),&geometry),Action::Scroll(value) if value==history)
            );
            assert!(matches!(
                handle_event(
                    &mut model,
                    Event::Key(KeyEvent::new_with_kind(
                        key,
                        KeyModifiers::SHIFT,
                        KeyEventKind::Release
                    )),
                    &geometry
                ),
                Action::None
            ));
        }
    }

    #[test]
    fn self_session_is_marked_and_cannot_attach_or_close() {
        let mut model = Model {
            terminal_title: "previous active".into(),
            ..Model::default()
        };
        model.sessions = vec![SessionItem {
            name: "self".into(),
            title: "own shell".into(),
            agent: "shell".into(),
            project: String::new(),
            state: SessionState::Working,
            self_session: true,
        }];
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = super::draw(frame, &mut model, None))
            .unwrap();
        let body = (0..30)
            .map(|y| buffer_line(terminal.backend().buffer(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("自身"));
        assert!(matches!(
            handle_event(
                &mut model,
                mouse(geometry.sessions.x, geometry.sessions.y, MouseButton::Left),
                &geometry
            ),
            Action::None
        ));
        assert!(model.notice.contains("不能重复接入"));
        assert!(model.selected_session.is_none());
        assert_eq!(model.terminal_title, "previous active");
        terminal
            .draw(|frame| {
                super::draw(frame, &mut model, None);
            })
            .unwrap();
        let body = (0..30)
            .map(|y| buffer_line(terminal.backend().buffer(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(body.contains("当前界面运行在该会话中"));
        assert!(!body.contains("正在接入会话"));
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &geometry
            ),
            Action::None
        ));
        handle_event(
            &mut model,
            mouse(geometry.sessions.x, geometry.sessions.y, MouseButton::Right),
            &geometry,
        );
        assert_eq!(
            menu_labels(&model, MenuKind::SelfInfo),
            vec!["承载当前TUI，退出后再关闭"]
        );
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
                &geometry
            ),
            Action::None
        ));
    }

    #[test]
    fn self_row_preserves_existing_target_and_navigation_skips_it() {
        let normal = SessionItem {
            name: "active".into(),
            title: "active".into(),
            agent: "shell".into(),
            project: String::new(),
            state: SessionState::Idle,
            self_session: false,
        };
        let mut carrier = normal.clone();
        carrier.name = "carrier".into();
        carrier.self_session = true;
        let mut other = normal.clone();
        other.name = "other".into();
        let mut model = Model {
            sessions: vec![normal, carrier, other],
            selected_session: Some(0),
            focus: Focus::Sessions,
            ..Default::default()
        };
        assert!(matches!(
            select_session_row(&mut model, 1, true),
            Action::None
        ));
        assert_eq!(model.selected_session, Some(0));
        assert!(matches!(
            move_selection(&mut model, 1),
            Action::SelectSession(2)
        ));
        assert_eq!(model.selected_session, Some(2));
        assert!(matches!(
            move_selection(&mut model, -1),
            Action::SelectSession(0)
        ));
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = super::draw(frame, &mut model, None))
            .unwrap();
        handle_event(
            &mut model,
            mouse(
                geometry.sessions.x,
                geometry.sessions.y + 3,
                MouseButton::Right,
            ),
            &geometry,
        );
        assert_eq!(model.selected_session, Some(0));
        assert!(matches!(
            model.menu.as_ref().unwrap().kind,
            MenuKind::SelfInfo
        ));
        assert!(matches!(activate_menu(&mut model), Action::None));
        assert_eq!(model.selected_session, Some(0));
    }

    #[test]
    fn footer_reports_authoritative_history_distance_without_reparsing_output() {
        let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
        let mut model = Model::default();
        terminal
            .draw(|frame| {
                super::draw(
                    frame,
                    &mut model,
                    Some(TerminalView {
                        rows: &[],
                        cursor: None,
                        mouse_reporting: true,
                        scrollbar: vt::Scrollbar {
                            total: 127,
                            offset: 100,
                            len: 27,
                        },
                        top: 70,
                    }),
                );
            })
            .unwrap();
        let footer = buffer_line(terminal.backend().buffer(), 28);
        assert!(footer.contains("30行 ⇧End回底"));
        assert_eq!(super::footer(&model, 12), "30行⇧End回底");
        model.history_behind = u64::MAX;
        assert!(Line::from(super::footer(&model, 12)).width() <= 12);
        model.item_notice = Some(ItemNotice {
            target: NoticeTarget::Host(Target::Local),
            summary: "详情".into(),
            detail: (0..160)
                .map(|index| format!("详情{index:03} "))
                .collect::<String>()
                + "END_OF_DETAIL",
        });
        model.prefix = true;
        terminal
            .draw(|frame| {
                draw_commands(frame, &mut model, Theme::Dark.palette());
            })
            .unwrap();
        let popup = (0..30)
            .map(|y| buffer_line(terminal.backend().buffer(), y))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(popup.contains(&u64::MAX.to_string()) && popup.contains("Shift+End 回到底部"));
        let detail = |terminal: &Terminal<TestBackend>| {
            let buffer = terminal.backend().buffer();
            let y = (0..30)
                .find(|&y| buffer_line(buffer, y).contains("详情 · ↑↓滚动"))
                .unwrap();
            buffer_line(buffer, y + 1)
        };
        let first_detail_line = detail(&terminal);
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)),
                &Geometry::default(),
            ),
            Action::None
        ));
        terminal
            .draw(|frame| draw_commands(frame, &mut model, Theme::Dark.palette()))
            .unwrap();
        assert_ne!(detail(&terminal), first_detail_line);
        assert!(model.commands_open());
        handle_event(
            &mut model,
            Event::Key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE)),
            &Geometry::default(),
        );
        terminal
            .draw(|frame| draw_commands(frame, &mut model, Theme::Dark.palette()))
            .unwrap();
        assert_eq!(detail(&terminal), first_detail_line);
        model.command_detail_offset = u16::MAX;
        terminal
            .draw(|frame| draw_commands(frame, &mut model, Theme::Dark.palette()))
            .unwrap();
        assert!(model.command_detail_offset > 0 && model.command_detail_offset < u16::MAX);
        assert!(
            (0..30).any(|y| buffer_line(terminal.backend().buffer(), y).contains("END_OF_DETAIL")),
            "the last scroll position shows real detail instead of blank rows"
        );
        assert!(model.mouse_reporting);
        model.reset_sessions();
        assert!(!model.mouse_reporting);
        assert!(
            super::footer(&model, 100) == format!("{COMMAND_SHORTCUT} 快捷指令"),
            "reset clears the derived history hint before the next render"
        );
    }

    #[test]
    fn terminal_tabs_keep_grid_columns_and_style_across_real_draw_diffs() {
        let mut vt = SessionVt::new(20, 4, 10).unwrap();
        vt.write(b"\x1b[44m\x1b[2J\x1b[H\x1b[31mA\tB\x1b[0m");
        assert_eq!(vt.rows()[0].cells[1].symbol, "\t", "VT保留TAB复制语义");
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        terminal
            .draw(|frame| {
                draw_vt_terminal(frame, frame.area(), &mut vt, Theme::Dark.palette(), false)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "A");
        assert_eq!(buffer[(1, 0)].symbol(), " ");
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(1));
        assert_eq!(
            buffer[(1, 0)].fg,
            Theme::Dark.palette().fg,
            "TAB仅写格符号，不继承当前SGR前景"
        );
        assert_eq!(buffer[(1, 0)].bg, Color::Indexed(4), "原网格背景色必须保留");
        assert_eq!(
            buffer[(8, 0)].symbol(),
            "B",
            "TAB已由VT网格推进到第9列，不能二次展开"
        );
        vt.write(b"\x1b[1;1H\x1b[32mC\tD\x1b[0m");
        terminal
            .draw(|frame| {
                draw_vt_terminal(frame, frame.area(), &mut vt, Theme::Light.palette(), false)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "C");
        assert_eq!(
            buffer[(1, 0)].bg,
            Color::Indexed(4),
            "未覆盖的原TAB格背景保留"
        );
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(2));
        assert_eq!(buffer[(8, 0)].symbol(), "D");
        assert_eq!(vt.rows()[0].cells[1].symbol, "\t");
    }

    #[test]
    fn terminal_control_projection_preserves_unicode_and_never_emits_controls() {
        for code in (0u8..=31).chain(127u8..=159) {
            let symbol = char::from(code).to_string();
            assert_eq!(terminal_symbol(&symbol), " ");
        }
        assert_eq!(
            terminal_symbol("\t\u{301}"),
            " \u{301}",
            "控制符基格的组合附标仍保留"
        );
        assert_eq!(terminal_symbol("e\u{301}\u{7f}"), "e\u{301}");
        for symbol in ["中", "e\u{301}", "👩\u{200d}💻", "✈\u{fe0f}", "ｶﾞ"] {
            assert!(matches!(terminal_symbol(symbol), Cow::Borrowed(value) if value == symbol));
        }
        // 经过真实diff/后端flush，不能只检查set_symbol后的buffer而漏掉cell_width断言。
        let mut terminal = Terminal::new(TestBackend::new(4, 1)).unwrap();
        for symbol in ["\0", "\t", "\r", "\x1b", "\u{7f}", "\u{85}", "\t\u{301}"] {
            terminal
                .draw(|frame| {
                    frame.buffer_mut()[(0, 0)].set_symbol(terminal_symbol(symbol).as_ref());
                })
                .unwrap();
        }
    }

    #[test]
    fn selection_highlight_keeps_wide_cells_and_hidden_text_attributes() {
        let mut vt = SessionVt::new(6, 1, 0).unwrap();
        vt.write("中\x1b[7;8mA\x1b[0mB".as_bytes());
        for theme in [Theme::Dark, Theme::Light] {
            let palette = theme.palette();
            let mut terminal = Terminal::new(TestBackend::new(8, 3)).unwrap();
            let area = Rect::new(1, 1, 4, 1);
            terminal
                .draw(|frame| {
                    draw_vt_terminal(frame, area, &mut vt, palette, false);
                    highlight_selection(
                        frame,
                        area,
                        &[
                            selection::Span {
                                row: 0,
                                start: 0,
                                end: 3,
                            },
                            selection::Span {
                                row: 1,
                                start: 0,
                                end: u16::MAX,
                            },
                        ],
                        palette,
                    );
                    // Ratatui emits a CJK head once and skips its physical tail
                    // in the backend diff. Check both cells in the full frame,
                    // where the overlay must still cover the whole wide glyph.
                    for column in 1..4 {
                        let cell = &frame.buffer_mut()[(column, 1)];
                        assert_eq!(cell.fg, palette.bg);
                        assert_eq!(cell.bg, palette.accent);
                        assert!(!cell.modifier.contains(Modifier::REVERSED));
                    }
                    assert!(frame.buffer_mut()[(3, 1)]
                        .modifier
                        .contains(Modifier::HIDDEN));
                })
                .unwrap();
            let buffer = terminal.backend().buffer();
            assert_eq!(buffer[(1, 1)].symbol(), "中");
            assert_eq!(buffer[(2, 1)].symbol(), " ");
            for column in [1, 3] {
                let cell = &buffer[(column, 1)];
                assert_eq!(cell.fg, palette.bg);
                assert_eq!(cell.bg, palette.accent);
                assert!(!cell.modifier.contains(Modifier::REVERSED));
            }
            assert!(buffer[(3, 1)].modifier.contains(Modifier::HIDDEN));
            assert_eq!(buffer[(4, 1)].symbol(), "B");
            assert_eq!(buffer[(4, 1)].bg, palette.bg);
            assert_eq!(buffer[(1, 2)].bg, Color::Reset);
            assert_eq!(vt.rows()[0].cells[2].symbol, "A");
            assert_ne!(vt.rows()[0].cells[2].attr & vt::attr::INVISIBLE, 0);
            assert_ne!(vt.rows()[0].cells[2].attr & vt::attr::INVERSE, 0);
        }
    }

    #[test]
    fn selection_highlight_clamps_to_pane_and_footer_does_not_replace_notice() {
        let mut terminal = Terminal::new(TestBackend::new(6, 3)).unwrap();
        let palette = Theme::Dark.palette();
        terminal
            .draw(|frame| {
                highlight_selection(
                    frame,
                    Rect::new(1, 1, 3, 1),
                    &[
                        selection::Span {
                            row: 0,
                            start: 1,
                            end: u16::MAX,
                        },
                        selection::Span {
                            row: 0,
                            start: 3,
                            end: 0,
                        },
                    ],
                    palette,
                );
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(1, 1)].bg, Color::Reset);
        assert_eq!(buffer[(2, 1)].bg, palette.accent);
        assert_eq!(buffer[(3, 1)].bg, palette.accent);
        assert_eq!(buffer[(4, 1)].bg, Color::Reset);
        let mut model = Model {
            selecting: true,
            notice: "已有错误提示".into(),
            selection_spans: vec![selection::Span {
                row: 0,
                start: 1,
                end: 2,
            }],
            ..Default::default()
        };
        assert_eq!(footer(&model, 100), format!("{COMMAND_SHORTCUT} 快捷指令"));
        assert_eq!(model.notice, "已有错误提示");
        model.reset_terminal_input();
        assert!(!model.selecting && model.selection_spans.is_empty());
        model.mouse_reporting = true;
        assert_eq!(footer(&model, 100), format!("{COMMAND_SHORTCUT} 快捷指令"));
        model.notice.clear();
        assert_eq!(footer(&model, 100), format!("{COMMAND_SHORTCUT} 快捷指令"));
    }

    fn host(name: &str, state: HostState) -> HostItem {
        HostItem {
            target: if state == HostState::Local {
                Target::Local
            } else {
                Target::Remote(name.into())
            },
            name: name.into(),
            rtt_ms: (state != HostState::Offline).then_some(12),
            state,
            exit_on: false,
            relay_on: false,
            proxy_on: false,
        }
    }

    #[test]
    fn item_notices_follow_stable_host_and_session_identity() {
        let session = SessionItem {
            name: "same-name".into(),
            title: "shell".into(),
            agent: "shell".into(),
            project: "project".into(),
            state: SessionState::Idle,
            self_session: false,
        };
        let mut model = Model {
            hosts: vec![
                host("我的电脑", HostState::Local),
                host("remote", HostState::Direct),
            ],
            sessions: vec![session.clone()],
            selected_host: 1,
            selected_session: Some(0),
            item_notice: Some(ItemNotice {
                target: NoticeTarget::Session {
                    target: Target::Local,
                    name: "same-name".into(),
                },
                summary: "复制失败".into(),
                detail: "原主机会话复制失败，请重新拖选".into(),
            }),
            ..Default::default()
        };
        assert!(
            session_notice(&model, &session).is_none(),
            "same name on another host cannot receive a notice"
        );
        assert!(host_notice(&model, &model.hosts[0]).is_some());
        assert!(host_notice(&model, &model.hosts[1]).is_none());
        model.selected_host = 0;
        assert!(session_notice(&model, &session).is_some());
        assert!(host_notice(&model, &model.hosts[0]).is_none());
        model.sessions.clear();
        assert!(
            host_notice(&model, &model.hosts[0]).is_some(),
            "deleted session falls back to its own host"
        );
        model.sessions = vec![session.clone(), session];
        model.sessions[0].name = "outer".into();
        model.sessions[1].name = "inner".into();
        model
            .sessions
            .iter_mut()
            .for_each(|session| session.self_session = true);
        model.selected_session = None;
        assert!(matches!(
            select_session_row(&mut model, 1, true),
            Action::None
        ));
        assert!(
            matches!(&model.item_notice.as_ref().unwrap().target,
            NoticeTarget::Session { name, .. } if name == "inner"),
            "self explanation belongs to the clicked ancestor session"
        );
    }

    #[test]
    fn command_shortcut_repeat_preserves_modifiers_and_release_pairing() {
        let geometry = Geometry {
            terminal: Rect::new(24, 1, 74, 28),
            ..Geometry::default()
        };
        for modifiers in [KeyModifiers::SUPER, KeyModifiers::CONTROL] {
            let mut model = Model {
                focus: Focus::Terminal,
                ..Model::default()
            };
            let press = KeyEvent::new(KeyCode::Char('b'), modifiers);
            let release =
                KeyEvent::new_with_kind(KeyCode::Char('b'), modifiers, KeyEventKind::Release);
            assert!(matches!(
                handle_event(&mut model, Event::Key(press), &geometry),
                Action::None
            ));
            assert!(model.commands_open());
            assert!(matches!(
                handle_event(&mut model, Event::Key(release), &geometry),
                Action::None
            ));
            assert!(
                model.commands_open(),
                "consumed shortcut release keeps the guide open"
            );
            assert!(
                matches!(handle_event(&mut model, Event::Key(press), &geometry), Action::SendKey(key) if key == press)
            );
            assert!(!model.commands_open());
            assert!(
                matches!(handle_event(&mut model, Event::Key(release), &geometry), Action::SendKey(key) if key == release)
            );
        }
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let modified = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::SUPER | KeyModifiers::ALT);
        assert!(
            matches!(handle_event(&mut model, Event::Key(modified), &geometry), Action::SendKey(key) if key == modified)
        );
        assert!(
            !model.commands_open(),
            "unrelated Super combinations stay with the application"
        );
    }

    #[test]
    fn command_popup_is_centered_and_consumes_mouse_paste_and_unknown_keys() {
        for (width, height, shortcut_modifiers) in [
            (40, 10, KeyModifiers::CONTROL),
            (40, 10, KeyModifiers::SUPER),
            (100, 30, KeyModifiers::CONTROL),
            (100, 30, KeyModifiers::SUPER),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut model = Model {
                hosts: vec![host("我的电脑", HostState::Local)],
                item_notice: Some(ItemNotice {
                    target: NoticeTarget::Host(Target::Local),
                    summary: "失败\0\u{1b}[31m".into(),
                    detail: "错误\u{1b}[31m\n说明\t\u{85}结束".into(),
                }),
                focus: Focus::Terminal,
                ..Default::default()
            };
            let mut geometry = Geometry::default();
            let displayed = TerminalView {
                rows: &[],
                cursor: Some(vt::Cursor {
                    visible: true,
                    ..Default::default()
                }),
                mouse_reporting: false,
                scrollbar: vt::Scrollbar::default(),
                top: 0,
            };
            terminal
                .draw(|frame| geometry = super::draw(frame, &mut model, Some(displayed)))
                .unwrap();
            assert!(terminal.backend().cursor_visible());
            let original_pane = geometry.terminal;
            let original_buffer = terminal.backend().buffer().clone();
            assert!(matches!(
                handle_event(
                    &mut model,
                    Event::Key(KeyEvent::new(KeyCode::Char('b'), shortcut_modifiers)),
                    &geometry
                ),
                Action::None
            ));
            assert!(model.modal_open());
            terminal
                .draw(|frame| {
                    geometry = super::draw(frame, &mut model, Some(displayed));
                })
                .unwrap();
            assert_eq!(
                geometry.terminal, original_pane,
                "command overlay cannot resize the session"
            );
            assert!(
                !terminal.backend().cursor_visible(),
                "modal hides the terminal cursor"
            );
            let buffer = terminal.backend().buffer();
            for y in 0..height {
                for x in 0..width {
                    assert!(buffer[(x, y)]
                        .symbol()
                        .chars()
                        .all(|char| !char.is_control()));
                }
            }
            let title_y = (0..height)
                .find(|&y| buffer_line(buffer, y).contains("快捷指令 · Ctrl+B"))
                .unwrap();
            let left = (0..width)
                .find(|&x| buffer[(x, title_y)].symbol() == "┌")
                .unwrap();
            let right = (left..width)
                .find(|&x| buffer[(x, title_y)].symbol() == "┐")
                .unwrap();
            let bottom = (title_y..height)
                .find(|&y| buffer[(left, y)].symbol() == "└")
                .unwrap();
            assert!(left > 0 && title_y > 0 && right < width - 1 && bottom < height - 1);
            assert!(left.abs_diff(width - right - 1) <= 1);
            assert!(title_y.abs_diff(height - bottom - 1) <= 1);
            assert!(right - left < 64 && bottom - title_y < 16);
            let first_y = (title_y + 1..bottom)
                .find(|&y| buffer_line(buffer, y).contains("h 主机  s 会话  Enter 终端"))
                .unwrap();
            assert!(buffer_line(buffer, first_y + 1).contains("n 新建会话  a 添加主机"));
            assert!(buffer_line(buffer, first_y + 2).contains("m 操作菜单  q 分离退出"));
            assert!(buffer_line(buffer, first_y + 3).contains("Ctrl+B 连按发送 Ctrl+B  Esc 取消"));
            assert!(buffer_line(buffer, first_y + 4).contains("详情 · ↑↓滚动"));
            assert!(buffer_line(buffer, first_y + 5).contains("错误"));
            assert!(
                (first_y - title_y - 1).abs_diff(bottom - first_y - 6) <= 1,
                "six visible content rows are vertically centered"
            );
            let first_x = (left + 1..right)
                .find(|&x| buffer[(x, first_y)].symbol() == "h")
                .unwrap();
            let group_width = Line::from("Ctrl+B 连按发送 Ctrl+B  Esc 取消").width() as u16;
            assert!(
                (first_x - left - 1).abs_diff(right - first_x - group_width) <= 1,
                "shared content block is centered using display columns"
            );
            for (offset, symbol) in [(0, "h"), (1, "n"), (2, "m"), (3, "C"), (4, "详"), (5, "错")]
            {
                assert_eq!(buffer[(first_x, first_y + offset)].symbol(), symbol);
            }
            for y in 0..height {
                for x in 0..width {
                    if x < left || x > right || y < title_y || y > bottom {
                        assert_eq!(buffer[(x, y)], original_buffer[(x, y)]);
                    }
                }
            }
            for event in [
                mouse(geometry.terminal.x, geometry.terminal.y, MouseButton::Left),
                Event::Paste("must-not-reach-pty".into()),
            ] {
                assert!(matches!(
                    handle_event(&mut model, event, &geometry),
                    Action::None
                ));
                assert!(model.modal_open());
            }
            assert!(matches!(
                handle_event(
                    &mut model,
                    Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
                    &geometry
                ),
                Action::None
            ));
            assert!(!model.modal_open());
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
                &geometry,
            );
            assert!(matches!(
                handle_event(
                    &mut model,
                    Event::Key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE)),
                    &geometry
                ),
                Action::None
            ));
            assert!(
                !model.prefix && model.add_host.is_some(),
                "a replaces command popup with add dialog"
            );
        }
    }

    fn mouse(x: u16, y: u16, button: MouseButton) -> Event {
        Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(button),
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        })
    }

    #[test]
    fn shell_keys_are_not_navigation() {
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let geometry = Geometry {
            terminal: Rect::new(30, 1, 80, 24),
            ..Geometry::default()
        };
        for key in [
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE),
            KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
            KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE),
            KeyEvent::new(KeyCode::F(10), KeyModifiers::NONE),
            KeyEvent::new_with_kind(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                KeyEventKind::Release,
            ),
        ] {
            assert!(
                matches!(handle_event(&mut model,Event::Key(key),&geometry),Action::SendKey(k) if k == key)
            );
        }
    }

    #[test]
    fn invisible_terminal_rejects_input_and_keeps_prefix_navigation() {
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let geometry = Geometry::default();
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
                &geometry
            ),
            Action::None
        ));
        assert!(matches!(
            handle_event(&mut model, Event::Paste("secret".into()), &geometry),
            Action::None
        ));
        assert!(matches!(
            handle_event(&mut model, mouse(5, 5, MouseButton::Left), &geometry),
            Action::None
        ));
        handle_event(
            &mut model,
            Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
            &geometry,
        );
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
                &geometry
            ),
            Action::Quit
        ));
    }

    #[test]
    fn hovering_sidebar_does_not_steal_terminal_focus() {
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let geometry = Geometry {
            hosts: Rect::new(0, 1, 30, 12),
            ..Geometry::default()
        };
        let event = Event::Mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 5,
            row: 5,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            handle_event(&mut model, event, &geometry),
            Action::None
        ));
        assert_eq!(model.focus, Focus::Terminal);
    }

    #[test]
    fn first_session_navigation_selects_first_item() {
        let mut model = Model {
            focus: Focus::Sessions,
            ..Model::default()
        };
        model.sessions = (0..3)
            .map(|index| SessionItem {
                name: format!("s{index}"),
                title: String::new(),
                agent: "shell".into(),
                project: String::new(),
                state: SessionState::Idle,
                self_session: false,
            })
            .collect();
        assert!(matches!(
            move_selection(&mut model, 1),
            Action::SelectSession(0)
        ));
        assert!(matches!(
            move_selection(&mut model, 1),
            Action::SelectSession(1)
        ));
    }

    #[test]
    fn ui_consumed_presses_do_not_leak_kitty_releases() {
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let geometry = Geometry {
            terminal: Rect::new(30, 1, 80, 24),
            ..Geometry::default()
        };
        let press = |code, mods| Event::Key(KeyEvent::new(code, mods));
        let release =
            |code, mods| Event::Key(KeyEvent::new_with_kind(code, mods, KeyEventKind::Release));
        handle_event(
            &mut model,
            press(KeyCode::Char('b'), KeyModifiers::CONTROL),
            &geometry,
        );
        assert!(matches!(
            handle_event(
                &mut model,
                release(KeyCode::Char('b'), KeyModifiers::CONTROL),
                &geometry
            ),
            Action::None
        ));
        assert!(matches!(
            handle_event(
                &mut model,
                press(KeyCode::Char('t'), KeyModifiers::NONE),
                &geometry
            ),
            Action::None
        ));
        assert!(matches!(
            handle_event(
                &mut model,
                release(KeyCode::Char('t'), KeyModifiers::NONE),
                &geometry
            ),
            Action::None
        ));
        assert!(matches!(
            handle_event(
                &mut model,
                release(KeyCode::Char('z'), KeyModifiers::NONE),
                &geometry
            ),
            Action::None
        ));
        model.focus = Focus::Hosts;
        for value in 0x4e00..0x4e80 {
            handle_event(
                &mut model,
                press(
                    KeyCode::Char(char::from_u32(value).unwrap()),
                    KeyModifiers::NONE,
                ),
                &geometry,
            );
        }
        assert_eq!(model.suppressed_releases.len(), 64);
        model.focus = Focus::Terminal;
        assert!(matches!(
            handle_event(
                &mut model,
                press(KeyCode::Char('t'), KeyModifiers::NONE),
                &geometry
            ),
            Action::SendKey(_)
        ));
        assert!(matches!(
            handle_event(
                &mut model,
                release(KeyCode::Char('t'), KeyModifiers::NONE),
                &geometry
            ),
            Action::SendKey(_)
        ));
        handle_event(
            &mut model,
            press(KeyCode::Char('c'), KeyModifiers::NONE),
            &geometry,
        );
        model.reset_terminal_input();
        assert!(
            model.forwarded_keys.is_empty()
                && model.suppressed_releases.is_empty()
                && !model.prefix
        );
        assert!(matches!(
            handle_event(
                &mut model,
                release(KeyCode::Char('c'), KeyModifiers::NONE),
                &geometry
            ),
            Action::None
        ));
    }

    #[test]
    fn forwarded_release_survives_prefix_without_consuming_it() {
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let geometry = Geometry {
            terminal: Rect::new(30, 1, 80, 24),
            ..Geometry::default()
        };
        let a = KeyCode::Char('a');
        let b = KeyCode::Char('b');
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(a, KeyModifiers::NONE)),
                &geometry
            ),
            Action::SendKey(_)
        ));
        handle_event(
            &mut model,
            Event::Key(KeyEvent::new(b, KeyModifiers::CONTROL)),
            &geometry,
        );
        let release = KeyEvent::new_with_kind(a, KeyModifiers::NONE, KeyEventKind::Release);
        assert!(
            matches!(handle_event(&mut model,Event::Key(release),&geometry),Action::SendKey(key) if key==release)
        );
        assert!(model.prefix);
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new_with_kind(
                    b,
                    KeyModifiers::CONTROL,
                    KeyEventKind::Release
                )),
                &geometry
            ),
            Action::None
        ));
        assert!(model.prefix);
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE)),
                &geometry
            ),
            Action::None
        ));
    }

    #[test]
    fn forwarded_release_survives_modal_and_hidden_terminal() {
        let mut model = Model {
            focus: Focus::Terminal,
            ..Model::default()
        };
        let visible = Geometry {
            terminal: Rect::new(30, 1, 80, 24),
            ..Geometry::default()
        };
        let a = KeyCode::Char('a');
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(a, KeyModifiers::NONE)),
                &visible
            ),
            Action::SendKey(_)
        ));
        model.add_host = Some(AddHost::default());
        model.focus = Focus::Hosts;
        let release = KeyEvent::new_with_kind(a, KeyModifiers::NONE, KeyEventKind::Release);
        assert!(
            matches!(handle_event(&mut model,Event::Key(release),&Geometry::default()),Action::SendKey(key) if key==release)
        );
        assert!(model.add_host.as_ref().unwrap().name.is_empty());
        model.add_host = None;
        model.focus = Focus::Terminal;
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE)),
                &Geometry::default()
            ),
            Action::None
        ));
    }

    #[test]
    fn command_popup_consumes_non_key_events_and_closes_after_a_command_or_reset() {
        let geometry = Geometry {
            terminal: Rect::new(30, 1, 80, 24),
            ..Geometry::default()
        };
        for event in [
            Event::FocusLost,
            Event::Resize(100, 30),
            mouse(1, 1, MouseButton::Left),
        ] {
            let mut model = Model {
                focus: Focus::Terminal,
                ..Model::default()
            };
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL)),
                &geometry,
            );
            assert!(matches!(
                handle_event(&mut model, event, &geometry),
                Action::None
            ));
            assert!(model.modal_open());
            assert!(matches!(
                handle_event(
                    &mut model,
                    Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
                    &geometry
                ),
                Action::Quit
            ));
            assert!(!model.modal_open());
        }
        let mut model = Model {
            focus: Focus::Terminal,
            prefix: true,
            add_host: Some(AddHost::default()),
            ..Model::default()
        };
        handle_event(
            &mut model,
            Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
            &geometry,
        );
        assert!(!model.prefix);
        assert_eq!(model.add_host.as_ref().unwrap().name, "q");
        model.prefix = true;
        model.clear_prefix();
        assert!(!model.prefix);
    }

    #[test]
    fn scroll_offset_hit_and_menu_clamp() {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut model = Model {
            hosts: (0..20)
                .map(|n| host(&format!("主机{n}"), HostState::Direct))
                .collect(),
            selected_host: 18,
            ..Model::default()
        };
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        let expected = model.hosts_offset;
        assert!(expected > 0);
        assert!(
            matches!(handle_event(&mut model,mouse(geometry.hosts.x,geometry.hosts.y,MouseButton::Left),&geometry),Action::SelectHost(n) if n==expected)
        );
        assert!(
            matches!(handle_event(&mut model,mouse(geometry.hosts.x+1,geometry.hosts.y+1,MouseButton::Left),&geometry),Action::SelectHost(n) if n==expected)
        );
        assert!(
            matches!(handle_event(&mut model,mouse(geometry.hosts.x+1,geometry.hosts.y+2,MouseButton::Right),&geometry),Action::SelectHost(n) if n==expected+1)
        );
        assert!(matches!(model.menu.as_ref().unwrap().kind, MenuKind::Host));
        model.menu = Some(Menu {
            kind: MenuKind::Host,
            x: 99,
            y: 23,
            selected: 0,
        });
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        assert!(geometry.menu.right() <= 100 && geometry.menu.bottom() <= 24);
        assert!(matches!(
            handle_event(
                &mut model,
                Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
                &geometry
            ),
            Action::None
        ));
        assert!(!model.modal_open());
    }

    #[test]
    fn two_line_session_hits_follow_wheel_offset_and_ignore_partial_bottom_slot() {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut model = Model {
            hosts: vec![host("local", HostState::Local)],
            sessions: (0..20)
                .map(|n| SessionItem {
                    name: format!("session-{n}"),
                    title: format!("title-{n}"),
                    agent: "shell".into(),
                    project: "project".into(),
                    state: SessionState::Idle,
                    self_session: false,
                })
                .collect(),
            selected_session: Some(18),
            ..Default::default()
        };
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        let wheel = Event::Mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: geometry.sessions.x + 1,
            row: geometry.sessions.y,
            modifiers: KeyModifiers::NONE,
        });
        assert!(matches!(
            handle_event(&mut model, wheel, &geometry),
            Action::SelectSession(19)
        ));
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        let first = model.sessions_offset;
        assert!(first > 0);
        assert!(
            buffer_line(terminal.backend().buffer(), geometry.sessions.y)
                .contains(&format!("title-{first}"))
        );
        assert!(matches!(handle_event(&mut model,
            mouse(geometry.sessions.x+1, geometry.sessions.y+3, MouseButton::Right), &geometry),
            Action::SelectSession(index) if index==first+1));
        assert_eq!(model.selected_session, Some(first + 1));
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        let menu = inner(geometry.menu);
        assert!(matches!(
            handle_event(
                &mut model,
                mouse(menu.x, menu.y, MouseButton::Left),
                &geometry
            ),
            Action::CloseSession
        ));

        // With the whole frame available, 32 rows allow seven complete host
        // items (14 content rows); 31 rows leave an odd spare physical row.
        // Only that spare row is inert. Neither case may let the footer select
        // a host or session beneath it.
        for (window_height, content_height, spare) in [(31, 13, true), (32, 14, false)] {
            let mut terminal = Terminal::new(TestBackend::new(100, window_height)).unwrap();
            model.hosts = (0..20).map(|_| host("remote", HostState::Direct)).collect();
            terminal
                .draw(|frame| geometry = draw(frame, &mut model, None))
                .unwrap();
            assert_eq!(geometry.hosts.height, content_height);
            let last_visible = model.hosts_offset + usize::from(content_height / ITEM_HEIGHT) - 1;
            let action = handle_event(
                &mut model,
                mouse(
                    geometry.hosts.x + 1,
                    geometry.hosts.bottom() - 1,
                    MouseButton::Left,
                ),
                &geometry,
            );
            if spare {
                assert!(matches!(action, Action::None));
            } else {
                assert!(matches!(action, Action::SelectHost(index) if index == last_visible));
            }
            let selected = (model.selected_host, model.selected_session);
            let footer = geometry.footer_area();
            assert!(matches!(
                handle_event(
                    &mut model,
                    mouse(footer.x + 1, footer.y, MouseButton::Left),
                    &geometry
                ),
                Action::None
            ));
            assert_eq!((model.selected_host, model.selected_session), selected);
        }
    }

    #[test]
    fn small_windows_and_wide_terminal_cells() {
        for (width, height) in [
            (1, 1),
            (2, 2),
            (4, 2),
            (20, 5),
            (39, 9),
            (48, 12),
            (120, 32),
        ] {
            let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
            let mut model = Model {
                hosts: vec![host("我的电脑", HostState::Local)],
                ..Model::default()
            };
            terminal
                .draw(|frame| {
                    draw(frame, &mut model, None);
                })
                .unwrap();
            model.prefix = true;
            terminal
                .draw(|frame| {
                    draw(frame, &mut model, None);
                })
                .unwrap();
        }
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut model = Model::default();
        let mut vt = SessionVt::new(65, 19, 0).unwrap();
        vt.write("中文测试\x1b[31mRED".as_bytes());
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, Some(&mut vt)))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(
            buffer[(geometry.terminal.x, geometry.terminal.y)].symbol(),
            "中"
        );
        assert_eq!(
            buffer[(geometry.terminal.x + 2, geometry.terminal.y)].symbol(),
            "文"
        );
        assert_eq!(
            buffer[(geometry.terminal.x + 8, geometry.terminal.y)].symbol(),
            "R"
        );
        assert_eq!(
            buffer[(geometry.terminal.x + 8, geometry.terminal.y)].fg,
            Color::Indexed(1)
        );
    }

    #[test]
    fn external_theme_changes_panel_colors_and_preserves_selection() {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut model = Model::default();
        model.hosts.push(host("我的电脑", HostState::Local));
        model.hosts.push(host("远程主机", HostState::Direct));
        model.selected_host = 1;
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        let selected_position = (geometry.hosts.x, geometry.hosts.y + 2);
        let dark = terminal.backend().buffer()[selected_position].bg;
        model.theme = Theme::Light;
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        assert_ne!(dark, terminal.backend().buffer()[selected_position].bg);
        assert_eq!(model.selected_host, 1);
        assert_eq!(model.theme, Theme::Light);
    }

    #[test]
    fn token_is_masked_and_paste_is_literal() {
        let secret = "secret-token-123";
        let mut model = Model {
            add_host: Some(AddHost {
                name: "测试".into(),
                token: secret.into(),
                field: 1,
                error: String::new(),
            }),
            ..Model::default()
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        let content = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect::<String>();
        assert!(!content.contains(secret));
        handle_event(&mut model, Event::Paste("\nfoo\r".into()), &geometry);
        assert_eq!(
            model.add_host.as_ref().unwrap().token,
            format!("{secret}foo")
        );
        assert!(
            matches!(handle_event(&mut model,Event::Key(KeyEvent::new(KeyCode::Enter,KeyModifiers::NONE)),&geometry),Action::AddHost{name,token} if name=="测试" && token==format!("{secret}foo"))
        );
    }
}
