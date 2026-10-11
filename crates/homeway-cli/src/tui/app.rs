//! 面板应用模型（tasks 4.1/4.5/4.6）：焦点模式、键路由、选中回落链、状态行差分。
//!
//! 全部纯逻辑（可 TestBackend/单测驱动）；线程接线在 runtime。纪律：
//! - **回落不写回**（r3 N-6）：回落链只改内存选中，持久化仅由用户发起的切换触发；
//! - **链路中断不回落**（r3 N）：主机回落仅在主机条目从显示序消失时发生——
//!   可恢复异常（link down/控制面断开）期间选中留守；
//! - **选中变化（含镜像）计为活动态触发**（r3 D）：由 runtime 在镜像路径调用。
//!
//! 消费方 = app 主循环（runtime，任务组 4b）——期间豁免 dead_code。

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

use super::hosts::HostView;
use super::local::LOCAL_HOST_ID;
use super::selection::SelectionState;
use super::sessions::SessionListModel;
use super::{hosts::HostsModel, local};

/// 焦点模式（spec 需求 4：双焦点）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Focus {
    /// 面板导航焦点：导航键由面板处理，不送达会话。
    Panel,
    /// 终端焦点：除固定退出组合键外全部透传（含 Alt/Fn/鼠标）。
    Terminal,
}

/// 导航焦点内的活动列表。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum Pane {
    #[default]
    Hosts,
    Sessions,
}

/// 退出终端焦点的固定组合键（F12——避开常见 TUI 应用与 shell 绑定空间，design D7）。
pub(crate) const EXIT_TERMINAL_KEY: KeyCode = KeyCode::F(12);

/// 面板级命令（导航焦点下按键的语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PanelCmd {
    /// 方向键（按活动 pane 解释：主机序或会话序移动）。
    MoveUp,
    MoveDown,
    SwitchPane,
    EnterTerminal,
    ExitTerminal,
    Quit,
    Noop,
}

/// 按键路由结果。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Route {
    Panel(PanelCmd),
    /// 终端焦点透传（调用方编码转发，spec 需求 4）。
    Passthrough,
}

/// 按键路由（spec 需求 4：终端焦点下除退出组合键外**全部**透传）。
/// 鼠标/粘贴不经过此函数（归属规则见 runtime：鼠标按会话上报位路由，粘贴走编码）。
pub(crate) fn route_key(key: &KeyEvent, focus: Focus) -> Route {
    // 释放/重复事件不参与面板导航（透传路径原样转发，由编码器处理 repeat/release）。
    if key.kind == KeyEventKind::Release {
        return match focus {
            Focus::Terminal => Route::Passthrough,
            Focus::Panel => Route::Panel(PanelCmd::Noop),
        };
    }
    match focus {
        Focus::Terminal => {
            if key.code == EXIT_TERMINAL_KEY && key.modifiers.is_empty() {
                Route::Panel(PanelCmd::ExitTerminal)
            } else {
                Route::Passthrough
            }
        }
        Focus::Panel => {
            let cmd = match key.code {
                KeyCode::Up => PanelCmd::MoveUp,
                KeyCode::Down => PanelCmd::MoveDown,
                KeyCode::Tab | KeyCode::Left | KeyCode::Right => PanelCmd::SwitchPane,
                KeyCode::Enter | KeyCode::F(12) => PanelCmd::EnterTerminal,
                KeyCode::Char('q') if key.modifiers.is_empty() => PanelCmd::Quit,
                _ => PanelCmd::Noop,
            };
            Route::Panel(cmd)
        }
    }
}

/// 面板应用模型。
pub(crate) struct AppModel {
    pub hosts: HostsModel,
    pub sessions: SessionListModel,
    /// 本机条目合成缓存（serve 状态拍刷新；None = 尚未首拍）。
    pub local: Option<HostView>,
    /// 选中主机 id（远端 = 主机表 id；本机 = 哨兵）。
    pub selected_host: Option<String>,
    pub pane: Pane,
    pub focus: Focus,
    /// 本实例是否交互实例（镜像跟随实例 false——不下发 RESIZE，r2 N-4）。
    pub interactive: bool,
    /// 主机显示序游标（导航用；selected_host 以 id 为真键）。
    pub host_cursor: usize,
    /// 会话显示序游标。
    pub session_cursor: usize,
}

impl AppModel {
    pub fn new() -> Self {
        AppModel {
            hosts: HostsModel::default(),
            sessions: SessionListModel::default(),
            local: None,
            selected_host: None,
            pane: Pane::Hosts,
            focus: Focus::Panel,
            interactive: true,
            host_cursor: 0,
            session_cursor: 0,
        }
    }

    /// 主机显示序（确定序：本机优先 + 名称升序 + 同名按加入先后，r3 N-6②）。
    pub fn display_hosts(&self) -> Vec<HostView> {
        self.hosts.display_order_with(self.local.clone())
    }

    pub fn selected_host_view(&self) -> Option<HostView> {
        let id = self.selected_host.as_deref()?;
        if id == LOCAL_HOST_ID {
            return self.local.clone();
        }
        self.hosts.get(id).cloned()
    }

    /// 主机导航游标落点（显示序内 id）。
    fn host_at(&self, idx: usize) -> Option<String> {
        self.display_hosts().get(idx).map(|h| h.id.clone())
    }

    /// 面板命令应用（导航焦点）。返回是否改变了选中（驱动即时列表重取，r3 D）。
    pub fn apply_panel(&mut self, cmd: PanelCmd) -> bool {
        let hosts_len = self.display_hosts().len();
        let sessions_len = self.sessions.sessions().len();
        match cmd {
            PanelCmd::MoveUp | PanelCmd::MoveDown if self.pane == Pane::Hosts => {
                let delta = if cmd == PanelCmd::MoveDown { 1i64 } else { -1i64 };
                let next = (self.host_cursor as i64 + delta).clamp(0, hosts_len.max(1) as i64 - 1) as usize;
                // 主机末位下探 ⇒ 落到会话栏（单主机时 ↑↓ 直接管理会话——r4 验收 UX）。
                if delta > 0 && next == self.host_cursor && hosts_len > 0 {
                    self.pane = Pane::Sessions;
                    self.session_cursor = 0;
                    if let Some(s) = self.sessions.sessions().first() {
                        let name = s.name.clone();
                        let changed = self.sessions.selected() != Some(name.as_str());
                        self.sessions.select(&name);
                        return changed;
                    }
                    return false;
                }
                if next != self.host_cursor {
                    self.host_cursor = next;
                    if let Some(id) = self.host_at(next) {
                        if self.selected_host.as_deref() != Some(id.as_str()) {
                            self.selected_host = Some(id);
                            self.sessions = SessionListModel::default(); // 换主机 = 会话列表重置
                            self.session_cursor = 0;
                            return true;
                        }
                    }
                }
                false
            }
            PanelCmd::MoveUp | PanelCmd::MoveDown if self.pane == Pane::Sessions => {
                let delta = if cmd == PanelCmd::MoveDown { 1i64 } else { -1i64 };
                // 会话顶上探主机栏（选最后一台主机），其余在栏内移动。
                if delta < 0 && self.session_cursor == 0 {
                    self.pane = Pane::Hosts;
                    self.host_cursor = hosts_len.max(1) - 1;
                    if let Some(h) = self.host_at(self.host_cursor) {
                        if self.selected_host.as_deref() != Some(h.as_str()) {
                            self.selected_host = Some(h);
                            self.sessions = SessionListModel::default();
                            self.session_cursor = 0;
                            return true;
                        }
                    }
                    return false;
                }
                let next = (self.session_cursor as i64 + delta).clamp(0, sessions_len.max(1) as i64 - 1) as usize;
                self.session_cursor = next;
                if let Some(s) = self.sessions.sessions().get(next) {
                    let name = s.name.clone();
                    let changed = self.sessions.selected() != Some(name.as_str());
                    self.sessions.select(&name);
                    return changed;
                }
                false
            }
            PanelCmd::SwitchPane => {
                self.pane = match self.pane {
                    Pane::Hosts => Pane::Sessions,
                    Pane::Sessions => Pane::Hosts,
                };
                false
            }
            PanelCmd::EnterTerminal => {
                if self.sessions.selected().is_some() {
                    self.focus = Focus::Terminal;
                }
                false
            }
            PanelCmd::ExitTerminal => {
                self.focus = Focus::Panel;
                false
            }
            // MoveUp/MoveDown 的两个 pane 分支已被上面守卫穷尽（Pane 二值），
            // 编译器不可知——此处兜底不可达。
            PanelCmd::Quit | PanelCmd::Noop | PanelCmd::MoveUp | PanelCmd::MoveDown => false,
        }
    }

    // ---- 选中回落链（spec 需求 10；回落不写回——调用方纪律） ----------------

    /// 启动期恢复（持久化选中）：主机阶段回落。
    /// 返回是否发生了回落（测试断言用）。
    pub fn restore(&mut self, sel: &SelectionState) -> bool {
        let Some(h) = sel.host_id.clone() else { return false };
        let known = h == LOCAL_HOST_ID && self.local.is_some()
            || self.hosts.get(&h).is_some();
        if known {
            self.selected_host = Some(h.clone());
            self.host_cursor = self
                .display_hosts()
                .iter()
                .position(|v| v.id == h)
                .unwrap_or(0);
        } else {
            self.fallback_host();
            return true;
        }
        if let Some(s) = &sel.session {
            // 会话存在性要等列表首拍（runtime 在 on_sessions 路径补回落）。
            self.sessions.select(s);
        }
        false
    }

    /// 会话列表首拍后的会话阶段回落：选中消失 ⇒ 同主机最近活跃（spec 回落链）。
    pub fn fallback_on_sessions(&mut self) -> bool {
        if self.sessions.selected().is_some() {
            return false;
        }
        if let Some(s) = self.sessions.most_recent() {
            let name = s.name.clone();
            self.sessions.select(&name);
            self.session_cursor = 0;
            return true;
        }
        false
    }

    /// 主机阶段回落：第一个可用条目（确定序）。全部不可用 ⇒ 空态（selected=None）。
    pub fn fallback_host(&mut self) -> bool {
        let first = self.display_hosts().into_iter().find(|h| {
            h.id == LOCAL_HOST_ID || h.state == "ready" || h.state == "starting"
        });
        self.selected_host = first.map(|h| h.id);
        self.host_cursor = 0;
        self.sessions = SessionListModel::default();
        self.session_cursor = 0;
        self.selected_host.is_some()
    }

    /// 主机从显示序消失（仅此触发主机回落，r3 N：可恢复异常不回落）。
    pub fn on_hosts_updated(&mut self) -> bool {
        if let Some(id) = &self.selected_host {
            let still = id == LOCAL_HOST_ID && self.local.is_some()
                || self.hosts.get(id).is_some();
            if !still {
                return self.fallback_host();
            }
        } else if self.selected_host.is_none() {
            // 无选中（空态起步）：首个可用条目出现即选中。
            return self.fallback_host();
        }
        false
    }

    // ---- 状态行差分（spec 需求 8：应用流口径速率） --------------------------

    /// 流量速率：上一拍 (rx,tx,at_ms) 与当前拍的差分；dt 非法或计数回退 ⇒ None。
    pub fn rate_of(
        prev: Option<(i64, i64, i64)>,
        cur: (i64, i64, i64),
    ) -> Option<(f64, f64)> {
        let (prx, ptx, pat) = prev?;
        let (rx, tx, at) = cur;
        let dt = (at - pat) as f64 / 1000.0;
        if dt <= 0.0 || rx < prx || tx < ptx {
            return None;
        }
        Some(((rx - prx) as f64 / dt, (tx - ptx) as f64 / dt))
    }
}

impl Default for AppModel {
    fn default() -> Self {
        Self::new()
    }
}

/// 合成本机条目刷新（runtime 每 serve 状态拍调用）。
pub(crate) fn refresh_local(app: &mut AppModel, hostname: &str, serve: local::ServeState, sock_ok: bool) {
    app.local = Some(local::compose_local(hostname, serve, sock_ok));
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::hosts::FeedInput;
    use crossterm::event::KeyModifiers as KM;
    use homeway_core::daemon::proto::HostState;

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

    fn seeded() -> AppModel {
        let mut app = AppModel::new();
        refresh_local(&mut app, "my-mac", local::ServeState::Running, true);
        let hosts = vec![host("peer-a", "alpha", "ready"), host("peer-b", "beta", "ready")];
        app.hosts.feed(FeedInput::Snapshot { generation: "g1", hosts: &hosts });
        app.fallback_host(); // 空态起步 ⇒ 首个可用（本机）
        app
    }

    #[test]
    fn route_terminal_focus_passthrough_all_but_exit() {
        // Alt 组合 / F1 / Ctrl+C / 字符：全部透传。
        for k in [
            key(KeyCode::Char('c'), KM::CONTROL),
            key(KeyCode::Char('x'), KM::ALT),
            key(KeyCode::F(1), KM::NONE),
            key(KeyCode::Char('a'), KM::NONE),
            key(KeyCode::Backspace, KM::NONE),
        ] {
            assert_eq!(route_key(&k, Focus::Terminal), Route::Passthrough, "{k:?}");
        }
        // 唯一例外：F12 退出终端焦点。
        assert_eq!(
            route_key(&key(EXIT_TERMINAL_KEY, KM::NONE), Focus::Terminal),
            Route::Panel(PanelCmd::ExitTerminal)
        );
        // Ctrl+F12 不是退出组合键（固定组合 = 裸 F12）。
        assert_eq!(route_key(&key(EXIT_TERMINAL_KEY, KM::CONTROL), Focus::Terminal), Route::Passthrough);
    }

    #[test]
    fn route_panel_focus_maps_navigation() {
        assert_eq!(route_key(&key(KeyCode::Tab, KM::NONE), Focus::Panel), Route::Panel(PanelCmd::SwitchPane));
        assert_eq!(route_key(&key(KeyCode::Enter, KM::NONE), Focus::Panel), Route::Panel(PanelCmd::EnterTerminal));
        assert_eq!(route_key(&key(KeyCode::Char('q'), KM::NONE), Focus::Panel), Route::Panel(PanelCmd::Quit));
        // 方向键按 pane 解释（在 apply_panel），路由层只标记面板处理。
        assert_eq!(route_key(&key(KeyCode::Up, KM::NONE), Focus::Panel), Route::Panel(PanelCmd::MoveUp));
    }

    fn key(code: KeyCode, mods: crossterm::event::KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn navigation_and_host_switch_resets_sessions() {
        let mut app = seeded();
        app.apply_panel(PanelCmd::MoveDown); // local → alpha
        assert_eq!(app.selected_host.as_deref(), Some("peer-a"));
        // 换主机 ⇒ 会话模型重置（列表等下一拍）。
        app.apply_panel(PanelCmd::MoveDown); // alpha → beta
        assert_eq!(app.selected_host.as_deref(), Some("peer-b"));
        assert_eq!(app.sessions.sessions().len(), 0);
    }

    #[test]
    fn fallback_chain_host_then_session() {
        // 启动恢复：持久主机不在表内 ⇒ 回落第一个可用。
        let mut app = seeded();
        let persisted = SelectionState {
            revision: 1,
            host_id: Some("peer-gone".into()),
            session: Some("ghost".into()),
        };
        assert!(app.restore(&persisted), "主机阶段回落");
        assert_eq!(app.selected_host.as_deref(), Some("local"), "第一个可用条目 = 确定序首位（本机可用即本机）");
        // 会话阶段：列表首拍无 ghost ⇒ 同主机最近活跃（会话模型与主机无关地验证）。
        app.sessions = SessionListModel::default();
        app.sessions
            .replace(parse_sessions(&[("real", 100), ("older", 50)]));
        assert!(app.fallback_on_sessions());
        assert_eq!(app.sessions.selected(), Some("real"), "回落到最近活跃");
        // 回落不写回（restore 未再动持久化——由调用方纪律保证，模型层无写接口）。
    }

    #[test]
    fn host_removed_falls_back_but_link_down_keeps() {
        let mut app = seeded();
        app.apply_panel(PanelCmd::MoveDown);
        assert_eq!(app.selected_host.as_deref(), Some("peer-a"));
        // 主机从表消失 ⇒ 回落（确定序首位可用 = 本机）。
        let hosts = vec![host("peer-b", "beta", "ready")];
        app.hosts.feed(FeedInput::Snapshot { generation: "g2", hosts: &hosts });
        assert!(app.on_hosts_updated());
        assert_eq!(app.selected_host.as_deref(), Some("local"));
        // 选中远端主机后其链路转 failed（可恢复异常）⇒ 留守不回落。
        app.apply_panel(PanelCmd::MoveDown); // local → beta
        assert_eq!(app.selected_host.as_deref(), Some("peer-b"));
        let hosts = vec![host("peer-b", "beta", "failed")];
        app.hosts.feed(FeedInput::Snapshot { generation: "g3", hosts: &hosts });
        assert!(!app.on_hosts_updated(), "可恢复异常不触发主机回落");
        assert_eq!(app.selected_host.as_deref(), Some("peer-b"), "留守");
    }

    #[test]
    fn single_host_down_falls_through_to_sessions() {
        // r4 验收 UX：单主机下 ↓ 落到会话栏（↑↓ 用户心智 = 连续浏览）。
        let mut app = AppModel::new();
        super::super::app::refresh_local(&mut app, "my-mac", super::super::local::ServeState::Running, true);
        app.fallback_host(); // 仅本机（无远端快照）⇒ 唯一条目
        assert_eq!(app.selected_host.as_deref(), Some("local"));
        app.sessions
            .replace(parse_sessions(&[("demo", 100)]));
        assert!(app.apply_panel(PanelCmd::MoveDown), "落到会话栏并选中首会话");
        assert_eq!(app.pane, Pane::Sessions);
        assert!(app.sessions.selected().is_some());
        // 会话顶上 ↑ 回到主机栏（选中不变——本机已是当前选中）。
        app.apply_panel(PanelCmd::MoveUp);
        assert_eq!(app.pane, Pane::Hosts);
        assert_eq!(app.selected_host.as_deref(), Some("local"));
    }

    #[test]
    fn rate_differs_and_guards() {
        assert_eq!(AppModel::rate_of(None, (100, 50, 1000)), None, "无上一拍");
        assert_eq!(AppModel::rate_of(Some((100, 50, 1000)), (1100, 1050, 2000)), Some((1000.0, 1000.0)));
        assert_eq!(AppModel::rate_of(Some((1100, 50, 1000)), (100, 50, 2000)), None, "计数回退");
        assert_eq!(AppModel::rate_of(Some((100, 50, 2000)), (100, 50, 1000)), None, "dt 非法");
    }

    fn parse_sessions(items: &[(&str, i64)]) -> Vec<super::super::sessions::SessionView> {
        items
            .iter()
            .map(|(n, t)| super::super::sessions::SessionView {
                name: (*n).into(),
                title: None,
                agent: "shell".into(),
                state_v2: "unknown".into(),
                last_active_ms: *t,
                legs: 0,
                clients: Vec::new(),
                cols: 80,
                rows: 24,
            })
            .collect()
    }
}
