//! **拥塞控制移植面**（M6.7「拥塞控制对比批」）：BBRv3 的移植与工厂接线。
//!
//! 唯一上游来源：**tquic**（<https://github.com/Tencent/tquic>，Apache-2.0），commit
//! `938e90adb460b5ff08b2bc6d11a3e1ba52c27a8d`（`develop`，2026-10-10 取件）：
//!
//! | 上游文件 | 本模块文件 | 搬了什么 / 没搬什么 |
//! |---|---|---|
//! | `src/congestion_control/bbr3.rs`（74,138 B） | [`bbr3`] | **全搬**（状态机/增益/上限自适应/ProbeRTT/丢包响应），接口层按框架差异改写（见该文件头差异表） |
//! | `src/congestion_control/delivery_rate.rs` | [`delivery_rate`] | 全搬（速率采样算法逐行保真）；「每包快照」的承载方式按框架改写 |
//! | `src/congestion_control/minmax.rs` | [`minmax`] | 全搬（含上游 `mod test`） |
//! | `src/congestion_control/pacing.rs` | **不搬** | pacing 属框架职责（框架的 pacer 按 `window()/RTT` 自算令牌桶）⇒ 搬进来会双发车 |
//! | `hystart_plus_plus.rs` | **不搬** | 上游 BBRv3 对它零引用（那是 CUBIC 的依赖） |
//! | `bbr.rs`（v1/v2 混合体）/`cubic.rs`/`copa.rs`/`dummy.rs` | **不搬** | 本批只对比 v3；对照组用框架自带的 CUBIC 与 BBRv1 |
//! | `congestion_control.rs`（trait） | **不搬** | 框架已给 `Controller`/`ControllerFactory`（签名见 `bbr3.rs` 差异表） |
//! | `log`/`rand` 依赖 | 删/自持 | 本仓不打控制器内日志；随机量用自持 splitmix64（不新增依赖） |
//!
//! 许可归属：Apache-2.0（tquic）+ `minmax.rs` 的 BSD-3（Google，随文件保留）——逐文件头
//! 有原文/出处，仓库级登记见 `THIRD-PARTY.md`。**移植不许抹出处**。
//!
//! 隔离面：`bbr3.rs`（点框架名）在本模块里是**异步面清单**成员（`tools/check-quic-isolation.sh`
//! 的 `ASYNC_FILES`）；`minmax.rs`/`delivery_rate.rs` 纯 std（受 ② 条递归真扫描）。

mod bbr3;
mod delivery_rate;
mod minmax;

pub(crate) use bbr3::Bbr3Config;

impl Bbr3Config {
    /// 工厂接线用的具名入口（`exit/transport.rs` 的 `cc_factory` 读它；按 MTU 定
    /// `min_cwnd`/`initial_cwnd`，与 `build(now, current_mtu)` 的实际值一致）。
    pub(crate) fn for_mtu(mtu: u16) -> Self {
        bbr3::Bbr3Config::from_mtu(mtu)
    }
}
