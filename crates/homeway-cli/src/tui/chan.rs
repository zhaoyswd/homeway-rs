//! 读帧→UI 通道（tasks 3.7）：有界 + 背压阻塞，**绝不静默丢字节**。
//!
//! 形态 = `std::sync::mpsc::sync_channel` 的双半封装：容量满时 `send` 阻塞——
//! 终端腿读帧线程随之阻塞，背压沿 QUIC 流自然回传到出口写者（出口侧 raw 腿写
//! 停滞退避语义，`service.rs` 任务 4.2），宁可滞后不可失步（丢一个字节 = 模拟器
//! 永久失步）。UI 侧以 [`RecvHalf::drain`] 非阻塞收全部待处理帧（**合帧**：UI
//! 跟不上时丢的是渲染拍，不是字节）。帧序 = 字节序（FIFO 保证）。
//!
//! 消费方 = app 主循环的腿线程（任务组 4）——期间豁免 dead_code。

#![allow(dead_code)]

use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TryRecvError};
use std::time::Duration;

/// 腿帧消息（读帧线程 → UI）。`Bytes` 载荷 = PTY 字节流切片（回放与实时同形）。
#[derive(Debug, PartialEq)]
pub(crate) enum LegMsg {
    Bytes(Vec<u8>),
    Attached,
    ReplayDone { truncated: bool },
    State { agent: u8, state_v2: u8, title: String },
    Ended { code: i32, reason: String },
}

/// 建通道（`cap` 为界：建议 64——一问一答突发吸收，语义同出口 F3 队列）。
pub(crate) fn leg_channel(cap: usize) -> (SendHalf, RecvHalf) {
    let (tx, rx) = std::sync::mpsc::sync_channel(cap);
    (SendHalf(tx), RecvHalf(rx))
}

/// 读帧线程侧半（`Send`：可 move 进腿线程）。满则阻塞，不断连、不丢帧。
pub(crate) struct SendHalf(SyncSender<LegMsg>);

impl SendHalf {
    /// 断连（UI 侧收线）⇒ `Err`——读帧线程收尾。
    pub fn send(&self, msg: LegMsg) -> Result<(), SendEnded> {
        self.0.send(msg).map_err(|_| SendEnded)
    }
}

/// UI 侧半。
pub(crate) struct RecvHalf(Receiver<LegMsg>);

impl RecvHalf {
    /// 非阻塞取一帧（渲染拍内逐帧处理）。
    pub fn try_recv(&self) -> Result<LegMsg, TryRecvError> {
        self.0.try_recv()
    }

    /// 带超时取一帧（UI 心跳形态：超时 = 无新帧 ⇒ 直接渲染当前网格）。
    pub fn recv_timeout(&self, timeout: Duration) -> Result<LegMsg, RecvTimeoutError> {
        self.0.recv_timeout(timeout)
    }

    /// **合帧收清**：非阻塞取空通道，返回全部待处理帧（顺序保持）。
    /// UI 每个渲染拍调用一次——积压一次性落地再画（丢拍不丢字节）。
    pub fn drain(&self) -> Vec<LegMsg> {
        let mut out = Vec::new();
        while let Ok(m) = self.0.try_recv() {
            out.push(m);
        }
        out
    }
}

/// 接收端已收线（读帧线程的收尾信号）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct SendEnded;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn fifo_preserves_byte_order() {
        let (send, recv) = leg_channel(64);
        for i in 0..8u8 {
            send.send(LegMsg::Bytes(vec![i])).expect("容量内发送");
        }
        let bytes: Vec<Vec<u8>> = recv
            .drain()
            .into_iter()
            .filter_map(|m| match m {
                LegMsg::Bytes(b) => Some(b),
                _ => None,
            })
            .collect();
        assert_eq!(bytes, (0..8).map(|i| vec![i]).collect::<Vec<_>>(), "FIFO 保序");
    }

    #[test]
    fn bounded_send_blocks_until_drained() {
        let (send, recv) = leg_channel(1);
        send.send(LegMsg::Bytes(b"a".to_vec())).expect("第一帧入队");
        // 容量 1 已满：线程再发一帧必然阻塞——100ms 后仍未完成 ⇒ 证明阻塞。
        let done = Arc::new(AtomicBool::new(false));
        let done2 = Arc::clone(&done);
        let h = std::thread::spawn(move || {
            let send = send; // SendHalf：可 move 进腿线程（Send）
            send.send(LegMsg::Bytes(b"b".to_vec())).expect("drain 后可达");
            done2.store(true, Ordering::SeqCst);
        });
        std::thread::sleep(Duration::from_millis(100));
        assert!(!done.load(Ordering::SeqCst), "满容量下 send 阻塞（背压生效）");
        // 消费一帧 ⇒ 阻塞的发送放行 ⇒ 无一帧丢失。
        assert_eq!(recv.try_recv(), Ok(LegMsg::Bytes(b"a".to_vec())));
        h.join().expect("发送线程收尾");
        assert!(done.load(Ordering::SeqCst));
        assert_eq!(recv.try_recv(), Ok(LegMsg::Bytes(b"b".to_vec())), "背压放行后帧完整");
    }

    #[test]
    fn drain_coalesces_without_loss() {
        let (send, recv) = leg_channel(8);
        for i in 0..5u8 {
            send.send(LegMsg::Bytes(vec![i])).expect("入队");
        }
        assert_eq!(recv.drain().len(), 5);
        assert_eq!(recv.drain().len(), 0, "收清后为空（下一次渲染拍无残留）");
    }

    #[test]
    fn send_reports_when_receiver_dropped() {
        let (send, recv) = leg_channel(2);
        drop(recv);
        assert_eq!(send.send(LegMsg::Bytes(b"x".to_vec())), Err(SendEnded), "接收端收线 = 读帧线程收尾信号");
    }
}
