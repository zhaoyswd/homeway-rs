//! 出口本机质量历史。driver 只作有界 try_send，worker 独占序列化 / 磁盘 / 差分。
//! 不观测业务内容；吞吐为实际观测值，不能解释为空闲带宽。
mod report;
mod storage;

use crate::server::intercept::quality::{FlowQuality, InterceptorQuality};
use crate::Logf;
use homeway_quic::{ExitPathQuality, ExitPathVia};
pub use report::{read_report, Coverage, DeviceReport, QualityQuery, QualityReport, TargetReport};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc, Arc,
};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub(crate) const SAMPLE_MS: u64 = 5_000;
const QUEUE_CAPACITY: usize = 4;

#[derive(Debug, thiserror::Error)]
pub enum QualityError {
    #[error("质量查询范围无效：起点必须早于终点，设备必须为完整 16 位十六进制 devTag")]
    InvalidQuery,
    #[error("质量历史 I/O：{0}")]
    Io(#[from] std::io::Error),
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

pub(crate) struct Collector {
    tx: mpsc::SyncSender<Batch>,
    dropped: Arc<AtomicU64>,
    done: mpsc::Receiver<()>,
}

struct Batch {
    at_ms: u64,
    paths: Vec<ExitPathQuality>,
    flows: InterceptorQuality,
    finished_overflow: u64,
    sampling_delay_ms: u64,
}

impl Collector {
    /// 线程创建失败只关闭观测，不影响出口。
    pub(crate) fn start(state: &Path, logf: Logf) -> Option<Self> {
        let (tx, rx) = mpsc::sync_channel::<Batch>(QUEUE_CAPACITY);
        let (done_tx, done) = mpsc::channel();
        let dropped = Arc::new(AtomicU64::new(0));
        let dropped_worker = Arc::clone(&dropped);
        let state = state.to_owned();
        let worker_logf = Arc::clone(&logf);
        match std::thread::Builder::new()
            .name("exit-quality".into())
            .spawn(move || {
                let logf = worker_logf;
                let mut worker = Worker::new();
                let mut store = storage::Store::new(&state);
                let mut errors = 0u64;
                let mut reported_drops = 0;
                while let Ok(batch) = rx.recv() {
                    let drops = dropped_worker.load(Ordering::Relaxed);
                    if drops > reported_drops {
                        (logf)(&format!("quality: 采样队列满或已关闭（累计丢弃={drops}）"));
                        reported_drops = drops;
                    }
                    let records =
                        worker.records(batch, dropped_worker.load(Ordering::Relaxed), errors);
                    for record in records {
                        if let Err(e) = store.append(&record) {
                            errors = errors.saturating_add(1);
                            // 持续磁盘故障按 1、2、4、8... 次节流；错误计数随下次成功 heartbeat 落盘。
                            if errors.is_power_of_two() {
                                (logf)(&format!(
                                    "quality: 历史写入失败（累计={errors}）：{e}"
                                ));
                            }
                        }
                    }
                }
                let _ = done_tx.send(());
            }) {
            Ok(_) => Some(Self { tx, dropped, done }), // 默认detach；显式finish只有界等候。
            Err(e) => {
                (logf)(&format!("quality: 观测线程创建失败：{e}"));
                None
            }
        }
    }

    /// 只在 VPN 转发已停止后调用；慢盘 / 故障线程不拖住收工。
    pub(crate) fn finish(self, budget: Duration, logf: &Logf) {
        let Self { tx, done, .. } = self;
        drop(tx);
        match done.recv_timeout(budget.min(Duration::from_millis(200))) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {
                (logf)("quality: 历史收尾超时，最后采样及关闭摘要未保证落盘")
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                (logf)("quality: 观测线程异常退出，最后采样及关闭摘要未保证落盘")
            }
        }
    }

    pub(crate) fn sample(
        &self,
        paths: Vec<ExitPathQuality>,
        flows: InterceptorQuality,
        finished_overflow: u64,
        sampling_delay_ms: u64,
    ) {
        let batch = Batch {
            at_ms: now_ms(),
            paths,
            flows,
            finished_overflow,
            sampling_delay_ms,
        };
        if self.tx.try_send(batch).is_err() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    schema: u8,
    run: u64,
    at_ms: u64,
    #[serde(flatten)]
    event: Event,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Event {
    Heartbeat {
        from_ms: u64,
        queue_dropped: u64,
        storage_errors: u64,
        finished_overflow: u64,
        closed_overflow: u64,
        active_flows: usize,
        shape_backlog_bytes: usize,
        shape_backlog_peak_bytes: usize,
        reactor_pumps: u64,
        reactor_peak_wall_us: u64,
        sampling_delay_ms: u64,
        start: bool,
    },
    Path(PathRecord),
    Flow(FlowQuality),
}

#[derive(Debug, Serialize, Deserialize)]
struct PathRecord {
    device: String,
    connection_id: u64,
    path_epoch: u64,
    remote: String,
    via: String,
    from_ms: u64,
    rtt_us: u64,
    cwnd: u64,
    udp_tx_bytes: u64,
    udp_rx_bytes: u64,
    sent_packets: u64,
    lost_packets: u64,
    congestion_events: u64,
    send_buffer_drops: u64,
    send_buffer_used: u64,
    path_stable: bool,
    baseline: bool,
    reset: bool,
    finished: bool,
}

struct Previous {
    at_ms: u64,
    epoch: u64,
    stable: bool,
    tx: u64,
    rx: u64,
    sent: u64,
    lost: u64,
    congestion: u64,
    drops: u64,
}

struct Worker {
    run: u64,
    last_ms: Option<u64>,
    previous: HashMap<([u8; 8], u64), Previous>,
}

impl Worker {
    fn new() -> Self {
        let mut bytes = [0; 8];
        let _ = getrandom::getrandom(&mut bytes);
        Self {
            run: u64::from_le_bytes(bytes),
            last_ms: None,
            previous: HashMap::new(),
        }
    }

    fn records(&mut self, batch: Batch, queue_dropped: u64, storage_errors: u64) -> Vec<Record> {
        let mut out = Vec::with_capacity(1 + batch.paths.len() + batch.flows.records.len());
        let mut push = |at_ms, event| {
            out.push(Record {
                schema: 1,
                run: self.run,
                at_ms,
                event,
            })
        };
        push(
            batch.at_ms,
            Event::Heartbeat {
                from_ms: self.last_ms.unwrap_or(batch.at_ms),
                queue_dropped,
                storage_errors,
                finished_overflow: batch.finished_overflow,
                closed_overflow: batch.flows.closed_overflow,
                active_flows: batch.flows.active_flows,
                shape_backlog_bytes: batch.flows.shape_backlog_bytes,
                reactor_peak_wall_us: batch.flows.reactor_peak_wall_us,
                shape_backlog_peak_bytes: batch.flows.shape_backlog_peak_bytes,
                reactor_pumps: batch.flows.reactor_pumps,
                sampling_delay_ms: batch.sampling_delay_ms,
                start: self.last_ms.is_none(),
            },
        );
        self.last_ms = Some(batch.at_ms);
        // 同一批 final 先于 active，允许相同 dev 的重连；连接标识不同不会串账。
        for path in batch.paths {
            let key = (path.dev, path.connection_id);
            let prev = self.previous.get(&key);
            let reset = prev.is_some_and(|p| {
                !path.path_stable
                    || !p.stable
                    || path.path_epoch != p.epoch
                    || path.observed_at_ms < p.at_ms
                    || path.udp_tx_bytes < p.tx
                    || path.udp_rx_bytes < p.rx
                    || path.sent_packets < p.sent
                    || path.lost_packets < p.lost
                    || path.congestion_events < p.congestion
                    || path.send_buffer_drops < p.drops
            });
            let valid = prev.filter(|_| !reset);
            let p = PathRecord {
                device: hex_device(path.dev),
                connection_id: path.connection_id,
                path_epoch: path.path_epoch,
                remote: path.remote.to_string(),
                via: match path.via {
                    ExitPathVia::Direct => "direct",
                    ExitPathVia::Relay => "relay",
                }
                .into(),
                from_ms: valid.map_or(path.observed_at_ms, |p| p.at_ms),
                rtt_us: path.rtt_us,
                cwnd: path.cwnd,
                udp_tx_bytes: valid.map_or(0, |p| path.udp_tx_bytes - p.tx),
                udp_rx_bytes: valid.map_or(0, |p| path.udp_rx_bytes - p.rx),
                sent_packets: valid.map_or(0, |p| path.sent_packets - p.sent),
                lost_packets: valid.map_or(0, |p| path.lost_packets - p.lost),
                congestion_events: valid.map_or(0, |p| path.congestion_events - p.congestion),
                send_buffer_drops: valid.map_or(0, |p| path.send_buffer_drops - p.drops),
                send_buffer_used: path.datagram_send_buffer_used,
                path_stable: path.path_stable,
                baseline: valid.is_none(),
                reset,
                finished: path.finished,
            };
            push(path.observed_at_ms, Event::Path(p));
            if path.finished {
                self.previous.remove(&key);
            } else {
                self.previous.insert(
                    key,
                    Previous {
                        at_ms: path.observed_at_ms,
                        epoch: path.path_epoch,
                        stable: path.path_stable,
                        tx: path.udp_tx_bytes,
                        rx: path.udp_rx_bytes,
                        sent: path.sent_packets,
                        lost: path.lost_packets,
                        congestion: path.congestion_events,
                        drops: path.send_buffer_drops,
                    },
                );
            }
        }
        // 防止 final 溢出造成 previous 无限增长；超出近期样本 1 分钟就作为下次新基线。
        self.previous
            .retain(|_, p| batch.at_ms.saturating_sub(p.at_ms) <= 60_000);
        for flow in batch.flows.records {
            push(flow.ended_unix_ms, Event::Flow(flow));
        }
        out
    }
}

fn hex_device(dev: [u8; 8]) -> String {
    dev.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests;
