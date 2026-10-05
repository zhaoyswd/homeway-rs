//! 控制面契约词汇（语义真源 `baseline:clientcore/facade/vocab.go`——spec
//! daemon-control-plane 冻结的受控枚举：错误码、操作名、订阅域、事件 kind、
//! reach.tier、流 kind 与 stream.end 原因。**只增不改**，新增走后续 spec delta）。
//!
//! 常量在 Rust 侧用 `&str` 常量 + 枚举双形态：词面（wire 值）用常量，闭合集合
//! （op 名）用枚举承载匹配；两者由测试钉死一致。

// ---------- 错误码表（spec「请求/响应与错误码表」，只增不改；注释列 = 是否断连） ----------

pub const CODE_UNKNOWN_OP: &str = "unknown_op"; // 未知操作名（不断连）
pub const CODE_NO_STREAM: &str = "no_stream"; // 未知/已关闭的 streamId（不断连）
pub const CODE_BAD_JSON: &str = "bad_json"; // 控制类 body 非法 JSON（断连）
pub const CODE_BAD_REQUEST: &str = "bad_request"; // 载荷字段缺失/类型不符/值域外（不断连）
pub const CODE_BAD_FRAME: &str = "bad_frame"; // 帧长超限/非法 op（断连；超限在读 body 前）
pub const CODE_NOT_READY: &str = "not_ready"; // 守护进程未就绪（不断连）
pub const CODE_SHUTTING_DOWN: &str = "shutting_down"; // 收工中（不断连）
pub const CODE_HOST_EXISTS: &str = "host_exists"; // 重复添加同后端（不断连）
pub const CODE_NO_HOST: &str = "no_host"; // 主机不存在（不断连）
pub const CODE_BAD_TOKEN: &str = "bad_token"; // token 非法（不断连）
pub const CODE_HOST_UNREACHABLE: &str = "host_unreachable"; // 全不可达且未带 force（不入表；不断连）
pub const CODE_STREAM_REFUSED: &str = "stream_refused"; // 流打开被拒（含在册流超上限；不断连）
pub const CODE_CURSOR_STALE: &str = "cursor_stale"; // 订阅游标过旧/代际失配（resync 语义；不断连）

// ---------- 操作名（spec 初始集，只增不改） ----------
//
// 枚举承载闭合匹配（handler 注册表逐项穷尽）；`as_str` 给 wire 词面。

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum OpName {
    DaemonStatus,
    HostAdd,
    HostRemove,
    HostList,
    SnapshotGet,
    EventsSubscribe,
    EventsUnsubscribe,
    StreamOpen,
    StreamClose,
    ForwardAdd,
    ForwardRemove,
    ForwardList,
    SocksOn,
    SocksOff,
    SocksStatus,
    SpeedtestStart,
    SpeedtestStatus,
    SpeedtestCancel,
    ServeStart,
    ServeStop,
    ServeRestart,
    ServeStatus,
    ServeToken,
    RelayStart,
    RelayStop,
    RelayRestart,
    RelayStatus,
    RelayToken,
}

impl OpName {
    pub const fn as_str(self) -> &'static str {
        match self {
            OpName::DaemonStatus => "daemon.status",
            OpName::HostAdd => "host.add",
            OpName::HostRemove => "host.remove",
            OpName::HostList => "host.list",
            OpName::SnapshotGet => "snapshot.get",
            OpName::EventsSubscribe => "events.subscribe",
            OpName::EventsUnsubscribe => "events.unsubscribe",
            OpName::StreamOpen => "stream.open",
            OpName::StreamClose => "stream.close",
            OpName::ForwardAdd => "forward.add",
            OpName::ForwardRemove => "forward.remove",
            OpName::ForwardList => "forward.list",
            OpName::SocksOn => "socks.on",
            OpName::SocksOff => "socks.off",
            OpName::SocksStatus => "socks.status",
            OpName::SpeedtestStart => "speedtest.start",
            OpName::SpeedtestStatus => "speedtest.status",
            OpName::SpeedtestCancel => "speedtest.cancel",
            OpName::ServeStart => "serve.start",
            OpName::ServeStop => "serve.stop",
            OpName::ServeRestart => "serve.restart",
            OpName::ServeStatus => "serve.status",
            OpName::ServeToken => "serve.token",
            OpName::RelayStart => "relay.start",
            OpName::RelayStop => "relay.stop",
            OpName::RelayRestart => "relay.restart",
            OpName::RelayStatus => "relay.status",
            OpName::RelayToken => "relay.token",
        }
    }

    /// 词面 → 枚举（词表外 = None → `unknown_op` 错误码，不断连）。
    pub fn parse(s: &str) -> Option<OpName> {
        Some(match s {
            "daemon.status" => OpName::DaemonStatus,
            "host.add" => OpName::HostAdd,
            "host.remove" => OpName::HostRemove,
            "host.list" => OpName::HostList,
            "snapshot.get" => OpName::SnapshotGet,
            "events.subscribe" => OpName::EventsSubscribe,
            "events.unsubscribe" => OpName::EventsUnsubscribe,
            "stream.open" => OpName::StreamOpen,
            "stream.close" => OpName::StreamClose,
            "forward.add" => OpName::ForwardAdd,
            "forward.remove" => OpName::ForwardRemove,
            "forward.list" => OpName::ForwardList,
            "socks.on" => OpName::SocksOn,
            "socks.off" => OpName::SocksOff,
            "socks.status" => OpName::SocksStatus,
            "speedtest.start" => OpName::SpeedtestStart,
            "speedtest.status" => OpName::SpeedtestStatus,
            "speedtest.cancel" => OpName::SpeedtestCancel,
            "serve.start" => OpName::ServeStart,
            "serve.stop" => OpName::ServeStop,
            "serve.restart" => OpName::ServeRestart,
            "serve.status" => OpName::ServeStatus,
            "serve.token" => OpName::ServeToken,
            "relay.start" => OpName::RelayStart,
            "relay.stop" => OpName::RelayStop,
            "relay.restart" => OpName::RelayRestart,
            "relay.status" => OpName::RelayStatus,
            "relay.token" => OpName::RelayToken,
            _ => return None,
        })
    }
}

// ---------- reload / goodbye 原因的稳定值（引用错误码表或其子集） ----------

pub const RELOAD_PROTO_MISMATCH: &str = "proto_mismatch";
pub const GOODBYE_OVERRUN: &str = "overrun";

// ---------- reach.tier（spec「命令归属规则」：direct|relay|skipped，只增不改） ----------

pub const REACH_TIER_DIRECT: &str = "direct"; // 有直连端点应答
pub const REACH_TIER_RELAY: &str = "relay"; // 直连全无应答且中继有应答
pub const REACH_TIER_SKIPPED: &str = "skipped"; // force 跳过探测（端点未实测语义）

// ---------- 订阅域词表（spec 初始集，只增不改） ----------

pub const DOMAIN_LINK: &str = "link";
pub const DOMAIN_SESSION: &str = "session";

/// 订阅域词表校验（`events.subscribe` 的 domains 值域）。
pub fn valid_domain(d: &str) -> bool {
    matches!(d, DOMAIN_LINK | DOMAIN_SESSION)
}

// ---------- 事件 kind 与归属（kind → 域 的唯一归属表） ----------

pub const KIND_LINK_CHANGED: &str = "link.changed";
pub const KIND_SESSION_ADDED: &str = "session.added";
pub const KIND_SESSION_REMOVED: &str = "session.removed";
pub const KIND_SESSION_STATE_CHANGED: &str = "session.state_changed";
pub const KIND_SESSION_DIAG: &str = "session.diag";

/// kind → 域 的唯一归属表（Publish 校验 + fixtures 对拍真源）。
pub fn kind_domain(kind: &str) -> Option<&'static str> {
    match kind {
        KIND_LINK_CHANGED => Some(DOMAIN_LINK),
        KIND_SESSION_ADDED | KIND_SESSION_REMOVED | KIND_SESSION_STATE_CHANGED | KIND_SESSION_DIAG => {
            Some(DOMAIN_SESSION)
        }
        _ => None,
    }
}

/// 事件 kind → wire 载荷体（字段表 spec「事件流」初始集，只增不改）。
#[derive(Debug, Clone)]
pub enum EventPayload {
    LinkChanged { host: String, via: String, ep: String, rtt_ms: i64, at: i64 },
    SessionAdded { host: String, name: String, added_at: i64 },
    SessionRemoved { host: String, reason: String },
    SessionStateChanged { host: String, state: String, reason: String },
    SessionDiag { host: String, reason: String },
}

impl EventPayload {
    /// 载荷体构造为 JSON value（字段名/序 = Go struct 序——消费侧按值取，序非契约）。
    pub fn kind(&self) -> &'static str {
        match self {
            EventPayload::LinkChanged { .. } => KIND_LINK_CHANGED,
            EventPayload::SessionAdded { .. } => KIND_SESSION_ADDED,
            EventPayload::SessionRemoved { .. } => KIND_SESSION_REMOVED,
            EventPayload::SessionStateChanged { .. } => KIND_SESSION_STATE_CHANGED,
            EventPayload::SessionDiag { .. } => KIND_SESSION_DIAG,
        }
    }

    pub fn domain(&self) -> &'static str {
        kind_domain(self.kind()).expect("词表内 kind 恒有归属")
    }

    pub fn to_value(&self) -> serde_json::Value {
        use serde_json::json;
        match self {
            EventPayload::LinkChanged { host, via, ep, rtt_ms, at } => json!({
                "host": host, "via": via, "ep": ep, "rttMs": rtt_ms, "at": at,
            }),
            EventPayload::SessionAdded { host, name, added_at } => json!({
                "host": host, "name": name, "addedAt": added_at,
            }),
            EventPayload::SessionRemoved { host, reason } => json!({
                "host": host, "reason": reason,
            }),
            EventPayload::SessionStateChanged { host, state, reason } => json!({
                "host": host, "state": state, "reason": reason,
            }),
            EventPayload::SessionDiag { host, reason } => json!({
                "host": host, "reason": reason,
            }),
        }
    }
}

/// session.removed 的 reason 值域（r2 新-15 契约③：user = 显式摘除、detach = 表收工）。
pub const SESSION_REMOVED_USER: &str = "user";
pub const SESSION_REMOVED_DETACH: &str = "detach";

// ---------- 诊因原因值（spec「诊因事件词表」） ----------

pub const DIAG_GATED: &str = "gated";
pub const DIAG_BUDGET: &str = "budget";
pub const DIAG_PROBE_WINDOW: &str = "probe_window";

// ---------- 流式通道词汇 ----------

/// 流 kind 值域（spec「流式通道」：受控枚举只增——term 为初始集，files 自 files-cli 期起）。
pub const STREAM_KIND_TERM: &str = "term";
pub const STREAM_KIND_FILES: &str = "files";

/// stream.end 的 reason 受控枚举（closed = 对端/前端主动关闭；gone = 目标主机不可达
/// 或主机会话收工——控制面层的本地原因）。
pub const STREAM_END_CLOSED: &str = "closed";
pub const STREAM_END_GONE: &str = "gone";

// ---------- 流腿的服务端口（客户端会话的核内约定；spec「流式通道」：端口 MUST NOT
// 出现在控制面词表——真源 = Go internal/server 的 DefaultTermPort 7724 /
// DefaultFilesPort 7802；一处定源：绑定点经 host 会话的恢复感知拨号携带） ----------

pub const TERM_SERVICE_PORT: u16 = 7724;
pub const FILES_SERVICE_PORT: u16 = 7802;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_names_roundtrip_full_table() {
        // 与 Go facade/vocab.go 的 28 个操作名逐一相等（词面即契约）。
        let all = [
            OpName::DaemonStatus, OpName::HostAdd, OpName::HostRemove, OpName::HostList,
            OpName::SnapshotGet, OpName::EventsSubscribe, OpName::EventsUnsubscribe,
            OpName::StreamOpen, OpName::StreamClose, OpName::ForwardAdd, OpName::ForwardRemove,
            OpName::ForwardList, OpName::SocksOn, OpName::SocksOff, OpName::SocksStatus,
            OpName::SpeedtestStart, OpName::SpeedtestStatus, OpName::SpeedtestCancel,
            OpName::ServeStart, OpName::ServeStop, OpName::ServeRestart, OpName::ServeStatus,
            OpName::ServeToken, OpName::RelayStart, OpName::RelayStop, OpName::RelayRestart,
            OpName::RelayStatus, OpName::RelayToken,
        ];
        for op in all {
            assert_eq!(OpName::parse(op.as_str()), Some(op), "{}", op.as_str());
        }
        assert_eq!(OpName::parse("host.list2"), None);
        assert_eq!(OpName::parse(""), None);
    }

    #[test]
    fn kind_domain_table() {
        assert_eq!(kind_domain(KIND_LINK_CHANGED), Some(DOMAIN_LINK));
        for k in [KIND_SESSION_ADDED, KIND_SESSION_REMOVED, KIND_SESSION_STATE_CHANGED, KIND_SESSION_DIAG] {
            assert_eq!(kind_domain(k), Some(DOMAIN_SESSION));
        }
        assert_eq!(kind_domain("term"), None);
    }
}
