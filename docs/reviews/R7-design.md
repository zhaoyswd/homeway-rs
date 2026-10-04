# R7 设计与评审记录 — APP 接入

> 第 1 棒（7a–7f）产出。评审渠道：dsh 外部评审（reviewer skill）；记录按节追加。

## §一 7b napi-rs OHOS 支持评估与技术选型（2026-10-04 拍板）

**结论（一句话）：不采用 napi-rs，转正退路 = C-ABI 导出（复刻 Go `//export` 的 20 个符号与语义）
+ 手写/镜像 NAPI 胶水——核侧恒同步请求/应答面使 napi-rs 增值近零，而其 OHOS 运行时支持
现状构成不必要的供应链与维护风险；C-ABI 路已被 PoC 实证且对 tier 消费链的侵入面积最小。**

### 1. napi-rs v3 OHOS 支持现状（调研事实）

- **构建链**：上游 `@napi-rs/cli` v3（≥3.x）的目标表**已含**三个 OHOS triple
  （`aarch64/armv7/x86_64-unknown-linux-ohos`），npm 已有 `-openharmony-arm64` 后缀产物
  （如 `@napi-rs/pinyin-openharmony-arm64`、`@napi-rs/nice-openharmony-arm64`）——
  **交叉编译与打包面**是通的。
- **运行时**：上游 `napi` crate 对 OHOS 的**模块注册**支持没有可查的已合入文档/PR
  （`github.com/napi-rs/napi-rs` 内未见 ohos 运行时适配的明确记录）；实际被生态采用的是
  **ohos-rs fork**（`napi-ohos` + `napi-derive-ohos` + `ohrs` CLI，fork 自 napi-rs，
  OHOS 模块注册由运行时接管、链接 `libace_napi.z.so`）。fork 体量：245 星 / 34 releases /
  MSRV 1.88 / 社区（richerfu 等）维护；另有 OpenHarmony-TPC 的 gitee 移植（仓库状态关闭）。
  EasyTier 鸿蒙移植等案例走的都是 fork 而非上游。
- **线程模型**：napi-rs v3 的异步面经 feature 门控（`tokio_rt`/`async-runtime`）——可关，
  但 fork 默认 tokio；本仓硬规则「自管线程 + poll(2)、不引 tokio」要求精确禁用配置。
- **已知限制（fork README 自述）**：async 函数需 feature；回调返回类型只能 `Result`。

### 2. 与退路（C-ABI + 手写 NAPI 胶水）的决策矩阵

| 维度 | napi-rs（经 ohos-rs fork 或赌上游） | C-ABI + 手写胶水（**选定**） |
|---|---|---|
| **导出面匹配度** | 20 个导出全部是**同步** C-ABI 请求/应答（string/int in → int/JSON-string out，无核侧回调、无核侧 Promise；`*Async` 壳在 C++/ArkTS 胶水面已存在）⇒ napi-rs 三大增值（TS codegen/async runtime/ThreadsafeFunction）对本面**近零** | 天然同构：Rust cdylib `extern "C"` 复刻 20 符号（同名同签名，含 CString 的 malloc/free 契约） |
| **tier 消费链侵入**（第 2 棒用户触点面积） | 替换 .so **并**改写 `tailcat_napi.cpp`/CMake/`Index.d.ts`——tier 侧四处同步契约整体重构 | **原位换 .so**（同名 `libclientcore.so`，tier 现有 extern 声明/胶水/Index.d.ts/Index.ets 原样工作）；第 2 棒 tier diff 最小化 |
| **供应链/维护** | 上游 OHOS 运行时支持未文档化 ⇒ 实际依赖 245 星社区 fork；HarmonyOS NDK 演进的跟随责任在第三方；napi-rs v3 本身大版本churn | 零新依赖；胶水 ~300 行自有（镜像 tier 现役 `tailcat_napi.cpp` 的形态，不发明）；OHOS 交叉配方 PoC 已实证（NDK clang 链接器 + ring 垫片 + `[patch.crates-io]`） |
| **体积** | napi 运行时机器码 + 注册样板，估 +50–150KB（对 1.0MB 全量 dylib 非决定性但方向为负） | 增量 ≈ 胶水与 facade 本体（几十 KB 级） |
| **线程模型合规** | 需精确禁 tokio feature 面；fork 默认开 | 核内自管线程 + poll(2) 原样（现役形态），无运行时注入 |
| **TS 类型生成** | 自动生成 `.d.ts`（但 tier 的 `Index.d.ts` 是四处同步契约锁定的手写件——生成物反而构成第二真源） | 复用 tier 现有手写 `Index.d.ts`（契约即类型真源，不变） |

### 3. 拍板理由归纳

1. **导出面形态决定论**：核侧无回调、无异步——napi-rs 解决的问题（跨线程 Promise、
   类型编解码样板）在这个面上不存在；C-ABI 是零成本精确解。
2. **tier 契约兼容是硬约束**：四处同步门（`check-napi-sync.sh` 整词匹配 `ClientCore*` ↔
   `clientCore*`）按符号名闭合；Rust 复刻同名符号 = tier 侧零改动即可联调，第 2 棒的
   用户触点收缩为「构建产物替换 + 验证」。
3. **风险不对称**：napi 路引入 fork 依赖与构建链变量（fork 的 NDK 跟随、feature 矩阵、
   v3 上游 churn），换取的是我们不需要的能力；C-ABI 路的全部风险点（OHOS 交叉、ring 垫片、
   链接）已被 PoC 实测清零。
4. ** CString 契约注意**（落 7c 实装）：Go 侧 `cstr()` 返回 malloc 块、由 NAPI 胶水 `free()`
   ——Rust 侧必须用 `libc::malloc`（非 Rust 分配器）保证 free 兼容；这是同名符号之外的
   隐性 ABI 契约，7c 的对照测试面覆盖。

### 4. 后续动作

- 不做 napi-rs 50 行冒烟（未选该路）；**等价冒烟** = 7c 完成后对本仓跑
  `cargo check --target aarch64-unknown-linux-ohos -p homeway-core`（OHOS 目标已装、
  链接器配置在位）——第 2 棒开工前唯一的交叉面风险已清。
- 第 2 棒（HSP 集成，用户触点）开工前置确认单列于 ROADMAP R7 节。
