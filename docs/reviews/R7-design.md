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
| ⑥panic 策略 | **已做（锁面）**：facade 全域锁 unwrap → into_inner（lock_unpoison/lock_host 单件）；extern "C" 壳的 catch_unwind 在 7h 的 homeway-capi（每导出包 panic 边界，返回安全错误值） | facade/* + homeway-capi（7h） |

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
