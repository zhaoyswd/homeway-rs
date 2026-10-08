//! 客户端连接面（M1 S2a）：端点 + 赛跑 + 登记控制流/刷新 + 迁移保持检测。
//!
//! **本目录属异步面**（`tools/check-quic-isolation.sh` 第 ② 条的 `ASYNC_DIRS`：`client/**`
//! ——与 `exit/**` 同款理由：这里的名字本来就是异步栈的；公面/协议/小件文件仍零命中）。
//!
//! 三处落点（真源逐条）：
//!
//! - **端点与 TransportConfig**：设计 §1.2 的定稿参数与出口面**共用同一份**
//!   （`exit::transport`——MTU/缓冲/ACK/保活/迁移对两端都是同一组判据值）；
//! - **服务端 RPK 钉定**：`exit::rpk::client_pin`（§1.3/§12-②；本切片接线后其
//!   dead-code 豁免已删，见 commit 的偏离说明）；
//! - **赛跑/登记/迁移**：§2.2（首个完成握手者胜）、§2.6（`hr-reg3` 上控制流 + 60s 刷新）、
//!   §2.3（`rebind` + 保持检测）。
//!
//! 单线程前提（同出口面）：本 crate 的岛 runtime 是 `current_thread`，且发送路径的
//! 「预检 → 发送」之间无 `await` ⇒ 不存在被别处插入的窗口（S6-2 的代码门条）。

mod migration;
mod race;
mod register;
#[cfg(test)]
mod tests;

use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use quinn::{Connection, Endpoint, RecvStream, SendStream, VarInt};
use tokio::time::Instant as TokioInstant;

use crate::cmd::{Candidate, IslandErr, Logf, RaceOutcome, Via};
use crate::config::{IslandConfig, IslandCredential};
use crate::reg3;

pub(crate) use migration::{MigrationEvent, Watch};
pub(crate) use race::LogGate;

/// 钉定用的服务端名（RPK 体系里名字无语义；两端取值只需一致且稳定）。
pub(crate) fn server_name() -> &'static str {
    crate::exit::rpk::client_pin::SERVER_NAME
}

/// 探活的轮询节拍（预算内的最小等待步长）。
const PROBE_POLL: Duration = Duration::from_millis(20);

/// 岛持有的连接面（岛线程独占 ⇒ 无锁；端点用 `Arc` 只为让赛跑任务持一份）。
pub(crate) struct Face {
    endpoint: Arc<Endpoint>,
    cfg: quinn::ClientConfig,
    cred: Arc<IslandCredential>,
    /// 当前本地地址（`rebind` 后就地更新；判据行 N-b 的「旧 → 新」源）。
    local: SocketAddrV4,
}

impl Face {
    /// 起端点（含 RPK 钉定 + §1.2 的 TransportConfig）。
    ///
    /// ⚠️ 必须在 runtime 上下文里调（`Endpoint::client` 取运行时的 IO 驱动）。
    pub(crate) fn open(cfg: IslandConfig) -> io::Result<Face> {
        let IslandConfig {
            credential,
            bind,
            patrol: _,
        } = cfg;
        let bind: SocketAddr = bind
            .map(SocketAddr::V4)
            .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)));
        let endpoint = Endpoint::client(bind)?;
        let local = match endpoint.local_addr()? {
            SocketAddr::V4(v4) => v4,
            SocketAddr::V6(_) => {
                return Err(io::Error::other("端点本地地址不是 IPv4（形态异常）"));
            }
        };
        let mut client_cfg = quinn::ClientConfig::new(
            crate::exit::rpk::client_pin::client_crypto_config(credential.pin())
                .map_err(io::Error::other)?,
        );
        client_cfg.transport_config(crate::exit::transport::transport_config());
        Ok(Face {
            endpoint: Arc::new(endpoint),
            cfg: client_cfg,
            cred: Arc::new(credential),
            local,
        })
    }

    /// 端点（`Arc` 克隆：赛跑任务要持一份——`quinn::Endpoint` 不可 Clone）。
    pub(crate) fn endpoint(&self) -> Arc<Endpoint> {
        Arc::clone(&self.endpoint)
    }

    /// 客户端配置（含钉定 + §1.2 TransportConfig；`Clone` 是 quinn 侧的常态）。
    pub(crate) fn client_config(&self) -> quinn::ClientConfig {
        self.cfg.clone()
    }

    /// 准入材料（`Arc` 克隆：赛跑/刷新任务共用一份，不复制敏感材料）。
    pub(crate) fn credential(&self) -> &Arc<IslandCredential> {
        &self.cred
    }

    /// 换本地 socket（迁移原语；设计 §2.3）。
    ///
    /// 返回值 = 新本地地址。**注意语义**：成功只代表 socket 换绑完成——路径是否真的
    /// 可用要等对端回包（保持检测见 [`Watch`]；对端不可达**没有专用错误**）。
    pub(crate) fn rebind(&mut self, local: Option<SocketAddrV4>) -> Result<SocketAddrV4, IslandErr> {
        let bind: SocketAddr = local
            .map(SocketAddr::V4)
            .unwrap_or_else(|| SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)));
        let sock = UdpSocket::bind(bind).map_err(IslandErr::Rebind)?;
        sock.set_nonblocking(true).map_err(IslandErr::Rebind)?;
        self.endpoint.rebind(sock).map_err(IslandErr::Rebind)?;
        let now = self.endpoint.local_addr().map_err(IslandErr::Rebind)?;
        let SocketAddr::V4(v4) = now else {
            // 端点恒为 IPv4（候选是 SocketAddrV4、绑定地址也由 V4 给出）
            return Err(IslandErr::Rebind(io::Error::other(
                "端点本地地址不是 IPv4（形态异常）",
            )));
        };
        self.local = v4;
        Ok(v4)
    }

    /// 当前本地地址（N-b 行的「旧 → 新」源）。
    pub(crate) fn local(&self) -> SocketAddrV4 {
        self.local
    }
}

/// 赛跑 + 登记（设计 §2.2/§2.6）：首个完成握手者胜 ⇒ 走首条 bidi 控制流登记 ⇒
/// 等准入窗（[`register::REG_SETTLE`]）⇒ 交回可用连接。
///
/// 自由函数而不是 `Face` 的方法：宿主把它搬进 `JoinSet` 任务（`Face` 本体留在宿主里
/// 供换绑/刷新用），故只搬必需的三件（端点 `Arc` + 配置 + 凭据 `Arc`）。
pub(crate) async fn connect(
    endpoint: Arc<Endpoint>,
    cfg: quinn::ClientConfig,
    cred: Arc<IslandCredential>,
    cands: &[Candidate],
    budget: Duration,
    log_c4: bool,
    logf: &Logf,
) -> Result<(Established, RaceOutcome), IslandErr> {
    race::run(endpoint, cfg, cred, cands, budget, log_c4, logf).await
}

/// 赛跑赢家的原料（登记已完成的连接 + 控制流两半边 + 连接绑定值）。
///
/// 由 [`race::run`] 产出、由岛宿主装配成 [`Live`]（宿主才知道巡检节拍）。
pub(crate) struct Established {
    pub(crate) conn: Connection,
    pub(crate) send: SendStream,
    pub(crate) recv: RecvStream,
    pub(crate) exporter: [u8; reg3::EXPORTER_LEN],
    pub(crate) via: Via,
    pub(crate) ep: SocketAddrV4,
}

/// 一次**已登记**的在用连接（岛线程独占）。
///
/// 控制流两半边都留着：写半边用于刷新帧，读半边**只持有不读**——提前 drop 会给对端发
/// RESET/STOP_SENDING 噪声（出口侧的控制流保持打开是其设计的一部分，见 `exit::conn`）。
pub(crate) struct Live {
    pub(crate) conn: Connection,
    send: SendStream,
    _recv: RecvStream,
    exporter: [u8; reg3::EXPORTER_LEN],
    pub(crate) via: Via,
    pub(crate) ep: SocketAddrV4,
    /// 刷新节拍（巡检拍驱动；`Instant` = runtime 时钟，与 `tokio::time` 同源）。
    refresh: register::RefreshTimer,
}

impl Live {
    /// 装配在用的连接面（登记帧已在 [`race::run`] 内写出）。
    pub(crate) fn new(e: Established, patrol: Duration) -> Self {
        Self {
            conn: e.conn,
            send: e.send,
            _recv: e.recv,
            exporter: e.exporter,
            via: e.via,
            ep: e.ep,
            refresh: register::RefreshTimer::new(patrol),
        }
    }

    /// 连接是否仍活（`close_reason` 为空）。
    pub(crate) fn alive(&self) -> bool {
        self.conn.close_reason().is_none()
    }

    pub(crate) fn rtt_ms(&self) -> u64 {
        self.conn.rtt().as_millis() as u64
    }

    /// `max_datagram_size()` 现值（设计 §2.1 的快照 `mtu` 位）。
    pub(crate) fn mds(&self) -> Option<u32> {
        self.conn.max_datagram_size().map(|v| v as u32)
    }

    /// `current_mtu` 现值（N-a 行/DPLPMTUD 观测）。
    pub(crate) fn current_mtu(&self) -> u16 {
        self.conn.stats().path.current_mtu
    }

    /// 收到的 UDP 报文计数（迁移保持判据的「对端回包」信号源；单调增）。
    pub(crate) fn udp_rx(&self) -> u64 {
        self.conn.stats().udp_rx.datagrams
    }

    /// 刷新到点则写一帧 `hr-reg3`（C15' 行；节拍 = 巡检节拍）。
    ///
    /// 返回 `false` = 连接已断（调用方清连接面）。写失败同样归「断」。
    pub(crate) async fn refresh_if_due(&mut self, cred: &IslandCredential, logf: &Logf) -> bool {
        if !self.refresh.due(TokioInstant::now()) {
            return true;
        }
        register::write_frame(
            &mut self.send,
            cred,
            &self.exporter,
            logf,
            self.ep,
            self.via.is_relay(),
            false,
        )
        .await
        .is_ok()
    }
}

/// 连接级判活（替代 `path_probe` 的连接面判据；设计 §2.5）。
///
/// 语义（**不误报活**）：连接已关 ⇒ [`IslandErr::ConnectionLost`]；否则做一次
/// **ack-eliciting 的最小写**（开一条 uni 流立即 reset：纯流面信号，对端应用面零语义、
/// 不占流槽、不污染出口的四类丢弃计数），再在 `budget` 内等**对端回包**证据
/// （`udp_rx` 增长 ⇒ ACK/数据到达）。预算内无证据 ⇒ [`IslandErr::ProbeNoResponse`]。
///
/// ⚠️ 端到端**回显**（今天 `path_probe` 探 `tunnel_ip:1` 的语义）需要服务流面 +
/// 数据面（S2-4/S2b、M3 的 `STREAM[probe]`）——本切片给的是「连接与路径活着」的
/// 协议级结论，够 patrol 判活/归因用，不够判「隧道内目标可达」。
pub(crate) async fn probe(conn: &Connection, budget: Duration) -> Result<Duration, IslandErr> {
    fn check(c: &Connection) -> Result<(), IslandErr> {
        if c.close_reason().is_some() {
            return Err(IslandErr::ConnectionLost);
        }
        Ok(())
    }
    check(conn)?;
    let rx0 = conn.stats().udp_rx.datagrams;
    let outcome = tokio::time::timeout(budget, async {
        // 主动一步：ack-eliciting 的最小写（有界——流控信用耗尽时宁可超时也不挂死）
        if let Ok(mut s) = conn.open_uni().await {
            let _ = s.reset(VarInt::from_u32(0));
        }
        let deadline = TokioInstant::now() + budget;
        loop {
            check(conn)?;
            if conn.stats().udp_rx.datagrams > rx0 {
                return Ok(conn.rtt());
            }
            if TokioInstant::now() >= deadline {
                return Err(IslandErr::ProbeNoResponse);
            }
            tokio::time::sleep(PROBE_POLL).await;
        }
    })
    .await;
    match outcome {
        Ok(v) => v,
        // 外层超时兜底（内层 deadline 已覆盖——双保险，保证回执有界）
        Err(_) => check(conn).and(Err(IslandErr::ProbeNoResponse)),
    }
}

/// 时长文案（判据行的 `%v`）：与既有 C 系列的 Go `%v`（ms 取整、去尾零）同口径。
/// 本 crate 是叶子（不得依赖 `homeway-core::go_fmt`）⇒ 只实现判据面用得到的区间。
pub(crate) fn fmt_dur(d: Duration) -> String {
    let ms = (d.as_nanos() + 500_000) / 1_000_000; // Go `Round(time.Millisecond)`
    if ms == 0 {
        return "0s".to_owned(); // Go `time.Duration(0).String()`
    }
    if ms < 1000 {
        return format!("{ms}ms");
    }
    let secs = ms / 1000;
    let frac = ms % 1000;
    let (m, s) = (secs / 60, secs % 60);
    let mut out = String::new();
    if m > 0 {
        out.push_str(&format!("{m}m"));
    }
    out.push_str(&s.to_string());
    if frac > 0 {
        let mut f = format!("{frac:03}");
        while f.ends_with('0') {
            f.pop();
        }
        out.push('.');
        out.push_str(&f);
    }
    out.push('s');
    out
}

#[cfg(test)]
mod fmt_tests {
    use super::fmt_dur;
    use std::time::Duration;

    /// 判据行时长文案的口径锚点（与 `homeway-core::go_fmt` 的 ms 取整族同形）。
    #[test]
    fn duration_text_matches_go_ms_shape() {
        assert_eq!(fmt_dur(Duration::ZERO), "0s");
        assert_eq!(fmt_dur(Duration::from_millis(500)), "500ms");
        assert_eq!(fmt_dur(Duration::from_millis(1500)), "1.5s");
        assert_eq!(fmt_dur(Duration::from_millis(30_500)), "30.5s");
        assert_eq!(fmt_dur(Duration::from_millis(90_500)), "1m30.5s");
        assert_eq!(fmt_dur(Duration::from_secs(60)), "1m0s");
    }
}
