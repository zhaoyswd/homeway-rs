//! 进程级 env 开关缓存（Q-I F4）。
//!
//! 热路径站点（每包 / 每 poll 唤醒 / 每排空轮）此前各自 `std::env::var_os`——macOS 上
//! getenv 走全局锁（`_os_unfair_lock`），实测单次 ≈55ns、`sample` 叶帧占出口驱动线程
//! 0.63% + 发送线程 0.32%（Q-I 设计 §0.2 实测）。这里一次读取缓存（`OnceLock`）。
//!
//! **语义**：env 在**首次读取**时定型——手动排障开关须在进程启动前设置
//! （运行中 `set_var` 不再生效；全仓无此用法，Q-I 设计 §2 F4 已核）。

use std::sync::OnceLock;


/// `HOMEWAY_QUIC_MTU`（M1 设计 §12-① 的 MTU 上限旋钮）**原始串**；解析/夹区间/记行
/// 同在 `facade::tun_exec`。**M5 C3**：同文件的 `transport_raw`（`HOMEWAY_TRANSPORT`
/// 承载开关）与 `wg_debug` 随 WG 面删除——单承载下前者是死旋钮（设计 §4.3①）。
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

    /// MTU 上限 env 只会返回「原始串或 None」，且重复读取同值——解析/记行不在本模块。
    #[test]
    fn mtu_raw_is_cached_string() {
        // 原始串 = 当前 env 值（首读缓存；本仓测试不 set_var，两条路都成立）
        assert_eq!(
            quic_mtu_raw().map(str::to_owned),
            std::env::var("HOMEWAY_QUIC_MTU").ok()
        );
        assert_eq!(quic_mtu_raw(), quic_mtu_raw(), "重复读取同值（缓存语义）");
    }
}
