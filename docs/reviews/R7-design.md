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

## §二 第 1 棒评审记录（dsh r1.5L80Vz，2026-10-04）

**结论**：工程形态总体扎实（facade 按域拆分、tunStatusJSON 真 Go 向量逐字节对账、OHOS 交叉清零、
flock 语义正确）；**6 高危 + 评审者复核补 2 高危**，其中「接线即坏」类 4 项已在本棒整改
（eb3e6fa / 2147b70 两 commit），结构性 4 项登记为**第 2 棒开工前置工单**（见下）。
评审原文全量 = `/tmp/dsh-review/r1.5L80Vz/output.md`（含三路并行对照核查）。

### 高危处置表

| # | 项 | 定级 | 处置 |
|---|---|---|---|
| F-05 | TunConfigJson 缺 camelCase rename（7 字段静默丢） | 高 | **已修**（eb3e6fa：rename_all + 守卫测试） |
| F-08 | SpeedParams 同面（窗口值静默忽略） | 高 | **已修**（同上） |
| C-2 | files 响应行 64KB 上限（512KB readText 直接 op_failed） | 高 | **已修**（eb3e6fa：上限只留请求行） |
| F-16 | term LIST 坏 JSON 被吞成假成功 | 中→高（复核加重） | **已修**（eb3e6fa） |
| F-30 | 锁覆盖面不全 + matrix/perf-ab 会被锁拒（判据静默失效） | 高 | **已修**（2147b70：dnstest/portfwd 接锁 + 脚本 16 处逃生口） |
| F-06/F-07/C-1 | attach 60s 死线未实装 + warmup 同步阻塞 JS 线程 + 无世代退出通知（锁泄漏） | 高 | **登记第 2 棒前置**（见工单①） |
| F-19/F-20/F-22/F-23 | 桥宿主鉴权/拨号在 accept 线程同步 + stop 删非己 bind 的 socket + 无超时探测 + panic 面 | 高 | **登记第 2 棒前置**（工单②） |

### 中危处置表（摘）

| # | 项 | 处置 |
|---|---|---|
| C-4② | readyBy 世代起点未清 | **已修**（eb3e6fa：begin_generation 清） |
| C-6 | 锁预建目录 0755 | **已修**（eb3e6fa：0700） |
| F-09 | tun_stop 抹失败终态/终态字节面 | 登记（工单③；窄窗——失败路径已当场放锁） |
| C-4①③ | attach 硬写 meowed=true / 健康位清清理缺 | 登记（工单③） |
| C-5 | attach-timeout 向量是 Go 不可达合成态（state/reason 双偏） | 登记（工单①随死线实装重产向量） |
| C-3 | 传输体受 15s 空闲超时（Go 无期限） | 登记（工单④） |
| F-10~F-12 | speedtest 取消空操作 / 引擎未接桥 / service 三面是桩 | **留桩性质登记**（第 2 棒接真引擎/真 Session——本棒范围就是接口+单测） |
| F-13/F-24/F-28 | 桥状态装配位接不上 / dial_port 接缝签名 / events 面无消费者 | 登记（工单⑤——trait 签名在接线时统一定） |
| F-14/F-15/F-17/F-18/F-21/F-25/F-27/F-31 | portfwd stale 复查 / MTU 形参 / null 表拒绝 / UDS 无拨号超时 / listen 持锁 / link_down 回帧 / 字符串错误 / 锁 fail-open | 登记（工单④⑤批处理） |

### 低危

F-33（SUMS 前缀）/F-34（§四冒烟结论补记——**已补：2026-10-04 cargo check OHOS 0 错 0 警**）/F-35（code 词表补 config/derp）
+ 评审低危表批量（readText mode 空串、entries 键序、err 前缀、帧 128KiB、version 文案、
tun_recover 未钳位等 12 项）——**登记第 2 棒顺手批**，不阻塞。

### 争议项（第 2 棒开工时拍板）

1. attach-timeout 的 code/state 归属（严格对齐 Go 生产路径 vs 保留 d.ts 描述改 tier 文档）；
2. events 面去留（Rust 独有则文档声明不参与词表对账 / 对齐 Go facade/bus.go 的不重不漏语义）；
3. FB-files 判据按 rc 判定（matrix.sh:627 grep -c . 无条件 PASS）。

### 第 2 棒开工前置工单（接线即坏类，按序）

1. **世代生命周期补全**：TunExecutor 增「世代退出通知」；warmup 改「启动即返」（Arc<StageMachine>
   或 facade 自持世代线程——tier 壳在 JS 线程同步直调 prepare，20s 暖机窗会冻结事件泵）；
   attach 60s 死线收割 + `idle/"attach-timeout"/"就绪后无人 attach，已自行收工放锁"` 终态
   （向量 C-5 随之重产）；meowed 沿用暖机结果；新世代清健康位/分类。
2. **桥宿主线程模型**：handle_conn 挪 spawn（鉴权+拨号出 accept 线程）；鉴权绝对 5s 期限；
   sock_path_free 200ms 预算；remove_sock_own 的 None 分支不删（换轨窗口误删在服务方 socket）；
   try_clone/锁中毒不 panic；listen 重试出宿主锁。
3. **tun_stop 终态语义**：只放锁不写阶段（终态由世代写；failed 保留）。
4. **传输/拨号期限**：files 传输体清 15s（大文件无期限）；UDS 拨号加 connect 预算；速度桥
   link_down 回帧面。
5. **trait/装配签名统一**：dial_port 接缝对真 Session（流 id vs fd 桥）、TunExecutor 类型化
   错误（F-27）、桥状态并入 serviceStatusJSON、speedtest 引擎接桥 + Cancel 真取消、service
   三面接真 Session、version 注入管线（HOMEWAY_CORE_VERSION + [lib] cdylib 配置 + tier
   build-core.sh 对接——F-03，动 tier 跟踪文件属用户触点）。
6. panic 策略：extern "C" 壳 catch_unwind + 锁 unwrap_or_else(into_inner)（F-02）。
