# 第三方代码归属（**移植/内嵌**面）

> 本文件只登记**被搬进本仓源码树的第三方代码**（移植、vendored、逐行改写）。
> 其余依赖（quinn / rustls / tokio / smoltcp / alacritty_terminal …）都是 crates.io
> 依赖，各自许可随 `Cargo.lock` 与上游包发布，不在此重复登记。
>
> 纪律：**移植必须留出处**——源文件头写清「上游仓库 / 文件 / commit / 许可 + 与上游的差异」，
> 本文件给仓库级台账；两处缺一即视为归属缺失（公开仓，硬要求）。

## tquic（Apache-2.0）—— 拥塞控制 BBRv3

- **上游**：<https://github.com/Tencent/tquic>（`tquic`），分支 `develop`，commit
  `938e90adb460b5ff08b2bc6d11a3e1ba52c27a8d`（取件日期 2026-10-10）。
- **许可**：Apache License, Version 2.0
  （<http://www.apache.org/licenses/LICENSE-2.0>）；上游文件头逐字保留在本仓文件头。
- **引进批**：M6.7「拥塞控制对比批」（`docs/reviews/CC-BBR3.md`，
  环境开关 `HOMEWAY_QUIC_CC = bbr3`；**默认档不变**）。
- **落地位置与对应关系**（逐文件）：

  | 上游文件 | 本仓文件 | 改写程度 |
  |---|---|---|
  | `src/congestion_control/bbr3.rs` | `crates/homeway-quic/src/cc/bbr3.rs` | 算法逐行保真；接口层按本仓框架的 `Controller` 改写（差异表在该文件头） |
  | `src/congestion_control/delivery_rate.rs` | `crates/homeway-quic/src/cc/delivery_rate.rs` | 算法逐行保真；「每包发送快照」的承载方式改写（差异表在该文件头） |
  | `src/congestion_control/minmax.rs` | `crates/homeway-quic/src/cc/minmax.rs` | 逐行保真（含上游 `mod test`）；仅可见性收窄 |
  | `pacing.rs` / `hystart_plus_plus.rs` / `bbr.rs` / `cubic.rs` / `copa.rs` / `dummy.rs` / `congestion_control.rs` | **未搬** | 见 `crates/homeway-quic/src/cc/mod.rs` 的搬/不搬表 |

- **上游再上游**：`minmax.rs` 承载 Google 的 BSD-3-Clause 许可头（该算法源自
  `lib/minmax.c` / Kathleen Nichols 的窗式 min/max 估计器）——该许可头随文件保留在
  `crates/homeway-quic/src/cc/minmax.rs` 顶部，**不得删除**。
- **未搬的运行时依赖**：上游用 `rand`（随机探测等待）与 `log`（控制器内日志）。本仓不引入
  这两个依赖：随机量改为自持的 splitmix64（`getrandom` 种子），日志删（本仓判据行走 `Logf`，
  控制器内不打行）。逐条见 `cc/bbr3.rs` 文件头差异表。
