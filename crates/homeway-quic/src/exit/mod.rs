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
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use quinn::{Connection, Endpoint, EndpointConfig};
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use crate::cmd::Logf;
use crate::rpk::{Ed25519Seed, RpkPublicKey};
use crate::sync_util::{lock_unpoison, log_spawn_failed, ExitSignal};

/// QUIC 面线程名（与岛 `homeway-quic` 区分：这是**出口侧**的那一枚）。
pub(crate) const EXIT_THREAD: &str = "homeway-quic-exit";
/// 到点收割线程名（镜像 `hw-quic-reap`）。
pub(crate) const REAP_THREAD: &str = "hw-quic-exit-reap";
/// 驱动循环回看节拍（stop 位 + 连接巡检；无事件时周期性回看，不做忙等）。
const TICK: Duration = Duration::from_millis(250);

/// 出口 QUIC 面配置（**全 std 类型 + 本岛 newtype**：同步面可见面，不夹带异步栈类型）。
pub struct ExitQuicConfig {
    /// 出口 RPK 私钥种子（**出口身份**：`HKDF(后端静态私钥, "homeway/quic-rpk")`，
    /// 由 `homeway-core` 的装配点派生——见 M1 设计 §1.3/§12-②）。
    pub rpk_seed: Ed25519Seed,
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
}

/// 出口 QUIC 面句柄（同步面）：地址/公开身份/读数/收工（幂等）。
pub struct ExitQuic {
    local_addr: SocketAddr,
    rpk_public_key: RpkPublicKey,
    stop_tx: UnboundedSender<()>,
    exit: Arc<ExitSignal>,
    handle: Mutex<Option<JoinHandle<()>>>,
    snapshot: Arc<Mutex<ExitQuicSnapshot>>,
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
        let snapshot = Arc::new(Mutex::new(ExitQuicSnapshot::default()));

        let handle = thread::Builder::new()
            .name(EXIT_THREAD.into())
            .spawn({
                let logf = Arc::clone(&logf);
                let exit = Arc::clone(&exit);
                let snapshot = Arc::clone(&snapshot);
                move || thread_body(socket, cfg, logf, ready_tx, stop_rx, exit, snapshot)
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
                snapshot,
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

    /// 运行读数（轮询；无阻塞）。
    pub fn snapshot(&self) -> ExitQuicSnapshot {
        lock_unpoison(&self.snapshot).clone()
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
    snapshot: Arc<Mutex<ExitQuicSnapshot>>,
) {
    let res = catch_unwind(AssertUnwindSafe(|| {
        run_exit(socket, cfg, &logf, ready_tx, stop_rx, &snapshot)
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

/// 端点到点前的装配与主循环（**全在专用线程的 `current_thread` runtime 内**）。
fn run_exit(
    socket: UdpSocket,
    cfg: ExitQuicConfig,
    logf: &Logf,
    ready_tx: Sender<Result<(SocketAddr, RpkPublicKey), ExitQuicErr>>,
    mut stop_rx: UnboundedReceiver<()>,
    snapshot: &Arc<Mutex<ExitQuicSnapshot>>,
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
        let mut admitted: u64 = 0;
        let mut path_changes: u64 = 0;
        loop {
            tokio::select! {
                _ = stop_rx.recv() => break,
                inc = endpoint.accept() => match inc {
                    // 已登记连接（S1b 起为 `hr-reg3` 登记+绑定；本棒只采纳与保活）
                    Some(incoming) => match incoming.await {
                        Ok(conn) => {
                            admitted += 1;
                            conns.push(LiveConn { remote: conn.remote_address(), conn });
                        }
                        Err(_e) => {} // 握手失败（坏 RPK/超时/对端放弃）：计数与记行归 S1b 的 E-q3
                    },
                    None => break,
                },
                _ = tokio::time::sleep(TICK) => {}
            }
            // 巡检（每拍）：清死连接 + 观测路径变更（E-q2 行的前身；S1b 起接 dev 归属）
            let before = conns.len();
            conns.retain(|c| c.conn.close_reason().is_none());
            let mut changes = 0u64;
            for c in conns.iter_mut() {
                let now = c.conn.remote_address();
                if now != c.remote {
                    c.remote = now;
                    changes += 1;
                }
            }
            path_changes += changes;
            if conns.len() != before || changes > 0 {
                *lock_unpoison(snapshot) = ExitQuicSnapshot {
                    connections: conns.len() as u64,
                    admitted,
                    path_changes,
                };
            }
        }
        // ---- 收工：先关端点（对各连接发 CONNECTION_CLOSE），再丢弃连接句柄 ----
        endpoint.close(0u32.into(), b"exit stopping");
        drop(conns);
        drop(endpoint);
    });
}
