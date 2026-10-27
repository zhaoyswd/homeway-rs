//! TUI 的纯显示与交互状态；网络与 PTY 生命周期由主循环管理。

use std::borrow::Cow;

use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use homeway_core::term::vt::{self, SessionVt};
use ratatui::{
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum Theme {
    #[default]
    Dark,
    Light,
}

impl Theme {
    pub(super) fn toggle(&mut self) {
        *self = match self {
            Self::Dark => Self::Light,
            Self::Light => Self::Dark,
        };
    }

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
    pub name: String,
    pub detail: String,
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
}

#[derive(Default)]
pub(super) struct Model {
    pub hosts: Vec<HostItem>,
    pub sessions: Vec<SessionItem>,
    pub selected_host: usize,
    pub selected_session: Option<usize>,
    pub theme: Theme,
    pub notice: String,
    pub focus: Focus,
    pub terminal_title: String,
    prefix: bool,
    hosts_offset: usize,
    sessions_offset: usize,
    suppressed_releases: Vec<KeyCode>,
    forwarded_keys: Vec<KeyCode>,
    menu: Option<Menu>,
    add_host: Option<AddHost>,
}

impl Model {
    pub(super) fn reset_sessions(&mut self) {
        self.sessions.clear();
        self.selected_session = None;
        self.sessions_offset = 0;
    }

    pub(super) fn modal_open(&self) -> bool {
        self.menu.is_some() || self.add_host.is_some()
    }

    pub(super) fn clear_prefix(&mut self) {
        self.prefix = false;
    }

    pub(super) fn reset_terminal_input(&mut self) {
        self.clear_prefix();
        self.forwarded_keys.clear();
        self.suppressed_releases.clear();
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
    SendKey(KeyEvent),
    Paste(String),
    TerminalMouse(MouseEvent),
    ThemeChanged,
}

#[derive(Default, Debug, Clone, Copy)]
pub(super) struct Geometry {
    pub terminal: Rect,
    hosts: Rect,
    sessions: Rect,
    host_add: Rect,
    session_add: Rect,
    create: Rect,
    theme: Rect,
    menu: Rect,
    dialog: Rect,
    name_input: Rect,
    token_input: Rect,
    submit: Rect,
}

#[derive(Debug, Clone, Copy)]
enum MenuKind {
    Local,
    Host,
    Session,
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
    vt: Option<&mut SessionVt>,
) -> Geometry {
    let area = frame.area();
    let p = model.theme.palette();
    frame.render_widget(
        Block::default().style(Style::default().fg(p.fg).bg(p.bg)),
        area,
    );
    let mut geometry = Geometry::default();
    if area.width < 48 || area.height < 12 {
        frame.render_widget(
            Paragraph::new("窗口太小，请调整到至少 48 列 × 12 行\nCtrl+B q 退出")
                .alignment(Alignment::Center)
                .style(Style::default().fg(p.muted)),
            area,
        );
        return geometry;
    }
    let sidebar = (area.width / 3)
        .clamp(26, 38)
        .min(area.width.saturating_sub(20));
    let body = Rect::new(area.x, area.y + 1, area.width, area.height - 3);
    let split = body.height / 2;
    let host_panel = Rect::new(body.x, body.y, sidebar, split + 1);
    let session_panel = Rect::new(body.x, body.y + split, sidebar, body.height - split);
    let terminal_panel = Rect::new(
        body.x + sidebar - 1,
        body.y,
        body.width - sidebar + 1,
        body.height,
    );
    geometry.hosts = inner(host_panel);
    geometry.sessions = inner(session_panel);
    geometry.terminal = inner(terminal_panel);
    geometry.host_add = Rect::new(host_panel.right() - 6, host_panel.y, 5, 1);
    geometry.session_add = Rect::new(session_panel.right() - 6, session_panel.y, 5, 1);
    geometry.theme = Rect::new(area.right() - 15, area.y, 14, 1);
    text(
        frame,
        Rect::new(area.x + 1, area.y, 20, 1),
        "homeway",
        Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
    );
    text(
        frame,
        geometry.theme,
        match model.theme {
            Theme::Dark => "[主题：深色]",
            Theme::Light => "[主题：浅色]",
        },
        Style::default().fg(p.muted),
    );
    panel(
        frame,
        host_panel,
        format!(" 主机 {} ", model.hosts.len()),
        model.focus == Focus::Hosts,
        p,
    );
    panel(
        frame,
        session_panel,
        format!(" 会话 {} ", model.sessions.len()),
        model.focus == Focus::Sessions,
        p,
    );
    panel(
        frame,
        terminal_panel,
        format!(
            " {} ",
            if model.terminal_title.is_empty() {
                "终端".to_owned()
            } else {
                clean(&model.terminal_title)
            }
        ),
        model.focus == Focus::Terminal,
        p,
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
    // 三个面板共用一条竖线和一条左侧横线。
    text(
        frame,
        Rect::new(body.x + sidebar - 1, body.y + split, 1, 1),
        "┤",
        Style::default().fg(p.border),
    );
    let host_slots = usize::from(geometry.hosts.height.div_ceil(3));
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
        let y = geometry.hosts.y + slot as u16 * 3;
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
            HostState::Local => ("本地", p.accent),
            HostState::Direct => ("直连", p.green),
            HostState::Relay => ("中继", p.amber),
            HostState::Offline => ("离线", p.muted),
        };
        text(
            frame,
            Rect::new(row.x, y, row.width, 1),
            Line::from(vec![
                Span::styled("● ", Style::default().fg(color)),
                Span::raw(format!("【{label}】{}", clean(&host.name))),
            ]),
            Style::default().fg(p.fg).bg(bg).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        );
        if row.height > 1 {
            let detail = if host.state == HostState::Local {
                format!(
                    "  出口：{}  中继：{}",
                    if host.exit_on { "开" } else { "关" },
                    if host.relay_on { "开" } else { "关" }
                )
            } else {
                format!(
                    "  {}{}",
                    clean(&host.detail),
                    if host.proxy_on { " · 代理开" } else { "" }
                )
            };
            text(
                frame,
                Rect::new(row.x, y + 1, row.width, 1),
                detail,
                Style::default().fg(p.muted).bg(bg),
            );
        }
    }
    let session_slots = usize::from(geometry.sessions.height.div_ceil(3));
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
        let y = geometry.sessions.y + slot as u16 * 3;
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
        frame.render_widget(Block::default().style(Style::default().bg(bg)), row);
        let (label, color) = match session.state {
            SessionState::Working => ("运行中", p.green),
            SessionState::Idle => ("空闲", p.muted),
            SessionState::Blocked => ("待操作", p.amber),
            SessionState::Unknown => ("未知", p.muted),
        };
        let title = if session.title.is_empty() {
            &session.name
        } else {
            &session.title
        };
        text(
            frame,
            Rect::new(row.x, y, row.width, 1),
            Line::from(vec![
                Span::styled("● ", Style::default().fg(color)),
                Span::raw(format!("【{label}】{}", clean(title))),
            ]),
            Style::default().fg(p.fg).bg(bg).add_modifier(if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            }),
        );
        if row.height > 1 {
            text(
                frame,
                Rect::new(row.x, y + 1, row.width, 1),
                format!("  {} {}", clean(&session.agent), clean(&session.project)),
                Style::default().fg(p.muted).bg(bg),
            );
        }
    }
    if model.sessions.is_empty() {
        text(
            frame,
            geometry.sessions,
            "暂无会话 · 点击 + 创建",
            Style::default().fg(p.muted),
        );
    }
    if let Some(vt) = vt {
        draw_terminal(
            frame,
            geometry.terminal,
            vt,
            p,
            model.focus == Focus::Terminal && !model.modal_open(),
        );
    } else if model.selected_session.is_none() {
        let terminal = geometry.terminal;
        let center_y = terminal.y + terminal.height.saturating_sub(5) / 2;
        text(
            frame,
            Rect::new(terminal.x, center_y, terminal.width, 1),
            Line::from("想在这台主机上做些什么？").alignment(Alignment::Center),
            Style::default().fg(p.fg),
        );
        text(
            frame,
            Rect::new(terminal.x, center_y + 2, terminal.width, 1),
            Line::from("创建 shell 会话，开始工作").alignment(Alignment::Center),
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
    let status = if model.prefix {
        "h 主机 · s 会话 · n 新建 · a 添加主机 · m 操作 · t 主题 · q 退出".to_owned()
    } else {
        clean(&model.notice)
    };
    text(
        frame,
        Rect::new(area.x + 1, area.bottom() - 2, area.width - 2, 1),
        status,
        Style::default().fg(p.amber),
    );
    text(
        frame,
        Rect::new(area.x + 1, area.bottom() - 1, area.width - 2, 1),
        "Ctrl+B h/s 焦点 · F2 / 右键 操作 · Ctrl+B q 退出",
        Style::default().fg(p.muted),
    );
    if model.menu.is_some() {
        draw_menu(frame, model, &mut geometry, p);
    }
    if model.add_host.is_some() {
        draw_add_host(frame, model, &mut geometry, p);
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

/// VT格的TAB是文本/复制标记，跳格已体现在绝对列中；Ratatui低层symbol只接受可绘制字符。
/// 仅投影显示：控制符基格画空白、保留其组合附标，正常Unicode不复制也不改写源VT。
fn terminal_symbol(symbol: &str) -> Cow<'_, str> {
    if symbol.is_empty() || symbol.chars().all(char::is_control) {
        return Cow::Borrowed(" ");
    }
    if !symbol.chars().any(char::is_control) {
        return Cow::Borrowed(symbol);
    }
    let mut visible: String = symbol.chars().filter(|c| !c.is_control()).collect();
    if symbol.chars().next().is_some_and(char::is_control) {
        visible.insert(0, ' ');
    }
    Cow::Owned(visible)
}

fn draw_terminal(
    frame: &mut Frame<'_>,
    area: Rect,
    vt: &mut SessionVt,
    p: Palette,
    show_cursor: bool,
) {
    for row in vt.rows() {
        if row.y >= area.height {
            continue;
        }
        for (x, cell) in row.cells.iter().enumerate().take(usize::from(area.width)) {
            if cell.skip || cell.width == 0 {
                continue;
            }
            if cell.width == 2 && x + 1 >= usize::from(area.width) {
                continue;
            }
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
            let y = area.y + row.y;
            if let Some(target) = frame.buffer_mut().cell_mut((x, y)) {
                let symbol = terminal_symbol(&cell.symbol);
                target.set_symbol(symbol.as_ref()).set_style(style);
            }
            if cell.width == 2 {
                if let Some(target) = frame.buffer_mut().cell_mut((x + 1, y)) {
                    target.set_symbol(" ").set_style(style);
                }
            }
        }
    }
    let cursor = vt.cursor();
    if show_cursor && cursor.visible && cursor.x < area.width && cursor.y < area.height {
        frame.set_cursor_position((area.x + cursor.x, area.y + cursor.y));
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
        Focus::Sessions if model.selected_session.is_some() => {
            (MenuKind::Session, geometry.sessions)
        }
        _ => return,
    };
    model.menu = Some(Menu {
        kind,
        x: area.x + 2,
        y: area.y + 1,
        selected: 0,
    });
}

/// 终端内只保留 Ctrl+B 前缀；功能键仍交给被管理的程序。
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
    if !matches!(event, Event::Key(_)) || model.modal_open() {
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
                model.prefix = false;
                return match key.code {
                    KeyCode::Char('q') => Action::Quit,
                    KeyCode::Char('t') => {
                        model.theme.toggle();
                        Action::ThemeChanged
                    }
                    KeyCode::Char('h') => {
                        model.focus = Focus::Hosts;
                        Action::None
                    }
                    KeyCode::Char('s') => {
                        model.focus = Focus::Sessions;
                        Action::None
                    }
                    KeyCode::Enter if model.selected_session.is_some() => {
                        model.focus = Focus::Terminal;
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
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && geometry.terminal.width > 0
                            && geometry.terminal.height > 0 =>
                    {
                        Action::SendKey(key)
                    }
                    _ => Action::None,
                };
            }
            if key.code == KeyCode::Char('b') && key.modifiers.contains(KeyModifiers::CONTROL) {
                model.prefix = true;
                return Action::None;
            }
            if model.focus == Focus::Terminal {
                return if geometry.terminal.width > 0 && geometry.terminal.height > 0 {
                    Action::SendKey(key)
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
                        model.focus = Focus::Terminal;
                        Action::SelectSession(index)
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
            let index = model
                .selected_session
                .map_or(0, |selected| selected.saturating_add_signed(delta))
                .min(model.sessions.len() - 1);
            model.selected_session = Some(index);
            Action::SelectSession(index)
        }
        _ => Action::None,
    }
}

fn handle_mouse(model: &mut Model, mouse: MouseEvent, geometry: &Geometry) -> Action {
    let (x, y) = (mouse.column, mouse.row);
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        if contains(geometry.theme, x, y) {
            model.theme.toggle();
            return Action::ThemeChanged;
        }
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
            let index = model.hosts_offset + usize::from((y - geometry.hosts.y) / 3);
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
            let index = model.sessions_offset + usize::from((y - geometry.sessions.y) / 3);
            if index < model.sessions.len() {
                model.selected_session = Some(index);
                if mouse.kind == MouseEventKind::Down(MouseButton::Right) {
                    model.menu = Some(Menu {
                        kind: MenuKind::Session,
                        x,
                        y,
                        selected: 0,
                    });
                } else {
                    model.focus = Focus::Terminal;
                }
                return Action::SelectSession(index);
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
    use ratatui::{backend::TestBackend, Terminal};

    #[test]
    fn terminal_tabs_keep_grid_columns_and_style_across_real_draw_diffs() {
        let mut vt = SessionVt::new(20, 4, 10).unwrap();
        vt.write(b"\x1b[44m\x1b[2J\x1b[H\x1b[31mA\tB\x1b[0m");
        assert_eq!(vt.rows()[0].cells[1].symbol, "\t", "VT保留TAB复制语义");
        let mut terminal = Terminal::new(TestBackend::new(20, 4)).unwrap();
        terminal
            .draw(|frame| draw_terminal(frame, frame.area(), &mut vt, Theme::Dark.palette(), false))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "A");
        assert_eq!(buffer[(1, 0)].symbol(), " ");
        assert_eq!(buffer[(0, 0)].fg, Color::Indexed(1));
        assert_eq!(buffer[(1, 0)].fg, Theme::Dark.palette().fg, "TAB仅写格符号，不继承当前SGR前景");
        assert_eq!(buffer[(1, 0)].bg, Color::Indexed(4), "原网格背景色必须保留");
        assert_eq!(
            buffer[(8, 0)].symbol(),
            "B",
            "TAB已由VT网格推进到第9列，不能二次展开"
        );
        vt.write(b"\x1b[1;1H\x1b[32mC\tD\x1b[0m");
        terminal
            .draw(|frame| {
                draw_terminal(frame, frame.area(), &mut vt, Theme::Light.palette(), false)
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "C");
        assert_eq!(buffer[(1, 0)].bg, Color::Indexed(4), "未覆盖的原TAB格背景保留");
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

    fn host(name: &str, state: HostState) -> HostItem {
        HostItem {
            name: name.into(),
            detail: "127.0.0.1  2ms".into(),
            state,
            exit_on: false,
            relay_on: false,
            proxy_on: false,
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
            Action::ThemeChanged
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
            Action::ThemeChanged
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
    fn prefix_is_cancelled_by_non_key_events_and_modal_or_selection_reset() {
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
            handle_event(&mut model, event, &geometry);
            assert!(matches!(
                handle_event(
                    &mut model,
                    Event::Key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE)),
                    &geometry
                ),
                Action::SendKey(_)
            ));
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
    fn small_windows_and_wide_terminal_cells() {
        for (width, height) in [(1, 1), (20, 5), (48, 12), (120, 32)] {
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
    fn theme_changes_panel_colors_and_preserves_selection() {
        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut model = Model::default();
        model.hosts.push(host("我的电脑", HostState::Local));
        model.hosts.push(host("远程主机", HostState::Direct));
        model.selected_host = 1;
        let mut geometry = Geometry::default();
        terminal
            .draw(|frame| geometry = draw(frame, &mut model, None))
            .unwrap();
        let selected_position = (geometry.hosts.x, geometry.hosts.y + 3);
        let dark = terminal.backend().buffer()[selected_position].bg;
        assert!(matches!(
            handle_event(
                &mut model,
                mouse(geometry.theme.x, geometry.theme.y, MouseButton::Left),
                &geometry
            ),
            Action::ThemeChanged
        ));
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
