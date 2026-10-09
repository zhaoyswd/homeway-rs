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

/// `HOMEWAY_TRANSPORT`（M1 设计 §4.1 的 A/B 开关）**原始串**：`quic`（缺省）/`wg`。
///
/// 为什么交原始串给调用方：非法值要「记行 + 按默认走」，而记行需要世代日志面——
/// 本模块只负责「首读缓存」这一件事（解析在 `facade::tun_exec` 的世代装配里做）。
pub(crate) fn transport_raw() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| std::env::var("HOMEWAY_TRANSPORT").ok())
        .as_deref()
}

/// `HOMEWAY_QUIC_MTU`（M1 设计 §12-① 的 MTU 上限旋钮）**原始串**；解析/夹区间/记行
/// 同在 `facade::tun_exec`（理由同 [`transport_raw`]）。
pub(crate) fn quic_mtu_raw() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| std::env::var("HOMEWAY_QUIC_MTU").ok())
        .as_deref()
}

/// `HOMEWAY_QUIC_ADMIT_RETRY`（M2 设计 §3.2 表末的排障开关）**原始串**；解析/记行同在
/// `server::quic_admit::resolve_retry_policy`（理由同 [`transport_raw`]：非法值要
/// 「记行 + 按缺省走」，而记行需要世代日志面）。
pub(crate) fn quic_admit_retry_raw() -> Option<&'static str> {
    static V: OnceLock<Option<String>> = OnceLock::new();
    V.get_or_init(|| std::env::var("HOMEWAY_QUIC_ADMIT_RETRY").ok())
        .as_deref()
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

    /// 两个新 env（M1 S3-1：A/B 开关 + MTU 上限）只会返回「原始串或 None」，且重复
    /// 读取同值——解析/记行不在本模块（见函数文档）。
    #[test]
    fn transport_and_mtu_raw_are_cached_strings() {
        // 原始串 = 当前 env 值（首读缓存；本仓测试不 set_var，两条路都成立）
        assert_eq!(
            transport_raw().map(str::to_owned),
            std::env::var("HOMEWAY_TRANSPORT").ok()
        );
        assert_eq!(
            quic_mtu_raw().map(str::to_owned),
            std::env::var("HOMEWAY_QUIC_MTU").ok()
        );
        assert_eq!(transport_raw(), transport_raw(), "重复读取同值（缓存语义）");
        assert_eq!(quic_mtu_raw(), quic_mtu_raw(), "重复读取同值（缓存语义）");
    }
}
