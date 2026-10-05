//! daemon/控制面（B0-2b：GAP-AUDIT P0-1 的剩余整块——Go `internal/daemon` +
//! `internal/control` + facade 表管理的 Rust 对齐）。
//!
//! 分层（依赖方向单向，与 Go 同构）：
//! - `frame`/`vocab`/`proto`：wire 底座（帧封装、冻结词表、JSON body）；
//! - `bus`：进程级事件总线（代际/seq/重放窗/订阅）；
//! - `listen`：control.sock UDS 监听（残留探测 + 权限面）；
//! - `server`：控制面服务器（握手/请求响应/事件推送/流式通道）——对宿主的依赖
//!   收窄为 [`Backend`] trait；
//! - `client`：控制面客户端（CLI 消费面）；
//! - `hosts`：多主机表（hosts.json + 每主机常驻会话）——host.* 的宿主面；
//! - [`DaemonCore`]：统一进程侧的 Backend 装配（hosts + 角色管理面注入）。
//!
//! 承载面 9 op（forward/socks/speedtest 托管）与 supervisor 退避重建（r1-M4）、
//! export/import/reset 的控制面接线归 B0-2 后续棒（挂账见 docs/reviews/B0-2b.md）。

pub mod bus;
pub mod client;
pub mod frame;
pub mod hosts;
pub mod listen;
pub mod proto;
pub mod server;
pub mod vocab;

use std::sync::Arc;

pub use proto::{BackendErr, RoleActionResult, RoleBrief, ServeTokenResult};
pub use server::{ControlServer, ServerConfig};

/// Unix 毫秒（表/事件时间戳的统一取时面）。
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------- 流腿连接（stream.open 的后端面） ----------

/// 流腿的字节流连接抽象（Go net.Conn 的收窄面：daemon 侧纯透传，读写阻塞、
/// close 幂等）。
///
/// 写停滞预算在实现内承载（Go streamUpTimeout 30s / upWorker 停滞 30s 的合并面：
/// 单写无进展上限，超时回 `TimedOut` → 收流 gone）。
pub trait StreamConn: Send + Sync {
    /// 阻塞读一块；Ok(空 Vec) = EOF（对端关）。
    fn read_chunk(&self) -> std::io::Result<Vec<u8>>;
    /// 阻塞写（返回本次推进字节数；预算耗尽/失败 = Err）。
    fn write_chunk(&self, data: &[u8]) -> std::io::Result<usize>;
    /// 关连接（幂等）。
    fn close(&self);
}

/// 隧道端口拨号的流腿适配器（Go streamConn 语义）：恢复感知拨号建连后的
/// `(client, conn_id)` 包装——read 阻塞到数据/EOF，写带背压重试与 30s 无进展预算。
pub struct TunnelConn {
    client: Arc<crate::wgcore::Client>,
    id: u64,
}

impl TunnelConn {
    pub fn new(client: Arc<crate::wgcore::Client>, id: u64) -> TunnelConn {
        TunnelConn { client, id }
    }
}

const WRITE_STALL: std::time::Duration = std::time::Duration::from_secs(30);

impl StreamConn for TunnelConn {
    fn read_chunk(&self) -> std::io::Result<Vec<u8>> {
        use crate::wgcore::ConnErr;
        match self.client.read(self.id) {
            Ok(v) => Ok(v),
            Err(ConnErr::Closed) => Ok(Vec::new()), // 对端 FIN
            Err(e) => Err(std::io::Error::other(e.to_string())),
        }
    }

    fn write_chunk(&self, data: &[u8]) -> std::io::Result<usize> {
        use crate::wgcore::ConnErr;
        let t0 = std::time::Instant::now();
        let mut off = 0usize;
        while off < data.len() {
            match self.client.write(self.id, data[off..].to_vec()) {
                Ok(w) if w.n > 0 => off += w.n,
                Ok(_) => {
                    // 零接纳 = 栈 B 背压：等空位（2ms 节拍；无进展预算兜底）。
                    if t0.elapsed() > WRITE_STALL {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::TimedOut,
                            "隧道写无进展超 30s（后端不读）",
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(2));
                }
                Err(ConnErr::Closed) => return Err(std::io::Error::other("连接已关闭")),
                Err(e) => return Err(std::io::Error::other(e.to_string())),
            }
        }
        Ok(data.len())
    }

    fn close(&self) {
        let _ = self.client.close(self.id);
    }
}

// ---------- serve/relay 角色管理面（宿主注入缝） ----------

/// 角色管理操作（serve/relay 五件对称 ×2；幂等语义在成功载荷呈现，不借道错误码）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoleOp {
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

/// 角色操作产物（serve/relay 的 status/token JSON 形状不同——Value 承载，序列化
/// 字段在装配层保证）。
#[derive(Debug, Clone)]
pub enum RoleOpOut {
    Action(RoleActionResult),
    Status(serde_json::Value),
    Token(ServeTokenResult),
}

/// serve/relay 角色的宿主面（统一进程侧实现——期望态写 config + 动态启停；由
/// CLI 装配层注入，`DaemonCore` 只透传）。`None`（未注入）= 角色面未装配（测试
/// 形态）：状态面给 absent 空形状、启停走 RoleStopped。
pub trait RoleHost: Send + Sync {
    fn role_op(&self, op: RoleOp) -> Result<RoleOpOut, BackendErr>;
    /// 角色状态摘要（daemon.status 的 roles 段）。
    fn statuses(&self) -> Vec<RoleBrief>;
}

// ---------- Backend（server 对宿主的窄依赖面） ----------

/// 控制面服务器的宿主面（全部方法必须可并发调用）。语义在各宿主模块
/// （hosts 表 / RoleHost），本 trait 只做 wire 映射的收窄边界。
pub trait Backend: Send + Sync {
    /// 服务端版本（welcome/daemon.status）。
    fn server_version(&self) -> String;
    /// 角色状态面（daemon.status）。
    fn roles_status(&self) -> Vec<RoleBrief>;
    /// 主机登记面（host.list / snapshot.get 的静态部分）。
    fn host_briefs(&self) -> Vec<proto::HostBrief>;
    /// host.add：decode → 有界旁路探测 → 入表（探测不挂请求预算——客户端先退时
    /// 服务端继续探测并已入表的既有口径）。
    fn add_host(&self, name: &str, token: &str, force: bool) -> Result<proto::HostAddResult, BackendErr>;
    /// host.remove（id = peerID hex）。
    fn remove_host(&self, host_hex: &str) -> Result<(), BackendErr>;
    /// 各主机动态面（state/reason/link/stats）。
    fn host_states(&self) -> Vec<proto::HostState>;
    /// 打开一条到目标主机指定 kind 服务（term=7724 / files=7802——端口映射在此处，
    /// 不进控制面词表）的隧道连接。NoHost = 主机不在表；NoSession = 会话不在
    /// （收工/重建窗口）；其余错误 = 主机不可达（stream_refused）。
    fn dial_stream(&self, kind: &str, host_hex: &str) -> Result<Arc<dyn StreamConn>, BackendErr>;
    /// 宿主未就绪（hosts 表未挂）——host.*/snapshot 类操作报 not_ready。
    fn not_ready(&self) -> bool;
    /// serve/relay 角色管理（宿主面注入）。
    fn role_op(&self, op: RoleOp) -> Result<RoleOpOut, BackendErr>;
}

// ---------- DaemonCore：统一进程侧的 Backend 装配 ----------

/// 统一进程的控制面宿主：hosts 表（client 角色）+ 角色管理面（serve/relay 动态
/// 启停——CLI 装配层注入）+ 进程级事件总线。
pub struct DaemonCore {
    version: String,
    pub bus: Arc<bus::Bus>,
    pub hosts: Arc<hosts::HostTable>,
    roles: Option<Arc<dyn RoleHost>>,
}

impl DaemonCore {
    /// 装配（hosts 表 = client 角色恒开的表语义）。`roles` = None 时角色面呈
    /// absent/RoleStopped（测试形态）。
    pub fn new(
        version: &str,
        client_dir: &std::path::Path,
        identity_dir: &std::path::Path,
        endpoint_cache_dir: &std::path::Path,
        logf: Arc<dyn Fn(&str) + Send + Sync>,
        roles: Option<Arc<dyn RoleHost>>,
    ) -> Result<Arc<DaemonCore>, String> {
        let bus = Arc::new(bus::Bus::new());
        let hosts = hosts::HostTable::open(client_dir, identity_dir, endpoint_cache_dir, logf, Arc::clone(&bus))?;
        hosts.publish_loaded();
        hosts.spawn_event_pump();
        Ok(Arc::new(DaemonCore { version: version.to_owned(), bus, hosts, roles }))
    }

    pub fn bus(&self) -> &Arc<bus::Bus> {
        &self.bus
    }

    /// client 角色收工（停全部会话 + 逐台 session.removed detach）。
    pub fn close(&self) {
        self.hosts.close();
    }
}

impl Backend for DaemonCore {
    fn server_version(&self) -> String {
        self.version.clone()
    }

    fn roles_status(&self) -> Vec<RoleBrief> {
        match &self.roles {
            Some(r) => r.statuses(),
            None => Vec::new(),
        }
    }

    fn host_briefs(&self) -> Vec<proto::HostBrief> {
        self.hosts.briefs()
    }

    fn add_host(&self, name: &str, token: &str, force: bool) -> Result<proto::HostAddResult, BackendErr> {
        self.hosts.add_host(name, token, force)
    }

    fn remove_host(&self, host_hex: &str) -> Result<(), BackendErr> {
        let id = hosts::decode_peer_id_pub(host_hex).ok_or(BackendErr::NoHost)?;
        self.hosts.remove_host(&id)
    }

    fn host_states(&self) -> Vec<proto::HostState> {
        self.hosts.states()
    }

    fn dial_stream(&self, kind: &str, host_hex: &str) -> Result<Arc<dyn StreamConn>, BackendErr> {
        let port = match kind {
            vocab::STREAM_KIND_TERM => vocab::TERM_SERVICE_PORT,
            vocab::STREAM_KIND_FILES => vocab::FILES_SERVICE_PORT,
            other => return Err(BackendErr::BadStreamKind(other.to_owned())),
        };
        let id = hosts::decode_peer_id_pub(host_hex).ok_or(BackendErr::NoHost)?;
        let Some(sess) = self.hosts.session(&id) else {
            // 登记在册但会话对象不在（构造失败/重建窗口）——Go 区分 NoSession
            // （stream_refused）而非 NoHost（hosts-3 整改）。
            return Err(BackendErr::NoSession);
        };
        // 恢复感知的隧道端口拨号（Go healingDial 同义；30s = Go dialTimeout）。
        let conn_id = sess
            .healing_dial_port(port, std::time::Duration::from_secs(30))
            .map_err(|e| match e {
                crate::wgcore::ConnErr::Timeout | crate::wgcore::ConnErr::Refused => {
                    BackendErr::Other(format!("隧道拨号 {kind}（{port}）失败：{e}"))
                }
                crate::wgcore::ConnErr::EngineGone => BackendErr::NoSession,
                other => BackendErr::Other(format!("隧道拨号 {kind}（{port}）失败：{other}")),
            })?;
        Ok(Arc::new(TunnelConn::new(sess.client(), conn_id)))
    }

    fn not_ready(&self) -> bool {
        false // 统一进程形态：hosts 表恒挂（DaemonCore::new 成功即在）
    }

    fn role_op(&self, op: RoleOp) -> Result<RoleOpOut, BackendErr> {
        match &self.roles {
            Some(r) => r.role_op(op),
            None => match op {
                RoleOp::ServeStatus => Ok(RoleOpOut::Status(serde_json::to_value(
                    proto::ServeStatusResult {
                        enabled: false,
                        state: "absent".to_owned(),
                        reason: None,
                        listen_port: None,
                        published: None,
                        token_mask: None,
                        endpoints: None,
                        peers: Vec::new(),
                        ddns: None,
                        intercept: Default::default(),
                    },
                )
                .unwrap())),
                RoleOp::RelayStatus => Ok(RoleOpOut::Status(serde_json::to_value(
                    proto::RelayStatusResult {
                        enabled: false,
                        state: "absent".to_owned(),
                        reason: None,
                        listen: None,
                        advertise: None,
                        token_mask: None,
                        open: false,
                        assocs: 0,
                        backends: Vec::new(),
                    },
                )
                .unwrap())),
                _ => Err(BackendErr::RoleStopped),
            },
        }
    }
}

#[cfg(test)]
mod tests;
