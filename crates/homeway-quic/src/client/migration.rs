//! 迁移保持检测（设计 §2.3 的失败面）：`rebind` 之后**对端不可达没有专用错误**——只能
//! 自己给判据。
//!
//! 判据（本切片定死、S2b 消费）：重绑后**一个巡检节拍**（[`crate::config::DEFAULT_PATROL`]
//! 缺省 60s / 测试可调短）内收到该连接的**任何对端回包**（`stats().udp_rx.datagrams` 增长
//! ⇒ ACK / PATH_CHALLENGE / 数据任一）即判**迁移完成**（打 N-b 行、`migrations` 计数）；
//! 到点仍无回包 ⇒ 判**未确认**（打行 + 置 `IslandSnapshot::migration_unconfirmed`），
//! 回落「重连/重赛跑」的动作由上层发起（M1 的真阶梯 = 既有 `session/recover`，S2b 接线）。
//!
//! 为什么用「入站报文计数」而不是 `remote_address()`：客户端侧看候选地址恒不变；
//! 能证明「新路径双向可用」的只有**收到对端回包**这一件事。计数由宿主从连接上取
//! （本模块是纯逻辑 ⇒ 两个分支都能被确定性单测覆盖）。

use std::net::SocketAddrV4;
use std::time::{Duration, Instant};

/// 一次重绑后的保持检测窗。
#[derive(Debug)]
pub(crate) struct Watch {
    from: SocketAddrV4,
    to: SocketAddrV4,
    /// 重绑时刻的入站报文计数（判定增量用）。
    rx0: u64,
    /// 重绑时刻（判窗到点）。
    at: Instant,
}

/// 检测窗的结论（宿主据此打行/入快照）。
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum MigrationEvent {
    /// 收到对端回包 ⇒ 迁移完成（`elapsed` = 重绑到回包的间隔）。
    Confirmed { elapsed: Duration },
    /// 一个巡检节拍内无回包 ⇒ 未确认（回落动作由上层发起）。
    Unconfirmed,
}

impl Watch {
    /// 起一次检测窗（`rx0` = 重绑后的入站报文计数基线；`at` = 重绑时刻，
    /// **显式注入**以便纯逻辑用例钉死时钟）。
    pub(crate) fn start(from: SocketAddrV4, to: SocketAddrV4, rx0: u64, at: Instant) -> Self {
        Self { from, to, rx0, at }
    }

    pub(crate) fn from(&self) -> SocketAddrV4 {
        self.from
    }

    pub(crate) fn to(&self) -> SocketAddrV4 {
        self.to
    }

    /// 拍问一次：有回包 ⇒ `Some(Confirmed)`；窗到点仍无 ⇒ `Some(Unconfirmed)`；
    /// 窗内未到点且无回包 ⇒ `None`（继续等）。`rx_now` 由调用方从连接取（`udp_rx()`）。
    pub(crate) fn tick(
        &self,
        rx_now: u64,
        patrol: Duration,
        now: Instant,
    ) -> Option<MigrationEvent> {
        if rx_now > self.rx0 {
            return Some(MigrationEvent::Confirmed {
                elapsed: now.duration_since(self.at),
            });
        }
        if now.duration_since(self.at) >= patrol {
            return Some(MigrationEvent::Unconfirmed);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 判据的纯逻辑面（无 IO）：窗内无回包 ⇒ `None`；有回包 ⇒ `Confirmed`；
    /// 到点仍无 ⇒ `Unconfirmed`（`N 拍无回包 ⇒ 回落` 的判据本体）。
    #[test]
    fn watch_confirms_on_inbound_and_fails_at_deadline() {
        let from: SocketAddrV4 = "127.0.0.1:1000".parse().unwrap();
        let to: SocketAddrV4 = "127.0.0.2:2000".parse().unwrap();
        let patrol = Duration::from_millis(200);
        let t0 = Instant::now();

        let w = Watch::start(from, to, 7, t0);
        assert_eq!(w.from(), from);
        assert_eq!(w.to(), to);
        // 窗内无回包 ⇒ 等下一拍
        assert_eq!(w.tick(7, patrol, t0 + patrol / 2), None, "窗内未到点");
        // 有回包 ⇒ 完成（间隔 = 拍问时刻 - 重绑时刻）
        assert_eq!(
            w.tick(8, patrol, t0 + patrol / 2),
            Some(MigrationEvent::Confirmed {
                elapsed: patrol / 2
            })
        );
        // 到点仍无回包 ⇒ 未确认
        assert_eq!(
            w.tick(7, patrol, t0 + patrol),
            Some(MigrationEvent::Unconfirmed)
        );
        // 到点后有回包仍优先判完成（回包是最强证据）
        assert!(matches!(
            w.tick(9, patrol, t0 + patrol * 2),
            Some(MigrationEvent::Confirmed { .. })
        ));
    }
}
