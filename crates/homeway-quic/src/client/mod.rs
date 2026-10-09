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
//! - **赛跑/准入/迁移**：§2.2（首个完成握手者胜）、§1.7/§1.8（`hr-reg4` 四帧准入 +
//!   60s `R4` 刷新，同一控制流）、§2.3（`rebind` + 保持检测）。
//!
//! 单线程前提（同出口面）：本 crate 的岛 runtime 是 `current_thread`，且发送路径的
//! 「预检 → 发送」之间无 `await` ⇒ 不存在被别处插入的窗口（S6-2 的代码门条）。

pub(crate) mod dataplane;
pub(crate) mod ladder;
mod migration;
mod race;
mod register;
mod relay_sock;
pub(crate) mod streams;
#[cfg(test)]
mod tests;

use std::io;
use std::net::SocketAddrV4;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use quinn::{Connection, Endpoint, EndpointConfig, ReadExactError, RecvStream, SendStream};
use tokio::sync::Mutex as TokioMutex;
use tokio::time::Instant as TokioInstant;

use crate::cmd::{Candidate, IslandErr, Logf, RaceOutcome, Via};
use crate::config::{IslandConfig, IslandCredential};
use crate::reg4;
use crate::stream::StreamTag;

pub(crate) use migration::{MigrationEvent, Watch};
pub(crate) use race::LogGate;
pub(crate) use register::session_closed;
pub(crate) use relay_sock::{ClientSock, RelayTable, SockStats};

/// 记行节流（仓内既有口径「首 3 + 每 100」——与 `driver::log_due` 同值同义；两处各一份
/// 是为了让 socket 层（异步面）不反向依赖宿主。）
pub(crate) fn log_due(n: u64) -> bool {
    n <= 3 || n.is_multiple_of(100)
}

/// 钉定用的服务端名（RPK 体系里名字无语义；两端取值只需一致且稳定）。
pub(crate) fn server_name() -> &'static str {
    crate::exit::rpk::client_pin::SERVER_NAME
}

/// 发送面错误的读数视图（快照面；`fresh` = 白名单命中且落在新鲜度窗内 ⇒ M 判据的证据）。
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct SendErrView {
    /// 累计非 `WouldBlock` 错误数。
    pub total: u64,
    /// 累计白名单命中数（`rebind` 清零）。
    pub local: u64,
    /// 末次白名单错误距今（`None` = 无/已清零）。
    pub age: Option<Duration>,
    /// 末次白名单错误的 **errno**（`kind` 的落纸形态；判据行/快照用）。
    pub last_errno: Option<i32>,
    /// 是否**新鲜**（窗内命中 ⇒ M（本机发送面错误）成立的证据）。
    pub fresh: bool,
}

/// 快探的回显载荷（1B；§3.2「写 1B + 等 1B 回显」）。
///
/// 出口侧（`exit::serve`）**原样回显**，不解释该字节——它的唯一作用是「让应用层证据
/// 双向可核」：客户端写出 1B、收到 1B（且逐字节相同）才判活（传输层未察觉的对端死亡
/// 只能由这种端到端回显定音，§13-T2 实测首 34 条 `PROBE_FAIL_REASON=None`）。
const PROBE_BYTE: u8 = 0x50; // 'P'

/// 岛持有的连接面（岛线程独占 ⇒ 无锁；端点用 `Arc` 只为让赛跑任务持一份）。
pub(crate) struct Face {
    endpoint: Arc<Endpoint>,
    cfg: quinn::ClientConfig,
    cred: Arc<IslandCredential>,
    /// 中继候选表（发送侧包封的路由键；岛在 `SetCandidates`/`Connect`/`adopt` 时装配）。
    relays: Arc<RelayTable>,
    /// socket 读数（非 kind=5 帧忽略计数等；快照面读）。
    stats: Arc<Mutex<SockStats>>,
    /// 记行口（socket 层的忽略计数行用；与宿主同一个 `Logf`）。
    logf: Logf,
    /// 当前本地地址（`rebind` 后就地更新；判据行 N-b 的「旧 → 新」源）。
    local: SocketAddrV4,
}

impl Face {
    /// 起端点（含 RPK 钉定 + §1.2 的 TransportConfig + **S2-7 的包封/剥壳 socket** +
    /// **M3 §1.7 的流面限制**）。
    ///
    /// ⚠️ 必须在 runtime 上下文里调（`ClientSock::open` 的 `from_std` 与 `Endpoint::new_with_abstract_socket`
    /// 都要 IO 驱动）。
    ///
    /// 为什么不用 `Endpoint::client`（内建 socket）：S2-7 要求「同一连接既能走直连裸包、
    /// 也能走中继信封」——包封键是**发送目的地址**，只有自定义 socket 才看得见它；两条
    /// 物理路径也必须是**同一枚本地 socket**（连接路径身份唯一，见 `relay_sock` 模块头）。
    pub(crate) fn open(cfg: IslandConfig, logf: Logf) -> io::Result<Face> {
        let IslandConfig {
            credential,
            bind,
            patrol: _,
            mtu_cap,
            streams: stream_limits,
            probe: _,
        } = cfg;
        let relays = RelayTable::new();
        let stats = Arc::new(Mutex::new(SockStats::default()));
        let (sock, local) =
            ClientSock::open(bind, Arc::clone(&relays), Arc::clone(&stats), Arc::clone(&logf))?;
        let endpoint = Endpoint::new_with_abstract_socket(
            EndpointConfig::default(),
            None,
            sock,
            Arc::new(quinn::TokioRuntime),
        )?;
        let mut client_cfg = quinn::ClientConfig::new(
            crate::exit::rpk::client_pin::client_crypto_config(credential.pin())
                .map_err(io::Error::other)?,
        );
        // S3-1：MTU 上限旋钮（`HOMEWAY_QUIC_MTU` / `tunConfig.quicMtuCap` 的落地位）
        // + M3：流窗口/并发（`StreamLimits`，与出口面同一份 `exit::transport` 组装）
        client_cfg.transport_config(crate::exit::transport::transport_config_with(
            mtu_cap,
            stream_limits,
        ));
        Ok(Face {
            endpoint: Arc::new(endpoint),
            cfg: client_cfg,
            cred: Arc::new(credential),
            relays,
            stats,
            logf,
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

    /// 中继候选表（S2-7 的发送侧路由；宿主在候选装配/采纳时写）。
    pub(crate) fn relays(&self) -> &Arc<RelayTable> {
        &self.relays
    }

    /// socket 读数（快照面读：非 kind=5 帧忽略计数等）。
    pub(crate) fn sock_stats(&self) -> (u64, u64, u64, u64) {
        let s = crate::sync_util::lock_unpoison(&self.stats);
        (s.relay_tx, s.rx_ignored, s.rx_dgrams, s.tx_dgrams)
    }

    /// 发送面错误读数（§3.1-N5 的岛侧信号；新鲜度按 `window` 现算——窗值来自
    /// `ProbeTuning::send_err_fresh`，判定的单源在 `SockStats::local_send_err_fresh`）。
    ///
    /// 未命中白名单的错误只进第一个计数（**不进 M/R 判别**——`WouldBlock` 是
    /// 「发送缓冲满，回去等 poller」的**正常回执**，计进去就会把上行拥塞误判成
    /// 「本机发送面错误」）。
    pub(crate) fn send_err_view(&self, window: Duration) -> SendErrView {
        let s = crate::sync_util::lock_unpoison(&self.stats);
        let now = std::time::Instant::now();
        SendErrView {
            total: s.send_errs,
            local: s.send_errs_local,
            age: s.last_local_send_err_at.map(|t| now.saturating_duration_since(t)),
            last_errno: s.last_local_send_err_errno,
            fresh: s.local_send_err_fresh(now, window),
        }
    }

    /// 换本地 socket（迁移原语；设计 §2.3）。
    ///
    /// 返回值 = 新本地地址。**注意语义**：成功只代表 socket 换绑完成——路径是否真的
    /// 可用要等对端回包（保持检测见 [`Watch`]；对端不可达**没有专用错误**）。
    ///
    /// 形态：新 socket 仍是 S2-7 的包封/剥壳 socket（共享同一张中继表与读数）——
    /// **不能**用 `Endpoint::rebind(stdio_socket)`（那会退回 quinn-udp 内建 socket，
    /// 中继路径当场失去信封语义）。
    pub(crate) fn rebind(&mut self, local: Option<SocketAddrV4>) -> Result<SocketAddrV4, IslandErr> {
        let (sock, v4) = ClientSock::open(
            local,
            Arc::clone(&self.relays),
            Arc::clone(&self.stats),
            Arc::clone(&self.logf),
        )
        .map_err(IslandErr::Rebind)?;
        self.endpoint
            .rebind_abstract(sock)
            .map_err(IslandErr::Rebind)?;
        self.local = v4;
        // §3.1-N5：**换网即清**发送面错误（`stats` 的 `Arc` 刻意跨 socket 共享——不清就会
        // 用换网前的陈旧 errno 决定下一步动作）。清零是「M/R 判别只看本网环境」的前提。
        crate::sync_util::lock_unpoison(&self.stats).clear_local_send_err();
        Ok(v4)
    }

    /// 当前本地地址（N-b 行的「旧 → 新」源）。
    pub(crate) fn local(&self) -> SocketAddrV4 {
        self.local
    }
}

/// 赛跑 + 准入（设计 §2.2/§1.7）：首个完成握手者胜 ⇒ 走首条 bidi 控制流四帧准入
/// （等 `A4` 确定性回执）⇒ 交回可用连接。
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

/// 赛跑赢家的原料（准入已完成的连接 + 控制流两半边 + 连接绑定值）。
///
/// 由 [`race::run`] 产出、由岛宿主装配成 [`Live`]（宿主才知道巡检节拍）。
pub(crate) struct Established {
    pub(crate) conn: Connection,
    pub(crate) send: SendStream,
    pub(crate) recv: RecvStream,
    pub(crate) exporter: [u8; reg4::EXPORTER_LEN],
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
    exporter: [u8; reg4::EXPORTER_LEN],
    pub(crate) via: Via,
    pub(crate) ep: SocketAddrV4,
    /// 刷新节拍（巡检拍驱动；`Instant` = runtime 时钟，与 `tokio::time` 同源）。
    refresh: register::RefreshTimer,
    /// **快探持久流**（§3.2：连接建立后常驻——每次探活写 1B 等 1B 回显，不每拍开新流；
    /// 失败时置 `None`，下一拍重开）。`TokioMutex` 串行化并发探活（同刻只许一条在途）。
    probe: Arc<TokioMutex<Option<ProbeStream>>>,
}

/// 快探持久流的两半边（`SendStream`+`RecvStream` 不可 `Clone` ⇒ 整体持在 `Live` 的槽里）。
///
/// `pub(crate)`：类型名要出现在 [`probe`] 的签名上（`driver.rs` 把槽搬进探活任务）。
pub(crate) struct ProbeStream {
    send: SendStream,
    recv: RecvStream,
}

impl Live {
    /// 装配在用的连接面（四帧准入已在 [`race::run`] 内完成）。
    pub(crate) fn new(e: Established, patrol: Duration) -> Self {
        Self {
            conn: e.conn,
            send: e.send,
            _recv: e.recv,
            exporter: e.exporter,
            via: e.via,
            ep: e.ep,
            refresh: register::RefreshTimer::new(patrol),
            probe: Arc::new(TokioMutex::new(None)),
        }
    }

    /// 快探槽（探活任务经它写/读；`Arc` 克隆进任务）。
    pub(crate) fn probe_slot(&self) -> &Arc<TokioMutex<Option<ProbeStream>>> {
        &self.probe
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

    /// 路径统计（设计 §6.2-3/§12-④：`lost_packets`/`congestion_events` 进状态 JSON 与
    /// S5 的证据包——「限速器静默丢被 QUIC 当拥塞」的分辨面）。
    pub(crate) fn path_stats(&self) -> (u64, u64) {
        let p = self.conn.stats().path;
        (p.lost_packets, p.congestion_events)
    }

    /// **已占用（未确认）的 DATAGRAM 发送缓冲字节数**（M1 交下项 N8① / 设计 §9.1.1）。
    ///
    /// 「已死未察觉期」（黑洞）里 `send_datagram` 恒 `Ok`，最多 1 MiB 已入缓冲的包会在连接
    /// 终结时被 quinn 静默丢——本读数让这批**从无计数变成可观测**。上限 = 每连接
    /// `exit::transport::DATAGRAM_BUFFER`（两端共用同一份 TransportConfig，故同一常量）。
    ///
    /// **残余（如实登记）**：连接终结时被 quinn 丢的那批仍**无逐包计数**——本读数只回答
    /// 「此刻缓冲里压着多少」，不回答「丢了哪些」。
    pub(crate) fn send_buffer_used(&self) -> u64 {
        (crate::exit::transport::DATAGRAM_BUFFER as u64)
            .saturating_sub(self.conn.datagram_send_buffer_space() as u64)
    }

    /// 刷新到点则写一帧 `R4`（`hr-reg4-refresh` 域；C15' 行；节拍 = 巡检节拍）。
    ///
    /// 返回 `false` = 连接已断（调用方清连接面）。写失败同样归「断」。
    pub(crate) async fn refresh_if_due(&mut self, cred: &IslandCredential, logf: &Logf) -> bool {
        if !self.refresh.due(TokioInstant::now()) {
            return true;
        }
        register::write_refresh(
            &mut self.send,
            cred,
            &self.exporter,
            logf,
            self.ep,
            self.via.is_relay(),
        )
        .await
        .is_ok()
    }
}

/// **快探（真回显；M3 §3.2 / §1.7-N13）**：`STREAM[probe]`（tag=5）持久流上「写 1B +
/// 等 1B 回显」，预算内拿到**逐字节相同**的回显 ⇒ 判活并回实测往返。
///
/// 语义（**不误报活**）：
/// - 连接已关 ⇒ [`IslandErr::ConnectionLost`]；
/// - 回显流首次使用时**懒开**（写 tag 首字节），此后跨拍常驻；任一步失败 ⇒ **丢弃该流**
///   （下一拍重开）并回 [`IslandErr::ProbeNoResponse`]——**不**回落到「连接级判据」
///   （§13-T2 的承重结论：传输层在对端进程死亡后 40s 才定音，60s 巡检拍解释不了真机
///   的 32s/41s；只有应用层回显能在 0.7s 级定音）；
/// - 与旧实现（`open_uni + reset` + 等 `udp_rx`）的关系：**这就是 §1.7-N13 要求的
///   「`uni=0` 与 probe 换真回显同切片」**——旧的 `open_uni` 在 `max_concurrent_uni_streams=0`
///   下会被流控永久挂起（对端广告 0 ⇒ 无信用），故两者必须一起换。
pub(crate) async fn probe(
    conn: &Connection,
    slot: &Arc<TokioMutex<Option<ProbeStream>>>,
    budget: Duration,
) -> Result<Duration, IslandErr> {
    fn check(c: &Connection) -> Result<(), IslandErr> {
        if c.close_reason().is_some() {
            return Err(IslandErr::ConnectionLost);
        }
        Ok(())
    }
    check(conn)?;
    let deadline = TokioInstant::now() + budget;
    // 并发探活串行化（同刻只许一条在途；拿锁本身也在预算内——否则一条卡死的探活会把
    // 后续探活拖出预算）。
    let mut guard = match tokio::time::timeout(budget, slot.lock()).await {
        Ok(g) => g,
        Err(_) => return Err(IslandErr::ProbeNoResponse),
    };
    if guard.is_none() {
        let left = deadline.saturating_duration_since(TokioInstant::now());
        match tokio::time::timeout(left, open_probe(conn)).await {
            Ok(Ok(ps)) => *guard = Some(ps),
            Ok(Err(e)) => return Err(e),
            Err(_) => return Err(IslandErr::ProbeNoResponse),
        }
    }
    let left = deadline.saturating_duration_since(TokioInstant::now());
    let ps = guard.as_mut().expect("上面已保证 Some");
    match tokio::time::timeout(left, echo_once(ps, conn)).await {
        Ok(Ok(rtt)) => Ok(rtt),
        Ok(Err(e)) => {
            *guard = None; // 流面已坏：丢弃，下一拍重开（**不**回落连接级判据）
            Err(e)
        }
        Err(_) => {
            *guard = None;
            check(conn).and(Err(IslandErr::ProbeNoResponse))
        }
    }
}

/// 懒开快探持久流（tag 首字节由**岛**写——`StreamTag` 是 tag 的单源，见 `stream.rs`）。
async fn open_probe(conn: &Connection) -> Result<ProbeStream, IslandErr> {
    let (mut send, recv) = conn.open_bi().await.map_err(|_| IslandErr::ConnectionLost)?;
    send.write_all(&[StreamTag::Probe.as_byte()])
        .await
        .map_err(|_| IslandErr::ConnectionLost)?;
    Ok(ProbeStream { send, recv })
}

/// 一次「写 1B + 读 1B 回显」；回显必须**逐字节相同**（否则按失败收——见 [`PROBE_BYTE`]）。
async fn echo_once(ps: &mut ProbeStream, conn: &Connection) -> Result<Duration, IslandErr> {
    let t0 = TokioInstant::now();
    ps.send
        .write_all(&[PROBE_BYTE])
        .await
        .map_err(|_| IslandErr::ProbeNoResponse)?;
    let mut one = [0u8; 1];
    match ps.recv.read_exact(&mut one).await {
        Ok(()) if one[0] == PROBE_BYTE => Ok(t0.elapsed()),
        Ok(()) => Err(IslandErr::ProbeNoResponse), // 回显内容不符（不是本探针的回波）
        Err(ReadExactError::FinishedEarly(0)) => Err(IslandErr::ProbeNoResponse),
        Err(_) => {
            // 对端复位/连接死：区分「连接真死」与「服务面卡死」
            if conn.close_reason().is_some() {
                Err(IslandErr::ConnectionLost)
            } else {
                Err(IslandErr::ProbeNoResponse)
            }
        }
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
