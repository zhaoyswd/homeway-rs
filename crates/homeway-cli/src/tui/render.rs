//! 面板渲染（r5 UI 批：向 herdr 视觉对齐——`herdr src/client/shell/sidebar.rs`
//! 同款设计语言）：**无边框区块** + 小标题（overlay0+BOLD）+ `─` 细分隔线 +
//! 两行条目（主名 subtext0-BOLD / 次行 overlay0）+ **整行底色选中**（selection_bg，
//! 非反白）+ 终端 pane 标题栏（accent 聚焦）带 `✕` 关闭、区块标题右侧 ` +`。
//!
//! 终端视图区域由 [`super::termview`] 负责；此处铺区域、标题与空态提示。
//!
//! 消费方 = app 主循环（runtime，任务组 4b）——期间豁免 dead_code。

#![allow(dead_code)]

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::{List, ListItem, ListState, Paragraph, Widget};
use ratatui::Frame;

use super::app::{AppModel, Focus};
use super::hosts::HostView;
use super::local::LOCAL_HOST_ID;
use super::sessions::AgentBadge;
use super::theme;

/// 输入浮层（` +` 动作打开）：label = 提示语（含动作名），buffer = 已输入，
/// error = 上次提交失败原因（有的话红字显示在输入行右侧）。
#[derive(Debug, Clone, Default)]
pub(crate) struct Overlay {
    pub label: String,
    pub buffer: String,
    pub error: Option<String>,
}

/// 左栏宽度占比（主机/会话列表）。
const LEFT_PCT: u16 = 35;

/// 四区布局（herdr 侧栏同款比例语义）：左上主机区 / 左下会话区 / 右侧终端 pane /
/// 底部状态行。区块内部再分层（标题行/分隔行/条目体）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct Regions {
    pub hosts: Rect,
    pub sessions: Rect,
    pub term: Rect,
    pub status: Rect,
}

pub(crate) fn layout(area: Rect) -> Regions {
    let v = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let h = Layout::horizontal([Constraint::Percentage(LEFT_PCT), Constraint::Percentage(100 - LEFT_PCT)])
        .split(v[0]);
    let l = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(h[0]);
    Regions { hosts: l[0], sessions: l[1], term: h[1], status: v[1] }
}

/// 鼠标命中区（纯布局函数——点击测试与绘制同源）。
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct HitZones {
    pub host_plus: Rect,
    pub sess_plus: Rect,
    pub term_close: Rect,
}

pub(crate) fn hit_zones(area: Rect) -> HitZones {
    let r = layout(area);
    HitZones {
        host_plus: Rect::new(r.hosts.right().saturating_sub(3), r.hosts.y, 3, 1),
        sess_plus: Rect::new(r.sessions.right().saturating_sub(3), r.sessions.y, 3, 1),
        term_close: Rect::new(r.term.right().saturating_sub(2), r.term.y, 2, 1),
    }
}

/// 从全屏区域一路派生终端内容区（layout → term pane → 标题栏之下）。
/// `term_area` 存全屏区域（click_select 与 hit_zones 同源），RESIZE 位点用本函数。
pub(crate) fn term_content_of(full_area: Rect) -> Rect {
    term_content(layout(full_area).term)
}

/// 终端 pane 的内容区 = 标题栏（首行）之下。
pub(crate) fn term_content(term_a: Rect) -> Rect {
    Rect {
        y: term_a.y + 1,
        height: term_a.height.saturating_sub(1),
        ..term_a
    }
}

/// 徽章显示词面（working/blocked/idle/unknown/无）。
pub(crate) fn badge_text(b: AgentBadge) -> &'static str {
    match b {
        AgentBadge::NoAgent => "无",
        AgentBadge::Working => "working",
        AgentBadge::Blocked => "blocked",
        AgentBadge::Idle => "idle",
        AgentBadge::Unknown => "unknown",
    }
}

/// 徽章状态色（herdr 语义色：working=yellow / blocked=red / idle=green）。
pub(crate) fn badge_color(b: AgentBadge) -> ratatui::style::Color {
    match b {
        AgentBadge::NoAgent => theme::OVERLAY0,
        AgentBadge::Working => theme::YELLOW,
        AgentBadge::Blocked => theme::RED,
        AgentBadge::Idle => theme::GREEN,
        AgentBadge::Unknown => theme::OVERLAY1,
    }
}

/// 主机条目圆点（四态）：直连绿 / 中继黄 / 连接中闪烁（blink_on 相位）/ 未连接灰。
pub(crate) fn host_dot(h: &HostView, blink_on: bool) -> Span<'static> {
    let fg = if h.state == "connecting" {
        if blink_on { theme::YELLOW } else { theme::OVERLAY0 }
    } else if h.state == "ready" {
        if h.via.as_deref() == Some("relay") { theme::YELLOW } else { theme::GREEN }
    } else {
        theme::OVERLAY0
    };
    Span::styled("●", Style::new().fg(fg))
}

/// 主机条目行：` ● 名称`（herdr spaces 行1 = icon + 主名）。
pub(crate) fn host_line(h: &HostView, blink_on: bool) -> Line<'static> {
    Line::from(vec![
        host_dot(h, blink_on),
        Span::raw(" "),
        Span::styled(h.name.clone(), theme::entry_name(false)),
    ])
}

/// 主机条目第二行（tier 口径副标题，herdr 行2 = 缩进次级信息）：已连接 =
/// `延迟 42ms(3s) · ↑tx ↓rx`；连接中 = 阶段文案；未连接 = 主机 id；本机 = `local`。
pub(crate) fn host_detail(h: &HostView, rate: Option<(f64, f64)>, now_ms: i64) -> String {
    if h.state == "connecting" {
        return match h.reason.as_deref() {
            Some(r) if !r.is_empty() => format!("连接中（{r}）"),
            _ => "连接中…".to_owned(),
        };
    }
    if h.state != "ready" {
        return h.id.clone();
    }
    if h.id == LOCAL_HOST_ID {
        return "local".to_owned();
    }
    let rtt = match (h.rtt_ms, h.link_at) {
        (Some(r), Some(at)) => {
            let age = (now_ms - at).max(0) / 1000;
            if age > 120 {
                format!("{r}ms({age}s 陈旧)")
            } else {
                format!("{r}ms({age}s)")
            }
        }
        _ => "—".to_owned(),
    };
    let flow = match rate {
        Some((rx, tx)) => format!("↑{}/s ↓{}/s", fmt_bytes(tx), fmt_bytes(rx)),
        None => "流量 —".to_owned(),
    };
    format!("{rtt} · {flow}")
}

/// 会话条目行：` ● 会话名`（● = 徽章状态色；herdr agents 行1 = icon + 主名）。
pub(crate) fn session_line(name: &str, badge: AgentBadge) -> Line<'static> {
    Line::from(vec![
        Span::styled("●", Style::new().fg(badge_color(badge))),
        Span::raw(" "),
        Span::styled(name.to_owned(), theme::entry_name(false)),
    ])
}

/// 会话条目第二行：`徽章 · 标题`（overlay0 次级）。
pub(crate) fn session_detail(badge: &str, title: Option<&str>) -> Line<'static> {
    Line::from(Span::styled(
        format!("{badge} · {}", title.unwrap_or("-")),
        theme::muted(),
    ))
}

/// 状态行基底（连接状态信息在主机条目第二行；相位/回看/焦点尾巴由 runtime 追加）。
pub(crate) fn status_line(app: &AppModel) -> String {
    if app.selected_host_view().is_none() {
        return "未选中主机（↑↓ 选择）".to_owned();
    }
    String::new()
}

/// 字节量人类可读（B/KB/MB/GB，二进制口径）。
pub(crate) fn fmt_bytes(v: f64) -> String {
    let (u, base): (&str, f64) = if v < 1024.0 { ("B", 1.0) }
        else if v < 1024.0 * 1024.0 { ("KB", 1024.0) }
        else if v < 1024.0 * 1024.0 * 1024.0 { ("MB", 1024.0 * 1024.0) }
        else { ("GB", 1024.0 * 1024.0 * 1024.0) };
    format!("{:.0}{}", v / base, u)
}

/// 条目体公共参数：区头（1 行）+ 区尾分隔线（1 行）之外的范围。
pub(crate) fn section_body(section: Rect) -> Rect {
    Rect {
        y: section.y + 1,
        height: section.height.saturating_sub(2),
        ..section
    }
}

/// 区块小标题行：` <名>`（左，overlay0+BOLD）+ ` +`（右，overlay0——命中区见
/// [`hit_zones`]）。
fn render_section_header(f: &mut Frame, section: Rect, title: &str) {
    let buf = f.buffer_mut();
    Line::from(Span::styled(format!(" {title}"), theme::section_header()))
        .render(section, buf);
    let plus_x = section.right().saturating_sub(2);
    if let Some(rc) = buf.cell_mut((plus_x, section.y)) {
        rc.set_symbol("+");
        rc.set_style(theme::muted());
    }
}

/// 区块底部 `─` 分隔线（surface_dim——herdr 区块分隔同款）。
fn render_section_divider(f: &mut Frame, section: Rect) {
    let y = section.bottom().saturating_sub(1);
    let buf = f.buffer_mut();
    for x in section.left()..section.right() {
        if let Some(rc) = buf.cell_mut((x, y)) {
            rc.set_symbol("─");
            rc.set_style(theme::divider());
        }
    }
}

/// 面板整体绘制（UI 主循环每拍调用）。返回鼠标命中区。
pub(crate) fn draw(
    f: &mut Frame,
    app: &mut AppModel,
    term: Option<&mut super::termview::TermView>,
    status: &str,
    rates: &std::collections::HashMap<String, (f64, f64)>,
    now_ms: i64,
    overlay: Option<&Overlay>,
) -> HitZones {
    let area = f.area();
    let regions = layout(area);
    let blink_on = (now_ms / 500) % 2 == 0;

    // ── 左上：主机区（spaces 风）─────────────────────────────────────────
    render_section_header(f, regions.hosts, "主机");
    render_section_divider(f, regions.hosts);
    let hosts = app.display_hosts();
    let host_items: Vec<ListItem> = hosts
        .iter()
        .map(|h| {
            let rate = rates.get(&h.id).copied();
            ListItem::new(vec![
                host_line(h, blink_on),
                Line::from(Span::styled(
                    host_detail(h, rate, now_ms),
                    theme::muted(),
                )),
                Line::from(""),
            ])
        })
        .collect();
    let mut host_ls = ListState::default();
    if !hosts.is_empty() {
        host_ls.select(Some(app.host_cursor.min(hosts.len() - 1)));
    }
    let host_body = section_body(regions.hosts);
    let host_list = List::new(host_items)
        .highlight_style(theme::selection());
    if !host_body.is_empty() {
        f.render_stateful_widget(host_list, host_body, &mut host_ls);
    }
    if hosts.is_empty() && !host_body.is_empty() {
        Paragraph::new(" （无主机）").style(theme::placeholder()).render(host_body, f.buffer_mut());
    }

    // ── 左下：会话区（agents 风）─────────────────────────────────────────
    render_section_header(f, regions.sessions, "会话");
    let sessions = app.sessions.sessions().to_vec();
    let sess_items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            let b = super::sessions::badge(&s.agent, &s.state_v2);
            let bt = badge_text(b);
            ListItem::new(vec![
                session_line(&s.name, b),
                session_detail(bt, s.title.as_deref()),
                Line::from(""),
            ])
        })
        .collect();
    let mut sess_ls = ListState::default();
    if !sessions.is_empty() {
        sess_ls.select(Some(app.session_cursor.min(sessions.len() - 1)));
    }
    let sess_body = section_body(regions.sessions);
    let sess_list = List::new(sess_items).highlight_style(theme::selection());
    if !sess_body.is_empty() {
        f.render_stateful_widget(sess_list, sess_body, &mut sess_ls);
    }
    if sessions.is_empty() && !sess_body.is_empty() {
        Paragraph::new(" （无会话）").style(theme::placeholder()).render(sess_body, f.buffer_mut());
    }

    // ── 右侧：终端 pane（标题栏 + 内容）或引导提示 ─────────────────────
    let term_a = regions.term;
    let hit = hit_zones(area);
    match term {
        Some(tv) => {
            // 标题栏：` 会话名 · 标题`（聚焦 = accent+BOLD，herdr pane title 同款）
            // + 右侧 ` ✕`（关闭 = 断开观察当前会话）。
            let sel = app.sessions.selected().unwrap_or("");
            let view = app
                .sessions
                .sessions()
                .iter()
                .find(|s| s.name == sel);
            let title_tail = view
                .and_then(|s| s.title.as_deref())
                .filter(|t| !t.is_empty())
                .map(|t| format!(" · {t}"))
                .unwrap_or_default();
            let title = Line::from(vec![
                Span::styled(
                    format!(" {sel}{title_tail}"),
                    theme::pane_title(app.focus == Focus::Terminal),
                ),
            ]);
            let bar = Rect { height: 1, ..term_a };
            title.render(bar, f.buffer_mut());
            if let Some(rc) = f.buffer_mut().cell_mut((hit.term_close.x, term_a.y)) {
                rc.set_symbol("✕");
                rc.set_style(theme::muted());
            }
            // 内容区 = 标题栏之下。
            let content = term_content(term_a);
            if content.height > 0 {
                tv.render(f, content);
                // 终端焦点的宿主光标 = 会话 VT 光标（会话声明可见位且贴底视口时；
                // 回看中的光标位置无语义，保持隐藏）。
                let cur = tv.leg.cursor();
                if app.focus == Focus::Terminal && cur.visible && tv.leg.display_offset() == 0 {
                    if let Some(p) = super::termview::cursor_in_pane(cur, content) {
                        f.set_cursor_position(p);
                    }
                }
            }
        }
        None => {
            Paragraph::new(" ↑↓ 选择主机 / 会话；Enter 进入终端焦点（F12 返回）；q 退出。\n 选中会话后，这里显示其终端画面。")
                .style(theme::placeholder())
                .render(term_a, f.buffer_mut());
        }
    }

    // ── 底部：状态行 ────────────────────────────────────────────────────
    let focus_tag = match app.focus {
        Focus::Panel => "",
        Focus::Terminal => " [终端焦点 F12 返回]",
    };
    Line::from(Span::styled(
        format!("{status}{focus_tag}"),
        theme::status_text(),
    ))
    .render(regions.status, f.buffer_mut());

    // ── 输入浮层（状态行上方一条，selection_bg 底）────────────────────────
    if let Some(ov) = overlay {
        let bar = Rect {
            y: regions.status.y.saturating_sub(1),
            height: 1,
            ..regions.status
        };
        if bar.height == 1 && bar.width > 4 {
            let mut spans = vec![Span::styled(
                format!(" {} {}▏", ov.label, ov.buffer),
                Style::new().fg(theme::TEXT).bg(theme::SELECTION_BG),
            )];
            if let Some(err) = &ov.error {
                spans.push(Span::styled(
                    format!("  {err}"),
                    Style::new().fg(theme::RED).bg(theme::SELECTION_BG),
                ));
            }
            Line::from(spans).render(bar, f.buffer_mut());
        }
    }

    hit
}

// ---------------------------------------------------------------------------
// 测试（TestBackend 快照）
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use homeway_core::daemon::proto::HostState;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    fn host(id: &str, name: &str, state: &str) -> HostState {
        HostState {
            id: id.into(),
            name: Some(name.into()),
            state: state.into(),
            reason: None,
            link: None,
            stats: None,
            added_at: None,
        }
    }

    fn buffer_text(t: &mut Terminal<TestBackend>) -> Vec<String> {
        let buf = t.backend().buffer();
        let w = buf.area.width as usize;
        buf.content
            .iter()
            .map(|c| c.symbol().to_owned())
            .collect::<Vec<_>>()
            .chunks(w)
            .map(|ch| ch.concat().trim_end().to_owned())
            .collect()
    }

    #[test]
    fn draw_herdr_layout_headers_buttons_and_two_line_entries() {
        let mut app = AppModel::new();
        super::super::app::refresh_local(&mut app, "my-mac", super::super::local::ServeState::Running, true);
        let mut alpha = host("peer-a", "alpha", "ready");
        alpha.link = Some(homeway_core::daemon::proto::HostLink {
            via: "direct".into(), ep: "e".into(), rtt_ms: 42, at: 61_000,
        });
        let hosts = vec![alpha, host("peer-b", "beta", "connecting")];
        app.hosts.feed(super::super::hosts::FeedInput::Snapshot { generation: "g1", hosts: &hosts });
        app.fallback_host();
        app.sessions.replace(vec![
            super::super::sessions::SessionView {
                name: "agent-a".into(), title: Some("重构".into()), agent: "codex".into(),
                state_v2: "working".into(), last_active_ms: 1, legs: 0, clients: Vec::new(), cols: 120, rows: 40,
            },
        ]);
        let mut t = Terminal::new(TestBackend::new(80, 16)).unwrap();
        let rates = std::iter::once(("peer-a".to_owned(), (10.0, 5.0))).collect();
        let mut hit_out = None;
        t.draw(|f| hit_out = Some(draw(f, &mut app, None, "", &rates, 61_000, None))).unwrap();
        let hit = hit_out.unwrap();
        let lines = buffer_text(&mut t);
        let joined = lines.join("\n");
        let flat = joined.replace(' ', "");
        // 区块小标题 + 加号（无边框——不再有 Block 边框字符）
        assert!(flat.contains("主机"), "主机区标题：{joined}");
        assert!(flat.contains("会话"), "会话区标题：{joined}");
        assert!(flat.contains("+"), "加号按钮：{joined}");
        assert!(!joined.contains('┌'), "无边框设计（无 Block 角字符）：{joined}");
        // 两行条目：● 名称 + 副标题
        assert!(flat.contains("●alpha"), "主机圆点+名称：{joined}");
        assert!(flat.contains("42ms(0s)"), "副标题采样年龄：{joined}");
        assert!(flat.contains("↑5B/s↓10B/s"), "副标题速率：{joined}");
        // 会话两行条目：● 名 / 徽章 · 标题
        assert!(flat.contains("●agent-a"), "会话圆点+名：{joined}");
        assert!(flat.contains("working·重构"), "会话副行徽章与标题：{joined}");
        // 命中区：+ 在标题行右端、布局同源
        assert!(hit.host_plus.y < hit.sess_plus.y, "主机区在上、会话区在下");
        assert_eq!(hit.host_plus.width, 3);
        assert_eq!(hit.term_close.width, 2);
    }

    #[test]
    fn term_pane_title_bar_with_close_button() {
        let mut app = AppModel::new();
        super::super::app::refresh_local(&mut app, "my-mac", super::super::local::ServeState::Running, true);
        let hosts = vec![host("peer-a", "alpha", "ready")];
        app.hosts.feed(super::super::hosts::FeedInput::Snapshot { generation: "g1", hosts: &hosts });
        app.fallback_host();
        app.sessions.replace(vec![
            super::super::sessions::SessionView {
                name: "main".into(), title: Some("构建中".into()), agent: "codex".into(),
                state_v2: "working".into(), last_active_ms: 1, legs: 0, clients: Vec::new(), cols: 120, rows: 40,
            },
        ]);
        app.sessions.select("main");
        app.focus = Focus::Terminal;
        let mut tv = super::super::termview::TermView::new(40, 8).unwrap();
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let rates = std::collections::HashMap::new();
        let mut hit_out = None;
        t.draw(|f| hit_out = Some(draw(f, &mut app, Some(&mut tv), "", &rates, 0, None))).unwrap();
        let _ = hit_out;
        let joined = buffer_text(&mut t).join("\n");
        let flat = joined.replace(' ', "");
        assert!(flat.contains("main·构建中"), "标题栏显示会话名与标题：{joined}");
        assert!(flat.contains("✕"), "标题栏右侧关闭按钮：{joined}");
    }

    #[test]
    fn overlay_renders_label_buffer_and_error() {
        let mut app = AppModel::new();
        let ov = Overlay {
            label: "+ 主机 token:".into(),
            buffer: "hmw2abc".into(),
            error: Some("token 非法".into()),
        };
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        let rates = std::collections::HashMap::new();
        let mut hit_out = None;
        t.draw(|f| hit_out = Some(draw(f, &mut app, None, "", &rates, 0, Some(&ov)))).unwrap();
        let _ = hit_out;
        let joined = buffer_text(&mut t).join("\n");
        let flat = joined.replace(' ', "");
        assert!(flat.contains("+主机token:"), "浮层标签：{joined}");
        assert!(flat.contains("hmw2abc"), "浮层输入缓冲：{joined}");
        assert!(flat.contains("token非法"), "浮层错误红字：{joined}");
    }

    #[test]
    fn host_dot_four_states() {
        use super::super::hosts::HostView;
        let fg = |s: Span<'static>| s.style.fg;
        let link = |via: &str| homeway_core::daemon::proto::HostLink {
            via: via.into(),
            ep: "e".into(),
            rtt_ms: 42,
            at: 1_000,
        };
        let mut ready = host("p", "a", "ready");
        ready.link = Some(link("direct"));
        assert_eq!(fg(host_dot(&HostView::from_state(&ready), true)), Some(theme::GREEN), "直连 = 绿");
        ready.link = Some(link("relay"));
        assert_eq!(fg(host_dot(&HostView::from_state(&ready), true)), Some(theme::YELLOW), "中继 = 黄");
        let connecting = host("p", "b", "connecting");
        let cv = HostView::from_state(&connecting);
        assert_eq!(fg(host_dot(&cv, true)), Some(theme::YELLOW), "连接中亮相 = 黄");
        assert_eq!(fg(host_dot(&cv, false)), Some(theme::OVERLAY0), "连接中暗相 = 灰（闪烁）");
        let down = host("p", "c", "failed");
        assert_eq!(fg(host_dot(&HostView::from_state(&down), true)), Some(theme::OVERLAY0), "未连接 = 灰");
    }

    #[test]
    fn host_detail_tier_shapes() {
        use super::super::hosts::HostView;
        let mut ready = host("peer-a", "alpha", "ready");
        ready.link = Some(homeway_core::daemon::proto::HostLink {
            via: "direct".into(), ep: "e".into(), rtt_ms: 42, at: 61_000,
        });
        let rv = HostView::from_state(&ready);
        assert_eq!(host_detail(&rv, Some((10.0, 5.0)), 61_000), "42ms(0s) · ↑5B/s ↓10B/s");
        assert_eq!(host_detail(&rv, None, 1_121_001), "42ms(1060s 陈旧) · 流量 —");
        let mut connecting = host("peer-b", "beta", "connecting");
        assert_eq!(host_detail(&HostView::from_state(&connecting), None, 0), "连接中…");
        connecting.reason = Some("正在建立隧道".into());
        assert_eq!(host_detail(&HostView::from_state(&connecting), None, 0), "连接中（正在建立隧道）");
        let down = host("peer-c", "gamma", "failed");
        assert_eq!(host_detail(&HostView::from_state(&down), None, 0), "peer-c");
    }

    #[test]
    fn status_line_unselected_hint() {
        let app = AppModel::new();
        assert!(status_line(&app).contains("未选中主机"), "空态引导");
        let mut app2 = AppModel::new();
        super::super::app::refresh_local(&mut app2, "my-mac", super::super::local::ServeState::Running, true);
        app2.selected_host = Some(LOCAL_HOST_ID.into());
        assert_eq!(status_line(&app2), "", "已选中 = 空基底");
    }

    #[test]
    fn term_content_shrinks_title_row() {
        let pane = Rect::new(10, 5, 40, 10);
        let content = term_content(pane);
        assert_eq!(content.y, 6);
        assert_eq!(content.height, 9);
        assert_eq!(content.width, 40);
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(512.0), "512B");
        assert_eq!(fmt_bytes(2048.0), "2KB");
        assert_eq!(fmt_bytes(3.0 * 1024.0 * 1024.0), "3MB");
    }
}
