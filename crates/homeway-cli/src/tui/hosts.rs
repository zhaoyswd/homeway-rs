//! 主机状态面（design D4 / tasks 3.1）：`snapshot.get` 初值 + `events.subscribe`
//! 增量的**纯模型**。I/O（ControlClient 接线、重连循环）在 backend——模型只吃
//! [`FeedInput`]，决策以 [`FeedEffect`] 输出，单测可全分支驱动。
//!
//! 语义基准 = `daemon_cli::apply_watch_event`（生产在役的 watch 消费规则）：
//! `session.added` 建行（state=connecting）、`session.removed` 删行、
//! `link.changed` 更新链路读数、`session.state_changed` 更新 state/reason、
//! diag 忽略。差异只有一处：**不复刻 watch 的 `cursor_stale → exit(1)`**——
//! 代际失配/重连 = 全量重取（`ControlUp` ⇒ [`FeedEffect::Resnapshot`]，r2 N-12③）。


// 模型先于接线落地（tasks 3.1 的 backend 接线（ControlClient 喂入循环）在下一批）——期间豁免 dead_code，接线后移除。
#![allow(dead_code)]
use std::collections::BTreeMap;

use homeway_core::daemon::proto::{EventBody, HostState};
use homeway_core::daemon::vocab;

use super::local::LOCAL_HOST_ID;

/// 主机条目（主机列表与状态行共用的视图形态）。
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HostView {
    pub id: String,
    pub name: String,
    pub state: String,
    pub reason: Option<String>,
    /// 连接形态：direct / relay；`none` 与缺失同归 `None`（spec：未知形态显示占位）。
    pub via: Option<String>,
    pub rtt_ms: Option<i64>,
    /// 链路快照时刻（毫秒）——状态行采样年龄的锚（spec：>120s 标陈旧）。
    pub link_at: Option<i64>,
    pub rx_bytes: Option<i64>,
    pub tx_bytes: Option<i64>,
    pub added_at: Option<i64>,
}

impl HostView {
    pub(crate) fn from_state(h: &HostState) -> Self {
        HostView {
            id: h.id.clone(),
            name: h.name.clone().unwrap_or_else(|| h.id.clone()),
            state: h.state.clone(),
            reason: h.reason.clone(),
            via: normalized_via(h.link.as_ref().map(|l| l.via.as_str())),
            rtt_ms: h.link.as_ref().map(|l| l.rtt_ms),
            link_at: h.link.as_ref().map(|l| l.at),
            rx_bytes: h.stats.as_ref().map(|s| s.rx_bytes),
            tx_bytes: h.stats.as_ref().map(|s| s.tx_bytes),
            added_at: h.added_at,
        }
    }
}

/// via 归一：`none` 与缺失同义（渲染为占位，不以误值冒充形态）。
fn normalized_via(via: Option<&str>) -> Option<String> {
    match via {
        Some(v) if v != "none" && !v.is_empty() => Some(v.to_owned()),
        _ => None,
    }
}

/// 模型的全部输入形态（I/O 层把 ControlClient 的往返与事件翻译成这些）。
#[derive(Debug)]
pub(crate) enum FeedInput<'a> {
    /// 全量快照（启动初值 / 代际失配或重连后的重取）。
    Snapshot { generation: &'a str, hosts: &'a [HostState] },
    /// 控制面事件（link/session 域；payload 内 `host` = 主机 id）。
    Event(&'a EventBody),
    /// 控制面断开（daemon 重启/连接死亡）——保留行、标记断开，UI 反映异常。
    ControlDown,
    /// 控制面重连成功（订阅已重建）——需要全量重取刷新行。
    ControlUp,
}

/// 模型对喂入的响应。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum FeedEffect {
    /// 模型已就地更新。
    Applied,
    /// 调用方应立即全量重取快照（`ControlUp` 之后必须跟随一次 Snapshot）。
    Resnapshot,
}

/// 主机模型。行键 = 主机 id（daemon 事件载荷的 `host` 字段）。
#[derive(Default)]
pub(crate) struct HostsModel {
    generation: Option<String>,
    hosts: BTreeMap<String, HostView>,
    control_up: bool,
}

impl HostsModel {
    pub fn feed(&mut self, input: FeedInput<'_>) -> FeedEffect {
        match input {
            FeedInput::Snapshot { generation, hosts } => {
                self.generation = Some(generation.to_owned());
                self.hosts = hosts
                    .iter()
                    .map(HostView::from_state)
                    .map(|h| (h.id.clone(), h))
                    .collect();
                self.control_up = true;
                FeedEffect::Applied
            }
            FeedInput::ControlDown => {
                self.control_up = false;
                FeedEffect::Applied
            }
            // 重连成功但行是旧代的——调用方必须跟随一次全量重取（spec 需求 7）。
            FeedInput::ControlUp => {
                self.control_up = true;
                FeedEffect::Resnapshot
            }
            FeedInput::Event(ev) => {
                self.apply_event(ev);
                FeedEffect::Applied
            }
        }
    }

    fn apply_event(&mut self, ev: &EventBody) {
        let Some(payload) = ev.payload.as_ref() else { return };
        let host = payload["host"].as_str().unwrap_or("").to_owned();
        if host.is_empty() {
            return;
        }
        match ev.kind.as_str() {
            vocab::KIND_SESSION_ADDED => {
                if !self.hosts.contains_key(&host) {
                    self.hosts.insert(
                        host.clone(),
                        HostView {
                            id: host.clone(),
                            name: payload["name"].as_str().unwrap_or("?").to_owned(),
                            state: "connecting".into(),
                            reason: None,
                            via: None,
                            rtt_ms: None,
                            link_at: None,
                            rx_bytes: None,
                            tx_bytes: None,
                            added_at: None,
                        },
                    );
                }
            }
            vocab::KIND_SESSION_REMOVED => {
                self.hosts.remove(&host);
            }
            vocab::KIND_LINK_CHANGED => {
                if let Some(h) = self.hosts.get_mut(&host) {
                    h.via = normalized_via(payload["via"].as_str());
                    h.rtt_ms = payload["rttMs"].as_i64();
                    h.link_at = payload["at"].as_i64();
                }
            }
            vocab::KIND_SESSION_STATE_CHANGED => {
                if let Some(h) = self.hosts.get_mut(&host) {
                    if let Some(s) = payload["state"].as_str() {
                        h.state = s.to_owned();
                    }
                    h.reason = payload["reason"].as_str().map(|s| s.to_owned());
                }
            }
            _ => {} // session.diag / 未知 kind：忽略（与 watch 同口径）
        }
    }

    pub fn generation(&self) -> Option<&str> {
        self.generation.as_deref()
    }

    pub fn control_up(&self) -> bool {
        self.control_up
    }

    pub fn get(&self, id: &str) -> Option<&HostView> {
        self.hosts.get(id)
    }

    /// 主机列表确定序（spec 需求 2 / r3 N）：本机条目优先，其余按名称升序、
    /// 同名按加入先后、再按 id 破平（BTreeMap 遍历序不保证显示序）。
    pub fn display_order_with(&self, local: Option<HostView>) -> Vec<HostView> {
        let mut all: Vec<HostView> = self.hosts.values().cloned().collect();
        all.sort_by(|a, b| {
            a.name
                .cmp(&b.name)
                .then_with(|| a.added_at.cmp(&b.added_at))
                .then_with(|| a.id.cmp(&b.id))
        });
        if let Some(l) = local {
            all.insert(0, l);
        }
        all
    }

    /// 本机条目是否在显示序内（供调用方判断空态——本机不可达且无远端主机）。
    pub fn is_empty_without(&self, local: &HostView) -> bool {
        self.hosts.is_empty() && local.id == LOCAL_HOST_ID
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeway_core::daemon::proto::{HostLink, HostRxTx};
    use serde_json::json;

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

    fn ev(kind: &str, payload: serde_json::Value) -> EventBody {
        EventBody { seq: 1, domain: vocab::kind_domain(kind).unwrap().into(), kind: kind.into(), payload: Some(payload) }
    }

    #[test]
    fn snapshot_replaces_and_sets_generation() {
        let mut m = HostsModel::default();
        let hosts = vec![host("peer-a", "alpha", "ready"), host("peer-b", "beta", "starting")];
        assert_eq!(m.feed(FeedInput::Snapshot { generation: "g1", hosts: &hosts }), FeedEffect::Applied);
        assert_eq!(m.generation(), Some("g1"));
        assert!(m.control_up());
        assert_eq!(m.hosts.len(), 2);
        // 再喂一次全量 = 整表替换（不是合并）。
        let hosts2 = vec![host("peer-a", "alpha", "ready")];
        m.feed(FeedInput::Snapshot { generation: "g1", hosts: &hosts2 });
        assert_eq!(m.hosts.len(), 1, "快照是整表替换");
    }

    #[test]
    fn event_sequence_drives_rows_like_watch() {
        let mut m = HostsModel::default();
        m.feed(FeedInput::Snapshot { generation: "g1", hosts: &[] });
        // added 建行（connecting）
        m.feed(FeedInput::Event(&ev(
            vocab::KIND_SESSION_ADDED,
            json!({"host": "peer-x", "name": "xbox"}),
        )));
        let x = m.get("peer-x").expect("added 建行");
        assert_eq!((x.name.as_str(), x.state.as_str()), ("xbox", "connecting"));
        // link.changed 更新链路（via=none 归 None 占位）
        m.feed(FeedInput::Event(&ev(
            vocab::KIND_LINK_CHANGED,
            json!({"host": "peer-x", "via": "relay", "ep": "1.2.3.4:1", "rttMs": 42, "at": 1000}),
        )));
        let x = m.get("peer-x").unwrap();
        assert_eq!(x.via.as_deref(), Some("relay"));
        assert_eq!(x.rtt_ms, Some(42));
        assert_eq!(x.link_at, Some(1000));
        m.feed(FeedInput::Event(&ev(
            vocab::KIND_LINK_CHANGED,
            json!({"host": "peer-x", "via": "none", "ep": "-", "rttMs": 1, "at": 1001}),
        )));
        assert_eq!(m.get("peer-x").unwrap().via, None, "via=none 归占位");
        // state_changed 更新 state/reason
        m.feed(FeedInput::Event(&ev(
            vocab::KIND_SESSION_STATE_CHANGED,
            json!({"host": "peer-x", "state": "failed", "reason": "拨号超时"}),
        )));
        let x = m.get("peer-x").unwrap();
        assert_eq!((x.state.as_str(), x.reason.as_deref()), ("failed", Some("拨号超时")));
        // removed 删行；未知主机/无载荷事件安全忽略
        m.feed(FeedInput::Event(&ev(vocab::KIND_SESSION_REMOVED, json!({"host": "peer-x", "reason": "user"}))));
        assert!(m.get("peer-x").is_none());
        m.feed(FeedInput::Event(&ev(vocab::KIND_SESSION_DIAG, json!({"host": "ghost", "reason": "?"}))));
        assert!(m.get("ghost").is_none());
    }

    #[test]
    fn control_down_up_cycles_with_resnapshot() {
        let mut m = HostsModel::default();
        let hosts = vec![host("peer-a", "alpha", "ready")];
        m.feed(FeedInput::Snapshot { generation: "g1", hosts: &hosts });
        m.feed(FeedInput::ControlDown);
        assert!(!m.control_up(), "断开标记（行保留供 UI 反映异常）");
        assert_eq!(m.hosts.len(), 1, "断开不删行");
        // 重连成功 ⇒ 必须全量重取（复刻 watch 的 exit(1) 是被明令禁止的形态）。
        assert_eq!(m.feed(FeedInput::ControlUp), FeedEffect::Resnapshot);
        let hosts2 = vec![host("peer-b", "beta", "ready")];
        m.feed(FeedInput::Snapshot { generation: "g2", hosts: &hosts2 });
        assert_eq!(m.generation(), Some("g2"), "重取后代际前移");
        assert!(m.hosts.contains_key("peer-b") && !m.hosts.contains_key("peer-a"));
    }

    #[test]
    fn display_order_local_first_then_name_then_added() {
        let mut m = HostsModel::default();
        let mut late = host("peer-late", "alpha", "ready");
        late.added_at = Some(200);
        let mut early = host("peer-early", "alpha", "ready");
        early.added_at = Some(100);
        let hosts = vec![late, early, host("peer-z", "zulu", "ready")];
        m.feed(FeedInput::Snapshot { generation: "g1", hosts: &hosts });
        let local = compose_local_for_test();
        let ids: Vec<String> = m.display_order_with(Some(local)).into_iter().map(|h| h.id).collect();
        assert_eq!(
            ids,
            ["local", "peer-early", "peer-late", "peer-z"],
            "本机优先 + 名称升序（字节序，字符串 Ord 即契约）+ 同名按加入先后"
        );
    }

    fn compose_local_for_test() -> HostView {
        super::super::local::compose_local("my-mac", super::super::local::ServeState::Running, true)
    }

    #[test]
    fn stats_and_link_from_snapshot() {
        let mut m = HostsModel::default();
        let mut h = host("peer-a", "alpha", "ready");
        h.link = Some(HostLink { via: "direct".into(), ep: "1.2.3.4:1".into(), rtt_ms: 12, at: 500 });
        h.stats = Some(HostRxTx { rx_bytes: 11, tx_bytes: 22 });
        m.feed(FeedInput::Snapshot { generation: "g1", hosts: &[h] });
        let v = m.get("peer-a").unwrap();
        assert_eq!((v.rtt_ms, v.link_at, v.rx_bytes, v.tx_bytes), (Some(12), Some(500), Some(11), Some(22)));
    }
}
