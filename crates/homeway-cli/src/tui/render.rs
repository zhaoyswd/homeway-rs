//! 四区渲染（tasks 4.1/4.2/4.3/4.5）：布局 + 主机列表 + 会话列表 + 状态行。
//! 全部经 `ratatui::backend::TestBackend` 快照断言（无真终端依赖）。
//!
//! 终端视图区域由 [`super::termview`] 负责；此处只铺区域与空态引导提示。
//!
//! 消费方 = app 主循环（runtime，任务组 4b）——期间豁免 dead_code。

#![allow(dead_code)]

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Widget};
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

/// 主机条目行：`name [state·via]`（本机条目带 local 标签；不可用态显式标注）。
pub(crate) fn host_line(h: &HostView) -> Line<'static> {
    let local = h.id == LOCAL_HOST_ID;
    let via = if local { "local".to_owned() } else { h.via.clone().unwrap_or_else(|| "—".into()) };
    let mut text = format!("{} [{}·{}]", h.name, h.state, via);
    let style = if local && h.state != "ready" {
        text = format!("{} [不可用]", h.name);
        Style::new().add_modifier(Modifier::DIM)
    } else {
        Style::new()
    };
    Line::from(Span::styled(text, style))
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

/// 状态行文本（spec 需求 8）：`name · 形态 · 延迟（含陈旧标注）· 流量速率`。
/// 读数不可得 = 明确占位「—」，不以空白或误值冒充。
pub(crate) fn status_line(
    app: &AppModel,
    now_ms: i64,
    rate: Option<(f64, f64)>,
) -> String {
    let Some(h) = app.selected_host_view() else {
        return "未选中主机（↑↓ 选择）".to_owned();
    };
    let local = h.id == LOCAL_HOST_ID;
    let via = if local { "local".to_owned() } else { h.via.clone().unwrap_or("—".into()) };
    let mut parts = vec![format!("{} · {}", h.name, via)];
    // 延迟：端到端探活口径 + 采样年龄（>120s 标陈旧——spec 需求 8 / r3 N-10）。
    if local {
        parts.push("延迟 —".to_owned());
    } else {
        match (h.rtt_ms, h.link_at) {
            (Some(rtt), Some(at)) => {
                let age = now_ms - at;
                if age > 120_000 {
                    parts.push(format!("延迟 {rtt}ms(陈旧)"));
                } else {
                    parts.push(format!("延迟 {rtt}ms"));
                }
            }
            _ => parts.push("延迟 —".to_owned()),
        }
    }
    // 流量：应用流口径速率（差分由 app 层算）；不可得 = 占位。
    match rate {
        Some((rx, tx)) => parts.push(format!("{} {}/s {} {}/s", "流量", fmt_bytes(rx), "↑", fmt_bytes(tx))),
        None => parts.push("流量 —".to_owned()),
    }
    parts.join(" · ")
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
) {
    let area = f.area();
    let (hosts_a, sess_a, term_a, status_a) = layout(area);
    // 左上：主机列表
    let hosts = app.display_hosts();
    let cursor_host = hosts.get(app.host_cursor).map(|h| h.id.clone());
    let items: Vec<ListItem> = hosts
        .iter()
        .map(|h| {
            let mut line = host_line(h);
            if cursor_host.as_deref() == Some(h.id.as_str()) {
                line.style = Style::new().add_modifier(Modifier::REVERSED);
            }
            ListItem::new(line)
        })
        .collect();
    let empty_hosts = items.is_empty();
    List::new(items)
        .block(Block::new().borders(Borders::ALL).title("主机"))
        .render(hosts_a, f.buffer_mut());
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
        Some(tv) => tv.render(f, term_a),
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
    fn draw_shows_hosts_sessions_status_and_hint() {
        let mut app = AppModel::new();
        super::super::app::refresh_local(&mut app, "my-mac", super::super::local::ServeState::Running, true);
        let hosts = vec![host("peer-a", "alpha", "ready"), host("peer-b", "beta", "ready")];
        app.hosts.feed(super::super::hosts::FeedInput::Snapshot { generation: "g1", hosts: &hosts });
        app.fallback_host();
        app.sessions.replace(vec![
            super::super::sessions::SessionView {
                name: "agent-a".into(), title: Some("重构".into()), agent: "codex".into(),
                state_v2: "working".into(), last_active_ms: 1, legs: 0, cols: 120, rows: 40,
            },
        ]);
        let mut t = Terminal::new(TestBackend::new(80, 12)).unwrap();
        t.draw(|f| draw(f, &mut app, None, "my-mac · local · 延迟 — · 流量 —")).unwrap();
        let lines = buffer_text(&mut t);
        let joined = lines.join("\n");
        // CJK 在 ratatui 缓冲按宽字符分格（拼接出现空格），中文断言一律去空格比对。
        let flat = joined.replace(' ', "");
        assert!(flat.contains("主机"), "主机区标题：{joined}");
        assert!(flat.contains("my-mac[ready·local]"), "本机条目带 local：{joined}");
        assert!(flat.contains("alpha[ready·—]") || flat.contains("alpha[ready·direct]"), "{joined}");
        assert!(flat.contains("会话（1）"), "会话区标题：{joined}");
        assert!(flat.contains("agent-a·working·重构"), "徽章与标题：{joined}");
        assert!(flat.contains("进入终端焦点"), "引导提示：{joined}");
        assert!(flat.contains("my-mac·local·延迟—·流量—"), "状态行（CJK 断言看 flat）：{joined}");
    }

    #[test]
    fn status_line_stale_and_placeholders() {
        let mut app = AppModel::new();
        let mut h = host("peer-a", "alpha", "ready");
        h.link = Some(homeway_core::daemon::proto::HostLink {
            via: "direct".into(), ep: "e".into(), rtt_ms: 42, at: 1_000,
        });
        let hosts = vec![h];
        app.hosts.feed(super::super::hosts::FeedInput::Snapshot { generation: "g1", hosts: &hosts });
        app.selected_host = Some("peer-a".into());
        // 新鲜采样
        let s = status_line(&app, 61_000, None);
        assert!(s.contains("alpha · direct") && s.contains("延迟 42ms") && s.contains("流量 —"), "{s}");
        // 陈旧采样（>120s）
        let s = status_line(&app, 1_121_001, Some((10.0, 5.0)));
        assert!(s.contains("延迟 42ms(陈旧)"), "{s}");
        assert!(s.contains("10B/s"), "{s}");
    }

    #[test]
    fn status_line_local_and_empty() {
        let mut app = AppModel::new();
        super::super::app::refresh_local(&mut app, "my-mac", super::super::local::ServeState::Running, true);
        app.selected_host = Some(LOCAL_HOST_ID.into());
        let s = status_line(&app, 0, None);
        assert!(s.contains("local") && s.contains("延迟 —"), "{s}");
        // 空态
        let app2 = AppModel::new();
        assert!(status_line(&app2, 0, None).contains("未选中主机"));
    }

    #[test]
    fn fmt_bytes_units() {
        assert_eq!(fmt_bytes(512.0), "512B");
        assert_eq!(fmt_bytes(2048.0), "2KB");
        assert_eq!(fmt_bytes(3.0 * 1024.0 * 1024.0), "3MB");
    }
}
