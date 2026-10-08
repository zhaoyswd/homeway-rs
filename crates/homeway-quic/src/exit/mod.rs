//! 出口 QUIC 面（M1 设计 §1.1/§1.2/§1.7）：**独立 UDP 端口** + 一枚专用线程 +
//! `current_thread` runtime。
//!
//! 为什么独立端口（§1.1）：`quinn::Endpoint` 独占一枚 UDP socket，而 `serve.listen` 的
//! socket 由 `ServerBind` 独占（poll 集 + 腿 fd 同构）；两栈共用 fd 需自写 demux，纯增
//! 复杂度。M1 是**双栈并存期**（WG 路径原样可用），单一端口会把两条栈的生命周期耦合。
//!
//! socket 的**绑定与退让在调用侧**（`homeway-core` 复用 WG 的 `+1…+9 → 随机` 退让，
//! §1.1）：本模块只把已绑好的 socket 交给 quinn 并起服务面——所以「实际端口」的权威
//! 来源是 [`ExitQuic::local_addr`]（退让后真实端口，进 token/UPnP/status 用）。
//!
//! 线程模型（§1.7）：与客户端岛同构——**专用线程 + `current_thread` runtime**
//! （不启 `rt-multi-thread`：单线程是结构不变量），线程名 [`EXIT_THREAD`]。收工：
//! [`ExitQuic::stop_within`] 有界预算内退出（到点 detach + 收割线程 `hw-quic-exit-reap`）。
//!
//! 本棒（S1a）范围 = 端点 + TransportConfig + 出口 RPK 身份 + E-q1 就绪行；
//! **准入（`hr-reg3`）与数据面（DATAGRAM↔intercept）是 S1b**，中继承载（`kind=5` +
//! 自定义 `AsyncUdpSocket`）、UPnP 与 token 端点类是 S1c。故本模块当前只到「端点在、
//! 能采纳连接、能观测、能收工」，**未接引擎数据面**。
//!
//! 隔离：本目录（`src/exit/**`）属**异步面**——`tools/check-quic-isolation.sh` 第 ② 条
//! 的白名单与 `driver.rs` 同款（其余 `src/` 文件零异步栈名字）。

mod rpk;
mod transport;

#[cfg(test)]
mod tests;

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use quinn::{Connection, Endpoint, EndpointConfig};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinSet;

use crate::cmd::Logf;
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::sync_util::{lock_unpoison, log_spawn_failed, ExitSignal};

/// QUIC 面线程名（与岛 `homeway-quic` 区分：这是**出口侧**的那一枚）。
pub(crate) const EXIT_THREAD: &str = "homeway-quic-exit";
/// 到点收割线程名（镜像 `hw-quic-reap`）。
pub(crate) const REAP_THREAD: &str = "hw-quic-exit-reap";
/// 驱动循环回看节拍（stop 位 + 连接巡检；无事件时周期性回看，不做忙等）。
const TICK: Duration = Duration::from_millis(250);

/// 并发握手上限（M1 设计 §9.3 Q-O；**保守资源上限**——M2 才有抗放大/Retry/限流）。
pub const DEFAULT_HANDSHAKE_CAP: usize = 64;
/// 握手期限（同上：超时不完成即弃——拦「占着槽位不完成」的形态，不替代 M2 的防放大。
/// 副作用登记：期限内未完成的握手丢掉后，对端重传 Initial 会再触发一轮 ⇒ 单源可反复
/// 触发（**上限仍受并发握手闸约束**），M2 的 Retry/限流面承接）。
pub const DEFAULT_HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);
/// 记行节流（仓内既有口径「首 3 + 每 100」——`relay/mod.rs` 的 `reject_log_due` 同款）。
fn log_due(n: u64) -> bool {
    n <= 3 || n.is_multiple_of(100)
}

/// 出口 QUIC 面配置（**全 std 类型 + 本岛 newtype**：同步面可见面，不夹带异步栈类型）。
pub struct ExitQuicConfig {
    /// 出口 RPK 私钥种子（**出口身份**：`HKDF(后端静态私钥, "homeway/quic-rpk")`，
    /// 由 `homeway-core` 的装配点派生——见 M1 设计 §1.3/§12-②）。
    pub rpk_seed: Ed25519Seed,
    /// 设备表上限（连接总数上限 = `2 × max_devices`，§9.3 Q-O）。
    pub max_devices: usize,
    /// 并发握手上限（缺省 [`DEFAULT_HANDSHAKE_CAP`]；测试调小以钉死拒绝路径）。
    pub handshake_cap: usize,
    /// 握手期限（缺省 [`DEFAULT_HANDSHAKE_DEADLINE`]；同上）。
    pub handshake_deadline: Duration,
}

impl ExitQuicConfig {
    /// 生产缺省（seed 与设备表上限由调用侧给；Q-O 的两个上限取设计定值）。
    pub fn new(rpk_seed: Ed25519Seed, max_devices: usize) -> Self {
        Self {
            rpk_seed,
            max_devices,
            handshake_cap: DEFAULT_HANDSHAKE_CAP,
            handshake_deadline: DEFAULT_HANDSHAKE_DEADLINE,
        }
    }

    /// 连接总数上限（`2 × max_devices`，§9.3 Q-O；饱和乘防溢出）。
    fn conn_cap(&self) -> usize {
        self.max_devices.saturating_mul(2)
    }
}

/// 起点失败（类型化；`#[non_exhaustive]` 防下游穷举）。
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum ExitQuicErr {
    /// 端点/socket 起不来（quinn 的 `Endpoint::new`，或 runtime 建不起来）。
    #[error("QUIC 端点起不来（{0}）")]
    Endpoint(#[from] io::Error),
    /// 服务端身份（RPK）构建失败。
    #[error("QUIC 服务端身份构建失败（{0}）")]
    Identity(#[from] crate::rpk::RpkErr),
}

/// 出口 QUIC 面的运行读数（同步面轮询；无阻塞）。
#[derive(Clone, Debug, Default)]
pub struct ExitQuicSnapshot {
    /// 当前存活连接数（已采纳、未结束）。
    pub connections: u64,
    /// 累计采纳连接数。
    pub admitted: u64,
    /// 观测到的路径变更次数（`remote_address()` 变化——S1b 的 E-q2 行前身）。
    pub path_changes: u64,
    /// 握手失败次数（对端放弃 / 协议错；期限到点单列 [`Self::handshake_timeouts`]）。
    pub handshake_failed: u64,
    /// 当前在途握手数（已收到 Initial、尚未完成）。
    pub handshakes_in_flight: u64,
    /// 因**并发握手超限**拒绝的次数（§9.3 Q-O）。
    pub handshake_refused: u64,
    /// 因**连接总数超限**拒绝的次数（§9.3 Q-O）。
    pub conn_refused: u64,
    /// 因**握手期限**到点放弃的次数（§9.3 Q-O）。
    pub handshake_timeouts: u64,
}

/// 计量面（原子直读——**反应式**，不必等巡检拍；`snapshot()` 由它组装）。
#[derive(Default)]
struct ExitStats {
    connections: AtomicU64,
    admitted: AtomicU64,
    path_changes: AtomicU64,
    handshake_failed: AtomicU64,
    handshakes_in_flight: AtomicU64,
    handshake_refused: AtomicU64,
    conn_refused: AtomicU64,
    handshake_timeouts: AtomicU64,
}

impl ExitStats {
    fn snapshot(&self) -> ExitQuicSnapshot {
        ExitQuicSnapshot {
            connections: self.connections.load(Ordering::SeqCst),
            admitted: self.admitted.load(Ordering::SeqCst),
            path_changes: self.path_changes.load(Ordering::SeqCst),
            handshake_failed: self.handshake_failed.load(Ordering::SeqCst),
            handshakes_in_flight: self.handshakes_in_flight.load(Ordering::SeqCst),
            handshake_refused: self.handshake_refused.load(Ordering::SeqCst),
            conn_refused: self.conn_refused.load(Ordering::SeqCst),
            handshake_timeouts: self.handshake_timeouts.load(Ordering::SeqCst),
        }
    }
}

/// 出口 QUIC 面句柄（同步面）：地址/公开身份/读数/收工（幂等）。
pub struct ExitQuic {
    local_addr: SocketAddr,
    rpk_public_key: RpkPublicKey,
    stop_tx: UnboundedSender<()>,
    exit: Arc<ExitSignal>,
    handle: Mutex<Option<JoinHandle<()>>>,
    stats: Arc<ExitStats>,
    logf: Logf,
}

impl ExitQuic {
    /// 起出口 QUIC 面。
    ///
    /// `socket` = **已绑定**的 UDP socket（退让语义在调用侧：复用 WG 的
    /// `+1…+9 → 随机`，M1 设计 §1.1）；交给 quinn 后本模块不再碰它。
    pub fn start(
        socket: UdpSocket,
        cfg: ExitQuicConfig,
        logf: Logf,
    ) -> Result<ExitQuic, ExitQuicErr> {
        // 端点与 runtime 都建在**专用线程内**（`tokio::net::UdpSocket::from_std` 需要
        // runtime 上下文）；起点失败的两种情形（runtime 建不出来 / 端点或身份建不出来）
        // 都必须在 `start` 返回前定音——不留「起了但死的面」。
        let (ready_tx, ready_rx) = mpsc::channel::<Result<(SocketAddr, RpkPublicKey), ExitQuicErr>>();
        let (stop_tx, stop_rx) = unbounded_channel::<()>();
        let exit = Arc::new(ExitSignal::new());
        let stats = Arc::new(ExitStats::default());

        let handle = thread::Builder::new()
            .name(EXIT_THREAD.into())
            .spawn({
                let logf = Arc::clone(&logf);
                let exit = Arc::clone(&exit);
                let stats = Arc::clone(&stats);
                move || thread_body(socket, cfg, logf, ready_tx, stop_rx, exit, stats)
            })
            .map_err(|e| {
                log_spawn_failed(&logf, EXIT_THREAD, &e, "出口 QUIC 面缺席（WG 面不受影响）");
                ExitQuicErr::Endpoint(e)
            })?;

        match ready_rx.recv() {
            Ok(Ok((local_addr, rpk_public_key))) => Ok(ExitQuic {
                local_addr,
                rpk_public_key,
                stop_tx,
                exit,
                handle: Mutex::new(Some(handle)),
                stats,
                logf,
            }),
            Ok(Err(e)) => {
                let _ = handle.join(); // 起点失败即退；join 掉不让线程悬空
                Err(e)
            }
            Err(_) => {
                // 线程在报告起点前就没了（panic 展开）：join 取回证据
                let _ = handle.join();
                Err(ExitQuicErr::Endpoint(io::Error::other(
                    "QUIC 面线程在就绪前退出（见其 panic 记行）",
                )))
            }
        }
    }

    /// 实际监听地址（**退让后的真实端口**——token/UPnP/status 的唯一来源，§1.1）。
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// 出口 RPK 公钥（进 token 的 32B；M1 设计 §12-②）。
    pub fn rpk_public_key(&self) -> RpkPublicKey {
        self.rpk_public_key
    }

    /// 运行读数（轮询；无阻塞——原子直读，不含锁等待）。
    pub fn snapshot(&self) -> ExitQuicSnapshot {
        self.stats.snapshot()
    }

    /// QUIC 面线程是否已退出。
    pub fn is_finished(&self) -> bool {
        self.exit.is_exited()
    }

    /// 有界收工：预算内退出返回 `true`；到点 detach 交收割线程并返回 `false`
    /// （返回值语义与 `Island::stop_within` 逐条同构：重入即 `true`）。
    pub fn stop_within(&self, deadline: Instant) -> bool {
        let _ = self.stop_tx.send(()); // 幂等：重复发送无害（接收端已退出）
        let h = match lock_unpoison(&self.handle).take() {
            None => return true, // 重入/已 detach：收工请求已发过
            Some(h) => h,
        };
        if self.exit.wait_exit(deadline) {
            join_and_log(h, &self.logf);
            return true;
        }
        let logf = Arc::clone(&self.logf);
        let spawned = thread::Builder::new()
            .name(REAP_THREAD.into())
            .spawn(move || {
                (logf)(&format!(
                    "{REAP_THREAD}: 到点 detach—— QUIC 面线程未在收工预算内退出，由本线程等待其自行退出"
                ));
                join_and_log(h, &logf);
            });
        if let Err(e) = spawned {
            log_spawn_failed(&self.logf, REAP_THREAD, &e, "QUIC 面线程的 join 侧记录缺席");
        }
        false
    }

    /// 无界收工（`Drop` 兜底；世代收工链一律走 [`Self::stop_within`]）。
    pub fn stop(&self) {
        let _ = self.stop_tx.send(());
        if let Some(h) = lock_unpoison(&self.handle).take() {
            join_and_log(h, &self.logf);
        }
    }
}

impl Drop for ExitQuic {
    fn drop(&mut self) {
        // 兜底：已 detach（stop 位已置）时不再等待——不挂死调用方。
        if !self.exit.is_exited() {
            self.stop();
        }
    }
}

/// join 收口：QUIC 面线程 panic 时记行（与岛的 `join_and_classify` 同分工；出口面的
/// 不健康分类（`mark_unhealthy_if_current(gen, …)`）与引擎巡检同批接——S1b）。
fn join_and_log(h: JoinHandle<()>, logf: &Logf) {
    if h.join().is_err() {
        (*logf)(&format!("quic: {EXIT_THREAD} 线程 panic —— 本世代 QUIC 面已死"));
    }
}

/// QUIC 面线程体：`catch_unwind` ⇒ 记行 ⇒ 置退出位（不复用该线程）。
fn thread_body(
    socket: UdpSocket,
    cfg: ExitQuicConfig,
    logf: Logf,
    ready_tx: Sender<Result<(SocketAddr, RpkPublicKey), ExitQuicErr>>,
    stop_rx: UnboundedReceiver<()>,
    exit: Arc<ExitSignal>,
    stats: Arc<ExitStats>,
) {
    let res = catch_unwind(AssertUnwindSafe(|| {
        run_exit(socket, cfg, &logf, ready_tx, stop_rx, &stats)
    }));
    if let Err(payload) = res {
        let msg = crate::driver::panic_msg(payload.as_ref());
        (*logf)(&format!(
            "quic: 出口 QUIC 面 panic（{msg}）—— 判不健康，线程退出（不复用该线程）"
        ));
    }
    exit.mark_exited();
}

/// 一个存活连接 + 最近观测到的远端地址（路径变更观测面：`remote_address()` 变化）。
struct LiveConn {
    conn: Connection,
    remote: SocketAddr,
}

/// 在途握手的结果（三态：采纳 / 失败 / 期限到点——各自的计数与记行口径不同）。
enum HandshakeOutcome {
    Accepted(SocketAddr, Connection),
    Failed(SocketAddr),
    Deadline(SocketAddr),
}

/// 端点到点前的装配与主循环（**全在专用线程的 `current_thread` runtime 内**）。
fn run_exit(
    socket: UdpSocket,
    cfg: ExitQuicConfig,
    logf: &Logf,
    ready_tx: Sender<Result<(SocketAddr, RpkPublicKey), ExitQuicErr>>,
    mut stop_rx: UnboundedReceiver<()>,
    stats: &Arc<ExitStats>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
            return;
        }
    };
    rt.block_on(async move {
        // ---- 服务端身份（出口 RPK：RFC 7250；私钥种子 → PKCS#8 → SPKI 出示）----
        let (server_cfg, rpk_public_key) = match rpk::server_config(&cfg.rpk_seed) {
            Ok(v) => v,
            Err(e) => {
                let _ = ready_tx.send(Err(ExitQuicErr::Identity(e)));
                return;
            }
        };
        // ---- 端点（socket 已在调用侧绑定：退让语义 = WG 同款）----
        let endpoint = match Endpoint::new(
            EndpointConfig::default(),
            Some(server_cfg),
            socket,
            Arc::new(quinn::TokioRuntime),
        ) {
            Ok(ep) => ep,
            Err(e) => {
                let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
                return;
            }
        };
        let local_addr = match endpoint.local_addr() {
            Ok(a) => a,
            Err(e) => {
                let _ = ready_tx.send(Err(ExitQuicErr::Endpoint(e)));
                return;
            }
        };
        // ---- E-q1（判据行：出口 QUIC 端点起后一行；形态见 M1 设计 §3.4）----
        (*logf)(&format!(
            "quic: 端点就绪（{local_addr}，migration={}，initial_mtu={}，datagram 缓冲 {}B）",
            transport::MIGRATION,
            transport::INITIAL_MTU,
            transport::DATAGRAM_BUFFER
        ));
        if ready_tx.send(Ok((local_addr, rpk_public_key))).is_err() {
            return; // 调用侧已放弃（start 失败路径）——直接收摊
        }

        let mut conns: Vec<LiveConn> = Vec::new();
        // 在途握手（并发上限与期限都挂在这张表上；`JoinSet` 保证收工时全部 abort）
        let mut handshakes: JoinSet<HandshakeOutcome> = JoinSet::new();
        let conn_cap = cfg.conn_cap();
        loop {
            tokio::select! {
                _ = stop_rx.recv() => break,
                inc = endpoint.accept() => match inc {
                    Some(incoming) => {
                        let peer = incoming.remote_address();
                        // 闸①：连接总数（**口径收紧一档**：存活连接 + 在途握手一起算——
                        // 否则「在途握手转正」会把上限撑破；§9.3 Q-O 的原义只写了连接总数）
                        let held = (conns.len() + handshakes.len()) as u64;
                        if held >= conn_cap as u64 {
                            incoming.refuse();
                            let n = stats.conn_refused.fetch_add(1, Ordering::SeqCst) + 1;
                            if log_due(n) {
                                (*logf)(&format!(
                                    "quic: 拒新连接（连接总数 {held}/{conn_cap} 超限，来自 {peer}；第 {n} 次）"
                                ));
                            }
                        } else if handshakes.len() >= cfg.handshake_cap {
                            // 闸②：并发握手上限（Q-O 的 64；M2 的抗放大面还没来）
                            incoming.refuse();
                            let n = stats.handshake_refused.fetch_add(1, Ordering::SeqCst) + 1;
                            if log_due(n) {
                                (*logf)(&format!(
                                    "quic: 拒新连接（并发握手 {}/{cap} 超限，来自 {peer}；第 {n} 次）",
                                    handshakes.len(),
                                    cap = cfg.handshake_cap
                                ));
                            }
                        } else {
                            // 闸③：握手期限（到点即弃；丢 `Connecting` = quinn 侧关连接）
                            let deadline = cfg.handshake_deadline;
                            stats.handshakes_in_flight.fetch_add(1, Ordering::SeqCst);
                            handshakes.spawn(async move {
                                match incoming.accept() {
                                    Ok(connecting) => match tokio::time::timeout(deadline, connecting).await {
                                        Ok(Ok(conn)) => HandshakeOutcome::Accepted(peer, conn),
                                        Ok(Err(_e)) => HandshakeOutcome::Failed(peer),
                                        Err(_) => HandshakeOutcome::Deadline(peer),
                                    },
                                    Err(_e) => HandshakeOutcome::Failed(peer),
                                }
                            });
                        }
                    }
                    None => break,
                },
                Some(joined) = handshakes.join_next(), if !handshakes.is_empty() => {
                    stats.handshakes_in_flight.fetch_sub(1, Ordering::SeqCst);
                    match joined {
                        Ok(HandshakeOutcome::Accepted(_peer, conn)) => {
                            stats.admitted.fetch_add(1, Ordering::SeqCst);
                            conns.push(LiveConn { remote: conn.remote_address(), conn });
                            stats.connections.store(conns.len() as u64, Ordering::SeqCst);
                        }
                        // 握手失败（错 RPK / 对端放弃）：明细记行与四类丢弃计数归 S1b 的
                        // E-q3——本棒只留总计数（客户端钉定判据的证据面）
                        Ok(HandshakeOutcome::Failed(_peer)) => {
                            stats.handshake_failed.fetch_add(1, Ordering::SeqCst);
                        }
                        Ok(HandshakeOutcome::Deadline(peer)) => {
                            let n = stats.handshake_timeouts.fetch_add(1, Ordering::SeqCst) + 1;
                            if log_due(n) {
                                (*logf)(&format!(
                                    "quic: 握手期限（{peer} 未在 {deadline:?} 内完成，已弃；第 {n} 次）",
                                    deadline = cfg.handshake_deadline
                                ));
                            }
                        }
                        Err(_join_err) => {} // 任务被 abort（收工路径）——不计
                    }
                }
                _ = tokio::time::sleep(TICK) => {}
            }
            // 巡检（每拍）：清死连接 + 观测路径变更（E-q2 行的前身；S1b 起接 dev 归属）
            conns.retain(|c| c.conn.close_reason().is_none());
            stats.connections.store(conns.len() as u64, Ordering::SeqCst);
            for c in conns.iter_mut() {
                let now = c.conn.remote_address();
                if now != c.remote {
                    c.remote = now;
                    stats.path_changes.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        // ---- 收工：先在途握手全弃（JoinSet drop 即 abort）→ 关端点 → 丢弃连接句柄 ----
        drop(handshakes);
        endpoint.close(0u32.into(), b"exit stopping");
        drop(conns);
        drop(endpoint);
    });
}
