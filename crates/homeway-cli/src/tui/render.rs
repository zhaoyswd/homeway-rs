//! 四区渲染（tasks 4.1/4.2/4.3/4.5）：布局 + 主机列表 + 会话列表 + 状态行。
//! 全部经 `ratatui::backend::TestBackend` 快照断言（无真终端依赖）。
//!
//! 终端视图区域由 [`super::termview`] 负责；此处只铺区域与空态引导提示。
//!
//! 消费方 = app 主循环（runtime，任务组 4b）——期间豁免 dead_code。

#![allow(dead_code)]

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color as RColor, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Widget};
use ratatui::Frame;

use super::app::{AppModel, Focus};
use super::hosts::HostView;
use super::local::LOCAL_HOST_ID;
use super::sessions::AgentBadge;

/// 左栏宽度占比（主机/会话列表）。
const LEFT_PCT: u16 = 35;

/// 四区布局（spec 需求 1）：左上主机列表 / 左下会话列表 / 右侧终端视图 / 底部状态行。
pub(crate) fn layout(area: Rect) -> (Rect, Rect, Rect, Rect) {
    let v = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).split(area);
    let h = Layout::horizontal([Constraint::Percentage(LEFT_PCT), Constraint::Percentage(100 - LEFT_PCT)])
        .split(v[0]);
    let l = Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(h[0]);
    (l[0], l[1], h[1], v[1])
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

/// 主机条目圆点（r5 验收批）：四态区分——直连绿 / 中继黄 / 连接中闪烁（黄↔灰由
/// blink_on 相位切换，约 0.5s 半拍）/ 未连接灰。本机已连接视同直连绿。
pub(crate) fn host_dot(h: &HostView, blink_on: bool) -> Span<'static> {
    let fg = if h.state == "connecting" {
        if blink_on { RColor::Yellow } else { RColor::DarkGray }
    } else if h.state == "ready" {
        if h.via.as_deref() == Some("relay") { RColor::Yellow } else { RColor::Green }
    } else {
        RColor::DarkGray
    };
    Span::styled("●", Style::new().fg(fg))
}

/// 主机条目行：`● name`（连接性由圆点承载，不再缀 [state·via] 尾巴；不可用态
/// 主机名压暗）。
pub(crate) fn host_line(h: &HostView, blink_on: bool) -> Line<'static> {
    let dim = h.state != "ready" && h.state != "connecting";
    let name = Span::styled(
        h.name.clone(),
        if dim { Style::new().add_modifier(Modifier::DIM) } else { Style::new() },
    );
    Line::from(vec![host_dot(h, blink_on), Span::raw(" "), name])
}

/// 主机条目第二行（r5 验收批：连接状态信息自状态行迁入；口径参考 tier 首页
/// HostCard 副标题）：已连接 = `延迟 42ms(3s) · ↑tx ↓rx`（采样年龄 spec 需求 8
/// 随迁，>120s 标陈旧）；连接中 = 连接中文案（带 reason）；未连接 = 主机 id
/// （token 掩码，tier 同款）；本机已连接 = `local`。
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

/// 会话条目行：`name · badge · title`。
pub(crate) fn session_line(
    name: &str,
    title: Option<&str>,
    badge: &str,
    selected: bool,
) -> Line<'static> {
    let title = title.unwrap_or("-");
    let text = format!("{name} · {badge} · {title}");
    Line::from(Span::styled(
        text,
        if selected { Style::new().add_modifier(Modifier::REVERSED) } else { Style::new() },
    ))
}

/// 状态行基底（r5 验收批：连接状态信息已迁主机条目第二行——此处只留未选中引导；
/// 相位/回看/焦点尾巴由 runtime 追加）。
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

/// 面板整体绘制（UI 主循环每拍调用）。
pub(crate) fn draw(
    f: &mut Frame,
    app: &mut AppModel,
    term: Option<&mut super::termview::TermView>,
    status: &str,
    rates: &std::collections::HashMap<String, (f64, f64)>,
    now_ms: i64,
) {
    let area = f.area();
    let (hosts_a, sess_a, term_a, status_a) = layout(area);
    let blink_on = (now_ms / 500) % 2 == 0;
    // 左上：主机列表（两行条目 = ● 名称 + tier 口径副标题；ListState 滚动跟随选中）
    let hosts = app.display_hosts();
    let items: Vec<ListItem> = hosts
        .iter()
        .map(|h| {
            let rate = rates.get(&h.id).copied();
            ListItem::new(vec![
                host_line(h, blink_on),
                Line::from(Span::styled(
                    host_detail(h, rate, now_ms),
                    Style::new().add_modifier(Modifier::DIM),
                )),
            ])
        })
        .collect();
    let empty_hosts = items.is_empty();
    let mut ls = ListState::default();
    if !hosts.is_empty() {
        ls.select(Some(app.host_cursor.min(hosts.len() - 1)));
    }
    let list = List::new(items)
        .block(Block::new().borders(Borders::ALL).title("主机"))
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    f.render_stateful_widget(list, hosts_a, &mut ls);
    if empty_hosts {
        Paragraph::new("（无主机）").render(hosts_a, f.buffer_mut());
    }
    // 左下：会话列表
    let sessions = app.sessions.sessions().to_vec();
    let selected_name = app.sessions.selected().map(|s| s.to_owned());
    let s_items: Vec<ListItem> = sessions
        .iter()
        .map(|s| {
            let b = super::sessions::badge(&s.agent, &s.state_v2);
            session_line(&s.name, s.title.as_deref(), badge_text(b), selected_name.as_deref() == Some(s.name.as_str()))
        })
        .map(ListItem::new)
        .collect();
    List::new(s_items)
        .block(Block::new().borders(Borders::ALL).title(format!("会话（{}）", sessions.len())))
        .render(sess_a, f.buffer_mut());
    // 右侧：终端视图或引导提示
    match term {
        Some(tv) => {
            tv.render(f, term_a);
            // 终端焦点的宿主光标 = 会话 VT 光标（会话声明可见位且贴底视口时；
            // 回看中的光标位置无语义，保持隐藏）。面板焦点下 ratatui 缺省隐藏。
            let cur = tv.leg.cursor();
            if app.focus == Focus::Terminal
                && cur.visible
                && tv.leg.display_offset() == 0
            {
                if let Some(p) = super::termview::cursor_in_pane(cur, term_a) {
                    f.set_cursor_position(p);
                }
            }
        }
        None => {
            Paragraph::new(
                "↑↓ 选择主机 / 会话；Enter 进入终端焦点（F12 返回）；q 退出。\n选中会话后，这里显示其终端画面。",
            )
            .render(term_a, f.buffer_mut());
        }
    }
    // 底部：状态行
    let focus_tag = match app.focus {
        Focus::Panel => "",
        Focus::Terminal => " [终端焦点 F12 返回]",
    };
    Line::from(Span::raw(format!("{status}{focus_tag}"))).render(status_a, f.buffer_mut());
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
    fn draw_shows_two_line_hosts_and_hint() {
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
        t.draw(|f| draw(f, &mut app, None, "", &rates, 61_000)).unwrap();
        let lines = buffer_text(&mut t);
        let joined = lines.join("\n");
        // CJK 在 ratatui 缓冲按宽字符分格（拼接出现空格），中文断言一律去空格比对。
        let flat = joined.replace(' ', "");
        assert!(flat.contains("主机"), "主机区标题：{joined}");
        // 两行条目：● 名称 + tier 口径副标题（圆点字符 + 副标题内容都在位）
        assert!(flat.contains("●alpha"), "圆点+名称行：{joined}");
        assert!(flat.contains("42ms(0s)"), "副标题带采样年龄（spec 需求 8）：{joined}");
        assert!(flat.contains("↑5B/s↓10B/s"), "副标题 ↑tx ↓rx 速率：{joined}");
        assert!(flat.contains("●beta"), "连接中主机条目在位：{joined}");
        assert!(flat.contains("会话（1）"), "会话区标题：{joined}");
        assert!(flat.contains("agent-a·working·重构"), "徽章与标题：{joined}");
        assert!(flat.contains("进入终端焦点"), "引导提示：{joined}");
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
        assert_eq!(fg(host_dot(&HostView::from_state(&ready), true)), Some(RColor::Green), "直连 = 绿");
        ready.link = Some(link("relay"));
        assert_eq!(fg(host_dot(&HostView::from_state(&ready), true)), Some(RColor::Yellow), "中继 = 黄");
        let connecting = host("p", "b", "connecting");
        let cv = HostView::from_state(&connecting);
        assert_eq!(fg(host_dot(&cv, true)), Some(RColor::Yellow), "连接中亮相 = 黄");
        assert_eq!(fg(host_dot(&cv, false)), Some(RColor::DarkGray), "连接中暗相 = 灰（闪烁）");
        let down = host("p", "c", "failed");
        assert_eq!(fg(host_dot(&HostView::from_state(&down), true)), Some(RColor::DarkGray), "未连接 = 灰");
    }

    #[test]
    fn host_detail_tier_shapes() {
        use super::super::hosts::HostView;
        let mut ready = host("peer-a", "alpha", "ready");
        ready.link = Some(homeway_core::daemon::proto::HostLink {
            via: "direct".into(), ep: "e".into(), rtt_ms: 42, at: 61_000,
        });
        let rv = HostView::from_state(&ready);
        // 已连接：延迟(采样年龄) · ↑tx ↓rx（tier 口径）
        assert_eq!(host_detail(&rv, Some((10.0, 5.0)), 61_000), "42ms(0s) · ↑5B/s ↓10B/s");
        // 陈旧采样（>120s；rv 锚 at=61_000 ⇒ (1121001-61000)/1000 = 1060s）
        assert_eq!(host_detail(&rv, None, 1_121_001), "42ms(1060s 陈旧) · 流量 —");
        // 连接中：阶段文案（带 reason / 无 reason）
        let mut connecting = host("peer-b", "beta", "connecting");
        assert_eq!(host_detail(&HostView::from_state(&connecting), None, 0), "连接中…");
        connecting.reason = Some("正在建立隧道".into());
        assert_eq!(host_detail(&HostView::from_state(&connecting), None, 0), "连接中（正在建立隧道）");
        // 未连接：主机 id（token 掩码，tier 同款）
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
        assert_eq!(status_line(&app2), "", "已选中 = 空基底（相位/回看/焦点尾巴由 runtime 追加）");
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(512.0), "512B");
        assert_eq!(fmt_bytes(2048.0), "2KB");
        assert_eq!(fmt_bytes(3.0 * 1024.0 * 1024.0), "3MB");
    }
}
