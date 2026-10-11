//! 会话列表模型（design D3 / tasks 3.3）：LIST 应答解析 + agent 徽章映射 +
//! 显示排序 + 选中保持。轮询 I/O（短连接拍）在 backend；本模块纯逻辑全分支单测。
//!
//! 词面真源 = `term/frames.rs` 的 `state_v2`/`agent` 模块。徽章映射（r1 R-3）：
//! `agent ∈ {shell, unknown}` ⇒ 无 agent，否则按 stateV2 四态
//! working / blocked / idle / unknown（真源无 done 态）。
//! 显示排序（r1 R-4）：最近活跃在前（lastActiveMs 降序、会话名升序破平），
//! 选中以会话名为键。


// 模型先于接线落地（tasks 3.3 的 LIST 轮询接线在下一批）——期间豁免 dead_code，接线后移除。
#![allow(dead_code)]
use serde::Deserialize;

use super::local::LOCAL_HOST_ID;

/// 会话条目（会话列表与终端视图选中共用的视图形态）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct SessionView {
    pub name: String,
    pub title: Option<String>,
    pub agent: String,
    pub state_v2: String,
    pub last_active_ms: i64,
    /// 观察腿数量（LIST `clients[]` 长度——腿满判定的输入，r2 N-2）。
    pub legs: usize,
}

/// agent 状态徽章（spec 需求 3 的五形态：四态 + 无 agent）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentBadge {
    NoAgent,
    Working,
    Blocked,
    Idle,
    Unknown,
}

/// (agent, stateV2) → 徽章。身份为 shell / unknown / 空 ⇒ 无 agent。
pub(crate) fn badge(agent: &str, state_v2: &str) -> AgentBadge {
    match agent {
        "shell" | "unknown" | "" => AgentBadge::NoAgent,
        _ => match state_v2 {
            "working" => AgentBadge::Working,
            "blocked" => AgentBadge::Blocked,
            "idle" => AgentBadge::Idle,
            _ => AgentBadge::Unknown,
        },
    }
}

/// 显示排序：最近活跃在前、会话名升序破平（就地排序）。
pub(crate) fn display_order(v: &mut [SessionView]) {
    v.sort_by(|a, b| b.last_active_ms.cmp(&a.last_active_ms).then_with(|| a.name.cmp(&b.name)));
}

/// 解析 LIST 应答载荷（`{"sessions":[…]}`；坏载荷 ⇒ `None`，轮询拍丢弃）。
pub(crate) fn parse_list_reply(payload: &[u8]) -> Option<Vec<SessionView>> {
    let wrap: ListWrap = serde_json::from_slice(payload).ok()?;
    Some(
        wrap.sessions
            .into_iter()
            .map(|s| SessionView {
                title: (!s.title.is_empty()).then_some(s.title),
                legs: s.clients.len(),
                name: s.name,
                agent: s.agent,
                state_v2: s.state_v2,
                last_active_ms: s.last_active_ms,
            })
            .collect(),
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListSession {
    name: String,
    #[serde(rename = "lastActiveMs")]
    last_active_ms: i64,
    #[serde(rename = "stateV2")]
    state_v2: String,
    agent: String,
    title: String,
    #[serde(default)]
    clients: Vec<serde_json::Value>,
}

/// 外层包装：`sessions` 必填——`{}`/`[]` 等缺键形态一律拒收（轮询拍丢弃）。
#[derive(Deserialize)]
struct ListWrap {
    sessions: Vec<ListSession>,
}

/// 选中主机的会话列表模型：排序 + 以会话名为键的选中保持（r1 R-4）。
#[derive(Default)]
pub(crate) struct SessionListModel {
    sessions: Vec<SessionView>,
    /// 选中（会话名）。目标消失时置空——**回落是 app 层动作**（回落不写回、
    /// 不经模型擅自换目标，r3 N）；模型只如实反映「选中目标已不存在」。
    selected: Option<String>,
}

impl SessionListModel {
    /// 整表替换（每拍 LIST 的结果）。返回内容是否变化（驱动重绘的判定之一）。
    pub fn replace(&mut self, mut sessions: Vec<SessionView>) -> bool {
        display_order(&mut sessions);
        let changed = sessions != self.sessions;
        if let Some(sel) = &self.selected {
            if !sessions.iter().any(|s| &s.name == sel) {
                self.selected = None;
            }
        }
        self.sessions = sessions;
        changed
    }

    pub fn sessions(&self) -> &[SessionView] {
        &self.sessions
    }

    pub fn selected(&self) -> Option<&str> {
        self.selected.as_deref()
    }

    /// 用户发起的选中切换（镜像接收也走这里——spec 需求 9）。
    /// 目标不存在时拒绝（保持原选中——回落链由 app 层按 spec 处理）。
    pub fn select(&mut self, name: &str) -> bool {
        if self.sessions.iter().any(|s| s.name == name) {
            self.selected = Some(name.to_owned());
            true
        } else {
            false
        }
    }

    /// 回落链第一步的输入：同主机最近活跃会话（显示序首）。
    pub fn most_recent(&self) -> Option<&SessionView> {
        self.sessions.first()
    }

    pub fn badge_of(&self, name: &str) -> Option<AgentBadge> {
        self.sessions
            .iter()
            .find(|s| s.name == name)
            .map(|s| badge(&s.agent, &s.state_v2))
    }

    /// 本机哨兵的直读捷径（本机条目选中时终端视图走本地 UDS——与远端同一模型）。
    pub fn is_local_host(host_id: &str) -> bool {
        host_id == LOCAL_HOST_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// tasks 3.3 内联 LIST 字样（不动冻结夹具；字段名与出口 `list_json` 同形）。
    /// 字节串字面量不能含非 ASCII，经 json! 构造（标题用例带中文）。
    fn sample_reply() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "sessions": [
                {"name":"agent-a","createdMs":1,"lastActiveMs":300,"attached":true,
                 "agent":"codex","stateV2":"working","title":"重构 auth","cwd":"/x",
                 "cols":120,"rows":40,"pid":11,
                 "clients":[{"kind":"app","cols":120,"rows":40,"sinceMs":5,"active":true}]},
                {"name":"shell","createdMs":2,"lastActiveMs":900,"attached":false,
                 "agent":"shell","stateV2":"unknown","title":"","cwd":"/y",
                 "cols":80,"rows":24,"pid":12,"clients":[]},
                {"name":"agent-b","createdMs":3,"lastActiveMs":300,"attached":false,
                 "agent":"claude","stateV2":"blocked","title":"","cwd":"/z",
                 "cols":80,"rows":24,"pid":13,"clients":[]}
            ]
        }))
        .expect("样例序列化")
    }

    #[test]
    fn badge_mapping_full_matrix() {
        assert_eq!(badge("shell", "unknown"), AgentBadge::NoAgent);
        assert_eq!(badge("unknown", "working"), AgentBadge::NoAgent, "未识别身份=无 agent（r1 R-3）");
        assert_eq!(badge("", "idle"), AgentBadge::NoAgent);
        assert_eq!(badge("codex", "working"), AgentBadge::Working);
        assert_eq!(badge("codex", "blocked"), AgentBadge::Blocked);
        assert_eq!(badge("claude", "idle"), AgentBadge::Idle);
        assert_eq!(badge("opencode", "weird"), AgentBadge::Unknown, "真源外的态=unknown 占位");
    }

    #[test]
    fn parse_list_reply_and_order() {
        let mut v = parse_list_reply(&sample_reply()).expect("样例可解析");
        assert_eq!(v.len(), 3);
        display_order(&mut v);
        // 最近活跃在前（900 > 300），同名活跃破平按会话名升序。
        assert_eq!(
            v.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
            ["shell", "agent-a", "agent-b"]
        );
        let a = v.iter().find(|s| s.name == "agent-a").unwrap();
        assert_eq!((a.agent.as_str(), a.state_v2.as_str(), a.legs, a.title.as_deref()), ("codex", "working", 1, Some("重构 auth")));
    }

    /// 只含 ASCII 的字样可保留字节串形态（坏载荷用例）。
    const BAD_PAYLOAD: &[u8] = b"{nope";

    #[test]
    fn parse_bad_payload_is_none() {
        assert_eq!(parse_list_reply(BAD_PAYLOAD), None);
        assert_eq!(parse_list_reply(b"[]"), None, "缺 sessions 包装 = 拒收");
    }

    #[test]
    fn replace_keeps_selection_by_name() {
        let mut m = SessionListModel::default();
        let v = parse_list_reply(&sample_reply()).expect("解析");
        m.replace(v.clone());
        assert!(m.select("agent-a"));
        assert_eq!(m.selected(), Some("agent-a"));
        // 重排不影响选中（以会话名为键，r1 R-4）。
        m.replace(v);
        assert_eq!(m.selected(), Some("agent-a"));
        assert_eq!(m.badge_of("agent-a"), Some(AgentBadge::Working));
    }

    #[test]
    fn selection_vanish_clears_and_fallback_input_ready() {
        let mut m = SessionListModel::default();
        let v = parse_list_reply(&sample_reply()).expect("解析");
        m.replace(v);
        assert!(m.select("agent-a"));
        // 选中目标消失（下一拍 LIST 无此会话）⇒ 选中置空；回落由 app 层做
        //（回落不写回，r3 N）——模型只如实反映。
        let gone = serde_json::to_vec(&json!({
            "sessions": [
                {"name":"shell","createdMs":2,"lastActiveMs":900,"attached":false,
                 "agent":"shell","stateV2":"unknown","title":"","cwd":"/y",
                 "cols":80,"rows":24,"pid":12,"clients":[]}
            ]
        }))
        .expect("序列化");
        m.replace(parse_list_reply(&gone).expect("解析"));
        assert_eq!(m.selected(), None, "选中目标消失 = 置空（不擅自换目标）");
        assert_eq!(m.most_recent().map(|s| s.name.as_str()), Some("shell"), "回落链输入 = 最近活跃");
    }

    #[test]
    fn select_rejects_unknown_session() {
        let mut m = SessionListModel::default();
        m.replace(parse_list_reply(&sample_reply()).expect("解析"));
        assert!(!m.select("ghost"), "未知会话拒绝（保持原选中）");
        assert_eq!(m.selected(), None);
    }

    #[test]
    fn local_sentinel_matches_selection_file() {
        assert!(SessionListModel::is_local_host(LOCAL_HOST_ID));
        assert!(!SessionListModel::is_local_host("peer-1"));
    }
}
