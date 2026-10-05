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
//! - `carriers`：承载面管理器束（forward/socks/speedtest 托管——D-1 实装）；
//! - [`DaemonCore`]：统一进程侧的 Backend 装配（hosts + 承载面 + 角色管理面注入）。

pub mod bus;
pub mod carriers;
pub mod client;
pub mod frame;
pub mod hosts;
pub mod listen;
pub mod proto;
pub mod server;
pub mod vocab;

use std::sync::Arc;

pub use carriers::{
    CarrierErr, ForwardRule, ForwardState, SocksState, SpeedtestOutcome, SpeedtestParams,
    SpeedtestStatus, SOCKS_DEFAULT_LISTEN,
};
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
    /// 半关写端（FIN；对端仍可发——承载面 pipe 的半关闭透传用）。不支持半关的
    /// 实现退化为全关（netpipe.Both 的「对端不支持则退化为 Close」同款）。
    fn shutdown_write(&self);
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

    fn shutdown_write(&self) {
        // 半关（FIN）：对端仍可发——Client::shutdown 是引擎原生半关面。
        let _ = self.client.shutdown(self.id);
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
    // ---------- 承载面（forward/socks/speedtest 托管——D-1） ----------
    /// forward.add（全局端口检查 + 成员检查 + 当场监听；失败不入表）。缺省实现 =
    /// 未装配（测试形态；生产宿主 DaemonCore 必实装）。
    fn forward_add(&self, rule: ForwardRule) -> Result<ForwardState, BackendErr> {
        let _ = rule;
        Err(BackendErr::Other("承载面未装配".to_owned()))
    }
    /// forward.remove（不强关在世连接）。
    fn forward_remove(&self, host_hex: &str, listen: u16) -> Result<(), BackendErr> {
        let _ = (host_hex, listen);
        Err(BackendErr::Other("承载面未装配".to_owned()))
    }
    /// forward.list（host 空 = 全部）。
    fn forward_list(&self, host_hex: &str) -> Vec<ForwardState> {
        let _ = host_hex;
        Vec::new()
    }
    /// socks.on（listen 0 = 记忆/缺省）；返回实际端口。
    fn socks_on(&self, host_hex: &str, listen: u16) -> Result<u16, BackendErr> {
        let _ = (host_hex, listen);
        Err(BackendErr::Other("承载面未装配".to_owned()))
    }
    /// socks.off（显式关在世连接；端口记忆保留）；返回记忆端口。
    fn socks_off(&self, host_hex: &str) -> Result<u16, BackendErr> {
        let _ = host_hex;
        Err(BackendErr::Other("承载面未装配".to_owned()))
    }
    /// socks.status（各主机承载态）。
    fn socks_states(&self) -> Vec<SocksState> {
        Vec::new()
    }
    /// speedtest.start（per-host 单飞；立即返回 waiting/busy 相位）。host 不在表 =
    /// no_host 错误（Go hostInTable 门同义）。
    fn speedtest_start(
        &self,
        host_hex: &str,
        p: SpeedtestParams,
    ) -> Result<carriers::speedrun::SpeedtestAck, BackendErr> {
        let _ = (host_hex, p);
        Err(BackendErr::Other("承载面未装配".to_owned()))
    }
    /// speedtest.status（无运行面 = None——CLI 判「运行面丢失」的形态）。
    fn speedtest_status(&self, host_hex: &str) -> Result<Option<SpeedtestStatus>, BackendErr> {
        let _ = host_hex;
        Ok(None)
    }
    /// speedtest.cancel（幂等）。
    fn speedtest_cancel(&self, host_hex: &str) -> Result<(), BackendErr> {
        let _ = host_hex;
        Ok(())
    }
}

// ---------- DaemonCore：统一进程侧的 Backend 装配 ----------

/// 统一进程的控制面宿主：hosts 表（client 角色）+ 承载面（forward/socks/speedtest
/// 托管——随表生命周期、stateDir 同源）+ 角色管理面（serve/relay 动态启停——CLI
/// 装配层注入）+ 进程级事件总线。
pub struct DaemonCore {
    version: String,
    pub bus: Arc<bus::Bus>,
    pub hosts: Arc<hosts::HostTable>,
    carriers: Arc<carriers::Carriers>,
    roles: Option<Arc<dyn RoleHost>>,
}

impl DaemonCore {
    /// 装配（hosts 表 = client 角色恒开的表语义；承载面随表同源同生命周期）。
    /// `roles` = None 时角色面呈 absent/RoleStopped（测试形态）。
    pub fn new(
        version: &str,
        client_dir: &std::path::Path,
        identity_dir: &std::path::Path,
        endpoint_cache_dir: &std::path::Path,
        logf: Arc<dyn Fn(&str) + Send + Sync>,
        warnf: Arc<dyn Fn(&str) + Send + Sync>,
        roles: Option<Arc<dyn RoleHost>>,
    ) -> Result<Arc<DaemonCore>, String> {
        let bus = Arc::new(bus::Bus::new());
        let hosts = hosts::HostTable::open(client_dir, identity_dir, endpoint_cache_dir, Arc::clone(&logf), Arc::clone(&bus))?;
        hosts.publish_loaded();
        hosts.spawn_event_pump();
        // 承载面拨号缝（Host 面：查表 + healing 拨号——同记账同重建感知；表外主机
        // = NoHost——绑定层沿 no_host 族映射）。
        let dial = carriers_dial_of(&hosts);
        let host_exists: carriers::HostExists = {
            let hosts = Arc::clone(&hosts);
            Arc::new(move |id: &[u8; 32]| hosts.session(id).is_some() || hosts.has_record(id))
        };
        let cars = carriers::Carriers::open(client_dir, dial, logf, warnf, host_exists)?;
        Ok(Arc::new(DaemonCore { version: version.to_owned(), bus, hosts, carriers: cars, roles }))
    }

    pub fn bus(&self) -> &Arc<bus::Bus> {
        &self.bus
    }

    /// client 角色收工（先收承载面——socks 显式关在世连接/speedtest 取消——再停
    /// 全部会话 + 逐台 session.removed detach）。
    pub fn close(&self) {
        self.carriers.close();
        self.hosts.close();
    }
}

/// 承载面拨号缝的表侧实现（Go carrierDialOf：Host 查表 + healing 拨号；预算 =
/// Go DialPort 的 15s 档）。
fn carriers_dial_of(hosts: &Arc<hosts::HostTable>) -> carriers::CarrierDial {
    let h1 = Arc::clone(hosts);
    let dial_port = Arc::new(
        move |host: &str, port: u16, budget: std::time::Duration| -> Result<carriers::CarrierConn, carriers::DialErr> {
            let id = hosts::decode_peer_id_pub(host).ok_or(carriers::DialErr::NoHost)?;
            let sess = h1.session(&id).ok_or(carriers::DialErr::NoSession)?;
            let conn_id =
                sess.healing_dial_port(port, budget.min(std::time::Duration::from_secs(15)))
                    .map_err(map_dial_err)?;
            Ok(carrier_conn_of(sess.client(), conn_id))
        },
    );
    let h2 = Arc::clone(hosts);
    let dial = Arc::new(
        move |host: &str,
              dst: std::net::SocketAddrV4,
              budget: std::time::Duration|
              -> Result<carriers::CarrierConn, carriers::DialErr> {
            let id = hosts::decode_peer_id_pub(host).ok_or(carriers::DialErr::NoHost)?;
            let sess = h2.session(&id).ok_or(carriers::DialErr::NoSession)?;
            let conn_id =
                sess.healing_dial_addr(dst, budget.min(std::time::Duration::from_secs(15)))
                    .map_err(map_dial_err)?;
            Ok(carrier_conn_of(sess.client(), conn_id))
        },
    );
    carriers::CarrierDial { dial_port, dial }
}

fn map_dial_err(e: crate::wgcore::ConnErr) -> carriers::DialErr {
    match e {
        crate::wgcore::ConnErr::Refused => carriers::DialErr::Refused,
        crate::wgcore::ConnErr::EngineGone => carriers::DialErr::NoSession,
        other => carriers::DialErr::Other(other.to_string()),
    }
}

/// 同一隧道连接的两种承载面（TunnelConn 透传腿 + speedtest 引擎腿）。
fn carrier_conn_of(client: Arc<crate::wgcore::Client>, id: u64) -> carriers::CarrierConn {
    carriers::CarrierConn {
        io: Arc::new(TunnelConn::new(Arc::clone(&client), id)),
        speed: crate::speedtest::engine_conn(client, id),
    }
}

fn carrier_err_to_backend(e: CarrierErr) -> BackendErr {
    let detail = e.to_string();
    match e {
        CarrierErr::NoHost => BackendErr::NoHost,
        _ => BackendErr::Other(detail),
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
        // 级联键规范化（低-3）：承载面按字符串比对 host——用解码后重编码的
        // canonical 小写 hex（大写形态的请求不级联漏删）。
        let canonical: String = id.iter().map(|b| format!("{b:02x}")).collect();
        // FIX-05：级联与承载面的「成员检查 + 落地」同锁（add_mu）串行——级联要么
        // 看到规则并删掉，要么规则因成员检查失败而根本落不了地（锁序恒为
        // add_mu → hosts.inner，无反向取锁面）。
        let _cascade = self.carriers.cascade_lock();
        self.hosts.remove_host(&id)?;
        self.carriers.remove_host_cascade_locked(&canonical);
        Ok(())
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

    // ---------- 承载面（语义在 carriers；本层只做 host hex 存在性闸 + 委托） ----------

    fn forward_add(&self, rule: ForwardRule) -> Result<ForwardState, BackendErr> {
        // 成功载荷 = 请求规则合成 listening 态（Go control.go:233-235 同义——免二次
        // 查表的 TOCTOU：并发 remove 后快照缺席会误报错误）。
        let brief = ForwardState {
            state: "listening".to_owned(),
            err: String::new(),
            conns: 0,
            rejected: 0,
            rule,
        };
        self.carriers.add_forward(brief.rule.clone()).map_err(carrier_err_to_backend)?;
        Ok(brief)
    }

    fn forward_remove(&self, host_hex: &str, listen: u16) -> Result<(), BackendErr> {
        self.carriers.remove_forward(host_hex, listen).map_err(carrier_err_to_backend)
    }

    fn forward_list(&self, host_hex: &str) -> Vec<ForwardState> {
        if !host_hex.is_empty() {
            match hosts::decode_peer_id_pub(host_hex) {
                Some(id) if self.hosts.has_record(&id) => {}
                _ => return Vec::new(), // host 不在表 = 空形状（Go hostInTable 门同义）
            }
        }
        self.carriers.forward_states(host_hex)
    }

    fn socks_on(&self, host_hex: &str, listen: u16) -> Result<u16, BackendErr> {
        self.carriers.socks_on(host_hex, listen).map_err(carrier_err_to_backend)
    }

    fn socks_off(&self, host_hex: &str) -> Result<u16, BackendErr> {
        self.carriers.socks_off(host_hex).map_err(carrier_err_to_backend)
    }

    fn socks_states(&self) -> Vec<SocksState> {
        self.carriers.socks_states()
    }

    fn speedtest_start(
        &self,
        host_hex: &str,
        p: SpeedtestParams,
    ) -> Result<carriers::speedrun::SpeedtestAck, BackendErr> {
        if !host_in_table(&self.hosts, host_hex) {
            return Err(BackendErr::NoHost);
        }
        Ok(self.carriers.speedtest_start(host_hex, p))
    }

    fn speedtest_status(&self, host_hex: &str) -> Result<Option<SpeedtestStatus>, BackendErr> {
        if !host_in_table(&self.hosts, host_hex) {
            return Err(BackendErr::NoHost);
        }
        Ok(self.carriers.speedtest_status(host_hex))
    }

    fn speedtest_cancel(&self, host_hex: &str) -> Result<(), BackendErr> {
        if !host_in_table(&self.hosts, host_hex) {
            return Err(BackendErr::NoHost);
        }
        self.carriers.speedtest_cancel(host_hex);
        Ok(())
    }
}

/// host hex 存在性闸（非法 hex 或不在表 = false）。
fn host_in_table(hosts: &Arc<hosts::HostTable>, host_hex: &str) -> bool {
    hosts::decode_peer_id_pub(host_hex).is_some_and(|id| hosts.has_record(&id))
}

#[cfg(test)]
mod tests;
