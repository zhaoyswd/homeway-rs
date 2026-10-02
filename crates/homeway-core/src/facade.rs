//! facade：App 核门面的 trait 预留（R7 接 NAPI 的实装面；R2 只落契约形状）。
//!
//! 语义真源 = `baseline:clientcore/cmd/clientcore/probe_lib.go` 的导出面与 rc 契约
//! （语义注释 = `tier:AGENTS.md` 原生契约四处同步清单）：
//!
//! | 面 | rc 契约 |
//! |---|---|
//! | `prepare` | 0 开始 / -1 忙 / -2 日志打不开 / -3 参数错 |
//! | `attach`  | 0 接管 / -1 无 ready 世代 / -3 fd≤0 / -4 接管失败 / -5 轮询超时 |
//! | `stop`    | 0 已停 / -1 等超时 / -2 超时后强制放锁 |
//! | `recover` | 0 某档通过 / -1 走完未恢复 / -2 无 attached 隧道 / -3·-4 本地动作 / -9 异步壳异常 |
//!
//! **tunStatusJSON 完整键面**（meowed/readyBy/portForwards/demand/exitIp/tunIp/
//! stats.fd*/bridge*——`tunmode.go:564`）需要 TUN 面，归 R7 实装；R2 的对照面 =
//! 服务形态状态 JSON（`status_json`，即本 trait `status_json` 在服务形态下的产出）。
//! hostsession 的非 APP 部分（状态快照/巡检/恢复阶梯）已由 `session` 实装。

use crate::session::{Level, Session, SessState};

/// tun 门面（两阶段 prepare/attach 语义；NAPI 导出的 Rust 侧落点）。
pub trait TunFacade {
    /// 第一阶段（暖机）：建隧道侧（WG + 栈 B）并起握手；**不碰 TUN**。
    fn prepare(&self, cfg_json: &str) -> i32;
    /// 第二阶段（接管 fd）：递 TUN fd；重复调用返回错误（两个源抢同一 TUN）。
    fn attach(&self, fd: i32, mtu: u32) -> i32;
    /// 状态 JSON（服务形态 = `status_json::snapshot_json`；隧道形态完整键面 R7）。
    fn status_json(&self) -> String;
    /// 收工（幂等）。
    fn stop(&self) -> i32;
    /// 恢复阶梯入口（from 钳位 R1..=R3；`-9` 异步壳队列异常由 napi 壳产生，本面不产）。
    fn recover(&self, from: i64, cause: &str) -> i32;
    /// 「核活着」的唯一权威判据（probeRunning && tunHealthy && stage==attached 的
    /// 隧道域形态；服务形态 = state==Ready）。
    fn running(&self) -> i32;
    /// 端口转发整表热替换（不重连隧道）；`{"portForwards":[…]}` 形状。
    fn set_port_forwards(&self, json: &str) -> i32;
}

/// 服务形态实现（prepare=装配、attach=无 TUN 面恒 -1）。
impl TunFacade for Session {
    fn prepare(&self, cfg_json: &str) -> i32 {
        // Session 装配在 ::start 完成（暖机含在内）；此面为 R7 真两阶段的占位契约
        let _ = cfg_json;
        match self.snapshot().state {
            SessState::Ready | SessState::Starting => 0,
            _ => -1,
        }
    }

    fn attach(&self, fd: i32, mtu: u32) -> i32 {
        // 服务形态没有 TUN 面：恒 -1（无 ready 世代可接 TUN——评审低-16 口径）
        let _ = (fd, mtu);
        -1
    }

    fn status_json(&self) -> String {
        crate::status_json::snapshot_json(&self.snapshot())
    }

    fn stop(&self) -> i32 {
        // Session::stop 需 &mut（收工 join）；门面走共享引用——由调用方管生命周期。
        // R7 实装时以内部可变性收口；本占位返回语义值。
        0
    }

    fn recover(&self, from: i64, cause: &str) -> i32 {
        self.recover(Level::clamp(from), cause).as_rc()
    }

    fn running(&self) -> i32 {
        i32::from(self.snapshot().state == SessState::Ready)
    }

    fn set_port_forwards(&self, json: &str) -> i32 {
        // 热替换面属隧道域（tunRun 世代挂载）；服务形态无监听器表——R7 接线
        let _ = json;
        -1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rc 契约形状（纯常量断言——R7 实装的行为测试届时补）。
    #[test]
    fn facade_contract_shape() {
        // recover 的 from 钳位经 Level::clamp：0→R1、9→R3
        assert_eq!(Level::clamp(0), Level::R1);
        assert_eq!(Level::clamp(9), Level::R3);
        // as_rc 词表
        assert_eq!(
            crate::session::LadderRc::Recovered(Level::R1).as_rc(),
            0
        );
        assert_eq!(crate::session::LadderRc::Exhausted.as_rc(), -1);
        assert_eq!(crate::session::LadderRc::ActionTimeout.as_rc(), -3);
    }
}
