//! 服务会话域（语义真源 `baseline:clientcore/cmd/clientcore/app_service.go` +
//! `clientcore/hostsession/service.go` 的 Session + Default 单例面）。
//!
//! App 进程内、无 TUN 的 WG 会话：VPN 未连接时承载 files/term 两座回环桥。
//! 返回码契约（导出面逐字不变）：
//! - `service_start`：0 已启动（幂等：starting/ready 下重复调用直接 0）｜-1 上一个
//!   实例还在收工｜-2 服务日志打不开｜-3 配置不是合法 JSON｜-4 token 为空；
//! - `service_stop`：0 已收工（或本就没跑）｜-1 等待超时（此后 start 一直 -1 防双持钥，
//!   调用方应重试 stop）。
//!
//! 状态 JSON（`crate::status_json::snapshot_json`——R2 已对照锁定）的桥接与
//! Session 实装在 7d；本模块先落 rc 门与状态面的组装位。

use serde_json::Value;

/// 服务域状态（rc 门的载体；Session/桥在 7d 接入）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// 无实例（状态 JSON 短路 `{"state":"idle"}`）。
    Idle,
    /// 受理中/已就绪（幂等面：start 重复调用直接 0）。
    Starting,
    Ready,
    /// 收工中（start 恒 -1 防双持钥；stop 重试到 0）。
    Stopping,
    /// 终态失败（可重新 start）。
    Failed,
}

/// 服务域（单例语义——App 全核一个服务会话；7d 在此挂真 Session）。
pub struct ServiceDomain {
    state: std::sync::Mutex<ServiceState>,
    /// 7d 接入的 Session 快照产出面（None = 状态 JSON 走 idle 短路）。
    snapshot_json: std::sync::Mutex<Option<Box<dyn Fn() -> String + Send>>>,
}

impl Default for ServiceDomain {
    fn default() -> Self {
        Self::new()
    }
}

impl ServiceDomain {
    pub fn new() -> Self {
        ServiceDomain {
            state: std::sync::Mutex::new(ServiceState::Idle),
            snapshot_json: std::sync::Mutex::new(None),
        }
    }

    /// 7d 装配：挂状态 JSON 产出面（真 Session 的 snapshot → status_json）。
    pub fn set_snapshot_json(&self, f: Option<Box<dyn Fn() -> String + Send>>) {
        *self.snapshot_json.lock().expect("服务快照锁中毒") = f;
    }

    /// ClientCoreServiceStart（rc 契约见模块头）。
    pub fn start(&self, cfg_json: &str) -> i32 {
        // 配置预检：-3（非法 JSON）/ -4（token 空）
        let v: Value = match serde_json::from_str(cfg_json) {
            Ok(v) => v,
            Err(_) => return -3,
        };
        if v.get("token").and_then(Value::as_str).unwrap_or("").is_empty() {
            return -4;
        }
        let mut st = self.state.lock().expect("服务状态锁中毒");
        match *st {
            ServiceState::Starting | ServiceState::Ready => 0, // 幂等
            ServiceState::Stopping => -1,                      // 上一个还在收工
            ServiceState::Idle | ServiceState::Failed => {
                *st = ServiceState::Starting;
                // 实际装配（日志开面 -2 / Session 起）在 7d 接入；受理语义先落
                0
            }
        }
    }

    /// ClientCoreServiceStop（rc 契约见模块头）。
    pub fn stop(&self) -> i32 {
        let mut st = self.state.lock().expect("服务状态锁中毒");
        match *st {
            ServiceState::Idle => 0, // 本就没在跑
            // 7d 接真收工（桥 → 会话 → 端点缓存落盘的顺序 + ≤6s 等待）；当前直收
            ServiceState::Starting | ServiceState::Ready | ServiceState::Failed => {
                *st = ServiceState::Idle;
                0
            }
            ServiceState::Stopping => -1,
        }
    }

    /// ClientCoreServiceStatus（无实例 = `{"state":"idle"}` 逐字节短路）。
    pub fn status_json(&self) -> String {
        if let Some(f) = self.snapshot_json.lock().expect("服务快照锁中毒").as_ref() {
            return f();
        }
        crate::status_json::idle_json().to_owned()
    }

    /// 状态位直写（7d 的 Session 状态机推进用 + 测试）。
    pub fn set_state(&self, s: ServiceState) {
        *self.state.lock().expect("服务状态锁中毒") = s;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// rc 契约：-3 非法 JSON / -4 空 token / 幂等 0 / stopping -1。
    #[test]
    fn start_rc_contract() {
        let d = ServiceDomain::new();
        assert_eq!(d.start("{oops"), -3);
        assert_eq!(d.start(r#"{"mtu":1280}"#), -4);
        assert_eq!(d.start(r#"{"token":"hmw1-x"}"#), 0);
        // starting/ready 幂等 0
        assert_eq!(d.start(r#"{"token":"hmw1-x"}"#), 0);
        d.set_state(ServiceState::Ready);
        assert_eq!(d.start(r#"{"token":"hmw1-x"}"#), 0);
        // 收工中 -1（防双持钥）
        d.set_state(ServiceState::Stopping);
        assert_eq!(d.start(r#"{"token":"hmw1-x"}"#), -1);
    }

    /// stop：idle 0；ready → 0 归位；failed 可重启。
    #[test]
    fn stop_contract() {
        let d = ServiceDomain::new();
        assert_eq!(d.stop(), 0);
        d.start(r#"{"token":"hmw1-x"}"#);
        assert_eq!(d.stop(), 0);
        assert_eq!(d.stop(), 0);
        d.set_state(ServiceState::Failed);
        assert_eq!(d.start(r#"{"token":"hmw1-x"}"#), 0); // failed 可重新受理
    }

    /// 状态 JSON：无产出面 = idle 短路逐字节；挂面后透传。
    #[test]
    fn status_json_shortcircuit() {
        let d = ServiceDomain::new();
        assert_eq!(d.status_json(), "{\"state\":\"idle\"}");
        d.set_snapshot_json(Some(Box::new(|| r#"{"state":"ready"}"#.to_owned())));
        assert_eq!(d.status_json(), r#"{"state":"ready"}"#);
    }
}
