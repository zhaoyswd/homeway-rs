//! 进程级 env 开关缓存（Q-I F4）。
//!
//! 热路径站点（每包 / 每 poll 唤醒 / 每排空轮）此前各自 `std::env::var_os`——macOS 上
//! getenv 走全局锁（`_os_unfair_lock`），实测单次 ≈55ns、`sample` 叶帧占出口驱动线程
//! 0.63% + 发送线程 0.32%（Q-I 设计 §0.2 实测）。这里一次读取缓存（`OnceLock`），
//! 与 `wgcore::ack_drain_bytes` 的既有先例同族。
//!
//! **语义**：env 在**首次读取**时定型——手动排障开关须在进程启动前设置
//! （运行中 `set_var` 不再生效；全仓无此用法，Q-I 设计 §2 F4 已核）。

use std::sync::OnceLock;

/// `HOMEWAY_TX_DBG`（出口发送面 `[TXDBG]`/`[ENCDBG]` 调试行）。
pub(crate) fn tx_dbg() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("HOMEWAY_TX_DBG").is_some())
}

/// `HOMEWAY_WG_DEBUG`（wgcore 写失败详情行）。
pub(crate) fn wg_debug() -> bool {
    static V: OnceLock<bool> = OnceLock::new();
    *V.get_or_init(|| std::env::var_os("HOMEWAY_WG_DEBUG").is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 未设置 = 恒 false；重复读取同值（OnceLock 缓存面）。若跑测环境恰好设置了该
    /// env（排障场景），则反向断言——两条路都不依赖运行中修改 env（set_var 在多线程
    /// 测试下不安全，设计 §2 F4 明确不引入）。
    #[test]
    fn tx_dbg_first_read_cached() {
        if std::env::var_os("HOMEWAY_TX_DBG").is_some() {
            assert!(tx_dbg(), "env 已设：首次读取缓存为 true");
        } else {
            assert!(!tx_dbg(), "未设置 = false");
        }
        assert_eq!(tx_dbg(), tx_dbg(), "重复读取同值（缓存语义）");
    }

    #[test]
    fn wg_debug_first_read_cached() {
        if std::env::var_os("HOMEWAY_WG_DEBUG").is_some() {
            assert!(wg_debug(), "env 已设：首次读取缓存为 true");
        } else {
            assert!(!wg_debug(), "未设置 = false");
        }
        assert_eq!(wg_debug(), wg_debug(), "重复读取同值（缓存语义）");
    }
}
