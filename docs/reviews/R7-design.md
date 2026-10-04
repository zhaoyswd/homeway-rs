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
| C-4② | readyBy 世代起点未清 | **已修（eb3e6fa 时点登记失实——见 §四末「失实登记修正」；实际修复落 7l/0baca9f：`StageMachine::begin_generation` 清 `ready_by` + 单测 `begin_generation_clears_ready_by`）** |
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

## §三 第 2 棒争议三条拍板（2026-10-04，开工时按工单约定拍板）

| # | 争议 | 拍板 | 依据 |
|---|---|---|---|
| 1 | attach-timeout 的 code/state 归属（严格对齐 Go 生产路径 vs 保留 d.ts 描述改 tier 文档） | **严格对齐 Go 生产路径**：`state=idle` + `code=attach-timeout` + `reason=就绪后无人 attach，已自行收工放锁`（tunmode.go:802 逐字）。C-5 向量已重产（`failed_attach_timeout` 案改名 `attach_timeout_idle`，三处同步：vecgen 生成器 / fixtures/vectors/tun_status.jsonl / Rust 对照测试）。原向量是「Go 不可达合成态」（failed + 错文案）——生产路径里 60s 死线写的是 idle，且该 code 只在世代收尾完成前的窗口可读（收尾 defer 统一归 idle 空码）；d.ts 的 code 词面族（stopped/attach-timeout）与生产路径兼容，无需改 tier 文档 | 生产语义唯一真源是 Go 代码行为；「窗口可读」语义两侧一致（Rust finish_generation 同样在收尾时清 code） |
| 2 | events 面去留（Rust 独有则文档声明不参与词表对账 / 对齐 Go facade/bus.go） | **保留 + 文档声明不参与词表对账**。events（EventHub 可轮询队列）不在 20 个导出面内（Go 无对应导出——App 侧推送经 IPC 在 ArkTS 层），是 facade 内部诊断/测试面；`facade/mod.rs` 头注释已声明。Go facade/bus.go 的不重不漏语义归 ArkTS 侧（跨进程推送通道不在核内），核侧不需要对齐物 | 对齐一个不存在的消费面只会引入第二真源；App 的状态通道在扩展进程侧（R7 不动 ArkTS） |
| 3 | FB-files 判据按 rc 判定（matrix.sh:627 grep -c . 无条件 PASS） | **按 rc 判定**：FB-files 的 files CLI 子命令以退出码为判据（rc!=0 红），输出行只作旁证（matrix.sh 已改） | grep -c . 对空输出也 PASS——判据门假绿（评审 F 项同类） |

**前置工单六项处置记录（2026-10-04 第 2 棒）**：

| 工单 | 处置 | 落点 |
|---|---|---|
| ①世代生命周期 | **已做**：TunExecutor.warmup 改「启动即返」（spawn 世代线程即返，同步硬失败仍 Err）；TunShared 承载世代共享面（单飞锁/世代号/阶段机/健康位/attach 通道/done 信号），执行体世代线程一切退出路径调 `finish_generation`（非 failed 回 idle 空码、放锁、unhealthy="stop"、done）；attach 60s 死线由世代线程收割（idle/"attach-timeout"/生产路径文案）；meowed 沿用暖机结果（attached 写入）；新世代清健康位/分类（begin_healthy） | facade/tun_shared.rs（新）+ facade/mod.rs 重构 + facade/tun_exec.rs（世代线程 gen_loop） |
| ②桥宿主线程模型 | **已做**：handle_conn 整体 spawn（鉴权 5s + 拨号 15s 出 accept 线程）；鉴权绝对 5s 期限（read_timeout 承载）；sock_path_free 200ms 预算（connect_budget：非阻塞 connect + poll）；remove_sock_own 的 None 分支不删；try_clone/锁中毒不 panic（lock_host + into_inner）；listen 重试不持宿主锁（信息交接式短临界区）；stop 有界等 accept/conn 线程收口（live 计数 + Condvar，2s） | facade/bridge_host.rs |
| ③tun_stop 终态语义 | **已做**：只放锁不写阶段（终态由世代线程 finish_generation 写；failed 保留）；-2 强制放锁路径同样不写阶段 | facade/mod.rs tun_stop + tun_shared |
| ④传输/拨号期限 | **已做**：files 传输体清 15s（read/write timeout None——取消走 cancel 断 I/O，开场警戒独立）；UDS 拨号加 connect 预算（connect_budget 公共件：files/term/speedtest 桥全换）；速度桥 link_down 回帧（非 refused 类回 report{link_down} 再有序收口——ReplyThenClose 语义：吞请求帧→回帧→FIN→短窗吞输入） | files_op.rs / term_op.rs / speedtest_op.rs / bridge_host.rs |
| ⑤trait/装配签名统一 | **已做**：dial_port 接缝 = BridgeStream trait（into_halves 拆半 + WriteHalf 半关——UDS 与 SessionConn 流适配同构）；TunExecutor 类型化错误 TunError（code() 映射状态码）；桥状态并入 serviceStatusJSON（bridge 四键）；speedtest 引擎接桥（run_dial 拨号闭包形态 + SpeedConn trait + BridgeSpeedConn 承载 + Cancel 真取消 = 看门狗 kill）；service 三面接真 Session（service_exec.rs：Session + 服务桥 + rc 门接线）；版本注入管线（HOMEWAY_CORE_VERSION option_env! 已在 facade::version，构建侧 7h 注入） | speedtest.rs / facade/{tun_exec,service_exec,speedtest_op,bridge_host}.rs |
| ⑥panic 策略 | **部分失实修正（见 §四末「失实登记修正」）**：eb3e6fa 时点只做了 tun_shared/mod 的锁面（lock_unpoison/lock_host）；demand/events/service_op/files_op/stage 的存量 `.expect("…锁中毒")` 约 40 处留到 7l 才收敛（LockUnpoison trait 化 + stage 直用 lock_unpoison）；三派生线程 catch_unwind 也在 7l。extern "C" 壳的 catch_unwind 在 7h 属实 | facade/*（7l 补齐）+ homeway-capi（7h） |

**登记（不阻塞）**：
- portfwd 监听器执行体未实现（pf 表存 + runner 状态面就位；`pf=0/0` 恒零）——Go 侧端口转发监听器的实装排第 3 棒；
- bind 全候选发送统计面（localErrAdopted/localErrTotal + sendTries 全败判据）未实现——tunStatusJSON 该二键缺省（可选键）、巡检噪声判定只用采纳路径粘性信号；
- fd: 快照行（fdSnapshot）不打——OHOS 沙箱 /proc/self/fd 面受限（Go 侧该行同样受限）；
- 隧道域无 REBUILD（对齐 Go：阶梯失败 markUnhealthy 交扩展重建整条 VPN——与服务域的 rebuild_session 语义分流）。

## §四 第 2 棒评审记录（dsh r2.JCkfJb，2026-10-04）

**结论**：集成面总体成立（评审明确「看过没发现问题」的面：世代线程退出路径无漏调
finish_generation、decap 分流键与 Go hub.isForB 逐条件等价、fd 所有权、EAGAIN-poll
修复、巡检语义与全部常量、attach 60s 死线逐字文案、桥宿主四项整改、Cancel kill 链、
capi 20 符号与 malloc 契约、E2E 判据行逐条可溯、tier 51036b4 无破坏）；**3 高危 +
11 中危 + 12 低危 + 评审者补充 8 条**，其中「接线即坏」类 4 项（H-1/H-2/H-3/我-1）
**当棒已整改**（见下表），其余登记第 3 棒工单。评审原文全量 =
`/tmp/dsh-review/r2.JCkfJb/output.md`（含里层 dsh 落盘原文路径与评审者自己的
可执行复现：H-2 的 done/attach 污染四信号实测）。

### P0 整改表（当棒，commit 见 git log "R7-7k 评审 r2 P0 整改"）

| # | 项 | 定级 | 处置 |
|---|---|---|---|
| H-2 | finish_generation 的 done/attach 通道无世代守卫（旧世代迟到收尾污染新世代：attach 恒 -1 → 60s 死线；done 假 0 → request_stop 永不执行 → prepare 恒 -1 只有进程重启能解） | 高 | **已修**：close_attach/done 写入过 gen 守卫（单例槽形态下以 gen 比对等价 Go 的 per-run 通道）；把 bug 钉成预期的单测改向（stale 收尾不得发 done/关 attach） |
| H-3 | 收工路径恒超 STOP_WAIT(3s)（stats 整段 sleep 60s tick + join 每线程 2s×3）⇒ tun_stop 常态化 -2，放大 H-2 窗口 | 高 | **已修**：stats/pusher 改 100ms 分片等待（片界查 stop）；join 总预算 2s（与 STOP_WAIT 留 1s 给桥/client 收尾） |
| H-1 | D4 待发包下推恒不触发：last_outbound 存 unix epoch ns 却当 Instant 时长减 ⇒ should_push 恒 false（静默错值不 panic） | 高 | **已修**：TunCounters 双轨（unix ns 留 JSON 面 + 单调相对读数 last_outbound_mono_ns 给下推器；process_mono_start 基准） |
| 我-1 | M-11 的真实后果：暖机软失败期 runner 整块消失 ⇒ exitIp/bridgeAuth 缺 ⇒ 扩展「拒绝建接口（DNS 将无处可去）」——Go 的软失败自愈路径在 Rust 变启动失败（E2E 两轮 meowed 成功未暴露） | 高 | **已修**：runner 的 link 兜底 via="none"（runner 块只要求世代在场，link 键恒在） |

### 第 3 棒工单（登记，按评审建议顺序）

**P1（中危批）**：M-1 mark_unhealthy 世代守卫（两个生产调用点都没过 gen——Go
markUnhealthy 内有 isCurrent）→ M-5/M-9/M-8 接缝类型化（connect_budget 未覆盖
connect 本身；is_refused_like/Report 归因的字符串嗅探 contains("refused"/"满员")；
取消被归因 interrupted 且 REASON_CANCELLED 死常量 + 取消打不断在途拨号）→
M-2 GenRun 登记过晚（identity/Client::start 之后——叠加 -2 出现同身份双 Client
窗口）→ M-3 attach_receiver 晚于 Ready 发布（tier 侧 rc≠0 直接 throw 不重试——
一行改）→ M-4 path_probe 无外层硬超时（引擎卡死 ⇒ 暖机永久 preparing/巡检卡死）→
M-6 begin_generation 不清 ready_by + **修正 R7-design §二/eb3e6fa 的两条失实
登记**（「已修」但 diff 无此行）→ M-7 SpeedHost 的 live/dir 恒缺（UI 恒「连接中」
——评审倾向本棒补最小相位出口，时间不足登记）→ M-10 service stop 超时提前 take
（同钥双会话窗口）+ dial_via_run 持锁拨号阻塞 status 轮询 → M-11 已由 P0 我-1
收其大半（link 兜底），runner 条件语义完全对齐留第 3 棒 → 我-2 隧道域端点缓存
三缺（set_candidates 未喂/set_on_hint 未接/save 未调——进程退出端点全丢）→
我-4 TunPacket 不写 wake 管道（上行每串包最长排队 250ms）。

**P2（低危批）**：L-1（-2 分支补健康位）/L-2（-2 日志预检提前）/L-3（三派生线程
catch_unwind + facade 存量 expect 收敛——工单⑥口径修正为「本棒新增面已做、存量
待批」）/L-4（packet-info 探测剥离缺失登记）/L-5（旁路探测 4s vs Go 8s、mtu clamp、
dial_ms 死字段三处常量对齐）/L-6（cause 文案用档位名）/L-7（下推器噪声窗 15s→5s）/
L-8（Cmd::TunStats/orphan_alive/BridgeSock.listener 三处死件）/L-9（ProbeReach
父预算 3.5s + 并行解析 + 去重——spec MUST）/L-11（starting 形态缺 elapsedMs、
failed 后 run 槽回收）/L-12（finish 不关本世代 stop）+ 我-3（capi guard fallback
立即求值 ⇒ 每次成功调用 malloc(1) 泄漏——改闭包一行）/我-5（write_fd_all 在
driver 线程对 POLLOUT 无界重试不查 stop）/我-7（R1 动作立即失败时 Go 继续验证、
Rust 直接收轮）/我-8（fd 读 n==0 静默 return vs Go continue）+ 我-6（**tier 侧
build-core.sh rust 档缺脏检出闸与钉定**——未提交代码可走正式出包路径；与「仓的
归属 R8 定」无关，闸门应先行——第 3 棒动 tier 时补）。

**驳回**：L-10（option_env! 增量失效疑虑）——cargo/rustc 的 env-dep 机制会跟踪
编译期 env 写入 dep-info，HOMEWAY_CORE_VERSION 变即重编（评审者与里层 dsh 双重
实测确认），不需要 build.rs。

**需拍板项（留给用户/第 3 棒）**：① bind 全候选发送统计留桩的边界——它在 Go 里
同时是巡检噪声门控的输入（sendTries 全败 ⇒ 环境噪声不计证据），Rust 只剩采纳
路径粘性信号 ⇒ 挂起禁发期（EPERM 事故形态）可能被计成质量失败、每拍烧 R1；是否
提前补 bind 统计面由用户定。② M-7 speedtest live/dir 最小相位出口 vs 登记受限。

## §五 第 3 棒（7l 整改批）处置表 + 拍板记录（2026-10-04）

**P1/P2 全量处置（commit `0baca9f`；tier 侧 `4b8a0a1`）——29 项全处置，无挂账**：

### P1 中危批（13 项）

| # | 项 | 处置 |
|---|---|---|
| M-1 | mark_unhealthy 世代守卫 | **修**：`TunShared::mark_unhealthy_if_current(gen, why)`；fd 错误回调捕获 gen、patrol 用 run.gen（两个生产调用点全过守卫；单测 `mark_unhealthy_generation_guard`） |
| M-2 | GenRun 登记过晚 | **修**：GenRun 构造 + state 登记提前到 `Client::start` 之前（client 改 `RwLock<Option<Arc<Client>>>` 后填）；identity 装配窗口的停止请求经 `TunShared` 停止位槽中继（`signal_stop`）；装配完成点查 stop 统一收口（不进暖机） |
| M-3 | attach_receiver 晚于 Ready | **修**：`attach_receiver()` 提到 `set_if_current(Ready)` 之前（一行时序） |
| M-4 | path_probe 无外层硬超时 | **修**：`recv_timeout(预算+2s)` 双保险（Go 暖机 select/time.After 同义）；成功连接补 close 防引擎内槽位滞留 |
| M-5 | connect_budget 未覆盖 connect | **修**：真非阻塞 connect（socket+fcntl O_NONBLOCK → EINPROGRESS → poll → SO_ERROR；darwin 无 SOCK_NONBLOCK 类型位故走 fcntl） |
| M-6 | begin_generation 不清 ready_by | **修**：`begin_generation` 清 `ready_by` + 单测；两条失实登记修正见本节末 |
| M-7 | speedtest live/dir 恒缺 | **修（拍板②）**：`LiveProgress` 原子面（相位+字节）+ `run_dial` 进度出口 + SpeedHost 差分 instBps + 相位跟随（down→up）；Status 面在途轮 live 三键齐 |
| M-8 | 取消被归因 interrupted | **修**：`SpeedtestError::{Busy,LinkDown,Cancelled}` 类型化（REASON_CANCELLED 复活、contains("满员/refused") 嗅探全消）；拨号间隙查取消位；残余登记：单次在途拨号自带 ≤10s 预算兜底（Go ctx 即刻打断——差值 = 取消生效点延到拨号预算边界） |
| M-9 | is_refused_like 字符串嗅探 | **修**：healing_dial（隧道/服务两域）把 `ConnErr::Refused` 映射成 `ErrorKind::ConnectionRefused` 带过接缝；`is_refused_like` 只认 kind |
| M-10 | service stop 提前 take + dial 持锁 | **修**：超时路径 run 槽保留（域状态 Stopping 挡 start）；`Session::stop` 改 `&self`（句柄 Mutex 化）+ `Arc<Session>` 克隆出锁再拨/再快照（status 轮询面不再被 15s 拨号阻塞） |
| M-11 | runner 条件语义（残尾） | **修**：runner/transport 组装抽 `runner_of/transport_of` 共享件；配合 M-2 早期登记，runner 从 prepare 起在场（Go setRunner 时序对齐），link 兜底沿用 P0 我-1 |
| 我-2 | 隧道域端点缓存三缺 | **修**：① `Client::start` 后 `set_candidates(merged)`（历史端点进赛跑集）② hint 链路线程（observe(Hint)+候选重投+打洞〔5s 节流+rearm_soft+probe 5s〕+落盘信号）③ save 去抖线程（1s 窗合并）+ Finish guard 终写；旁路探测 on_ep 补落盘信号 |
| 我-4 | TunPacket 不写 wake 管道 | **修**：读线程投包后写 wake 管道（与 Client::send 共享 `Arc<Mutex<Option<i32>>>` 锁位——stop 关闭后写入自然 no-op）；残余登记：cmd 通道无界 vs Go hubQueue=512（driver 每拍全量 drain，堆积只在一拍内；换 bounded 通道引入新阻塞面，不换） |

### P2 低危批（16 项）

| # | 项 | 处置 |
|---|---|---|
| L-1 | -2 分支补健康位 | **修**：`mark_unhealthy("stop")`（Go 同分支） |
| L-2 | 日志 -2 停在 preparing | **修**：LogOpen 错误路径 `set_if_current(gen, Idle)` 回 idle（单测 `log_open_minus2_resets_stage_to_idle`） |
| L-3 | 派生线程无 catch_unwind + 存量 expect | **修**：`spawn_derived` 统一壳（panic→日志+markUnhealthy_if_current("panic")）；demand/events/service_op/files_op 存量 expect 收敛（LockUnpoison trait），stage 直用 lock_unpoison |
| L-4 | packet-info 探测剥离缺失 | **修**（优于登记）：读侧 4B PI 自动探测剥离（Go tunfd_unix.go 同款判定：PI 头 + 合法 IP 版本号双条件） |
| L-5 | 三处常量 | **修**：旁路探测 8s（Go 同值）；mtu `≤0→1280` 不 clamp（Go Normalize 同形）；dial_ms 缺省 15000 且经 `BridgeHost::set_dial_timeout` 热传入（死字段转正） |
| L-6 | cause 文案档位名 | **修**：`扩展下推(R1 重握手)` 形态（Level::clamp(from).name()） |
| L-7 | 下推器噪声窗 15s | **修**：改 `demand::OUTBOUND_FRESH`(5s)（Go shouldPush 同源） |
| L-8 | 三处死件 | **修**：`Cmd::TunStats` 删（计数经 Arc<TunCounters> 直读）；`orphan_alive/orphan_flag` 删（无消费者的诊断位）；`BridgeSock.listener` 删（恒 None 的空操作——监听器由 accept 线程独占，stop 收口靠 stopped 位+有界等待） |
| L-9 | ProbeReach 无预算/串行解析/不去重 | **修**：父预算 3.5s（spec MUST）+ 解析并行（1.5s 子预算，挂死线程超时即弃）+ 地址去重 + 探测预算 = 余量与 3s 取小 |
| L-10 | option_env! 增量失效 | **驳回维持**（r2 已证伪：cargo env-dep 机制跟踪编译期 env；双层实测） |
| L-11 | starting 缺 elapsedMs / failed 后槽不回收 | **修**：ServiceRun.since → starting 形态带 elapsedMs（Go serviceSnapshotJSON 同形）；会话线程 Err 清 run 槽 + 停桥（start 不再恒 -1） |
| L-12 | finish 不关本世代 stop | **修**：TunShared 停止位槽（`set_stop_flag`/`signal_stop`）；finish_generation 对当前世代置位（Go close(r.stop) 同义；单测 `finish_stops_current_generation_flag`） |
| 我-3 | capi guard fallback 立即求值泄漏 | **修**：fallback 改 `impl FnOnce() -> T` 闭包（懒求值；13 处调用点全改） |
| 我-5 | write_fd_all 无界重试 + 卸源丢停止通道 | **修**：POLLOUT 总预算 5s（超预算按 TimedOut 收 ⇒ 卸源路径）；写失败卸源**先置读线程停止位**再卸（评审者「不认同②」的修复建议：卸源必须同时 stop.store(true)——照办） |
| 我-7 | R1 动作立即失败收轮 | **修**：ResetPeerSession 立即失败只记日志「丢会话失败——按既有状态验证」继续验证（Go 闭包恒 nil 同义）；超时仍 -3 收轮 |
| 我-8 | fd 读 n==0 静默判死 | **修**：continue + poll 一片（防非阻塞 fd 热自旋；Go continue 同义） |
| 我-6 | tier rust 档缺脏检出闸与钉定 | **修（tier `4b8a0a1`）**：核相关路径（crates/tools/Cargo.*）脏检出默认硬失败；`HOMEWAY_RS_ALLOW_DIRTY=1` 逃生口（醒目告警）；产物版本标记 SHA == HEAD 钉定校验（拦旧产物复检/脏码未重编） |

### 拍板记录（两条留桩边界，第 3 棒拍板）

1. **bind 全候选发送统计——拍板：提前补全（不留桩）**。理由：它在 Go 里同时是
   巡检噪声门控的输入（sendTries>0 且全本地失败 ⇒ 环境性禁发不计证据），挂起
   禁发期（EPERM 事故形态）被计成质量失败、每拍烧 R1 的风险在本产品是**真实场景**
   （挂起/唤醒是核心测试面），不属「可等 R8」的优化面。落点：`Bind`
   send_tries/send_local_fails（采纳单发 + 镜像逐候选计数；过渡双发/解锁补发的
   尽力语义不进计数——Go FIX-09 同口径）→ `Client::swap_send_stats`（差分等价
   swap-reset）→ 巡检噪声双信号（Go tunmode.go:1024-1027 双保险同构）+
   `local_err_counters` → tunStatusJSON `demand.localErrAdopted/localErrTotal` 两键
   （runner 键缺省的登记项一并消掉）。
2. **M-7 speedtest live/dir——拍板：本棒补最小相位出口**（评审者倾向同向：UI
   消费面已就位〔SpeedTestStore.ets 按 dir 分档〕，登记受限等于把已修好的 UI 面
   留在恒「连接中」）。落点见 M-7 行。

### 失实登记修正（M-6 要求，2026-10-04 第 3 棒核实）

1. **§二 C-4②「已修（eb3e6fa：begin_generation 清）」失实**——`git show eb3e6fa`
   的 diff 无此行（只有提交信息正文提到）；生产代码在 7l/0baca9f 才落
   （`begin_generation` 清 `ready_by`）。§二表已就地标注。
2. **§三 工单⑥「已做（锁面）：facade 全域锁 unwrap → into_inner」部分失实**——
   eb3e6fa 只覆盖 tun_shared/mod/bridge_host 三处；demand/events/service_op/
   files_op/stage 约 40 处 `.expect("…锁中毒")` 与三派生线程 catch_unwind 留到
   7l 才收敛。§三表已就地标注。eb3e6fa 提交信息本身不可改，以本节为准。

## §六 第 3 棒复核轮（dsh r3=r7l.5XAMBa，2026-10-04）处置表

**复核结论摘要（原文 = /tmp/dsh-review/r7l.5XAMBa/output.md）**：§五 29 项「已修」
逐条对 diff 核验**无失实复发**（r2 的教训过关）；新引入面五路复查
（run_dial 五参/Session::stop&self/wake 共享/connect_budget/swap 差分数学）**无问题**；
登记项三条理由核验（上行归 R8 成立/cmd 无界成立/M-8 兜底**部分失真**）；抓出
**F1 高危回归**（gen_loop 两条早退路径漏 finish_generation——前一棒 P0 同形态）+
4 项「修但不完整」（M-2 中继对 gen≥2 不可达 / M-7 字节三角累计 / M-10 双会话窗口
未闭 + 新引入持锁阻塞 / 拍板① 计数口径）+ 低危 12 条。

**处置（commit 3ab9891 + tier e6c640c 一族）**：

| # | 定级 | 处置 |
|---|---|---|
| F1 早退漏 finish_generation | 高 | **修**：EarlyFinish 守卫（真 Finish 挂上后 disarm 交接）+ 回归单测 `tunnel_empty_candidates_releases_lock`（域名端点 token 驱动真 gen_loop，验「失败后下一次 prepare 可受理」） |
| F2 state 槽不清 ⇒ 中继 gen≥2 不可达 | 中 | **修**：Finish::drop 以 ptr_eq 清自己；request_stop/recover 加陈旧世代过滤（陈旧→共享面旗标中继 / -2） |
| F3 term 测试偶发挂死 | 中（流程门） | **无法复现**：本机串行 4 轮（单测 0.87s + 3×全量 30.4s）全绿；登记环境偶发（term/ 本批未触碰；评审者 base 证据 n=1）——ci-local 若再现按评审建议加硬期限，归 R8 测试健壮性 |
| F4 LiveProgress 三角累计 | 中 | **修**：pump 分片上报改「本片增量」+ `live_progress_delta_accounting` 单测 |
| F5 bind 计数口径 | 中 | **修**：解锁补发/RREG 腿补计数（writeUDP 统一点全覆盖）；采纳路径错误只进 adoptedLocalErrCount（localErrTotal=镜像专属——Go bind_test 口径）；注释更正；+计数面单测（0.0.0.0 候选=必然本地错） |
| F6 stop 双会话窗口 + 持锁阻塞 | 中 | **修**：顶层锁开头即放；fully_stopped 完成信号（重试 stop 等同一个信号）；warmup 期 stop 竞态收口（不发布 Ready 自查自收）；清槽全 ptr_eq |
| F7 取消抢先分支 | 中 | **修**：取消旗标优先于一切归因（Go isCancelled 先判） |
| F8 取消后无期限执行者 | 中 | **修**（取消分支）：看门狗 kill 后守到 done/父预算、迟登记连接周期性再杀；**登记 R8**：SpeedConn 的 Go SetDeadline 同义期限面（timeout 分支的极端形态） |
| F9 tier 闸门四类绕过 | 中 | **修三**（tier e6c640c + 本仓 build-app-core.sh 对齐）：路径集补 fixtures/.cargo/rust-toolchain.toml、git 状态 fail-closed、钉定改整串比较（含 +dirty）；**登记 R8**：tier 侧落 homeway-rs.pin 期望 SHA（与「仓归属」决策一并） |
| F10 healing_dial 兜底丢 kind | 低 | **登记**（会话已坏形态，回 link_down 语义等价合理） |
| F11 set_stop_flag 无守卫 | 低 | **修**：set_stop_flag_if_current |
| F12 装配窗口 recover -4 vs Go -2 | 低 | **修**：陈旧世代 → -2 |
| F13 M-7 三处口径差 | 低 | **修**（上行拨号窗相位回空档）；**登记**（含预热字节的 bytes 口径/终态键面——tier 零消费） |
| F14 rustfmt 三处 | 低 | **修**：fmt 落本批触碰文件（全仓 fmt 波及 71 个未触碰文件已回退） |
| F15 ServiceRun↔Bridge 引用环 | 低 | **登记 R8**（存量，每 start/stop 周期泄漏一套对象） |
| F16 观察项（PI 求值序/hint 线程孤儿语义/write 返 0 空转） | 低 | **登记**（无行为差异/既有孤儿语义延伸/理论边界） |

**闭环判断（评审者原话）**：不建议按「29 项全处置」直接收口 → 处置：F1-F9 全修后
重跑门禁（332 lib 全绿含 4 串行轮 + clippy 0 + OHOS 交叉 0 + ci-local 终轮），R7 按本表收口。
