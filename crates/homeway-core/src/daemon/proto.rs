//! 控制面 JSON body 结构（语义真源 `baseline:internal/control/proto.go`）。
//!
//! JSON 面纪律（spec「词表冻结」三层）：解码忽略未知字段（serde 默认行为，全部
//! 结构体不加 deny_unknown_fields）；编码只要求结构等价（键序不敏感）；二进制帧
//! 逐字节冻结由 fixtures 对拍守住（`fixtures/control-cp-v1/frames.jsonl` + 本模块
//! 的对拍测试）。`omitempty` 语义 = `Option` + `skip_serializing_if`。

use serde::{Deserialize, Serialize};
use serde_json::Value;


// ---------- 握手与生命周期 ----------

/// 前端标识（hello 携带；kind 为自由字符串——服务端不校验值域）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct FrontendInfo {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct HelloBody {
    #[serde(rename = "protoVersion", default)]
    pub proto_version: i32,
    #[serde(default)]
    pub frontend: FrontendInfo,
}

/// 握手应答：serverVersion + 代际（每次守护进程启动重新随机）+ serverSeq。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WelcomeBody {
    #[serde(rename = "serverVersion")]
    pub server_version: String,
    pub generation: String,
    #[serde(rename = "serverSeq")]
    pub server_seq: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReloadBody {
    pub reason: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GoodbyeBody {
    pub reason: String,
}

// ---------- 请求/响应 ----------

/// 解码宽松面（Go encoding/json 零值语义——wire-2 整改）：缺 corr = 0（corr=0 是
/// 服务端主动通知的保留值）、缺 op = 空串（→ unknown_op 应答，不断连）。**只有
/// body 不是合法 JSON 才 bad_json 断连**——这是不变量（集成测试钉住）。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RequestBody {
    #[serde(default)]
    pub corr: u64,
    #[serde(default)]
    pub op: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Value>,
}

/// 响应体。`ok=false` 不代表连接有问题——错误码表里只有 bad_json/bad_frame 断连。
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResponseBody {
    pub corr: u64,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// 可行动归因原文（码窄、detail 自由文本——CLI 据此直接给结论）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

/// 操作错误（服务端内部 → wire 映射的载体；客户端侧 = 从 rsp 解出的错误）：
/// 稳定码 + 可行动归因。
#[derive(Debug, Clone, thiserror::Error)]
#[error("{code}{detail}")] // detail 自带「: 」前缀（空则空串）
pub struct OpError {
    pub code: String,
    /// 空 = 无归因；非空以「: 」开头拼进 Display。
    detail: String,
}

impl OpError {
    pub fn code(code: &str) -> Self {
        OpError { code: code.to_owned(), detail: String::new() }
    }

    pub fn with_detail(code: &str, detail: impl Into<String>) -> Self {
        OpError { code: code.to_owned(), detail: format!(": {}", detail.into()) }
    }

    /// 归因原文（不含码与前缀）。
    pub fn detail(&self) -> &str {
        self.detail.strip_prefix(": ").unwrap_or("")
    }
}

/// 后端哨兵错误族（server 层映射到错误码表；Go control.Backend 哨兵的 Rust 面）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BackendErr {
    #[error("host 已在表中")]
    HostExists,
    #[error("token 非法：{0}")]
    BadToken(String),
    #[error("host 不在表中")]
    NoHost,
    #[error("host 全不可达（探测无应答且未带 force）")]
    HostUnreachable,
    #[error("会话不在（收工/重建窗口）")]
    NoSession,
    #[error("角色未在运行（先 start）")]
    RoleStopped,
    #[error("流 kind 值域外：{0}")]
    BadStreamKind(String),
    /// 其余错误（值域外/冲突/监听失败/落盘失败等）→ bad_request + 归因。
    #[error("{0}")]
    Other(String),
}

// ---------- host 面 ----------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HostAddArgs {
    #[serde(default)]
    pub name: Option<String>,
    pub token: String,
    #[serde(default)]
    pub force: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReachTested {
    pub ep: String,
    pub relay: bool,
    #[serde(rename = "rttMs")]
    pub rtt_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostReach {
    pub tier: String,
    #[serde(rename = "bestEp", skip_serializing_if = "Option::is_none")]
    pub best_ep: Option<String>,
    #[serde(rename = "rttMs", skip_serializing_if = "Option::is_none")]
    pub rtt_ms: Option<i64>,
    pub tested: Vec<ReachTested>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostAddResult {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "addedAt")]
    pub added_at: i64,
    pub reach: HostReach,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HostRemoveArgs {
    #[serde(default)]
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostBrief {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(rename = "addedAt")]
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HostListResult {
    pub hosts: Vec<HostBrief>,
}

// ---------- 订阅面 ----------

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SubscribeArgs {
    pub domains: Option<Vec<String>>,
    #[serde(default)]
    pub cursor: Option<u64>,
    #[serde(default)]
    pub view: Option<String>,
    #[serde(default)]
    pub generation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscribeResult {
    pub domains: Vec<String>,
    pub cursor: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    pub generation: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UnsubscribeArgs {
    pub domains: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsubscribeResult {
    pub domains: Vec<String>,
}

// ---------- 流式通道 ----------

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct StreamOpenArgs {
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub host: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamOpenResult {
    #[serde(rename = "streamId")]
    pub stream_id: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct StreamCloseArgs {
    #[serde(rename = "streamId")]
    pub stream_id: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEndBody {
    #[serde(rename = "streamId")]
    pub stream_id: u32,
    pub reason: String,
}

// ---------- 事件帧 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventBody {
    pub seq: u64,
    pub domain: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,
}

// ---------- daemon.status / snapshot.get ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleBrief {
    pub name: String,
    pub state: String,
    pub restarts: i32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostLink {
    pub via: String,
    pub ep: String,
    #[serde(rename = "rttMs")]
    pub rtt_ms: i64,
    pub at: i64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HostRxTx {
    #[serde(rename = "rxBytes")]
    pub rx_bytes: i64,
    #[serde(rename = "txBytes")]
    pub tx_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostState {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub link: Option<HostLink>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stats: Option<HostRxTx>,
    #[serde(rename = "addedAt", skip_serializing_if = "Option::is_none")]
    pub added_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HostDemandBrief {
    pub host: String,
    pub active: bool,
    pub reason: String,
    pub at: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatusResult {
    #[serde(rename = "serverVersion")]
    pub server_version: String,
    pub generation: String,
    pub seq: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<i32>,
    pub roles: Vec<RoleBrief>,
    pub hosts: Vec<HostState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub demand: Option<Vec<HostDemandBrief>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SnapshotResult {
    pub seq: u64,
    pub generation: String,
    pub hosts: Vec<HostState>,
}

// ---------- serve/relay 角色管理 ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoleActionResult {
    pub action: String, // started | already | stopped | already | restarted
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServePeerBrief {
    pub dev: String,
    #[serde(rename = "tunnelIp")]
    pub tunnel_ip: String,
    #[serde(rename = "lastReg")]
    pub last_reg: i64,
    #[serde(rename = "idleMs")]
    pub idle_ms: i64,
}

#[derive(Debug, Clone, Default, Copy, Serialize, Deserialize)]
pub struct ServeInterceptBits {
    #[serde(rename = "dialOk")]
    pub dial_ok: u64,
    #[serde(rename = "dialFail")]
    pub dial_fail: u64,
    pub reject: u64,
    pub flows: u64,
    /// Q-B 新增丢弃计数（F3/F4/F7/F10；additive——旧载荷缺省 0）。
    #[serde(rename = "udpDrop", default)]
    pub udp_drop: u64,
    #[serde(rename = "shapeDrop", default)]
    pub shape_drop: u64,
    #[serde(rename = "fragDrop", default)]
    pub frag_drop: u64,
}

/// 出口 QUIC 面的计数快照（M3 S2；M2 交下的 L4 项——`serve status --json` 的**平级
/// additive 段**，面未起 = 整段缺席 ⇒ 旧载荷/旧读者零影响）。
///
/// 键名 = `homeway-quic` 的 `ExitQuicSnapshot` 字段名逐字（**单源**：`from_snapshot` 是
/// 唯一构造面，加字段即编译红——防两处键表漂移）。口径与行内自带数同源（M2 §15-1）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServeQuicBits {
    pub connections: u64,
    pub admitted: u64,
    #[serde(rename = "pathChanges")]
    pub path_changes: u64,
    #[serde(rename = "handshakeFailed")]
    pub handshake_failed: u64,
    #[serde(rename = "handshakePeerClosed")]
    pub handshake_peer_closed: u64,
    #[serde(rename = "handshakesInFlight")]
    pub handshakes_in_flight: u64,
    #[serde(rename = "handshakeRefused")]
    pub handshake_refused: u64,
    #[serde(rename = "connRefused")]
    pub conn_refused: u64,
    #[serde(rename = "handshakeTimeouts")]
    pub handshake_timeouts: u64,
    #[serde(rename = "regsAccepted")]
    pub regs_accepted: u64,
    #[serde(rename = "regsRejected")]
    pub regs_rejected: u64,
    #[serde(rename = "challengesIssued")]
    pub challenges_issued: u64,
    #[serde(rename = "challengesRefused")]
    pub challenges_refused: u64,
    #[serde(rename = "proofRejected")]
    pub proof_rejected: u64,
    #[serde(rename = "pendingExpired")]
    pub pending_expired: u64,
    #[serde(rename = "admitTimeouts")]
    pub admit_timeouts: u64,
    #[serde(rename = "retrySent")]
    pub retry_sent: u64,
    #[serde(rename = "floodRefused")]
    pub flood_refused: u64,
    #[serde(rename = "proofCooldowns")]
    pub proof_cooldowns: u64,
    #[serde(rename = "dropTooLarge")]
    pub drop_too_large: u64,
    #[serde(rename = "dropSendBufferFull")]
    pub drop_send_buffer_full: u64,
    #[serde(rename = "dropUnregistered")]
    pub drop_unregistered: u64,
    #[serde(rename = "dropSrcRejected")]
    pub drop_src_rejected: u64,
    #[serde(rename = "streamsOpen")]
    pub streams_open: u64,
    #[serde(rename = "streamRefused")]
    pub stream_refused: u64,
    #[serde(rename = "streamsClosed")]
    pub streams_closed: u64,
    #[serde(rename = "streamBytesIn")]
    pub stream_bytes_in: u64,
    #[serde(rename = "streamBytesOut")]
    pub stream_bytes_out: u64,
}

impl ServeQuicBits {
    /// 出口快照 → 状态段（**唯一构造面**；字段一对一，加字段即此处编译红）。
    pub fn from_snapshot(s: &homeway_quic::ExitQuicSnapshot) -> Self {
        Self {
            connections: s.connections,
            admitted: s.admitted,
            path_changes: s.path_changes,
            handshake_failed: s.handshake_failed,
            handshake_peer_closed: s.handshake_peer_closed,
            handshakes_in_flight: s.handshakes_in_flight,
            handshake_refused: s.handshake_refused,
            conn_refused: s.conn_refused,
            handshake_timeouts: s.handshake_timeouts,
            regs_accepted: s.regs_accepted,
            regs_rejected: s.regs_rejected,
            challenges_issued: s.challenges_issued,
            challenges_refused: s.challenges_refused,
            proof_rejected: s.proof_rejected,
            pending_expired: s.pending_expired,
            admit_timeouts: s.admit_timeouts,
            retry_sent: s.retry_sent,
            flood_refused: s.flood_refused,
            proof_cooldowns: s.proof_cooldowns,
            drop_too_large: s.drop_too_large,
            drop_send_buffer_full: s.drop_send_buffer_full,
            drop_unregistered: s.drop_unregistered,
            drop_src_rejected: s.drop_src_rejected,
            streams_open: s.streams_open,
            stream_refused: s.stream_refused,
            streams_closed: s.streams_closed,
            stream_bytes_in: s.stream_bytes_in,
            stream_bytes_out: s.stream_bytes_out,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeStatusResult {
    pub enabled: bool,
    pub state: String, // running|stopping|stopped|failed|absent
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(rename = "listenPort", skip_serializing_if = "Option::is_none")]
    pub listen_port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub published: Option<Vec<String>>,
    #[serde(rename = "tokenMask", skip_serializing_if = "Option::is_none")]
    pub token_mask: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Vec<String>>,
    pub peers: Vec<ServePeerBrief>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ddns: Option<Vec<serde_json::Value>>,
    pub intercept: ServeInterceptBits,
    /// 出口 QUIC 面计数（M3 S2 的 additive 平级段；**面未起/未装配 = 整段缺席**——
    /// 旧载荷与旧读者逐字节零影响）。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quic: Option<ServeQuicBits>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServeTokenResult {
    pub token: String,
    pub source: String, // runtime | ledger | derived
    #[serde(rename = "endpoints", skip_serializing_if = "Option::is_none")]
    pub eps: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayBackendBrief {
    pub label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub addr: Option<String>,
    #[serde(rename = "lastActive")]
    pub last_active: i64,
    pub verified: bool,
    #[serde(rename = "ctlVerified")]
    pub ctl_verified: bool,
    #[serde(rename = "hasCtl")]
    pub has_ctl: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayStatusResult {
    pub enabled: bool,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub listen: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub advertise: Option<String>,
    #[serde(rename = "tokenMask", skip_serializing_if = "Option::is_none")]
    pub token_mask: Option<String>,
    pub open: bool,
    pub assocs: i32,
    pub backends: Vec<RelayBackendBrief>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelayTokenResult {
    pub token: String,
    pub source: String,
    #[serde(rename = "endpoints", skip_serializing_if = "Option::is_none")]
    pub eps: Option<Vec<String>>,
}

// ---------- forward / socks / speedtest（承载面 wire 体；语义真源
// `baseline:internal/control/proto.go`——3e 只增，design D6） ----------
//
// host 载荷 = peerID hex（CLI 侧 resolve 先行解析，同 term/files）。
// 错误码零新增：值域外/冲突 = bad_request、主机不在表 = no_host（用例锁死映射）。

/// forward.add 载荷。target_ip 空 = 出口自己；target_port 0 = 同监听端口。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ForwardAddArgs {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub listen: u16,
    #[serde(rename = "targetIp", default, skip_serializing_if = "String::is_empty")]
    pub target_ip: String,
    #[serde(rename = "targetPort", default, skip_serializing_if = "is_zero_u16")]
    pub target_port: u16,
}

fn is_zero_u16(v: &u16) -> bool {
    *v == 0
}

/// forward.add 成功载荷（建成的规则面，listening 态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardRuleBrief {
    pub host: String,
    pub listen: u16,
    #[serde(rename = "targetIp", default, skip_serializing_if = "String::is_empty")]
    pub target_ip: String,
    #[serde(rename = "targetPort", default, skip_serializing_if = "is_zero_u16")]
    pub target_port: u16,
    pub state: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub err: String,
    pub conns: i32,
    /// 超并发上限被拒计数。
    #[serde(default, skip_serializing_if = "is_zero_i32")]
    pub rejected: i32,
}

fn is_zero_i32(v: &i32) -> bool {
    *v == 0
}

/// forward.add 成功载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardAddResult {
    pub rule: ForwardRuleBrief,
}

/// forward.remove 载荷。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ForwardRemoveArgs {
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub listen: u16,
}

/// forward.remove 成功载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardRemoveResult {
    pub removed: bool,
}

/// forward.list 载荷（host 空 = 全部）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ForwardListArgs {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub host: String,
}

/// forward.list 成功载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForwardListResult {
    pub forwards: Vec<ForwardRuleBrief>,
}

/// socks.on 载荷（listen 0 = 沿用记忆端口，无记忆则 1080）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SocksOnArgs {
    #[serde(default)]
    pub host: String,
    #[serde(default, skip_serializing_if = "is_zero_u16")]
    pub listen: u16,
}

/// socks.on 成功载荷（实际监听端口）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocksOnResult {
    pub listen: u16,
}

/// socks.off 载荷。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SocksOffArgs {
    #[serde(default)]
    pub host: String,
}

/// socks.off 成功载荷（listen = 记忆保留的端口，下次 on 缺省沿用）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocksOffResult {
    pub listen: u16,
}

/// socks.status 单条（off 也出现——listen = 记住的端口）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocksBrief {
    pub host: String,
    pub on: bool,
    pub listen: u16,
    pub conns: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub err: String,
}

/// socks.status 成功载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SocksStatusResult {
    pub socks: Vec<SocksBrief>,
}

/// speedtest.start 载荷（0 = 手机口径默认；waitMs 0 = 不等——start 立即返回
/// waiting 相位、等待由 runner 状态机承载）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SpeedtestStartArgs {
    #[serde(default)]
    pub host: String,
    #[serde(rename = "downMs", default, skip_serializing_if = "is_zero_i64")]
    pub down_ms: i64,
    #[serde(rename = "upMs", default, skip_serializing_if = "is_zero_i64")]
    pub up_ms: i64,
    #[serde(rename = "warmupMs", default, skip_serializing_if = "is_zero_i64")]
    pub warmup_ms: i64,
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub streams: i64,
    #[serde(rename = "waitMs", default, skip_serializing_if = "is_zero_i64")]
    pub wait_ms: i64,
}

fn is_zero_i64(v: &i64) -> bool {
    *v == 0
}

/// speedtest.start 成功载荷（waiting 或 busy——busy 是载荷里的 reason，同手机信封
/// 形态、不占错误码表）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedtestStartAck {
    /// waiting | busy。
    pub phase: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
}

/// speedtest.status 载荷。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SpeedtestStatusArgs {
    #[serde(default)]
    pub host: String,
}

/// speedtest 终态结果（镜像引擎 Result 字段名——与手机信封同名，CLI --json 对拍真源）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedtestResultBrief {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub msg: String,
    #[serde(rename = "downBps", default, skip_serializing_if = "is_zero_f64")]
    pub down_bps: f64,
    #[serde(rename = "upBps", default, skip_serializing_if = "is_zero_f64")]
    pub up_bps: f64,
    #[serde(rename = "usageDown", default, skip_serializing_if = "is_zero_i64")]
    pub usage_down: i64,
    #[serde(rename = "usageUp", default, skip_serializing_if = "is_zero_i64")]
    pub usage_up: i64,
    #[serde(rename = "wallMs", default, skip_serializing_if = "is_zero_i64")]
    pub wall_ms: i64,
}

fn is_zero_f64(v: &f64) -> bool {
    *v == 0.0
}

/// speedtest.status 成功载荷：waiting 相位（waitRemainMs）或引擎快照（phase/dir/
/// instBps/usage/bytes/elapsedMs——与 Go 手机 Status 信封同面）；轮次到终态时
/// result 携带完整结果（None = 未到终态）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedtestStatusResult {
    pub host: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub waiting: bool,
    #[serde(rename = "waitRemainMs", default, skip_serializing_if = "is_zero_i64")]
    pub wait_remain_ms: i64,
    pub phase: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub reason: String,
    #[serde(rename = "usageDown", default, skip_serializing_if = "is_zero_i64")]
    pub usage_down: i64,
    #[serde(rename = "usageUp", default, skip_serializing_if = "is_zero_i64")]
    pub usage_up: i64,
    /// 相位方向（down/up——Go CLI 的 st.Dir 直读面，中-3）。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub dir: String,
    /// 当前相位累计字节。
    #[serde(default, skip_serializing_if = "is_zero_i64")]
    pub bytes: i64,
    /// 相位瞬时速率（daemon 侧差分——Go CLI 的 st.InstBps 直读面）。
    #[serde(rename = "instBps", default, skip_serializing_if = "is_zero_f64")]
    pub inst_bps: f64,
    #[serde(rename = "elapsedMs", default, skip_serializing_if = "is_zero_i64")]
    pub elapsed_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<SpeedtestResultBrief>,
}

/// speedtest.cancel 载荷（只作用指定主机，不波及轮转）。
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SpeedtestCancelArgs {
    #[serde(default)]
    pub host: String,
}

/// speedtest.cancel 成功载荷。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeedtestCancelResult {
    pub cancelled: bool,
}

// ---------- 编解码助手 ----------

/// 帧化一个 JSON body（serde 序列化 + 帧封装；body 类型必须无序列化失败面——
/// 全部为普通值类型，`expect` 按不可达处理）。
pub fn encode_json_frame(op: super::frame::Op, body: &impl Serialize) -> Vec<u8> {
    let b = serde_json::to_vec(body).expect("控制面 body 序列化不可失败（普通值类型）");
    super::frame::encode_frame(op, &b)
}

/// already 序列化好的 body（如缓存的事件帧）原样帧化。
pub fn encode_raw_frame(op: super::frame::Op, body: &[u8]) -> Vec<u8> {
    super::frame::encode_frame(op, body)
}

/// rsp 的统一构造：err 为 None 时 ok=true + result。
pub fn response_body(corr: u64, result: Option<Value>, err: Option<&OpError>) -> ResponseBody {
    match err {
        Some(e) => ResponseBody {
            corr,
            ok: false,
            error: Some(e.code.clone()),
            detail: (!e.detail().is_empty()).then(|| e.detail().to_owned()),
            result: None,
        },
        None => ResponseBody { corr, ok: true, error: None, detail: None, result },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::vocab;

    #[test]
    fn response_omitempty_matches_go_shape() {
        // 错误 rsp：Go 形态 = {"corr":9,"ok":false,"error":"unknown_op"}（无 result/detail）。
        let b = response_body(9, None, Some(&OpError::code(vocab::CODE_UNKNOWN_OP)));
        let s = serde_json::to_string(&b).unwrap();
        assert_eq!(s, r#"{"corr":9,"ok":false,"error":"unknown_op"}"#);
        // 带归因：detail 在 error 后。
        let b = response_body(9, None, Some(&OpError::with_detail(vocab::CODE_BAD_REQUEST, "端口被占")));
        let s = serde_json::to_string(&b).unwrap();
        assert_eq!(s, r#"{"corr":9,"ok":false,"error":"bad_request","detail":"端口被占"}"#);
        // 成功 rsp：ok=true + result。
        let b = response_body(7, Some(serde_json::json!({"seq": 42})), None);
        let s = serde_json::to_string(&b).unwrap();
        assert_eq!(s, r#"{"corr":7,"ok":true,"result":{"seq":42}}"#);
    }

    /// **判据（M3 S2：出口 `serve status --json` 的 `quic` 段，M2 交下的 L4 项）**：
    /// ① 段未装配（`None`）⇒ **整段缺席**（旧载荷逐字节零影响）；
    /// ② 段在 ⇒ 键集与出口快照字段**一对一**（`from_snapshot` 是唯一构造面）；
    /// ③ 计数原样透传（判据：同一份 `ExitQuicSnapshot` 读两次的读数一致）。
    #[test]
    fn serve_status_quic_segment_is_additive_and_one_to_one() {
        let mut r = ServeStatusResult {
            enabled: true,
            state: "running".to_owned(),
            reason: None,
            listen_port: Some(42641),
            published: None,
            token_mask: None,
            endpoints: None,
            peers: Vec::new(),
            ddns: None,
            intercept: ServeInterceptBits::default(),
            quic: None,
        };
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        assert!(v.get("quic").is_none(), "面未起 ⇒ 整段缺席：{v}");

        let s = homeway_quic::ExitQuicSnapshot {
            connections: 2,
            admitted: 9,
            handshake_failed: 3,
            challenges_issued: 7,
            regs_accepted: 5,
            regs_rejected: 1,
            streams_open: 4,
            stream_refused: 2,
            streams_closed: 3,
            stream_bytes_in: 1024,
            stream_bytes_out: 2048,
            ..Default::default()
        };
        r.quic = Some(ServeQuicBits::from_snapshot(&s));
        let v: serde_json::Value = serde_json::to_value(&r).unwrap();
        let q = v.get("quic").expect("段在");
        assert_eq!(q["connections"], 2);
        assert_eq!(q["admitted"], 9);
        assert_eq!(q["pathChanges"], 0, "缺省 0（字段一对一，不留洞）");
        assert_eq!(q["streamsOpen"], 4);
        assert_eq!(q["streamRefused"], 2);
        assert_eq!(q["streamsClosed"], 3);
        assert_eq!(q["streamBytesIn"], 1024);
        assert_eq!(q["streamBytesOut"], 2048);
        // 键数 = 快照字段数（加字段忘了映射 = 本断言红）
        let n = q.as_object().unwrap().len();
        assert_eq!(n, 28, "键数 = ExitQuicSnapshot 字段数（实得 {n}）：{q}");
        // 反序列化回环（旧载荷无 quic 键也能读：additive）
        let back: ServeStatusResult = serde_json::from_value(v).unwrap();
        assert_eq!(back.quic.unwrap().streams_open, 4);
        let legacy: ServeStatusResult =
            serde_json::from_str(r#"{"enabled":true,"state":"running","peers":[],"intercept":{"dialOk":0,"dialFail":0,"reject":0,"flows":0}}"#)
                .unwrap();
        assert!(legacy.quic.is_none(), "旧载荷零影响");
    }

    #[test]
    fn hello_decode_ignores_unknown_fields() {
        let raw = br#"{"protoVersion":1,"frontend":{"kind":"cli","name":"homeway","version":"0.5.0","extra":1},"junk":true}"#;
        let h: HelloBody = serde_json::from_slice(raw).unwrap();
        assert_eq!(h.proto_version, 1);
        assert_eq!(h.frontend.kind, "cli");
    }

    #[test]
    fn subscribe_args_domains_optional_semantics() {
        // 缺 domains 键 = None（→ bad_request）；空数组 = Some(vec![])（合法——回显空集）。
        let a: SubscribeArgs = serde_json::from_str("{}").unwrap();
        assert!(a.domains.is_none());
        let a: SubscribeArgs = serde_json::from_str(r#"{"domains":[]}"#).unwrap();
        assert_eq!(a.domains.as_deref(), Some(&[][..]));
    }

    #[test]
    fn operror_display() {
        assert_eq!(OpError::code("bad_request").to_string(), "bad_request");
        assert_eq!(OpError::with_detail("bad_request", "x").to_string(), "bad_request: x");
        assert_eq!(OpError::with_detail("bad_request", "x").detail(), "x");
    }
}
